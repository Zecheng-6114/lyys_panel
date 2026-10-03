use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::Serialize;
use sysinfo::{Disks, Networks, Pid, ProcessesToUpdate, System, ThreadKind};

use crate::AppState;

/// 单个挂载点的容量概况（只含真实块设备，虚拟/透传的已被 `is_virtual_fs` 滤掉）
#[derive(Clone, Serialize)]
pub struct MountInfo {
    pub mount: String,
    pub fs: String,
    /// 占用百分比
    pub pct: f64,
}

/// 单次系统快照（对外 API 返回结构）
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub ts: i64,
    /// CPU 使用率（%）
    pub cpu: f64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub disk_used: i64,
    pub disk_total: i64,
    /// 网络速率（字节/秒），由相邻两次采样的累计值差值计算
    pub net_in_per_sec: i64,
    pub net_out_per_sec: i64,
    /// 平均负载 1/5/15 分钟
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    /// CPU 核心数（负载条按 核数×100% 归一）
    pub cpu_cores: usize,
    /// 运行时长（秒）
    pub uptime: u64,
    pub swap_used: i64,
    pub swap_total: i64,
    /// 进程数：数 /proc 下的数字目录，单次 readdir，刻意不走 sysinfo 全表刷新
    pub procs: usize,
    /// 磁盘 I/O 速率（字节/秒），由 /proc/diskstats 扇区计数的差值换算
    pub disk_read_per_sec: i64,
    pub disk_write_per_sec: i64,
    /// 分区数，以及占用率最高的那个挂载点 —— 「磁盘」卡看总量，
    /// 这张看最紧张的一个：多分区机器上聚合值会把快满的分区平均掉。
    pub disk_partitions: usize,
    pub disk_worst_mount: String,
    pub disk_worst_pct: f64,
    /// 计入统计的挂载点明细 —— 分区构成因机器而异，「占用对不上」时
    /// 需要能看到到底算了哪几个
    pub disk_mounts: Vec<MountInfo>,
    /// 机器信息（进程内不变）：编译架构、发行版、内核版本
    pub arch: String,
    pub distro: String,
    pub kernel: String,
}

/// 磁盘容量枚举的复用间隔。容量变化以分钟计，跟着 5 秒的采样节奏刷新纯属浪费。
const DISK_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// 监控采集器：持有 sysinfo 实例并维护网络速率计算所需的上一次采样
pub struct Monitor {
    sys: System,
    last_net: HashMap<String, (u64, u64)>,
    last_instant: Instant,
    /// 磁盘容量与分区缓存：为 None 时强制刷新
    disk_cache: Option<(Instant, DiskStat)>,
    /// 上次磁盘 I/O 累计值 `(读字节, 写字节, 采样时刻)`，用于换算速率
    last_disk_io: Option<(u64, u64, Instant)>,
    /// 发行版与内核版本：进程生命周期内不变，首次读取后缓存
    os_name: Option<String>,
    kernel: Option<String>,
    /// 最近一次采样的结果，供 `latest()` 读取
    last_snapshot: Option<Snapshot>,
}

/// 磁盘容量统计（`disk_cache` 的载荷）
#[derive(Clone)]
struct DiskStat {
    used: i64,
    total: i64,
    partitions: usize,
    /// 占用率最高的挂载点及其百分比
    worst_mount: String,
    worst_pct: f64,
    mounts: Vec<MountInfo>,
}

