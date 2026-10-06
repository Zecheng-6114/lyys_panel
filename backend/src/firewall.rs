//! 防火墙管理：探测本机防火墙后端（ufw / firewalld），读取状态与规则，
//! 并提供放行 / 拒绝 / 删除规则与启用停用。
//!
//! 边界：只驱动发行版自带的防火墙前端（Debian 系 ufw、Arch 系 firewalld），
//! 不直接读写 iptables/nftables 裸规则 —— 面板不接管防火墙的完整语义，避免与
//! 用户手工维护的规则集打架。目标平台上哪个可用就用哪个，都没有则如实报「未安装」。
//!
//! 所有命令经 [`crate::cmd`] 统一出口执行（超时、输出上限、Firewall 组互斥）。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::process::Command;

use crate::cmd::{self, Budget};

/// 防火墙状态
#[derive(Serialize)]
pub struct Status {
    /// 探测到的后端：`ufw` / `firewalld` / `none`
    pub backend: String,
    /// 是否检测到可用的防火墙命令
    pub installed: bool,
    /// 当前是否处于启用状态
    pub active: bool,
    /// 该后端是否支持「拒绝」规则（firewalld 走富规则，暂不支持）
    pub deny_supported: bool,
    /// 后端原始状态文本（供界面展示诊断信息）
    pub detail: String,
}

/// 一条规则
#[derive(Serialize)]
pub struct Rule {
    /// 动作：ALLOW / DENY / REJECT / LIMIT
    pub action: String,
    /// 协议：tcp / udp / any（服务规则为空）
    pub protocol: String,
    /// 端口或端口范围；服务类规则为空
    pub port: String,
    /// 来源（Anywhere 表示任意地址）
    pub from: String,
    /// 备注：服务名或空
    pub comment: String,
    /// 删除时回传的标识。ufw 为规则编号；firewalld 为 `8080/tcp` 或 `service:http`
    pub id: String,
}

/// 规则动作
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Allow,
    Deny,
}

