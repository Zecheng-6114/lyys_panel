//! 网站管理：Nginx 站点（静态目录托管 / 反向代理），可选 HTTPS。
//!
//! 边界与设计：
//! - 只管理面板自己生成的站点文件，命名统一为 `/etc/nginx/conf.d/lyys-site-<id>.conf`。
//!   Debian 默认 `nginx.conf` 已 `include /etc/nginx/conf.d/*.conf`；Arch 默认**没有**
//!   （连 conf.d 目录都不存在），故首次启用站点时会在 `http {}` 块插入一行带标记的
//!   include（插入前先备份 `nginx.conf`）。不碰 `sites-available` 里用户自己的站点。
//! - 配置由面板按字段生成（表单驱动），不接受原始配置文本 —— 避免写出语义正确但
//!   把 nginx 写挂的文件。
//! - 应用流程是「写文件 → `nginx -t` 校验 → 通过才 reload」：校验失败即把文件回滚成
//!   改动前内容并报错。绝不让一份坏配置停留在 conf.d 里。
//! - TLS 证书：可上传正式证书/私钥，也可由面板用 rcgen 生成自签证书。私钥一律
//!   0600，且绝不经接口回显（列表只回 `tls` / `cert_set` 布尔）。
//!
//! 站点元数据存 `数据目录/websites/index.json`，证书落 `数据目录/websites/<id>.{crt,key}`，
//! nginx 配置文件作为派生产物按需重写；列表直接读注册表，无须反向解析 nginx 配置。
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::cmd::{self, Budget};

/// 面板自管站点配置目录
const CONF_DIR: &str = "/etc/nginx/conf.d";
const PREFIX: &str = "lyys-site-";
/// nginx 主配置（用于确保站点目录被 include）
const NGINX_CONF: &str = "/etc/nginx/nginx.conf";
/// 注入到 `http {}` 块的 include 行，末尾标记便于人工识别是面板所加
const INCLUDE_LINE: &str = "    include /etc/nginx/conf.d/*.conf;  # LYYS Panel managed\n";

/// 站点类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteKind {
    /// 静态目录托管
    Static,
    /// 反向代理到上游
    Proxy,
}

/// 单个站点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    /// 站点标识（十六进制），用于拼配置文件名
    pub id: String,
    /// `server_name`，空格分隔多个域名（可用 `*.` 通配、`_` 作兜底）
    pub name: String,
    pub kind: SiteKind,
    /// 静态站点根目录（绝对路径）
    #[serde(default)]
    pub root: String,
    /// 反向代理上游（http(s)://host[:port][/path]）
    #[serde(default)]
    pub upstream: String,
    /// 监听端口
    pub listen: u16,
    /// 是否启用 HTTPS
    #[serde(default)]
    pub tls: bool,
    /// 是否启用（停用即从 conf.d 移除配置）
    #[serde(default)]
    pub enabled: bool,
}

/// nginx 运行状态（供界面判断是否已安装）
#[derive(Serialize)]
pub struct Status {
    pub installed: bool,
    pub active: bool,
    /// 版本号，如 `1.24.0`；取不到时为空
    pub version: String,
}

/// 生成一个不与现有站点冲突的标识（十六进制时间戳，天然是合法 slug）
pub fn new_id(existing: &[Site]) -> String {
    let mut n = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as u128;
    loop {
        let id = format!("{n:x}");
        if !existing.iter().any(|s| s.id == id) {
            return id;
        }
        n += 1;
    }
}