impl Monitor {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Self {
            sys,
            disk_cache: None,
            last_disk_io: None,
            last_net: HashMap::new(),
            last_instant: Instant::now(),
            os_name: None,
            kernel: None,
            last_snapshot: None,
        }
    }

    /// 刷新并生成快照
    pub fn snapshot(&mut self) -> Snapshot {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        // 这里刻意**不**刷新进程表：那是一次全系统 /proc 扫描，是快照里最贵的操作，
        // 而前端唯一用到它的 process_count 已经移除（见 Snapshot 的注释）。
        // 需要进程数据时走 processes()，它自己会刷新。

        let cpu = self.sys.global_cpu_usage() as f64;
        let mem_used = self.sys.used_memory() as i64;
        let mem_total = self.sys.total_memory() as i64;

        let disk = self.disk_usage();
        let (disk_used, disk_total) = (disk.used, disk.total);

        // 网络速率 = (本次累计 - 上次累计) / 时间间隔
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_instant)
            .as_secs_f64()
            .max(1e-6);
        let mut net_in = 0u64;
        let mut net_out = 0u64;
        let mut new_net = HashMap::new();
        for (name, data) in Networks::new_with_refreshed_list().iter() {
            let (li, lo) = self.last_net.get(name).copied().unwrap_or((0, 0));
            net_in += data.received().saturating_sub(li);
            net_out += data.transmitted().saturating_sub(lo);
            new_net.insert(name.clone(), (data.received(), data.transmitted()));
        }
        self.last_net = new_net;
        self.last_instant = now;

        // 磁盘 I/O 速率：与网络同一套差值算法，共用本次的 now
        let (io_read, io_write) = Self::disk_io_total();
        let (prev_read, prev_write, prev_at) = self.last_disk_io.unwrap_or((io_read, io_write, now));
        let io_dt = now.duration_since(prev_at).as_secs_f64().max(1e-6);
        self.last_disk_io = Some((io_read, io_write, now));

        let (os_name, kernel) = self.os_info();

        let snap = Snapshot {
            ts: time::OffsetDateTime::now_utc().unix_timestamp(),
            cpu,
            mem_used,
            mem_total,
            disk_used,
            disk_total,
            net_in_per_sec: (net_in as f64 / dt) as i64,
            net_out_per_sec: (net_out as f64 / dt) as i64,
            load1: System::load_average().one,
            load5: System::load_average().five,
            load15: System::load_average().fifteen,
            cpu_cores: self.sys.cpus().len(),
            uptime: System::uptime(),
            swap_used: self.sys.used_swap() as i64,
            swap_total: self.sys.total_swap() as i64,
            procs: std::fs::read_dir("/proc")
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| {
                            let n = e.file_name();
                            let s = n.to_string_lossy();
                            !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
                        })
                        .count()
                })
                .unwrap_or(0),
            disk_read_per_sec: (io_read.saturating_sub(prev_read) as f64 / io_dt) as i64,
            disk_write_per_sec: (io_write.saturating_sub(prev_write) as f64 / io_dt) as i64,
            disk_partitions: disk.partitions,
            disk_worst_mount: disk.worst_mount,
            disk_worst_pct: disk.worst_pct,
            disk_mounts: disk.mounts,
            arch: std::env::consts::ARCH.to_string(),
            distro: os_name,
            kernel,
        };
        // 缓存下来给 latest() 用：速率类指标由本次调用算了差值，别的
        // 消费者再算一次会把采样窗口切碎（见 latest() 的说明）。
        self.last_snapshot = Some(snap.clone());
        snap
    }

    /// 最近一次采样的快照。
    ///
    /// **速率类指标（网络、磁盘 I/O）靠「本次累计 − 上次累计」得出，因此
    /// 每次调用 `snapshot()` 都会重置差值基准。** 它只能由后台采样器按固定
    /// 的 5 秒节奏调用；其余消费者（HTTP 接口、诊断）一律走这里读缓存。
    ///
    /// 曾经的写法是接口直接调 `snapshot()`：前端每来一次请求就把采样窗口
    /// 切成「距上次后台采样 0.1 秒」的一小段，窗口内几乎没有新增 I/O，
    /// 于是速率恒被算成 0 —— 网络卡其实也有同样的问题，只是一直没暴露。
    pub fn latest(&mut self) -> Snapshot {
        match &self.last_snapshot {
            Some(s) => s.clone(),
            // 服务刚起、采样器还没跑过第一轮时现场补一次，避免首屏全零
            None => self.snapshot(),
        }
    }

    /// 磁盘容量与分区快照：枚举所有挂载点需要逐次 statfs，而容量变化很慢，
    /// 没必要跟着 5 秒的采样节奏刷新 —— 缓存一段时间复用即可。
    ///
    /// 顺便挑出占用率最高的挂载点：聚合总量会把「某个分区快满了」
    /// 平均成「整体不高」，而那才是运维真正要找的信息。
    fn disk_usage(&mut self) -> DiskStat {
        if let Some((at, stat)) = &self.disk_cache
            && at.elapsed() < DISK_REFRESH_INTERVAL {
                return stat.clone();
            }
        let mut used = 0i64;
        let mut total = 0i64;
        let mut partitions = 0usize;
        let mut worst_mount = String::new();
        let mut worst_pct = 0f64;
        let mut mounts = Vec::new();
        let mut seen_devices: HashSet<String> = HashSet::new();
        for d in Disks::new_with_refreshed_list().iter() {
            let t = d.total_space();
            let fs = d.file_system().to_string_lossy().into_owned();
            if t == 0 || is_virtual_fs(&fs) {
                continue;
            }
            // 同一块设备常被 bind mount 到多处：这台 WSL 上 /mnt/wslg/distro
            // 与 /var/lib/docker 都绑在根分区，三处占用率完全相同。按挂载点
            // 累加会把容量算成 3 倍（实测 3.24T，而设备只有 1007G）——
            // 百分比侥幸没变，绝对数值则是假的。按设备名去重。
            let dev = d.name().to_string_lossy().into_owned();
            if !dev.is_empty() && !seen_devices.insert(dev) {
                continue;
            }
            let busy = t - d.available_space();
            total += t as i64;
            used += busy as i64;
            partitions += 1;
            // 按占用率比较，不按剩余字节：100G 用到 90% 比 1T 用到 10%
            // 更该被看见
            let pct = busy as f64 / t as f64 * 100.0;
            mounts.push(MountInfo {
                mount: d.mount_point().to_string_lossy().into_owned(),
                fs,
                pct,
            });
            if pct > worst_pct {
                worst_pct = pct;
                worst_mount = mounts.last().map(|m| m.mount.clone()).unwrap_or_default();
            }
        }
        let stat = DiskStat {
            used,
            total,
            partitions,
            worst_mount,
            worst_pct,
            mounts,
        };
        self.disk_cache = Some((Instant::now(), stat.clone()));
        stat
    }

    /// 磁盘累计 I/O 字节数（读、写）。
    ///
    /// 直接读 /proc/diskstats，而不是走 sysinfo 的 `Disk::usage()`：后者要求
    /// 每次刷新都重新枚举挂载点（逐次 statfs），而 `/mnt/c` 这类 9p 挂载的
    /// statfs 并不便宜；diskstats 只是一次纯文件读，成本可忽略。
    fn disk_io_total() -> (u64, u64) {
        std::fs::read_to_string("/proc/diskstats")
            .map(|text| parse_diskstats(&text))
            .unwrap_or((0, 0))
    }

    /// 发行版与内核版本。
    ///
    /// 两者在进程生命周期内都不变：只需首次读 /etc/os-release 与 uname，
    /// 之后复用缓存，「系统」卡片每 5 秒刷新也不会重复解析。
    fn os_info(&mut self) -> (String, String) {
        if self.os_name.is_none() {
            self.os_name = System::name();
        }
        if self.kernel.is_none() {
            self.kernel = System::kernel_version();
        }
        (
            self.os_name.clone().unwrap_or_default(),
            self.kernel.clone().unwrap_or_default(),
        )
    }

    /// 进程列表（按 CPU 降序）。
    ///
    /// **只剔除用户线程**：sysinfo 会把同一进程的用户线程也列进来，每个线程条目
    /// 都带着整份进程内存与 CPU（它们共享地址空间）。不过滤的话，进程页会多出
    /// 成倍的「进程」，实例卡片求和更会得到「内存 400G / CPU 3500%」这类物理上
    /// 不可能的数字 —— 多线程应用（java、tokio 服务）尤其明显。
    ///
    /// 判定按 `thread_kind`：`Some(Userland)` 是用户线程（剔除）；
    /// `Some(Kernel)` 是内核线程 —— 它们是各自独立的进程（Tgid == Pid），
    /// 进程页本就该显示；`None` 是普通进程。
    pub fn processes(&mut self) -> Vec<ProcessInfo> {
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut list: Vec<ProcessInfo> = self
            .sys
            .processes()
            .iter()
            .filter(|(_, p)| p.thread_kind() != Some(ThreadKind::Userland))
            .map(|(pid, p)| ProcessInfo {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu: p.cpu_usage() as f64,
                mem: p.memory() as i64,
                user: p.user_id().map(|u| u.to_string()).unwrap_or_default(),
                status: p.status().to_string(),
                exe: p
                    .exe()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
            .collect();
        list.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        list
    }

    /// 结束进程（需要 root 或同用户权限）
    pub fn kill_process(&mut self, pid: u32) -> bool {
        self.sys
            .process(Pid::from_u32(pid))
            .is_some_and(|p| p.kill())
    }
}

/// 进程信息（对外 API 返回结构）
#[derive(Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f64,
    pub mem: i64,
    pub user: String,
    pub status: String,
    /// 可执行文件的绝对路径。内核线程没有 exe，权限不足时也读不到，
    /// 两种情况都留空串 —— 调用方（rprocess 的系统/应用判定）把空值当系统进程。
    pub exe: String,
}

