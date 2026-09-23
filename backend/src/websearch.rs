//! 联网取信息：搜索与抓网页。
//!
//! 搜索走 cn.bing.com 的结果页（服务器实测可达；google 与 duckduckgo 都不通），
//! 解析出标题 / 链接 / 摘要三段；抓取则把页面剥成纯文本。
//!
//! 两处比逐字照搬多做的防护：
//! - **拦内网地址**：抓回来的网页内容是不可信的，里面若写「去访问
//!   http://192.168.x.x/...」，不该让面板替它去打内网；回环与私有网段一律拒绝。
//! - **长度上限**：正文塞进上下文会挤掉对话历史，截断到固定长度。

use regex::Regex;
use serde_json::json;
use std::net::ToSocketAddrs;

use crate::ai::{build_agent, transport_error};

/// 搜索 / 抓取的单次超时
const WEB_TIMEOUT_SECS: u64 = 20;
/// 抓回来正文的长度上限（字符）
const MAX_PAGE_CHARS: usize = 3000;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// 百分号编码。只保留 RFC 3986 的非保留字符，其余全部转义 —— 中文查询词
/// 必须编码后才能进 URL。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn html_unescape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&' {
            if let Some(semi) = chars[i..].iter().position(|&c| c == ';') {
                let ent: String = chars[i + 1..i + semi].iter().collect();
                let decoded = match ent.as_str() {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => {
                        let num = if let Some(r) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                            u32::from_str_radix(r, 16).ok()
                        } else {
                            ent.strip_prefix('#').and_then(|r| r.parse::<u32>().ok())
                        };
                        num.and_then(char::from_u32)
                    }
                };
                if let Some(c) = decoded {
                    out.push(c);
                    i += semi + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn strip_tags(s: &str) -> String {
    // 先用空串替换标签，再做实体解码：否则「&lt;b&gt;」这类文本会被二次剥离
    let re = Regex::new(r"<[^>]+>").expect("标签正则不会失败");
    html_unescape(&re.replace_all(s, ""))
}

/// 请求一个页面，返回 (状态码, 内容类型, 字节体)
fn http_get(url: &str) -> Result<(u16, String, Vec<u8>), String> {
    let agent = build_agent(WEB_TIMEOUT_SECS);
    let req = agent
        .get(url)
        .set("User-Agent", UA)
        .set("Accept-Language", "zh-CN,zh;q=0.9");
    match req.call() {
        Ok(resp) => {
            let status = resp.status();
            let ctype = resp.header("content-type").unwrap_or("").to_string();
            let mut body: Vec<u8> = Vec::new();
            use std::io::Read as _;
            match resp.into_reader().read_to_end(&mut body) {
                Ok(_) => Ok((status, ctype, body)),
                Err(e) => Err(format!("读取响应失败：{e}")),
            }
        }
        // 4xx/5xx 也是有效信息，把状态码带回去
        Err(ureq::Error::Status(code, _)) => Ok((code, String::new(), Vec::new())),
        Err(ureq::Error::Transport(t)) => Err(transport_error(&t, WEB_TIMEOUT_SECS)),
    }
}

/// 是不是指向内网 / 回环的地址。解析不出来也当作不安全。
fn is_private_target(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return true;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // 带用户名密码的（http://a@b/）一律拒掉，免得被用来绕解析
    if authority.is_empty() || authority.contains('@') {
        return true;
    }
    let default_port = if url.starts_with("https://") { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().unwrap_or(default_port))
        }
        _ => (authority.to_string(), default_port),
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match (host.as_str(), port).to_socket_addrs() {
        Ok(addrs) => addrs.map(|a| a.ip()).any(|ip| match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        }),
        Err(_) => true,
    }
}

