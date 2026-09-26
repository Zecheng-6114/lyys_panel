// AI 助手功能暂时停用（用户决定），相关模块整体注释；恢复时放开下面 5 行
// mod ai;
// mod aimemory;
// mod aitools;
mod api;
mod auth;
mod crontab;
mod db;
mod distro;
mod docker;
// mod emotion;
mod embed;
mod files;
mod logs;
mod monitor;
mod network;
mod opservice;
mod packages;
mod rprocess;
// mod websearch; // 仅被 aitools（AI 工具）引用，随 AI 一起停用

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::Request;
use axum::http::{header, HeaderName, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::Response;
use tokio::sync::Mutex as AsyncMutex;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<db::Db>,
    /// 监控采集器（sysinfo 需要内部可变，用异步互斥锁保护）
    pub monitor: Arc<AsyncMutex<monitor::Monitor>>,
    /// JWT 签名密钥（P0-2：从环境变量或密钥文件加载，不落数据库）
    pub jwt_secret: Arc<[u8]>,
    /// 登录失败退避器
    pub throttle: Arc<auth::LoginThrottle>,
    /// Token 吊销名单（P1-1：登出后服务端拒绝旧 token）
    pub revocations: Arc<auth::TokenRevocations>,
    /// 数据目录：JWT 密钥文件、初始密码等敏感文件的存放根目录
    /// （P0-2 起承载安全职责，不再是 AI 专用；AI 助手恢复时可直接复用本字段）
    pub data_dir: Arc<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志：默认 info，可用 RUST_LOG 覆盖
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 发行版检测：不支持的系统直接退出，避免后续包管理/服务命令乱套
    if let Err(e) = distro::init() {
        tracing::error!("{e}");
        eprintln!("{e}");
        std::process::exit(1);
    }
    tracing::info!("检测到发行版：{}", distro::pretty());

    let addr = std::env::var("PANEL_ADDR").unwrap_or_else(|_| "127.0.0.1:3789".into());
    let db_path = std::env::var("PANEL_DB").unwrap_or_else(|_| "data/panel.db".into());

    // 数据目录：默认与数据库同级（JWT 密钥文件、初始密码文件放这里），
    // 可用 PANEL_DATA_DIR 覆盖。P0-2：该目录在文件管理接口中被整体禁访问。
    let data_dir: PathBuf = std::env::var("PANEL_DATA_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::path::Path::new(&db_path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_path_buf())
        })
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&data_dir).context("创建数据目录失败")?;
    let data_dir = data_dir.canonicalize().context("解析数据目录失败")?;

    let db = db::Db::open(&db_path).context("初始化数据库失败")?;

    // P0-2 迁移：旧版本把 JWT 密钥存在 settings 表里（可被文件接口拖库提取后
    // 伪造长效 token），现改为密钥文件/环境变量，这里把库里的遗留密钥删掉。
    // 注意不做「旧密钥搬家」——沿用旧密钥等于把已泄露的凭证原样保留。
    if db.get_setting("jwt_secret")?.is_some() {
        db.remove_setting("jwt_secret")?;
        tracing::info!("已移除数据库中的遗留 JWT 密钥（改用密钥文件/环境变量，本次将签发全新 token）");
    }

    // P0-2：密钥优先级 环境变量 PANEL_JWT_SECRET > <data_dir>/jwt_secret.key > 新生成
    let jwt_secret: Arc<[u8]> = Arc::from(auth::load_jwt_secret(&data_dir)?);

    // P0-2：数据目录整体对文件管理接口禁访问（含 *.db / -wal / -shm）
    files::set_protected_dir(data_dir.clone());

    let monitor = Arc::new(AsyncMutex::new(monitor::Monitor::new()));

    // 首次启动时引导管理员账号（P0-1：随机密码写 0600 文件，不落日志）
    auth::ensure_admin(&db, &data_dir)?;

    let state = AppState {
        db: Arc::new(db),
        monitor,
        jwt_secret,
        throttle: Arc::new(auth::LoginThrottle::new()),
        revocations: Arc::new(auth::TokenRevocations::new()),
        data_dir: Arc::new(data_dir),
    };

    // 启动后台监控采样任务
    monitor::spawn_sampler(state.clone());

    // 压缩放在外层：前端产物里 element-plus 一个包就 790KB，不压的话每次
    // 打开页面都在裸传。默认谓词已排除 SSE 与图片等不可压内容，不会影响
    // AI 流式对话与语音接口。
    //
    // 安全响应头放在最内层：保证 4xx/5x 及静态资源等所有响应（包括错误
    // 路径）都带上五项头部（P1-4），且早于压缩层完成头注入。
    let app = api::router(state)
        .layer(middleware::from_fn(security_headers))
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("监听地址 {addr} 失败"))?;
    tracing::info!("面板服务已启动：http://{addr}");
    // 带上连接信息，登录限流需要来源 IP
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// 五项安全响应头（P1-4）：
/// - `X-Content-Type-Options: nosniff`：禁止浏览器 MIME 嗅探（配合正确的
///   Content-Type，堵住上传内容被当脚本执行的经典链）；
/// - `X-Frame-Options: DENY`：禁止被嵌入 iframe（点击劫持防护）；
/// - `Content-Security-Policy`：默认只允许同源资源；img 放行 data:（主题
///   背景图）；style 放行 unsafe-inline（Element Plus 组件内联样式所需）；
/// - `Referrer-Policy: no-referrer`：不向第三方泄漏面板地址；
/// - `Permissions-Policy`：收禁摄像头/麦克风/定位/USB/支付等能力。
async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'",
        ),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // Permissions-Policy 未在 http crate 注册为标准常量，用 HeaderName 构造
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
    );
    resp
}
