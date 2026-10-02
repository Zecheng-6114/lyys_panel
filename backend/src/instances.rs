//! 实例视图：把「被托管的东西」从系统里单独提出来。
//!
//! 两个来源：
//!   - 容器：Docker 管理的负载，天然隔离，有独立的文件树与日志
//!   - 主机应用：可执行文件不在系统目录、也不在容器里的进程，按可执行路径聚合
//!
//! 划分口径与进程页的系统/应用一致——进程页留给系统维护，应用侧集中到这里，
//! 每块都能直达它自己的文件、日志与进程。

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::monitor::ProcessInfo;
use crate::AppState;

/// 实例来源
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum InstanceKind {
    Container,
    Host,
}

/// 实例（对外 API 返回结构）
#[derive(Serialize)]
pub struct Instance {
    /// 全局唯一，形如 `container:1a2b3c4d5e6f` / `app:/opt/foo/bin/foo`。
    /// 前端把它原样回传给进程页、文件页、日志页做范围限定。
    pub id: String,
    pub kind: InstanceKind,
    pub name: String,
    /// 容器为镜像名；主机应用为可执行文件路径
    pub detail: String,
    pub state: String,
    /// 容器为端口映射；主机应用为空
    pub ports: String,
    /// 容器为所属 compose 项目；主机应用为空
    pub project: String,
    pub cpu: String,
    pub mem: String,
    /// 关联的主机进程号；容器进程在容器内，此处为空
    pub pids: Vec<u32>,
}

/// 容器实例的 id 前缀
pub const CONTAINER_PREFIX: &str = "container:";
/// 主机应用实例的 id 前缀
pub const APP_PREFIX: &str = "app:";

/// 构造容器实例 id
pub fn container_id(short: &str) -> String {
    format!("{CONTAINER_PREFIX}{short}")
}

/// 构造主机应用实例 id
pub fn app_id(exe: &str) -> String {
    format!("{APP_PREFIX}{exe}")
}

/// 列出全部实例：容器在前，主机应用在后；同类内按名称排序保证顺序稳定。
pub async fn list(state: &AppState) -> Result<Vec<Instance>> {
    let mut out = Vec::new();

    // Docker 未安装或未启动时静默跳过——这类机器仍应有实例页，只是只有主机应用。
    // 这里不能把错误往外抛：一个未装 Docker 的服务器不该连应用列表都打不开。
    if let Ok(containers) = crate::docker::containers().await {
        out.extend(containers.into_iter().map(from_container));
    }

    let procs = {
        let mut m = state.monitor.lock().await;
        m.processes()
    };
    out.extend(aggregate_host_apps(&procs));

    out.sort_by(|a, b| {
        let a_container = a.kind == InstanceKind::Container;
        let b_container = b.kind == InstanceKind::Container;
        b_container.cmp(&a_container).then_with(|| a.name.cmp(&b.name))
    });

    Ok(out)
}

/// 容器 → 实例
fn from_container(c: crate::docker::ContainerInfo) -> Instance {
    Instance {
        id: container_id(&c.id),
        kind: InstanceKind::Container,
        name: c.name,
        detail: c.image,
        state: c.state,
        ports: c.ports,
        project: c.project,
        cpu: c.cpu,
        mem: c.mem,
        pids: Vec::new(),
    }
}

/// 把非系统进程按可执行路径聚合成应用实例。
///
/// 一个应用常有多个进程（master + workers），按路径合并才对得上「一个应用」这个概念；
/// 按 pid 铺开只会把实例列表变成第二个进程列表。
fn aggregate_host_apps(procs: &[ProcessInfo]) -> Vec<Instance> {
    let mut grouped: BTreeMap<String, Vec<&ProcessInfo>> = BTreeMap::new();
    for p in procs {
        if crate::rprocess::classify(p) != crate::rprocess::Scope::App {
            continue;
        }
        // 容器进程归容器的进程视图；读不到 exe 的进程没有可用于聚合的身份，都不入列
        if p.exe.is_empty() || crate::rprocess::in_container(p.pid) {
            continue;
        }
        grouped.entry(p.exe.clone()).or_default().push(p);
    }

    grouped
        .into_iter()
        .map(|(exe, list)| {
            let cpu: f64 = list.iter().map(|p| p.cpu).sum();
            let mem: i64 = list.iter().map(|p| p.mem).sum();
            let name = exe.rsplit('/').next().unwrap_or(&exe).to_string();
            let mut pids: Vec<u32> = list.iter().map(|p| p.pid).collect();
            pids.sort_unstable();
            Instance {
                id: app_id(&exe),
                kind: InstanceKind::Host,
                name,
                detail: exe,
                state: "running".into(),
                ports: String::new(),
                project: String::new(),
                cpu: format!("{cpu:.1}%"),
                mem: fmt_bytes(mem),
                pids,
            }
        })
        .collect()
}

/// 字节数转可读文本，与容器侧 `docker stats` 的风格保持一致
fn fmt_bytes(n: i64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_at(exe: &str, pid: u32, cpu: f64, mem: i64) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: exe.rsplit('/').next().unwrap_or(exe).into(),
            cpu,
            mem,
            user: "app".into(),
            status: "Run".into(),
            exe: exe.into(),
        }
    }

    #[test]
    fn same_exe_collapses_into_one_instance() {
        // 不存在的 pid：cgroup 读不到，判定只走路径分支
        let procs = vec![
            proc_at("/opt/foo/bin/foo", 999_001, 1.5, 1024),
            proc_at("/opt/foo/bin/foo", 999_002, 2.5, 2048),
            proc_at("/usr/bin/sshd", 999_003, 9.0, 4096),
        ];
        let list = aggregate_host_apps(&procs);

        assert_eq!(list.len(), 1, "系统进程不进实例列表，同名可执行文件合并为一条");
        let app = &list[0];
        assert_eq!(app.name, "foo");
        assert_eq!(app.detail, "/opt/foo/bin/foo");
        assert_eq!(app.pids, vec![999_001, 999_002]);
        assert_eq!(app.cpu, "4.0%");
        assert_eq!(app.mem, "3.0K");
        assert_eq!(app.id, "app:/opt/foo/bin/foo");
    }

    #[test]
    fn processes_without_exe_are_skipped() {
        // 读不到 exe 的进程没有可用于聚合的身份，不能凭空造出一个实例
        let procs = vec![proc_at("", 999_004, 1.0, 10)];
        assert!(aggregate_host_apps(&procs).is_empty());
    }

    #[test]
    fn container_instance_carries_its_id_prefix() {
        let inst = from_container(crate::docker::ContainerInfo {
            id: "1a2b3c4d5e6f".into(),
            name: "web".into(),
            image: "nginx:latest".into(),
            state: "running".into(),
            status: "Up 3 hours".into(),
            ports: "0.0.0.0:8080->80/tcp".into(),
            running_for: "3 hours".into(),
            cpu: "0.5%".into(),
            mem: "12.3MiB".into(),
            project: String::new(),
        });
        assert_eq!(inst.id, "container:1a2b3c4d5e6f");
        assert_eq!(inst.kind, InstanceKind::Container);
        assert_eq!(inst.detail, "nginx:latest");
    }
}
