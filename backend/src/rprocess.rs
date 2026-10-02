//! 主机进程视图。
//!
//! 进程页的用途是服务器维护（系统服务、内核线程、存储与网络），所以默认只返回
//! 系统进程；被托管的负载——容器里的进程、部署在 /opt 或 /home 下的应用——归到
//! 应用侧，由实例页单独呈现。两侧都能按需切换，维护时想看全量不必换页面。

use anyhow::Result;

use crate::monitor::ProcessInfo;
use crate::AppState;

/// 进程范围筛选
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    System,
    App,
    All,
}

impl Scope {
    /// 解析 `?scope=` 参数。无法识别一律回落到 System——
    /// 默认给全量会把应用进程混进系统视图，正好违背这个筛选存在的理由。
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("app") => Self::App,
            Some("all") => Self::All,
            _ => Self::System,
        }
    }
}

/// 容器运行时在 `/proc/<pid>/cgroup` 里留下的路径特征。
///
/// 必须先按 cgroup 摘出容器进程：容器内的可执行文件同样位于 `/usr/bin`，只按路径
/// 判定会把它们误归为系统进程，而这恰恰是本功能要避免的。
///
/// 匹配的是**容器实例的 cgroup 路径**（`docker-<id>.scope`、`/docker/<id>` 等），
/// 不是运行时名字本身——写成 `docker` 会把 `docker.service` 里的 dockerd 也算成
/// 容器进程，而它恰恰属于系统侧。
const RUNTIME_HINTS: [&str; 6] = [
    "docker-",
    "/docker/",
    "cri-containerd-",
    "crio-",
    "libpod-",
    "kubepods",
];

/// 系统目录前缀。落在这里的可执行文件按系统组件对待。
const SYSTEM_PREFIXES: [&str; 6] = ["/usr/", "/bin/", "/sbin/", "/lib/", "/lib64/", "/etc/"];

/// 位于根目录、但确属系统基础设施的可执行文件。
///
/// `/init` 是 WSL 发行版的引导与 Windows 互操作中继：`init-systemd(...)`、
/// `SessionLeader`、`Relay(...)` 以及 9p 文件服务器（挂载 `/mnt/c` 等）都指向它。
/// 它不在任何系统目录下，只按前缀判定会被当成用户应用，在实例页里冒出一张
/// 名为 `init` 的卡片 —— 而它既不是被托管的负载，也不该占用应用视图。
const SYSTEM_EXES: [&str; 1] = ["/init"];

/// 进程是否跑在容器内
pub fn in_container(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .is_ok_and(|text| RUNTIME_HINTS.iter().any(|h| text.contains(h)))
}

/// 可执行路径是否属于系统组件。
/// 空路径（内核线程没有 exe，或权限不足读不到）算系统 —— 这类进程都属于主机本身。
fn is_system_exe(exe: &str) -> bool {
    exe.is_empty()
        || SYSTEM_EXES.contains(&exe)
        || SYSTEM_PREFIXES.iter().any(|p| exe.starts_with(p))
}

/// 判定进程属于应用侧还是系统侧
pub fn classify(p: &ProcessInfo) -> Scope {
    if in_container(p.pid) || !is_system_exe(&p.exe) {
        Scope::App
    } else {
        Scope::System
    }
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

/// 进程是否属于该实例。实例 id 形如 `container:<短ID>` / `app:<可执行路径>`。
fn owns(p: &ProcessInfo, instance: &str) -> bool {
    if let Some(short) = instance.strip_prefix(crate::instances::CONTAINER_PREFIX) {
        return cgroup_owns(p.pid, short);
    }
    if let Some(exe) = instance.strip_prefix(crate::instances::APP_PREFIX) {
        return p.exe == exe;
    }
    false
}

/// 获取进程列表（按 CPU 降序）。
///
/// `instance` 非空时以它为准，`scope` 不再参与——从实例卡片点进来的意图是
/// 「看这个实例的进程」，此时再叠一层系统/应用筛选只会让人困惑。
pub async fn list(
    state: &AppState,
    scope: Scope,
    instance: Option<&str>,
) -> Result<Vec<ProcessInfo>> {
    let all = {
        let mut m = state.monitor.lock().await;
        m.processes()
    };

    // 分类与实例归属都要逐个读 /proc/<pid>/cgroup，放在锁外做
    if let Some(id) = instance {
        return Ok(all.into_iter().filter(|p| owns(p, id)).collect());
    }
    if scope == Scope::All {
        return Ok(all);
    }
    Ok(all.into_iter().filter(|p| classify(p) == scope).collect())
}

/// 结束指定进程
pub async fn kill(state: &AppState, pid: u32) -> Result<bool> {
    let mut m = state.monitor.lock().await;
    Ok(m.kill_process(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_at(pid: u32, exe: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: "x".into(),
            cpu: 0.0,
            mem: 0,
            user: "root".into(),
            status: "Run".into(),
            exe: exe.into(),
        }
    }

    #[test]
    fn scope_parse_defaults_to_system() {
        assert_eq!(Scope::parse(None), Scope::System);
        assert_eq!(Scope::parse(Some("app")), Scope::App);
        assert_eq!(Scope::parse(Some("all")), Scope::All);
        assert_eq!(
            Scope::parse(Some("nonsense")),
            Scope::System,
            "无法识别的取值必须回落到默认的系统视图"
        );
    }

    #[test]
    fn system_exe_covers_kernel_threads_and_system_dirs() {
        // 内核线程没有 exe，它们属于主机本身
        assert!(is_system_exe(""));
        assert!(is_system_exe("/usr/bin/systemd"));
        assert!(is_system_exe("/usr/lib/systemd/systemd-journald"));
        assert!(is_system_exe("/sbin/init"));
        assert!(!is_system_exe("/opt/app/run"));
        assert!(!is_system_exe("/home/deploy/bin/svc"));
    }

    #[test]
    fn system_exe_covers_wsl_bootstrap_at_the_root() {
        // /init 是 WSL 的引导与 Windows 互操作中继，位于根目录所以不在任何系统
        // 前缀之下；它属于主机基础设施，不能以「应用」的身份出现在实例页里。
        assert!(is_system_exe("/init"));
        assert!(
            !is_system_exe("/myapp"),
            "根目录下的普通可执行文件仍应算应用"
        );
    }

    #[test]
    fn classify_sends_foreign_paths_to_app() {
        // 故意用一个不存在的 pid：cgroup 读不到 → in_container 为 false，
        // 这样断言只依赖路径判据，不会因测试机本身跑在容器里而翻转。
        assert_eq!(classify(&proc_at(999_999, "/usr/bin/sshd")), Scope::System);
        assert_eq!(classify(&proc_at(999_999, "/opt/app/run")), Scope::App);
    }
}
