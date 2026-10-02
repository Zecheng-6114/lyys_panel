use axum::extract::{
    ConnectInfo, DefaultBodyLimit, FromRequest, FromRequestParts, Multipart, OriginalUri, Path,
    Query, Request, State,
};
// 4.3 容器日志流：WebSocket 升级提取器
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{header, request::Parts, StatusCode};
use axum::middleware::{self, Next};
// AI 助手功能暂时停用（见文件末尾 "AI 助手已停用" 说明），以下导入仅 AI 段使用
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
// use std::convert::Infallible;
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
    /// 403：已认证但角色权限不足（2.1 RBAC）
    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: msg.into(),
        }
    }
    /// 将业务层 anyhow 错误转为 400。
    ///
    /// P1-3 约定：错误链的最外层消息面向用户（各业务模块已保证不含
    /// 路径/errno/命令 stderr），完整错误链（含底层细节）写入 tracing
    /// 日志，方便服务端排查。
    fn file_err(e: anyhow::Error) -> Self {
        tracing::warn!("请求处理失败：{e:#}");
        Self {
            status: StatusCode::BAD_REQUEST,
            message: e.to_string(),
        }
    }

    /// 内部错误（P1-3）：响应体统一通用文案，细节只进 tracing 日志
    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "内部错误，请稍后再试".into(),
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
    /// P1-3 错误脱敏：500 响应统一通用文案；完整错误链（可能含路径、errno、
    /// 命令输出等内部细节）只写入 tracing 日志，不回显给客户端。
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("内部错误：{e:#}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "内部错误，请稍后再试".to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

/// 认证后的当前用户（从 Authorization: Bearer <token> 解析）
///
/// token 校验通过后按 sub 查库补全用户名/角色（2.1 RBAC）：角色以库为准，
/// 改角色即时生效，无需等 token 过期。查不到用户（已被删除）视为登录失效。
/// `jti`/`exp` 供登出吊销当前 token（P1-1）；`must_change` 供前端强制改密。
pub struct AuthUser {
    /// 用户 id
    pub id: i64,
    /// 用户名（审计与会话列表展示用）
    pub username: String,
    /// 角色：admin / operator / viewer
    pub role: String,
    /// 首登强制改密标记
    pub must_change: bool,
    /// 本枚 token 的唯一 id（登出时加入服务端吊销名单）
    pub jti: String,
    /// 本枚 token 的过期时间（Unix 秒，吊销有效期到点为止）
    pub exp: usize,
}

impl AuthUser {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::unauthorized("缺少认证信息"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::unauthorized("认证头格式错误"))?;
        let claims = auth::verify_token(&state.jwt_secret, token)
            .map_err(|_| ApiError::unauthorized("登录已过期，请重新登录"))?;
        // P1-1：已登出（吊销）的 token 一律拒绝
        if state.revocations.is_revoked(&claims.jti).await {
            return Err(ApiError::unauthorized("登录已失效，请重新登录"));
        }
        // 2.4：被「踢出」的会话（jti 已从会话表删除且非本人当前登录）同样拒绝。
        // 会话表只登记有效会话，登出/踢出/改密都会移除对应行。
        if !state.db.session_exists_async(&claims.jti).await? {
            return Err(ApiError::unauthorized("登录已失效，请重新登录"));
        }
        let user = state
            .db
            .user_by_id_async(claims.sub)
            .await?
            .ok_or_else(|| ApiError::unauthorized("账号已被删除，请重新登录"))?;
        let must_change = user.2;
        // 2.2：首登强制改密闸门——未改密前除改密/登出/查询自身信息外一律 403。
        // nest 会剥掉 /api 前缀，必须用 OriginalUri 拿完整路径判断。
        if must_change {
            let path = parts
                .extensions
                .get::<OriginalUri>()
                .map(|u| u.0.path())
                .unwrap_or_else(|| parts.uri.path());
            const ALLOWED: [&str; 3] = ["/api/me", "/api/account/password", "/api/logout"];
            if !ALLOWED.contains(&path) {
                return Err(ApiError::forbidden("请先修改初始密码"));
            }
        }
        Ok(AuthUser {
            id: claims.sub,
            username: user.0,
            role: user.1,
            must_change,
            jti: claims.jti,
            exp: claims.exp,
        })
    }
}

/// 角色守卫提取器：`RequireRole<1>`（operator 及以上）或 `RequireRole<2>`（admin）
/// 放在 handler 参数里即可，角色不足返回 403；内部包裹 [`AuthUser`]，
/// handler 需要操作者身份时直接解构取出，避免重复走一遍鉴权链。
/// viewer 只读（仅 GET 路由）、operator 可执行业务写操作、
/// admin 全权（账号/审计/会话管理）。
pub struct RequireRole<const MIN: u8>(pub AuthUser);

/// 角色等级：viewer=0 < operator=1 < admin=2
fn role_level(role: &str) -> u8 {
    match role {
        "admin" => 2,
        "operator" => 1,
        _ => 0,
    }
}

impl<const MIN: u8> FromRequestParts<AppState> for RequireRole<MIN> {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if role_level(&user.role) < MIN {
            return Err(ApiError::forbidden("权限不足"));
        }
        Ok(RequireRole(user))
    }
}

/// 是否信任反向代理传入的 `X-Forwarded-For`（P2-3，用 `PANEL_TRUST_PROXY=1` 开启）。
///
/// **默认必须关闭**。该头部由客户端自行携带，只有在面板确实位于可信反代之后、
/// 且反代对每个请求都覆盖（不是追加）它时才有意义。无条件信任等于让来源 IP
/// 变成攻击者可控的字段，反而削弱登录退避与审计。取值只在首次调用时读一次。
fn trust_proxy() -> bool {
    static TRUST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRUST.get_or_init(|| {
        matches!(
            std::env::var("PANEL_TRUST_PROXY").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        )
    })
}

/// 从请求扩展中取出来源 IP；取不到（如未启用连接信息）时退回环回地址。
///
/// 单独抽成函数，供 [`AuthUser`] 与 [`ClientIp`] 共用，保证两处口径一致。
/// 开启 `PANEL_TRUST_PROXY` 时改取 `X-Forwarded-For` 的最左值（原始客户端）；
/// 该值解析失败则退回连接地址，不会因为一个畸形头把来源判成未知。
fn client_ip(parts: &Parts) -> IpAddr {
    if trust_proxy() {
        if let Some(ip) = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse::<IpAddr>().ok())
        {
            return ip;
        }
    }
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

/// 登录响应体。
/// `role`/`must_change` 供前端做菜单裁剪与首登强制改密（2.1/2.2）。
#[derive(Serialize)]
struct LoginResp {
    token: String,
    username: String,
    role: String,
    must_change: bool,
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

/// 统一 JSON 请求体提取器（P1-3）。
///
/// axum 内置 `Json` 的拒绝响应会把 serde 细节（类型名、枚举取值、行列号）
/// 原样回显给客户端；这里包一层，保留原状态码（400/415/422 均为 4xx，
/// 语义正确），但响应体统一为通用文案，细节只进 tracing 日志。
struct SafeJson<T>(T);

impl<S, T> FromRequest<S> for SafeJson<T>
where
    T: Send,
    Json<T>: FromRequest<S>,
    <Json<T> as FromRequest<S>>::Rejection: IntoResponse + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(SafeJson(value)),
            Err(rej) => {
                let resp = rej.into_response();
                let (parts, body) = resp.into_parts();
                let bytes = axum::body::to_bytes(body, 4096).await.unwrap_or_default();
                let detail = String::from_utf8_lossy(&bytes);
                tracing::warn!("请求体解析失败（{}）：{}", parts.status, detail.trim());
                Err(ApiError {
                    status: parts.status,
                    message: "请求体格式错误".into(),
                })
            }
        }
    }
}

async fn login(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ClientIp(ip): ClientIp,
    SafeJson(req): SafeJson<LoginReq>,
) -> Result<Json<LoginResp>, ApiError> {
    // 登录由 handler 自行写审计（中间件拿不到 body 里的用户名），
    // 审计写失败只告警，不阻断登录流程
    let audit_login =
        |state: &AppState, uid: Option<i64>, name: &str, status: u16, ip: std::net::IpAddr| {
            let state = state.clone();
            let name = name.to_string();
            tokio::spawn(async move {
                let ts = time::OffsetDateTime::now_utc().unix_timestamp();
                if let Err(e) = state
                    .db
                    .audit_async(
                        ts,
                        uid,
                        &name,
                        "POST",
                        "/api/login",
                        status,
                        &ip.to_string(),
                        None,
                    )
                    .await
                {
                    tracing::warn!("写入登录审计失败：{e}");
                }
            });
        };

    let wait = state.throttle.retry_after(ip, &req.username).await;
    if !wait.is_zero() {
        let secs = wait.as_secs().max(1);
        return Err(ApiError::too_many_requests(format!(
            "登录尝试过于频繁，请 {secs} 秒后再试"
        )));
    }

    let user = state.db.find_user_full_async(&req.username).await?;
    let Some(row) = user else {
        let delay = state.throttle.record_failure(ip, &req.username).await;
        tracing::warn!(
            "登录失败（用户不存在）：user={} ip={} 退避={}s",
            req.username,
            ip,
            delay.as_secs()
        );
        audit_login(&state, None, &req.username, 401, ip);
        return Err(ApiError::unauthorized("用户名或密码错误"));
    };
    // argon2 校验是 CPU 密集操作（默认参数下约 100ms），必须离开异步工作线程，
    // 否则并发登录会把 tokio 的线程池占满。
    let password = req.password.clone();
    let hash_for_verify = row.password_hash.clone();
    let verified =
        tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash_for_verify))
            .await
            // 后台任务崩溃属服务端内部错误：细节进日志，客户端只收通用 500（P1-3）
            .map_err(|e| {
                tracing::error!("密码校验任务异常：{e}");
                ApiError::internal()
            })?;
    if !verified {
        let delay = state.throttle.record_failure(ip, &req.username).await;
        tracing::warn!(
            "登录失败（密码错误）：user={} ip={} 退避={}s",
            req.username,
            ip,
            delay.as_secs()
        );
        audit_login(&state, Some(row.id), &req.username, 401, ip);
        return Err(ApiError::unauthorized("用户名或密码错误"));
    }

    state.throttle.record_success(ip, &req.username).await;
    // P0-1：首次登录成功后删除初始密码文件（一次性文件方案）
    auth::cleanup_initial_password(&state.data_dir);

    // 2.4：签发 token 并登记会话。AuthUser 鉴权链强制校验 jti 在会话表中存在，
    // 因此这里必须写入 sessions，否则登录后所有请求都会被拒（401）。
    let (token, jti, exp) = auth::issue_token(&state.jwt_secret, row.id)?;
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let iat = time::OffsetDateTime::now_utc().unix_timestamp();
    state
        .db
        .session_add_async(&jti, row.id, &req.username, &ua, &ip.to_string(), iat, exp)
        .await?;

    tracing::info!("登录成功：user={} ip={}", req.username, ip);
    audit_login(&state, Some(row.id), &req.username, 200, ip);
    Ok(Json(LoginResp {
        token,
        username: req.username,
        role: row.role,
        must_change: row.must_change,
    }))
}