/// 标识白名单：仅小写字母、数字、连字符，字母数字开头，1–40 字符
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `server_name` 白名单：空格分隔的若干域名/IP/通配符，字符集限定到
/// 字母数字与 `.` `-` `_`，并允许 `*.` 前缀。任何换行 / `;` / `{` 都被排除。
///
/// 刻意用 `split(' ')` 而非 `split_whitespace()`：后者会把换行、制表符也当作
/// 分隔符，于是 `a\nb` 会被拆成两个合法域名放行 —— 而它写进配置会直接破坏
/// `server_name ...;` 的结构。这里改用单空格分隔，并显式拒绝控制字符。
fn valid_server_name(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > 253 || s.chars().any(|c| c.is_control()) {
        return false;
    }
    s.split(' ').filter(|p| !p.is_empty()).all(|part| {
        let p = part.strip_prefix("*.").unwrap_or(part);
        !p.is_empty()
            && !p.contains("..")
            && !p.starts_with('.')
            && !p.ends_with('.')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    })
}

/// 目录路径白名单：绝对路径，禁空白与控制字符，禁 `..`、`;`、`{`、`}`
fn valid_dir(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('/')
        && s.len() <= 512
        && !s.split(['/', '\\']).any(|seg| seg == "..")
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ';' | '{' | '}' | '"' | '\''))
}

/// 上游地址白名单：http(s):// 开头，无空白 / 换行 / `;` `{` `}`
fn valid_upstream(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("http://") || s.starts_with("https://"))
        && s.len() <= 512
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ';' | '{' | '}' | '"' | '\''))
}

fn validate(site: &Site) -> Result<()> {
    if !valid_id(&site.id) {
        bail!("站点标识非法");
    }
    if !valid_server_name(&site.name) {
        bail!("服务器名（server_name）非法：仅限域名 / IP，多个用空格分隔");
    }
    if site.listen == 0 {
        bail!("监听端口必须在 1–65535 之间");
    }
    match site.kind {
        SiteKind::Static => {
            if site.root.trim().is_empty() || !valid_dir(&site.root) {
                bail!("静态站点根目录必须是不含空白的绝对路径");
            }
        }
        SiteKind::Proxy => {
            if !valid_upstream(&site.upstream) {
                bail!("上游地址必须以 http(s):// 开头且不含空白");
            }
        }
    }
    Ok(())
}

/// 站点配置文件名
fn conf_path(id: &str) -> PathBuf {
    Path::new(CONF_DIR).join(format!("{PREFIX}{id}.conf"))
}

fn websites_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("websites")
}

/// 证书与私钥路径
fn cert_paths(data_dir: &Path, id: &str) -> (PathBuf, PathBuf) {
    let dir = websites_dir(data_dir);
    (dir.join(format!("{id}.crt")), dir.join(format!("{id}.key")))
}

fn index_path(data_dir: &Path) -> PathBuf {
    websites_dir(data_dir).join("index.json")
}

// ---------- nginx 配置生成 ----------

/// 按站点字段生成一份自包含的 `server {}` 配置。
///
/// 只输出最小可用指令：TLS 用 `listen <port> ssl;`（不写 `http2`，以避开
/// nginx 1.25 起对 `listen ... http2` 的弃用告警），其余走 nginx 默认值。
fn conf_body(site: &Site, cert: &Path, key: &Path) -> String {
    let mut s = String::new();
    s.push_str("# 由 LYYS Panel 生成，请勿手改（保存站点时会整体重写）\n");
    s.push_str("server {\n");
    if site.tls {
        s.push_str(&format!("    listen {} ssl;\n", site.listen));
    } else {
        s.push_str(&format!("    listen {};\n", site.listen));
    }
    s.push_str(&format!("    server_name {};\n", site.name.trim()));
    if site.tls {
        s.push_str(&format!("    ssl_certificate {};\n", cert.display()));
        s.push_str(&format!("    ssl_certificate_key {};\n", key.display()));
    }
    match site.kind {
        SiteKind::Static => {
            s.push_str(&format!("    root {};\n", site.root.trim()));
            s.push_str("    index index.html;\n");
            s.push_str("    location / {\n        try_files $uri $uri/ =404;\n    }\n");
        }
        SiteKind::Proxy => {
            s.push_str("    location / {\n");
            s.push_str(&format!("        proxy_pass {};\n", site.upstream.trim()));
            s.push_str("        proxy_set_header Host $host;\n");
            s.push_str("        proxy_set_header X-Real-IP $remote_addr;\n");
            s.push_str("        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n");
            s.push_str("        proxy_set_header X-Forwarded-Proto $scheme;\n");
            s.push_str("    }\n");
        }
    }
    s.push_str("}\n");
    s
}

