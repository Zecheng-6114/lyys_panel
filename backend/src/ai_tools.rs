//! AI 工具调用：把面板自身的**只读**运维查询与联网检索暴露为
//! OpenAI function calling 工具。
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
///
/// 描述里写清「什么时候该用」而不是只写「这是什么」——模型选错工具多半是
/// 描述只说明了能力、没说明场景。联网类工具额外标注了它查到的是外部资料。
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
            description: "查询最近的历史监控采样点（CPU/内存/网络时间序列），用于判断\"是一直这样还是刚刚开始\"",
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
            description: "列出当前运行进程（PID、名称、CPU/内存占用），用于定位高占用进程",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "list_services",
            description: "列出 systemd 服务单元及其运行状态",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "read_service_unit",
            description: "读取某个 systemd 服务的 unit 文件全文（含 drop-in 覆盖片段）。\
                          排查服务启动参数、端口、依赖关系时用它",
            parameters: json!({
                "type": "object",
                "properties": {
                    "unit": {"type": "string", "description": "服务单元名，如 nginx 或 nginx.service"}
                },
                "required": ["unit"]
            }),
            min_role: 0,
        },
        ToolDef {
            name: "list_packages",
            description: "列出**已安装**的软件包",
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
            description: "列出有可用更新的软件包（已安装版本 → 可升级版本）",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "search_packages",
            description: "在软件源中检索**可安装**的软件包。用户问\"有没有某个软件/该装哪个包\"时用它，\
                          注意与 list_packages（只看已装）区分",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "搜索关键词，如 nginx"},
                    "limit": {"type": "integer", "description": "最多返回条数，默认 30，最大 200"}
                },
                "required": ["query"]
            }),
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
            name: "list_compose_projects",
            description: "列出 Docker Compose 项目及其服务，用于理解多容器应用的组成",
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
            name: "get_network_routes",
            description: "查询路由表与默认网关，排查\"连不通某个网段\"时用它",
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
            description: "读取 systemd journal 最近日志，排查服务崩溃/启动失败",
            parameters: json!({
                "type": "object",
                "properties": {
                    "unit": {"type": "string", "description": "服务单元名，如 ssh；省略则读全部"},
                    "lines": {"type": "integer", "description": "回看行数，默认 50，最大 200"}
                }
            }),
            min_role: 0,
        },
        ToolDef {
            name: "list_log_files",
            description: "列出面板可读取的日志文件路径，与 read_log_file 配合使用",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "read_log_file",
            description: "读取面板可读日志文件的末尾若干行（只读白名单目录内的文件）",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "日志文件路径，取自 list_log_files 的结果"},
                    "lines": {"type": "integer", "description": "回看行数，默认 100，最大 500"}
                },
                "required": ["path"]
            }),
            min_role: 0,
        },
        ToolDef {
            name: "list_cron_jobs",
            description: "列出计划任务（crontab 条目），用于排查定时任务、重复执行、备份失败等问题",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "list_backups",
            description: "列出面板的备份文件（名称、大小、时间）",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "list_instances",
            description: "列出面板聚合的实例（Docker 容器与 systemd 服务），含各自 CPU/内存占用。\
                          想概览\"这台机器上跑着什么\"时优先用它",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "get_alert_config",
            description: "查询告警规则配置与最近的告警事件，用于回答\"为什么没告警/告警过几次\"",
            parameters: no_args.clone(),
            min_role: 0,
        },
        ToolDef {
            name: "web_search",
            description: "联网搜索公开网页，用于查软件文档、报错含义、版本变更等面板之外的信息。\
                          回答引用这些内容时必须给出链接。搜索词用自然语言或关键词均可",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "搜索词"},
                    "count": {"type": "integer", "description": "返回条数，默认 6，最大 20"}
                },
                "required": ["query"]
            }),
            min_role: 0,
        },
        ToolDef {
            name: "fetch_web_page",
            description: "抓取指定网页并返回纯文本正文（已去掉脚本与样式）。\
                          想读某个文档页、changelog、issue 的具体内容时用它，比只靠搜索摘要准确",
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "网页地址，必须是 http/https"},
                    "max_chars": {"type": "integer", "description": "最多返回的字符数，默认 8000，最大 40000"}
                },
                "required": ["url"]
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

/// 一次工具执行的结果。`text` 回灌给模型，`ok` 供前端标记成功/失败——
/// 两者必须分开：模型需要知道失败原因，而界面只需要一个状态位。
pub(super) struct ToolOutcome {
    pub text: String,
    pub ok: bool,
}

impl ToolOutcome {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: true,
        }
    }
    fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ok: false,
        }
    }
}

/// 把检索结果排成模型易读的编号列表（纯函数，便于单测）
fn format_hits(provider: &str, hits: &[crate::websearch::SearchHit]) -> String {
    let mut s = format!("搜索结果（通道 {provider}，共 {} 条）：\n", hits.len());
    for (i, h) in hits.iter().enumerate() {
        s.push_str(&format!("\n{}. {}\n   {}\n", i + 1, h.title, h.url));
        if !h.snippet.is_empty() {
            s.push_str(&format!("   {}\n", h.snippet));
        }
    }
    s.push_str("\n需要正文时用 fetch_web_page 打开具体链接。");
    s
}