/// 在常见目录里定位命令。面板以 root 运行，服务的 PATH 往往不含 /usr/sbin，
/// 故不依赖 PATH 直接探测固定位置。
fn which(name: &str) -> Option<String> {
    for dir in [
        "/usr/sbin",
        "/sbin",
        "/usr/local/sbin",
        "/usr/bin",
        "/bin",
        "/usr/local/bin",
    ] {
        let p = format!("{dir}/{name}");
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// ufw 状态文本 → (是否启用, 规则列表)。纯函数，便于单测。
fn parse_ufw_status(text: &str) -> (bool, Vec<Rule>) {
    const ACTIONS: [&str; 4] = ["ALLOW", "DENY", "REJECT", "LIMIT"];
    let active = text.contains("Status: active");
    let mut rules = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        // 规则行形如 `[ 1] 22/tcp  ALLOW IN  Anywhere`
        if !line.starts_with('[') {
            continue;
        }
        let Some(close) = line.find(']') else { continue };
        let id = line[1..close].trim().to_string();
        let rest = line[close + 1..].trim();
        let parts: Vec<&str> = rest.split_whitespace().collect();
        // 「To」可能是多词的服务名（如 `Nginx Full`），故以动作关键字定位分界
        let Some(ai) = parts.iter().position(|p| ACTIONS.contains(p)) else {
            continue;
        };
        let to = parts[..ai].join(" ");
        let action = parts[ai].to_string();
        let after = &parts[ai + 1..];
        // 跳过方向列（IN / OUT / IN/OUT / FWD）
        let skip = after
            .first()
            .is_some_and(|s| matches!(*s, "IN" | "OUT" | "IN/OUT" | "FWD"));
        let from = if skip {
            after.get(1..).unwrap_or_default().join(" ")
        } else {
            after.join(" ")
        };
        let (port, protocol) = match to.split_once('/') {
            Some((p, proto)) => (p.trim().to_string(), proto.trim().to_string()),
            None => (to.clone(), "any".to_string()),
        };
        rules.push(Rule {
            action,
            protocol,
            port,
            from: if from.is_empty() {
                "Anywhere".to_string()
            } else {
                from
            },
            comment: String::new(),
            id,
        });
    }
    (active, rules)
}

/// firewalld 端口列表文本（`80/tcp 443/tcp`）→ 规则列表
fn parse_firewalld_ports(text: &str) -> Vec<Rule> {
    text.split_whitespace()
        .filter_map(|tok| {
            let (port, proto) = tok.split_once('/')?;
            Some(Rule {
                action: "ALLOW".to_string(),
                protocol: proto.to_string(),
                port: port.to_string(),
                from: "Anywhere".to_string(),
                comment: String::new(),
                id: tok.to_string(),
            })
        })
        .collect()
}

/// firewalld 服务列表文本（`ssh http`）→ 规则列表
fn parse_firewalld_services(text: &str) -> Vec<Rule> {
    text.split_whitespace()
        .map(|name| Rule {
            action: "ALLOW".to_string(),
            protocol: String::new(),
            port: String::new(),
            from: "Anywhere".to_string(),
            comment: format!("服务 {name}"),
            id: format!("service:{name}"),
        })
        .collect()
}

/// 读取防火墙状态
pub async fn status() -> Result<Status> {
    if let Some(ufw) = which("ufw") {
        let mut c = Command::new(ufw);
        c.arg("status");
        let out = cmd::run(&mut c, Budget::query(10)).await?;
        let text = text_of(&out.stdout);
        let (active, _) = parse_ufw_status(&text);
        return Ok(Status {
            backend: "ufw".into(),
            installed: true,
            active,
            deny_supported: true,
            detail: summarize(&text),
        });
    }
    if let Some(fw) = which("firewall-cmd") {
        let mut c = Command::new(fw);
        c.arg("--state");
        let out = cmd::run(&mut c, Budget::query(10)).await?;
        let text = text_of(&out.stdout).trim().to_string();
        return Ok(Status {
            backend: "firewalld".into(),
            installed: true,
            active: text == "running",
            deny_supported: false,
            detail: text,
        });
    }
    Ok(Status {
        backend: "none".into(),
        installed: false,
        active: false,
        deny_supported: false,
        detail: String::new(),
    })
}

/// 状态详情只取前若干行，避免把整段状态文本回显给界面
fn summarize(text: &str) -> String {
    text.lines().take(6).collect::<Vec<_>>().join("\n")
}

/// 列出当前规则
pub async fn rules() -> Result<Vec<Rule>> {
    if let Some(ufw) = which("ufw") {
        let mut c = Command::new(ufw);
        c.arg("status").arg("numbered");
        let out = cmd::run(&mut c, Budget::query(10)).await?;
        let (_, rules) = parse_ufw_status(&text_of(&out.stdout));
        return Ok(rules);
    }
    if let Some(fw) = which("firewall-cmd") {
        let mut c = Command::new(&fw);
        c.arg("--list-ports");
        let ports = cmd::run(&mut c, Budget::query(10)).await?;
        let mut rules = parse_firewalld_ports(&text_of(&ports.stdout));
        let mut c2 = Command::new(fw);
        c2.arg("--list-services");
        let svcs = cmd::run(&mut c2, Budget::query(10)).await?;
        rules.extend(parse_firewalld_services(&text_of(&svcs.stdout)));
        return Ok(rules);
    }
    Ok(Vec::new())
}

/// 校验端口：单个端口或 `起始-结束` 范围，1..=65535
fn valid_port(port: &str) -> bool {
    let check = |s: &str| -> bool {
        s.parse::<u16>().map(|n| n > 0).unwrap_or(false)
    };
    match port.split_once('-') {
        Some((a, b)) => check(a) && check(b),
        None => check(port),
    }
}

/// 校验来源地址：IPv4/IPv6 字面量或 CIDR，只允许十六进制与 `. : /`
fn valid_from(from: &str) -> bool {
    !from.is_empty()
        && from.len() <= 64
        && from
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, '.' | ':' | '/'))
}

fn valid_proto(proto: &str) -> bool {
    matches!(proto, "tcp" | "udp")
}