// ---------- 证书 ----------

/// 用 rcgen 生成自签证书（有效期 10 年，与面板自身证书同口径）。
/// SAN 取 server_name 的各段（`_` 兜底名与空段跳过），全空则退回 localhost。
fn generate_self_signed(name: &str, cert_path: &Path, key_path: &Path) -> Result<()> {
    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent).context("创建站点证书目录失败")?;
    }
    let mut sans: Vec<String> = name
        .split_whitespace()
        .filter(|s| !s.is_empty() && *s != "_")
        .map(|s| s.to_string())
        .collect();
    if sans.is_empty() {
        sans.push("localhost".to_string());
    }

    let now = time::OffsetDateTime::now_utc();
    let mut params = rcgen::CertificateParams::new(sans).context("生成自签证书参数失败")?;
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(3650);
    let cn = name.split_whitespace().next().unwrap_or("LYYS Site");
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String(cn.to_string()),
    );
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("生成站点私钥失败")?;
    let cert = params.self_signed(&key_pair).context("生成自签证书失败")?;

    write_key(key_path, &key_pair.serialize_pem())?;
    std::fs::write(cert_path, cert.pem()).context("写入站点证书失败")?;
    Ok(())
}

/// 写私钥文件（0600）并确保父目录存在
fn write_key(path: &Path, pem: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("创建站点证书目录失败")?;
    }
    std::fs::write(path, pem).context("写入站点私钥失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .context("设置站点私钥权限失败")?;
    }
    Ok(())
}

// ---------- 注册表 ----------

