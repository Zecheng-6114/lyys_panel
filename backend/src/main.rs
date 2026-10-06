mod ai; // 4.5 AI 助手：悬浮球单助手（人格提示词 + 工具调用 + 按用户持久化历史）
mod ai_tools; // AI 工具调用：面板只读运维查询暴露为 function calling 工具
mod alerts;
mod api;
mod auth;
mod backup;
mod cmd; // P1-1 统一命令执行器：超时 + 分组互斥 + 输出上限，全后端唯一的外部进程出口
mod container_files; // 容器内文件浏览：只读，走 docker exec
mod crontab;
mod db;
mod distro;
mod docker;
mod embed;
mod files;
mod firewall; // 防火墙管理：ufw / firewalld 的规则读写与启停（仅 admin）
mod instances; // 实例视图：容器与 systemd 服务聚合，作为各自日志/进程的入口
mod jobs; // P2-1 作业队列：长操作脱离请求生命周期，输出落表并经 SSE 增量推送
mod logs;
mod monitor;
mod network;
mod ops; // 4.3 深度运维：unit 文件查看、SMART 磁盘健康
mod opservice;
mod packages;
mod rprocess;
mod remote; // 备份远端投递：把本地备份复制到 WebDAV 异地存储（可选加密）
mod security; // 安全入口：访问路径前缀 + IP 白名单，登录前的访问闸门
mod terminal; // 交互式终端：浏览器 ↔ 本机 PTY 的双向 WebSocket 桥（仅 admin）
mod timers; // systemd 定时器：计划任务的另一种承载 + journald 执行日志
mod tls;
mod update;
mod websearch; // AI 助手联网：搜索多通道 + 网页正文抓取（含 SSRF 闸门）
mod websites; // 网站管理：Nginx 站点（静态托管 / 反向代理，可选 HTTPS）生成与启停

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
    /// 告警规则重载信号（P2-2）：设置页保存规则后发送递增信号，
    /// 采样循环用 `select!` 监听它并立即重载，取代此前最长 1 小时的延迟。
    pub alert_reload: Arc<tokio::sync::watch::Sender<u64>>,
    /// 数据目录：JWT 密钥文件、初始密码等敏感文件的存放根目录
    /// （P0-2 起承载安全职责）
    pub data_dir: Arc<PathBuf>,
    /// 数据库文件路径（3.1 恢复流程在启动时需要原始路径字符串）
    pub db_path: Arc<String>,
    /// P2-1 作业运行时索引：句柄用于取消、广播通道用于 SSE 推送。
    /// 作业的事实来源是 jobs 表，这里只是进程内索引，重启即清空。
    pub jobs: Arc<jobs::JobHub>,
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

    // 3.1 恢复：打开数据库之前先应用恢复标记（若有），否则旧库会被直接打开
    if backup::apply_pending_restore(&data_dir, &db_path)? {
        tracing::warn!("本次启动已应用数据库恢复");
    }

    let db = db::Db::open(&db_path).context("初始化数据库失败")?;

    // P0-2 迁移：旧版本把 JWT 密钥存在 settings 表里（可被文件接口拖库提取后
    // 伪造长效 token），现改为密钥文件/环境变量，这里把库里的遗留密钥删掉。
    // 注意不做「旧密钥搬家」——沿用旧密钥等于把已泄露的凭证原样保留。
    if db.get_setting("jwt_secret")?.is_some() {
        db.remove_setting("jwt_secret")?;
        tracing::info!(
            "已移除数据库中的遗留 JWT 密钥（改用密钥文件/环境变量，本次将签发全新 token）"
        );
    }

    // P0-2：密钥优先级 环境变量 PANEL_JWT_SECRET > <data_dir>/jwt_secret.key > 新生成
    let jwt_secret: Arc<[u8]> = Arc::from(auth::load_jwt_secret(&data_dir)?);

    // P0-2：数据目录整体对文件管理接口禁访问（含 *.db / -wal / -shm）
    files::set_protected_dir(data_dir.clone());

    let monitor = Arc::new(AsyncMutex::new(monitor::Monitor::new()));

    // 首次启动时引导管理员账号（P0-1：随机密码写 0600 文件，不落日志）
    auth::ensure_admin(&db, &data_dir)?;

    // P2-1 重启收尾：上次运行中被中断的作业不能永远停在「运行中」。
    // 刻意不自动续跑 —— 安装类操作被中断后系统可能停在半完成状态，
    // 自动重试比重做更危险，交回用户在界面上重新提交。
    match db.job_mark_interrupted(time::OffsetDateTime::now_utc().unix_timestamp()) {
        Ok(n) if n > 0 => {
            tracing::warn!("已将 {n} 个重启前遗留的作业标记为 interrupted");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("作业重启收尾失败：{e:#}"),
    }

    // P2-3：退避计数与吊销名单改存数据库（重启不再清零），各自持一份连接池句柄
    let throttle = Arc::new(auth::LoginThrottle::new(db.clone()));
    let revocations = Arc::new(auth::TokenRevocations::new(db.clone()));
    // P2-2：告警规则重载信号通道（初始值 0，采样循环只关心「有变化」）
    let (alert_reload, _) = tokio::sync::watch::channel(0u64);

    let state = AppState {
        db: Arc::new(db),
        monitor,
        jwt_secret,
        throttle,
        revocations,
        alert_reload: Arc::new(alert_reload),
        data_dir: Arc::new(data_dir),
        db_path: Arc::new(db_path),
        jobs: Arc::new(jobs::JobHub::new()),
    };

    // 启动后台监控采样任务
    monitor::spawn_sampler(state.clone());
    // 容器列表的后台采样：CLI 每次 fork 都要 ~170ms，不能让实例页每请求付一遍
    instances::spawn_container_sampler();

    // 压缩放在外层：前端产物里 element-plus 一个包就 790KB，不压的话每次
    // 打开页面都在裸传。默认谓词已排除 SSE 与图片等不可压内容。
    //
    // 安全响应头放在最内层：保证 4xx/5x 及静态资源等所有响应（包括错误
    // 路径）都带上五项头部（P1-4），且早于压缩层完成头注入。
    // 1.3 HTTPS：PANEL_TLS=auto（默认，自签）/ custom（指定证书）/ off（纯 HTTP）
    let tls_settings = tls::settings_from_env(&state.data_dir)?;

    let app = api::router(state)
        .layer(middleware::from_fn(security_headers))
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    // （tls_settings 已在构建 router 前解析）
    if let Some(ts) = &tls_settings {
        if ts.mode == tls::TlsMode::Auto {
            tls::ensure_self_signed(&ts.cert, &ts.key)?;
        }
        // 可选：PANEL_HTTP_PORT 设置后额外监听一个纯 HTTP 端口，301 跳转到 HTTPS
        if let Ok(http_port) = std::env::var("PANEL_HTTP_PORT") {
            spawn_http_redirect(http_port, addr.clone());
        }
        // 带上连接信息，登录限流需要来源 IP（与 HTTP 分支同一约定）
        let socket_addr: std::net::SocketAddr = addr
            .parse()
            .with_context(|| format!("监听地址 {addr} 无法解析为 SocketAddr"))?;
        let rustls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(&ts.cert, &ts.key)
            .await
            .context("加载 TLS 证书失败")?;
        tracing::info!("面板服务已启动：https://{addr}");
        axum_server::bind_rustls(socket_addr, rustls_config)
            .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await?;
    } else {
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
    }
    Ok(())
}

