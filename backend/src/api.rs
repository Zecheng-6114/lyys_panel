use axum::extract::{
    ConnectInfo, DefaultBodyLimit, FromRequest, FromRequestParts, Multipart, OriginalUri, Path,
    Query, Request, State,
};
// 4.3 容器日志流：WebSocket 升级提取器
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{header, request::Parts, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
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
    if trust_proxy()
        && let Some(ip) = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse::<IpAddr>().ok())
        {
            return ip;
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
    let password = req.password;
    let hash_for_verify = row.password_hash;
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
    let pwd = req.password;
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
    if let Some(existing) = state.db.find_user_async(&req.username).await?
        && existing.0 != id {
            return Err(ApiError::bad("用户名已存在"));
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
    /// 实例 id（container:<短ID> / service:<单元名>）；给了就只看该实例的进程
    instance: Option<String>,
}

async fn processes_list(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ProcessesQuery>,
) -> Result<Json<Vec<monitor::ProcessInfo>>, ApiError> {
    Ok(Json(rprocess::list(&state, q.instance.as_deref()).await?))
}

/// 实例列表：容器 + systemd 服务，作为各自日志/进程的入口
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

/// 从实例 id 取容器短 ID。服务等非容器实例没有独立文件系统，
/// 明确报错而不是静默退化成宿主机路径——那会让人以为在看实例里的东西。
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

async fn files_compress(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let archive = crate::files::compress(&req.path)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "path": archive })))
}

#[derive(Deserialize)]
struct ExtractReq {
    path: String,
    /// 解压目标目录，缺省为压缩包所在目录
    dest: Option<String>,
}

async fn files_extract(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<ExtractReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let dir = crate::files::extract(&req.path, req.dest.as_deref())
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "path": dir })))
}

#[derive(Deserialize)]
struct ChmodReq {
    path: String,
    mode: String,
}

async fn files_chmod(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<ChmodReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::chmod(&req.path, &req.mode)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ChownReq {
    path: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    group: String,
}

async fn files_chown(
    _: RequireRole<1>,
    SafeJson(req): SafeJson<ChownReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::files::chown(&req.path, &req.owner, &req.group)
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
    let Some(kw) = q.filter.filter(|s| !s.is_empty()) else {
        return Err(ApiError::bad("请输入搜索关键字"));
    };
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

// ---------- systemd 定时器（计划任务升级，operator 起） ----------

async fn timers_list(
    State(state): State<AppState>,
    _: AuthUser,
) -> Result<Json<Vec<crate::timers::TimerJob>>, ApiError> {
    let data_dir = state.data_dir.clone();
    let jobs = tokio::task::spawn_blocking(move || crate::timers::list(&data_dir))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(jobs))
}

#[derive(Deserialize)]
struct TimerReq {
    /// 新建时留空 → 服务端分配标识；更新时必填
    id: Option<String>,
    schedule: String,
    command: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    enabled: bool,
}

async fn timers_save(
    State(state): State<AppState>,
    _: RequireRole<1>,
    SafeJson(req): SafeJson<TimerReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    let mut job = crate::timers::TimerJob {
        id: req.id.unwrap_or_default(),
        schedule: req.schedule,
        command: req.command,
        description: req.description,
        enabled: req.enabled,
    };
    let is_new = job.id.is_empty();
    if is_new {
        let dd = data_dir.clone();
        job.id = tokio::task::spawn_blocking(move || crate::timers::new_id(&crate::timers::list(&dd)))
            .await
            .map_err(|_| ApiError::internal())?;
    }
    let id = job.id.clone();
    crate::timers::save(&data_dir, &job, is_new)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct TimerIdReq {
    id: String,
}

async fn timers_delete(
    State(state): State<AppState>,
    _: RequireRole<1>,
    SafeJson(req): SafeJson<TimerIdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    crate::timers::delete(&data_dir, &req.id)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn timers_run(
    State(state): State<AppState>,
    _: RequireRole<1>,
    SafeJson(req): SafeJson<TimerIdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    crate::timers::run_now(&data_dir, &req.id)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "已触发执行，稍后可在日志中查看结果"
    })))
}

#[derive(Deserialize)]
struct TimerLogsQuery {
    id: String,
    lines: Option<usize>,
}

async fn timers_logs(
    _: RequireRole<1>,
    Query(q): Query<TimerLogsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let text = crate::timers::logs(&q.id, q.lines.unwrap_or(200))
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "logs": text })))
}

// ---------- 网站管理（Nginx，admin 专属） ----------

async fn websites_status(_: RequireRole<2>) -> Result<Json<crate::websites::Status>, ApiError> {
    Ok(Json(crate::websites::status().await))
}

async fn websites_list(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<Vec<crate::websites::Site>>, ApiError> {
    let data_dir = state.data_dir.clone();
    let sites = tokio::task::spawn_blocking(move || crate::websites::list(&data_dir))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(sites))
}

#[derive(Deserialize)]
struct SiteReq {
    /// 新建时留空 → 服务端分配标识；更新时必填
    id: Option<String>,
    name: String,
    kind: crate::websites::SiteKind,
    #[serde(default)]
    root: String,
    #[serde(default)]
    upstream: String,
    listen: u16,
    #[serde(default)]
    tls: bool,
    /// 启用 Let's Encrypt 自动证书（隐含 tls=true）
    #[serde(default)]
    acme: bool,
    #[serde(default)]
    enabled: bool,
    /// 本次上传的证书 PEM（留空 = 沿用已存文件；TLS 且无文件时自动生成自签证书）
    #[serde(default)]
    cert_pem: String,
    /// 本次上传的私钥 PEM
    #[serde(default)]
    key_pem: String,
}