/// 采样周期。2 秒一条 —— 仪表盘卡片与趋势图都跟着这个节奏走：
/// 放到 5 秒时前端再叠加一次轮询，最新点能滞后十几秒，看上去就是「几十秒才动一下」；
/// 再快则落库量与告警评估开销线性增长，2 秒已经看不出更高的刷新率了。
const SAMPLE_PERIOD: std::time::Duration = std::time::Duration::from_secs(2);

/// 后台采样任务：每 SAMPLE_PERIOD 写入一条监控历史，每小时聚合降采样 + 清理过期数据
/// （保留策略常量见 db.rs）；每小时评估告警规则、执行每日自动备份
pub fn spawn_sampler(state: AppState) {
    tokio::spawn(async move {
        let db = state.db.clone();
        let monitor = state.monitor.clone();
        let data_dir = state.data_dir.clone();
        // 3.3 告警：规则在设置页保存后即时重载（P2-2），评估在每次采样进行
        let mut rules = crate::alerts::load_rules(&db);
        let mut engine = crate::alerts::AlertEngine::new(&rules);
        // P2-2：监听重载信号。sender 意外消失时关掉该分支，退化为纯定时采样，
        // 否则 changed() 会立即返回 Err 造成忙循环。
        let mut alert_reload = state.alert_reload.subscribe();
        let mut reload_live = true;
        let mut ticks: u32 = 0;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(SAMPLE_PERIOD) => {}
                changed = alert_reload.changed(), if reload_live => {
                    if changed.is_err() {
                        reload_live = false;
                    } else {
                        rules = crate::alerts::load_rules(&db);
                        engine.resync(&rules);
                        tracing::info!("告警规则已即时重载（设置页变更）");
                    }
                    // 信号只负责重载规则，本次不采样，直接回到等待
                    continue;
                }
            }
            let snap = {
                let mut m = monitor.lock().await;
                m.snapshot()
            };
            if let Err(e) = db.insert_metric_async(&snap).await {
                tracing::warn!("写入监控采样失败：{e}");
            }
            // 3.3：评估告警规则（滞回状态机，只在翻转时出事件）
            let metrics = crate::alerts::Metrics {
                cpu: snap.cpu,
                mem_used: snap.mem_used,
                mem_total: snap.mem_total,
                disk_used: snap.disk_used,
                disk_total: snap.disk_total,
            };
            for ev in engine.evaluate(&rules, metrics) {
                let metric = crate::alerts::metric_label(ev.metric).to_string();
                let state_str = if ev.firing { "firing" } else { "resolved" };
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                if let Err(e) = db
                    .alert_event_add_async(
                        now,
                        metric.clone(),
                        ev.value,
                        ev.threshold,
                        state_str.into(),
                    )
                    .await
                {
                    tracing::warn!("写入告警事件失败：{e}");
                }
                let db2 = db.clone();
                let msg = format!(
                    "[LYYS 面板告警] {metric} {}：当前 {:.1}%，阈值 {:.0}%",
                    if ev.firing {
                        "超过"
                    } else {
                        "恢复到阈值以下"
                    },
                    ev.value,
                    ev.threshold
                );
                let notify = rules
                    .iter()
                    .find(|r| r.metric == ev.metric)
                    .map(|r| ev.firing || r.notify_resolve)
                    .unwrap_or(ev.firing);
                if notify {
                    tokio::spawn(async move {
                        if let Some(url) = crate::alerts::load_webhook(&db2) {
                            let client = reqwest::Client::new();
                            crate::alerts::send_webhook(&client, &url, &msg).await;
                        }
                    });
                }
                tracing::info!(
                    "告警事件：{metric} {:.1}% 阈值 {:.0}%（{state_str}）",
                    ev.value,
                    ev.threshold
                );
            }
            // 每 720 次采样（约 1 小时）执行一次保留策略
            ticks += 1;
            if ticks >= 720 {
                ticks = 0;
                // 兜底重载规则：正常路径已由 P2-2 的信号即时触发，这条只为
                // 「信号丢失」留后路，不可删除
                rules = crate::alerts::load_rules(&db);
                engine.resync(&rules);
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                if let Err(e) = db
                    .rollup_and_prune_async(
                        now - crate::db::RAW_RETENTION_SECS,
                        now - crate::db::HOURLY_RETENTION_SECS,
                    )
                    .await
                {
                    tracing::warn!("监控历史聚合清理失败：{e}");
                }
                // 2.3/2.4：过期会话与超期审计随手清理，防止两张表无限增长
                if let Err(e) = db.session_prune_async(now).await {
                    tracing::warn!("清理过期会话失败：{e}");
                }
                if let Err(e) = db
                    .audit_prune_async(now - crate::db::AUDIT_RETENTION_SECS)
                    .await
                {
                    tracing::warn!("清理过期审计日志失败：{e}");
                }
                // P2-3：清理过期认证状态，并把退避记录裁剪到上限内
                if let Err(e) = db
                    .auth_state_prune_async(now, crate::auth::MAX_THROTTLE_ENTRIES)
                    .await
                {
                    tracing::warn!("清理认证状态失败：{e}");
                }
                // 3.3：告警事件同样保留 90 天
                if let Err(e) = db
                    .alert_event_prune_async(now - crate::db::AUDIT_RETENTION_SECS)
                    .await
                {
                    tracing::warn!("清理过期告警事件失败：{e}");
                }
                // P2-1：作业行同样 90 天；只删终态行，running/pending 不会被误清
                if let Err(e) = db
                    .job_prune_async(now - crate::db::AUDIT_RETENTION_SECS)
                    .await
                {
                    tracing::warn!("清理过期作业失败：{e}");
                }
                // 3.1：每日自动备份（VACUUM 是重 IO 操作，放 spawn_blocking）
                let db3 = db.clone();
                let dir3 = data_dir.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    crate::backup::maybe_daily_backup(&db3, &dir3)
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r)
                {
                    tracing::warn!("每日自动备份失败：{e}");
                }
            }
        }
    });
}

