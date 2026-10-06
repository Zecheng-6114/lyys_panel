//! P2-1 本地作业表与任务流。
//!
//! 长操作（装包 / 拉镜像 / 备份）不再绑在 HTTP 请求生命周期里：请求只负责登记
//! 作业并拿到 id，实际执行交给进程内的 tokio 任务，输出写 jobs 表并通过 SSE
//! 增量推给前端。
//!
//! **数据库是作业的唯一事实来源**，进程内的句柄表只用于取消。因此面板重启后
//! 历史作业及其输出尾部仍然可查，只是运行中的那些由启动收尾标记为 interrupted。
//!
//! 执行体不另起并发控制：它调用各业务模块时沿用的仍是 P1-1 的 `Budget` 与
//! `LockGroup`，包管理器作业天然被 `LockGroup::Package` 的静态锁排成队列。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::cmd::LockGroup;
use crate::AppState;

/// 每个作业保留的输出行数（环形截断）。作业结束后仍可回看，表不会无限增长；
/// 超出部分不可追溯，这是刻意的取舍。
pub const TAIL_LINES: usize = 200;
/// 广播通道容量：前端消费慢时丢最旧的行，绝不阻塞执行体
const CHANNEL_CAP: usize = 512;
/// 运行中将输出尾部落库的间隔
const PERSIST_INTERVAL: Duration = Duration::from_millis(1000);

/// 作业类型。首批只接四类长操作，查询类接口保持同步（它们有 15s 级预算，
/// 做成作业只会让前端更麻烦）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    PkgInstall,
    PkgRemove,
    PkgUpgrade,
    PkgSysupgrade,
    PkgUpdate,
    BackupCreate,
    DockerPull,
    DockerInstall,
}

impl JobKind {
    /// 落库与接口交互用的字符串标签
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PkgInstall => "pkg_install",
            Self::PkgRemove => "pkg_remove",
            Self::PkgUpgrade => "pkg_upgrade",
            Self::PkgSysupgrade => "pkg_sysupgrade",
            Self::PkgUpdate => "pkg_update",
            Self::BackupCreate => "backup_create",
            Self::DockerPull => "docker_pull",
            Self::DockerInstall => "docker_install",
        }
    }

    /// 中文显示名（日志与前端提示）
    pub fn label(self) -> &'static str {
        match self {
            Self::PkgInstall => "安装软件包",
            Self::PkgRemove => "卸载软件包",
            Self::PkgUpgrade => "升级软件包",
            Self::PkgSysupgrade => "滚动更新",
            Self::PkgUpdate => "刷新软件索引",
            Self::BackupCreate => "创建备份",
            Self::DockerPull => "拉取镜像",
            Self::DockerInstall => "安装 Docker",
        }
    }

    /// 解析接口传来的类型标签
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pkg_install" => Self::PkgInstall,
            "pkg_remove" => Self::PkgRemove,
            "pkg_upgrade" => Self::PkgUpgrade,
            "pkg_sysupgrade" => Self::PkgSysupgrade,
            "pkg_update" => Self::PkgUpdate,
            "backup_create" => Self::BackupCreate,
            "docker_pull" => Self::DockerPull,
            "docker_install" => Self::DockerInstall,
            _ => return None,
        })
    }

    /// 该作业第一个会争用的执行分组，仅用于作业层排队（见 [`gate`]）。
    ///
    /// `DockerInstall` 归 Package：它先经包管理器装包、再起守护进程，
    /// 第一处争用点就是包管理器锁。
    fn lock_group(self) -> LockGroup {
        match self {
            Self::PkgInstall
            | Self::PkgRemove
            | Self::PkgUpgrade
            | Self::PkgSysupgrade
            | Self::PkgUpdate
            | Self::DockerInstall => LockGroup::Package,
            Self::DockerPull => LockGroup::Docker,
            // 备份执行体直调 spawn_blocking，不经 cmd 的分组锁
            Self::BackupCreate => LockGroup::None,
        }
    }
}