/// 登出（P1-1 + 2.4）：吊销当前 token 并移除会话登记，
/// 之后该 token 再请求任何受保护接口都会被拒（401）。
async fn logout(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.revocations.revoke(&user.jti, user.exp as i64).await;
    state.db.session_remove_async(&user.jti).await?;
    tracing::info!("登出（token 已吊销）：user_id={}", user.id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- 账号体系（2.1 / 2.2 / 2.3 / 2.4） ----------

/// 当前登录用户信息（前端刷新页面后恢复角色与强制改密状态用）
#[derive(Serialize)]
struct MeResp {
    id: i64,
    username: String,
    role: String,
    must_change: bool,
}

async fn me(user: AuthUser) -> Result<Json<MeResp>, ApiError> {
    Ok(Json(MeResp {
        id: user.id,
        username: user.username,
        role: user.role,
        must_change: user.must_change,
    }))
}

#[derive(Deserialize)]
struct ChangePwdReq {
    old_password: String,
    new_password: String,
}

/// 修改密码（任何角色）。校验旧密码后更新哈希，并踢掉该用户的其他会话，
/// 当前会话保留（避免改密把自己关在门外）。
async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    SafeJson(req): SafeJson<ChangePwdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.new_password.len() < 8 {
        return Err(ApiError::bad("新密码至少 8 位"));
    }
    let row = state
        .db
        .find_user_full_async(&user.username)
        .await?
        .ok_or_else(|| ApiError::unauthorized("账号已被删除，请重新登录"))?;
    let old = req.old_password.clone();
    let hash = row.password_hash.clone();
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&old, &hash))
        .await
        .map_err(|e| {
            tracing::error!("密码校验任务异常：{e}");
            ApiError::internal()
        })?;
    if !ok {
        return Err(ApiError::bad("旧密码错误"));
    }
    let new = req.new_password.clone();
    let (new_hash, new_salt) = tokio::task::spawn_blocking(move || auth::hash_password(&new))
        .await
        .map_err(|e| {
            tracing::error!("密码哈希任务异常：{e}");
            ApiError::internal()
        })?
        .map_err(|e| {
            tracing::error!("生成密码哈希失败：{e:#}");
            ApiError::internal()
        })?;
    state
        .db
        .set_password_async(user.id, &new_hash, &new_salt)
        .await?;
    // 踢掉该用户的其他会话；当前 jti 保留（改密不把自己关在门外）
    state
        .db
        .session_remove_user_except_async(user.id, &user.jti)
        .await?;
    tracing::info!("修改密码：user_id={}", user.id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 用户行（API 响应，不含密码字段）
#[derive(Serialize)]
struct UserResp {
    id: i64,
    username: String,
    role: String,
    must_change: bool,
}

fn valid_role(role: &str) -> bool {
    matches!(role, "admin" | "operator" | "viewer")
}

fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

async fn users_list(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<Vec<UserResp>>, ApiError> {
    let rows = state.db.list_users_async().await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, username, role, must_change)| UserResp {
                id,
                username,
                role,
                must_change,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct CreateUserReq {
    username: String,
    password: String,
    role: String,
}

/// 创建用户（admin）。新用户默认 must_change=1，首次登录强制改密（2.2）。
async fn users_create(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<CreateUserReq>,
) -> Result<Json<UserResp>, ApiError> {
    if !valid_username(&req.username) {
        return Err(ApiError::bad(
            "用户名仅允许字母、数字、下划线、连字符，长度 1-32",
        ));
    }
    if req.password.len() < 8 {
        return Err(ApiError::bad("密码至少 8 位"));
    }
    if !valid_role(&req.role) {
        return Err(ApiError::bad("非法角色"));
    }
    if state.db.find_user_async(&req.username).await?.is_some() {
        return Err(ApiError::bad("用户名已存在"));
    }
    let pwd = req.password.clone();
    let (hash, salt) = tokio::task::spawn_blocking(move || auth::hash_password(&pwd))
        .await
        .map_err(|e| {
            tracing::error!("密码哈希任务异常：{e}");
            ApiError::internal()
        })?
        .map_err(|e| {
            tracing::error!("生成密码哈希失败：{e:#}");
            ApiError::internal()
        })?;
    let id = state
        .db
        .create_user_role_async(&req.username, &hash, &salt, &req.role, true)
        .await?;
    tracing::info!(
        "创建用户：id={} user={} role={}",
        id,
        req.username,
        req.role
    );
    Ok(Json(UserResp {
        id,
        username: req.username,
        role: req.role,
        must_change: true,
    }))
}

#[derive(Deserialize)]
struct UpdateUserReq {
    username: String,
    role: String,
    /// 管理员重设该用户密码（可选；不传则不改密码）
    new_password: Option<String>,
}

/// 修改用户（admin）：改名/改角色/重设密码。
/// 重设密码或改角色都会踢掉该用户的全部会话，让变更立即生效。
async fn users_update(
    State(state): State<AppState>,
    RequireRole(actor): RequireRole<2>,
    Path(id): Path<i64>,
    SafeJson(req): SafeJson<UpdateUserReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.db.user_exists_async(id).await? {
        return Err(ApiError::bad("用户不存在"));
    }
    if !valid_username(&req.username) {
        return Err(ApiError::bad(
            "用户名仅允许字母、数字、下划线、连字符，长度 1-32",
        ));
    }
    if !valid_role(&req.role) {
        return Err(ApiError::bad("非法角色"));
    }
    // 改名撞车检查（排除自己）
    if let Some(existing) = state.db.find_user_async(&req.username).await? {
        if existing.0 != id {
            return Err(ApiError::bad("用户名已存在"));
        }
    }
    // 自保护：不允许把自己降级或改走，防止把最后一个 admin 关在门外
    if id == actor.id && (req.role != "admin" || req.username != actor.username) {
        return Err(ApiError::bad("不能修改自己的用户名或降级自己"));
    }
    state
        .db
        .update_user_async(id, &req.username, &req.role)
        .await?;
    if let Some(pwd) = &req.new_password {
        if pwd.len() < 8 {
            return Err(ApiError::bad("新密码至少 8 位"));
        }
        let pwd = pwd.clone();
        let (hash, salt) = tokio::task::spawn_blocking(move || auth::hash_password(&pwd))
            .await
            .map_err(|e| {
                tracing::error!("密码哈希任务异常：{e}");
                ApiError::internal()
            })?
            .map_err(|e| {
                tracing::error!("生成密码哈希失败：{e:#}");
                ApiError::internal()
            })?;
        state.db.set_password_async(id, &hash, &salt).await?;
    }
    // 角色/密码变更立即生效：踢掉该用户全部会话（含操作者本人改自己的场景，
    // 但上面已禁止自己降级，因此只有改密会踢自己——重登即可）
    let removed = state.db.session_remove_user_async(id).await?;
    tracing::info!(
        "更新用户：id={} user={} role={} 踢除会话={}",
        id,
        req.username,
        req.role,
        removed
    );
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 删除用户（admin）。不允许删除自己。
async fn users_delete(
    State(state): State<AppState>,
    RequireRole(actor): RequireRole<2>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if id == actor.id {
        return Err(ApiError::bad("不能删除自己"));
    }
    if !state.db.user_exists_async(id).await? {
        return Err(ApiError::bad("用户不存在"));
    }
    state.db.delete_user_async(id).await?;
    state.db.session_remove_user_async(id).await?;
    tracing::info!("删除用户：id={}", id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AuditQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

/// 审计日志查询（admin，2.3）
async fn audit_query(
    State(state): State<AppState>,
    _: RequireRole<2>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<crate::db::AuditRow>>, ApiError> {
    let limit = q.limit.clamp(1, 500);
    let offset = q.offset.max(0);
    Ok(Json(state.db.audit_list_async(limit, offset).await?))
}

/// 会话行（API 响应）：jti 只回传前 8 位，防止完整 jti 泄露被用于构造吊销请求
#[derive(Serialize)]
struct SessionResp {
    jti_prefix: String,
    user_id: i64,
    username: String,
    ua: String,
    ip: String,
    iat: i64,
    exp: i64,
    /// 是否为当前请求所在的会话
    current: bool,
}

/// 在线会话列表（2.4）：admin 看全部，其他角色只看自己的
async fn sessions_list(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<SessionResp>>, ApiError> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let rows = state.db.session_list_async(now).await?;
    let visible: Vec<SessionResp> = rows
        .into_iter()
        .filter(|r| user.is_admin() || r.user_id == user.id)
        .map(|r| SessionResp {
            jti_prefix: r.jti.chars().take(8).collect(),
            user_id: r.user_id,
            username: r.username,
            ua: r.ua,
            ip: r.ip,
            iat: r.iat,
            exp: r.exp,
            current: r.jti == user.jti,
        })
        .collect();
    Ok(Json(visible))
}

#[derive(Deserialize)]
struct KickReq {
    /// 要踢除会话的目标用户 id
    user_id: i64,
}

/// 踢出某用户的全部在线会话（admin，2.4）。
///
/// 删除 sessions 表行即可让该用户所有 token 立即失效：AuthUser 鉴权链
/// 会因 session_exists 失败而返回 401，无需再动内存吊销名单。
/// 会话列表只回传 jti 前缀（防泄露），因此按 user_id 整户踢除，
/// 不提供按单枚 jti 精确踢出。
async fn sessions_kick(
    State(state): State<AppState>,
    RequireRole(actor): RequireRole<2>,
    SafeJson(req): SafeJson<KickReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let n = state.db.session_remove_user_async(req.user_id).await?;
    tracing::info!(
        "踢出用户全部会话：actor={} target={} 数量={}",
        actor.id,
        req.user_id,
        n
    );
    Ok(Json(serde_json::json!({ "ok": true, "removed": n })))
}

async fn system_state(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<monitor::Snapshot>, ApiError> {
    let mut m = state.monitor.lock().await;
    // 读采样器留下的那一份，而不是现场重算 —— 重算会重置速率差值基准，
    // 使网络与磁盘 I/O 恒接近 0（详见 Monitor::latest 的说明）
    Ok(Json(m.latest()))
}

#[derive(Deserialize)]
struct HistoryQuery {
    /// 返回最近多少个采样点，默认 120（未传 from/to 时生效）
    #[serde(default = "default_limit")]
    limit: i64,
    /// 可选时间窗口（Unix 秒）：from/to 同时给出时按窗口查询，
    /// 起点超出原始保留窗口则自动切小时聚合表（1.2 保留策略）
    from: Option<i64>,
    to: Option<i64>,
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
    match (q.from, q.to) {
        (Some(from), Some(to)) if from <= to => {
            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            let raw_from = now - crate::db::RAW_RETENTION_SECS;
            Ok(Json(
                state.db.history_async(from, to, raw_from, limit).await?,
            ))
        }
        _ => Ok(Json(state.db.recent_metrics_async(limit).await?)),
    }
}

#[derive(Deserialize)]
struct ProcessesQuery {
    /// system（默认）/ app / all
    scope: Option<String>,
    /// 实例 id（container:<短ID> / app:<路径>）；给了就只看该实例的进程
    instance: Option<String>,
}

async fn processes_list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ProcessesQuery>,
) -> Result<Json<Vec<monitor::ProcessInfo>>, ApiError> {
    let scope = rprocess::Scope::parse(q.scope.as_deref());
    Ok(Json(
        rprocess::list(&state, scope, q.instance.as_deref()).await?,
    ))
}

/// 实例列表：容器 + 主机应用，作为各自文件/日志/进程的入口
async fn instances_list(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<crate::instances::Instance>>, ApiError> {
    crate::instances::list(&state)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct InstanceFileQuery {
    path: String,
}

/// 从实例 id 取容器短 ID。主机应用没有容器文件系统，
/// 明确报错而不是静默退化成宿主机路径——那会让人以为在看容器里的东西。
fn container_of(id: &str) -> Result<&str, ApiError> {
    id.strip_prefix(crate::instances::CONTAINER_PREFIX)
        .ok_or_else(|| ApiError::bad("该实例不是容器，没有独立的文件系统"))
}

/// 容器内目录列表（只读）
async fn instance_files(
    _user: AuthUser,
    Path(id): Path<String>,
    Query(q): Query<InstanceFileQuery>,
) -> Result<Json<crate::files::DirListing>, ApiError> {
    let cid = container_of(&id)?;
    crate::container_files::list_dir(cid, &q.path)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

/// 容器内文本文件内容（只读）
async fn instance_file_read(
    _user: AuthUser,
    Path(id): Path<String>,
    Query(q): Query<InstanceFileQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let cid = container_of(&id)?;
    let text = crate::container_files::read_file(cid, &q.path)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "content": text })))
}

#[derive(Deserialize)]
struct KillReq {
    pid: u32,
}

async fn processes_kill(
    State(state): State<AppState>,
    _: RequireRole<1>,
    SafeJson(req): SafeJson<KillReq>,
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
    _: RequireRole<1>,
    SafeJson(req): SafeJson<ServiceActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let out = opservice::action(&req.name, req.action)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct PowerActionReq {
    action: opservice::PowerAction,
}

/// 电源级操作（admin）：面板重启 / 整机重启 / 关机。
/// 整机操作危险，仅 admin 可用；审计中间件自动记录非 GET 请求。
async fn power_action(
    _: RequireRole<2>,
    SafeJson(req): SafeJson<PowerActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let out = opservice::power(req.action)
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

/// 4.3 服务 unit 文件查看的查询参数
#[derive(Deserialize)]
struct ServiceUnitQuery {
    name: String,
}

fn default_lines() -> u32 {
    200
}

async fn logs_journal(
    _user: AuthUser,
    Query(q): Query<JournalQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // P1-3：journal 按用户给的 unit 读取，失败多为参数问题 → 4xx + 通用文案，
    // 细节（journalctl 输出等）只进日志
    let text = crate::logs::journal(q.unit.as_deref(), q.lines)
        .await
        .map_err(|e| {
            tracing::warn!("日志读取失败（journal）：{e:#}");
            ApiError::bad("日志读取失败（参数无效或 journal 服务不可用）")
        })?;
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
    // P1-3：路径来自客户端，读取失败（不存在/无权限/非普通文件）属输入问题
    // → 4xx + 通用文案，不回显路径与 errno
    let text = crate::logs::tail_file(&q.path, q.lines)
        .await
        .map_err(|e| {
            tracing::warn!("日志读取失败（tail）：{e:#}");
            ApiError::bad("无法读取该日志（不存在、无权限或非普通文件）")
        })?;
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
    let text = crate::files::read_file(&q.path)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "content": text })))
}

#[derive(Deserialize)]
struct WriteReq {
    path: String,
    content: String,
}

async fn files_write(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<WriteReq>,
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
    _: RequireRole<1>,
    SafeJson(req): SafeJson<MkdirReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::mkdir(&req.path)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn files_delete(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<PathQuery>,
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
    _: RequireRole<1>,
    SafeJson(req): SafeJson<RenameReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::rename(&req.from, &req.to)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn files_download(_user: AuthUser, Query(q): Query<PathQuery>) -> Result<Response, ApiError> {
    let (name, bytes) = crate::files::download(&q.path)
        .await
        .map_err(ApiError::file_err)?;
    let ct = mime_guess::from_path(&name)
        .first_or_octet_stream()
        .to_string();
    // 文件名可能含非 ASCII，使用 RFC 5987 编码
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let disposition = format!("attachment; filename*=UTF-8''{encoded}");
    let mut resp = ([(header::CONTENT_TYPE, ct)], bytes).into_response();
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// 上传：multipart 表单，字段 dir（目标目录）+ file（文件）
async fn files_upload(
    _: RequireRole<1>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut dir = String::new();
    let mut fname = String::new();
    let mut fbytes: Vec<u8> = Vec::new();
    // P1-3：multipart 解析错误属客户端请求问题 → 4xx + 通用文案，细节只进日志
    while let Some(field) = mp.next_field().await.map_err(|e| {
        tracing::warn!("上传数据解析失败：{e}");
        ApiError::bad("上传请求格式错误")
    })? {
        match field.name().unwrap_or("") {
            "dir" => {
                dir = field.text().await.map_err(|e| {
                    tracing::warn!("上传字段 dir 读取失败：{e}");
                    ApiError::bad("上传请求格式错误")
                })?
            }
            "file" => {
                fname = field.file_name().unwrap_or("upload.bin").to_string();
                fbytes = field
                    .bytes()
                    .await
                    .map_err(|e| {
                        tracing::warn!("上传字段 file 读取失败：{e}");
                        ApiError::bad("上传数据读取失败")
                    })?
                    .to_vec();
            }
            _ => {}
        }
    }
    if dir.is_empty() || fbytes.is_empty() {
        return Err(ApiError::bad("缺少 dir 或 file 字段"));
    }
    let size = fbytes.len();
    let saved = crate::files::save_upload(&dir, &fname, fbytes)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "path": saved, "size": size }),
    ))
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
    crate::packages::upgradable()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
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
    _: RequireRole<1>,
    SafeJson(req): SafeJson<PkgActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 同步路径：P2-1 起前端长操作默认走 /api/jobs，这里保留原语义供
    // 脚本调用与灰度回退；空回调表示不需要逐行输出。
    let output = match req.action.as_str() {
        "update" => crate::packages::update_index(&mut |_: &str| {}).await,
        "install" => crate::packages::install(&req.names, &mut |_: &str| {}).await,
        "upgrade" => crate::packages::upgrade(&req.names, &mut |_: &str| {}).await,
        "sysupgrade" => crate::packages::system_upgrade(&mut |_: &str| {}).await,
        "remove" => crate::packages::remove(&req.names, &mut |_: &str| {}).await,
        _ => Err(anyhow::anyhow!("未知操作")),
    }
    .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "output": output })))
}

