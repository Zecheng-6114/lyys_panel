//! AI 助手：把 OpenAI 兼容的 chat/completions 接口以流式方式转发给前端。
//!
//! 设计边界（4.4 AI 重写范围）：
//! - 默认无状态转发：不存会话、不做记忆，历史由前端随请求携带；
//!   请求带 `use_tools` 时启用服务端工具循环（面板只读运维工具，见 ai_tools），
//!   工具消息仅在循环内临时存在，不进前端历史、不落库；
//! - 上游地址 / 密钥 / 模型来自设置页配置（settings 表）或环境变量，
//!   密钥不进日志、不向前端明文回显；
//! - 流式透传上游 SSE 字节流，前端按 SSE 解析，后端不做内容改写。
//!
//! 配置来源与优先级（高 → 低）：
//! 1. 设置页配置（settings 表 `ai_api_config`，经 /api/ai/config 保存）；
//! 2. 环境变量（panel.env / systemd，改后重启面板生效）；
//! 3. 内置默认值（base=`https://api.openai.com/v1`，model=`gpt-4o-mini`）。
//!    密钥没有默认值：设置页与环境变量都未配置时，对话请求返回 400。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::api::AuthUser;
use crate::db::Db;
use crate::AppState;

/// 请求体上限：单条消息内容最大字节数（UTF-8）。防御性限制，正常问答远用不到。
const MAX_CONTENT_BYTES: usize = 32 * 1024;
/// 请求体上限：一次请求最多携带的消息条数（含 system/assistant 历史）。
const MAX_MESSAGES: usize = 64;

/// settings 表中 AI 配置的键名（值为 JSON，见 [`StoredAiConfig`]）。
/// api.rs 的 /api/ai/config 读写同一键，两处必须保持一致。
pub(super) const AI_CONFIG_KEY: &str = "ai_api_config";

/// 设置页持久化的 AI 配置（settings 表，JSON 序列化存储）。
/// 字段均可缺省/为空：空值表示该项回退到环境变量或内置默认。
#[derive(Default, Deserialize, Serialize)]
pub(super) struct StoredAiConfig {
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub model: String,
}

/// 从 settings 表原始值解析配置；缺失/损坏一律按未配置处理（回退环境变量），
/// 不让一条坏数据把整个 AI 功能打挂。
pub(super) fn parse_stored(raw: Option<String>) -> StoredAiConfig {
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 本模块专用错误：与 api::ApiError 同构（JSON `{"error": ...}`），
/// 因 ApiError 的构造器不对外公开，这里单独实现一份。
#[derive(Debug)]
pub(super) struct AiError {
    status: StatusCode,
    message: String,
}

impl AiError {
    pub(super) fn bad(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    pub(super) fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
    pub(super) fn upstream(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: msg.into(),
        }
    }
    /// 群聊模块把 AiError 转 GroupError 时复用同一状态码
    pub(super) fn status_code(&self) -> StatusCode {
        self.status
    }
}

/// 面向调用方的错误文案（Display 与 IntoResponse 的 message 一致），
/// 群聊调度把失败原因写进房间系统消息时用。
impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl IntoResponse for AiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

/// 前端请求体：完整对话历史由前端携带（服务端不持久化）。
/// `use_tools`：为 true 时启用面板只读工具（function calling），服务端跑
/// 工具循环；为 false（默认）保持纯透传，行为与既有单聊完全一致。
#[derive(Deserialize)]
pub(super) struct ChatReq {
    messages: Vec<ChatMsg>,
    #[serde(default)]
    use_tools: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct ChatMsg {
    pub(super) role: String,
    /// role=tool 时可为空（结果通过 content 文本回灌，但 OpenAI 允许空串）；
    /// 其余角色由 validate 保证非空。
    pub(super) content: String,
    /// assistant 消息携带的工具调用请求（仅服务端工具循环内部使用，
    /// 前端历史不含此字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<serde_json::Value>>,
    /// role=tool 消息回填对应的 tool_call_id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
}

impl ChatMsg {
    /// 普通文本消息（system/user/assistant），不带工具字段。
    pub(super) fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

/// 共享的出站 HTTP 客户端。流式响应不能设整体超时（长回答会被掐断），
/// 只设连接超时，读超时依赖上游服务器与客户端断开。
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client 构建失败")
    })
}