/// 同组作业的排队闸门。
///
/// 为什么不直接依赖 `cmd` 的分组锁：那把锁是在 `run_streaming` 内部获取的，
/// 作业观察不到「排队结束、真正开始」这一刻，结果排队中的作业在库里也已经写成
/// running —— 细案要求的「第二个作业停在 pending」落不了地。这里按分组再排一次，
/// 拿到闸门之后才写 running，状态因此与事实一致。
///
/// 与 `cmd` 的分组锁不重复：同组作业已被本层串行化，第二层几乎不会阻塞；但也不能
/// 去掉第二层 —— 它还要挡住不经作业的同步路径（如接口直连的装机操作）。
static PKG_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static DOCKER_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 取分组对应的排队闸门。`LockGroup::None`（无互斥）返回 None，不排队。
fn gate(group: LockGroup) -> Option<&'static tokio::sync::Mutex<()>> {
    match group {
        LockGroup::Package => Some(&PKG_GATE),
        LockGroup::Docker => Some(&DOCKER_GATE),
        LockGroup::None | LockGroup::Systemd | LockGroup::Firewall => None,
    }
}

/// SSE 事件载荷
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    /// 新增输出（一次一行）
    Lines { text: String },
    /// 到达终态
    Done {
        status: String,
        exit_code: Option<i64>,
        error: Option<String>,
    },
}

/// 进程内作业索引：句柄用于取消，广播通道用于 SSE 推送。
///
/// 表是事实来源，这里只是运行时索引 —— 面板重启后本结构清空，
/// 表里遗留的 running 行由启动收尾改成 interrupted。
#[derive(Default)]
pub struct JobHub {
    handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    channels: Mutex<HashMap<String, broadcast::Sender<JobEvent>>>,
}

impl JobHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn add_handle(&self, id: &str, h: tokio::task::JoinHandle<()>) {
        if let Ok(mut m) = self.handles.lock() {
            m.insert(id.to_string(), h);
        }
    }

    fn add_channel(&self, id: &str, tx: broadcast::Sender<JobEvent>) {
        if let Ok(mut m) = self.channels.lock() {
            m.insert(id.to_string(), tx);
        }
    }

    /// 订阅作业的增量事件（SSE 用）。作业已结束或不存在时返回 None，
    /// 调用方据此只回放数据库里的输出尾部。
    pub fn subscribe(&self, id: &str) -> Option<broadcast::Receiver<JobEvent>> {
        self.channels.lock().ok()?.get(id).map(|tx| tx.subscribe())
    }

    /// 执行体结束后清理自己的句柄与通道
    fn remove(&self, id: &str) {
        if let Ok(mut m) = self.handles.lock() {
            m.remove(id);
        }
        if let Ok(mut m) = self.channels.lock() {
            m.remove(id);
        }
    }

    /// 清理已结束但未及移除的句柄（异常路径兜底），避免长期运行缓慢增长
    fn prune_finished(&self) {
        if let Ok(mut m) = self.handles.lock() {
            m.retain(|_, h| !h.is_finished());
        }
    }

    /// 取消作业：abort 执行体。执行器已对子进程设置 `kill_on_drop`，
    /// future 被丢弃时子进程随之终止。返回 false 表示作业已不在运行中。
    pub fn cancel(&self, id: &str) -> bool {
        let handle = self.handles.lock().ok().and_then(|mut m| m.remove(id));
        match handle {
            Some(h) => {
                h.abort();
                true
            }
            None => false,
        }
    }
}

/// 提交作业：登记 pending 行 → 建立广播通道 → spawn 执行体 → 立即返回 id。
///
/// 入参校验与命令构造都在执行体内完成（与同步路径共用同一套函数），
/// 因此提交阶段只可能因写库失败而报错 —— 不会出现"内存里在跑、表里查不到"。
pub async fn submit(state: &AppState, kind: JobKind, payload: serde_json::Value) -> Result<String> {
    let id = new_id();
    let payload_str = payload.to_string();
    state
        .db
        .job_insert_async(&id, kind.as_str(), &payload_str, now_secs())
        .await
        .context("登记作业失败")?;

    let (tx, _rx) = broadcast::channel(CHANNEL_CAP);
    state.jobs.prune_finished();
    state.jobs.add_channel(&id, tx.clone());

    let st = state.clone();
    let job_id = id.clone();
    let handle = tokio::spawn(async move {
        run_job(st.clone(), &job_id, kind, &payload_str, tx).await;
        st.jobs.remove(&job_id);
    });
    state.jobs.add_handle(&id, handle);

    tracing::info!("已提交作业 {id}（{}）", kind.label());
    Ok(id)
}