/// 新增放行 / 拒绝规则。`port` 与 `from` 至少给一个。
pub async fn add(op: Op, port: &str, proto: &str, from: &str) -> Result<()> {
    let has_port = !port.trim().is_empty();
    let has_from = !from.trim().is_empty();
    if !has_port && !has_from {
        anyhow::bail!("请至少填写端口或来源地址");
    }
    if has_port && !valid_port(port) {
        anyhow::bail!("端口格式非法（应为 1-65535 或 起始-结束）");
    }
    if has_port && !valid_proto(proto) {
        anyhow::bail!("协议只支持 tcp / udp");
    }
    if has_from && !valid_from(from) {
        anyhow::bail!("来源地址格式非法");
    }

    if let Some(ufw) = which("ufw") {
        let mut c = Command::new(ufw);
        c.arg(match op {
            Op::Allow => "allow",
            Op::Deny => "deny",
        });
        match (has_port, has_from) {
            // 仅来源：ufw allow from 1.2.3.4
            (false, true) => {
                c.arg("from").arg(from);
            }
            // 仅端口：ufw allow 8080/tcp
            (true, false) => {
                c.arg(format!("{port}/{proto}"));
            }
            // 端口 + 来源：ufw allow from 1.2.3.4 to any port 8080 proto tcp
            (true, true) => {
                c.arg("from")
                    .arg(from)
                    .arg("to")
                    .arg("any")
                    .arg("port")
                    .arg(port)
                    .arg("proto")
                    .arg(proto);
            }
            (false, false) => unreachable!("上面已排除"),
        }
        let out = cmd::run(&mut c, Budget::firewall(15)).await?;
        if !out.success() {
            anyhow::bail!("ufw 拒绝了该规则：{}", text_of(&out.stderr).trim());
        }
        return Ok(());
    }

    if let Some(fw) = which("firewall-cmd") {
        if op == Op::Deny {
            anyhow::bail!("firewalld 后端暂不支持「拒绝」规则，请改用富规则手工配置");
        }
        let proto = if has_port { proto } else { "tcp" };
        // firewalld 以「区域」为中心，这里统一操作默认区域 + permanent
        let mut c = Command::new(&fw);
        if has_port {
            c.arg(format!("--add-port={port}/{proto}"));
        } else {
            // 只有来源时用富规则放行该来源
            c.arg(format!(
                "--add-rich-rule=rule family=\"ipv4\" source address=\"{from}\" accept"
            ));
        }
        c.arg("--permanent");
        let out = cmd::run(&mut c, Budget::firewall(15)).await?;
        if !out.success() {
            anyhow::bail!("firewall-cmd 拒绝了该规则：{}", text_of(&out.stderr).trim());
        }
        reload(&fw).await?;
        return Ok(());
    }

    anyhow::bail!("未检测到可用的防火墙（ufw / firewalld）")
}

/// 删除一条规则。`id` 来自 [`Rule::id`]。
pub async fn remove(id: &str) -> Result<()> {
    if let Some(ufw) = which("ufw") {
        let n: u32 = id
            .trim()
            .parse()
            .context("规则编号非法（ufw 按编号删除）")?;
        let mut c = Command::new(ufw);
        c.arg("--force").arg("delete").arg(n.to_string());
        let out = cmd::run(&mut c, Budget::firewall(15)).await?;
        if !out.success() {
            anyhow::bail!("删除失败：{}", text_of(&out.stderr).trim());
        }
        return Ok(());
    }
    if let Some(fw) = which("firewall-cmd") {
        let mut c = Command::new(&fw);
        match id.strip_prefix("service:") {
            Some(name) => {
                c.arg(format!("--remove-service={name}"));
            }
            None => {
                c.arg(format!("--remove-port={id}"));
            }
        }
        c.arg("--permanent");
        let out = cmd::run(&mut c, Budget::firewall(15)).await?;
        if !out.success() {
            anyhow::bail!("删除失败：{}", text_of(&out.stderr).trim());
        }
        reload(&fw).await?;
        return Ok(());
    }
    anyhow::bail!("未检测到可用的防火墙（ufw / firewalld）")
}