async fn websites_save(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<SiteReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    let mut site = crate::websites::Site {
        id: req.id.unwrap_or_default(),
        name: req.name,
        kind: req.kind,
        root: req.root,
        upstream: req.upstream,
        listen: req.listen,
        // 自动证书必须有 HTTPS 承载，勾选即隐含开启 TLS
        tls: req.tls || req.acme,
        acme: req.acme,
        enabled: req.enabled,
    };
    let is_new = site.id.is_empty();
    if is_new {
        let dd = data_dir.clone();
        site.id =
            tokio::task::spawn_blocking(move || crate::websites::new_id(&crate::websites::list(&dd)))
                .await
                .map_err(|_| ApiError::internal())?;
    }
    // 证书与私钥必须成对提供，避免只换一半导致证书与私钥不匹配
    let new_cert = match (req.cert_pem.trim().is_empty(), req.key_pem.trim().is_empty()) {
        (true, true) => None,
        (false, false) => Some((req.cert_pem, req.key_pem)),
        _ => return Err(ApiError::bad("证书与私钥需同时提供")),
    };
    let id = site.id.clone();
    crate::websites::save(&data_dir, &site, is_new, new_cert)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct SiteIdReq {
    id: String,
}

async fn websites_delete(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<SiteIdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    crate::websites::delete(&data_dir, &req.id)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- Let's Encrypt 自动证书（ACME，admin 专属） ----------

async fn acme_settings_get(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<crate::acme::AcmeSettings>, ApiError> {
    let db = state.db.clone();
    let s = tokio::task::spawn_blocking(move || crate::acme::load_settings(&db))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(s))
}

async fn acme_settings_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<crate::acme::AcmeSettings>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let email = req.email.trim().to_string();
    // 留空表示不提交 contact；填了就必须是邮箱形态，否则 CA 会拒单
    if !email.is_empty()
        && (!email.contains('@') || email.chars().any(char::is_whitespace) || email.len() > 254)
    {
        return Err(ApiError::bad("邮箱格式不正确"));
    }
    let settings = crate::acme::AcmeSettings {
        email,
        staging: req.staging,
    };
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || crate::acme::save_settings(&db, &settings))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 立即为站点申请 / 续期证书（首次签发走这里；之后由后台任务按到期自动续）
async fn acme_issue(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<SiteIdReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    let id = req.id.clone();
    let dd = data_dir.clone();
    let site = tokio::task::spawn_blocking(move || {
        crate::websites::list(&dd).into_iter().find(|s| s.id == id)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .ok_or_else(|| ApiError::bad("站点不存在"))?;

    let db = state.db.clone();
    let settings = tokio::task::spawn_blocking(move || crate::acme::load_settings(&db))
        .await
        .map_err(|_| ApiError::internal())?;

    crate::websites::issue_acme(&data_dir, &site, &settings)
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

// ---------- 防火墙管理 ----------
//
// 只驱动发行版自带的防火墙前端（Debian 系 ufw / Arch 系 firewalld），
// 不接管 iptables/nftables 裸规则。改规则与启停都会改变主机网络暴露面，
// 属危险操作，读写一律 admin 专属。

async fn firewall_status(_: RequireRole<2>) -> Result<Json<crate::firewall::Status>, ApiError> {
    crate::firewall::status()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

async fn firewall_rules(_: RequireRole<2>) -> Result<Json<Vec<crate::firewall::Rule>>, ApiError> {
    crate::firewall::rules()
        .await
        .map(Json)
        .map_err(ApiError::file_err)
}

#[derive(Deserialize)]
struct FirewallAddReq {
    op: crate::firewall::Op,
    #[serde(default)]
    port: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    from: String,
}

async fn firewall_rule_add(
    _: RequireRole<2>,
    SafeJson(req): SafeJson<FirewallAddReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::firewall::add(req.op, &req.port, &req.protocol, &req.from)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct FirewallRemoveReq {
    id: String,
}

async fn firewall_rule_remove(
    _: RequireRole<2>,
    SafeJson(req): SafeJson<FirewallRemoveReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::firewall::remove(&req.id)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct FirewallToggleReq {
    enable: bool,
}

async fn firewall_toggle(
    _: RequireRole<2>,
    SafeJson(req): SafeJson<FirewallToggleReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::firewall::toggle(req.enable)
        .await
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
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
/// - 允许字段：version / name / radius / shadow / colors{primary,bg_page,bg_card,text} /
///   bg_image / bg_zoom / bg_x / bg_y / card_opacity / card_blur
/// - 颜色：必须为 `#rrggbb`（6 位十六进制，带 #）
/// - radius：数值 0..=64
/// - shadow：布尔，false = 关闭贴面卡片/内容块的投影（缺省视为 true）；
///   只作用于贴面那一档，对话框与下拉菜单等浮层的投影不受它控制
/// - bg_image：必须以 `data:image/` 开头，且不含引号/括号/反斜杠/控制字符
///   （这些字符可闭合 CSS 的 `url("...")` 字符串，构成样式注入逃逸）
/// - card_opacity：仪表盘卡片底色的不透明度（百分数）30..=100；
///   只影响仪表盘那张卡片产出的 `--panel-card-bg`，其余面板不受它控制
/// - card_blur：仪表盘卡片的高斯模糊半径（px）0..=40，产出 `--panel-card-blur`
///   （backdrop-filter 的磨砂玻璃）
///
/// 观感只有两个字段：不透明度（card_opacity）与模糊（card_blur）。两者都作用于
/// 整套界面表面 —— 仪表盘卡片与应用外壳（侧栏 / 顶栏 / 内容区底板）共用同一个
/// `--panel-card-bg` 与 `--panel-card-blur`，不额外提供「只改外壳」的开关。
/// 曾短暂存在的 chrome_opacity 已移除：多一个独立旋钮只会做出割裂的层次。
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
            "shadow" => {
                if v.as_bool().is_none() {
                    return Err("shadow 必须是布尔值".into());
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
            "bg_zoom" => {
                let z = v.as_f64().ok_or("bg_zoom 必须是数字")?;
                if !(100.0..=300.0).contains(&z) {
                    return Err("bg_zoom 必须在 100..300 之间".into());
                }
            }
            "bg_x" | "bg_y" => {
                let n = v.as_f64().ok_or(format!("{k} 必须是数字"))?;
                if !(0.0..=100.0).contains(&n) {
                    return Err(format!("{k} 必须在 0..100 之间"));
                }
            }
            "card_opacity" => {
                let n = v.as_f64().ok_or("card_opacity 必须是数字")?;
                // 下限 30：再低卡片与它下面的背景图糊成一片，卡上读数失去依托
                if !(30.0..=100.0).contains(&n) {
                    return Err("card_opacity 必须在 30..100 之间".into());
                }
            }
            "card_blur" => {
                let n = v.as_f64().ok_or("card_blur 必须是数字")?;
                // 上限 40：采样半径超过卡片短边后只剩一片均匀色块，磨砂感反而没了
                if !(0.0..=40.0).contains(&n) {
                    return Err("card_blur 必须在 0..40 之间".into());
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

// ---------- 界面字体（可选，不随包分发） ----------

/// settings 表中界面字体配置的键名（值为前端序列化的 JSON 字符串）。
///
/// 面板不再内嵌第三方字体：CJK 全字库一个文件就近 8MB，多字重会让二进制凭空
/// 多出几十 MB，而它只在用户想要统一界面字体时才有意义。默认走系统字体栈，
/// 需要的用户在设置页自行上传字体文件（并自行遵守该字体的许可协议）。
const FONT_KEY: &str = "ui_font";
/// 单个字体文件上限。CJK 全字库 TTF 常见 8~15MB，留足余量取 24MB。
const FONT_MAX_BYTES: usize = 24 * 1024 * 1024;
/// 自定义字体在数据目录下的子目录名
const FONT_DIR: &str = "fonts";

/// 字体文件名白名单：`<16 位小写十六进制>.<扩展名>`。
///
/// 文件名一律由服务端在入库时生成（见 [`font_upload`]），这里只做复核。
/// 关键在于文件名里不含 `.`（除扩展名外）与路径分隔符，于是
/// `data_dir/fonts/<name>` 无论如何都拼不出 `..` 或绝对路径。
fn valid_font_file(name: &str) -> bool {
    let (stem, ext) = match name.rsplit_once('.') {
        Some(v) => v,
        None => return false,
    };
    stem.len() == 16
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && matches!(ext, "ttf" | "otf" | "woff" | "woff2")
}

/// 字体名白名单：这个名字会被拼进注入页面的 `font-family`。引号/分号/花括号/
/// 反斜杠/圆括号/逗号/控制字符可把字符串闭合出去构成样式注入，一律拒绝
/// （与主题的 validate_theme 同一套思路，双端各校验一次）。
fn valid_font_family(s: &str) -> bool {
    if s.is_empty() || s.chars().count() > 64 {
        return false;
    }
    !s.chars().any(|c| {
        matches!(c, '"' | '\'' | ';' | '{' | '}' | '\\' | '(' | ')' | ',') || (c as u32) < 0x20
    })
}

/// 字体配置字段白名单校验。允许字段：version / family / faces[{weight,file}]。
/// 非 null 的配置必须同时给出 family 与非空 faces —— 缺任何一半，前端都拼不出
/// 可用的 @font-face，存下去只会得到一份「设置了但没生效」的配置。
fn validate_font(cfg: &serde_json::Value) -> Result<(), String> {
    let obj = cfg.as_object().ok_or_else(|| "字体配置必须是对象".to_string())?;
    for (k, v) in obj {
        match k.as_str() {
            "version" => {
                if v.as_number().is_none() {
                    return Err("version 必须是数字".into());
                }
            }
            "family" => {
                let s = v.as_str().ok_or("family 必须是字符串")?;
                if !valid_font_family(s) {
                    return Err("字体名称含不允许的字符或为空（上限 64 字符）".into());
                }
            }
            "faces" => {
                let arr = v.as_array().ok_or("faces 必须是数组")?;
                if arr.len() > 10 {
                    return Err("字重文件过多（上限 10 个）".into());
                }
                for f in arr {
                    let fo = f.as_object().ok_or("faces 元素必须是对象")?;
                    for key in fo.keys() {
                        if !matches!(key.as_str(), "weight" | "file") {
                            return Err(format!("faces 含未知字段：{key}"));
                        }
                    }
                    let w = fo
                        .get("weight")
                        .and_then(|x| x.as_u64())
                        .ok_or("weight 必须是整数")?;
                    if !(100..=900).contains(&w) {
                        return Err("weight 必须在 100..900 之间".into());
                    }
                    let file = fo
                        .get("file")
                        .and_then(|x| x.as_str())
                        .ok_or("file 必须是字符串")?;
                    if !valid_font_file(file) {
                        return Err("字体文件名不合法".into());
                    }
                }
            }
            _ => return Err(format!("未知字段：{k}")),
        }
    }
    let faces_ok = obj
        .get("faces")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    if obj.get("family").is_none() || !faces_ok {
        return Err("字体配置必须包含 family 与非空的 faces".into());
    }
    Ok(())
}

/// 取出配置引用的字体文件名（用于存在性校验与旧文件清理）；null/损坏返回空表
fn font_files(cfg: &serde_json::Value) -> Vec<String> {
    cfg.get("faces")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|f| f.get("file").and_then(|x| x.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 按文件头判定字体类型。只认扩展名白名单里的四种；识别不出的一律拒绝，
/// 避免把任意文件塞进字体目录后由公开路由 /fonts/custom/* 原样吐出去。
fn detect_font_ext(b: &[u8]) -> Option<&'static str> {
    match b {
        [0x77, 0x4F, 0x46, 0x32, ..] => Some("woff2"),
        [0x77, 0x4F, 0x46, 0x46, ..] => Some("woff"),
        [0x4F, 0x54, 0x54, 0x4F, ..] => Some("otf"),
        // 0x00010000 与 "true"（Apple 版）都是 TTF 的合法文件头
        [0x00, 0x01, 0x00, 0x00, ..] | [0x74, 0x72, 0x75, 0x65, ..] => Some("ttf"),
        _ => None,
    }
}

/// 删除字体目录中不再被配置引用的文件。上传与配置保存是两个动作，用户换字体
/// 时旧文件不会自动消失；不清理就会在数据目录里越攒越多（那台机器可能只有
/// 1GB 盘）。目录不存在视为无可清理，不报错。
async fn prune_fonts(dir: &std::path::Path, keep: &[String]) -> anyhow::Result<()> {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.iter().any(|k| k == &name)
            && let Err(e) = tokio::fs::remove_file(entry.path()).await
        {
            tracing::warn!("删除未引用字体文件 {name} 失败：{e}");
        }
    }
    Ok(())
}

/// 读取字体配置；未定制过（或用系统字体）时返回 null
async fn font_get(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let raw = state.db.get_setting_async(FONT_KEY).await?;
    let value = match raw {
        Some(s) => {
            let v =
                serde_json::from_str::<serde_json::Value>(&s).unwrap_or(serde_json::Value::Null);
            if !v.is_null() && validate_font(&v).is_err() {
                tracing::warn!("忽略库中未通过安全校验的字体配置");
                serde_json::Value::Null
            } else {
                v
            }
        }
        None => serde_json::Value::Null,
    };
    Ok(Json(serde_json::json!({ "config": value })))
}

/// 保存字体配置（admin）。传 null 表示清除自定义字体、恢复系统字体，
/// 同时会把已上传的字体文件一并清掉。
async fn font_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let text =
        String::from_utf8(body.to_vec()).map_err(|_| ApiError::bad("字体配置必须是 UTF-8 JSON"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| ApiError::bad("字体配置 JSON 无法解析"))?;
    let dir = state.data_dir.join(FONT_DIR);
    if !value.is_null() {
        validate_font(&value).map_err(ApiError::bad)?;
        // 引用的文件必须都已落盘，否则存下去就是一份永远加载不出字体的配置
        for file in font_files(&value) {
            if tokio::fs::metadata(dir.join(&file)).await.is_err() {
                return Err(ApiError::bad(format!("字体文件 {file} 不存在，请重新上传")));
            }
        }
    }
    let stored = if value.is_null() { "" } else { &value.to_string() };
    state.db.set_setting_async(FONT_KEY, stored).await?;
    // 落库成功后再清理，避免配置保存失败却先删了文件
    let keep = font_files(&value);
    if let Err(e) = prune_fonts(&dir, &keep).await {
        tracing::warn!("清理未引用字体文件失败：{e:#}");
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 上传单个字重文件：multipart 字段 `file`。
///
/// 文件名取内容 sha256 前 16 位、扩展名由文件头判定（用户给的文件名不可信）。
/// 内容相同即同名，重复上传天然去重；文件名与内容绑定，于是
/// [`font_serve`] 可以放心用 immutable 强缓存。
async fn font_upload(
    State(state): State<AppState>,
    _: RequireRole<2>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(field) = mp.next_field().await.map_err(|e| {
        tracing::warn!("字体上传解析失败：{e}");
        ApiError::bad("上传请求格式错误")
    })? {
        if field.name() == Some("file") {
            bytes = field
                .bytes()
                .await
                .map_err(|e| {
                    tracing::warn!("字体上传字段 file 读取失败：{e}");
                    ApiError::bad("上传数据读取失败")
                })?
                .to_vec();
        }
    }
    if bytes.is_empty() {
        return Err(ApiError::bad("缺少 file 字段"));
    }
    if bytes.len() > FONT_MAX_BYTES {
        return Err(ApiError::bad(format!(
            "字体文件过大（上限 {}MB）",
            FONT_MAX_BYTES / 1024 / 1024
        )));
    }
    let ext = detect_font_ext(&bytes)
        .ok_or_else(|| ApiError::bad("不是可识别的字体文件（支持 ttf/otf/woff/woff2）"))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let mut hex = String::with_capacity(16);
    for b in &digest.as_ref()[..8] {
        hex.push_str(&format!("{b:02x}"));
    }
    let name = format!("{hex}.{ext}");
    let dir = state.data_dir.join(FONT_DIR);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        tracing::error!("创建字体目录失败：{e:#}");
        ApiError::internal()
    })?;
    tokio::fs::write(dir.join(&name), &bytes).await.map_err(|e| {
        tracing::error!("写入字体文件失败：{e:#}");
        ApiError::internal()
    })?;
    Ok(Json(
        serde_json::json!({ "ok": true, "file": name, "size": bytes.len() }),
    ))
}

/// 自定义字体文件服务：@font-face 的请求由浏览器直接发出，带不上 Authorization
/// 头，所以这条路由挂在鉴权之外（与 /assets/* 同一暴露面）。文件名是内容哈希且
/// 过了白名单复核 —— 既不敏感，也猜不到。
async fn font_serve(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if !valid_font_file(&name) {
        return (StatusCode::NOT_FOUND, "字体不存在").into_response();
    }
    match tokio::fs::read(state.data_dir.join(FONT_DIR).join(&name)).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&name).first_or_octet_stream();
            (
                [
                    (
                        header::CONTENT_TYPE,
                        HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| {
                            HeaderValue::from_static("application/octet-stream")
                        }),
                    ),
                    (
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("public, max-age=31536000, immutable"),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "字体不存在").into_response(),
    }
}

// ---------- 4.2 仪表盘自定义 ----------

/// settings 表中仪表盘配置的键名
const DASHBOARD_KEY: &str = "dashboard_config";
/// 配置 JSON 字节上限：结构极小（一个字符串数组），4KB 已远超需要，纯防滥用
const DASHBOARD_MAX_BYTES: usize = 4 * 1024;

/// 仪表盘卡片白名单（与前端 stores/dashboard.ts 的 DASH_CARDS 一一对应）
///
/// 顺序即默认展示顺序，按「占用 / 活动 / 机器」三行排：前三行分别回答
/// 「还剩多少」「现在在忙什么」「这是台什么机器」。
const DASHBOARD_CARDS: [&str; 13] = [
    "cpu", "mem", "disk", "swap", "diskio", "net", "load", "procs", "partitions", "uptime",
    "cores", "sysinfo", "chart",
];

/// 卡片可跨的最大列数 / 行数。
///
/// 列上限是栅格整宽（4 列）—— 趋势图这类需要横向空间的卡片要占满一行；
/// 行上限 3：行高固定 88px，3 行约 288px，数值卡再多出来的只是空白。
/// 🔴 必须与前端 `stores/dashboard.ts` 的栅格常量保持一致。
const MAX_CARD_W: u64 = 4;
const MAX_CARD_H: u64 = 3;

/// 趋势图卡单独放宽的高度上限（行）：折线图越高越好读，3 行太局促。
/// 🔴 与前端 `stores/dashboard.ts` 的 `MAX_CARD_H_CHART` 同源。
const MAX_CARD_H_CHART: u64 = 6;

/// 仪表盘配置校验（复用 P1-2 主题校验思路：白名单 + 类型 + 长度）：
/// - 只允许一个顶层字段 cards；
/// - cards 为非空数组，长度 ≤ 白名单大小，元素在白名单内且不重复；
/// - 元素可以是字符串（旧格式，等价于默认 1×1），或 `{id, w, h}` 对象。
///
/// 尺寸是后加的，**必须继续接受纯字符串写法** —— 否则老用户已存的配置
/// 会在下次保存前就被判非法。
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
        // 卡片项兼容两种写法：字符串（最老格式）、{id,w,h}。尺寸是后加的，
        // 老配置必须一直有效；布局由数组顺序决定，不接受显式坐标。
        let id = match c {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Object(o) => {
                for k in o.keys() {
                    if k != "id" && k != "w" && k != "h" {
                        return Err(format!("卡片项含未知字段：{k}"));
                    }
                }
                let id = o
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "卡片项缺少 id".to_string())?;
                // 缺省即 1×1：前端只在用户改过尺寸时才写出 w/h。
                // 但写了就必须是整数 —— 类型不对属客户端 bug，静默按默认
                // 处理会把错误藏起来，与项目其余校验的严格口径一致。
                let w = match o.get("w") {
                    Some(v) => v.as_u64().ok_or_else(|| "卡片宽度必须是整数".to_string())?,
                    None => 1,
                };
                let h = match o.get("h") {
                    Some(v) => v.as_u64().ok_or_else(|| "卡片高度必须是整数".to_string())?,
                    None => 1,
                };
                if !(1..=MAX_CARD_W).contains(&w) {
                    return Err(format!("卡片宽度超出范围（1–{MAX_CARD_W}）：{id}"));
                }
                // 高度上限按卡片类型给：趋势图能占更多行
                let max_h = if id == "chart" {
                    MAX_CARD_H_CHART
                } else {
                    MAX_CARD_H
                };
                if !(1..=max_h).contains(&h) {
                    return Err(format!("卡片高度超出范围（1–{max_h}）：{id}"));
                }
                id
            }
            _ => return Err("卡片项必须是字符串或 {id,w,h} 对象".to_string()),
        };
        if !DASHBOARD_CARDS.contains(&id) {
            return Err(format!("未知卡片：{id}"));
        }
        if !seen.insert(id.to_string()) {
            return Err(format!("卡片重复：{id}"));
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
/// `search_base` 允许为空（空 = 用内置搜索通道），非空时同样要求 http(s)。
fn validate_ai_config(
    base: &str,
    key: &str,
    model: &str,
    search_base: &str,
) -> Result<(), String> {
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
    if search_base.len() > AI_BASE_MAX_LEN {
        return Err("联网搜索地址过长".into());
    }
    if !search_base.is_empty()
        && !search_base.starts_with("http://")
        && !search_base.starts_with("https://")
    {
        return Err("联网搜索地址必须以 http(s):// 开头".into());
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
            // 生效的联网搜索地址（可能来自环境变量）；空串表示走内置 Bing/DuckDuckGo 通道
            "search_base": crate::ai::resolve_search_base(&stored).unwrap_or_default(),
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
        search_base: Option<String>,
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
    if let Some(v) = req.search_base {
        stored.search_base = v.trim().to_string();
    }
    validate_ai_config(&stored.base, &stored.key, &stored.model, &stored.search_base)
        .map_err(ApiError::bad)?;
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
/// 成功时返回该账号的角色，供调用方做更细的权限判定（如终端仅限 admin）。
async fn ws_auth(state: &AppState, token: &str) -> Result<String, ApiError> {
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
    // user = (用户名, 角色, 是否强改密)；角色回给调用方做权限判定
    Ok(user.1)
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

#[derive(Deserialize)]
struct TerminalQuery {
    token: String,
    /// 前端上报的终端尺寸（列/行），缺省用 80×24
    cols: Option<u16>,
    rows: Option<u16>,
}

/// 交互式终端（WebSocket）：仅 admin。鉴权与日志流同口径（token 走查询串）。
/// 之所以单独限制角色：终端是任意命令执行，权限级别与电源操作对齐，不下放给
/// viewer/operator。
async fn terminal_ws(
    State(state): State<AppState>,
    Query(q): Query<TerminalQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let role = ws_auth(&state, &q.token).await?;
    if role != "admin" {
        return Err(ApiError::forbidden("终端功能仅限管理员"));
    }
    // 尺寸按合理区间夹紧，防止超大行列数拖垮前端渲染与内核
    let cols = q.cols.unwrap_or(80).clamp(20, 500);
    let rows = q.rows.unwrap_or(24).clamp(5, 300);
    Ok(ws.on_upgrade(move |socket| crate::terminal::bridge(socket, cols, rows)))
}

// ---------- 3.1 备份管理（admin）----------

/// 导入备份体积上限（主库通常几百 KB ～ 几十 MB）
const BACKUP_MAX_BYTES: usize = 200 * 1024 * 1024;

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
    // 远端已启用则顺带投递一份；失败只记日志，不影响本地备份
    if let Err(e) = crate::remote::upload_backup(&state.data_dir, &name).await {
        tracing::warn!("备份远端投递失败：{e:#}");
    }
    Ok(Json(serde_json::json!({ "ok": true, "name": name })))
}

async fn backup_download(
    State(state): State<AppState>,
    _: RequireRole<2>,
    Query(q): Query<BackupNameQuery>,
) -> Result<Response, ApiError> {
    let data_dir = state.data_dir.clone();
    let name = q.name;
    let name_for_check = name.clone();
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

#[derive(Deserialize, Default)]
struct RemoteConfigReq {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    url: String,
    #[serde(default)]
    username: String,
    /// 留空 = 沿用已存密码，避免「未改动即被清空」
    #[serde(default)]
    password: String,
    #[serde(default)]
    encrypt: bool,
    /// 留空 = 沿用已存口令
    #[serde(default)]
    passphrase: String,
}

#[derive(Deserialize)]
struct BackupConfigReq {
    #[serde(default)]
    dir: String,
    keep: usize,
    #[serde(default)]
    remote: RemoteConfigReq,
}

/// 配置的对外视图：远端密码/口令只回「是否已设置」，绝不回明文（安全基线 §1）
fn backup_config_view(cfg: &crate::backup::BackupConfig) -> serde_json::Value {
    serde_json::json!({
        "dir": cfg.dir,
        "keep": cfg.keep,
        "remote": {
            "enabled": cfg.remote.enabled,
            "url": cfg.remote.url,
            "username": cfg.remote.username,
            "password_set": !cfg.remote.password.is_empty(),
            "encrypt": cfg.remote.encrypt,
            "passphrase_set": !cfg.remote.passphrase.trim().is_empty(),
        }
    })
}

/// 读取备份配置（目录 + 保留份数 + 远端投递）
async fn backups_config_get(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    let cfg = tokio::task::spawn_blocking(move || crate::backup::load_config(&data_dir))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(backup_config_view(&cfg)))
}

async fn backups_config_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<BackupConfigReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let data_dir = state.data_dir.clone();
    let saved = tokio::task::spawn_blocking(move || -> anyhow::Result<crate::backup::BackupConfig> {
        let mut cfg = crate::backup::load_config(&data_dir);
        cfg.dir = req.dir;
        cfg.keep = req.keep;
        cfg.remote.enabled = req.remote.enabled;
        cfg.remote.url = req.remote.url.trim().to_string();
        cfg.remote.username = req.remote.username.trim().to_string();
        if !req.remote.password.is_empty() {
            cfg.remote.password = req.remote.password;
        }
        cfg.remote.encrypt = req.remote.encrypt;
        if !req.remote.passphrase.trim().is_empty() {
            cfg.remote.passphrase = req.remote.passphrase.trim().to_string();
        }
        crate::backup::validate_config(&cfg)?;
        crate::backup::save_config(&data_dir, &cfg)?;
        Ok(cfg)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::file_err)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "config": backup_config_view(&saved) }),
    ))
}

/// 把最新一份本地备份立即投递到远端（admin）
async fn backup_remote_upload(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let dir = state.data_dir.clone();
    let list = tokio::task::spawn_blocking(move || crate::backup::list_backups(&dir))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    let Some(latest) = list.first() else {
        return Err(ApiError::bad("没有可上传的本地备份"));
    };
    let name = latest.name.clone();
    let uploaded = crate::remote::upload_backup(&state.data_dir, &name)
        .await
        .map_err(ApiError::file_err)?;
    if !uploaded {
        return Err(ApiError::bad("未启用远端备份，请先在设置中配置"));
    }
    Ok(Json(serde_json::json!({ "ok": true, "name": name })))
}

/// 上传外部备份文件（multipart 字段 `file`，可选 `passphrase`）：校验后存入备份目录，
/// 之后在列表中正常点「恢复」即可。加密副本必须带正确口令才能导入。
async fn backup_upload(
    State(state): State<AppState>,
    _: RequireRole<2>,
    mut mp: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut passphrase = String::new();
    while let Some(field) = mp.next_field().await.map_err(|e| {
        tracing::warn!("备份上传解析失败：{e}");
        ApiError::bad("上传请求格式错误")
    })? {
        if field.name() == Some("file") {
            bytes = field
                .bytes()
                .await
                .map_err(|e| {
                    tracing::warn!("备份上传字段 file 读取失败：{e}");
                    ApiError::bad("上传数据读取失败")
                })?
                .to_vec();
        } else if field.name() == Some("passphrase") {
            passphrase = field.text().await.map_err(|e| {
                tracing::warn!("备份上传字段 passphrase 读取失败：{e}");
                ApiError::bad("上传数据读取失败")
            })?;
        }
    }
    if bytes.is_empty() {
        return Err(ApiError::bad("缺少 file 字段"));
    }
    if bytes.len() > BACKUP_MAX_BYTES {
        return Err(ApiError::bad(format!(
            "备份文件过大（上限 {}MB）",
            BACKUP_MAX_BYTES / 1024 / 1024
        )));
    }
    // 加密副本先解密再交给导入；非加密文件直接走原路径
    let bytes = if crate::backup::is_encrypted(&bytes) {
        if passphrase.trim().is_empty() {
            return Err(ApiError::bad("该备份已加密，请填写解密口令"));
        }
        crate::backup::decrypt_bytes(passphrase.trim(), &bytes).map_err(ApiError::file_err)?
    } else {
        bytes
    };
    let data_dir = state.data_dir.clone();
    let name = tokio::task::spawn_blocking(move || crate::backup::import_backup(&data_dir, &bytes))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true, "name": name })))
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
    let crate::update::Download { bytes, tag, sha256 } = download;
    tokio::task::spawn_blocking(move || crate::update::install_binary(&bytes, Some(&sha256)))
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
    tokio::task::spawn_blocking(move || crate::alerts::save_rules(&db, &rules))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    // P2-2：唤醒采样循环重载规则（receiver 被丢弃时此处无副作用）
    state.alert_reload.send_modify(|v| *v = v.wrapping_add(1));
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ChannelsReq {
    channels: Vec<crate::alerts::AlertChannel>,
}

async fn alerts_channels_get(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.clone();
    let channels = tokio::task::spawn_blocking(move || crate::alerts::load_channels(&db))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(serde_json::json!({ "channels": channels })))
}

async fn alerts_channels_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<ChannelsReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.channels.len() > 20 {
        return Err(ApiError::bad("通知渠道数量过多（上限 20 条）"));
    }
    for ch in &req.channels {
        crate::alerts::validate_channel(ch).map_err(|e| ApiError::bad(e.to_string()))?;
    }
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || crate::alerts::save_channels(&db, &req.channels))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ChannelTestReq {
    channel: crate::alerts::AlertChannel,
}

/// 用给定渠道发一条测试消息 —— 不落库、不改状态，供管理员校验配置是否可用。
async fn alerts_channel_test(
    _: RequireRole<2>,
    SafeJson(req): SafeJson<ChannelTestReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::alerts::validate_channel(&req.channel).map_err(|e| ApiError::bad(e.to_string()))?;
    // 带上超时：上游无响应时不能让这个请求一直挂着
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| ApiError::internal())?;
    let text = "【LYYS Panel】这是一条测试消息，收到即表示渠道配置可用。";
    crate::alerts::send_channel(&client, &req.channel, text)
        .await
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

// ---------- 3.4 站点可用性探针（admin）----------

#[derive(Deserialize)]
struct ProbesReq {
    targets: Vec<crate::probe::ProbeTarget>,
}

async fn probes_get(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.clone();
    let targets = tokio::task::spawn_blocking(move || crate::probe::load_targets(&db))
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(Json(serde_json::json!({ "targets": targets })))
}

/// 全量保存目标集（前端提交完整列表）。保存后借用告警重载信号：探针任务同样
/// 订阅它，因此新增/修改的目标会立刻重探，而不必等满一个探测周期。
async fn probes_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<ProbesReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if req.targets.len() > crate::probe::TARGET_MAX {
        return Err(ApiError::bad(format!(
            "监控目标过多（上限 {} 条）",
            crate::probe::TARGET_MAX
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for t in &req.targets {
        crate::probe::validate_target(t).map_err(|e| ApiError::bad(e.to_string()))?;
        if !seen.insert(t.id.clone()) {
            return Err(ApiError::bad("目标 id 重复"));
        }
    }
    let db = state.db.clone();
    let targets = req.targets.clone();
    tokio::task::spawn_blocking(move || crate::probe::save_targets(&db, &targets))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::file_err)?;
    state.alert_reload.send_modify(|v| *v = v.wrapping_add(1));
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 配置与运行态合并返回，前端一次拿全（未探测过的目标 last_check = 0）
async fn probes_status(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<Vec<crate::probe::ProbeStatus>>, ApiError> {
    let db = state.db.clone();
    let targets = tokio::task::spawn_blocking(move || crate::probe::load_targets(&db))
        .await
        .map_err(|_| ApiError::internal())?;
    let engine = state.probes.lock().unwrap_or_else(|p| p.into_inner());
    Ok(Json(engine.snapshot(&targets)))
}

#[derive(Deserialize)]
struct ProbeTestReq {
    target: crate::probe::ProbeTarget,
}

/// 立即探测一次（不落库、不改状态），供保存前确认地址是否真的可达
async fn probes_test(
    _: RequireRole<2>,
    SafeJson(req): SafeJson<ProbeTestReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::probe::validate_target(&req.target).map_err(|e| ApiError::bad(e.to_string()))?;
    let client = reqwest::Client::builder()
        .user_agent("lyys-panel-probe/1.0")
        .danger_accept_invalid_certs(req.target.insecure)
        .build()
        .map_err(|_| ApiError::internal())?;
    let out = crate::probe::check(&client, &req.target).await;
    Ok(Json(serde_json::json!({
        "ok": out.ok,
        "status": out.status,
        "ms": out.ms,
        "error": out.error,
    })))
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

// ---------- 安全入口（admin 专属） ----------
//
// 入口前缀与 IP 白名单由 security::guard 挂最外层逐请求生效，这里只负责读写配置。
// 保存时做与中间件同口径的校验，避免把面板存成谁也进不去的配置。

async fn security_get(
    State(state): State<AppState>,
    _: RequireRole<2>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entrance = state
        .db
        .get_setting_async(crate::security::KEY_ENTRANCE)
        .await?
        .unwrap_or_default();
    let allowlist = state
        .db
        .get_setting_async(crate::security::KEY_ALLOWLIST)
        .await?
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "entrance": entrance,
        "allowlist": allowlist,
    })))
}

#[derive(Deserialize)]
struct SecuritySetReq {
    #[serde(default)]
    entrance: String,
    #[serde(default)]
    allowlist: String,
}

async fn security_set(
    State(state): State<AppState>,
    _: RequireRole<2>,
    SafeJson(req): SafeJson<SecuritySetReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entrance =
        crate::security::normalize_entrance(&req.entrance).map_err(|e| ApiError::bad(e.to_string()))?;
    crate::security::parse_allowlist(&req.allowlist).map_err(|e| ApiError::bad(e.to_string()))?;
    let allowlist = req.allowlist.trim().to_string();
    state
        .db
        .set_setting_async(crate::security::KEY_ENTRANCE, &entrance)
        .await?;
    state
        .db
        .set_setting_async(crate::security::KEY_ALLOWLIST, &allowlist)
        .await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "entrance": entrance,
        "allowlist": allowlist,
    })))
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
        .route("/files/compress", post(files_compress))
        .route("/files/extract", post(files_extract))
        .route("/files/chmod", post(files_chmod))
        .route("/files/chown", post(files_chown))
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
        // systemd 定时器：crontab 之外的计划任务承载，带 journald 执行日志
        .route(
            "/timers",
            get(timers_list).post(timers_save).delete(timers_delete),
        )
        .route("/timers/run", post(timers_run))
        .route("/timers/logs", get(timers_logs))
        // 网站管理（admin 专属）：生成 nginx 站点配置并校验 / 重载
        .route("/websites/status", get(websites_status))
        .route(
            "/websites",
            get(websites_list).post(websites_save).delete(websites_delete),
        )
        // Let's Encrypt 自动证书：账户设置 + 单站点申请/续期
        .route("/websites/acme", get(acme_settings_get).post(acme_settings_set))
        .route("/websites/acme/issue", post(acme_issue))
        .route("/network/interfaces", get(net_interfaces))
        .route("/network/routes", get(net_routes))
        .route("/network/connections", get(net_connections))
        .route("/network/dns", get(net_dns))
        // 防火墙管理（admin 专属）：状态与规则只读，增删/启停改主机暴露面
        .route("/firewall/status", get(firewall_status))
        .route("/firewall/rules", get(firewall_rules))
        .route("/firewall/rule", post(firewall_rule_add).delete(firewall_rule_remove))
        .route("/firewall/toggle", post(firewall_toggle))
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
        // 交互式终端（WebSocket，仅 admin）：同样是查询串 token + 手动鉴权
        .route("/terminal", get(terminal_ws))
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
        // 界面字体：GET 读取（全员）、POST 保存（admin）。上传单独一条 ——
        // 字体文件可达 20MB+，需放宽默认 2MB 的请求体上限
        .route("/font", get(font_get).post(font_set))
        .route(
            "/font/upload",
            post(font_upload).layer(DefaultBodyLimit::max(FONT_MAX_BYTES)),
        )
        // 3.1 备份管理（admin 专属）
        .route(
            "/backups",
            get(backups_list).post(backups_create).delete(backup_delete),
        )
        .route("/backups/download", get(backup_download))
        .route("/backups/restore", post(backup_restore))
        .route(
            "/backups/config",
            get(backups_config_get).post(backups_config_set),
        )
        .route(
            "/backups/upload",
            post(backup_upload).layer(DefaultBodyLimit::max(BACKUP_MAX_BYTES)),
        )
        .route("/backups/remote/upload", post(backup_remote_upload))
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
            "/alerts/channels",
            get(alerts_channels_get).post(alerts_channels_set),
        )
        .route("/alerts/channels/test", post(alerts_channel_test))
        .route("/alerts/events", get(alerts_events))
        .route("/alerts/probes", get(probes_get).post(probes_set))
        .route("/alerts/probes/status", get(probes_status))
        .route("/alerts/probes/test", post(probes_test))
        // 安全入口配置：访问路径前缀与 IP 白名单（admin 专属）
        .route("/security", get(security_get).post(security_set))
        // 2.3：审计中间件挂在受保护路由上，记录所有非 GET 业务请求
        // （from_fn 不支持 State 提取器，必须用 from_fn_with_state）
        .layer(middleware::from_fn_with_state(state.clone(), audit_mw))
        // 4.5 AI 悬浮球助手：对话/历史 + 配置（人格提示词与技能说明）
        .route("/ai/chat", post(crate::ai::ai_chat))
        .route("/ai/history", get(crate::ai::ai_history))
        .route("/ai/history/clear", post(crate::ai::ai_history_clear))
        .route("/ai/config", get(ai_config_get).post(ai_config_set));
    let inner = Router::new()
        .route("/health", get(health))
        .route("/api/login", post(login))
        // 自定义字体文件：@font-face 请求带不上 Authorization 头，必须挂在鉴权之外
        .route("/fonts/custom/{name}", get(font_serve))
        .nest("/api", protected)
        .fallback(crate::embed::handler)
        .with_state(state.clone());

    // 安全入口：最外层闸门，SPA、/api 与静态资源一视同仁；
    // 未配前缀的白名单为空时逐请求读库都是空值，等同直接放行。
    //
    // 必须套在路由匹配之前。`Router::layer` 是「逐路由」包裹（axum 会给
    // path_router 与 fallback 各套一层），套在已匹配的路由上再改写 URI 改变不了
    // 匹配结果 —— 这正是 guard 剥前缀却仍落到 SPA fallback 的原因。把整棵路由树
    // 放进一个空的外层 Router 的 fallback，再把 guard 套上去：请求先过闸门、
    // 剥掉前缀，之后才进入内层匹配。
    Router::new()
        .fallback_service(inner)
        .layer(middleware::from_fn_with_state(state, crate::security::guard))
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
        // 带尺寸的对象写法
        assert!(validate_dashboard_config(
            &json!({ "cards": [{ "id": "cpu", "w": 2, "h": 2 }, { "id": "mem", "w": 2 }] })
        )
        .is_ok());
        // w/h 缺省即 1×1 —— 前端只在用户改过尺寸时才写出它们
        assert!(
            validate_dashboard_config(&json!({ "cards": [{ "id": "cpu" }] })).is_ok()
        );
        // 新旧混写：老配置里追加一张带尺寸的卡片时会出现
        assert!(validate_dashboard_config(
            &json!({ "cards": ["mem", { "id": "cpu", "w": 2 }] })
        )
        .is_ok());
        // 放宽后的上限：整宽 4 列 + 2 行（趋势图卡的默认尺寸）
        assert!(
            validate_dashboard_config(&json!({ "cards": [{ "id": "chart", "w": 4, "h": 2 }] }))
                .is_ok()
        );
        // 高度到 3 行也合法；趋势图卡另有更高的上限（6 行）
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "h": 3 }] })).is_ok());
        assert!(
            validate_dashboard_config(&json!({ "cards": [{ "id": "chart", "w": 4, "h": 6 }] }))
                .is_ok()
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
        // 尺寸相关：超范围 / 类型错 / 缺 id / 对象里有未知字段。
        // 宽度上限 4 —— 这不是人为限制，是栅格本身只有 4 列，跨 5 列会溢出；
        // 高度上限按卡片类型：数值卡 3 行、趋势图卡 6 行
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "w": 5 }] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "h": 4 }] })).is_err());
        assert!(
            validate_dashboard_config(&json!({ "cards": [{ "id": "chart", "h": 7 }] })).is_err()
        );
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "w": 0 }] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "w": "2" }] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": [{ "w": 2 }] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "evil", "w": 2 }] })).is_err());
        // 坐标不是合法字段：布局由顺序决定，不接受显式位置
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "x": 1 }] })).is_err());
        assert!(validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "y": "0" }] })).is_err());
        assert!(
            validate_dashboard_config(&json!({ "cards": [{ "id": "cpu", "w": 2 }, "cpu"] }))
                .is_err()
        );
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
            "shadow": false,
            "colors": {
                "primary": "#409eff",
                "bg_page": "#141414",
                "bg_card": "#1F1F1F",
                "text": "#ffffff"
            },
            "bg_image": "data:image/png;base64,iVBORw0KGgo=",
            "card_opacity": 80,
            "card_blur": 12
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

    /// card_opacity：数值 30..=100 闭区间（仪表盘卡片底色不透明度）；
    /// 再透就与卡片下的背景图糊成一片，故下限不是 0
    #[test]
    fn theme_card_opacity_bounds() {
        assert!(validate_theme(&json!({ "card_opacity": 30 })).is_ok());
        assert!(validate_theme(&json!({ "card_opacity": 100 })).is_ok());
        assert!(validate_theme(&json!({ "card_opacity": 65 })).is_ok());
        assert!(validate_theme(&json!({ "card_opacity": 29 })).is_err());
        assert!(validate_theme(&json!({ "card_opacity": 0 })).is_err());
        assert!(validate_theme(&json!({ "card_opacity": 101 })).is_err());
        // 类型必须对：字符串/布尔虽可转成数字也不当数值用
        assert!(validate_theme(&json!({ "card_opacity": "80" })).is_err());
        assert!(validate_theme(&json!({ "card_opacity": true })).is_err());
    }

    /// card_blur：数值 0..=40 闭区间（仪表盘卡片的磨砂玻璃半径）。
    /// 0 合法（等于不模糊）；上限 40 之外只剩一片均匀色块，磨砂感反而消失
    #[test]
    fn theme_card_blur_bounds() {
        assert!(validate_theme(&json!({ "card_blur": 0 })).is_ok());
        assert!(validate_theme(&json!({ "card_blur": 12 })).is_ok());
        assert!(validate_theme(&json!({ "card_blur": 40 })).is_ok());
        assert!(validate_theme(&json!({ "card_blur": -1 })).is_err());
        assert!(validate_theme(&json!({ "card_blur": 41 })).is_err());
        // 类型必须对：字符串/布尔不当数值用（否则可注入任意 CSS 值）
        assert!(validate_theme(&json!({ "card_blur": "12px" })).is_err());
        assert!(validate_theme(&json!({ "card_blur": true })).is_err());
    }

    /// 观感只有两个字段。曾经短暂存在的 chrome_opacity（外壳独立不透明度）已移除，
    /// 必须按未知字段被拒绝 —— 留在白名单里等于继续接受一个不产生任何效果的旋钮
    #[test]
    fn theme_removed_chrome_opacity_rejected() {
        assert!(validate_theme(&json!({ "chrome_opacity": 90 })).is_err());
        assert!(validate_theme(&json!({ "chrome_opacity": 100 })).is_err());
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
        // shadow：仅布尔；数字/字符串虽是真值也不当开关用（类型必须对）
        assert!(validate_theme(&json!({ "shadow": true })).is_ok());
        assert!(validate_theme(&json!({ "shadow": false })).is_ok());
        assert!(validate_theme(&json!({ "shadow": 1 })).is_err());
        assert!(validate_theme(&json!({ "shadow": "true" })).is_err());
    }

    // ---------- AI API 配置校验（设置页） ----------

    /// 四项全空 = 全部回退环境变量/默认值，必须放行
    #[test]
    fn ai_config_empty_passes() {
        assert!(validate_ai_config("", "", "", "").is_ok());
    }

    /// 地址：只接受 http(s):// 开头，其余一律拒绝（上游地址与联网搜索地址同规则）
    #[test]
    fn ai_config_base_validation() {
        for b in ["https://api.openai.com/v1", "http://127.0.0.1:8000/v1"] {
            assert!(
                validate_ai_config(b, "", "", "").is_ok(),
                "应接受合法地址：{b}"
            );
        }
        for b in ["ftp://evil", "api.example.com/v1", "javascript:alert(1)", "//x"] {
            assert!(
                validate_ai_config(b, "", "", "").is_err(),
                "应拒绝非法地址：{b:?}"
            );
            // 联网搜索地址走同一套校验
            assert!(
                validate_ai_config("", "", "", b).is_err(),
                "应拒绝非法搜索地址：{b:?}"
            );
        }
        // 空搜索地址合法（表示使用内置通道）
        assert!(validate_ai_config("", "", "", "").is_ok());
        assert!(validate_ai_config("", "", "", "http://10.0.0.1:8888").is_ok());
    }

    /// 长度上限：超限拒绝，上限值本身放行
    #[test]
    fn ai_config_length_limits() {
        let base_ok = format!("https://a{}", "b".repeat(AI_BASE_MAX_LEN - 10));
        assert!(validate_ai_config(&base_ok, "", "", "").is_ok());
        let base_bad = format!("https://{}", "a".repeat(AI_BASE_MAX_LEN));
        assert!(validate_ai_config(&base_bad, "", "", "").is_err());
        assert!(validate_ai_config("", &"k".repeat(AI_KEY_MAX_LEN), "", "").is_ok());
        assert!(validate_ai_config("", &"k".repeat(AI_KEY_MAX_LEN + 1), "", "").is_err());
        assert!(validate_ai_config("", "", &"m".repeat(AI_MODEL_MAX_LEN), "").is_ok());
        assert!(validate_ai_config("", "", &"m".repeat(AI_MODEL_MAX_LEN + 1), "").is_err());
        let search_ok = format!("https://s{}", "c".repeat(AI_BASE_MAX_LEN - 10));
        assert!(validate_ai_config("", "", "", &search_ok).is_ok());
        let search_bad = format!("https://{}", "s".repeat(AI_BASE_MAX_LEN));
        assert!(validate_ai_config("", "", "", &search_bad).is_err());
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