/// 包管理能力描述。前端不猜发行版：Arch 系没有「可升级」这个概念，
/// 只有滚动更新（同步数据库 + 全量升级一步完成），标签与按钮都据此决定。
#[derive(Serialize)]
struct PkgMeta {
    /// "arch" / "debian"
    family: &'static str,
    /// /etc/os-release 的 PRETTY_NAME，用于界面提示
    pretty: &'static str,
    /// 包管理器命令名："pacman" / "apt"
    manager: &'static str,
    /// true = 滚动更新发行版（Arch 系）
    rolling: bool,
}

async fn packages_meta(_user: AuthUser) -> Json<PkgMeta> {
    let rolling = matches!(crate::distro::family(), crate::distro::Family::Arch);
    Json(PkgMeta {
        family: if rolling { "arch" } else { "debian" },
        pretty: crate::distro::pretty(),
        manager: if rolling { "pacman" } else { "apt" },
        rolling,
    })
}

// ---------- 计划任务 ----------

async fn cron_list(_user: AuthUser) -> Result<Json<Vec<crate::crontab::CronEntry>>, ApiError> {
    crate::crontab::list()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct CronReq {
    index: Option<usize>,
    entry: crate::crontab::CronEntry,
}

async fn cron_add(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<CronReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::crontab::add(&req.entry)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn cron_update(
    _: State<AppState>,
    _: RequireRole<1>,
    SafeJson(req): SafeJson<CronReq>,
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
    _: RequireRole<1>,
    SafeJson(req): SafeJson<CronDeleteReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::crontab::delete(req.index)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- 网络查看 ----------

async fn net_interfaces(_user: AuthUser) -> Result<Json<serde_json::Value>, ApiError> {
    crate::network::interfaces()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

async fn net_routes(_user: AuthUser) -> Result<Json<serde_json::Value>, ApiError> {
    crate::network::routes()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

async fn net_connections(_user: AuthUser) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    crate::network::connections()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

async fn net_dns(_user: AuthUser) -> Result<Json<Vec<String>>, ApiError> {
    crate::network::dns()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

// ---------- Docker 控制 ----------

/// Docker 环境状态：未安装或守护进程未启动时也返回 200，由前端决定展示方式
async fn docker_status(_user: AuthUser) -> Result<Json<crate::docker::DockerStatus>, ApiError> {
    crate::docker::status()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

/// 一键安装 Docker（同步路径；前端长操作走 /api/jobs 的 docker_install）
async fn docker_install(_: RequireRole<2>) -> Result<Json<serde_json::Value>, ApiError> {
    let output = crate::docker::install(&mut |_: &str| {})
        .await
        .map_err(ApiError::file_err)?;
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
    _: RequireRole<1>,
    SafeJson(req): SafeJson<ContainerActionReq>,
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
    crate::docker::images()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct ImageActionReq {
    action: String,
    /// pull 时为镜像名，remove 时为镜像 ID
    target: String,
}

async fn docker_image_action(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<ImageActionReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let output = match req.action.as_str() {
        "pull" => crate::docker::pull(&req.target, &mut |_: &str| {}).await,
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
    _: RequireRole<1>,
    SafeJson(req): SafeJson<ComposeActionReq>,
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

// ---------- 主题定制 ----------

/// settings 表中主题配置的键名（值为前端序列化的 JSON 字符串）
const THEME_KEY: &str = "theme_config";
/// 配置 JSON 的字节上限。背景图以 data URL 内嵌，2MB 图片 base64 后约 2.7MB，
/// 留一点余量取 3MB；防误传超大文件撑爆数据库。
const THEME_MAX_BYTES: usize = 3 * 1024 * 1024;

/// 主题字段白名单校验（P1-2）：前端会在应用前再校验一次，这里做服务端闸门，
/// 防止把恶意值存进库后经主题 CSS 注入攻击浏览器。
///
/// - 允许字段：version / name / radius / colors{primary,bg_page,bg_card,text} / bg_image
/// - 颜色：必须为 `#rrggbb`（6 位十六进制，带 #）
/// - radius：数值 0..=64
/// - bg_image：必须以 `data:image/` 开头，且不含引号/括号/反斜杠/控制字符
///   （这些字符可闭合 CSS 的 `url("...")` 字符串，构成样式注入逃逸）
fn validate_theme(cfg: &serde_json::Value) -> Result<(), String> {
    let obj = cfg
        .as_object()
        .ok_or_else(|| "主题配置必须是对象".to_string())?;
    for (k, v) in obj {
        match k.as_str() {
            "version" => {
                if v.as_number().is_none() {
                    return Err("version 必须是数字".into());
                }
            }
            "name" => {
                let s = v.as_str().ok_or("name 必须是字符串")?;
                if s.chars().count() > 64 {
                    return Err("name 过长（上限 64 字符）".into());
                }
            }
            "radius" => {
                let r = v.as_f64().ok_or("radius 必须是数字")?;
                if !(0.0..=64.0).contains(&r) {
                    return Err("radius 必须在 0..64 之间".into());
                }
            }
            "colors" => {
                let colors = v.as_object().ok_or("colors 必须是对象")?;
                for (ck, cv) in colors {
                    if !matches!(ck.as_str(), "primary" | "bg_page" | "bg_card" | "text") {
                        return Err(format!("未知颜色字段：{ck}"));
                    }
                    let s = cv.as_str().ok_or("颜色值必须是字符串")?;
                    let b = s.as_bytes();
                    let ok = b.len() == 7
                        && b[0] == b'#'
                        && b[1..].iter().all(|c| c.is_ascii_hexdigit());
                    if !ok {
                        return Err(format!("颜色 {ck} 必须是 #rrggbb 格式（6 位十六进制）"));
                    }
                }
            }
            "bg_image" => {
                let s = v.as_str().ok_or("bg_image 必须是字符串")?;
                if !s.starts_with("data:image/") {
                    return Err("bg_image 仅允许 data:image/ 前缀的 data URL".into());
                }
                if s.chars()
                    .any(|c| c == '"' || c == ')' || c == '\\' || (c as u32) < 0x20)
                {
                    return Err("bg_image 含有不允许的字符".into());
                }
            }
            _ => return Err(format!("未知字段：{k}")),
        }
    }
    Ok(())
}

/// 读取主题配置；未定制过时返回 null，前端据此使用默认样式。
/// 库里存了历史遗留的非法配置时按无定制处理（前端另有二次校验兜底）。
async fn theme_get(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    // DB 失败属服务端内部错误 → 500 通用文案（P1-3）
    let raw = state.db.get_setting_async(THEME_KEY).await?;
    let value = match raw {
        Some(s) => {
            let v =
                serde_json::from_str::<serde_json::Value>(&s).unwrap_or(serde_json::Value::Null);
            if !v.is_null() && validate_theme(&v).is_err() {
                tracing::warn!("忽略库中未通过安全校验的主题配置");
                serde_json::Value::Null
            } else {
                v
            }
        }
        None => serde_json::Value::Null,
    };
    Ok(Json(serde_json::json!({ "config": value })))
}

/// 保存主题配置。请求体为原样 JSON，先做字段白名单校验（P1-2）再入库；
/// 传 null 表示删除配置、恢复默认。
async fn theme_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > THEME_MAX_BYTES {
        return Err(ApiError::bad(format!(
            "主题配置过大（上限 {}MB）",
            THEME_MAX_BYTES / 1024 / 1024
        )));
    }
    let text =
        String::from_utf8(body.to_vec()).map_err(|_| ApiError::bad("主题配置必须是 UTF-8 JSON"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| ApiError::bad("主题配置 JSON 无法解析"))?;
    if !value.is_null() {
        // P1-2：服务端白名单校验，不通过不入库
        validate_theme(&value).map_err(ApiError::bad)?;
    }
    let stored = if value.is_null() {
        ""
    } else {
        &value.to_string()
    };
    state.db.set_setting_async(THEME_KEY, stored).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- 4.2 仪表盘自定义 ----------

/// settings 表中仪表盘配置的键名
const DASHBOARD_KEY: &str = "dashboard_config";
/// 配置 JSON 字节上限：结构极小（一个字符串数组），4KB 已远超需要，纯防滥用
const DASHBOARD_MAX_BYTES: usize = 4 * 1024;

/// 仪表盘卡片白名单（与前端 Dashboard.vue 的卡片 id 一一对应）
const DASHBOARD_CARDS: [&str; 8] = [
    "cpu", "mem", "disk", "net", "load", "uptime", "swap", "procs",
];

/// 仪表盘配置校验（复用 P1-2 主题校验思路：白名单 + 类型 + 长度）：
/// - 只允许一个顶层字段 cards；
/// - cards 为非空字符串数组，长度 ≤ 白名单大小，元素在白名单内且不重复。
fn validate_dashboard_config(cfg: &serde_json::Value) -> Result<(), String> {
    let obj = cfg
        .as_object()
        .ok_or_else(|| "配置必须是 JSON 对象".to_string())?;
    for k in obj.keys() {
        if k != "cards" {
            return Err(format!("未知字段：{k}"));
        }
    }
    let cards = obj
        .get("cards")
        .ok_or_else(|| "缺少 cards 字段".to_string())?
        .as_array()
        .ok_or_else(|| "cards 必须是数组".to_string())?;
    if cards.is_empty() {
        return Err("cards 不能为空".to_string());
    }
    if cards.len() > DASHBOARD_CARDS.len() {
        return Err(format!("卡片数量过多（上限 {}）", DASHBOARD_CARDS.len()));
    }
    let mut seen = std::collections::HashSet::new();
    for c in cards {
        let s = c.as_str().ok_or_else(|| "卡片项必须是字符串".to_string())?;
        if !DASHBOARD_CARDS.contains(&s) {
            return Err(format!("未知卡片：{s}"));
        }
        if !seen.insert(s.to_string()) {
            return Err(format!("卡片重复：{s}"));
        }
    }
    Ok(())
}

/// 读取仪表盘配置；未定制过返回 null，前端使用默认顺序展示全部卡片。
async fn dashboard_config_get(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let raw = state.db.get_setting_async(DASHBOARD_KEY).await?;
    let value = match raw {
        Some(s) if !s.is_empty() => {
            serde_json::from_str::<serde_json::Value>(&s).unwrap_or(serde_json::Value::Null)
        }
        _ => serde_json::Value::Null,
    };
    Ok(Json(serde_json::json!({ "config": value })))
}

/// 保存仪表盘配置。body 传 null 表示删除配置、恢复默认。
async fn dashboard_config_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > DASHBOARD_MAX_BYTES {
        return Err(ApiError::bad("仪表盘配置过大"));
    }
    let text = String::from_utf8(body.to_vec())
        .map_err(|_| ApiError::bad("仪表盘配置必须是 UTF-8 JSON"))?;
    // 空请求体（axios 发 JSON null 时不带字节）与字面 null 同义：恢复默认
    let value: serde_json::Value = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text).map_err(|_| ApiError::bad("仪表盘配置 JSON 无法解析"))?
    };
    if !value.is_null() {
        // 白名单校验不通过不入库
        validate_dashboard_config(&value).map_err(ApiError::bad)?;
    }
    let stored = if value.is_null() {
        ""
    } else {
        &value.to_string()
    };
    state.db.set_setting_async(DASHBOARD_KEY, stored).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- AI API 配置（设置页） ----------

/// settings 表键名与解析逻辑都在 ai.rs，保持单一事实来源
const AI_CONFIG_KEY: &str = crate::ai::AI_CONFIG_KEY;
/// 配置 JSON 字节上限：API 三元组 + 人格/技能文本，64KB 足够宽松
const AI_CONFIG_MAX_BYTES: usize = 64 * 1024;
const AI_BASE_MAX_LEN: usize = 512;
const AI_KEY_MAX_LEN: usize = 512;
const AI_MODEL_MAX_LEN: usize = 128;

/// 密钥脱敏回显：≥8 字符保留末 4 位，更短的一律全遮。
/// 任何接口都不应返回密钥明文，这是唯一的回显形态。
fn mask_key(key: &str) -> String {
    let n = key.chars().count();
    if n >= 8 {
        let tail: String = key.chars().skip(n - 4).collect();
        format!("••••{tail}")
    } else {
        "••••".to_string()
    }
}

/// 保存前校验（独立成函数以便单元测试）：长度上限 + 地址协议白名单。
/// 密钥内容不校验格式（各家供应商前缀不一），只限制长度。
fn validate_ai_config(base: &str, key: &str, model: &str) -> Result<(), String> {
    if base.len() > AI_BASE_MAX_LEN {
        return Err("上游 API 地址过长".into());
    }
    if !base.is_empty() && !base.starts_with("http://") && !base.starts_with("https://") {
        return Err("上游 API 地址必须以 http(s):// 开头".into());
    }
    if key.len() > AI_KEY_MAX_LEN {
        return Err("API 密钥过长".into());
    }
    if model.len() > AI_MODEL_MAX_LEN {
        return Err("模型名过长".into());
    }
    Ok(())
}

/// 读取 AI API 配置（登录即可，AI 聊天页的「未配置」提示要用）。
/// 密钥只回 `key_masked`（脱敏）与 `key_set`（是否已存），绝不回明文。
async fn ai_config_get(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let stored = crate::ai::parse_stored(
        state.db.get_setting_async(AI_CONFIG_KEY).await?,
    );
    let env_key = crate::ai::env_var("AI_API_KEY").is_some();
    let key_set = !stored.key.trim().is_empty();
    // 回显「生效值」：设置页为空时环境变量/默认值兜底，便于管理员核对
    let base = if stored.base.trim().is_empty() {
        crate::ai::env_var("AI_API_BASE")
            .unwrap_or_else(|| "https://api.openai.com/v1".into())
    } else {
        stored.base.trim().to_string()
    };
    let model = if stored.model.trim().is_empty() {
        crate::ai::env_var("AI_MODEL").unwrap_or_else(|| "gpt-4o-mini".into())
    } else {
        stored.model.trim().to_string()
    };
    Ok(Json(serde_json::json!({
        "config": {
            "base": base,
            "model": model,
            "key_set": key_set,
            "key_masked": if key_set { Some(mask_key(stored.key.trim())) } else { None },
            "env_key_set": env_key,
            "configured": key_set || env_key,
            "persona": stored.persona,
            "skills": stored.skills,
        }
    })))
}

/// 保存 AI 配置（仅 admin）。字段语义：
/// - `base`/`model`：传值即覆盖（空串=清除，回退环境变量/默认值）；
/// - `key`：`None` = 不改（前端不传即保留原密钥），空串 = 清除，非空 = 覆盖；
/// - `persona`/`skills`：传值即覆盖（空串=清除，回退内置默认人格）。
async fn ai_config_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > AI_CONFIG_MAX_BYTES {
        return Err(ApiError::bad("AI 配置过大"));
    }
    let text = String::from_utf8(body.to_vec())
        .map_err(|_| ApiError::bad("AI 配置必须是 UTF-8 JSON"))?;
    #[derive(Deserialize)]
    struct Req {
        base: Option<String>,
        key: Option<String>,
        model: Option<String>,
        persona: Option<String>,
        skills: Option<String>,
    }
    let req: Req =
        serde_json::from_str(&text).map_err(|_| ApiError::bad("AI 配置 JSON 无法解析"))?;
    let mut stored = crate::ai::parse_stored(
        state.db.get_setting_async(AI_CONFIG_KEY).await?,
    );
    if let Some(v) = req.base {
        stored.base = v.trim().to_string();
    }
    if let Some(v) = req.key {
        stored.key = v.trim().to_string();
    }
    if let Some(v) = req.model {
        stored.model = v.trim().to_string();
    }
    if let Some(v) = req.persona {
        stored.persona = v;
    }
    if let Some(v) = req.skills {
        stored.skills = v;
    }
    validate_ai_config(&stored.base, &stored.key, &stored.model).map_err(ApiError::bad)?;
    let json = serde_json::to_string(&stored)
        .map_err(|_| ApiError::bad("AI 配置序列化失败"))?;
    state.db.set_setting_async(AI_CONFIG_KEY, &json).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- 4.3 深度运维 ----------

/// 服务 unit 文件内容查看（只读）。所有角色可用（viewer 也允许查看）。
async fn services_unit_get(
    Query(q): Query<ServiceUnitQuery>,
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let name = q.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("缺少服务名"));
    }
    let content = crate::ops::unit_file(name)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(
        serde_json::json!({ "name": name, "content": content }),
    ))
}

/// SMART 磁盘健康概要（只读探测）。smartctl 缺失时返回降级提示而非报错。
async fn disks_smart_get(_user: AuthUser) -> Result<Json<crate::ops::SmartReport>, ApiError> {
    let report = crate::ops::smart_report().await;
    Ok(Json(report))
}

// 4.3 容器日志流（WebSocket）

/// WS 查询参数：浏览器端 WebSocket 无法携带 Authorization 头，
/// token 经查询串传递；id 为目标容器，tail 为初始回看行数。
#[derive(Deserialize)]
struct LogStreamQuery {
    id: String,
    token: String,
    tail: Option<usize>,
}

/// WebSocket 日志流的手动鉴权：与 AuthUser 提取器同一套口径
/// （token 校验 → 吊销名单 → 会话存在 → 账号存在 → 首登改密闸门），
/// 只是认证载体从 Header 换成了查询串。任意失败一律拒绝升级。
/// 4.5 会话 WS 同样复用此函数（故 pub(crate)）。
pub(crate) async fn ws_auth(state: &AppState, token: &str) -> Result<(), ApiError> {
    let claims = auth::verify_token(&state.jwt_secret, token)
        .map_err(|_| ApiError::unauthorized("登录已过期，请重新登录"))?;
    if state.revocations.is_revoked(&claims.jti).await {
        return Err(ApiError::unauthorized("登录已失效，请重新登录"));
    }
    if !state.db.session_exists_async(&claims.jti).await? {
        return Err(ApiError::unauthorized("登录已失效，请重新登录"));
    }
    let user = state
        .db
        .user_by_id_async(claims.sub)
        .await?
        .ok_or_else(|| ApiError::unauthorized("账号已被删除，请重新登录"))?;
    if user.2 {
        return Err(ApiError::forbidden("请先修改初始密码"));
    }
    Ok(())
}

async fn docker_logstream_ws(
    State(state): State<AppState>,
    Query(q): Query<LogStreamQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    ws_auth(&state, &q.token).await?;
    // check_id 返回 anyhow::Error，转成面向客户端的通用文案（细节只进日志）。
    // id 先转成 owned String 再移动进 WS 升级闭包，避免借用 q 的局部值。
    let id = q.id.trim().to_string();
    crate::docker::check_id(&id).map_err(|e| {
        tracing::warn!("容器日志流：非法容器标识");
        ApiError::bad(e.to_string())
    })?;
    // 初始回看行数限制在 1..=1000，防超大查询拖垮 docker daemon
    let tail = q.tail.unwrap_or(200).clamp(1, 1000);
    Ok(ws.on_upgrade(move |socket| crate::ops::container_log_stream(socket, id, tail)))
}

// ---------- 3.1 备份管理（admin）----------

#[derive(Deserialize)]
struct BackupNameQuery {
    name: String,
}

async fn backups_list(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<Vec<crate::backup::BackupInfo>>, ApiError> {
    let data_dir = state.data_dir.clone();
    let list = tokio::task::spawn_blocking(move || crate::backup::list_backups(&data_dir))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(list))
}

async fn backups_create(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.clone();
    let data_dir = state.data_dir.clone();
    let name = tokio::task::spawn_blocking(move || crate::backup::create_backup(&db, &data_dir))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "name": name })))
}

async fn backup_download(
    State(state): State<AppState>,
    _: RequireRole<2>,
    Query(q): Query<BackupNameQuery>,
) -> Result<Response, ApiError> {
    let data_dir = state.data_dir.clone();
    let name = q.name.clone();
    let name_for_check = q.name.clone();
    let path = tokio::task::spawn_blocking(move || {
        crate::backup::resolve_backup(&data_dir, &name_for_check)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::file_err)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::file_err(anyhow::anyhow!("读取备份文件失败")))?;
    // 备份名是纯 ASCII 白名单格式，直接拼 Content-Disposition 即可
    let resp = (
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response();
    Ok(resp)
}

async fn backup_delete(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<BackupNameQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || crate::backup::delete_backup(&data_dir, &req.name))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 登记恢复（重启服务后生效）。恢复会整体覆盖当前库，操作前请先下载备份留档。
async fn backup_restore(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<BackupNameQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || crate::backup::set_pending_restore(&data_dir, &req.name))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "恢复已登记，重启面板服务后生效"
    })))
}

