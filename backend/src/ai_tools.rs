//! AI 工具调用：把面板自身的**只读**运维查询暴露为 OpenAI function calling 工具。
//!
//! 设计边界（用户确认的三条决策）：
//! - 只暴露 GET 类只读端点；写操作（杀进程/重启/文件写等）从定义层面杜绝，
//!   不存在"配置里不小心开了危险工具"的可能；
//! - 权限以**触发者角色**为上限：执行前按 role_level 校验一次。当前全部
//!   工具 min_role=viewer（与面板只读端点的既有权限一致），未来加写工具
//!   时闸门直接生效；
//! - 悬浮球助手（ai.rs）拿到 tool_calls 后调用本模块执行，工具消息仅存在于
//!   [`execute`]，结果作为 role=tool 消息回灌上游，不持久化。
//!
//! 执行器直接复用各模块的底层函数（与 api.rs 的 handler 同源），
//! 不走 HTTP 回环：省一次序列化/鉴权往返，也不依赖监听地址。

use serde_json::json;

use crate::AppState;

/// 工具结果注入上下文前的截断上限（UTF-8 字节）。
/// 防御性限制：进程列表/包列表可达数百 KB，不截断会撑爆上游上下文。
pub(super) const MAX_RESULT_BYTES: usize = 8 * 1024;

/// 角色等级：与 api.rs 的 role_level 同口径（viewer=0 < operator=1 < admin=2）。
fn role_level(role: &str) -> u8 {
    match role {
        "admin" => 2,
        "operator" => 1,
        _ => 0,
    }
}

/// 一个工具的注册信息：OpenAI function schema + 所需最低角色。
pub(super) struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema（参数定义），原样放进 function.parameters
    pub parameters: serde_json::Value,
    pub min_role: u8,
}

/// 工具注册表（唯一事实源）：schema 给上游看，execute 按 name 分派。
fn registry() -> Vec<ToolDef> {
    let no_args = json!({"type": "object", "properties": {}});
    vec![
        ToolDef {
            name: "get_system_state",
            description: "查询服务器当前实时状态：CPU 使用率、内存用量、负载、磁盘、网络速率等快照",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "get_system_history",
            description: "查询最近的历史监控采样点（CPU/内存/网络时间序列）",
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "返回采样点数量，默认 24，最大 2000"}
                }
            }),
            min_role: 0,
        },
        ToolDef {
            name: "list_processes",
            description: "列出当前运行进程（PID、名称、CPU/内存占用）",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "list_services",
            description: "列出 systemd 服务单元及其状态",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "list_packages",
            description: "列出已安装软件包",
            parameters: json!({
                "type": "object",
                "properties": {
                    "filter": {"type": "string", "description": "按包名子串过滤"},
                    "limit": {"type": "integer", "description": "最多返回条数，默认 200，最大 2000"}
                }
            }),
            min_role: 0,
        },
        ToolDef {
            name: "list_upgradable_packages",
            description: "列出有可用更新的软件包",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "get_docker_status",
            description: "查询 Docker 是否安装、守护进程是否运行",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "list_docker_containers",
            description: "列出 Docker 容器（含运行状态）",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "list_docker_images",
            description: "列出本地 Docker 镜像",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "get_network_interfaces",
            description: "查询网络接口及其地址（ip addr 的 JSON 输出）",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "get_dns_config",
            description: "查询当前 DNS 服务器配置（/etc/resolv.conf）",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "get_smart_health",
            description: "查询磁盘 SMART 健康报告（型号、健康状态、通电时长等）",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "read_journal_logs",
            description: "读取 systemd journal 最近日志",
            parameters: json!({
                "type": "object",
                "properties": {
                    "unit": {"type": "string", "description": "服务单元名，如 ssh；省略则读全部"},
                    "lines": {"type": "integer", "description": "回看行数，默认 50，最大 200"}
                }
            }),
            min_role: 0,
        },
    ]
}

/// 生成上游请求体里的 `tools` 数组（OpenAI function calling 格式）。
pub(super) fn tools_json() -> serde_json::Value {
    json!(registry()
        .into_iter()
        .map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            }
        }))
        .collect::<Vec<_>>())
}

/// 按触发者角色过滤出可用工具名集合（执行时的角色闸门与之一致）。
fn allowed(name: &str, role: &str) -> bool {
    registry()
        .into_iter()
        .find(|t| t.name == name)
        .map(|t| role_level(role) >= t.min_role)
        .unwrap_or(false)
}

