//! 4.5 AI 群聊：多用户 + 多 AI 混合房间，服务端持久化 + WebSocket 实时推送。
//!
//! 第一阶段只做 **AI 之间的互相 @（调度链）**，不含工具调用：
//! - 无 @ 的人类消息 = 闲聊，AI 全体沉默；
//! - @ 指定 AI = 该 AI 直接回复；
//! - @ 管理员 AI = 调度链：管理员思考后自己输出 / 继续委托其他 AI / 总结结束；
//! - AI 回复中 @ 其他 AI 真实触发接话（计入轮数）；AI @ 管理员仅是汇报信号，
//!   链不结束，由管理员判断；管理员总结末尾输出 `[CHAIN_END]` 显式结束；
//! - 某 AI 无响应（上游错误/超时/空回复）→ 自动触发管理员 AI 接管；
//! - 轮数：普通用户触发的链最多 [`MAX_ROUNDS_VIEWER`] 轮，耗尽后强制管理员
//!   总结收尾；面板 admin 触发的链上限 [`MAX_ROUNDS_ADMIN`]；
//! - admin 可强行停止进行中的链（逐块检查停止标记，广播 chain_stopped）。
//!
//! 上游调用复用 [`crate::ai`] 的配置解析与 SSE 语义；成员级覆盖只允许
//! model / api_base，密钥一律走全局配置（密钥不落库）。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use crate::ai::{self, ChatMsg};
use crate::api::AuthUser;
use crate::auth;
use crate::db::{AiMemberRow, AiMessageRow, AiRoomRow};
use crate::AppState;

/// 普通用户触发的调度链最多执行的 AI 轮数
const MAX_ROUNDS_VIEWER: usize = 5;
/// 面板 admin 触发的链轮数上限（防御失控循环，业务上视为"无上限"）
const MAX_ROUNDS_ADMIN: usize = 20;
/// 单轮上游调用的整体超时（连接 + 完整流式读取）
const TURN_TIMEOUT_SECS: u64 = 120;
/// 组装上下文时携带的最近消息条数
const HISTORY_LIMIT: i64 = 64;
/// 单条消息内容上限（与单聊口径一致）
const MAX_CONTENT_BYTES: usize = 32 * 1024;
/// 管理员显式结束链的标记
const CHAIN_END: &str = "[CHAIN_END]";

// ---------- 错误 ----------

/// 与 ai::AiError 同构的 JSON 错误（api::ApiError 构造器私有，不复用）。
#[derive(Debug)]
pub(super) struct GroupError {
    status: StatusCode,
    message: String,
}

impl GroupError {
    fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            message: msg.into(),
        }
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, msg)
    }
    fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, msg)
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, msg)
    }
}

impl From<ai::AiError> for GroupError {
    fn from(e: ai::AiError) -> Self {
        Self::new(e.status_code(), e.to_string())
    }
}

impl From<anyhow::Error> for GroupError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("群聊内部错误：{e:#}");
        Self::internal("内部错误，请稍后再试")
    }
}

impl axum::response::IntoResponse for GroupError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

// ---------- 房间 Hub（广播通道） ----------

/// 每房间一个 broadcast 通道，服务端产生的事件（消息/增量/停止）推给全部
/// 在线连接。通道常驻：房间删除后残留的 Sender 只是少量内存，重启即清。
fn hubs() -> &'static Mutex<HashMap<i64, broadcast::Sender<String>>> {
    static HUBS: OnceLock<Mutex<HashMap<i64, broadcast::Sender<String>>>> = OnceLock::new();
    HUBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn subscribe(room_id: i64) -> broadcast::Receiver<String> {
    let mut map = hubs().lock().expect("hub 锁中毒");
    map.entry(room_id)
        .or_insert_with(|| broadcast::channel(512).0)
        .subscribe()
}

/// 向房间广播一个 JSON 事件；无订阅者时静默丢弃（消息已落库，刷新可见）。
fn publish(room_id: i64, payload: serde_json::Value) {
    let tx = hubs()
        .lock()
        .expect("hub 锁中毒")
        .get(&room_id)
        .cloned();
    if let Some(tx) = tx {
        let _ = tx.send(payload.to_string());
    }
}

// ---------- 调度链状态 ----------

/// 每房间的运行状态：busy 串行化同房间的链（一次只跑一条），
/// stop 为 admin 强停标记（逐块检查）。
struct ChainState {
    busy: AsyncMutex<()>,
    stop: AtomicBool,
}

