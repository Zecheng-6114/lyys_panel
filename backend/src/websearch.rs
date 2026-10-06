//! AI 助手的联网能力：网页搜索与网页正文抓取。
//!
//! 设计边界：
//! - **只读**：仅发起 GET 请求读取公开网页，不携带面板凭据、不写任何远端；
//! - **零新依赖**：HTML 解析走纯字符串扫描。面板构建体积敏感，且引入 HTML
//!   解析栈（html5ever 一族）只为取标题/摘要不划算；
//! - **搜索多通道**：管理员配置的 SearxNG（JSON 接口）优先，否则依次抓取
//!   Bing（`cn.bing.com` → `www.bing.com`）与 DuckDuckGo，第一个有结果的胜出。
//!   本机实测 DuckDuckGo 不可达（出口代理 502）、`cn.bing.com` 正常，故顺序如此；
//! - **SSRF 防护**：`fetch_web_page` 的 URL 来自模型，属不可信输入。仅允许
//!   http/https，逐跳解析主机名并拒绝环回/私网/链路本地/多播/保留地址，
//!   重定向**手动**跟随（最多 [`MAX_REDIRECTS`] 跳）以便每跳重新校验；
//! - **体积与时间双上限**：单次响应最多读 [`MAX_BODY`] 字节，整体 12s 超时。
//!
//! 搜索通道走的是固定主机（或管理员显式配置的地址），不受 SSRF 闸门约束——
//! 否则自建在 `127.0.0.1` 上的 SearxNG 会被自己的防护挡掉。

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;

/// 出站统一 UA：不少站点对默认 UA（`reqwest/0.x`）直接返回验证页或空结果
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
/// 单次响应最多读入的字节数（搜索页/文档页都远小于此）
const MAX_BODY: usize = 1024 * 1024;
/// 单次请求总超时（含重定向各跳的累计时间）
const TIMEOUT: Duration = Duration::from_secs(12);
/// 手动跟随重定向的最大跳数
const MAX_REDIRECTS: usize = 5;
/// 抓正文时默认返回的最大字符数
const DEFAULT_MAX_CHARS: usize = 8_000;

/// 检索结果条目。字段用 snake_case 直接进 JSON 给模型看，
/// 这里不加 serde rename，保持与工具 schema 描述一致。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // 关键：不让 reqwest 自动跟随重定向，否则跳转到内网地址就绕过了
            // 逐跳校验（fetch_web_page 的 SSRF 闸门会形同虚设）。
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(8))
            .timeout(TIMEOUT)
            .build()
            .expect("websearch client 构建失败")
    })
}

/// 带体积上限的 GET，手动跟随重定向。
/// `guard=true` 时每一跳都做 SSRF 校验（供模型传入的 URL 使用）。
/// 返回 (最终 URL, Content-Type, 原始字节)。
async fn get_bytes(url: &str, guard: bool) -> Result<(String, Option<String>, Vec<u8>)> {
    let mut current = reqwest::Url::parse(url).with_context(|| format!("URL 无法解析：{url}"))?;
    for _ in 0..=MAX_REDIRECTS {
        if guard {
            ensure_public(&current).await?;
        }
        let resp = client()
            .get(current.clone())
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
            .send()
            .await
            .with_context(|| format!("请求失败：{current}"))?;

        let status = resp.status();
        if status.is_redirection() {
            let Some(loc) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
            else {
                bail!("重定向缺少 Location 头");
            };
            if guard {
                // 相对跳转要基于当前 URL 解析；`join` 同时会拒绝非法目标
                current = current
                    .join(loc)
                    .context("重定向目标无法解析")?;
            } else {
                current = current.join(loc).context("重定向目标无法解析")?;
            }
            continue;
        }
        if !status.is_success() {
            bail!("远端返回 HTTP {}", status.as_u16());
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        // 先按 Content-Length 快速拒绝，再流式读并卡上限：
        // 不能直接 resp.bytes()，超大页面会把内存吃满
        if let Some(len) = resp.content_length()
            && len as usize > MAX_BODY
        {
            bail!("远端响应过大（{} 字节）", len);
        }
        let mut body: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("读取响应体失败")?;
            let remain = MAX_BODY.saturating_sub(body.len());
            if chunk.len() >= remain {
                body.extend_from_slice(&chunk[..remain]);
                break;
            }
            body.extend_from_slice(&chunk);
        }
        return Ok((current.to_string(), ctype, body));
    }
    bail!("重定向次数过多")
}