/// 读取环境变量的辅助：空串视为未配置。
/// api.rs 的 ai_config_get 回显生效值时也要读环境变量，故设 pub(super)。
pub(super) fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// 配置合并（settings 优先、环境变量兜底），独立成纯函数便于单测
/// （进程级环境变量是全局的，DB 依赖没法进单元测试）。
/// 返回 (base, key, model)；key 两级都取不到时报 BAD_REQUEST。
fn resolve(stored: &StoredAiConfig) -> Result<(String, String, String), AiError> {
    // 密钥：设置页 > 环境变量，均无则未配置
    let key = if stored.key.trim().is_empty() {
        env_var("AI_API_KEY").ok_or_else(|| {
            AiError::bad("AI 功能未配置：请在设置页或环境变量中配置 API 密钥")
        })?
    } else {
        stored.key.trim().to_string()
    };
    // 上游地址：设置页 > 环境变量 > 默认值，统一去掉尾部斜杠
    let base = if stored.base.trim().is_empty() {
        env_var("AI_API_BASE").unwrap_or_else(|| "https://api.openai.com/v1".into())
    } else {
        stored.base.trim().to_string()
    }
    .trim_end_matches('/')
    .to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(AiError::bad("上游 API 地址无效：必须以 http(s):// 开头"));
    }
    // 模型名：设置页 > 环境变量 > 默认值
    let model = if stored.model.trim().is_empty() {
        env_var("AI_MODEL").unwrap_or_else(|| "gpt-4o-mini".into())
    } else {
        stored.model.trim().to_string()
    };
    Ok((base, key, model))
}

/// 汇总设置页配置与环境变量，得到生效的三元组。每次请求都现读：
/// 设置页保存后下一个请求立即生效，无需重启面板。
async fn ai_config(db: &Db) -> Result<(String, String, String), AiError> {
    let raw = db
        .get_setting_async(AI_CONFIG_KEY)
        .await
        .map_err(|e| AiError::internal(format!("读取 AI 配置失败：{e}")))?;
    resolve(&parse_stored(raw))
}

/// 群聊模块复用（4.5）：全局配置 + 成员级覆盖（model / api_base，空串=不覆盖；
/// 密钥不提供成员级覆盖，一律走全局配置，遵循「密钥不落库」约束）。
pub(super) async fn ai_config_with_overrides(
    db: &Db,
    model_override: &str,
    base_override: &str,
) -> Result<(String, String, String), AiError> {
    let (base, key, model) = ai_config(db).await?;
    let base = if base_override.trim().is_empty() {
        base
    } else {
        let b = base_override.trim().trim_end_matches('/').to_string();
        if !b.starts_with("http://") && !b.starts_with("https://") {
            return Err(AiError::bad("成员级上游地址无效：必须以 http(s):// 开头"));
        }
        b
    };
    let model = if model_override.trim().is_empty() {
        model
    } else {
        model_override.trim().to_string()
    };
    Ok((base, key, model))
}

/// 群聊模块复用：向流式上游发起一次 chat/completions 请求。
/// 错误语义与 ai_chat 一致（连接失败/非 2xx 转 AiError，细节只进日志）。
/// `think: true`：Ollama 的思考型模型（Qwen3 等）默认可能不输出思维链，
/// 显式开启后 delta 里才会带 reasoning 字段；不支持该参数的上游会忽略它。
/// `tools`：Some 时附带 function calling 工具定义。
/// 返回 [`StreamErr`]：`client_reject=true` 表示上游以 4xx 拒绝请求
/// （最常见：不支持 tools 参数），工具循环据此做降级重试。
pub(super) struct StreamErr {
    pub err: AiError,
    pub client_reject: bool,
}

pub(super) async fn stream_completion(
    base: &str,
    key: &str,
    model: &str,
    messages: &[ChatMsg],
    tools: Option<&serde_json::Value>,
) -> Result<reqwest::Response, StreamErr> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "think": true,
    });
    if let Some(t) = tools {
        body["tools"] = t.clone();
    }
    let resp = http_client()
        .post(format!("{base}/chat/completions"))
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("AI 上游连接失败：{e}（base={base}）");
            StreamErr {
                err: AiError::upstream("无法连接 AI 服务，请检查上游 API 地址与网络配置"),
                client_reject: false,
            }
        })?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("AI 上游返回 {status}：{text}");
        let brief: String = text.chars().take(300).collect();
        return Err(StreamErr {
            err: AiError::upstream(format!(
                "AI 服务返回错误（HTTP {}）：{brief}",
                status.as_u16()
            )),
            client_reject: status.is_client_error(),
        });
    }
    Ok(resp)
}