/// 作业执行体：排队 → 标记 running → 执行 → 写终态 → 广播结束事件。
async fn run_job(
    state: AppState,
    id: &str,
    kind: JobKind,
    payload: &str,
    tx: broadcast::Sender<JobEvent>,
) {
    // 同组作业在此排队：拿到闸门之前状态停在 pending，这才对得上细案的
    // 「第二个作业停在 pending，第一个结束后才开始执行」。
    // 任务被 cancel 时守卫随 future 丢弃，闸门自动释放。
    let _gate = match gate(kind.lock_group()) {
        Some(g) => Some(g.lock().await),
        None => None,
    };

    if let Err(e) = state.db.job_mark_running_async(id, now_secs()).await {
        tracing::warn!("作业 {id} 置为 running 失败：{e:#}");
    }

    // 输出环形缓冲
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));

    // 落库协程：周期写一次尾部快照。执行器的输出回调是同步的，不能在里面
    // await，所以把落库挪到这里串行做——同一协程内顺序执行，快照不会乱序覆盖。
    let done = Arc::new(AtomicBool::new(false));
    let persist = {
        let db = state.db.clone();
        let tail = tail.clone();
        let done = done.clone();
        let job_id = id.to_string();
        tokio::spawn(async move {
            while !done.load(Ordering::Relaxed) {
                tokio::time::sleep(PERSIST_INTERVAL).await;
                let snapshot = snapshot_tail(&tail);
                if let Err(e) = db.job_set_tail_async(&job_id, &snapshot).await {
                    tracing::warn!("写入作业 {job_id} 输出失败：{e:#}");
                }
            }
        })
    };

    // 输出接收器：只做零等待的两件事 —— 写内存环形缓冲、广播给 SSE 订阅者
    let sink_tail = tail.clone();
    let sink_tx = tx.clone();
    let mut sink = move |line: &str| {
        if let Ok(mut buf) = sink_tail.lock() {
            if buf.len() >= TAIL_LINES {
                buf.pop_front();
            }
            buf.push_back(line.to_string());
        }
        // 无订阅者时 send 失败属正常（前端可能还没连上）
        let _ = sink_tx.send(JobEvent::Lines {
            text: line.to_string(),
        });
    };

    let result = execute(&state, kind, payload, &mut sink).await;

    // 收尾前先让落库协程退出并完成最后一次写入
    done.store(true, Ordering::Relaxed);
    let _ = persist.await;

    let tail_text = snapshot_tail(&tail);
    let (status, exit_code, error) = match &result {
        Ok(code) => ("success", *code, None),
        Err(e) => ("failed", exit_code_of(e), Some(e.to_string())),
    };
    if let Err(e) = state
        .db
        .job_finish_async(
            id,
            status,
            exit_code,
            &tail_text,
            error.as_deref(),
            now_secs(),
        )
        .await
    {
        tracing::warn!("作业 {id} 写入终态失败：{e:#}");
    }
    let _ = tx.send(JobEvent::Done {
        status: status.to_string(),
        exit_code,
        error,
    });
    tracing::info!("作业 {id} 结束：{status}");
}

