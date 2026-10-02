//! 主机进程视图。
//!
//! 进程页是服务器的全量进程列表，用于维护——系统服务、内核线程、容器与应用进程
//! 一并可见。被托管的负载在实例页按容器 / systemd 服务归类呈现，实例卡片可以把
//! 这里限定到单个实例。

use anyhow::Result;

use crate::monitor::ProcessInfo;
use crate::AppState;

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
}