// ---------- 3.2 面板自更新（admin）----------

/// 上传二进制体积上限（release 产物约 10-40MB）
const UPDATE_MAX_BYTES: usize = 80 * 1024 * 1024;

async fn update_check(_: RequireRole<2>) -> Result<Json<crate::update::UpdateStatus>, ApiError> {
    let client = reqwest::Client::new();
    match crate::update::check(&client).await {
        Ok(s) => Ok(Json(s)),
        Err(e) => {
            // 检查失败（离线/限流）不算接口错误：带回当前版本与失败说明
            tracing::warn!("检查更新失败：{e:#}");
            Ok(Json(crate::update::UpdateStatus {
                current: crate::update::CURRENT_VERSION.into(),
                latest: None,
                has_update: false,
                error: Some(e.to_string()),
            }))
        }
    }
}

async fn update_install(_: RequireRole<2>) -> Result<Json<serde_json::Value>, ApiError> {
    let client = reqwest::Client::new();
    let download = crate::update::download_github(&client)
        .await
        .map_err(ApiError::file_err)?;
    // P1-2：sha256 由 download_github 一并取回；取不到校验和文件时在上一步就已失败
    let tag = download.tag.clone();
    tokio::task::spawn_blocking(move || {
        crate::update::install_binary(&download.bytes, Some(&download.sha256))
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "version": tag,
        "message": "新版本已就位，重启面板服务后生效"
    })))
}