/// 虚拟 / 透传文件系统：容量不属于本机磁盘，计入会污染「磁盘」占用率与
/// 「分区」卡的最紧张挂载点。
///
/// 在 WSL 上尤其明显：`/mnt/c`、`/usr/lib/wsl/drivers` 都是 Windows 分区的
/// 9p 透传。把它们和 Linux 根分区一起求和，会得到「13.8T 用了 3.4T」这种
/// 既不是宿主、也不是 guest 的数字，而且「最紧张的分区」会指向一个
/// Windows 驱动目录 —— 对运维毫无意义。
const VIRTUAL_FS: [&str; 13] = [
    "tmpfs",    // 内存盘（/run、/dev/shm）
    "devtmpfs", // /dev
    "devfs",
    "squashfs", // snap 等只读镜像
    "9p",       // WSL 宿主目录透传
    "drvfs",    // 同上（旧版 WSL）
    "vboxsf",   // VirtualBox 共享目录
    "virtiofs",
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "overlay", // 容器叠加层
];

/// 是否为虚拟/透传文件系统。`fuse` 按前缀匹配 —— fuse.sshfs 这类
/// 用户态文件系统种类太多，逐个列举不如一律排除。
fn is_virtual_fs(fs: &str) -> bool {
    let fs = fs.to_ascii_lowercase();
    VIRTUAL_FS.contains(&fs.as_str()) || fs.starts_with("fuse")
}

