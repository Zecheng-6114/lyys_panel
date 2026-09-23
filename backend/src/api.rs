use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Multipart, Query, State};
use axum::http::{header, request::Parts, StatusCode};
// AI 助手功能暂时停用（见文件末尾 "AI 助手已停用" 说明），以下导入仅 AI 段使用
// use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
// use std::convert::Infallible;
// use tokio_stream::wrappers::ReceiverStream;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::auth;
use crate::monitor;
use crate::opservice;
use crate::rprocess;
use crate::AppState;

/// 统一 API 错误：携带 HTTP 状态码与中文消息
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: msg.into(),
        }
    }
    /// 429：用于登录退避等限流场景
    fn too_many_requests(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: msg.into(),
        }
    }
    /// 将任意 Display 错误转为 400（文件模块用，携带完整错误链）
    fn file_err(e: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: e.to_string(),
        }
    }
}

/// 供审计模块把失败原因写进 detail
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::fmt::Debug for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiError({}): {}", self.status.as_u16(), self.message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("内部错误：{e:#}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

/// 认证后的当前用户（从 Authorization: Bearer <token> 解析）
///
/// 当前只区分「已登录」，handler 用它作为守卫参数即可。保留 `id` 是为了
/// 后续按用户区分数据（多管理员、个人偏好）时不用改鉴权链路。
#[allow(dead_code)]
pub struct AuthUser {
    /// 用户 id（MVP 单管理员）
    pub id: i64,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::unauthorized("缺少认证信息"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::unauthorized("认证头格式错误"))?;
        let id = auth::verify_token(&state.jwt_secret, token)
            .map_err(|_| ApiError::unauthorized("登录已过期，请重新登录"))?;
        Ok(AuthUser { id })
    }
}

/// 从请求扩展中取出来源 IP；取不到（如未启用连接信息）时退回环回地址。
///
/// 单独抽成函数，供 [`AuthUser`] 与 [`ClientIp`] 共用，保证两处口径一致。
fn client_ip(parts: &Parts) -> IpAddr {
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// 登录请求体
#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

/// 登录响应体
#[derive(Serialize)]
struct LoginResp {
    token: String,
    username: String,
}

/// 登录失败退避不需要认证，但需要来源 IP 用于限流。
///
/// axum 0.8 的 `ConnectInfo` 没有 `OptionalFromRequestParts` 实现，
/// 直接用它会在缺少连接信息时整体拒绝请求。这里自己取，
/// 取不到就视为环回地址 —— 限流退化为按用户名计数，而不是完全不限流。
struct ClientIp(IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(client_ip(parts)))
    }
}

async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(req): Json<LoginReq>,
) -> Result<Json<LoginResp>, ApiError> {
    let wait = state.throttle.retry_after(ip, &req.username);
    if !wait.is_zero() {
        let secs = wait.as_secs().max(1);
        return Err(ApiError::too_many_requests(format!(
            "登录尝试过于频繁，请 {secs} 秒后再试"
        )));
    }

    let user = state.db.find_user_async(&req.username).await?;
    let Some((id, hash, _salt)) = user else {
        let delay = state.throttle.record_failure(ip, &req.username);
        tracing::warn!(
            "登录失败（用户不存在）：user={} ip={} 退避={}s",
            req.username,
            ip,
            delay.as_secs()
        );
        return Err(ApiError::unauthorized("用户名或密码错误"));
    };
    // argon2 校验是 CPU 密集操作（默认参数下约 100ms），必须离开异步工作线程，
    // 否则并发登录会把 tokio 的线程池占满。
    let password = req.password.clone();
    let hash_for_verify = hash.clone();
    let verified = tokio::task::spawn_blocking(move || {
        auth::verify_password(&password, &hash_for_verify)
    })
    .await
    .map_err(ApiError::file_err)?;
    if !verified {
        let delay = state.throttle.record_failure(ip, &req.username);
        tracing::warn!(
            "登录失败（密码错误）：user={} ip={} 退避={}s",
            req.username,
            ip,
            delay.as_secs()
        );
        return Err(ApiError::unauthorized("用户名或密码错误"));
    }

    state.throttle.record_success(ip, &req.username);
    let token = auth::issue_token(&state.jwt_secret, id)?;
    tracing::info!("登录成功：user={} ip={}", req.username, ip);
    Ok(Json(LoginResp {
        token,
        username: req.username,
    }))
}