fn chain_state(room_id: i64) -> std::sync::Arc<ChainState> {
    static CHAINS: OnceLock<Mutex<HashMap<i64, std::sync::Arc<ChainState>>>> = OnceLock::new();
    let mut map = CHAINS.get_or_init(Default::default).lock().expect("链状态锁中毒");
    map.entry(room_id)
        .or_insert_with(|| std::sync::Arc::new(ChainState {
            busy: AsyncMutex::new(()),
            stop: AtomicBool::new(false),
        }))
        .clone()
}

// ---------- @ 解析 ----------

/// 候选名末尾常见标点：全名匹配不中时逐次剥离后重试
/// （"@Coder, 帮忙" 应命中成员 "Coder"，而成员名本身不含这些尾缀）。
fn is_trailing_punct(c: char) -> bool {
    matches!(
        c,
        ',' | '.'
            | ';'
            | ':'
            | '!'
            | '?'
            | '"'
            | '\''
            | ')'
            | ']'
            | '}'
            | '，'
            | '。'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | '）'
            | '】'
            | '」'
            | '』'
    )
}

/// 从文本中解析被 @ 的 AI 成员 id（按出现顺序去重）。
/// 规则：`@` 后连续读取非空白、非 `@` 字符作为候选名，与房间内成员名精确匹配；
/// 全名不中时逐次剥离末尾标点后重试。成员名禁止包含空白与 `@`（创建时校验），
/// 保证解析无歧义。
pub(super) fn parse_mentions(
    text: &str,
    members: &[AiMemberRow],
    exclude_self: Option<i64>,
) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut j = i + 1;
            while j < chars.len() && !chars[j].is_whitespace() && chars[j] != '@' {
                j += 1;
            }
            let mut cand: String = chars[i + 1..j].iter().collect();
            while !cand.is_empty() {
                if let Some(m) = members.iter().find(|m| m.name == cand) {
                    if Some(m.id) != exclude_self && !out.contains(&m.id) {
                        out.push(m.id);
                    }
                    break;
                }
                let last = cand.chars().next_back().expect("cand 非空");
                if !is_trailing_punct(last) {
                    break;
                }
                cand.truncate(cand.len() - last.len_utf8());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// 房间协作规则（注入每个 AI 的 system prompt 末尾）。
fn collab_rules(roster: &[AiMemberRow], admin_names: &[String]) -> String {
    let mut s = String::from(
        "\n\n【群聊协作规则】\
         \n1. 只有消息明确 @ 你时你才需要回复；未 @ 任何 AI 的消息是人类闲聊，保持沉默（服务端已过滤，此规则供你理解上下文）。\
         \n2. 你完成任务后，必须在回复末尾 @ 管理员AI 汇报任务完成。\
         \n3. 遇到无法解决的问题时，@ 管理员AI 求助，或直接 @ 更合适的其他 AI 协作。\
         \n4. 被 @ 只是收到汇报/求助信号，对话不会自动结束：管理员AI 必须思考并决定\
         自己输出、继续委托其他 AI，或总结结束。\
         \n5. 管理员AI 决定结束时，输出总结并在末尾单独写 [CHAIN_END]；除此之外任何成员不得使用该标记。",
    );
    if !roster.is_empty() {
        s.push_str("\n【房间 AI 成员名单】");
        for m in roster {
            s.push_str(&format!(
                "\n- @{}（{}）",
                m.name,
                if m.is_admin { "管理员AI" } else { "普通AI" }
            ));
        }
    }
    if !admin_names.is_empty() {
        s.push_str(&format!("\n【管理员AI】{}", admin_names.join("、")));
    }
    s
}

/// 把房间流水映射为某个 AI 视角的对话历史：
/// 自己的发言 → assistant；他人（人类/其它 AI/系统）→ user 并带来源前缀。
fn build_history(self_name: &str, rows: &[AiMessageRow]) -> Vec<ChatMsg> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let (role, content) = match r.sender_type.as_str() {
            "ai" if r.sender_name == self_name => ("assistant".to_string(), r.content.clone()),
            "ai" => ("user".to_string(), format!("[{}] {}", r.sender_name, r.content)),
            "system" => ("user".to_string(), format!("[系统] {}", r.content)),
            _ => ("user".to_string(), format!("[{}] {}", r.sender_name, r.content)),
        };
        out.push(ChatMsg { role, content });
    }
    out
}

// ---------- 调度链引擎 ----------

/// 单轮结果
enum TurnOutcome {
    /// AI 正常产出：最终文本（已剥离 [CHAIN_END]）、是否由管理员显式结束
    Done { content: String, ended: bool },
    /// 无响应：上游错误 / 超时 / 空回复
    Failed(String),
}