/// 解析 /proc/diskstats，累加读写扇区计数并换算成字节。
///
/// 字段布局（内核 5.x 起）：0 major / 1 minor / 2 name / 3 reads / 4 merged /
/// 5 sectors_read / 6 ms_reading / 7 writes / 8 merged / 9 sectors_written …
/// 扇区固定按 512 字节换算 —— 内核的扇区单位恒为 512，与设备物理扇区无关。
///
/// 跳过三类设备，否则会重复计数或计入噪声：
///   `loop*` / `ram*` —— 回环与内存盘，不是真实 I/O
///   `dm-*`           —— device-mapper 映射层，其底层物理设备已被单独计数
///   `sr*`            —— 光驱
fn parse_diskstats(text: &str) -> (u64, u64) {
    let mut read = 0u64;
    let mut write = 0u64;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let name = f[2];
        if name.starts_with("loop")
            || name.starts_with("ram")
            || name.starts_with("dm-")
            || name.starts_with("sr")
        {
            continue;
        }
        read += f[5].parse::<u64>().unwrap_or(0) * 512;
        write += f[9].parse::<u64>().unwrap_or(0) * 512;
    }
    (read, write)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程列表里只允许出现线程组主线程（Tgid == Pid）。
    ///
    /// sysinfo 会把同一进程的**用户线程**也列进来，而每个线程条目都带着整份
    /// 进程内存与 CPU（共享地址空间）—— 不过滤的话，进程页会多出成倍的
    /// 「进程」，实例卡片按它求和更会得到「内存 400G / CPU 3507%」这类
    /// 物理上不可能的数字。内核线程（Tgid == Pid，只是没有用户态地址
    /// 空间）是各自独立的进程，必须保留。
    #[test]
    fn processes_never_include_userland_threads() {
        let mut m = Monitor::new();
        let listed = m.processes();
        assert!(!listed.is_empty(), "至少要能列出自己这个进程");

        // 每一项都必须是主线程：比对 /proc/<pid>/status 里的 Tgid
        for p in &listed {
            let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", p.pid)) else {
                continue; // 刚退出的进程，跳过
            };
            let tgid = status
                .lines()
                .find_map(|l| l.strip_prefix("Tgid:"))
                .and_then(|v| v.trim().parse::<u32>().ok());
            assert_eq!(
                tgid,
                Some(p.pid),
                "pid {} 是用户线程（属于 Tgid {tgid:?}），不该被当成进程列出",
                p.pid
            );
        }

        // 不能过滤过头：测试进程自己就是主线程，必须还在列表里
        let me = std::process::id();
        assert!(
            listed.iter().any(|p| p.pid == me),
            "测试进程自己是主线程，不该被滤掉"
        );
    }

    #[test]
    fn diskstats_sums_real_devices_only() {
        // 真实 /proc/diskstats 的行格式（字段数按内核 5.x 对齐）
        let text = concat!(
            "   8       0 sda 1000 0 2000 50 2000 0 4000 80 0 0 0 0 0 0\n",
            "   7       0 loop0 500 0 999999 10 0 0 0 0 0 0 0 0 0 0\n",
            " 253       0 dm-0 100 0 123456 5 100 0 654321 5 0 0 0 0 0 0\n",
            "  11       0 sr0 1 0 8 0 0 0 0 0 0 0 0 0 0 0\n",
        );
        let (read, write) = parse_diskstats(text);
        // 只累加 sda：2000 扇区读、4000 扇区写，各 ×512
        assert_eq!(read, 2000 * 512);
        assert_eq!(write, 4000 * 512);
    }

    #[test]
    fn diskstats_ignores_short_lines() {
        // 字段不足的行直接跳过，不能 panic（老内核或裁剪格式）
        assert_eq!(parse_diskstats("8 0 sda 1 2 3"), (0, 0));
        assert_eq!(parse_diskstats(""), (0, 0));
    }

    #[test]
    fn virtual_filesystems_are_excluded_from_disk_totals() {
        // WSL 的 Windows 透传挂载、内存盘、容器层都不能算进磁盘容量
        assert!(is_virtual_fs("9p"));
        assert!(is_virtual_fs("drvfs"));
        assert!(is_virtual_fs("tmpfs"));
        assert!(is_virtual_fs("overlay"));
        // fuse 按前缀匹配：种类太多，一律排除
        assert!(is_virtual_fs("fuse.sshfs"));
        // 大小写不应影响判定（内核返回的写法不保证）
        assert!(is_virtual_fs("TMPFS"));
        // 真实文件系统必须保留
        assert!(!is_virtual_fs("ext4"));
        assert!(!is_virtual_fs("xfs"));
        assert!(!is_virtual_fs("btrfs"));
    }
}