async fn system_state(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<monitor::Snapshot>, ApiError> {
    let mut m = state.monitor.lock().await;
    Ok(Json(m.snapshot()))
}

#[derive(Deserialize)]
struct HistoryQuery {
    /// 返回最近多少个采样点，默认 120
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    120
}

async fn system_history(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<crate::db::MetricPoint>>, ApiError> {
    let limit = q.limit.clamp(1, 2000);
    Ok(Json(state.db.recent_metrics_async(limit).await?))
}

async fn processes_list(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<monitor::ProcessInfo>>, ApiError> {
    Ok(Json(rprocess::list(&state).await?))
}

#[derive(Deserialize)]
struct KillReq {
    pid: u32,
}

async fn processes_kill(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<KillReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 失败必须往外抛：早先的实现用 `.is_ok()` 取布尔后就丢弃了错误，
    // 导致杀进程失败（如权限不足）时前端仍显示成功。
    let ok = rprocess::kill(&state, req.pid)
        .await
        .map_err(ApiError::file_err)?;
    if !ok {
        return Err(ApiError::bad(format!(
            "结束进程 {} 失败（可能权限不足）",
            req.pid
        )));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn services_list(_user: AuthUser) -> Result<Json<Vec<opservice::ServiceInfo>>, ApiError> {
    Ok(Json(opservice::list().await?))
}

#[derive(Deserialize)]
struct ServiceActionReq {
    name: String,
    action: opservice::Action,
}

async fn services_action(
    _user: AuthUser,
    Json(req): Json<ServiceActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let out = opservice::action(&req.name, req.action)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct JournalQuery {
    unit: Option<String>,
    #[serde(default = "default_lines")]
    lines: u32,
}

fn default_lines() -> u32 {
    200
}

async fn logs_journal(
    _user: AuthUser,
    Query(q): Query<JournalQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let text = crate::logs::journal(q.unit.as_deref(), q.lines).await?;
    Ok(Json(serde_json::json!({ "text": text })))
}

async fn logs_files(_user: AuthUser) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(crate::logs::list_files().await?))
}

#[derive(Deserialize)]
struct TailQuery {
    path: String,
    #[serde(default = "default_lines")]
    lines: u32,
}

async fn logs_tail(
    _user: AuthUser,
    Query(q): Query<TailQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let text = crate::logs::tail_file(&q.path, q.lines).await?;
    Ok(Json(serde_json::json!({ "text": text })))
}

// ---------- 文件管理 ----------

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

async fn files_list(
    _user: AuthUser,
    Query(q): Query<PathQuery>,
) -> Result<Json<crate::files::DirListing>, ApiError> {
    crate::files::list_dir(&q.path)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

async fn files_read(
    _user: AuthUser,
    Query(q): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let text = crate::files::read_file(&q.path).await.map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "content": text })))
}

#[derive(Deserialize)]
struct WriteReq {
    path: String,
    content: String,
}

async fn files_write(
    _user: AuthUser,
    Json(req): Json<WriteReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::write_file(&req.path, &req.content)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MkdirReq {
    path: String,
}

async fn files_mkdir(
    _user: AuthUser,
    Json(req): Json<MkdirReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::mkdir(&req.path)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn files_delete(
    _user: AuthUser,
    Json(req): Json<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::remove(&req.path)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RenameReq {
    from: String,
    to: String,
}

async fn files_rename(
    _user: AuthUser,
    Json(req): Json<RenameReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::rename(&req.from, &req.to)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn files_download(
    _user: AuthUser,
    Query(q): Query<PathQuery>,
) -> Result<Response, ApiError> {
    let (name, bytes) = crate::files::download(&q.path).await.map_err(ApiError::file_err)?;
    let ct = mime_guess::from_path(&name)
        .first_or_octet_stream()
        .to_string();
    // 文件名可能含非 ASCII，使用 RFC 5987 编码
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric()
                || matches!(b, b'.' | b'-' | b'_' | b'~')
            {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let disposition = format!("attachment; filename*=UTF-8''{encoded}");
    let mut resp = (
        [(header::CONTENT_TYPE, ct)],
        bytes,
    )
        .into_response();
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        resp.headers_mut()
            .insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// 上传：multipart 表单，字段 dir（目标目录）+ file（文件）
async fn files_upload(
    _user: AuthUser,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut dir = String::new();
    let mut fname = String::new();
    let mut fbytes: Vec<u8> = Vec::new();
    while let Some(field) = mp.next_field().await.map_err(ApiError::file_err)? {
        match field.name().unwrap_or("") {
            "dir" => dir = field.text().await.map_err(ApiError::file_err)?,
            "file" => {
                fname = field.file_name().unwrap_or("upload.bin").to_string();
                fbytes = field.bytes().await.map_err(ApiError::file_err)?.to_vec();
            }
            _ => {}
        }
    }
    if dir.is_empty() || fbytes.is_empty() {
        return Err(ApiError::file_err("缺少 dir 或 file 字段"));
    }
    let size = fbytes.len();
    let saved = crate::files::save_upload(&dir, &fname, fbytes)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "path": saved, "size": size })))
}

// ---------- 软件包管理 ----------

#[derive(Deserialize)]
struct PkgListQuery {
    filter: Option<String>,
    #[serde(default = "default_pkg_limit")]
    limit: usize,
}

fn default_pkg_limit() -> usize {
    500
}

async fn packages_list(
    _user: AuthUser,
    Query(q): Query<PkgListQuery>,
) -> Result<Json<Vec<crate::packages::PackageInfo>>, ApiError> {
    let limit = q.limit.clamp(1, 5000);
    crate::packages::list_installed(q.filter.as_deref(), limit)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

async fn packages_upgradable(
    _user: AuthUser,
) -> Result<Json<Vec<crate::packages::PackageInfo>>, ApiError> {
    crate::packages::upgradable().await.map(Json).map_err(ApiError::file_err)
}

async fn packages_search(
    _user: AuthUser,
    Query(q): Query<PkgListQuery>,
) -> Result<Json<Vec<crate::packages::PackageInfo>>, ApiError> {
    let kw = q.filter.clone().unwrap_or_default();
    if kw.is_empty() {
        return Err(ApiError::bad("请输入搜索关键字"));
    }
    let limit = q.limit.clamp(1, 2000);
    crate::packages::search(&kw, limit)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct PkgActionReq {
    action: String,
    names: Vec<String>,
}

async fn packages_action(
    _user: AuthUser,
    Json(req): Json<PkgActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let output = match req.action.as_str() {
        "update" => crate::packages::update_index().await,
        "install" => crate::packages::install(&req.names).await,
        "upgrade" => crate::packages::upgrade(&req.names).await,
        "remove" => crate::packages::remove(&req.names).await,
        _ => Err(anyhow::anyhow!("未知操作")),
    }
    .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "output": output })))
}

// ---------- 计划任务 ----------

async fn cron_list(_user: AuthUser) -> Result<Json<Vec<crate::crontab::CronEntry>>, ApiError> {
    crate::crontab::list().await.map(Json).map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct CronReq {
    index: Option<usize>,
    entry: crate::crontab::CronEntry,
}

async fn cron_add(
    _user: AuthUser,
    Json(req): Json<CronReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::crontab::add(&req.entry)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn cron_update(
    _: State<AppState>,
    _user: AuthUser,
    Json(req): Json<CronReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let index = req.index.ok_or_else(|| ApiError::bad("缺少 index"))?;
    crate::crontab::update(index, &req.entry)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CronDeleteReq {
    index: usize,
}

async fn cron_delete(
    _user: AuthUser,
    Json(req): Json<CronDeleteReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::crontab::delete(req.index)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- 网络查看 ----------

async fn net_interfaces(
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::network::interfaces().await.map(Json).map_err(ApiError::file_err)
}

async fn net_routes(_user: AuthUser) -> Result<Json<serde_json::Value>, ApiError> {
    crate::network::routes().await.map(Json).map_err(ApiError::file_err)
}

async fn net_connections(
    _user: AuthUser,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    crate::network::connections().await.map(Json).map_err(ApiError::file_err)
}

async fn net_dns(_user: AuthUser) -> Result<Json<Vec<String>>, ApiError> {
    crate::network::dns().await.map(Json).map_err(ApiError::file_err)
}

// ---------- Docker 控制 ----------

/// Docker 环境状态：未安装或守护进程未启动时也返回 200，由前端决定展示方式
async fn docker_status(_user: AuthUser) -> Result<Json<crate::docker::DockerStatus>, ApiError> {
    crate::docker::status().await.map(Json).map_err(ApiError::file_err)
}

/// 一键安装 Docker（耗时较长，前端应给出等待提示）
async fn docker_install(_user: AuthUser) -> Result<Json<serde_json::Value>, ApiError> {
    let output = crate::docker::install().await.map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "output": output })))
}

async fn docker_containers(
    _user: AuthUser,
) -> Result<Json<Vec<crate::docker::ContainerInfo>>, ApiError> {
    crate::docker::containers()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct ContainerActionReq {
    id: String,
    action: String,
}

async fn docker_container_action(
    _user: AuthUser,
    Json(req): Json<ContainerActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let act = parse_docker_action(&req.action)?;
    let output = crate::docker::container_action(&req.id, act)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "output": output })))
}

