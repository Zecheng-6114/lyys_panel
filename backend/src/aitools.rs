//! AI 助手能调用的工具。
//!
//! 调用循环在 `ai.rs` 里（SSE 流中累积 tool_calls、以 role:"tool" 回灌结果、
//! 限制最大轮数），这里只管「有哪些工具」和「怎么执行」。
//!
//! **一律只读**：工具只查询面板已有的数据（系统状态、进程、服务、日志、
//! Docker、计划任务），不提供任何启停服务、杀进程、改文件的能力 —— 模型
//! 判断错的时候，代价只是查错了一次，而不是把服务器搞坏。

use serde_json::{json, Value};

/// 单次返回给模型的行数上限，避免把上下文塞爆
const MAX_ITEMS: usize = 40;
const MAX_LOG_LINES: u32 = 200;

fn tool(name: &str, desc: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {"type": "object", "properties": properties, "required": required},
        }
    })
}

/// 工具清单（OpenAI 兼容格式）
pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "system_overview",
            "查看这台服务器当前的 CPU 使用率、内存、磁盘、网络速率。问「机器现在怎么样」「负载高不高」「内存还剩多少」这类问题时用它",
            json!({}),
            vec![],
        ),
        tool(
            "list_processes",
            "列出当前占用最高的进程（按 CPU 排序），用于排查「什么在吃 CPU / 内存」",
            json!({"limit": {"type": "integer", "description": "返回条数，默认 10，最多 40"}}),
            vec![],
        ),
        tool(
            "list_services",
            "列出 systemd 服务及其运行状态。不填 filter 时只列正在运行的服务；填 filter 则按名称模糊匹配（含未运行的），用于查询某个服务是否在跑",
            json!({"filter": {"type": "string", "description": "服务名关键字，如 nginx、docker"}}),
            vec![],
        ),
        tool(
            "read_logs",
            "读取日志：给 unit 读该 systemd 服务的 journal 日志，给 file 读某个日志文件的末尾。两者都不给则列出可读的日志文件",
            json!({
                "unit": {"type": "string", "description": "systemd 服务名，如 nginx"},
                "file": {"type": "string", "description": "日志文件的绝对路径"},
                "lines": {"type": "integer", "description": "读取行数，默认 50，最多 200"}
            }),
            vec![],
        ),
        tool(
            "docker",
            "查询 Docker：action=containers 列出容器，action=images 列出镜像，action=compose 列出 Compose 项目，action=status 看 Docker 是否安装与运行；给 container 则返回该容器最近的日志",
            json!({
                "action": {"type": "string", "description": "containers / images / compose / status"},
                "container": {"type": "string", "description": "容器名或 ID，填则返回其日志"}
            }),
            vec![],
        ),
        tool(
            "list_crontab",
            "列出当前用户的计划任务（crontab）",
            json!({}),
            vec![],
        ),
        tool(
            "web_search",
            "联网搜索。本机之外、你不确定或可能已过时的信息（软件版本、报错含义、 \
             某个服务的用法、新闻）都用它查，不要凭记忆答。返回若干条标题与链接",
            json!({
                "query": {"type": "string", "description": "搜索词"},
                "count": {"type": "integer", "description": "返回条数，默认 5，最多 10"}
            }),
            vec!["query"],
        ),
        tool(
            "fetch_url",
            "抓取一个网页的正文并转成纯文本。通常先用 web_search 拿到链接，再用它读具体内容； \
             对方直接给了链接时也用它",
            json!({"url": {"type": "string", "description": "http/https 网址"}}),
            vec!["url"],
        ),
    ]
}

/// 给前端显示的一行摘要（不参与模型上下文）
pub fn summary(name: &str, args: &Value) -> String {
    match name {
        "system_overview" => "查询系统概览".into(),
        "list_processes" => format!(
            "查看进程列表（前 {} 条）",
            args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10)
        ),
        "list_services" => match args.get("filter").and_then(|v| v.as_str()) {
            Some(f) if !f.is_empty() => format!("查询服务：{f}"),
            _ => "查看运行中的服务".into(),
        },
        "read_logs" => {
            if let Some(u) = args.get("unit").and_then(|v| v.as_str()) {
                format!("读取日志：{u}")
            } else if let Some(f) = args.get("file").and_then(|v| v.as_str()) {
                format!("读取日志文件：{f}")
            } else {
                "列出日志文件".into()
            }
        }
        "docker" => match args.get("container").and_then(|v| v.as_str()) {
            Some(c) => format!("查看容器日志：{c}"),
            None => format!(
                "查询 Docker：{}",
                args.get("action").and_then(|v| v.as_str()).unwrap_or("containers")
            ),
        },
        "list_crontab" => "查看计划任务".into(),
        "web_search" => format!(
            "联网搜索：{}",
            args.get("query").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "fetch_url" => format!(
            "抓取网页：{}",
            args.get("url").and_then(|v| v.as_str()).unwrap_or("")
        ),
        other => format!("调用 {other}"),
    }
}

