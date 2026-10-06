//! 安全入口：登录之前的访问闸门（P0 对标补齐）。
//!
//! 两项独立能力，任一项未配置即视为关闭：
//! - **访问路径前缀**：面板只在该前缀下可达，其余路径一律 404（不泄露面板存在）。
//!   请求进入路由前剥掉前缀，前端靠注入的 `<base href>` 把资源与接口请求也带上前缀。
//! - **IP 白名单**：仅放行名单内的来源地址（支持单 IP 与 CIDR）。
//!
//! 逐请求读取配置（SQLite 本地读，代价可忽略），换取「改完立即生效」且无缓存
//! 失效问题；中间件套在整棵路由树之外（见 `api::router`），因此对 SPA、/api 与
//! 静态资源同样生效，且先于路径匹配运行 —— 前缀剥离才能真正改变被匹配的路径。

use anyhow::{bail, Result};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::http::uri::{PathAndQuery, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, SocketAddr};

use crate::AppState;

/// 访问路径前缀（空 = 未启用）
pub const KEY_ENTRANCE: &str = "security_entrance";
/// IP 白名单（空 = 不限制）
pub const KEY_ALLOWLIST: &str = "security_allowlist";

/// 入口路径不允许与既有顶层路径重名，避免出现 `/api/api/...` 这种自相矛盾的地址
const RESERVED: [&str; 6] = ["api", "health", "assets", "fonts", "favicon.ico", "vite.svg"];

/// 归一化入口路径：去除首尾斜杠，校验字符与长度。空串表示关闭。
pub fn normalize_entrance(raw: &str) -> Result<String> {
    let s = raw.trim().trim_matches('/');
    if s.is_empty() {
        return Ok(String::new());
    }
    if !(4..=64).contains(&s.len()) {
        bail!("入口路径长度需为 4-64 个字符");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("入口路径只能包含字母、数字、连字符与下划线");
    }
    if RESERVED.contains(&s.to_ascii_lowercase().as_str()) {
        bail!("入口路径不能使用保留字：{}", RESERVED.join(" / "));
    }
    Ok(s.to_string())
}

/// 单条放行网段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpNet {
    V4 { base: u32, mask: u32 },
    V6 { base: u128, mask: u128 },
}

impl IpNet {
    /// 该地址是否落在网段内
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self, ip) {
            (IpNet::V4 { base, mask }, IpAddr::V4(v4)) => u32::from(v4) & mask == *base,
            (IpNet::V6 { base, mask }, IpAddr::V6(v6)) => u128::from(v6) & mask == *base,
            _ => false,
        }
    }
}

/// 解析一条白名单条目：`10.0.0.1`、`10.0.0.0/8`、`fd00::/8` 均可
pub fn parse_net(raw: &str) -> Result<IpNet> {
    let s = raw.trim();
    let (addr_part, prefix_part) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let addr: IpAddr = addr_part
        .parse()
        .map_err(|_| anyhow::anyhow!("无法解析 IP：{raw}"))?;
    match addr {
        IpAddr::V4(v4) => {
            let bits = match prefix_part {
                Some(p) => p
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n <= 32)
                    .ok_or_else(|| anyhow::anyhow!("IPv4 前缀非法：{raw}"))?,
                None => 32,
            };
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            Ok(IpNet::V4 {
                base: u32::from(v4) & mask,
                mask,
            })
        }
        IpAddr::V6(v6) => {
            let bits = match prefix_part {
                Some(p) => p
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n <= 128)
                    .ok_or_else(|| anyhow::anyhow!("IPv6 前缀非法：{raw}"))?,
                None => 128,
            };
            let mask = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            Ok(IpNet::V6 {
                base: u128::from(v6) & mask,
                mask,
            })
        }
    }
}

/// 解析整个白名单文本：以空白、逗号或换行分隔；任一条非法即整体拒绝
pub fn parse_allowlist(raw: &str) -> Result<Vec<IpNet>> {
    let mut out = Vec::new();
    for item in raw.split([' ', '\t', '\r', '\n', ',']).filter(|s| !s.trim().is_empty()) {
        out.push(parse_net(item)?);
    }
    Ok(out)
}

/// 取请求来源地址：直连时用对端地址；对端是回环（面板前有反代）时采用
/// `X-Forwarded-For` 的第一跳 —— 否则反代后面所有请求都会是同一条 127.0.0.1。
/// 直连场景不信任该头，避免客户端伪造头部绕过白名单。
fn client_ip(req: &Request) -> Option<IpAddr> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)?;
    if peer.ip().is_loopback()
        && let Some(first) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        && let Ok(ip) = first.trim().parse::<IpAddr>()
    {
        return Some(ip);
    }
    Some(peer.ip())
}