#[derive(Deserialize)]
struct DockerLogsQuery {
    id: String,
    #[serde(default = "default_log_tail")]
    tail: usize,
}

fn default_log_tail() -> usize {
    200
}

async fn docker_logs(
    _user: AuthUser,
    Query(q): Query<DockerLogsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let logs = crate::docker::logs(&q.id, q.tail)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "logs": logs })))
}

async fn docker_images(_user: AuthUser) -> Result<Json<Vec<crate::docker::ImageInfo>>, ApiError> {
    crate::docker::images().await.map(Json).map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct ImageActionReq {
    action: String,
    /// pull 时为镜像名，remove 时为镜像 ID
    target: String,
}

async fn docker_image_action(
    _user: AuthUser,
    Json(req): Json<ImageActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let output = match req.action.as_str() {
        "pull" => crate::docker::pull(&req.target).await,
        "remove" => crate::docker::remove_image(&req.target).await,
        _ => return Err(ApiError::bad("未知的镜像操作")),
    }
    .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "output": output })))
}

async fn docker_compose(
    _user: AuthUser,
) -> Result<Json<Vec<crate::docker::ComposeProject>>, ApiError> {
    crate::docker::compose_projects()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct ComposeActionReq {
    name: String,
    action: String,
}

async fn docker_compose_action(
    _user: AuthUser,
    Json(req): Json<ComposeActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let act = parse_docker_action(&req.action)?;
    let output = crate::docker::compose_action(&req.name, act)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "output": output })))
}