/// 启用 / 停用防火墙
pub async fn toggle(enable: bool) -> Result<()> {
    if let Some(ufw) = which("ufw") {
        let mut c = Command::new(ufw);
        if enable {
            c.arg("--force").arg("enable");
        } else {
            c.arg("disable");
        }
        let out = cmd::run(&mut c, Budget::firewall(20)).await?;
        if !out.success() {
            anyhow::bail!("操作失败：{}", text_of(&out.stderr).trim());
        }
        return Ok(());
    }
    if which("firewall-cmd").is_some() {
        // firewalld 由 systemd 托管，启停走 systemctl（Systemd 组串行）
        let mut c = Command::new("systemctl");
        c.arg(if enable { "start" } else { "stop" })
            .arg("firewalld");
        let out = cmd::run(&mut c, Budget::systemd(20)).await?;
        if !out.success() {
            anyhow::bail!("操作失败：{}", text_of(&out.stderr).trim());
        }
        return Ok(());
    }
    anyhow::bail!("未检测到可用的防火墙（ufw / firewalld）")
}

/// firewalld 修改 permanent 规则后需 reload 才生效
async fn reload(fw: &str) -> Result<()> {
    let mut c = Command::new(fw);
    c.arg("--reload");
    let out = cmd::run(&mut c, Budget::firewall(15)).await?;
    if !out.success() {
        anyhow::bail!("reload 失败：{}", text_of(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ufw_reads_active_and_rules() {
        let text = "\
Status: active
Logging: on (low)
Default: deny (incoming), allow (outgoing), disabled (routed)

     To                         Action      From
     --                         ------      ----
[ 1] 22/tcp                     ALLOW IN    Anywhere
[ 2] 443/tcp                    ALLOW IN    Anywhere (v6)
[ 3] 8080/tcp                   DENY IN     203.0.113.5
[ 4] Nginx Full                 ALLOW IN    Anywhere
";
        let (active, rules) = parse_ufw_status(text);
        assert!(active);
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].id, "1");
        assert_eq!(rules[0].port, "22");
        assert_eq!(rules[0].protocol, "tcp");
        assert_eq!(rules[0].action, "ALLOW");
        assert_eq!(rules[0].from, "Anywhere");
        assert_eq!(rules[1].from, "Anywhere (v6)");
        assert_eq!(rules[2].action, "DENY");
        assert_eq!(rules[2].from, "203.0.113.5");
        // 多词服务名不能被切碎
        assert_eq!(rules[3].port, "Nginx Full");
    }

    #[test]
    fn parse_ufw_inactive_has_no_rules() {
        let text = "Status: inactive\n";
        let (active, rules) = parse_ufw_status(text);
        assert!(!active);
        assert!(rules.is_empty());
    }

    #[test]
    fn parse_firewalld_ports_and_services() {
        let ports = parse_firewalld_ports("80/tcp 443/tcp 1000-2000/udp");
        assert_eq!(ports.len(), 3);
        assert_eq!(ports[2].port, "1000-2000");
        assert_eq!(ports[2].protocol, "udp");
        assert_eq!(ports[0].id, "80/tcp");

        let svcs = parse_firewalld_services("ssh http");
        assert_eq!(svcs.len(), 2);
        assert_eq!(svcs[0].id, "service:ssh");
        assert!(svcs[0].port.is_empty());
    }

    #[test]
    fn validates_port_and_source() {
        assert!(valid_port("80"));
        assert!(valid_port("1000-2000"));
        assert!(!valid_port("0"));
        assert!(!valid_port("70000"));
        assert!(!valid_port("80;rm -rf /"));
        assert!(valid_proto("tcp"));
        assert!(!valid_proto("sctp"));

        assert!(valid_from("1.2.3.4"));
        assert!(valid_from("10.0.0.0/8"));
        assert!(valid_from("::1"));
        assert!(!valid_from(""));
        assert!(!valid_from("1.2.3.4; reboot"));
    }
}
