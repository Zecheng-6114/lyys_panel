//! AI 的长期记忆。
//!
//! 自建而非引入现成的记忆框架（Mem0 / Letta / Zep 都是独立的 Python 服务，
//! 为一个面板多养一套服务不划算）：面板本来就有 SQLite，这里只做三件事 ——
//! 存、取、忘。
//!
//! 向量化复用「AI 助手」那个接口的 `/embeddings`（Ollama 原生支持），
//! 因此不引入任何新的本地推理依赖。拿不到向量（接口不支持、没配 embedding
//! 模型、网络不通）时自动退化为关键词重合度召回，记忆功能仍然可用。

use anyhow::{Result, bail};
use serde_json::json;
use std::collections::HashSet;

use crate::ai::{AiConfig, build_agent, brief, transport_error};
use crate::db::Db;

/// 最多注入多少条记忆进上下文
const RECALL_TOP_K: usize = 5;
/// 相似度低于这个值就不注入，避免无关记忆噪声
const MIN_SIMILARITY: f32 = 0.35;
/// 关键词召回至少要有的重合度
const MIN_KEYWORD_SCORE: f32 = 0.12;
/// 向量化请求超时
const EMBED_TIMEOUT_SECS: u64 = 60;

/// settings 里的键
pub const KEY_EMBED_MODEL: &str = "ai.embed_model";
pub const KEY_AUTO_REMEMBER: &str = "ai.auto_remember";
/// 是否允许助手调用工具（全部只读）
pub const KEY_TOOLS_ENABLED: &str = "ai.tools_enabled";
/// 语音合成服务地址（CosyVoice 那类 HTTP 服务）
pub const KEY_TTS_URL: &str = "ai.tts_url";
/// 是否每轮自动朗读回复
pub const KEY_TTS_AUTO: &str = "ai.tts_auto";

/// 调 `/embeddings` 取一段文本的向量。失败返回 None，由调用方决定退化策略。
pub fn embed(cfg: &AiConfig, model: &str, text: &str) -> Option<Vec<f32>> {
    if cfg.base_url.is_empty() || model.is_empty() || text.trim().is_empty() {
        return None;
    }
    let url = format!("{}/embeddings", cfg.base_url);
    let key = cfg.api_key.clone();
    let payload = json!({ "model": model, "input": text }).to_string();
    let agent = build_agent(EMBED_TIMEOUT_SECS);
    let mut req = agent
        .post(&url)
        .set("Content-Type", "application/json");
    if !key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let resp = match req.send_string(&payload) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            tracing::warn!("取向量失败（{}）：{}", code, brief(&body));
            return None;
        }
        Err(ureq::Error::Transport(t)) => {
            tracing::warn!("取向量失败：{}", transport_error(&t, EMBED_TIMEOUT_SECS));
            return None;
        }
    };
    let body = resp.into_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    // 兼容两种形态：OpenAI 的 data[0].embedding 与 Ollama 的 embedding
    let arr = v
        .pointer("/data/0/embedding")
        .or_else(|| v.get("embedding"))?
        .as_array()?;
    Some(arr.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// 中文没有空格，按二元字组切；英文按词切。用于退化路径的关键词召回。
fn tokens(text: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut prev: Option<char> = None;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            continue;
        }
        if let Some(p) = prev {
            if p.is_alphanumeric() || ch.is_alphanumeric() {
                let mut s = String::new();
                s.push(p);
                s.push(ch);
                set.insert(s);
            }
        }
        prev = Some(ch);
    }
    for w in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() > 1)
    {
        set.insert(w.to_lowercase());
    }
    set
}

/// 关键词重合度（Jaccard 的简化版：交集 / 查询词数）
fn keyword_score(query: &str, doc: &str) -> f32 {
    let q = tokens(query);
    if q.is_empty() {
        return 0.0;
    }
    let d = tokens(doc);
    let hit = q.iter().filter(|t| d.contains(*t)).count();
    hit as f32 / q.len() as f32
}