/// SSRF 闸门：解析主机名并拒绝任何非公网地址。
/// 域名解析出多个地址时，只要有一个不可信就整体拒绝（避免 DNS 轮询绕过）。
async fn ensure_public(url: &reqwest::Url) -> Result<()> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        bail!("只允许 http/https 地址");
    }
    let host = url.host_str().context("URL 缺少主机名")?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("域名无法解析：{host}"))?;
    let mut seen = false;
    for addr in addrs {
        seen = true;
        if !is_public_ip(addr.ip()) {
            bail!("目标地址不是公网地址，已拒绝");
        }
    }
    if !seen {
        bail!("域名无法解析：{host}");
    }
    Ok(())
}

/// 公网地址判定：把各类特殊用途网段一并排除。
/// 面板以 root 运行，这类请求一旦放开等于给了内网探测能力。
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                // 100.64.0.0/10 CGNAT
                || (o[0] == 100 && (64..128).contains(&o[1]))
                // 192.0.0.0/24 IETF 协议保留
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                // 198.18.0.0/15 基准测试
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                // 240.0.0.0/4 保留
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 唯一本地地址
                || (s[0] & 0xfe00) == 0xfc00
                // fe80::/10 链路本地
                || (s[0] & 0xffc0) == 0xfe80
                // ::ffff:0:0/96 IPv4 映射地址：按内嵌 IPv4 再判一次
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !is_public_ip(IpAddr::V4(v4))))
        }
    }
}

// ---------- 搜索 ----------

/// 联网搜索。返回 (通道名, 结果列表)；全部通道失败时返回 Err（错误里带各通道原因）。
pub async fn search(
    query: &str,
    count: usize,
    searx_base: Option<&str>,
) -> Result<(String, Vec<SearchHit>)> {
    let query = query.trim();
    if query.is_empty() {
        bail!("搜索关键词为空");
    }
    let count = count.clamp(1, 20);
    let mut errs: Vec<String> = Vec::new();

    if let Some(base) = searx_base.map(str::trim).filter(|s| !s.is_empty()) {
        match searx(base, query, count).await {
            Ok(hits) if !hits.is_empty() => return Ok(("searxng".into(), hits)),
            Ok(_) => errs.push("searxng：无结果".into()),
            Err(e) => errs.push(format!("searxng：{e}")),
        }
    }

    let bing = [
        format!("https://cn.bing.com/search?q={}&setlang=zh-CN", pct_encode(query)),
        format!("https://www.bing.com/search?q={}", pct_encode(query)),
    ];
    for url in bing {
        match get_bytes(&url, false).await {
            Ok((_, ctype, body)) => {
                let hits = parse_bing(&decode_body(&body, ctype.as_deref()), count);
                if !hits.is_empty() {
                    return Ok(("bing".into(), hits));
                }
                errs.push("bing：无结果".into());
            }
            Err(e) => errs.push(format!("bing：{e}")),
        }
    }

    let ddg = format!("https://html.duckduckgo.com/html/?q={}", pct_encode(query));
    match get_bytes(&ddg, false).await {
        Ok((_, ctype, body)) => {
            let hits = parse_ddg(&decode_body(&body, ctype.as_deref()), count);
            if !hits.is_empty() {
                return Ok(("duckduckgo".into(), hits));
            }
            errs.push("duckduckgo：无结果".into());
        }
        Err(e) => errs.push(format!("duckduckgo：{e}")),
    }

    bail!("联网搜索不可用（{}）", errs.join("；"))
}