fn gb(bytes: i64) -> String {
    format!("{:.1} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

fn rate(bytes: i64) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb > 1024.0 {
        format!("{:.1} MB/s", kb / 1024.0)
    } else {
        format!("{:.0} KB/s", kb)
    }
}

/// 执行工具，返回给模型看的结果文本。失败也返回文本（把错误讲清楚，
/// 让模型自己决定怎么说），不要抛错打断对话。
pub async fn run(name: &str, args: &Value) -> String {
    match name {
        "system_overview" => system_overview().await,
        "list_processes" => list_processes(args).await,
        "list_services" => list_services(args).await,
        "read_logs" => read_logs(args).await,
        "docker" => docker(args).await,
        "list_crontab" => list_crontab().await,
        // 搜索与抓取是同步的（ureq + 同步正则），直接跑在当前的阻塞线程上
        "web_search" => crate::websearch::search(
            args.get("query").and_then(|v| v.as_str()).unwrap_or(""),
            args.get("count").and_then(|v| v.as_u64()).unwrap_or(5) as usize,
        ),
        "fetch_url" => crate::websearch::fetch(
            args.get("url").and_then(|v| v.as_str()).unwrap_or(""),
        ),
        other => format!("没有名为 {other} 的工具"),
    }
}

async fn system_overview() -> String {
    // 单采一次：Monitor 内部维护采样历史与速率差分，这里只要当下读数
    let mut m = crate::monitor::Monitor::new();
    let s = m.snapshot();
    json!({
        "cpu_percent": (s.cpu * 10.0).round() / 10.0,
        "memory": format!("{} / {}（{:.0}%）", gb(s.mem_used), gb(s.mem_total),
            s.mem_used as f64 / s.mem_total.max(1) as f64 * 100.0),
        "disk": format!("{} / {}（{:.0}%）", gb(s.disk_used), gb(s.disk_total),
            s.disk_used as f64 / s.disk_total.max(1) as f64 * 100.0),
        "network": {"in": rate(s.net_in_per_sec), "out": rate(s.net_out_per_sec)},
    })
    .to_string()
}

async fn list_processes(args: &Value) -> String {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .clamp(1, MAX_ITEMS as u64) as usize;
    let mut m = crate::monitor::Monitor::new();
    let list = m.processes();
    if list.is_empty() {
        return "没有取到进程列表".into();
    }
    let rows: Vec<Value> = list
        .into_iter()
        .take(limit)
        .map(|p| {
            json!({
                "pid": p.pid,
                "name": p.name,
                "cpu": (p.cpu * 10.0).round() / 10.0,
                "mem": gb(p.mem),
                "user": p.user,
                "status": p.status,
            })
        })
        .collect();
    json!({ "count": rows.len(), "processes": rows }).to_string()
}

async fn list_services(args: &Value) -> String {
    let filter = args
        .get("filter")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    match crate::opservice::list().await {
        Ok(list) => {
            let rows: Vec<Value> = list
                .into_iter()
                .filter(|s| {
                    if filter.is_empty() {
                        s.active == "active"
                    } else {
                        s.name.to_lowercase().contains(&filter)
                            || s.description.to_lowercase().contains(&filter)
                    }
                })
                .take(MAX_ITEMS)
                .map(|s| {
                    json!({
                        "name": s.name,
                        "active": s.active,
                        "sub": s.sub,
                        "load": s.load,
                        "description": s.description,
                    })
                })
                .collect();
            if rows.is_empty() {
                return if filter.is_empty() {
                    "没有正在运行的服务".into()
                } else {
                    format!("没有匹配「{filter}」的服务")
                };
            }
            json!({ "count": rows.len(), "services": rows }).to_string()
        }
        Err(e) => format!("查询服务失败：{e}"),
    }
}

async fn read_logs(args: &Value) -> String {
    let lines = args
        .get("lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, MAX_LOG_LINES as u64) as u32;
    let unit = args.get("unit").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let file = args.get("file").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let out = match (unit, file) {
        (Some(u), _) => crate::logs::journal(Some(u), lines).await,
        (None, Some(f)) => crate::logs::tail_file(f, lines).await,
        (None, None) => match crate::logs::list_files().await {
            Ok(files) => {
                return json!({ "log_files": files.into_iter().take(MAX_ITEMS).collect::<Vec<_>>() })
                    .to_string();
            }
            Err(e) => Err(e),
        },
    };
    match out {
        Ok(text) => {
            // 日志可能很长，截断到末 8000 字符，够模型判断又不会撑爆上下文
            let text = if text.chars().count() > 8000 {
                let tail: String = text.chars().skip(text.chars().count() - 8000).collect();
                format!("（已截断，仅保留末尾部分）\n{tail}")
            } else {
                text
            };
            text
        }
        Err(e) => format!("读取日志失败：{e}"),
    }
}

async fn docker(args: &Value) -> String {
    if let Some(c) = args
        .get("container")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return match crate::docker::logs(c, 100).await {
            Ok(text) => text,
            Err(e) => format!("读取容器 {c} 的日志失败：{e}"),
        };
    }
    match args.get("action").and_then(|v| v.as_str()).unwrap_or("containers") {
        "status" => match crate::docker::status().await {
            Ok(s) => json!(s).to_string(),
            Err(e) => format!("查询 Docker 状态失败：{e}"),
        },
        "images" => match crate::docker::images().await {
            Ok(list) => json!({ "count": list.len(), "images": list }).to_string(),
            Err(e) => format!("查询镜像失败：{e}"),
        },
        "compose" => match crate::docker::compose_projects().await {
            Ok(list) => json!({ "count": list.len(), "projects": list }).to_string(),
            Err(e) => format!("查询 Compose 项目失败：{e}"),
        },
        _ => match crate::docker::containers().await {
            Ok(list) => json!({ "count": list.len(), "containers": list }).to_string(),
            Err(e) => format!("查询容器失败：{e}"),
        },
    }
}

async fn list_crontab() -> String {
    match crate::crontab::list().await {
        Ok(list) => json!({ "count": list.len(), "entries": list }).to_string(),
        Err(e) => format!("读取计划任务失败：{e}"),
    }
}