/// 工具循环引擎（单聊 use_tools 与群聊共用）：
/// - [`TurnEvent`]：引擎向调用方发出的事件流（正文/思维链增量、工具状态、错误）；
///   调用方各自翻译成 SSE chunk（单聊）或 WS 事件（群聊）。
/// - [`run_turn`]：完整一轮"上游回答 + 工具往返"。流里聚合到 tool_calls 就执行
///   面板只读工具、以 role=tool 消息回灌并重新请求，直到产出正文或达到
///   [`MAX_TOOL_ROUNDS`]；上游以 4xx 拒绝 tools 时自动去掉 tools 重试一次
///   （兼容不支持 function calling 的上游）。
/// - 工具消息只存在于本次引擎调用的临时上下文，不持久化、不回传前端历史。
const MAX_TOOL_ROUNDS: usize = 4;

pub(super) enum TurnEvent {
    /// 增量：kind = "content" | "reasoning"
    Delta { kind: &'static str, text: String },
    /// 工具状态：state = "start" | "done"
    Tool { name: String, state: &'static str },
}

pub(super) struct TurnResult {
    pub content: String,
    pub reasoning: String,
}

/// 流式 tool_calls 分片聚合：OpenAI 协议里 id/name/arguments 都可能拆成多帧，
/// 按 index 累积。测试用纯函数。
#[derive(Default)]
pub(super) struct ToolCallBuf {
    pub id: String,
    pub name: String,
    pub args: String,
}

/// 把一条 chunk JSON 里的 delta.tool_calls 分片折叠进聚合表。纯函数便于单测。
pub(super) fn fold_tool_calls(
    map: &mut BTreeMap<usize, ToolCallBuf>,
    delta: &serde_json::Value,
) {
    let Some(arr) = delta.get("tool_calls").and_then(|v| v.as_array()) else {
        return;
    };
    for frag in arr {
        let idx = frag.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let e = map.entry(idx).or_default();
        if let Some(id) = frag.get("id").and_then(|v| v.as_str()) {
            e.id = id.to_string();
        }
        if let Some(n) = frag
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
        {
            e.name.push_str(n);
        }
        if let Some(a) = frag
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(|a| a.as_str())
        {
            e.args.push_str(a);
        }
    }
}

/// 一次上游调用的生效配置三元组（run_turn 参数收纳用）。
pub(super) struct Upstream<'a> {
    pub base: &'a str,
    pub key: &'a str,
    pub model: &'a str,
}

/// 执行一轮（含工具往返）。`stop` 为 Some 时每块检查（群聊 admin 强停）；
/// `tx` 接收端被 drop（前端断开）时引擎退出。
/// 返回 Err 仅用于"本轮彻底失败"；工具执行失败不报错（以文本回灌模型）。
pub(super) async fn run_turn(
    state: &AppState,
    role: &str,
    up: Upstream<'_>,
    mut messages: Vec<ChatMsg>,
    mut tools: Option<serde_json::Value>,
    stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    tx: &mpsc::Sender<TurnEvent>,
) -> Result<TurnResult, AiError> {
    use std::sync::atomic::Ordering;
    let stopped = || stop.as_ref().map(|s| s.load(Ordering::Relaxed)).unwrap_or(false);
    let mut acc_reasoning = String::new();

    for _round in 0..MAX_TOOL_ROUNDS {
        if stopped() {
            return Err(AiError::upstream("已被管理员停止"));
        }
        // 发起请求；上游带 tools 时 4xx 拒绝 → 去掉 tools 立即重试一次
        let resp = loop {
            match stream_completion(up.base, up.key, up.model, &messages, tools.as_ref()).await {
                Ok(r) => break r,
                Err(se) if se.client_reject && tools.is_some() => {
                    tracing::warn!("上游拒绝 tools 参数，降级为无工具请求");
                    tools = None;
                }
                Err(se) => return Err(se.err),
            }
        };

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls: BTreeMap<usize, ToolCallBuf> = BTreeMap::new();
        while let Some(item) = stream.next().await {
            if stopped() {
                return Err(AiError::upstream("已被管理员停止"));
            }
            match item {
                Ok(bytes) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(pos) = buf.find('\n') {
                        let line = buf[..pos].trim_end_matches('\r').to_string();
                        buf.drain(..=pos);
                        let Some(data) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let data = data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) else {
                            continue;
                        };
                        if let Some(msg) = chunk
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                        {
                            return Err(AiError::upstream(msg.to_string()));
                        }
                        let Some(delta) = chunk
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("delta"))
                        else {
                            continue;
                        };
                        if let Some(t) = delta
                            .get("reasoning_content")
                            .or_else(|| delta.get("reasoning"))
                            .and_then(|c| c.as_str())
                        {
                            if !t.is_empty() {
                                reasoning.push_str(t);
                                let _ = tx
                                    .send(TurnEvent::Delta {
                                        kind: "reasoning",
                                        text: t.to_string(),
                                    })
                                    .await;
                            }
                        }
                        if let Some(d) = delta.get("content").and_then(|c| c.as_str()) {
                            if !d.is_empty() {
                                content.push_str(d);
                                // 接收端被丢弃 = 前端断开，中止本轮
                                if tx
                                    .send(TurnEvent::Delta {
                                        kind: "content",
                                        text: d.to_string(),
                                    })
                                    .await
                                    .is_err()
                                {
                                    return Err(AiError::upstream("客户端已断开"));
                                }
                            }
                        }
                        fold_tool_calls(&mut calls, delta);
                    }
                }
                Err(e) => return Err(AiError::upstream(format!("读取上游流失败：{e}"))),
            }
        }
        acc_reasoning.push_str(&reasoning);

        if calls.is_empty() {
            return Ok(TurnResult {
                content,
                reasoning: acc_reasoning,
            });
        }

        // 有工具调用：assistant 消息（含 tool_calls）+ 逐个执行回灌
        let tc: Vec<serde_json::Value> = calls
            .values()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "type": "function",
                    "function": {
                        "name": c.name,
                        "arguments": if c.args.is_empty() { "{}" } else { &c.args },
                    },
                })
            })
            .collect();
        messages.push(ChatMsg {
            role: "assistant".into(),
            content: content.clone(),
            tool_calls: Some(tc),
            tool_call_id: None,
        });
        for c in calls.values() {
            let _ = tx
                .send(TurnEvent::Tool {
                    name: c.name.clone(),
                    state: "start",
                })
                .await;
            let parsed: serde_json::Value =
                serde_json::from_str(if c.args.is_empty() { "{}" } else { &c.args })
                    .unwrap_or_else(|_| serde_json::json!({}));
            let result = crate::ai_tools::execute(state, &c.name, &parsed, role).await;
            messages.push(ChatMsg {
                role: "tool".into(),
                content: result,
                tool_calls: None,
                tool_call_id: Some(c.id.clone()),
            });
            let _ = tx
                .send(TurnEvent::Tool {
                    name: c.name.clone(),
                    state: "done",
                })
                .await;
        }
        // 下一轮：带着工具结果重新请求
    }

    // 轮数耗尽仍未产出正文：返回空正文，由调用方按"无响应"处理
    Ok(TurnResult {
        content: String::new(),
        reasoning: acc_reasoning,
    })
}