/// 按类型执行作业主体。Ok(退出码) 表示成功，Err 为失败原因（写入 error 列）。
async fn execute(
    state: &AppState,
    kind: JobKind,
    payload: &str,
    sink: &mut (dyn FnMut(&str) + Send),
) -> Result<Option<i64>> {
    let params: serde_json::Value = serde_json::from_str(payload).context("作业参数解析失败")?;

    match kind {
        JobKind::BackupCreate => {
            sink("开始创建数据库备份…");
            let db = state.db.clone();
            let data_dir = state.data_dir.clone();
            let name =
                tokio::task::spawn_blocking(move || crate::backup::create_backup(&db, &data_dir))
                    .await
                    .map_err(|_| anyhow::anyhow!("备份任务调度失败"))?
                    .context("创建备份失败")?;
            sink(&format!("备份已生成：{name}"));
            // 远端已启用则顺带投递一份；失败只记日志，不影响本地备份
            if let Err(e) = crate::remote::upload_backup(&state.data_dir, &name).await {
                tracing::warn!("备份远端投递失败：{e:#}");
            }
        }
        JobKind::DockerPull => {
            let target = str_param(&params, "target")?;
            crate::docker::pull(&target, &mut *sink).await?;
        }
        JobKind::DockerInstall => {
            crate::docker::install(&mut *sink).await?;
        }
        _ => {
            let names = names_param(&params);
            match kind {
                JobKind::PkgInstall => crate::packages::install(&names, &mut *sink).await?,
                JobKind::PkgRemove => crate::packages::remove(&names, &mut *sink).await?,
                JobKind::PkgUpgrade => crate::packages::upgrade(&names, &mut *sink).await?,
                JobKind::PkgSysupgrade => crate::packages::system_upgrade(&mut *sink).await?,
                JobKind::PkgUpdate => crate::packages::update_index(&mut *sink).await?,
                // 上面的分支已穷尽其余类型
                _ => unreachable!(),
            };
        }
    }
    Ok(Some(0))
}

/// 从失败原因里取结构化退出码，落进 `jobs.exit_code`。
///
/// 只有「进程真的以非零码退出」才有码；信号杀死（None）与面板自造的失败
/// （参数非法、备份报错）没有退出码，列上留 NULL 才是诚实的。
fn exit_code_of(e: &anyhow::Error) -> Option<i64> {
    e.downcast_ref::<crate::cmd::CommandFailed>()
        .and_then(|f| f.code)
        .map(i64::from)
}

/// 取字符串参数（缺失即报错，避免把空串当合法输入送进命令构造）
fn str_param(params: &serde_json::Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("缺少参数 {key}"))
}

/// 取包名数组。合法性由 packages 模块的 check_name 把关（单一校验点）
fn names_param(params: &serde_json::Value) -> Vec<String> {
    params
        .get("names")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default()
}

