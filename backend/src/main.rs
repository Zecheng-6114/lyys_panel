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

use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex as AsyncMutex;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<db::Db>,
    /// 监控采集器（sysinfo 需要内部可变，用异步互斥锁保护）
    pub monitor: Arc<AsyncMutex<monitor::Monitor>>,
    pub jwt_secret: Arc<[u8; 32]>,
    /// 登录失败退避器
    pub throttle: Arc<auth::LoginThrottle>,
    // 数据目录字段仅 AI 助手（emotion 模型）使用，随 AI 停用一起注释（恢复时放开）
    /*
    /// 数据目录：模型等随数据存放的文件的根。默认取 PANEL_DB 所在目录，
    /// 可用 PANEL_DATA_DIR 覆盖。
    pub data_dir: Arc<str>,
    */
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

    // 数据目录仅 AI 助手使用，随 AI 停用一起注释（恢复时放开）
    /*
    // 数据目录默认与数据库同级（模型等文件放这里），可用 PANEL_DATA_DIR 覆盖
    let data_dir = std::env::var("PANEL_DATA_DIR").unwrap_or_else(|_| {
        std::path::Path::new(&db_path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| ".".to_string())
    });
    */

    let db = db::Db::open(&db_path).context("初始化数据库失败")?;
    let jwt_secret: Arc<[u8; 32]> = Arc::from(auth::load_or_create_secret(&db)?);
    let monitor = Arc::new(AsyncMutex::new(monitor::Monitor::new()));

    // 首次启动时引导管理员账号
    auth::ensure_admin(&db)?;

    let state = AppState {
        db: Arc::new(db),
        monitor,
        jwt_secret,
        throttle: Arc::new(auth::LoginThrottle::new()),
        // data_dir: Arc::from(data_dir.as_str()), // 随 AI 停用
    };

    // 启动后台监控采样任务
    monitor::spawn_sampler(state.clone());

    // 压缩放在最外层：前端产物里 element-plus 一个包就 790KB，不压的话每次
    // 打开页面都在裸传。默认谓词已排除 SSE 与图片等不可压内容，不会影响
    // AI 流式对话与语音接口。
    let app = api::router(state)
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
