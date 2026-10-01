//! AI 助手：把 OpenAI 兼容的 chat/completions 接口以流式方式转发给前端。
//!
//! 设计边界（4.4 AI 重写范围）：
//! - 只做无状态转发：不存会话、不做记忆、不做工具调用，历史由前端随请求携带；
//! - 上游地址 / 密钥 / 模型来自设置页配置（settings 表）或环境变量，
//!   密钥不进日志、不向前端明文回显；
//! - 流式透传上游 SSE 字节流，前端按 SSE 解析，后端不做内容改写。
//!
//! 配置来源与优先级（高 → 低）：
//! 1. 设置页配置（settings 表 `ai_api_config`，经 /api/ai/config 保存）；
//! 2. 环境变量（panel.env / systemd，改后重启面板生效）；
//! 3. 内置默认值（base=`https://api.openai.com/v1`，model=`gpt-4o-mini`）。
//!    密钥没有默认值：设置页与环境变量都未配置时，对话请求返回 400。

use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
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
#[derive(Deserialize)]
pub(super) struct ChatReq {
    messages: Vec<ChatMsg>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct ChatMsg {
    pub(super) role: String,
    pub(super) content: String,
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
pub(super) async fn stream_completion(
    base: &str,
    key: &str,
    model: &str,
    messages: &[ChatMsg],
) -> Result<reqwest::Response, AiError> {
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "think": true,
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
    Ok(resp)
}

/// 群聊模块复用：从一条 SSE `data:` 负载中提取正文与思考增量。
/// 思考型模型在 delta 里携带思维链，字段名因上游而异：
/// DeepSeek/vLLM 用 `reasoning_content`，Ollama 用 `reasoning`；
/// `content` 才是正式回答。两者都返回，调用方分别处理。
/// 非 chunk JSON 或无增量返回 None。上游错误对象（chunk.error）转成 Err。
pub(super) type SseDelta = (Option<String>, Option<String>);

pub(super) fn sse_delta(data: &str) -> Result<Option<SseDelta>, AiError> {
    let chunk: serde_json::Value = serde_json::from_str(data)
        .map_err(|_| AiError::upstream("AI 上游返回了无法解析的数据块"))?;
    if let Some(msg) = chunk
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Err(AiError::upstream(msg.to_string()));
    }
    let delta = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"));
    let content = delta
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let reasoning = delta
        .and_then(|d| {
            d.get("reasoning_content")
                .or_else(|| d.get("reasoning"))
        })
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    if content.is_none() && reasoning.is_none() {
        return Ok(None);
    }
    Ok(Some((content, reasoning)))
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
    _user: AuthUser,
    Json(req): Json<ChatReq>,
) -> Result<Response, AiError> {
    validate(&req).map_err(AiError::bad)?;

    let (base, key, model) = ai_config(&state.db).await?;
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
            // 只记错误与地址，绝不记录密钥
            tracing::warn!("AI 上游连接失败：{e}（base={base}）");
            AiError::upstream("无法连接 AI 服务，请检查上游 API 地址与网络配置")
        })?;

    let status = resp.status();
    if !status.is_success() {
        // 上游错误：截取一段错误体帮助定位（多为 quota/key/model 问题），
        // 同时完整错误进 tracing 日志。
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("AI 上游返回 {status}：{text}");
        let brief: String = text.chars().take(300).collect();
        return Err(AiError::upstream(format!(
            "AI 服务返回错误（HTTP {}）：{brief}",
            status.as_u16()
        )));
    }

    // 上游 2xx：把响应体按字节块搬到下游 SSE 流。用独立任务做搬运，
    // 前端中断（AbortController）时接收端被丢弃，send 失败自然退出。
    // 通道元素类型由 chunk 的类型反推（Result<Bytes, anyhow::Error>）。
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
            messages: vec![ChatMsg {
                role: "tool".into(),
                content: "x".into(),
            }],
        };
        assert!(validate(&bad_role).is_err());
        let empty_content = ChatReq {
            messages: vec![ChatMsg {
                role: "user".into(),
                content: String::new(),
            }],
        };
        assert!(validate(&empty_content).is_err());
        let too_long = ChatReq {
            messages: vec![ChatMsg {
                role: "user".into(),
                content: "a".repeat(MAX_CONTENT_BYTES + 1),
            }],
        };
        assert!(validate(&too_long).is_err());
    }
}