/// SearxNG 的 JSON 接口（需实例开启 `format=json`）
async fn searx(base: &str, query: &str, count: usize) -> Result<Vec<SearchHit>> {
    let url = format!(
        "{}/search?q={}&format=json&language=zh-CN",
        base.trim_end_matches('/'),
        pct_encode(query)
    );
    let (_, _, body) = get_bytes(&url, false).await?;
    let v: serde_json::Value = serde_json::from_slice(&body).context("返回不是合法 JSON")?;
    let arr = v
        .get("results")
        .and_then(|r| r.as_array())
        .context("返回缺少 results 字段")?;
    Ok(arr
        .iter()
        .take(count)
        .filter_map(|r| {
            let url = r.get("url")?.as_str()?.to_string();
            let title = r.get("title").and_then(|t| t.as_str()).unwrap_or_default();
            let snippet = r
                .get("content")
                .or_else(|| r.get("snippet"))
                .and_then(|c| c.as_str())
                .unwrap_or_default();
            Some(SearchHit {
                title: title.trim().to_string(),
                url,
                snippet: snippet.trim().to_string(),
            })
        })
        .collect())
}

/// Bing 结果页解析。结构稳定为：
/// `<li class="b_algo" ...><h2 ...><a ... href="URL" ...>TITLE</a></h2>...<p ...>SNIPPET</p>`
fn parse_bing(html: &str, count: usize) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for block in html.split("<li class=\"b_algo\"").skip(1) {
        if out.len() >= count {
            break;
        }
        let Some(after_h2) = block.split_once("<h2").map(|(_, r)| r) else {
            continue;
        };
        let Some((_, after_href)) = after_h2.split_once("href=\"") else {
            continue;
        };
        let Some((href, rest)) = after_href.split_once('"') else {
            continue;
        };
        let title = rest
            .split_once('>')
            .and_then(|(_, r)| r.split_once("</a>"))
            .map(|(t, _)| text_of(t))
            .unwrap_or_default();
        let snippet = rest
            .split_once("<p")
            .and_then(|(_, r)| r.split_once('>'))
            .and_then(|(_, r)| r.split_once("</p>"))
            .map(|(t, _)| text_of(t))
            .unwrap_or_default();
        let url = unescape(href.trim());
        // 只收 http(s) 结果：Bing 会混入 javascript: 之类的侧栏链接
        if title.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        out.push(SearchHit {
            title,
            url,
            snippet,
        });
    }
    out
}

/// DuckDuckGo HTML 版解析。链接是 `//duckduckgo.com/l/?uddg=<百分号编码的真实地址>`
/// 的跳转形式，需要把 `uddg` 参数解出来。
fn parse_ddg(html: &str, count: usize) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for block in html.split("class=\"result__a\"").skip(1) {
        if out.len() >= count {
            break;
        }
        let Some((_, after_href)) = block.split_once("href=\"") else {
            continue;
        };
        let Some((href, rest)) = after_href.split_once('"') else {
            continue;
        };
        let title = rest
            .split_once('>')
            .and_then(|(_, r)| r.split_once("</a>"))
            .map(|(t, _)| text_of(t))
            .unwrap_or_default();
        let snippet = rest
            .split_once("result__snippet")
            .and_then(|(_, r)| r.split_once('>'))
            .and_then(|(_, r)| r.split_once("</a>"))
            .map(|(t, _)| text_of(t))
            .unwrap_or_default();
        let url = ddg_real_url(&unescape(href.trim()));
        if title.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        out.push(SearchHit {
            title,
            url,
            snippet,
        });
    }
    out
}

/// 从 DuckDuckGo 跳转链接里取出真实地址；非跳转形式原样返回
fn ddg_real_url(href: &str) -> String {
    if let Some((_, rest)) = href.split_once("uddg=") {
        let value = rest.split('&').next().unwrap_or(rest);
        return pct_decode(value);
    }
    href.to_string()
}

// ---------- 网页正文 ----------