/// 执行一轮 AI 发言：组装上下文 → 流式上游 → 广播增量 → 落库广播全文。
/// `extra_instruction` 用于"轮数耗尽强制管理员总结"等追加指令。
async fn one_turn(
    state: &AppState,
    room_id: i64,
    member: &AiMemberRow,
    roster: &[AiMemberRow],
    cs: &ChainState,
    extra_instruction: Option<&str>,
) -> TurnOutcome {
    let admin_names: Vec<String> = roster.iter().filter(|m| m.is_admin).map(|m| m.name.clone()).collect();
    let mut sys = member.persona.clone();
    sys.push_str(&collab_rules(roster, &admin_names));
    if let Some(x) = extra_instruction {
        sys.push('\n');
        sys.push_str(x);
    }

    let history = match state.db.ai_message_list_async(room_id, HISTORY_LIMIT).await {
        Ok(h) => h,
        Err(e) => return TurnOutcome::Failed(format!("读取房间历史失败：{e}")),
    };
    let mut messages = vec![ChatMsg {
        role: "system".into(),
        content: sys,
    }];
    messages.extend(build_history(&member.name, &history));

    let (base, key, model) =
        match ai::ai_config_with_overrides(&state.db, &member.model, &member.api_base).await {
            Ok(c) => c,
            Err(e) => return TurnOutcome::Failed(e.to_string()),
        };

    // 整体超时包住"发请求 + 读完流"；逐块检查 admin 强停标记。
    let job = async {
        let resp = match ai::stream_completion(&base, &key, &model, &messages).await {
            Ok(r) => r,
            Err(e) => return TurnOutcome::Failed(e.to_string()),
        };
        let mut stream = resp.bytes_stream();
        let mut buf = String::new(); // 未处理的 SSE 残余
        let mut full = String::new(); // 已产出全文
        while let Some(item) = stream.next().await {
            if cs.stop.load(Ordering::Relaxed) {
                return TurnOutcome::Failed("已被管理员停止".into());
            }
            match item {
                Ok(bytes) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(pos) = buf.find('\n') {
                        let line = buf[..pos].trim_end_matches('\r').to_string();
                        buf.drain(..=pos);
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        match ai::sse_delta(data) {
                            Ok(Some(d)) if !d.is_empty() => {
                                full.push_str(&d);
                                publish(
                                    room_id,
                                    serde_json::json!({
                                        "type": "ai_delta",
                                        "room": room_id,
                                        "member_id": member.id,
                                        "delta": d,
                                    }),
                                );
                            }
                            Ok(_) => {}
                            Err(e) => return TurnOutcome::Failed(e.to_string()),
                        }
                    }
                }
                Err(e) => return TurnOutcome::Failed(format!("读取上游流失败：{e}")),
            }
        }
        TurnOutcome::Done { content: full, ended: false }
    };

    let outcome = match tokio::time::timeout(
        std::time::Duration::from_secs(TURN_TIMEOUT_SECS),
        job,
    )
    .await
    {
        Ok(o) => o,
        Err(_) => TurnOutcome::Failed(format!("上游响应超时（>{TURN_TIMEOUT_SECS}s）")),
    };

    // 落库 + 广播全文（仅成功轮）
    if let TurnOutcome::Done { content, .. } = &outcome {
        let mut content = content.clone();
        let mut ended = false;
        if member.is_admin {
            if let Some(pos) = content.find(CHAIN_END) {
                ended = true;
                content.replace_range(pos..pos + CHAIN_END.len(), "");
                content = content.trim_end().to_string();
            }
        }
        if content.trim().is_empty() {
            return TurnOutcome::Failed("上游返回空内容".into());
        }
        let ts = now_ts();
        match state
            .db
            .ai_message_add_async(
                room_id,
                "ai".into(),
                member.id,
                member.name.clone(),
                content.clone(),
                ts,
            )
            .await
        {
            Ok((id, ts)) => publish(
                room_id,
                serde_json::json!({
                    "type": "msg",
                    "room": room_id,
                    "message": {
                        "id": id, "room_id": room_id, "sender_type": "ai",
                        "sender_id": member.id, "sender_name": member.name,
                        "content": content, "ts": ts,
                    },
                }),
            ),
            Err(e) => return TurnOutcome::Failed(format!("消息落库失败：{e}")),
        }
        return TurnOutcome::Done { content, ended };
    }
    outcome
}