/// 把请求路径的前缀剥掉（保留查询串）；返回 None 表示该路径不在入口之下。
fn strip_prefix(path: &str, prefix: &str) -> Option<String> {
    if path == prefix {
        return Some("/".to_string());
    }
    path.strip_prefix(prefix)
        .filter(|rest| rest.starts_with('/'))
        .map(|rest| rest.to_string())
}

/// 最外层访问闸门：先看 IP 白名单，再看入口前缀。
pub async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    // 1) IP 白名单：配置非法或读库失败都按「不限制」处理，避免把自己锁在门外
    let allow_raw = state
        .db
        .get_setting_async(KEY_ALLOWLIST)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let nets = parse_allowlist(&allow_raw).unwrap_or_default();
    if !nets.is_empty() {
        match client_ip(&req) {
            Some(ip) if nets.iter().any(|n| n.contains(ip)) => {}
            _ => {
                return (StatusCode::FORBIDDEN, "来源地址不在白名单内").into_response();
            }
        }
    }

    // 2) 入口前缀：不在前缀下的请求静默 404
    let entrance = state
        .db
        .get_setting_async(KEY_ENTRANCE)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let entrance = entrance.trim().trim_matches('/').to_string();
    if !entrance.is_empty() {
        let prefix = format!("/{entrance}");
        let Some(rest) = strip_prefix(req.uri().path(), &prefix) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let query = req
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        if let Ok(pq) = format!("{rest}{query}").parse::<PathAndQuery>() {
            let mut parts = req.uri().clone().into_parts();
            parts.path_and_query = Some(pq);
            if let Ok(uri) = Uri::from_parts(parts) {
                *req.uri_mut() = uri;
            }
        }
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_entrance() {
        assert_eq!(normalize_entrance("").unwrap(), "");
        assert_eq!(normalize_entrance("  /abc123/  ").unwrap(), "abc123");
        assert_eq!(normalize_entrance("abc-1_2").unwrap(), "abc-1_2");
        assert!(normalize_entrance("abc").is_err()); // 太短
        assert!(normalize_entrance("a".repeat(65).as_str()).is_err());
        assert!(normalize_entrance("abc def").is_err());
        assert!(normalize_entrance("abc/def").is_err()); // 斜杠已在 trim 阶段去掉，中间的不允许
        assert!(normalize_entrance("api").is_err()); // 保留字
        assert!(normalize_entrance("Assets").is_err());
    }

    #[test]
    fn matches_ipv4_and_ipv6_nets() {
        let v4: IpAddr = "10.1.2.3".parse().unwrap();
        let net = parse_net("10.1.0.0/16").unwrap();
        assert!(net.contains(v4));
        assert!(!net.contains("10.2.0.1".parse().unwrap()));
        // 不带前缀 = 单机
        let one = parse_net("192.168.1.5").unwrap();
        assert!(one.contains("192.168.1.5".parse().unwrap()));
        assert!(!one.contains("192.168.1.6".parse().unwrap()));
        // 0 前缀放行全部
        let all = parse_net("0.0.0.0/0").unwrap();
        assert!(all.contains("203.0.113.9".parse().unwrap()));
        // IPv6
        let v6 = parse_net("fd00::/8").unwrap();
        assert!(v6.contains("fd00::1".parse().unwrap()));
        assert!(!v6.contains("fe80::1".parse().unwrap()));
        // 类型不匹配不放行
        assert!(!net.contains("fd00::1".parse().unwrap()));
    }

    #[test]
    fn parses_allowlist_and_rejects_bad_entries() {
        let nets = parse_allowlist("10.0.0.1, 192.168.0.0/24\nfd00::/8\t127.0.0.1").unwrap();
        assert_eq!(nets.len(), 4);
        assert!(parse_allowlist("10.0.0.1, not-an-ip").is_err());
        assert!(parse_allowlist("10.0.0.0/33").is_err());
        assert!(parse_allowlist("   ").unwrap().is_empty());
    }

    #[test]
    fn strips_prefix_only_under_entrance() {
        assert_eq!(strip_prefix("/abc", "/abc").as_deref(), Some("/"));
        assert_eq!(strip_prefix("/abc/", "/abc").as_deref(), Some("/"));
        assert_eq!(
            strip_prefix("/abc/dashboard", "/abc").as_deref(),
            Some("/dashboard")
        );
        assert_eq!(
            strip_prefix("/abc/api/login", "/abc").as_deref(),
            Some("/api/login")
        );
        // 相似前缀不能被误伤
        assert_eq!(strip_prefix("/abcd", "/abc"), None);
        assert_eq!(strip_prefix("/other", "/abc"), None);
        assert_eq!(strip_prefix("/", "/abc"), None);
    }
}