/// 召回与当前输入相关的记忆。
///
/// 有向量就用余弦；任何一步不可用就退化为关键词。两条路都拿不到就返回空，
/// 绝不让记忆检索把对话卡住或报错。
pub async fn recall(
    db: &Db,
    cfg: &AiConfig,
    embed_model: &str,
    query: &str,
) -> Vec<String> {
    let all = match db.ai_memory_vectors_async().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("读取记忆失败：{e}");
            return Vec::new();
        }
    };

    // 语义召回
    let qvec = embed(cfg, embed_model, query);
    if let Some(qv) = qvec {
        let mut scored: Vec<(f32, String)> = all
            .iter()
            .map(|(_, content, vec)| (cosine(&qv, vec), content.clone()))
            .filter(|(s, _)| *s >= MIN_SIMILARITY)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        if !scored.is_empty() {
            return scored
                .into_iter()
                .take(RECALL_TOP_K)
                .map(|(_, c)| c)
                .collect();
        }
    }

    // 关键词召回（向量不可用，或语义上确实没有相近的）
    // 关键词路径要能覆盖没算出向量的记忆，所以另取一次全量
    let listed = db.ai_memory_list_async(500).await.unwrap_or_default();
    let mut scored: Vec<(f32, String)> = listed
        .into_iter()
        .map(|m| (keyword_score(query, &m.content), m.content))
        .filter(|(s, _)| *s >= MIN_KEYWORD_SCORE)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(RECALL_TOP_K).map(|(_, c)| c).collect()
}

/// 把召回结果拼成要注入系统提示词的一段
pub fn memory_block(memories: &[String]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut out = String::from("[记忆] 以下是此前记下的、与这次对话可能相关的事（不是他刚说的话）：\n");
    for m in memories {
        out.push_str("- ");
        out.push_str(m.trim());
        out.push('\n');
    }
    out.push_str("用得上的话自然地用，不要复述这份列表，也不要说明它从哪来。");
    out
}

/// 从一轮对话里抽取值得长期记住的事。
///
/// 让模型自己判断而不是靠规则 —— 规则很难分清「今天重启了服务」和
/// 「他的面板部署在 192.168.1.253」。抽取失败就当这轮没有可记的。
pub fn extract_facts(cfg: &AiConfig, user_text: &str, reply: &str) -> Vec<String> {
    if user_text.trim().is_empty() {
        return Vec::new();
    }
    let prompt = format!(
        "从下面这轮对话里挑出「值得长期记住的、关于他的事实」，每行一条，不带序号与标点修饰。\n\
         只记稳定的信息：称呼、偏好、习惯、住处、设备与环境的配置、说过要办的事、对我的要求。\n\
         不要记：一时的状态（今天很累）、普通的问答内容、约定之外的一次性操作。\n\
         没有值得记的就只输出一个「无」。\n\n\
         他说：{user_text}\n我回：{reply}"
    );
    let url = format!("{}/chat/completions", cfg.base_url);
    let key = cfg.api_key.clone();
    let payload = json!({
        "model": cfg.model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
    })
    .to_string();
    let agent = build_agent(cfg.timeout_secs);
    let mut req = agent
        .post(&url)
        .set("Content-Type", "application/json");
    if !key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let Ok(resp) = req.send_string(&payload) else {
        return Vec::new();
    };
    let Ok(body) = resp.into_string() else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
        return Vec::new();
    };
    let Some(text) = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
    else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', '·', '1', '2', '3', '4', '5', '.', ' ']).trim())
        .filter(|l| !l.is_empty() && *l != "无" && l.len() <= 200)
        .take(5)
        .map(|l| l.to_string())
        .collect()
}

/// 判断一条记忆是否已经存在（避免每次对话都重复记同一条）
pub async fn exists(db: &Db, content: &str) -> Result<bool> {
    let list = db.ai_memory_list_async(500).await?;
    let needle = content.trim();
    Ok(list.iter().any(|m| m.content.trim() == needle))
}

/// 写入一条记忆（顺带算向量）
pub async fn remember(db: &Db, cfg: &AiConfig, embed_model: &str, content: &str) -> Result<i64> {
    let content = content.trim();
    if content.is_empty() {
        bail!("记忆内容不能为空");
    }
    if content.chars().count() > 500 {
        bail!("一条记忆请控制在 500 字以内");
    }
    let vec = embed(cfg, embed_model, content);
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();
    db.ai_memory_add_async(ts, content.to_string(), vec).await
}

/// 读取开关类配置，
pub async fn bool_setting(db: &Db, key: &str, default: bool) -> bool {
    db.get_setting_async(key)
        .await
        .ok()
        .flatten()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

pub async fn set_bool_setting(db: &Db, key: &str, value: bool) -> Result<()> {
    db.set_setting_async(key, if value { "1" } else { "0" }).await
}