/// 把前端传来的动作字符串转成枚举，错误提示统一为中文
fn parse_docker_action(s: &str) -> Result<crate::docker::Action, ApiError> {
    serde_json::from_value::<crate::docker::Action>(serde_json::Value::String(s.to_string()))
        .map_err(|_| ApiError::bad(format!("未知操作：{s}")))
}

// ---------- AI 助手（暂时停用）----------
//
// 用户决定：当前版本不需要 AI 助手功能，先注释掉。恢复时把下面整段
// 以及路由注册中的 /ai/* 去掉块注释、并恢复文件顶部 sse 相关导入即可。
/*
/// 读取 AI 配置（API Key 只回是否已填）
async fn ai_config_get(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<crate::ai::AiConfigView>, ApiError> {
    let cfg = crate::ai::load_config(&state.db)
        .await
        .map_err(ApiError::file_err)?;
    let embed_model = state
        .db
        .get_setting_async(crate::aimemory::KEY_EMBED_MODEL)
        .await
        .map_err(ApiError::file_err)?
        .unwrap_or_default();
    let auto_remember =
        crate::aimemory::bool_setting(&state.db, crate::aimemory::KEY_AUTO_REMEMBER, true).await;
    let tools_enabled =
        crate::aimemory::bool_setting(&state.db, crate::aimemory::KEY_TOOLS_ENABLED, true).await;
    let tts_url = state
        .db
        .get_setting_async(crate::aimemory::KEY_TTS_URL)
        .await
        .map_err(ApiError::file_err)?
        .unwrap_or_default();
    let tts_auto =
        crate::aimemory::bool_setting(&state.db, crate::aimemory::KEY_TTS_AUTO, false).await;
    Ok(Json(crate::ai::AiConfigView {
        base_url: cfg.base_url,
        model: cfg.model,
        has_key: !cfg.api_key.is_empty(),
        timeout_secs: cfg.timeout_secs,
        embed_model,
        auto_remember,
        tools_enabled,
        tts_url,
        tts_auto,
    }))
}

#[derive(Deserialize)]
struct AiConfigReq {
    base_url: String,
    model: String,
    /// 留空表示不改动已保存的 Key
    #[serde(default)]
    api_key: String,
    /// 等待模型响应的上限（秒），0 表示用默认值
    #[serde(default)]
    timeout_secs: u64,
    /// 算记忆向量的模型（Ollama 需先 pull 一个，如 nomic-embed-text）
    #[serde(default)]
    embed_model: String,
    /// 是否每轮自动记下值得记的事
    #[serde(default = "default_true")]
    auto_remember: bool,
    /// 是否允许助手调用工具
    #[serde(default = "default_true")]
    tools_enabled: bool,
    /// 语音合成服务地址（留空表示不启用朗读）
    #[serde(default)]
    tts_url: String,
    /// 是否每轮自动朗读回复
    #[serde(default)]
    tts_auto: bool,
}

fn default_true() -> bool {
    true
}

async fn ai_config_set(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<AiConfigReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::ai::save_config(
        &state.db,
        crate::ai::AiConfig {
            base_url: req.base_url,
            api_key: req.api_key,
            model: req.model,
            timeout_secs: req.timeout_secs,
        },
    )
    .await
    .map_err(ApiError::file_err)?;
    state
        .db
        .set_setting_async(crate::aimemory::KEY_EMBED_MODEL, req.embed_model.trim())
        .await
        .map_err(ApiError::file_err)?;
    crate::aimemory::set_bool_setting(&state.db, crate::aimemory::KEY_AUTO_REMEMBER, req.auto_remember)
        .await
        .map_err(ApiError::file_err)?;
    crate::aimemory::set_bool_setting(&state.db, crate::aimemory::KEY_TOOLS_ENABLED, req.tools_enabled)
        .await
        .map_err(ApiError::file_err)?;
    state
        .db
        .set_setting_async(crate::aimemory::KEY_TTS_URL, req.tts_url.trim())
        .await
        .map_err(ApiError::file_err)?;
    crate::aimemory::set_bool_setting(&state.db, crate::aimemory::KEY_TTS_AUTO, req.tts_auto)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AiModelsQuery {
    /// 允许用还没保存的地址先试拉一次，不填则用已保存的配置
    base_url: Option<String>,
}

// ---------- AI 记忆 ----------

/// 记忆列表
async fn ai_memory_list(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<crate::db::AiMemory>>, ApiError> {
    state
        .db
        .ai_memory_list_async(500)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct MemoryAddReq {
    content: String,
}

/// 手动记一条
async fn ai_memory_add(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<MemoryAddReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let cfg = crate::ai::load_config(&state.db)
        .await
        .map_err(ApiError::file_err)?;
    let model = embed_model(&state).await;
    let id = crate::aimemory::remember(&state.db, &cfg, &model, &req.content)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct MemoryDeleteReq {
    /// 给 id 就按 id 删，给 text 就删除包含该文字的记忆
    id: Option<i64>,
    text: Option<String>,
}

async fn ai_memory_delete(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<MemoryDeleteReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let n = match (req.id, req.text) {
        (Some(id), _) => state
            .db
            .ai_memory_delete_async(id)
            .await
            .map_err(ApiError::file_err)?,
        (None, Some(text)) if !text.trim().is_empty() => state
            .db
            .ai_memory_forget_async(text.trim().to_string())
            .await
            .map_err(ApiError::file_err)?,
        _ => return Err(ApiError::bad("请提供要删除的记忆 id 或包含的文字")),
    };
    Ok(Json(serde_json::json!({ "ok": true, "removed": n })))
}

// ---------- AI 语音 ----------

#[derive(Deserialize)]
struct SpeechReq {
    text: String,
}

/// 把一段文本合成成语音，直接把 wav 字节回给浏览器。
///
/// 走面板中转而不是让浏览器直连语音服务：地址只配一处，也就不必给
/// 语音服务再单独开 CORS。
async fn ai_speech(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<SpeechReq>,
) -> Result<Response, ApiError> {
    let url = state
        .db
        .get_setting_async(crate::aimemory::KEY_TTS_URL)
        .await
        .map_err(ApiError::file_err)?
        .unwrap_or_default();
    let text = req.text;
    let wav = tokio::task::spawn_blocking(move || crate::ai::synthesize(&url, &text))
        .await
        .map_err(|e| ApiError::bad(format!("语音合成任务异常：{e}")))?
        .map_err(ApiError::file_err)?;
    Response::builder()
        .header(header::CONTENT_TYPE, "audio/wav")
        // 内容是即合成即播的，不该被缓存
        .header(header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(wav))
        .map_err(|e| ApiError::bad(format!("构造响应失败：{e}")))
}

/// 语音服务的可用性（页面上的状态提示用）
async fn ai_speech_status(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let url = state
        .db
        .get_setting_async(crate::aimemory::KEY_TTS_URL)
        .await
        .map_err(ApiError::file_err)?
        .unwrap_or_default();
    if url.trim().is_empty() {
        return Ok(Json(serde_json::json!({ "configured": false })));
    }
    let probe_url = format!("{}/health", url.trim().trim_end_matches('/'));
    let ok = tokio::task::spawn_blocking(move || {
        let agent = crate::ai::build_agent(10);
        agent.get(&probe_url).call().is_ok()
    })
    .await
    .unwrap_or(false);
    Ok(Json(serde_json::json!({ "configured": true, "alive": ok })))
}

// ---------- AI 会话存档 ----------

/// 会话元信息列表（不含正文）
async fn ai_session_list(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<crate::db::AiSessionMeta>>, ApiError> {
    state
        .db
        .ai_session_list_async(200)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct AiSessionIdReq {
    id: i64,
}

async fn ai_session_get(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<AiSessionIdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match state
        .db
        .ai_session_get_async(q.id)
        .await
        .map_err(ApiError::file_err)?
    {
        Some((title, messages)) => {
            // messages 本就是 JSON 文本，直接嵌回响应，避免多一层字符串转义
            let parsed: serde_json::Value =
                serde_json::from_str(&messages).unwrap_or_else(|_| serde_json::json!([]));
            Ok(Json(
                serde_json::json!({ "id": q.id, "title": title, "messages": parsed }),
            ))
        }
        None => Err(ApiError::bad("会话不存在")),
    }
}

#[derive(Deserialize)]
struct AiSessionSaveReq {
    /// 不给表示新建
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    title: String,
    messages: serde_json::Value,
}

/// 保存会话。前端每轮对话结束后整份覆盖 —— 消息量很小，没必要做增量。
async fn ai_session_save(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<AiSessionSaveReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let messages = req.messages.to_string();
    if messages.len() > 4 * 1024 * 1024 {
        return Err(ApiError::bad("会话过长，无法保存"));
    }
    let msg_count = req
        .messages
        .as_array()
        .map(|a| a.len() as i64)
        .unwrap_or(0);
    // 标题取用户第一句话，空会话给个占位
    let title = if req.title.trim().is_empty() {
        "新对话".to_string()
    } else {
        req.title.trim().chars().take(40).collect()
    };
    let id = state
        .db
        .ai_session_save_async(req.id, title, msg_count, messages)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn ai_session_delete(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<AiSessionIdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let n = state
        .db
        .ai_session_delete_async(req.id)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "removed": n })))
}

/// 当前配置的向量模型名
async fn embed_model(state: &AppState) -> String {
    state
        .db
        .get_setting_async(crate::aimemory::KEY_EMBED_MODEL)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 把情绪判定与召回的记忆拼成一段注入系统提示词的内容
async fn context_block(
    state: &AppState,
    cfg: &crate::ai::AiConfig,
    messages: &[crate::ai::Message],
) -> (Option<String>, Option<crate::emotion::Emotion>) {
    let user_text = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let emotion = detect_emotion(state, messages).await;
    let memories = crate::aimemory::recall(state.db.as_ref(), cfg, &embed_model(state).await, &user_text).await;

    let mut parts: Vec<String> = Vec::new();
    if let Some(e) = &emotion {
        parts.push(crate::ai::emotion_block(e));
    }
    let mem = crate::aimemory::memory_block(&memories);
    if !mem.is_empty() {
        parts.push(mem);
    }
    let block = if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    };
    (block, emotion)
}

/// 拉取可选模型列表，供页面上的下拉框使用
async fn ai_models(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<AiModelsQuery>,
) -> Result<Json<Vec<String>>, ApiError> {
    let mut cfg = crate::ai::load_config(&state.db)
        .await
        .map_err(ApiError::file_err)?;
    if let Some(b) = q.base_url.as_deref().map(str::trim) {
        if !b.is_empty() {
            cfg.base_url = b.trim_end_matches('/').to_string();
        }
    }
    crate::ai::list_models(cfg)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct AiChatReq {
    messages: Vec<crate::ai::Message>,
}

// ---------- AI 情绪 ----------

/// 情绪模型的就绪状态：模型文件是否齐全、ONNX Runtime 是否加载成功
async fn ai_emotion_status(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ready = crate::emotion::ready(&state.data_dir);
    // 真正试一次初始化：文件齐了不代表运行库能加载，这里把两类问题分开报
    let probe = if ready {
        crate::emotion::classify(&state.data_dir, "你好")
            .map(|_| String::new())
            .unwrap_or_else(|e| format!("{e}"))
    } else {
        String::new()
    };
    Ok(Json(serde_json::json!({
        "ready": ready,
        "usable": ready && probe.is_empty(),
        "error": probe,
    })))
}

#[derive(Deserialize)]
struct EmotionReq {
    text: String,
}

/// 直接判定一段文本的情绪（页面上可以拿它试效果）
async fn ai_emotion_classify(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<EmotionReq>,
) -> Result<Json<crate::emotion::Emotion>, ApiError> {
    let dir = state.data_dir.to_string();
    let text = req.text;
    tokio::task::spawn_blocking(move || crate::emotion::classify(&dir, &text))
        .await
        .map_err(|e| ApiError::bad(format!("情绪识别任务异常：{e}")))?
        .map(Json)
        .map_err(ApiError::file_err)
}

/// 对话前先判定用户这句话的情绪；模型不可用就返回 None（不阻断对话）
async fn detect_emotion(state: &AppState, messages: &[crate::ai::Message]) -> Option<crate::emotion::Emotion> {
    let text = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())?;
    if !crate::emotion::ready(&state.data_dir) {
        return None;
    }
    let dir = state.data_dir.to_string();
    match tokio::task::spawn_blocking(move || crate::emotion::classify(&dir, &text)).await {
        Ok(Ok(e)) => Some(e),
        Ok(Err(e)) => {
            tracing::warn!("情绪识别失败，本次按无情绪处理：{e}");
            None
        }
        Err(e) => {
            tracing::warn!("情绪识别任务异常：{e}");
            None
        }
    }
}

async fn ai_chat(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<AiChatReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let cfg = crate::ai::load_config(&state.db)
        .await
        .map_err(ApiError::file_err)?;
    let (block, _) = context_block(&state, &cfg, &req.messages).await;
    let reply = crate::ai::chat(cfg, req.messages, block)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "reply": reply })))
}

/// 流式对话：用 SSE 把模型的输出逐块推给前端，不必等整段生成完
async fn ai_chat_stream(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<AiChatReq>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, ApiError> {
    let cfg = crate::ai::load_config(&state.db)
        .await
        .map_err(ApiError::file_err)?;
    let (block, emotion) = context_block(&state, &cfg, &req.messages).await;
    // 这一轮说完之后要做的善后：自动记下值得长期记住的事
    let user_text = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();
    // 工具执行器：工具循环跑在阻塞线程里，面板的查询又是异步的，
    // 所以在这里捕获 runtime handle，由它负责把异步查询跑起来
    let runner: Option<crate::ai::ToolRunner> = if crate::aimemory::bool_setting(
        &state.db,
        crate::aimemory::KEY_TOOLS_ENABLED,
        true,
    )
    .await
    {
        let handle = tokio::runtime::Handle::current();
        Some(std::sync::Arc::new(
            move |name: &str, args: &serde_json::Value| {
                let name = name.to_string();
                let args = args.clone();
                handle.block_on(crate::aitools::run(&name, &args))
            },
        ))
    } else {
        None
    };
    let post = crate::ai::PostTurn {
        db: state.db.as_ref().clone(),
        cfg: cfg.clone(),
        embed_model: embed_model(&state).await,
        user_text,
    };
    let rx = crate::ai::chat_stream(cfg, req.messages, block, emotion, Some(post), runner)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Sse::new(ReceiverStream::new(rx)))
}
*/