/// 纯 HTTP → HTTPS 的 301 跳转服务（PANEL_HTTP_PORT 启用时）
fn spawn_http_redirect(http_port: String, https_addr: String) {
    tokio::spawn(async move {
        // rsplit_once 取最后一段，天然兼容 IPv6 字面量 `[::1]:3789`
        let (host, port) = https_addr
            .rsplit_once(':')
            .unwrap_or((https_addr.as_str(), "3789"));
        let target = format!("https://{host}:{port}");
        let log_target = target.clone();
        let app = axum::routing::any(move |uri: axum::http::Uri| {
            let target = target.clone();
            async move {
                let location = format!("{target}{}", uri.path());
                (
                    [(axum::http::header::LOCATION, location)],
                    axum::http::StatusCode::MOVED_PERMANENTLY,
                )
            }
        });
        let bind = format!("{host}:{http_port}");
        match tokio::net::TcpListener::bind(&bind).await {
            Ok(l) => {
                tracing::info!("HTTP 跳转监听：http://{bind} → {log_target}");
                if let Err(e) = axum::serve(l, app).await {
                    tracing::warn!("HTTP 跳转服务退出：{e}");
                }
            }
            Err(e) => tracing::warn!("HTTP 跳转监听 {bind} 失败：{e}"),
        }
    });
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
    // Permissions-Policy 未在 http crate 注册为标准常量，用 HeaderName 构造。
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
    );
    resp
}