/// 写入一条系统消息并广播（调度链的失败接管 / 轮数耗尽等事件对房间可见）。
async fn system_note(state: &AppState, room_id: i64, text: &str) {
    let ts = now_ts();
    if let Ok((id, ts)) = state
        .db
        .ai_message_add_async(room_id, "system".into(), 0, "系统".into(), text.into(), ts)
        .await
    {
        publish(
            room_id,
            serde_json::json!({
                "type": "msg",
                "room": room_id,
                "message": {
                    "id": id, "room_id": room_id, "sender_type": "system",
                    "sender_id": 0, "sender_name": "系统",
                    "content": text, "ts": ts,
                },
            }),
        );
    }
}

fn now_ts() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// 调度链主循环：从 `initial` 队列依次触发 AI，回复中的 @ 继续入队，
/// 直到 [CHAIN_END] / 队列耗尽 / 轮数上限 / 被强停。
/// 同房间同时只跑一条链（busy 串行化；后来的人类消息只落库，由链逐轮读新历史）。
async fn run_chain(
    state: AppState,
    room_id: i64,
    initial: Vec<i64>,
    trigger_is_admin: bool,
) {
    let cs = chain_state(room_id);
    // try_lock：已有链在跑则广播提示后退出（消息已持久化，正在跑的链会读到）。
    // guard 持有到本函数结束 = 同房间串行化。
    let Ok(_guard) = cs.busy.try_lock() else {
        publish(
            room_id,
            serde_json::json!({
                "type": "chain_busy", "room": room_id,
                "note": "调度链进行中，你的消息已记录，当前链会读取后续回复",
            }),
        );
        return;
    };
    cs.stop.store(false, Ordering::Relaxed);

    let roster = match state.db.ai_member_list_async(room_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("读取房间成员失败：{e}");
            return;
        }
    };
    let admins: Vec<&AiMemberRow> = roster.iter().filter(|m| m.is_admin).collect();
    let max_rounds = if trigger_is_admin {
        MAX_ROUNDS_ADMIN
    } else {
        MAX_ROUNDS_VIEWER
    };

    let mut queue: VecDeque<i64> = initial.into();
    let mut turns = 0usize;
    let mut ended = false;

    while let Some(mid) = queue.pop_front() {
        if cs.stop.load(Ordering::Relaxed) {
            system_note(&state, room_id, "调度链已被管理员停止").await;
            publish(room_id, serde_json::json!({ "type": "chain_stopped", "room": room_id }));
            return;
        }
        if turns >= max_rounds {
            break;
        }
        let member = match roster.iter().find(|m| m.id == mid) {
            Some(m) => m.clone(),
            None => continue, // 成员已被删除
        };
        turns += 1;

        // 广播"开始输出"占位（前端渲染打字气泡）
        publish(
            room_id,
            serde_json::json!({
                "type": "ai_start", "room": room_id,
                "member_id": member.id, "member_name": member.name,
            }),
        );
        let outcome = one_turn(&state, room_id, &member, &roster, &cs, None).await;
        let (content, chain_ended) = match outcome {
            TurnOutcome::Done { content, ended } => (content, ended),
            TurnOutcome::Failed(msg) => {
                system_note(&state, room_id, &format!("「{}」无响应：{msg}", member.name)).await;
                // 无响应 → 管理员接管（失败者本身是管理员则不再自举）
                if !member.is_admin {
                    for a in &admins {
                        if !queue.contains(&a.id) {
                            queue.push_back(a.id);
                        }
                    }
                }
                continue;
            }
        };
        if chain_ended {
            ended = true;
            break;
        }
        // AI 回复中的 @ 继续触发（排除自己 @ 自己）
        for id in parse_mentions(&content, &roster, Some(member.id)) {
            if !queue.contains(&id) {
                queue.push_back(id);
            }
        }
    }

    // 轮数耗尽仍未结束：强制管理员总结收尾
    if !ended && turns >= max_rounds {
        if let Some(a) = admins.first() {
            system_note(
                &state,
                room_id,
                &format!("调度链已达轮数上限（{max_rounds} 轮），请管理员AI「{}」立即总结收尾", a.name),
            )
            .await;
            publish(
                room_id,
                serde_json::json!({
                    "type": "ai_start", "room": room_id,
                    "member_id": a.id, "member_name": a.name,
                }),
            );
            let extra = "轮数已用尽，请立即总结当前协作进展并结束本轮调度，在回复末尾输出 [CHAIN_END]。";
            if let TurnOutcome::Done { ended, .. } =
                one_turn(&state, room_id, a, &roster, &cs, Some(extra)).await
            {
                if ended {
                    return;
                }
            }
        }
        // 没有管理员成员或总结失败：链到此为止
        system_note(&state, room_id, "调度链已达轮数上限，自动结束").await;
        publish(room_id, serde_json::json!({ "type": "chain_stopped", "room": room_id }));
    }
}