/// 执行一次工具调用，返回给模型的文本结果（永远不抛错：
/// 未知工具/权限不足/执行失败都以文本形式回灌，让模型自行调整）。
pub(super) async fn execute(
    state: &AppState,
    name: &str,
    args: &serde_json::Value,
    role: &str,
) -> ToolOutcome {
    if !registry().iter().any(|t| t.name == name) {
        return ToolOutcome::err(format!("错误：未知工具 {name}"));
    }
    if !allowed(name, role) {
        return ToolOutcome::err("错误：当前用户权限不足，无法使用该工具");
    }
    // 包一层 async 块：这样各分支里可以用 `?` 取必填参数并直接短路，
    // 不用为每个参数写一次 match。块自身的输出类型就是 Result。
    let result: anyhow::Result<String> = async {
        match name {
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
        "read_service_unit" => {
            let unit = args
                .get("unit")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("缺少 unit 参数"))?;
            crate::ops::unit_file(unit).await
        }
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
        "search_packages" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("缺少 query 参数"))?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(30).clamp(1, 200) as usize;
            crate::packages::search(query, limit)
                .await
                .and_then(|v| serde_json::to_string(&v).map_err(Into::into))
        }
        "get_docker_status" => crate::docker::status()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_docker_containers" => crate::docker::containers()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_docker_images" => crate::docker::images()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_compose_projects" => crate::docker::compose_projects()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "get_network_interfaces" => crate::network::interfaces()
            .await
            .map(|v| v.to_string()),
        "get_network_routes" => crate::network::routes().await.map(|v| v.to_string()),
        "get_dns_config" => crate::network::dns()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "get_smart_health" => Ok(serde_json::to_string(&crate::ops::smart_report().await).unwrap_or_default()),
        "read_journal_logs" => {
            let unit = args.get("unit").and_then(|v| v.as_str());
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(50).min(200) as u32;
            crate::logs::journal(unit, lines).await
        }
        "list_log_files" => crate::logs::list_files()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "read_log_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("缺少 path 参数"))?;
            let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(100).min(500) as u32;
            crate::logs::tail_file(path, lines).await
        }
        "list_cron_jobs" => crate::crontab::list()
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_backups" => crate::backup::list_backups(&state.data_dir)
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "list_instances" => crate::instances::list(state)
            .await
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        "get_alert_config" => {
            let rules = crate::alerts::load_rules(&state.db);
            let events = state.db.alert_event_list_async(20, 0).await?;
            serde_json::to_string(&serde_json::json!({
                "rules": rules,
                "recent_events": events,
            }))
            .map_err(Into::into)
        }
        "web_search" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("缺少 query 参数"))?;
            let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(6).clamp(1, 20) as usize;
            let base = crate::ai::search_base(&state.db).await;
            crate::websearch::search(query, count, base.as_deref())
                .await
                .map(|(provider, hits)| format_hits(&provider, &hits))
        }
        "fetch_web_page" => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("缺少 url 参数"))?;
            let max = args
                .get("max_chars")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            crate::websearch::fetch_text(url, max)
                .await
                .map(|(final_url, text)| format!("页面：{final_url}\n\n{text}"))
        }
        // registry() 已保证不会走到这里；保留兜底
        _ => Err(anyhow::anyhow!("未知工具 {name}")),
        }
    }
    .await;
    match result {
        Ok(text) => ToolOutcome::ok(truncate_utf8(text, MAX_RESULT_BYTES)),
        Err(e) => {
            // 失败细节进日志，回灌给模型的文本保持通用（不泄漏路径/errno）
            tracing::warn!("AI 工具 {name} 执行失败：{e:#}");
            ToolOutcome::err(format!("工具 {name} 执行失败：请检查参数或稍后重试"))
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
        assert!(allowed("web_search", "viewer"));
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

    #[test]
    fn every_registered_tool_has_a_dispatch_arm() {
        // 注册表与 execute 的 match 分派必须一一对应：漏一个就会在运行时
        // 落到兜底分支、以"未知工具"回灌，而模型完全看不出是自己人的疏漏。
        // 这里用源码文本核对——比逐个真的调用一次轻得多。
        let src = include_str!("ai_tools.rs");
        let dispatch = src
            .split("let result: anyhow::Result<String> = async {")
            .nth(1)
            .and_then(|s| s.split("// registry() 已保证不会走到这里").next())
            .expect("未找到 execute 分派体");
        for t in registry() {
            assert!(
                dispatch.contains(&format!("\"{}\" =>", t.name)),
                "工具 {} 没有对应的 execute 分支",
                t.name
            );
        }
    }

    #[test]
    fn hits_are_formatted_with_links() {
        let hits = vec![
            crate::websearch::SearchHit {
                title: "Nginx 文档".into(),
                url: "https://nginx.org/en/docs/".into(),
                snippet: "官方文档".into(),
            },
            crate::websearch::SearchHit {
                title: "无摘要".into(),
                url: "https://example.com/".into(),
                snippet: String::new(),
            },
        ];
        let s = format_hits("bing", &hits);
        assert!(s.contains("通道 bing"));
        assert!(s.contains("1. Nginx 文档"));
        assert!(s.contains("https://nginx.org/en/docs/"));
        assert!(s.contains("2. 无摘要"));
        // 摘要在为空时不产生空行残留
        assert!(!s.contains("\n   \n"));
    }
}