/// 健康检查（无需认证）
async fn health() -> &'static str {
    "ok"
}

/// 组装路由：/api/* 下所有业务接口都需要认证，其余走前端 SPA
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/system/state", get(system_state))
        .route("/system/history", get(system_history))
        .route("/processes", get(processes_list).post(processes_kill))
        .route("/services", get(services_list).post(services_action))
        .route("/logs/journal", get(logs_journal))
        .route("/logs/files", get(logs_files))
        .route("/logs/tail", get(logs_tail))
        .route("/files/list", get(files_list))
        .route("/files/read", get(files_read))
        .route("/files/write", post(files_write))
        .route("/files/mkdir", post(files_mkdir))
        .route("/files/delete", post(files_delete))
        .route("/files/rename", post(files_rename))
        .route("/files/download", get(files_download))
        .route(
            "/files/upload",
            // 上传限制 100MB
            post(files_upload).layer(DefaultBodyLimit::max(100 * 1024 * 1024)),
        )
        .route("/packages", get(packages_list))
        .route("/packages/upgradable", get(packages_upgradable))
        .route("/packages/search", get(packages_search))
        .route("/packages/action", post(packages_action))
        .route("/cron", get(cron_list).post(cron_add).put(cron_update).delete(cron_delete))
        .route("/network/interfaces", get(net_interfaces))
        .route("/network/routes", get(net_routes))
        .route("/network/connections", get(net_connections))
        .route("/network/dns", get(net_dns))
        .route("/docker/status", get(docker_status))
        .route("/docker/install", post(docker_install))
        .route("/docker/containers", get(docker_containers))
        .route("/docker/container/action", post(docker_container_action))
        .route("/docker/logs", get(docker_logs))
        .route("/docker/images", get(docker_images))
        .route("/docker/image/action", post(docker_image_action))
        .route("/docker/compose", get(docker_compose))
        .route("/docker/compose/action", post(docker_compose_action));
        // AI 助手路由暂时停用（与文件头部 AI 处理函数段一起注释，恢复时去掉本注释并补回链式调用）
        /*
        .route("/ai/config", get(ai_config_get).post(ai_config_set))
        .route("/ai/models", get(ai_models))
        .route("/ai/emotion/status", get(ai_emotion_status))
        .route("/ai/emotion", post(ai_emotion_classify))
        .route("/ai/speech", post(ai_speech))
        .route("/ai/speech/status", get(ai_speech_status))
        .route("/ai/memory", get(ai_memory_list).post(ai_memory_add))
        .route("/ai/memory/delete", post(ai_memory_delete))
        .route(
            "/ai/session",
            get(ai_session_list).post(ai_session_save),
        )
        .route("/ai/session/get", get(ai_session_get))
        .route("/ai/session/delete", post(ai_session_delete))
        .route("/ai/chat", post(ai_chat))
        .route("/ai/chat/stream", post(ai_chat_stream));
        */

    Router::new()
        .route("/health", get(health))
        .route("/api/login", post(login))
        .nest("/api", protected)
        .fallback(crate::embed::handler)
        .with_state(state)
}