// ---------- WebSocket ----------

#[derive(Deserialize)]
pub(super) struct GroupWsQuery {
    pub room: i64,
    pub token: String,
}

/// WS 手动鉴权（与 api::ws_auth 同口径），额外返回连接者身份供房间归属判断。
async fn ws_auth_user(
    state: &AppState,
    token: &str,
) -> Result<(i64, String, bool), GroupError> {
    let claims = auth::verify_token(&state.jwt_secret, token)
        .map_err(|_| GroupError::unauthorized("登录已过期，请重新登录"))?;
    if state.revocations.is_revoked(&claims.jti) {
        return Err(GroupError::unauthorized("登录已失效，请重新登录"));
    }
    if !state.db.session_exists_async(&claims.jti).await? {
        return Err(GroupError::unauthorized("登录已失效，请重新登录"));
    }
    let user = state
        .db
        .user_by_id_async(claims.sub)
        .await?
        .ok_or_else(|| GroupError::unauthorized("账号已被删除，请重新登录"))?;
    if user.2 {
        return Err(GroupError::forbidden("请先修改初始密码"));
    }
    Ok((claims.sub, user.0, user.1 == "admin"))
}

/// GET /api/ai/ws?room=<id>&token=<t>
pub(super) async fn ai_group_ws(
    State(state): State<AppState>,
    Query(q): Query<GroupWsQuery>,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, GroupError> {
    let (uid, uname, is_admin) = ws_auth_user(&state, &q.token).await?;
    if !state.db.ai_room_can_access_async(q.room, uid, is_admin).await? {
        return Err(GroupError::forbidden("无权访问该房间"));
    }
    Ok(ws.on_upgrade(move |socket| room_socket(socket, state, q.room, uid, uname)))
}

/// 单连接生命周期：读客户端帧（user_msg / stop）+ 转发 Hub 事件。
async fn room_socket(
    socket: WebSocket,
    state: AppState,
    room_id: i64,
    uid: i64,
    uname: String,
) {
    let (mut sink, mut stream) = socket.split();
    let mut rx = subscribe(room_id);
    // 客户端 → 服务端 的读侧放到独立任务：socket 已拆分，两任务各持一半。
    // 通道元素为客户端 JSON 帧原文；读侧结束（连接断开）时发送端 drop，主循环感知。
    let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<String>(64);
    let reader = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    if in_tx.send(t.to_string()).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {} // Ping/Pong/Binary 忽略
            }
        }
    });

    // 连接建立先推一份最近历史给本连接（只发自己，避免 Hub 重复回放给他人）
    if let Ok(rows) = state.db.ai_message_list_async(room_id, HISTORY_LIMIT).await {
        let payload = serde_json::json!({ "type": "history", "room": room_id, "messages": rows });
        if sink.send(Message::Text(payload.to_string().into())).await.is_err() {
            reader.abort();
            return;
        }
    }

    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break; // 已断开
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            frame = in_rx.recv() => match frame {
                Some(text) => handle_client_frame(&state, room_id, uid, &uname, &text).await,
                None => break, // 读侧任务结束 = 连接关闭
            },
        }
    }
    reader.abort();
}

/// 处理客户端 JSON 帧：`{"type":"user_msg","content":"..."}` / `{"type":"stop"}`
async fn handle_client_frame(state: &AppState, room_id: i64, uid: i64, uname: &str, raw: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("user_msg") => {
            let Some(content) = v.get("content").and_then(|c| c.as_str()) else {
                return;
            };
            let content = content.trim();
            if content.is_empty() || content.len() > MAX_CONTENT_BYTES {
                return;
            }
            let ts = now_ts();
            let Ok((id, ts)) = state
                .db
                .ai_message_add_async(
                    room_id,
                    "user".into(),
                    uid,
                    uname.to_string(),
                    content.to_string(),
                    ts,
                )
                .await
            else {
                return;
            };
            publish(
                room_id,
                serde_json::json!({
                    "type": "msg", "room": room_id,
                    "message": {
                        "id": id, "room_id": room_id, "sender_type": "user",
                        "sender_id": uid, "sender_name": uname,
                        "content": content, "ts": ts,
                    },
                }),
            );
            // 触发调度：无 @ = 沉默；有 @ = 队列
            let roster = match state.db.ai_member_list_async(room_id).await {
                Ok(r) => r,
                Err(_) => return,
            };
            let mentions = parse_mentions(content, &roster, None);
            if !mentions.is_empty() {
                let st = state.clone();
                let is_admin = {
                    match st.db.user_by_id_async(uid).await {
                        Ok(Some(u)) => u.1 == "admin",
                        _ => false,
                    }
                };
                tokio::spawn(run_chain(st, room_id, mentions, is_admin));
            }
        }
        Some("stop") => {
            // 仅面板 admin 可强停进行中的链
            let admin = match state.db.user_by_id_async(uid).await {
                Ok(Some(u)) => u.1 == "admin",
                _ => false,
            };
            if admin {
                chain_state(room_id).stop.store(true, Ordering::Relaxed);
            }
        }
        _ => {}
    }
}