/// 抓取网页并转成纯文本。返回 (最终 URL, 正文文本)。
pub async fn fetch_text(url: &str, max_chars: Option<usize>) -> Result<(String, String)> {
    let (final_url, ctype, body) = get_bytes(url, true).await?;
    let (text, non_utf8) = decode_body_flagged(&body, ctype.as_deref());
    let mut text = html_to_text(&text);
    let limit = max_chars.unwrap_or(DEFAULT_MAX_CHARS).clamp(200, 40_000);
    text = truncate_chars(&text, limit);
    if non_utf8 {
        // 无第三方编码库，非 UTF-8 页面只能按 UTF-8 有损解码；
        // 明确告知模型，免得它把乱码当成页面真实内容反复揣摩
        text.push_str("\n\n（注意：该页面不是 UTF-8 编码，以上文本可能存在乱码）");
    }
    Ok((final_url, text))
}

/// 按字符数截断（不是字节数：中文按字节截会把字数算少大半）
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…（已截断）")
}

/// 按 Content-Type / `<meta charset>` 猜测编码并解码；无法识别按 UTF-8 有损处理
fn decode_body(bytes: &[u8], content_type: Option<&str>) -> String {
    decode_body_flagged(bytes, content_type).0
}

/// 同上，并返回「是否为非 UTF-8 页面」的标志
fn decode_body_flagged(bytes: &[u8], content_type: Option<&str>) -> (String, bool) {
    let label = charset_of(content_type).or_else(|| sniff_meta_charset(bytes));
    let is_utf8 = match label.as_deref() {
        Some(l) => {
            let l = l.to_ascii_lowercase();
            l == "utf-8" || l == "utf8"
        }
        None => true,
    };
    if is_utf8 {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    // 已知非 UTF-8（多为 GBK 系中文站）：先看能否恰好是 UTF-8，不能则标记
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

/// 从 Content-Type 头取 charset 参数
fn charset_of(content_type: Option<&str>) -> Option<String> {
    let ct = content_type?;
    let (_, rest) = ct.split_once("charset=")?;
    Some(
        rest.split(';')
            .next()
            .unwrap_or(rest)
            .trim()
            .trim_matches('"')
            .to_string(),
    )
}

/// 从 HTML 头部的 `<meta charset>` / `<meta http-equiv>` 里嗅探编码
fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let idx = text.find("charset")?;
    let rest = &text[idx + "charset".len()..];
    let rest = rest.trim_start_matches(['=', '"', ' ', '\'']);
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .unwrap_or(rest.len());
    let label = &rest[..end];
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

/// HTML 转纯文本：先剔除整段非正文元素，再把块级标签当换行，最后压缩空白。
fn html_to_text(html: &str) -> String {
    let stripped = remove_blocks(html, &["script", "style", "noscript", "svg", "template", "head"]);
    let mut out = String::with_capacity(stripped.len() / 2);
    let mut tag = String::new();
    let mut in_tag = false;
    for ch in stripped.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let closing = tag.trim_start().starts_with('/');
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .to_ascii_lowercase();
                // 只在**闭合**标签与自闭合标签（br/hr）上换行：
                // 开闭都换会得到双倍换行，块与块之间平白多出一行空白。
                if is_block_tag(&name) && (closing || name == "br" || name == "hr") {
                    out.push('\n');
                }
            }
            _ if in_tag => tag.push(ch),
            _ => out.push(ch),
        }
    }
    let decoded = unescape(&out);
    normalize_ws(&decoded)
}

/// 整段移除 `<tag ...>...</tag>`；无闭合标签时截到结尾（避免把脚本当正文）
fn remove_blocks(html: &str, tags: &[&str]) -> String {
    let mut out = html.to_string();
    for tag in tags {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        // 只小写 `from` 之后的未处理后缀：前缀已扫描过且无残留开标签，偏移也不受后续删除影响。
        let mut from = 0;
        loop {
            let lower = out[from..].to_ascii_lowercase();
            let Some(rel_start) = lower.find(&open) else { break };
            let start = from + rel_start;
            // 闭标签要在同一段里找，避免跨过已删区域
            let Some(rel) = lower[rel_start..].find(&close) else {
                out.truncate(start);
                break;
            };
            let end = start + rel + close.len();
            out.replace_range(start..end, "\n");
            // 替换后该位置只剩 1 字节的 "\n"，从其后继续找下一个块
            from = start + 1;
        }
    }
    out
}