/// POST /api/ai/chat（需登录）
///
/// 请求：`{"messages": [{"role": "user|assistant|system", "content": "..."}]}`
/// 响应：SSE 流（`text/event-stream`），原样透传上游 chat/completions 的
/// chunk（`data: {...}` 行 + `data: [DONE]` 结束标记），前端自行解析增量。
/// 请求体校验（独立成函数以便单元测试）：条数、角色白名单、内容长度。
fn validate(req: &ChatReq) -> Result<(), String> {
    if req.messages.is_empty() {
        return Err("messages 不能为空".into());
    }
    if req.messages.len() > MAX_MESSAGES {
        return Err(format!("消息数超过上限（最多 {MAX_MESSAGES} 条）"));
    }
    for m in &req.messages {
        if !matches!(m.role.as_str(), "system" | "user" | "assistant") {
            return Err(format!("不支持的消息角色：{}", m.role));
        }
        if m.content.is_empty() {
            return Err("消息内容不能为空".into());
        }
        if m.content.len() > MAX_CONTENT_BYTES {
            return Err("单条消息内容过长（上限 32KB）".into());
        }
    }
    Ok(())
}

pub(super) async fn ai_chat(
    State(state): State<AppState>,
    // 提取器本身即鉴权：token 无效/被吊销直接 401
    user: AuthUser,
    Json(req): Json<ChatReq>,
) -> Result<Response, AiError> {
    validate(&req).map_err(AiError::bad)?;

    let (base, key, model) = ai_config(&state.db).await?;

    // 未启用工具：纯透传（既有行为，零变化）
    if !req.use_tools {
        let body = serde_json::json!({
            "model": model,
            "messages": req.messages,
            "stream": true,
        });
        let resp = http_client()
            .post(format!("{base}/chat/completions"))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!("AI 上游连接失败：{e}（base={base}）");
                AiError::upstream("无法连接 AI 服务，请检查上游 API 地址与网络配置")
            })?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!("AI 上游返回 {status}：{text}");
            let brief: String = text.chars().take(300).collect();
            return Err(AiError::upstream(format!(
                "AI 服务返回错误（HTTP {}）：{brief}",
                status.as_u16()
            )));
        }
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let mut resp = resp;
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        if tx.send(Ok(chunk)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!(e))).await;
                        break;
                    }
                }
            }
        });
        let stream = Body::from_stream(ReceiverStream::new(rx));
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(stream)
            .map_err(|_| AiError::upstream("构造流式响应失败"));
    }

    // 启用工具：服务端跑工具循环，把引擎事件翻译成 OpenAI 兼容 SSE chunk。
    // 前端解析逻辑不变：content 增量照常渲染，工具状态以合成 chunk
    // {"tool": {...}} 出现（前端识别后显示 🔧 状态行）。
    let (out_tx, out_rx) = mpsc::channel::<Result<Bytes, anyhow::Error>>(16);
    let st = state.clone();
    let role = user.role.clone();
    let mut msgs = req.messages.clone();
    tokio::spawn(async move {
        let (etx, mut erx) = mpsc::channel(64);
        let engine = tokio::spawn(async move {
            run_turn(
                &st,
                &role,
                Upstream {
                    base: &base,
                    key: &key,
                    model: &model,
                },
                std::mem::take(&mut msgs),
                Some(crate::ai_tools::tools_json()),
                None,
                &etx,
            )
            .await
        });
        while let Some(ev) = erx.recv().await {
            let chunk = match ev {
                TurnEvent::Delta { kind, text } => serde_json::json!({
                    "choices": [{"delta": if kind == "content" {
                        serde_json::json!({"content": text})
                    } else {
                        serde_json::json!({"reasoning_content": text})
                    }}]
                }),
                TurnEvent::Tool { name, state } => serde_json::json!({
                    "tool": {"name": name, "state": state}
                }),
            };
            let line = format!("data: {}\n\n", chunk);
            if out_tx.send(Ok(Bytes::from(line))).await.is_err() {
                engine.abort();
                return;
            }
        }
        match engine.await {
            Ok(Ok(_)) => {
                let _ = out_tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
            }
            Ok(Err(e)) => {
                let c = serde_json::json!({"error": {"message": e.to_string()}});
                let _ = out_tx
                    .send(Ok(Bytes::from(format!("data: {c}\n\n"))))
                    .await;
            }
            Err(_) => {}
        }
    });
    let stream = Body::from_stream(ReceiverStream::new(out_rx));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(stream)
        .map_err(|_| AiError::upstream("构造流式响应失败"))
}