// ---------- REST：房间 / 成员 / 消息 ----------

#[derive(Deserialize)]
pub(super) struct RoomCreateReq {
    name: String,
    #[serde(default)]
    visibility: Option<String>,
}

/// GET /api/ai/rooms —— 当前用户可见的房间列表
pub(super) async fn rooms_list(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<AiRoomRow>>, GroupError> {
    Ok(Json(
        state
            .db
            .ai_room_list_async(user.id, user.is_admin())
            .await?,
    ))
}

/// POST /api/ai/rooms —— 创建房间。public 仅面板 admin 可建；
/// 普通用户最多创建 5 个房间，超限需联系管理员。
pub(super) async fn rooms_create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<RoomCreateReq>,
) -> Result<Json<AiRoomRow>, GroupError> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(GroupError::bad("房间名称长度须为 1~64 字符"));
    }
    let visibility = match req.visibility.as_deref().map(str::trim) {
        None | Some("") | Some("private") => "private",
        Some("public") if user.is_admin() => "public",
        Some("public") => return Err(GroupError::forbidden("仅管理员可创建公开房间")),
        Some(_) => return Err(GroupError::bad("可见性仅支持 private / public")),
    };

    // 普通用户最多创建 5 个房间
    if !user.is_admin() {
        let cnt = state.db.ai_room_count_by_creator_async(user.id).await?;
        if cnt >= 5 {
            return Err(GroupError::forbidden(
                "普通用户最多创建 5 个房间，如需更多请联系管理员审批",
            ));
        }
    }

    let id = state
        .db
        .ai_room_add_async(name.into(), user.id, visibility.into(), now_ts())
        .await?;
    Ok(Json(AiRoomRow {
        id,
        name: name.into(),
        creator_id: user.id,
        visibility: visibility.into(),
        created: now_ts(),
    }))
}

/// POST /api/ai/rooms/default —— 幂等确保当前用户有默认单 AI 房间（首次进入自动建）
pub(super) async fn rooms_default(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<AiRoomRow>, GroupError> {
    const DEFAULT_NAME: &str = "默认助手";
    let rooms = state
        .db
        .ai_room_list_async(user.id, user.is_admin())
        .await?;
    if let Some(r) = rooms.iter().find(|r| r.creator_id == user.id && r.name == DEFAULT_NAME) {
        return Ok(Json(r.clone()));
    }
    let id = state
        .db
        .ai_room_add_async(DEFAULT_NAME.into(), user.id, "private".into(), now_ts())
        .await?;
    state
        .db
        .ai_member_add_async(
            id,
            "助手".into(),
            "你是面板内置的 AI 助手「助手」，同时也是本房间的管理员AI：负责接收其它 AI 的汇报与求助，思考后决定自己回答、委托其它 AI，或总结结束。".into(),
            true,
            String::new(),
            String::new(),
            0,
        )
        .await?;
    let room = state
        .db
        .ai_room_list_async(user.id, user.is_admin())
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| GroupError::internal("默认房间创建后不可见"))?;
    Ok(Json(room))
}

#[derive(Deserialize)]
pub(super) struct RoomIdReq {
    id: i64,
}