/// 手动上传新二进制（内网无法访问 GitHub 时的旁路）
async fn update_upload(
    _: RequireRole<2>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.is_empty() {
        return Err(ApiError::bad("上传内容为空"));
    }
    let bytes = body.to_vec();
    // 手动通道没有上游校验和可比对，只能做体积 + magic 两道闸；
    // 该接口本身已要求 admin 身份，风险面与 GitHub 拉取通道不同。
    tokio::task::spawn_blocking(move || crate::update::install_binary(&bytes, None))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "新版本已就位，重启面板服务后生效"
    })))
}

// ---------- 3.3 告警通知（admin）----------

async fn alerts_rules_get(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<Vec<crate::alerts::AlertRule>>, ApiError> {
    let db = state.db.clone();
    let rules = tokio::task::spawn_blocking(move || crate::alerts::load_rules(&db))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(rules))
}

/// 全量保存规则集（前端提交完整列表）。
///
/// P2-2：保存成功后立刻向采样循环发一次递增信号，规则即时生效；采样循环内
/// 仍有 720 tick 的兜底重载，信号丢失不会让规则永久不生效。
async fn alerts_rules_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(rules): SafeJson<Vec<crate::alerts::AlertRule>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if rules.len() > 20 {
        return Err(ApiError::bad("规则数量过多（上限 20 条）"));
    }
    for r in &rules {
        crate::alerts::validate_rule(r).map_err(|e| ApiError::bad(e.to_string()))?;
    }
    let db = state.db.clone();
    let owned = rules.clone();
    tokio::task::spawn_blocking(move || crate::alerts::save_rules(&db, &owned))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    // P2-2：唤醒采样循环重载规则（receiver 被丢弃时此处无副作用）
    state.alert_reload.send_modify(|v| *v = v.wrapping_add(1));
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct WebhookReq {
    url: Option<String>,
}