/// UTF-8 安全截断：在不超过 max 字节的前提下按字符边界切断，
/// 截断发生时追加省略标记。
fn truncate_utf8(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…（已截断）", &s[..end])
}

/// 执行一次工具调用，返回给模型的文本结果（永远不抛错：
/// 未知工具/权限不足/执行失败都以文本形式回灌，让模型自行调整）。
pub(super) async fn execute(
    state: &AppState,
    name: &str,
    args: &serde_json::Value,
    role: &str,
) -> String {
    if !registry().iter().any(|t| t.name == name) {
        return format!("错误：未知工具 {name}");
    }
    if !allowed(name, role) {
        return "错误：当前用户权限不足，无法使用该工具".to_string();
    }
    let result: anyhow::Result<String> = match name {
        "get_system_state" => {
            let mut m = state.monitor.lock().await;
            // latest() 而非 snapshot()：后者会重置速率差值基准，
            // AI 每问一次系统状态就把网络/磁盘 I/O 的采样窗口切碎一次
            serde_json::to_string(&m.latest()).map_err(|e| e.into())
        }
        "get_system_history" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(24).clamp(1, 2000);
            state
                .db
                .recent_metrics_async(limit)
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(Into::into))
        }
        // 助手做系统诊断时要看完整进程视图，不限定到某个实例
        "list_processes" => crate::rprocess::list(state, None)
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_services" => crate::opservice::list()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_packages" => {
            let filter = args.get("filter").and_then(|v| v.as_str());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(200).clamp(1, 2000) as usize;
            crate::packages::list_installed(filter, limit)
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(Into::into))
        }
        "list_upgradable_packages" => crate::packages::upgradable()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "get_docker_status" => crate::docker::status()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_docker_containers" => crate::docker::containers()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_docker_images" => crate::docker::images()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "get_network_interfaces" => crate::network::interfaces()
            .await
            .map(|v| v.to_string()),
        "get_dns_config" => crate::network::dns()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "get_smart_health" => Ok(serde_json::to_string(&crate::ops::smart_report().await).unwrap_or_default()),
        "read_journal_logs" => {
            let unit = args.get("unit").and_then(|v| v.as_str());
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(50).min(200) as u32;
            crate::logs::journal(unit, lines).await
        }
        // registry() 已保证不会走到这里；保留兜底
        _ => Err(anyhow::anyhow!("未知工具 {name}")),
    };
    match result {
        Ok(text) => truncate_utf8(text, MAX_RESULT_BYTES),
        Err(e) => {
            // 失败细节进日志，回灌给模型的文本保持通用（不泄漏路径/errno）
            tracing::warn!("AI 工具 {name} 执行失败：{e:#}");
            format!("工具 {name} 执行失败：请检查参数或稍后重试")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_gate() {
        // 当前全部工具 viewer 可用；未知工具一律拒绝
        assert!(allowed("get_system_state", "viewer"));
        assert!(allowed("get_system_state", "admin"));
        assert!(!allowed("no_such_tool", "admin"));
        assert_eq!(role_level("viewer"), 0);
        assert_eq!(role_level("operator"), 1);
        assert_eq!(role_level("admin"), 2);
        assert_eq!(role_level("bogus"), 0);
    }

    #[test]
    fn truncation_is_char_safe() {
        let s = "中文字符测试".repeat(1000);
        let t = truncate_utf8(s.clone(), 32);
        assert!(t.len() <= 32 + "…（已截断）".len());
        assert!(t.ends_with("已截断）"));
        // 未超限的原样返回
        assert_eq!(truncate_utf8("abc".into(), 8), "abc");
        // 截断点落在多字节字符中间时按字符边界回退
        let t2 = truncate_utf8("日本語".into(), 4);
        assert!(t2.starts_with("日"));
    }

    #[test]
    fn tools_json_shape() {
        let v = tools_json();
        let arr = v.as_array().expect("tools 应为数组");
        assert_eq!(arr.len(), registry().len());
        for item in arr {
            assert_eq!(item["type"], "function");
            assert!(item["function"]["name"].as_str().is_some());
            assert_eq!(item["function"]["parameters"]["type"], "object");
        }
        // 名字唯一（上游对重名工具行为未定义）
        let mut names: Vec<&str> = arr.iter().map(|i| i["function"]["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
    }
}