/// POST /api/ai/rooms/remove —— 创建者或面板 admin 删除房间
pub(super) async fn rooms_remove(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<RoomIdReq>,
) -> Result<Json<serde_json::Value>, GroupError> {
    let room = state.db.ai_room_get_async(req.id).await?.ok_or_else(|| GroupError::bad("房间不存在"))?;
    if room.1 != user.id && !user.is_admin() {
        return Err(GroupError::forbidden("只有创建者或管理员可以删除房间"));
    }
    state.db.ai_room_delete_async(req.id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub(super) struct RoomQuery {
    room: i64,
}

/// GET /api/ai/members?room=<id>
pub(super) async fn members_list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<RoomQuery>,
) -> Result<Json<Vec<AiMemberRow>>, GroupError> {
    check_access(&state, &user, q.room).await?;
    Ok(Json(state.db.ai_member_list_async(q.room).await?))
}

#[derive(Deserialize)]
pub(super) struct MemberReq {
    room_id: i64,
    name: String,
    #[serde(default)]
    persona: Option<String>,
    #[serde(default)]
    is_admin: Option<bool>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_base: Option<String>,
    #[serde(default)]
    sort: Option<i64>,
}

fn validate_member_name(name: &str) -> Result<String, GroupError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 32 {
        return Err(GroupError::bad("成员名称长度须为 1~32 字符"));
    }
    if name.contains('@') || name.chars().any(|c| c.is_whitespace()) {
        return Err(GroupError::bad("成员名称不得包含空白或 @ 字符"));
    }
    Ok(name.to_string())
}

/// 房间管理权限：创建者或面板 admin（成员/邀请/删除同口径）
async fn check_manage(
    state: &AppState,
    user: &AuthUser,
    room_id: i64,
) -> Result<(), GroupError> {
    let room = state.db.ai_room_get_async(room_id).await?.ok_or_else(|| GroupError::bad("房间不存在"))?;
    if room.1 != user.id && !user.is_admin() {
        return Err(GroupError::forbidden("只有房间创建者或管理员可以管理成员"));
    }
    Ok(())
}

async fn check_access(state: &AppState, user: &AuthUser, room_id: i64) -> Result<(), GroupError> {
    if !state
        .db
        .ai_room_can_access_async(room_id, user.id, user.is_admin())
        .await?
    {
        return Err(GroupError::forbidden("无权访问该房间"));
    }
    Ok(())
}

/// POST /api/ai/members —— 添加 AI 成员
pub(super) async fn members_create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MemberReq>,
) -> Result<Json<AiMemberRow>, GroupError> {
    check_manage(&state, &user, req.room_id).await?;
    let name = validate_member_name(&req.name)?;
    if state.db.ai_member_by_name_async(req.room_id, name.clone()).await?.is_some() {
        return Err(GroupError::bad("房间内已存在同名 AI 成员"));
    }
    let persona = req.persona.unwrap_or_default().trim().to_string();
    let is_admin = req.is_admin.unwrap_or(false);
    let model = req.model.unwrap_or_default().trim().to_string();
    let api_base = req.api_base.unwrap_or_default().trim().to_string();
    let sort = req.sort.unwrap_or(0);
    let id = state
        .db
        .ai_member_add_async(
            req.room_id,
            name.clone(),
            persona.clone(),
            is_admin,
            model.clone(),
            api_base.clone(),
            sort,
        )
        .await?;
    Ok(Json(AiMemberRow {
        id,
        room_id: req.room_id,
        name,
        persona,
        is_admin,
        model,
        api_base,
        sort,
    }))
}

#[derive(Deserialize)]
pub(super) struct MemberUpdateReq {
    id: i64,
    name: String,
    #[serde(default)]
    persona: Option<String>,
    #[serde(default)]
    is_admin: Option<bool>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_base: Option<String>,
}