/// 供 api.rs 挂载路由：`.route("/ai/chat", post(ai::ai_chat))`
#[cfg(test)]
mod tests {
    use super::*;

    fn err_of<T>(r: Result<T, AiError>) -> AiError {
        match r {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        }
    }

    // 所有场景共用一个 #[test]：resolve 读进程级环境变量，
    // 拆成多个并行 test 会互相污染（env 是全局的），必须串行执行。
    #[test]
    fn config_resolution() {
        // 1) 设置页与环境变量均未配置密钥：报未配置（BAD_REQUEST）
        std::env::remove_var("AI_API_KEY");
        let e = err_of(resolve(&StoredAiConfig::default()));
        assert_eq!(e.status, StatusCode::BAD_REQUEST);

        // 2) 仅设置页配置密钥即可用，base/model 回退默认值
        let stored = StoredAiConfig {
            key: "sk-stored".into(),
            ..Default::default()
        };
        let (base, key, model) = resolve(&stored).expect("stored key should pass");
        assert_eq!(base, "https://api.openai.com/v1");
        assert_eq!(key, "sk-stored");
        assert_eq!(model, "gpt-4o-mini");

        // 3) 环境变量兜底 + 协议校验
        std::env::set_var("AI_API_KEY", "sk-env");
        std::env::set_var("AI_API_BASE", "ftp://evil");
        let e = err_of(resolve(&StoredAiConfig::default()));
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        assert!(e.message.contains("http(s)"));

        // 4) 设置页覆盖环境变量 + 去尾部斜杠
        let stored = StoredAiConfig {
            base: "https://api.example.com/v1/".into(),
            key: "sk-stored".into(),
            model: "my-model".into(),
        };
        std::env::set_var("AI_API_BASE", "https://env.example.com/v1");
        std::env::set_var("AI_MODEL", "env-model");
        let (base, key, model) = resolve(&stored).expect("valid config should pass");
        assert_eq!(base, "https://api.example.com/v1");
        assert_eq!(key, "sk-stored");
        assert_eq!(model, "my-model");

        // 5) 设置页字段留空 = 该项回退环境变量
        let stored = StoredAiConfig {
            base: "  ".into(),
            key: "sk-stored".into(),
            model: String::new(),
        };
        let (base, _, model) = resolve(&stored).expect("fallback to env");
        assert_eq!(base, "https://env.example.com/v1");
        assert_eq!(model, "env-model");

        std::env::remove_var("AI_API_KEY");
        std::env::remove_var("AI_API_BASE");
        std::env::remove_var("AI_MODEL");
    }

