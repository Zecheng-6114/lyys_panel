//! 主机进程视图。
//!
//! 进程页是服务器的全量进程列表，用于维护——系统服务、内核线程、容器与应用进程
//! 一并可见。被托管的负载在实例页按容器 / systemd 服务归类呈现，实例卡片可以把
//! 这里限定到单个实例。

use anyhow::Result;

use crate::AppState;
use crate::monitor::ProcessInfo;

/// 读取 `/proc/<pid>/cgroup`，取出进程所属的 systemd service 单元。
pub fn cgroup_unit(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|text| unit_from_cgroup_text(&text))
}

/// 从 `/proc/<pid>/cgroup` 的文本里解析所属 service 单元（纯函数，便于单测）。
///
/// 两种格式都写成 `<路径>/<单元名>`：
///
/// ```text
/// 0::/system.slice/minecraft.service                （cgroup v2）
/// 1:name=systemd:/system.slice/minecraft.service    （cgroup v1）
/// ```
///
/// 每行按 `:` 最多切三段，第三段是路径；多行命中时取最深的一个单元——嵌套在
/// `user@1000.service` 下的用户单元应归到自己，而不是归到 user 管理器。
fn unit_from_cgroup_text(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.splitn(3, ':').nth(2))
        .flat_map(|path| path.split('/').map(str::to_string).collect::<Vec<_>>())
        .rfind(|c| c.ends_with(".service"))
}

/// 进程是否挂在指定容器下。
///
/// cgroup 里的容器 ID 是全 64 位，容器列表给的是 12 位短 ID，
/// 所以按 `<运行时>-<短ID>` 前缀匹配，而不是要求完整相等。
fn cgroup_owns(pid: u32, short_id: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
        return false;
    };
    ["docker-", "cri-containerd-", "crio-", "libpod-"]
        .iter()
        .any(|prefix| text.contains(&format!("{prefix}{short_id}")))
}

/// 进程是否属于该实例。实例 id 形如 `container:<短ID>` / `service:<单元名>`。
fn owns(p: &ProcessInfo, instance: &str) -> bool {
    if let Some(short) = instance.strip_prefix(crate::instances::CONTAINER_PREFIX) {
        return cgroup_owns(p.pid, short);
    }
    if let Some(unit) = instance.strip_prefix(crate::instances::SERVICE_PREFIX) {
        return cgroup_unit(p.pid).as_deref() == Some(unit);
    }
    false
}

/// 获取进程列表（按 CPU 降序）。
///
/// `instance` 非空时只看该实例下的进程，否则返回全量。
pub async fn list(state: &AppState, instance: Option<&str>) -> Result<Vec<ProcessInfo>> {
    let all = {
        let mut m = state.monitor.lock().await;
        m.processes()
    };

    // 实例归属要逐个读 /proc/<pid>/cgroup，放在锁外做
    match instance {
        Some(id) => Ok(all.into_iter().filter(|p| owns(p, id)).collect()),
        None => Ok(all),
    }
}

/// 进程详情（仪表盘 Top 5 点开抽屉用）。
///
/// 全部来自 `/proc/<pid>/` 的直读，**只读**：这里不碰任何写操作。
/// 每个字段都可能缺失（进程随时退出、权限不足、或该内核没提供该文件），
/// 所以一律用 `Option` —— 拿不到就显示「—」，不用 0 冒充。
#[derive(serde::Serialize, Default)]
pub struct ProcessDetail {
    pub pid: u32,
    pub name: String,
    /// 命令行（`/proc/<pid>/cmdline`，NUL 分隔）。内核线程为空
    pub cmdline: Option<String>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub user: String,
    pub status: String,
    /// 父进程 PID（`/proc/<pid>/stat` 第 4 个字段）
    pub ppid: Option<u32>,
    /// 常驻内存（KB）
    pub rss_kb: Option<u64>,
    /// 虚拟内存（KB）
    pub vsz_kb: Option<u64>,
    pub threads: Option<u32>,
    /// 进程启动时刻（Unix 秒）
    pub started_at: Option<i64>,
    /// 累计读写的物理磁盘字节（`/proc/<pid>/io`，可能因权限不足读不到）
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
    /// 打开的 fd 数（`/proc/<pid>/fd` 条目数）
    pub open_fds: Option<usize>,
    /// 所属 systemd 单元（取不到为 None）
    pub unit: Option<String>,
    /// 是否仍在运行（详情接口里重新确认一次，避免对已退出的 PID 做操作）
    pub alive: bool,
}