/// POST /api/ai/members/update
pub(super) async fn members_update(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MemberUpdateReq>,
) -> Result<Json<serde_json::Value>, GroupError> {
    let member = state
        .db
        .ai_member_get_async(req.id)
        .await?
        .ok_or_else(|| GroupError::bad("成员不存在"))?;
    check_manage(&state, &user, member.0).await?;
    let name = validate_member_name(&req.name)?;
    if let Some(other) = state.db.ai_member_by_name_async(member.0, name.clone()).await? {
        if other.0 != req.id {
            return Err(GroupError::bad("房间内已存在同名 AI 成员"));
        }
    }
    state
        .db
        .ai_member_update_async(
            req.id,
            name,
            req.persona.unwrap_or_default().trim().to_string(),
            req.is_admin.unwrap_or(false),
            req.model.unwrap_or_default().trim().to_string(),
            req.api_base.unwrap_or_default().trim().to_string(),
        )
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub(super) struct MemberIdReq {
    id: i64,
}

/// POST /api/ai/members/remove
pub(super) async fn members_remove(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MemberIdReq>,
) -> Result<Json<serde_json::Value>, GroupError> {
    let member = state
        .db
        .ai_member_get_async(req.id)
        .await?
        .ok_or_else(|| GroupError::bad("成员不存在"))?;
    check_manage(&state, &user, member.0).await?;
    state.db.ai_member_delete_async(req.id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub(super) struct InviteReq {
    room_id: i64,
    user_id: i64,
}

/// POST /api/ai/rooms/invite —— 邀请用户进私有房间
pub(super) async fn rooms_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<InviteReq>,
) -> Result<Json<serde_json::Value>, GroupError> {
    check_manage(&state, &user, req.room_id).await?;
    if !state.db.user_exists_async(req.user_id).await? {
        return Err(GroupError::bad("受邀用户不存在"));
    }
    state.db.ai_room_invite_async(req.room_id, req.user_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /api/ai/rooms/uninvite
pub(super) async fn rooms_uninvite(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<InviteReq>,
) -> Result<Json<serde_json::Value>, GroupError> {
    check_manage(&state, &user, req.room_id).await?;
    state.db.ai_room_uninvite_async(req.room_id, req.user_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// GET /api/ai/rooms/users?room=<id> —— 受邀用户 id 列表
pub(super) async fn rooms_users(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<RoomQuery>,
) -> Result<Json<Vec<i64>>, GroupError> {
    check_manage(&state, &user, q.room).await?;
    Ok(Json(state.db.ai_room_users_async(q.room).await?))
}

/// GET /api/ai/messages?room=<id> —— 房间最近消息（REST 兜底，WS 首屏用）
pub(super) async fn messages_list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<RoomQuery>,
) -> Result<Json<Vec<AiMessageRow>>, GroupError> {
    check_access(&state, &user, q.room).await?;
    Ok(Json(state.db.ai_message_list_async(q.room, HISTORY_LIMIT).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: i64, name: &str, is_admin: bool) -> AiMemberRow {
        AiMemberRow {
            id,
            room_id: 1,
            name: name.into(),
            persona: String::new(),
            is_admin,
            model: String::new(),
            api_base: String::new(),
            sort: 0,
        }
    }

    #[test]
    fn mentions_parse_in_order_dedup_and_exclude_self() {
        let roster = vec![member(1, "助手", true), member(2, "Coder", false), member(3, "审核员", false)];
        // 顺序 + 去重
        assert_eq!(
            parse_mentions("@Coder 看一下，再 @Coder 和 @审核员", &roster, None),
            vec![2, 3]
        );
        // 排除自己 @ 自己
        assert_eq!(parse_mentions("@助手 @Coder", &roster, Some(1)), vec![2]);
        // 无 @ / 未知名字 → 空
        assert!(parse_mentions("普通闲聊", &roster, None).is_empty());
        assert!(parse_mentions("@不存在的人 来一下", &roster, None).is_empty());
        // @ 紧跟空白不成立；名字止于空白
        assert_eq!(parse_mentions("@Coder, 帮忙", &roster, None), vec![2]);
    }

    #[test]
    fn history_maps_own_speech_to_assistant() {
        let rows = vec![
            AiMessageRow { id: 1, room_id: 1, sender_type: "user".into(), sender_id: 9, sender_name: "alice".into(), content: "做个页面".into(), ts: 0 },
            AiMessageRow { id: 2, room_id: 1, sender_type: "ai".into(), sender_id: 2, sender_name: "Coder".into(), content: "我来".into(), ts: 1 },
            AiMessageRow { id: 3, room_id: 1, sender_type: "ai".into(), sender_id: 1, sender_name: "助手".into(), content: "收到".into(), ts: 2 },
            AiMessageRow { id: 4, room_id: 1, sender_type: "system".into(), sender_id: 0, sender_name: "系统".into(), content: "超时".into(), ts: 3 },
        ];
        let h = build_history("助手", &rows);
        assert_eq!(h[0].role, "user");
        assert_eq!(h[0].content, "[alice] 做个页面");
        assert_eq!(h[1].content, "[Coder] 我来");
        assert_eq!(h[2].role, "assistant");
        assert_eq!(h[2].content, "收到");
        assert_eq!(h[3].content, "[系统] 超时");
    }

    #[test]
    fn chain_end_marker_detection() {
        // one_turn 的剥离逻辑：标记存在 → ended=true 且文本不含标记
        let mut content = "总结完毕。\n[CHAIN_END]".to_string();
        let pos = content.find(CHAIN_END).expect("marker present");
        content.replace_range(pos..pos + CHAIN_END.len(), "");
        let content = content.trim_end().to_string();
        assert_eq!(content, "总结完毕。");
    }
}