    #[test]
    fn parse_stored_tolerates_bad_data() {
        assert_eq!(parse_stored(None).key, "");
        assert_eq!(parse_stored(Some(String::new())).key, "");
        assert_eq!(parse_stored(Some("not json".into())).base, "");
        let stored = parse_stored(Some(r#"{"base":"https://a/v1","key":"k"}"#.into()));
        assert_eq!(stored.base, "https://a/v1");
        assert_eq!(stored.key, "k");
        assert_eq!(stored.model, "");
    }

    #[test]
    fn request_validation() {
        // 合法：单条 user 消息
        let ok: ChatReq = serde_json::from_str(
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .expect("valid body");
        assert!(validate(&ok).is_ok());

        // 空 messages：serde 层合法（Vec 允许空），但业务校验拒绝
        let empty: ChatReq =
            serde_json::from_str(r#"{"messages":[]}"#).expect("empty vec parses");
        assert!(validate(&empty).is_err());

        // 非法角色 / 空内容 / 超长内容
        let bad_role = ChatReq {
            messages: vec![ChatMsg::text("tool", "x")],
            use_tools: false,
        };
        assert!(validate(&bad_role).is_err());
        let empty_content = ChatReq {
            messages: vec![ChatMsg::text("user", "")],
            use_tools: false,
        };
        assert!(validate(&empty_content).is_err());
        let too_long = ChatReq {
            messages: vec![ChatMsg::text("user", "a".repeat(MAX_CONTENT_BYTES + 1))],
            use_tools: false,
        };
        assert!(validate(&too_long).is_err());
    }

    #[test]
    fn fold_tool_calls_accumulates_fragments() {
        // 典型流式分片：第一帧带 id+name，后续帧只带 arguments 片段
        let mut m: BTreeMap<usize, ToolCallBuf> = BTreeMap::new();
        let f1 = serde_json::json!({
            "tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "get_", "arguments": "{\"li"}}]
        });
        let f2 = serde_json::json!({
            "tool_calls": [{"index": 0, "function": {"name": "limit", "arguments": "mit\":50}"}}]
        });
        // 第二个并行调用（index 1）
        let f3 = serde_json::json!({
            "tool_calls": [{"index": 1, "id": "call_2", "function": {"name": "list_services", "arguments": "{}"}}]
        });
        fold_tool_calls(&mut m, &f1);
        fold_tool_calls(&mut m, &f2);
        fold_tool_calls(&mut m, &f3);
        assert_eq!(m.len(), 2);
        assert_eq!(m[&0].id, "call_1");
        assert_eq!(m[&0].name, "get_limit");
        assert_eq!(m[&0].args, "{\"limit\":50}");
        assert_eq!(m[&1].name, "list_services");
        // 无 tool_calls 的 delta 不影响聚合
        fold_tool_calls(&mut m, &serde_json::json!({"content": "hi"}));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn chatmsg_serializes_without_tool_fields() {
        // skip_serializing_if：普通消息的请求体与改造前一致
        let j = serde_json::to_string(&ChatMsg::text("user", "你好")).unwrap();
        assert_eq!(j, r#"{"role":"user","content":"你好"}"#);
    }
}