/// 读进程详情。`pid` 不存在时返回 `None`（而不是报错）——
/// 进程随时可能退出，这是正常情况，调用方按「已退出」呈现。
pub fn detail(pid: u32) -> Option<ProcessDetail> {
    let base = format!("/proc/{pid}");
    if !std::path::Path::new(&base).exists() {
        return None;
    }

    let read = |f: &str| std::fs::read_to_string(format!("{base}/{f}")).ok();

    // cmdline 是 NUL 分隔的 argv；内核线程是空文件
    let cmdline = read("cmdline").and_then(|s| {
        let joined = s
            .split('\0')
            .filter(|a| !a.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    });

    // status 里的 VmRSS / VmSize / Threads
    let status_text = read("status").unwrap_or_default();
    let field_kb = |key: &str| -> Option<u64> {
        status_text
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
    };
    let rss_kb = field_kb("VmRSS:");
    let vsz_kb = field_kb("VmSize:");
    let threads = status_text
        .lines()
        .find(|l| l.starts_with("Threads:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok());
    let status_name = status_text
        .lines()
        .find(|l| l.starts_with("State:"))
        .map(|l| l["State:".len()..].trim().to_string())
        .unwrap_or_default();
    // 真实 uid 在 status 里，比读 /proc/<pid> 属主更直接
    let uid = status_text
        .lines()
        .find(|l| l.starts_with("Uid:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u32>().ok());
    let user = uid
        .and_then(username_of)
        .unwrap_or_else(|| uid.map(|u| u.to_string()).unwrap_or_else(|| "—".into()));

    // stat：ppid 与启动时刻。第 2 个字段是 `(comm)`，可能含空格与括号，
    // 所以先定位最后一个 ')' 再切剩余字段 —— 不能用 split_whitespace 直接取
    let (ppid, started_at) = match read("stat") {
        Some(s) => {
            let rest = s
                .rsplit_once(')')
                .map(|(_, r)| r.trim().to_string())
                .unwrap_or_default();
            let f: Vec<&str> = rest.split_whitespace().collect();
            // rest 的第 1 个是 state，所以 ppid 在下标 1；starttime（第 22 字段）
            // 对应 rest 下标 19，单位是时钟滴答
            let ppid = f.get(1).and_then(|v| v.parse().ok());
            let started = f
                .get(19)
                .and_then(|v| v.parse::<u64>().ok())
                .and_then(start_time_from_ticks);
            (ppid, started)
        }
        None => (None, None),
    };

    // io：read_bytes / write_bytes（物理设备 I/O）
    let io_text = read("io").unwrap_or_default();
    let io_field = |key: &str| -> Option<u64> {
        io_text
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
    };

    let open_fds = std::fs::read_dir(format!("{base}/fd"))
        .ok()
        .map(|d| d.count());

    Some(ProcessDetail {
        pid,
        name: read("comm")
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        cmdline,
        exe: std::fs::read_link(format!("{base}/exe"))
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
        cwd: std::fs::read_link(format!("{base}/cwd"))
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
        user,
        status: status_name,
        ppid,
        rss_kb,
        vsz_kb,
        threads,
        started_at,
        io_read_bytes: io_field("read_bytes:"),
        io_write_bytes: io_field("write_bytes:"),
        open_fds,
        unit: cgroup_unit(pid),
        alive: true,
    })
}

/// 时钟滴答 → 进程启动的 Unix 秒。
///
/// `/proc/<pid>/stat` 的 starttime 是「自系统启动起的时钟滴答数」，
/// 需要 `sysconf(_SC_CLK_TCK)` 与 `/proc/uptime` 换算。取不到就返回 None ——
/// 猜一个值会显示成错误的启动时间，比显示「—」更糟。
///
/// `cfg(unix)` 是必须的：`sysconf` 只在 Unix 目标上存在，而开发机是 Windows
/// （面板本身只在 Linux 跑，但 Windows 上必须编得过才能改代码）。
#[cfg(unix)]
fn start_time_from_ticks(ticks: u64) -> Option<i64> {
    // SAFETY: sysconf 只读系统常量，无副作用
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    let uptime_secs: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let boot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64()
        - uptime_secs;
    Some((boot + ticks as f64 / hz as f64) as i64)
}

/// 非 Unix（Windows 开发机）：没有 /proc 也没有 sysconf，直接给 None。
#[cfg(not(unix))]
fn start_time_from_ticks(_ticks: u64) -> Option<i64> {
    None
}

/// uid → 用户名。只用于展示，取不到就回退成数字 uid。
fn username_of(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() >= 3 && f[2].parse::<u32>().ok() == Some(uid) {
            Some(f[0].to_string())
        } else {
            None
        }
    })
}

/// 结束指定进程
pub async fn kill(state: &AppState, pid: u32) -> Result<bool> {
    let mut m = state.monitor.lock().await;
    Ok(m.kill_process(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_at(pid: u32) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: "x".into(),
            cpu: 0.0,
            mem: 0,
            user: "root".into(),
            status: "Run".into(),
            exe: "/usr/bin/java".into(),
        }
    }

    #[test]
    fn unit_from_cgroup_text_reads_v2_path() {
        assert_eq!(
            unit_from_cgroup_text("0::/system.slice/minecraft.service").as_deref(),
            Some("minecraft.service")
        );
    }

    #[test]
    fn unit_from_cgroup_text_reads_v1_path() {
        let text = "12:cpuset:/\n1:name=systemd:/system.slice/minecraft.service\n";
        assert_eq!(
            unit_from_cgroup_text(text).as_deref(),
            Some("minecraft.service")
        );
    }

    #[test]
    fn unit_from_cgroup_text_takes_the_deepest_unit() {
        // 嵌套在用户管理器下的单元要归到自己，而不是归到 user@1000.service
        let text = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/foo.service";
        assert_eq!(unit_from_cgroup_text(text).as_deref(), Some("foo.service"));
    }

    #[test]
    fn unit_from_cgroup_text_ignores_scopes_and_bare_paths() {
        assert_eq!(unit_from_cgroup_text("0::/\n"), None);
        // 容器与 PID 1 都不落在 service 单元上
        assert_eq!(
            unit_from_cgroup_text("0::/system.slice/docker-1a2b3c4d.scope"),
            None
        );
        assert_eq!(unit_from_cgroup_text("0::/init.scope"), None);
    }

    #[test]
    fn owns_rejects_unknown_instance_ids() {
        // 用不存在的 pid：两条分支都读不到 cgroup，只验证 id 前缀的解析
        let p = proc_at(999_999);
        assert!(!owns(&p, "bogus:/opt/foo/bin/foo"));
        assert!(!owns(&p, "service:minecraft.service"));
        assert!(!owns(&p, "container:1a2b3c4d"));
    }

    #[test]
    fn owns_matches_the_unit_a_live_process_belongs_to() {
        let me = std::process::id();
        let p = proc_at(me);
        // 测试进程通常挂在 session scope 而非 service 单元下，因此只在取到单元时验证正向分支
        if let Some(unit) = cgroup_unit(me) {
            assert!(owns(&p, &format!("service:{unit}")));
            assert!(!owns(&p, "service:other.service"));
        }
    }

    /// 详情读取：拿测试进程自己当样本，验证 /proc 解析链路（只在 Linux 上有意义）。
    ///
    /// 不能在 Windows 上断言：那边没有 /proc，`detail` 会返回 None。
    #[cfg(target_os = "linux")]
    #[test]
    fn detail_reads_the_current_process() {
        let me = std::process::id();
        let d = detail(me).expect("当前进程的详情必须能读到");
        assert_eq!(d.pid, me);
        assert!(d.alive);
        assert!(!d.name.is_empty(), "进程名不该为空");
        // 常驻内存必然大于 0（测试进程也要占内存）
        assert!(d.rss_kb.unwrap_or(0) > 0, "VmRSS 应可读且大于 0");
        // 线程数至少 1
        assert!(d.threads.unwrap_or(0) >= 1);
        // uid 解析成用户名（找不到用户名时回退成数字，也不该是「—」）
        assert_ne!(d.user, "—", "uid 应能解析出用户名或数字");
        // 打开的 fd：测试进程至少有 0/1/2
        assert!(d.open_fds.unwrap_or(0) >= 1, "至少应有标准流 fd");
    }

    /// 不存在的 PID 返回 None（而不是报错）：进程随时可能退出，这是正常情况。
    #[test]
    fn detail_returns_none_for_a_missing_pid() {
        // Linux 上 /proc/sys/kernel/pid_max 上限远小于这个值
        assert!(detail(9_999_999).is_none());
    }
}