async fn alerts_webhook_get(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.clone();
    let url = tokio::task::spawn_blocking(move || crate::alerts::load_webhook(&db))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(serde_json::json!({ "url": url })))
}

async fn alerts_webhook_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<WebhookReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.clone();
    let url = req.url.clone();
    // 校验失败必须以 400 反馈（save_webhook 内部会 validate）
    if let Some(u) = &url {
        if !u.is_empty() {
            crate::alerts::validate_webhook_url(u).map_err(|e| ApiError::bad(e.to_string()))?;
        }
    }
    tokio::task::spawn_blocking(move || crate::alerts::save_webhook(&db, url.as_deref()))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn alerts_events(
    State(state): State<AppState>,
    _: RequireRole<2>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<crate::db::AlertEventRow>>, ApiError> {
    let limit = q.limit.clamp(1, 500);
    let offset = q.offset.max(0);
    Ok(Json(state.db.alert_event_list_async(limit, offset).await?))
}

// ---------- 审计中间件（2.3） ----------

/// 从请求头尽力解析出当前用户（id, username）。
/// 解析失败返回 None（登录前请求、非法 token 等），审计仍会记录匿名条目。
async fn audit_actor(state: &AppState, headers: &axum::http::HeaderMap) -> Option<(i64, String)> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())?
        .strip_prefix("Bearer ")?;
    let claims = auth::verify_token(&state.jwt_secret, token).ok()?;
    if state.revocations.is_revoked(&claims.jti).await {
        return None;
    }
    let user = state.db.user_by_id_async(claims.sub).await.ok()??;
    Some((claims.sub, user.0))
}

/// 审计摘要取样的体积上限：超过即不取样（见 [`take_audit_detail`]）
const AUDIT_BODY_LIMIT: usize = 8 * 1024;

/// 审计摘要的字段白名单。
///
/// 只认这些键、其余一律不入库，这样即便请求体里混进密码或 token，
/// 也不存在进入审计表的路径 —— 白名单本身就是第一道防线。
const AUDIT_DETAIL_FIELDS: [&str; 8] = [
    "action",
    "path",
    "name",
    "names",
    "package",
    "unit",
    "container",
    "target",
];

/// 单个字段值的长度上限
const AUDIT_VALUE_LIMIT: usize = 200;

/// 第二道防线：白名单键里若含这些字样也丢弃
const AUDIT_SENSITIVE: [&str; 4] = ["password", "token", "secret", "new_password"];

