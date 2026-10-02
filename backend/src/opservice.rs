use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;

/// systemd 服务信息
#[derive(Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

/// 操作类型
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Start,
    Stop,
    Restart,
    Reload,
}

impl Action {
    /// 动词文本，既用于拼 `systemctl <action>`，也用于审计记录
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Reload => "reload",
        }
    }
}

/// 电源/面板级操作（3.x 补充：统一的重启与关机入口）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PowerAction {
    /// 重启面板自身服务（自更新后生效用）
    PanelRestart,
    /// 重启整机
    Reboot,
    /// 关机
    Shutdown,
}

impl PowerAction {
    /// 对应的 systemctl 动词；面板重启返回专用处理标记
    fn systemctl_arg(&self) -> Option<&'static str> {
        match self {
            PowerAction::PanelRestart => None,
            PowerAction::Reboot => Some("reboot"),
            PowerAction::Shutdown => Some("poweroff"),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PowerAction::PanelRestart => "panel-restart",
            PowerAction::Reboot => "reboot",
            PowerAction::Shutdown => "shutdown",
        }
    }
}

/// 面板服务单元名（与部署 systemd 单元一致）
const PANEL_UNIT: &str = "lyys-panel";

/// 执行电源级操作。面板重启走 `systemctl restart lyys-panel`；整机
/// 重启/关机走 `systemctl reboot|poweroff`（root 运行，无需 polkit 认证）。
pub async fn power(act: PowerAction) -> Result<serde_json::Value> {
    // P1-1：电源类写操作走 Systemd 组串行，预算 20s
    let mut cmd = Command::new("systemctl");
    match act.systemctl_arg() {
        Some(verb) => {
            cmd.arg(verb);
        }
        None => {
            cmd.arg("restart").arg(PANEL_UNIT);
        }
    }
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::systemd(20))
        .await
        .context("执行 systemctl 失败")?;
    Ok(json!({
        "ok": out.status.success(),
        "action": act.as_str(),
        "stderr": String::from_utf8_lossy(&out.stderr).into_owned(),
    }))
}

/// 列出所有已加载的 systemd 单元（type=service）
pub async fn list() -> Result<Vec<ServiceInfo>> {
    // P1-1：只读列表查询，预算 15s
    let mut cmd = Command::new("systemctl");
    cmd.args([
        "list-units",
        "--type=service",
        "--all",
        "--no-legend",
        "--plain",
        "--output=json",
    ]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 systemctl 失败，请确认系统使用 systemd")?;
    if !out.status.success() {
        // 某些旧版本不支持 --output=json，回退到文本解析
        return list_from_text().await;
    }
    let units: Vec<ServiceInfo> = serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v: serde_json::Value| {
            let name = v.get("unit")?.as_str()?.to_string();
            Some(ServiceInfo {
                name,
                load: v
                    .get("load")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                active: v
                    .get("active")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                sub: v
                    .get("sub")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: v
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect();
    Ok(units)
}

/// 文本模式回退解析（兼容不支持 json 输出的 systemctl）
async fn list_from_text() -> Result<Vec<ServiceInfo>> {
    let mut cmd = Command::new("systemctl");
    cmd.args([
        "list-units",
        "--type=service",
        "--all",
        "--no-legend",
        "--plain",
    ]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 systemctl 失败")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut list = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(name), Some(load), Some(active), Some(sub)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let desc = it.collect::<Vec<_>>().join(" ");
        list.push(ServiceInfo {
            name: name.to_string(),
            load: load.to_string(),
            active: active.to_string(),
            sub: sub.to_string(),
            description: desc,
        });
    }
    Ok(list)
}

/// 对服务执行操作（start/stop/restart/reload）
pub async fn action(name: &str, act: Action) -> Result<serde_json::Value> {
    // 基础校验：服务名只允许安全字符，避免命令注入
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '@' | '-' | '_'))
    {
        anyhow::bail!("非法服务名");
    }
    // P1-1：服务写操作走 Systemd 组串行，预算 20s
    let mut cmd = Command::new("systemctl");
    cmd.arg(act.as_str()).arg(name);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::systemd(20))
        .await
        .context("执行 systemctl 失败")?;
    Ok(json!({
        "ok": out.status.success(),
        "stderr": String::from_utf8_lossy(&out.stderr).into_owned(),
    }))
}
