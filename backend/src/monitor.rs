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

/// 监控历史保留时长：7 天
const RETENTION_SECS: i64 = 7 * 24 * 3600;

/// 后台采样任务：每 5 秒写入一条监控历史，每小时清理过期数据
pub fn spawn_sampler(state: AppState) {
    tokio::spawn(async move {
        let db = state.db.clone();
        let monitor = state.monitor.clone();
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
            // 每 720 次采样（约 1 小时）清理一次过期历史
            ticks += 1;
            if ticks >= 720 {
                ticks = 0;
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                match db.prune_metrics_async(now - RETENTION_SECS).await {
                    Ok(n) if n > 0 => tracing::info!("清理过期监控历史 {n} 条"),
                    Err(e) => tracing::warn!("清理监控历史失败：{e}"),
                    _ => {}
                }
            }
        }
    });
}