fn load_index(data_dir: &Path) -> Vec<Site> {
    std::fs::read_to_string(index_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_index(data_dir: &Path, sites: &[Site]) -> Result<()> {
    let dir = websites_dir(data_dir);
    std::fs::create_dir_all(&dir).context("创建站点目录失败")?;
    let s = serde_json::to_string_pretty(sites).context("序列化站点失败")?;
    std::fs::write(index_path(data_dir), s).context("写入站点注册表失败")?;
    Ok(())
}

/// 站点列表（按 id 排序，顺序稳定）
pub fn list(data_dir: &Path) -> Vec<Site> {
    let mut sites = load_index(data_dir);
    sites.sort_by(|a, b| a.id.cmp(&b.id));
    sites
}

// ---------- nginx 交互 ----------

/// 在常见目录里定位命令。面板以 root 运行，服务的 PATH 往往不含 /usr/sbin。
fn which(name: &str) -> Option<String> {
    for dir in ["/usr/sbin", "/sbin", "/usr/local/sbin", "/usr/bin", "/bin", "/usr/local/bin"] {
        let p = format!("{dir}/{name}");
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

/// nginx 是否可用（未安装则无法管理站点）
pub fn installed() -> bool {
    which("nginx").is_some()
}

/// 探测 nginx 运行状态与版本
pub async fn status() -> Status {
    let Some(nginx) = which("nginx") else {
        return Status {
            installed: false,
            active: false,
            version: String::new(),
        };
    };
    // 版本：`nginx -v` 输出走 stderr，形如 `nginx version: nginx/1.24.0`
    let mut v = Command::new(&nginx);
    v.arg("-v");
    let version = match cmd::run(&mut v, Budget::query(10)).await {
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            text.rsplit('/')
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_default()
        }
        Err(_) => String::new(),
    };
    // 是否在运行：is-active 退出码 0 即 active
    let mut c = Command::new("systemctl");
    c.args(["is-active", "nginx"]);
    let active = matches!(cmd::run(&mut c, Budget::query(10)).await, Ok(o) if o.success());
    Status {
        installed: true,
        active,
        version,
    }
}

/// 确保 `nginx.conf` 的 `http {}` 块 include 了站点目录。
///
/// Debian 默认已包含；Arch 默认没有，此时在 `http {` 行之后插入一行带标记的
/// include，并先把原文件备份为 `nginx.conf.lyys-bak`（仅备份一次），便于人工回退。
fn ensure_include() -> Result<()> {
    std::fs::create_dir_all(CONF_DIR).context("创建 nginx 配置目录失败")?;
    let text = std::fs::read_to_string(NGINX_CONF).context("读取 nginx.conf 失败")?;
    // 已生效的 include 里出现该目录即无需处理（跳过被注释掉的行）
    let included = text
        .lines()
        .any(|l| !l.trim_start().starts_with('#') && l.contains("conf.d/*.conf"));
    if included {
        return Ok(());
    }

    let mut out = String::with_capacity(text.len() + INCLUDE_LINE.len());
    let mut injected = false;
    for line in text.lines() {
        out.push_str(line);
        out.push('\n');
        if !injected {
            let t = line.trim_start();
            if !t.starts_with('#') && t.starts_with("http") && line.trim_end().ends_with('{') {
                out.push_str(INCLUDE_LINE);
                injected = true;
            }
        }
    }
    if !injected {
        bail!("未能在 nginx.conf 中定位 http 块，请手动加入 include {CONF_DIR}/*.conf;");
    }

    let bak = format!("{NGINX_CONF}.lyys-bak");
    if !Path::new(&bak).exists() {
        std::fs::copy(NGINX_CONF, &bak).context("备份 nginx.conf 失败")?;
    }
    std::fs::write(NGINX_CONF, out).context("写入 nginx.conf 失败")?;
    tracing::info!("已在 nginx.conf 的 http 块接入站点目录 {CONF_DIR}");
    Ok(())
}

/// `nginx -t` 校验当前全部配置；失败时把 stderr 末尾几行带回，便于定位
async fn nginx_test() -> Result<()> {
    let Some(nginx) = which("nginx") else {
        bail!("未安装 nginx");
    };
    let mut c = Command::new(&nginx);
    c.arg("-t");
    let out = cmd::run(&mut c, Budget::query(20))
        .await
        .context("调用 nginx 失败")?;
    if !out.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(4).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("nginx 配置校验未通过：{}", tail.join(" | "));
    }
    Ok(())
}

/// systemctl 写命令；失败只记日志并返回统一文案
async fn systemctl_ok(args: &[&str], what: &str) -> Result<()> {
    let mut c = Command::new("systemctl");
    c.args(args);
    let out = cmd::run(&mut c, Budget::systemd(20))
        .await
        .context("调用 systemctl 失败")?;
    if !out.success() {
        tracing::warn!(
            "systemctl {} stderr：{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        bail!("{what}失败，详见服务端日志");
    }
    Ok(())
}

/// 确保 nginx 已启用并在运行，然后 reload 以载入新配置
async fn apply_reload() -> Result<()> {
    systemctl_ok(&["enable", "--now", "nginx"], "启动 nginx").await?;
    systemctl_ok(&["reload", "nginx"], "重载 nginx").await
}

/// 把配置文件恢复成改动前内容（None 表示此前不存在 → 删除）
fn restore_conf(conf: &Path, prev: Option<String>) {
    match prev {
        Some(text) => {
            let _ = std::fs::write(conf, text);
        }
        None => {
            let _ = std::fs::remove_file(conf);
        }
    }
}

// ---------- 增删改 ----------

/// 新建或更新站点。`new_cert` 为本次上传的 `(证书, 私钥)` PEM，None 表示沿用
/// 已存文件（TLS 且尚无文件时自动生成自签证书）。
///
/// 顺序：校验 → 落证书 → 写配置（失败回滚）→ `nginx -t`（失败回滚）→ 重载 → 落注册表。
pub async fn save(
    data_dir: &Path,
    site: &Site,
    is_new: bool,
    new_cert: Option<(String, String)>,
) -> Result<()> {
    validate(site)?;
    if !installed() {
        bail!("未安装 nginx，请先在「软件」页安装后再管理站点");
    }

    let sites = load_index(data_dir);
    let exists = sites.iter().any(|s| s.id == site.id);
    if is_new && exists {
        bail!("站点标识已存在");
    }
    if !is_new && !exists {
        bail!("站点不存在");
    }

    let (cert_path, key_path) = cert_paths(data_dir, &site.id);
    if site.tls {
        match new_cert {
            Some((c, k)) => {
                if !c.contains("BEGIN CERTIFICATE") || !k.contains("BEGIN") {
                    bail!("证书或私钥不是有效的 PEM 内容");
                }
                std::fs::write(&cert_path, c).context("写入站点证书失败")?;
                write_key(&key_path, &k)?;
            }
            None => {
                if !(cert_path.exists() && key_path.exists()) {
                    generate_self_signed(&site.name, &cert_path, &key_path)?;
                }
            }
        }
    }

    let conf = conf_path(&site.id);
    let prev = std::fs::read_to_string(&conf).ok();
    if site.enabled {
        // 启用站点前先确保 conf.d 被 nginx 纳入（Arch 默认没有）
        ensure_include()?;
        std::fs::write(&conf, conf_body(site, &cert_path, &key_path)).context("写入站点配置失败")?;
        if let Err(e) = nginx_test().await {
            restore_conf(&conf, prev);
            return Err(e);
        }
    } else {
        let _ = std::fs::remove_file(&conf);
    }

    if let Err(e) = apply_reload().await {
        restore_conf(&conf, prev);
        return Err(e);
    }

    let mut sites = sites;
    sites.retain(|s| s.id != site.id);
    sites.push(site.clone());
    save_index(data_dir, &sites)?;
    tracing::info!("已保存站点 {}（{}，端口 {}）", site.id, site.name, site.listen);
    Ok(())
}

/// 删除站点：移除配置、证书与注册表条目
pub async fn delete(data_dir: &Path, id: &str) -> Result<()> {
    if !valid_id(id) {
        bail!("站点标识非法");
    }
    let mut sites = load_index(data_dir);
    if !sites.iter().any(|s| s.id == id) {
        bail!("站点不存在");
    }
    let _ = std::fs::remove_file(conf_path(id));
    // 配置已移除，尽力重载使改动生效（nginx 未运行等情况下不阻断删除）
    if installed()
        && let Err(e) = apply_reload().await
    {
        tracing::warn!("删除站点后重载 nginx 失败（忽略）：{e:#}");
    }
    let (cert_path, key_path) = cert_paths(data_dir, id);
    let _ = std::fs::remove_file(cert_path);
    let _ = std::fs::remove_file(key_path);
    sites.retain(|s| s.id != id);
    save_index(data_dir, &sites)?;
    tracing::info!("已删除站点 {id}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> Site {
        Site {
            id: "a1b2c3".into(),
            name: "example.com www.example.com".into(),
            kind: SiteKind::Proxy,
            root: String::new(),
            upstream: "http://127.0.0.1:3000".into(),
            listen: 80,
            tls: false,
            enabled: true,
        }
    }

    #[test]
    fn id_and_name_validation() {
        assert!(valid_id("a1b2c3"));
        assert!(!valid_id(""));
        assert!(!valid_id("UPPER"));
        assert!(!valid_id("has/slash"));

        assert!(valid_server_name("example.com"));
        assert!(valid_server_name("example.com www.example.com"));
        assert!(valid_server_name("*.example.com"));
        assert!(valid_server_name("_"));
        assert!(!valid_server_name(""));
        // 注入与结构破坏字符必须被拒
        assert!(!valid_server_name("a;b"));
        assert!(!valid_server_name("a\nb"));
        assert!(!valid_server_name("a{b}"));
        assert!(!valid_server_name("a b c;"));
        assert!(!valid_server_name(".."));
    }

    #[test]
    fn dir_and_upstream_validation() {
        assert!(valid_dir("/var/www/html"));
        assert!(!valid_dir("relative/path"));
        assert!(!valid_dir("/var/www/../etc"));
        assert!(!valid_dir("/var/www/my site"));
        assert!(!valid_dir("/var/www;rm"));

        assert!(valid_upstream("http://127.0.0.1:3000"));
        assert!(valid_upstream("https://backend.local/app"));
        assert!(!valid_upstream("ftp://x"));
        assert!(!valid_upstream("http://a b"));
        assert!(!valid_upstream("http://x;y"));
    }

    #[test]
    fn validate_requires_kind_fields() {
        let mut s = site();
        s.upstream = "".into();
        assert!(validate(&s).is_err(), "反向代理缺上游应被拒");

        let mut s = site();
        s.kind = SiteKind::Static;
        s.root = "".into();
        assert!(validate(&s).is_err(), "静态站点缺根目录应被拒");

        let mut s = site();
        s.kind = SiteKind::Static;
        s.root = "/var/www/html".into();
        assert!(validate(&s).is_ok());

        let mut s = site();
        s.listen = 0;
        assert!(validate(&s).is_err());
    }

    #[test]
    fn conf_body_proxy_and_static_and_tls() {
        let cert = Path::new("/var/lib/lyys-panel/websites/a1b2c3.crt");
        let key = Path::new("/var/lib/lyys-panel/websites/a1b2c3.key");

        let proxy = conf_body(&site(), cert, key);
        assert!(proxy.contains("listen 80;"));
        assert!(proxy.contains("server_name example.com www.example.com;"));
        assert!(proxy.contains("proxy_pass http://127.0.0.1:3000;"));
        assert!(proxy.contains("proxy_set_header X-Forwarded-Proto $scheme;"));
        assert!(!proxy.contains("ssl_certificate"));

        let mut st = site();
        st.kind = SiteKind::Static;
        st.root = "/var/www/html".into();
        st.tls = true;
        st.listen = 443;
        let static_conf = conf_body(&st, cert, key);
        assert!(static_conf.contains("listen 443 ssl;"));
        assert!(static_conf.contains("root /var/www/html;"));
        assert!(static_conf.contains("try_files $uri $uri/ =404;"));
        assert!(static_conf.contains("ssl_certificate /var/lib/lyys-panel/websites/a1b2c3.crt;"));
        assert!(static_conf.contains("ssl_certificate_key /var/lib/lyys-panel/websites/a1b2c3.key;"));
    }

    #[test]
    fn index_roundtrip_and_new_id() {
        let dir = std::env::temp_dir().join(format!(
            "lyys_sites_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(list(&dir).is_empty());

        let mut a = site();
        a.id = "aaa".into();
        let mut b = site();
        b.id = "bbb".into();
        b.enabled = false;
        save_index(&dir, &[a.clone(), b.clone()]).unwrap();

        let got = list(&dir);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "aaa");
        assert!(!got[1].enabled);

        let fresh = new_id(&got);
        assert!(valid_id(&fresh));
        assert!(!got.iter().any(|s| s.id == fresh));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_signed_cert_is_written() {
        let dir = std::env::temp_dir().join(format!(
            "lyys_site_cert_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (cert, key) = (dir.join("s.crt"), dir.join("s.key"));
        generate_self_signed("example.com", &cert, &key).unwrap();
        let cert_pem = std::fs::read_to_string(&cert).unwrap();
        let key_pem = std::fs::read_to_string(&key).unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "私钥必须是 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
