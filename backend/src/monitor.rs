use std::collections::HashMap;
use std::time::Instant;

use serde::Serialize;
use sysinfo::{Disks, Networks, Pid, ProcessesToUpdate, System};

use crate::AppState;

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
}

/// 磁盘容量枚举的复用间隔。容量变化以分钟计，跟着 5 秒的采样节奏刷新纯属浪费。
const DISK_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// 监控采集器：持有 sysinfo 实例并维护网络速率计算所需的上一次采样
pub struct Monitor {
    sys: System,
    last_net: HashMap<String, (u64, u64)>,
    last_instant: Instant,
    /// 磁盘容量缓存：`(刷新时刻, 已用, 总量)`，为 None 时强制刷新
    disk_cache: Option<(Instant, i64, i64)>,
}

impl Monitor {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Self {
            sys,
            disk_cache: None,
            last_net: HashMap::new(),
            last_instant: Instant::now(),
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

        let (disk_used, disk_total) = self.disk_usage();

        // 网络速率 = (本次累计 - 上次累计) / 时间间隔
        let now = Instant::now();
        let dt = now.duration_since(self.last_instant).as_secs_f64().max(1e-6);
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

        Snapshot {
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
        }
    }

    /// 磁盘容量快照：枚举所有挂载点需要逐次 statfs，而容量变化很慢，
    /// 没必要跟着 5 秒的采样节奏刷新 —— 缓存一段时间复用即可。
    fn disk_usage(&mut self) -> (i64, i64) {
        if let Some((at, used, total)) = self.disk_cache {
            if at.elapsed() < DISK_REFRESH_INTERVAL {
                return (used, total);
            }
        }
        let mut used = 0i64;
        let mut total = 0i64;
        for d in Disks::new_with_refreshed_list().iter() {
            total += d.total_space() as i64;
            used += (d.total_space() - d.available_space()) as i64;
        }
        self.disk_cache = Some((Instant::now(), used, total));
        (used, total)
    }

    /// 进程列表（按 CPU 降序）
    pub fn processes(&mut self) -> Vec<ProcessInfo> {
        self.sys.refresh_processes(
            ProcessesToUpdate::All,
            true,
        );
        let mut list: Vec<ProcessInfo> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| ProcessInfo {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu: p.cpu_usage() as f64,
                mem: p.memory() as i64,
                user: p.user_id().map(|u| u.to_string()).unwrap_or_default(),
                status: p.status().to_string(),
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
}

/// 后台采样任务：每 5 秒写入一条监控历史，每小时聚合降采样 + 清理过期数据
/// （保留策略常量见 db.rs）；每小时评估告警规则、执行每日自动备份
pub fn spawn_sampler(state: AppState) {
    tokio::spawn(async move {
        let db = state.db.clone();
        let monitor = state.monitor.clone();
        let data_dir = state.data_dir.clone();
        // 3.3 告警：规则每小时从 settings 重载一次，评估在每次采样进行
        let mut rules = crate::alerts::load_rules(&db);
        let mut engine = crate::alerts::AlertEngine::new(&rules);
        let mut ticks: u32 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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
                    .alert_event_add_async(now, metric.clone(), ev.value, ev.threshold, state_str.into())
                    .await
                {
                    tracing::warn!("写入告警事件失败：{e}");
                }
                let db2 = db.clone();
                let msg = format!(
                    "[LYYS 面板告警] {metric} {}：当前 {:.1}%，阈值 {:.0}%",
                    if ev.firing { "超过" } else { "恢复到阈值以下" },
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
                tracing::info!("告警事件：{metric} {:.1}% 阈值 {:.0}%（{state_str}）", ev.value, ev.threshold);
            }
            // 每 720 次采样（约 1 小时）执行一次保留策略
            ticks += 1;
            if ticks >= 720 {
                ticks = 0;
                // 重载规则（设置页改动即时生效上限为 1 小时延迟）
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
                // 3.3：告警事件同样保留 90 天
                if let Err(e) = db
                    .alert_event_prune_async(now - crate::db::AUDIT_RETENTION_SECS)
                    .await
                {
                    tracing::warn!("清理过期告警事件失败：{e}");
                }
                // 3.1：每日自动备份（VACUUM 是重 IO 操作，放 spawn_blocking）
                let db3 = db.clone();
                let dir3 = data_dir.clone();
                if let Err(e) =
                    tokio::task::spawn_blocking(move || crate::backup::maybe_daily_backup(&db3, &dir3))
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