/// 块级标签名（命中即视为换行边界）
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "br" | "p"
            | "div"
            | "li"
            | "ul"
            | "ol"
            | "tr"
            | "td"
            | "th"
            | "table"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "blockquote"
            | "nav"
            | "aside"
            | "main"
            | "dl"
            | "dt"
            | "dd"
            | "hr"
            | "form"
            | "figure"
            | "figcaption"
    )
}

/// 去掉全部标签保留文本（用于标题/摘要这类短片段）
fn text_of(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    normalize_ws(&unescape(&out))
}

/// 压缩空白：行内多空格合一、连续空行折叠为一个、行首尾去空白
fn normalize_ws(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut pending_space = false;
    for ch in s.chars() {
        match ch {
            '\n' => {
                lines.push(std::mem::take(&mut cur));
                pending_space = false;
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    pending_space = true;
                }
            }
            c => {
                if pending_space {
                    cur.push(' ');
                    pending_space = false;
                }
                cur.push(c);
            }
        }
    }
    lines.push(cur);
    let mut out: Vec<&str> = Vec::new();
    let mut blank = false;
    for l in &lines {
        let t = l.trim();
        if t.is_empty() {
            if !out.is_empty() && !blank {
                out.push("");
            }
            blank = true;
        } else {
            out.push(t);
            blank = false;
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// HTML 实体解码：支持数字（十进制/十六进制）与常见命名实体。
/// 只解一层——实体里再嵌套实体的写法在真实网页里不存在。
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s.as_bytes()[i] == b'&'
            && let Some(rel) = s[i..].find(';').filter(|p| *p <= 10)
        {
            let ent = &s[i + 1..i + rel];
            if let Some(ch) = decode_entity(ent) {
                out.push(ch);
                i += rel + 1;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn decode_entity(ent: &str) -> Option<char> {
    if let Some(num) = ent.strip_prefix('#') {
        let hex = num.strip_prefix('x').or_else(|| num.strip_prefix('X'));
        let code = match hex {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match ent {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" | "ensp" | "emsp" | "thinsp" => ' ',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "middot" => '·',
        "ldquo" => '“',
        "rdquo" => '”',
        "lsquo" => '‘',
        "rsquo" => '’',
        "copy" => '©',
        "reg" => '®',
        "deg" => '°',
        "times" => '×',
        "laquo" => '«',
        "raquo" => '»',
        _ => return None,
    })
}

/// 百分号编码（用于把查询词拼进 URL）
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// 百分号解码（用于还原 DuckDuckGo 跳转链接里的真实地址）
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 3 <= b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_and_percent_codecs() {
        assert_eq!(unescape("a&amp;b&lt;c&gt;d&quot;e"), "a&b<c>d\"e");
        assert_eq!(unescape("&#39;&#x4E2D;"), "'中");
        assert_eq!(unescape("100% &nbsp; ok"), "100%   ok");
        // 未知实体原样保留，不能吞掉内容
        assert_eq!(unescape("a&zz;b"), "a&zz;b");
        // 单个 & 结尾不越界
        assert_eq!(unescape("tail&"), "tail&");
        assert_eq!(pct_encode("nginx 配置"), "nginx+%E9%85%8D%E7%BD%AE");
        assert_eq!(pct_decode("https%3A%2F%2Fa.com%2Fb"), "https://a.com/b");
    }

    #[test]
    fn ssrf_gate_rejects_special_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.1.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(!is_public_ip(ip), "{ip} 应被拒绝");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "104.16.0.1", "2606:4700:4700::1111"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(is_public_ip(ip), "{ip} 应放行");
        }
        // IPv4-mapped IPv6 的私网地址要走内层判定
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(!is_public_ip(mapped));
    }

    #[test]
    fn bing_and_ddg_parsing() {
        let bing = r#"<ol id="b_results">
        <li class="b_algo" data-id><h2 class=""><a target="_blank" href="https://nginx.org/en/" h="ID=SERP,1"><strong>nginx</strong></a></h2>
        <div class="b_caption"><p class="b_lineclamp2">nginx (&quot;engine x&quot;) is an HTTP server &amp; proxy</p></div></li>
        <li class="b_algo"><h2><a href="https://example.com/a?b=1&amp;c=2">Second</a></h2><p>snippet two</p></li>
        <li class="b_algo"><h2><a href="javascript:void(0)">Bad</a></h2></li>
        </ol>"#;
        let hits = parse_bing(bing, 5);
        assert_eq!(hits.len(), 2, "非 http 链接应被丢弃：{hits:?}");
        assert_eq!(hits[0].title, "nginx");
        assert_eq!(hits[0].url, "https://nginx.org/en/");
        assert!(hits[0].snippet.contains("HTTP server & proxy"));
        assert_eq!(hits[1].url, "https://example.com/a?b=1&c=2");
        // count 生效
        assert_eq!(parse_bing(bing, 1).len(), 1);

        let ddg = r#"<div class="result"><a rel="nofollow" class="result__a"
            href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fdoc&amp;rut=abc">Doc Title</a>
            <a class="result__snippet" href="x">Snip <b>here</b></a></div>"#;
        let hits = parse_ddg(ddg, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.org/doc");
        assert_eq!(hits[0].title, "Doc Title");
        assert_eq!(hits[0].snippet, "Snip here");
    }

    #[test]
    fn html_to_text_drops_scripts_and_keeps_blocks() {
        let html = "<html><head><style>a{color:red}</style></head><body>\
            <script>var x = '<p>not text</p>';</script>\
            <h1>标题</h1><p>第一段</p><p>第二段 &amp; 更多</p>\
            <ul><li>项目一</li><li>项目二</li></ul></body></html>";
        let text = html_to_text(html);
        assert!(!text.contains("var x"), "脚本内容必须剔除：{text}");
        assert!(!text.contains("color:red"), "样式内容必须剔除：{text}");
        assert!(text.contains("标题"));
        assert!(text.contains("第一段"));
        assert!(text.contains("第二段 & 更多"));
        assert!(text.contains("项目一\n项目二"), "块级标签应成为换行：{text}");
        // 连续空行折叠
        assert!(!text.contains("\n\n\n"));
    }

    #[test]
    fn charset_detection() {
        assert_eq!(
            charset_of(Some("text/html; charset=GBK")).as_deref(),
            Some("GBK")
        );
        assert_eq!(
            charset_of(Some("text/html; charset=\"UTF-8\"; x=1")).as_deref(),
            Some("UTF-8")
        );
        assert_eq!(charset_of(Some("text/html")), None);
        let gbk_page = "<html><head><meta charset=\"gb2312\"><title>t</title></head>".as_bytes();
        assert_eq!(sniff_meta_charset(gbk_page).as_deref(), Some("gb2312"));
        // 非 UTF-8 页面要打上标记（避免把乱码当正文）
        let (_, flagged) = decode_body_flagged(&[0xff, 0xfe, 0x00, 0x41], Some("text/html; charset=gbk"));
        assert!(flagged);
        let (_, flagged) = decode_body_flagged("你好".as_bytes(), Some("text/html; charset=utf-8"));
        assert!(!flagged);
    }

    #[test]
    fn truncation_counts_chars_not_bytes() {
        assert_eq!(truncate_chars("短", 10), "短");
        let t = truncate_chars("中文中文中文", 4);
        assert!(t.starts_with("中文中文"));
        assert!(t.ends_with("（已截断）"));
    }
}