/// 搜索：返回若干条「标题 / 链接 / 摘要」
pub fn search(query: &str, count: usize) -> String {
    let query = query.trim();
    if query.is_empty() {
        return "搜索词为空".into();
    }
    let count = count.clamp(1, 10);
    let url = format!(
        "https://cn.bing.com/search?q={}&count={count}",
        percent_encode(query)
    );
    let page = match http_get(&url) {
        Ok((200, _, body)) => String::from_utf8_lossy(&body).to_string(),
        Ok((status, _, _)) => return format!("搜索失败：HTTP {status}"),
        Err(e) => return format!("搜索失败：{e}"),
    };

    let re_block = Regex::new(r#"(?s)<li class="b_algo".*?</li>"#).expect("正则不会失败");
    // 标题锚点优先从 h2 里找：块里第一个 <a> 常常是站点名那个，带上 URL 与「›」
    let re_title = Regex::new(r#"(?s)<h2[^>]*>.*?<a[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#)
        .expect("正则不会失败");
    let re_anchor = Regex::new(r#"(?s)<a[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#).expect("正则不会失败");
    let re_snip = Regex::new(r"(?s)<p[^>]*>(.*?)</p>").expect("正则不会失败");
    let re_urlish = Regex::new(r"https?://\S+").expect("正则不会失败");

    let mut items: Vec<serde_json::Value> = Vec::new();
    for block in re_block.find_iter(&page) {
        let block = block.as_str();
        let caps = re_title.captures(block).or_else(|| re_anchor.captures(block));
        let Some(caps) = caps else {
            continue;
        };
        let link = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        // 标题里可能还夹着 URL 片段，剔掉并清掉两端的「›」
        let raw = re_urlish.replace_all(caps.get(2).map(|m| m.as_str()).unwrap_or(""), "");
        let title = strip_tags(&raw)
            .trim()
            .trim_matches(|c: char| c == '›' || c == '·' || c.is_whitespace())
            .to_string();
        let snippet = re_snip
            .captures(block)
            .map(|c| strip_tags(c.get(1).map(|m| m.as_str()).unwrap_or("")).trim().to_string())
            .unwrap_or_default();
        if title.is_empty() && link.is_empty() {
            continue;
        }
        items.push(json!({ "title": title, "url": link, "snippet": snippet }));
        if items.len() >= count {
            break;
        }
    }
    if items.is_empty() {
        return "没有搜到结果（可能是搜索页结构变了，或被限流）".into();
    }
    json!({ "query": query, "results": items }).to_string()
}

/// 抓取网页并剥成纯文本
pub fn fetch(url: &str) -> String {
    let url = url.trim();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return "只支持 http/https 网址".into();
    }
    if is_private_target(url) {
        return "这个地址解析不了，或指向内网/回环地址，已拒绝抓取".into();
    }
    let (status, ctype, body) = match http_get(url) {
        Ok(x) => x,
        Err(e) => return format!("抓取失败：{e}"),
    };
    if status != 200 {
        return format!("抓取失败：HTTP {status}");
    }
    if ctype.contains("application/pdf") || ctype.contains("image/") || ctype.contains("audio/")
        || ctype.contains("video/")
    {
        return format!("这个地址不是网页（{ctype}），读不了。");
    }
    // 编码：UTF-8 优先，失败就按 GBK 兜底（中文站点常见）
    let page = String::from_utf8(body.clone())
        .ok()
        .or_else(|| decode_gbk(&body))
        .unwrap_or_else(|| String::from_utf8_lossy(&body).to_string());

    // 注：rust regex 不支持反向引用 \1，改为三条独立正则分别剥离 script/style/noscript
    let re_hidden = Regex::new(r"(?is)<script[^>]*>.*?</script>").expect("正则不会失败");
    let page = re_hidden.replace_all(&page, " ").to_string();
    let re_hidden = Regex::new(r"(?is)<style[^>]*>.*?</style>").expect("正则不会失败");
    let page = re_hidden.replace_all(&page, " ").to_string();
    let re_hidden = Regex::new(r"(?is)<noscript[^>]*>.*?</noscript>").expect("正则不会失败");
    let page = re_hidden.replace_all(&page, " ").to_string();
    let re_tag = Regex::new(r"(?is)<[^>]+>").expect("正则不会失败");
    let text = re_tag.replace_all(&page, " ").to_string();
    let re_ws = Regex::new(r"\s+").expect("正则不会失败");
    let text = strip_tags(&re_ws.replace_all(&text, " "));

    let text = text.trim();
    if text.is_empty() {
        return "(页面没有正文内容)".into();
    }
    if text.chars().count() > MAX_PAGE_CHARS {
        let head: String = text.chars().take(MAX_PAGE_CHARS).collect();
        return format!("{head}\n（正文过长，已截断）");
    }
    text.to_string()
}

/// GBK 兜底：只做最小实现 —— 用编码表逐字节还原常见汉字区间的做法不划算，
/// 这里交给系统命令 iconv 处理，失败就返回 None 让调用方退回 lossy 解码。
fn decode_gbk(data: &[u8]) -> Option<String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new("iconv")
        .args(["-f", "GBK", "-t", "UTF-8"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(data).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}
