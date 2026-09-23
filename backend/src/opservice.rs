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

/// 列出所有已加载的 systemd 单元（type=service）
pub async fn list() -> Result<Vec<ServiceInfo>> {
    let out = Command::new("systemctl")
        .args(["list-units", "--type=service", "--all", "--no-legend", "--plain", "--output=json"])
        .output()
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
                load: v.get("load").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                active: v.get("active").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                sub: v.get("sub").and_then(|x| x.as_str()).unwrap_or("").to_string(),
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
    let out = Command::new("systemctl")
        .args(["list-units", "--type=service", "--all", "--no-legend", "--plain"])
        .output()
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
    let out = Command::new("systemctl")
        .arg(act.as_str())
        .arg(name)
        .output()
        .await
        .context("执行 systemctl 失败")?;
    Ok(json!({
        "ok": out.status.success(),
        "stderr": String::from_utf8_lossy(&out.stderr).into_owned(),
    }))
}
