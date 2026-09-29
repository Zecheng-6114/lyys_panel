//! AI 助手：把 OpenAI 兼容的 chat/completions 接口以流式方式转发给前端。
//!
//! 设计边界（4.4 AI 重写范围）：
//! - 只做无状态转发：不存会话、不做记忆、不做工具调用，历史由前端随请求携带；
//! - 上游地址 / 密钥 / 模型全部来自环境变量，不落数据库、不进日志；
//! - 流式透传上游 SSE 字节流，前端按 SSE 解析，后端不做内容改写。
//!
//! 配置（panel.env / systemd 环境变量，改后重启面板生效）：
//! - `AI_API_KEY`  必填，上游 API 密钥（Bearer Token）；
//! - `AI_API_BASE` 选填，默认 `https://api.openai.com/v1`，填到 /v1 为止；
//! - `AI_MODEL`    选填，默认 `gpt-4o-mini`。

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
use crate::AppState;

/// 请求体上限：单条消息内容最大字节数（UTF-8）。防御性限制，正常问答远用不到。
const MAX_CONTENT_BYTES: usize = 32 * 1024;
/// 请求体上限：一次请求最多携带的消息条数（含 system/assistant 历史）。
const MAX_MESSAGES: usize = 64;

/// 本模块专用错误：与 api::ApiError 同构（JSON `{"error": ...}`），
/// 因 ApiError 的构造器不对外公开，这里单独实现一份。
#[derive(Debug)]
pub(super) struct AiError {
    status: StatusCode,
    message: String,
}

impl AiError {
    fn bad(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn upstream(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: msg.into(),
        }
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
    role: String,
    content: String,
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

/// 读取并校验 AI 配置。每次请求都读环境变量：改配置后重启面板即可生效，
/// 无需为此引入配置热更新机制。
fn ai_config() -> Result<(String, String, String), AiError> {
    let key = std::env::var("AI_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            AiError::bad("AI 功能未配置：请在面板环境变量中设置 AI_API_KEY 后重启面板")
        })?;
    let base = std::env::var("AI_API_BASE")
        .unwrap_or_else(|_| "https://api.openai.com/v1".into())
        .trim()
        .trim_end_matches('/')
        .to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(AiError::bad("AI_API_BASE 配置无效：必须以 http(s):// 开头"));
    }
    let model = std::env::var("AI_MODEL")
        .unwrap_or_else(|_| "gpt-4o-mini".into())
        .trim()
        .to_string();
    if model.is_empty() {
        return Err(AiError::bad("AI_MODEL 配置无效：不能为空"));
    }
    Ok((base, key, model))
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
    State(_state): State<AppState>,
    // 提取器本身即鉴权：token 无效/被吊销直接 401
    _user: AuthUser,
    Json(req): Json<ChatReq>,
) -> Result<Response, AiError> {
    validate(&req).map_err(AiError::bad)?;

    let (base, key, model) = ai_config()?;
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
            tracing::warn!("AI 上游连接失败：{e}");
            AiError::upstream("无法连接 AI 服务，请检查 AI_API_BASE 网络配置")
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

    // 三个场景共用一个 #[test]：ai_config 读进程级环境变量，
    // 拆成多个并行 test 会互相污染（env 是全局的），必须串行执行。
    #[test]
    fn config_env_handling() {
        // 1) 未配置密钥：报未配置（BAD_REQUEST）
        std::env::remove_var("AI_API_KEY");
        let e = err_of(ai_config());
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        assert!(e.message.contains("AI_API_KEY"));

        // 2) base 协议非法：拒绝
        std::env::set_var("AI_API_KEY", "sk-test");
        std::env::set_var("AI_API_BASE", "ftp://evil");
        let e = err_of(ai_config());
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        assert!(e.message.contains("AI_API_BASE"));

        // 3) 缺省值 + 去尾部斜杠
        std::env::set_var("AI_API_KEY", "sk-test");
        std::env::set_var("AI_API_BASE", "https://api.example.com/v1/");
        std::env::remove_var("AI_MODEL");
        let (base, key, model) = ai_config().expect("valid config should pass");
        assert_eq!(base, "https://api.example.com/v1");
        assert_eq!(key, "sk-test");
        assert_eq!(model, "gpt-4o-mini");

        std::env::remove_var("AI_API_KEY");
        std::env::remove_var("AI_API_BASE");
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