/// 从 JSON 请求体中提取审计摘要；任何一步不合条件都返回 None（审计退化为旧行为）。
fn summarize_body(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = value.as_object()?;
    let mut parts = Vec::new();
    for key in AUDIT_DETAIL_FIELDS {
        if AUDIT_SENSITIVE.iter().any(|s| key.contains(s)) {
            continue;
        }
        let Some(v) = obj.get(key) else { continue };
        let text = match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            // 数组取前几项：装/卸包是 {"names": ["a","b"]}
            serde_json::Value::Array(items) => {
                let joined: Vec<String> = items
                    .iter()
                    .take(8)
                    .map(|i| match i {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                if joined.is_empty() {
                    continue;
                }
                joined.join(",")
            }
            // 嵌套对象不展开：递归下去等于让调用方决定审计表里存什么
            _ => continue,
        };
        let shown: String = text.chars().take(AUDIT_VALUE_LIMIT).collect();
        parts.push(format!("{key}={shown}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

/// 按闸门条件取样请求体，并把 body 原样交还下游。
///
/// 这一步把中间件从「只读观察者」变成「消费并重建 body」的角色，所以闸门设得保守：
/// 必须是 JSON、必须带 Content-Length、且长度不超过 [`AUDIT_BODY_LIMIT`]。
/// 分块传输（无 Content-Length）与大 body 一律跳过，不把大文件上传的体积拉进内存。
///
/// 读取失败时交还一个空体：这不改变最终结果 —— 能走到这里的请求其 Content-Length
/// 已声明在阈值内，真正读失败只可能是流中断或声明与实际不符，两种情况下游本来也
/// 拿不到可用的 body。
async fn take_audit_detail(req: Request) -> (Request, Option<String>) {
    let is_json = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    let declared = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    let Some(declared) = declared else {
        return (req, None);
    };
    if !is_json || declared > AUDIT_BODY_LIMIT {
        return (req, None);
    }

    let (parts, body) = req.into_parts();
    match axum::body::to_bytes(body, AUDIT_BODY_LIMIT).await {
        Ok(bytes) => {
            let detail = summarize_body(&bytes);
            // 原样重建：下游 extractor 必须还能正常反序列化
            let rebuilt = Request::from_parts(parts, axum::body::Body::from(bytes));
            (rebuilt, detail)
        }
        Err(_) => (Request::from_parts(parts, axum::body::Body::empty()), None),
    }
}

/// 审计中间件：记录 /api 下所有非 GET 请求（方法、路径、状态码、来源 IP、操作者）。
/// 路径不含查询串，避免敏感参数（如密码走 body 不落库）进入审计。
/// 登录接口由 handler 自行记录（body 里有用户名，中间件拿不到），此处跳过。
async fn audit_mw(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    // nest 剥掉了 /api 前缀，用 OriginalUri 记录完整路径
    let full_path = req
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    if method == axum::http::Method::GET || full_path == "/api/login" {
        return next.run(req).await;
    }
    let path = full_path;
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_default();
    let headers = req.headers().clone();
    let actor = audit_actor(&state, &headers).await;
    // P1-3：取样参数摘要。取样后 body 已原样重建，下游 handler 不受影响
    let (req, detail) = take_audit_detail(req).await;
    let resp = next.run(req).await;
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();
    let (user_id, username) = match actor {
        Some((id, name)) => (Some(id), name),
        None => (None, "-".to_string()),
    };
    if let Err(e) = state
        .db
        .audit_async(
            ts,
            user_id,
            &username,
            method.as_str(),
            &path,
            resp.status().as_u16(),
            &ip,
            detail.as_deref(),
        )
        .await
    {
        // 审计写失败不阻断业务响应，只留 tracing 告警
        tracing::warn!("写入审计日志失败：{e}");
    }
    resp
}

/// 健康检查（无需认证）
async fn health() -> &'static str {
    "ok"
}

/// 组装路由：/api/* 下所有业务接口都需要认证，其余走前端 SPA
// ---------- P2-1 作业队列 ----------

#[derive(Deserialize)]
struct JobSubmitReq {
    /// 作业类型标签，取值见 `jobs::JobKind::parse`
    kind: String,
    /// 作业参数（包名列表 / 镜像引用等），原样落进 jobs.payload
    #[serde(default)]
    payload: serde_json::Value,
}

/// 提交作业：立即返回 id，不等命令跑完。
/// 参数校验与命令构造在执行体内完成（与同步路径同一套函数），
/// 因此这里唯一可能的失败原因是写库。
async fn jobs_submit(
    State(state): State<AppState>,
    _: RequireRole<1>,
    SafeJson(req): SafeJson<JobSubmitReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let kind = crate::jobs::JobKind::parse(&req.kind)
        .ok_or_else(|| ApiError::bad(format!("未知的作业类型：{}", req.kind)))?;
    let id = crate::jobs::submit(&state, kind, req.payload)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct JobListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

/// 作业列表（创建时间倒序分页）
async fn jobs_list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<JobListQuery>,
) -> Result<Json<Vec<crate::db::JobRow>>, ApiError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    state
        .db
        .job_list_async(limit, offset)
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

/// 单条作业详情
async fn jobs_get(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<crate::db::JobRow>, ApiError> {
    match state
        .db
        .job_get_async(&id)
        .await
        .map_err(ApiError::file_err)?
    {
        Some(row) => Ok(Json(row)),
        None => Err(ApiError::bad("作业不存在")),
    }
}

/// 取消作业。SSE 是单向的，所以取消必须走这个独立的 POST。
async fn jobs_cancel(
    State(state): State<AppState>,
    _: RequireRole<1>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 顺序要紧：先 abort 执行体（子进程靠 kill_on_drop 带走），再落状态。
    // 反过来的话，执行体可能在两次写之间把状态改成 failed，与用户的取消对不上。
    state.jobs.cancel(&id);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let n = state
        .db
        .job_set_status_async(&id, "cancelled", now)
        .await
        .map_err(ApiError::file_err)?;
    if n == 0 {
        return Err(ApiError::bad("作业不存在或已结束"));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 作业输出流（SSE）：先回放库里的输出尾部，再续推增量。
///
/// 断流不丢进度 —— 前端刷新页面会重连并按尾部重放；但超过 `jobs::TAIL_LINES`
/// 的早期内容不可追溯，这是刻意的取舍。
async fn jobs_stream(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let Some(job) = state
        .db
        .job_get_async(&id)
        .await
        .map_err(ApiError::file_err)?
    else {
        return Err(ApiError::bad("作业不存在"));
    };
    let rx = state.jobs.subscribe(&id);

    let (tx, out_rx) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        // 回放已有尾部（一次发一段，前端按行追加）
        if !job.stdout_tail.is_empty() {
            let ev = crate::jobs::JobEvent::Lines {
                text: job.stdout_tail.clone(),
            };
            if send_event(&tx, &ev).await.is_err() {
                return;
            }
        }

        // 已到终态：补一条 done 后收流，不必等增量
        let terminal = matches!(
            job.status.as_str(),
            "success" | "failed" | "cancelled" | "interrupted"
        );
        if terminal {
            let ev = crate::jobs::JobEvent::Done {
                status: job.status.clone(),
                exit_code: job.exit_code,
                error: job.error.clone(),
            };
            let _ = send_event(&tx, &ev).await;
            return;
        }

        let Some(mut rx) = rx else {
            return;
        };
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let done = matches!(ev, crate::jobs::JobEvent::Done { .. });
                    if send_event(&tx, &ev).await.is_err() {
                        return;
                    }
                    if done {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // 前端消费慢导致丢行：只告警，不断连（尾部仍完整）
                    tracing::warn!("作业 {id} 的流订阅落后 {n} 行");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(out_rx);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(axum::body::Body::from_stream(stream))
        .map_err(|_| ApiError::internal())
}

/// 把一条作业事件编码成 SSE 帧写入通道；接收端已断开时返回 Err
async fn send_event(
    tx: &tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::convert::Infallible>>,
    ev: &crate::jobs::JobEvent,
) -> Result<(), ()> {
    let Ok(body) = serde_json::to_string(ev) else {
        return Ok(());
    };
    tx.send(Ok(axum::body::Bytes::from(format!("data: {body}\n\n"))))
        .await
        .map_err(|_| ())
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/logout", post(logout))
        // 账号体系（2.x）：/me 与改密任何角色可用；用户/审计/踢会话 admin 专属；
        // 会话列表所有角色可用（handler 内按角色过滤可见范围）
        .route("/me", get(me))
        .route("/account/password", post(change_password))
        .route("/users", get(users_list).post(users_create))
        .route(
            "/users/{id}",
            axum::routing::put(users_update).delete(users_delete),
        )
        .route("/audit", get(audit_query))
        .route("/sessions", get(sessions_list))
        .route("/sessions/kick", post(sessions_kick))
        .route("/system/state", get(system_state))
        .route("/system/history", get(system_history))
        .route("/instances", get(instances_list))
        .route("/instances/{id}/files", get(instance_files))
        .route("/instances/{id}/file", get(instance_file_read))
        .route("/processes", get(processes_list).post(processes_kill))
        .route("/services", get(services_list).post(services_action))
        .route("/power", post(power_action))
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
        .route("/packages/meta", get(packages_meta))
        .route("/packages/upgradable", get(packages_upgradable))
        .route("/packages/search", get(packages_search))
        .route("/packages/action", post(packages_action))
        // P2-1 作业队列：长操作改由这里登记，输出走 SSE 增量推送。
        // SSE 单向，故取消另走 POST /jobs/{id}/cancel
        .route("/jobs", get(jobs_list).post(jobs_submit))
        .route("/jobs/{id}", get(jobs_get))
        .route("/jobs/{id}/cancel", post(jobs_cancel))
        .route("/jobs/{id}/stream", get(jobs_stream))
        .route(
            "/cron",
            get(cron_list)
                .post(cron_add)
                .put(cron_update)
                .delete(cron_delete),
        )
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
        .route("/docker/compose/action", post(docker_compose_action))
        // 4.3 容器日志流（WebSocket）。浏览器 WS 无法带 Authorization 头，
        // token 走查询串，handler 内做与 AuthUser 同口径的手动鉴权
        .route("/docker/logstream", get(docker_logstream_ws))
        // 4.2 仪表盘自定义：GET 读取（全员）、POST 保存（admin，白名单校验）
        .route(
            "/dashboard-config",
            get(dashboard_config_get).post(dashboard_config_set),
        )
        // 4.3 深度运维：unit 文件查看与 SMART 健康均为只读探测，全员可用
        .route("/services/unit", get(services_unit_get))
        .route("/disks/smart", get(disks_smart_get))
        // 主题定制：GET 读取、POST 保存。背景图以 data URL 内嵌，需放宽默认 2MB 请求体上限
        .route(
            "/theme",
            get(theme_get)
                .post(theme_set)
                .layer(DefaultBodyLimit::max(THEME_MAX_BYTES)),
        )
        // 3.1 备份管理（admin 专属）
        .route(
            "/backups",
            get(backups_list).post(backups_create).delete(backup_delete),
        )
        .route("/backups/download", get(backup_download))
        .route("/backups/restore", post(backup_restore))
        // 3.2 自更新（admin 专属）。上传通道需放宽请求体上限
        .route("/update/check", get(update_check))
        .route("/update/install", post(update_install))
        .route(
            "/update/upload",
            post(update_upload).layer(DefaultBodyLimit::max(UPDATE_MAX_BYTES)),
        )
        // 3.3 告警通知（admin 专属）
        .route(
            "/alerts/rules",
            get(alerts_rules_get).post(alerts_rules_set),
        )
        .route(
            "/alerts/webhook",
            get(alerts_webhook_get).post(alerts_webhook_set),
        )
        .route("/alerts/events", get(alerts_events))
        // 2.3：审计中间件挂在受保护路由上，记录所有非 GET 业务请求
        // （from_fn 不支持 State 提取器，必须用 from_fn_with_state）
        .layer(middleware::from_fn_with_state(state.clone(), audit_mw))
        // 4.5 AI 悬浮球助手：对话/历史 + 配置（人格提示词与技能说明）
        .route("/ai/chat", post(crate::ai::ai_chat))
        .route("/ai/history", get(crate::ai::ai_history))
        .route("/ai/history/clear", post(crate::ai::ai_history_clear))
        .route("/ai/config", get(ai_config_get).post(ai_config_set));
    Router::new()
        .route("/health", get(health))
        .route("/api/login", post(login))
        .nest("/api", protected)
        .fallback(crate::embed::handler)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::{
        mask_key, summarize_body, validate_ai_config, validate_dashboard_config, validate_theme,
        AI_BASE_MAX_LEN, AI_KEY_MAX_LEN, AI_MODEL_MAX_LEN,
    };
    use serde_json::json;

    // ---------- 4.2 仪表盘配置校验 ----------

    #[test]
    fn dashboard_valid_configs() {
        assert!(validate_dashboard_config(&json!({ "cards": ["cpu", "mem"] })).is_ok());
        assert!(
            validate_dashboard_config(&json!({ "cards": ["net", "disk", "mem", "cpu"] })).is_ok()
        );
    }

    #[test]
    fn dashboard_rejects_bad_configs() {
        // 非对象 / 缺字段 / cards 非数组 / 空数组
        assert!(validate_dashboard_config(&json!("x")).is_err());
        assert!(validate_dashboard_config(&json!({})).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": "cpu" })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": [] })).is_err());
        // 未知卡片 / 非字符串项 / 重复卡片 / 未知顶层字段
        assert!(validate_dashboard_config(&json!({ "cards": ["evil"] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": [1] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": ["cpu", "cpu"] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": ["cpu"], "evil": 1 })).is_err());
        // 数量超过白名单大小（复制白名单 + 1 也进不来，元素重复会先被拦）
        assert!(validate_dashboard_config(&json!({
            "cards": ["cpu", "mem", "disk", "net", "cpu", "mem", "disk", "net", "cpu"]
        }))
        .is_err());
    }

    /// 一份完全合法的主题配置（各用例以此为基底做破坏性修改）
    fn valid_config() -> serde_json::Value {
        json!({
            "version": 1,
            "name": "我的主题",
            "radius": 8,
            "colors": {
                "primary": "#409eff",
                "bg_page": "#141414",
                "bg_card": "#1F1F1F",
                "text": "#ffffff"
            },
            "bg_image": "data:image/png;base64,iVBORw0KGgo="
        })
    }

    /// 基底配置必须通过（防止后续用例在错误前提上"假绿"）
    #[test]
    fn theme_valid_config_accepted() {
        assert!(validate_theme(&valid_config()).is_ok());
        // 空对象没有可注入字段，按白名单语义放行
        assert!(validate_theme(&json!({})).is_ok());
    }

    /// 颜色：仅接受 7 字节 `#rrggbb`（十六进制大小写均可）；
    /// 名字色、缺 #、3 位缩写、超长、CSS 注入串一律拒绝
    #[test]
    fn theme_color_validation() {
        let ok = ["#000000", "#ffffff", "#FF00aa", "#1a2B3c"];
        for c in ok {
            let cfg = json!({ "colors": { "primary": c } });
            assert!(validate_theme(&cfg).is_ok(), "应接受合法颜色：{c}");
        }
        let bad = [
            "red",                     // 名字色
            "#FFF",                    // 3 位缩写
            "#FFFFF",                  // 5 位
            "#FFFFFFF",                // 8 位超长
            "FFFFFF",                  // 缺 #
            "#409eff;",                // 带分号（闭合属性注入）
            "red;} body{display:none", // CSS 逃逸注入串
            "#40 9eff",                // 含空格
            "#409egff",                // 非十六进制字符
        ];
        for c in bad {
            let cfg = json!({ "colors": { "primary": c } });
            assert!(validate_theme(&cfg).is_err(), "应拒绝非法颜色：{c:?}");
        }
        // 颜色值不是字符串 / colors 不是对象
        assert!(validate_theme(&json!({ "colors": { "primary": 1 } })).is_err());
        assert!(validate_theme(&json!({ "colors": "#000000" })).is_err());
    }

    /// colors 子字段同样走白名单：未知键拒绝（防注入任意 CSS 属性名）
    #[test]
    fn theme_unknown_color_key_rejected() {
        assert!(validate_theme(&json!({
            "colors": { "background-image": "url(javascript:alert(1))" }
        }))
        .is_err());
    }

    /// radius：数值 0..=64 闭区间；边界外与非数字拒绝
    #[test]
    fn theme_radius_bounds() {
        assert!(validate_theme(&json!({ "radius": 0 })).is_ok());
        assert!(validate_theme(&json!({ "radius": 64 })).is_ok());
        assert!(validate_theme(&json!({ "radius": -1 })).is_err());
        assert!(validate_theme(&json!({ "radius": 65 })).is_err());
        assert!(validate_theme(&json!({ "radius": 64.5 })).is_err());
        assert!(validate_theme(&json!({ "radius": "8" })).is_err());
        assert!(validate_theme(&json!({ "radius": true })).is_err());
        // NaN/Infinity 在 JSON 里无法表达，但极大值同样越界
        assert!(validate_theme(&json!({ "radius": 1e10 })).is_err());
    }

    /// bg_image：仅 data:image/ 前缀；含引号/括号/反斜杠/控制字符
    /// （可闭合 CSS url("...") 构成注入逃逸）一律拒绝
    #[test]
    fn theme_bg_image_validation() {
        assert!(validate_theme(&json!({
            "bg_image": "data:image/png;base64,iVBORw0KGgo="
        }))
        .is_ok());
        assert!(
            validate_theme(&json!({
                "bg_image": "data:image/svg+xml;utf8,<svg></svg>"
            }))
            .is_ok(),
            "合法 data URL 放行"
        );
        let bad = [
            "http://evil.example/x.png",       // 非 data: 前缀（外链跟踪）
            "javascript:alert(1)",             // 协议注入
            "data:application/html;base64,xx", // 非 image 类型
            "data:image/png,url(\"x\")",       // 含双引号
            "data:image/png,v(1)",             // 含右括号
            "data:image/png,\\escape",         // 含反斜杠
            "data:image/png,\u{1}ctrl",        // 含控制字符
        ];
        for s in bad {
            let cfg = json!({ "bg_image": s });
            assert!(validate_theme(&cfg).is_err(), "应拒绝非法 bg_image：{s:?}");
        }
        assert!(validate_theme(&json!({ "bg_image": 123 })).is_err());
    }

    /// 顶层字段白名单与类型：未知字段、非对象、name/version 类型与长度
    #[test]
    fn theme_whitelist_and_types() {
        // 未知顶层字段（即使值合法）也拒绝——白名单而非黑名单
        assert!(validate_theme(&json!({ "evil_field": "x" })).is_err());
        // 配置必须是对象
        assert!(validate_theme(&json!("string")).is_err());
        assert!(validate_theme(&json!([1, 2])).is_err());
        // name：字符串且 ≤64 字符
        assert!(validate_theme(&json!({ "name": 123 })).is_err());
        assert!(validate_theme(&json!({ "name": "a".repeat(64) })).is_ok());
        assert!(validate_theme(&json!({ "name": "a".repeat(65) })).is_err());
        // version：必须是数字
        assert!(validate_theme(&json!({ "version": "1" })).is_err());
        assert!(validate_theme(&json!({ "version": 2 })).is_ok());
    }

    // ---------- AI API 配置校验（设置页） ----------

    /// 三项全空 = 全部回退环境变量/默认值，必须放行
    #[test]
    fn ai_config_empty_passes() {
        assert!(validate_ai_config("", "", "").is_ok());
    }

    /// 地址：只接受 http(s):// 开头，其余一律拒绝
    #[test]
    fn ai_config_base_validation() {
        for b in ["https://api.openai.com/v1", "http://127.0.0.1:8000/v1"] {
            assert!(validate_ai_config(b, "", "").is_ok(), "应接受合法地址：{b}");
        }
        for b in ["ftp://evil", "api.example.com/v1", "javascript:alert(1)", "//x"] {
            assert!(validate_ai_config(b, "", "").is_err(), "应拒绝非法地址：{b:?}");
        }
    }

    /// 长度上限：超限拒绝，上限值本身放行
    #[test]
    fn ai_config_length_limits() {
        let base_ok = format!("https://a{}", "b".repeat(AI_BASE_MAX_LEN - 10));
        assert!(validate_ai_config(&base_ok, "", "").is_ok());
        let base_bad = format!("https://{}", "a".repeat(AI_BASE_MAX_LEN));
        assert!(validate_ai_config(&base_bad, "", "").is_err());
        assert!(validate_ai_config("", &"k".repeat(AI_KEY_MAX_LEN), "").is_ok());
        assert!(validate_ai_config("", &"k".repeat(AI_KEY_MAX_LEN + 1), "").is_err());
        assert!(validate_ai_config("", "", &"m".repeat(AI_MODEL_MAX_LEN)).is_ok());
        assert!(validate_ai_config("", "", &"m".repeat(AI_MODEL_MAX_LEN + 1)).is_err());
    }

    /// 密钥脱敏：长密钥保留末 4 位，短密钥全遮，绝不出现完整明文
    #[test]
    fn ai_config_key_masking() {
        assert_eq!(mask_key("sk-1234567890abcdef"), "••••cdef");
        // 8 字符恰好保留末 4 位
        assert_eq!(mask_key("12345678"), "••••5678");
        // 7 字符及以下全遮
        assert_eq!(mask_key("short"), "••••");
        assert_eq!(mask_key(""), "••••");
        let m = mask_key("sk-1234567890abcdef");
        assert!(!m.contains("sk-"), "脱敏结果不得包含明文前缀：{m}");
    }

    /// P1-3：审计摘要只取白名单字段；敏感字段进不来；值截断到 200 字符
    #[test]
    fn audit_detail_summary_is_whitelisted() {
        // 白名单命中：action 与 names（数组取前几项）
        let body = br#"{"action":"install","names":["docker","git"],"extra":"ignored"}"#;
        let d = summarize_body(body).unwrap();
        assert!(d.contains("action=install"), "{d}");
        assert!(d.contains("names=docker,git"), "{d}");
        assert!(!d.contains("extra"), "白名单外的字段不得入库：{d}");

        // 敏感字段一律剔除（白名单只认键名，这里是叠加的第二道防线）
        let leaked = br#"{"name":"ok","password":"hunter2","token":"abc","secret":"s"}"#;
        assert_eq!(
            summarize_body(leaked).unwrap(),
            "name=ok",
            "敏感字段必须被剔除"
        );

        // 超长值截断到 200 字符
        let long = format!(r#"{{"path":"{}"}}"#, "a".repeat(500));
        let d = summarize_body(long.as_bytes()).unwrap();
        assert_eq!(d.len(), "path=".len() + 200, "值必须截断");

        // 非 JSON、非对象、无白名单字段、空数组 → 不产生摘要
        assert!(summarize_body(b"not json").is_none());
        assert!(summarize_body(b"[1,2,3]").is_none());
        assert!(summarize_body(br#"{"other":1}"#).is_none());
        assert!(summarize_body(br#"{"names":[]}"#).is_none(), "空数组不记");
    }
}