/// 当前 Unix 秒
fn now_secs() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// 32 位十六进制作业 id。
///
/// 用 rand 而非引入 uuid 依赖：面板是单机自用，作业量级为个位数，
/// 128 位随机串的碰撞概率远低于任何实用阈值，多一个依赖只多一份维护面。
fn new_id() -> String {
    let bytes: [u8; 16] = rand::random();
    let mut s = String::with_capacity(32);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 把环形缓冲拼成落库与回放用的文本
fn snapshot_tail(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    match tail.lock() {
        Ok(buf) => buf.iter().cloned().collect::<Vec<_>>().join("\n"),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 类型标签双向一致：落库值能解析回原枚举（接口与存储共用同一套标签）
    #[test]
    fn kind_labels_round_trip() {
        for k in [
            JobKind::PkgInstall,
            JobKind::PkgRemove,
            JobKind::PkgUpgrade,
            JobKind::PkgSysupgrade,
            JobKind::PkgUpdate,
            JobKind::BackupCreate,
            JobKind::DockerPull,
            JobKind::DockerInstall,
        ] {
            assert_eq!(JobKind::parse(k.as_str()), Some(k), "标签 {k:?} 无法回解");
            assert!(!k.label().is_empty());
        }
        assert_eq!(JobKind::parse("nope"), None);
        assert_eq!(JobKind::parse(""), None);
    }

    /// id 是 32 位小写十六进制，且不重复
    #[test]
    fn job_id_is_hex32_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 32);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
    }

    /// 环形缓冲只保留最后 TAIL_LINES 行，超出部分从头部丢弃
    #[test]
    fn tail_ring_keeps_last_lines() {
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        {
            let mut buf = tail.lock().unwrap();
            for i in 0..(TAIL_LINES + 50) {
                if buf.len() >= TAIL_LINES {
                    buf.pop_front();
                }
                buf.push_back(format!("line-{i}"));
            }
        }
        let text = snapshot_tail(&tail);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), TAIL_LINES);
        assert_eq!(lines[0], "line-50", "最早的行应被逐出");
        assert_eq!(lines[TAIL_LINES - 1], format!("line-{}", TAIL_LINES + 49));
    }

    /// 参数提取：缺 target 必须报错而不是退化成空串送进命令
    #[test]
    fn params_are_validated() {
        let v: serde_json::Value = serde_json::json!({"names": ["nginx", "curl"]});
        assert_eq!(
            names_param(&v),
            vec!["nginx".to_string(), "curl".to_string()]
        );
        assert!(names_param(&serde_json::json!({})).is_empty());
        assert!(str_param(&v, "target").is_err());
        assert!(str_param(&serde_json::json!({"target": ""}), "target").is_err());
        assert_eq!(
            str_param(&serde_json::json!({"target": "nginx:latest"}), "target").unwrap(),
            "nginx:latest"
        );
    }

    /// 取消会 abort 执行体（子进程靠 kill_on_drop 随之终止）；
    /// 重复取消与取消不存在的作业都返回 false —— handler 据此回「已结束」而不是假装成功。
    #[tokio::test]
    async fn cancel_aborts_running_task() {
        let hub = JobHub::new();
        let reached_end = Arc::new(AtomicBool::new(false));
        let flag = reached_end.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            flag.store(true, Ordering::Relaxed);
        });
        hub.add_handle("job-1", handle);

        assert!(hub.cancel("job-1"), "运行中的作业应可取消");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !reached_end.load(Ordering::Relaxed),
            "被取消的任务不得跑到结尾"
        );
        assert!(!hub.cancel("job-1"), "重复取消应返回 false");
        assert!(!hub.cancel("nope"), "不存在的作业应返回 false");
    }

    /// 订阅只对活跃作业有效：通道摘除后 subscribe 返回 None，
    /// SSE handler 据此改为「只回放库里的尾部」。
    #[tokio::test]
    async fn subscribe_only_while_channel_present() {
        let hub = JobHub::new();
        let (tx, _rx) = broadcast::channel::<JobEvent>(8);
        hub.add_channel("job-2", tx);

        assert!(hub.subscribe("job-2").is_some());
        assert!(hub.subscribe("other").is_none());
        hub.remove("job-2");
        assert!(hub.subscribe("job-2").is_none(), "通道摘除后不应再能订阅");
    }

    /// 退出码只从 cmd 的结构化错误里取，且要穿透 `.context()` 包装；
    /// 面板自造的失败与「被信号杀死」都没有退出码，必须留 None。
    #[test]
    fn exit_code_comes_only_from_command_failed() {
        let failed = anyhow::Error::new(crate::cmd::CommandFailed {
            code: Some(1),
            message: "安装失败".into(),
        });
        assert_eq!(exit_code_of(&failed), Some(1), "应取到退出码");
        assert_eq!(
            exit_code_of(&failed.context("外层包装")),
            Some(1),
            "context 包装不应挡住 downcast"
        );
        assert_eq!(exit_code_of(&anyhow::anyhow!("缺少参数 target")), None);
        // 文案里出现的数字不能当退出码用（防止退回「正则捞数字」的实现）
        assert_eq!(
            exit_code_of(&anyhow::anyhow!("安装失败（退出码 1），详见服务端日志")),
            None
        );
    }

    /// 同组作业才排队：Package / Docker 有闸门，无互斥的分组不排队。
    #[test]
    fn gate_covers_contending_groups_only() {
        assert!(gate(LockGroup::Package).is_some());
        assert!(gate(LockGroup::Docker).is_some());
        assert!(gate(LockGroup::None).is_none());
        assert!(gate(LockGroup::Systemd).is_none());
    }

    /// 每个作业类型都要有确定的分组映射：装机类归 Package、拉镜像归 Docker、
    /// 不经 cmd 的备份不排队。写错分组会让作业层排队与实际锁不一致。
    #[test]
    fn every_kind_maps_to_its_group() {
        for k in [
            JobKind::PkgInstall,
            JobKind::PkgRemove,
            JobKind::PkgUpgrade,
            JobKind::PkgSysupgrade,
            JobKind::PkgUpdate,
            JobKind::DockerInstall,
        ] {
            assert_eq!(k.lock_group(), LockGroup::Package, "{k:?} 应归 Package");
        }
        assert_eq!(JobKind::DockerPull.lock_group(), LockGroup::Docker);
        assert_eq!(JobKind::BackupCreate.lock_group(), LockGroup::None);
    }
}
