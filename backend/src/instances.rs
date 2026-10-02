//! 实例视图：把「被托管的东西」从系统里单独提出来。
//!
//! 两个来源都对应显式的管理边界——容器由 Docker 管理，服务由管理员写进 systemd
//! 单元；进程扫描不参与判定，避免漏掉「跑在系统目录下的解释器型应用」（如 java
//! 起的服务端），也避免把恰好在 /opt 下的进程凭空认成应用。
//!
//! 每块都能直达它自己的日志与进程。

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
    Service,
}

/// 实例（对外 API 返回结构）
#[derive(Serialize)]
pub struct Instance {
    /// 全局唯一，形如 `container:1a2b3c4d5e6f` / `service:minecraft.service`。
    /// 前端把它原样回传给进程页、日志页做范围限定。
    pub id: String,
    pub kind: InstanceKind,
    pub name: String,
    /// 容器为镜像名；服务为单元描述
    pub detail: String,
    pub state: String,
    /// 容器为端口映射；服务为空
    pub ports: String,
    /// 容器为所属 compose 项目；服务为空
    pub project: String,
    pub cpu: String,
    pub mem: String,
    /// 关联的主机进程号；容器进程在容器内，此处为空
    pub pids: Vec<u32>,
}

/// 容器实例的 id 前缀
pub const CONTAINER_PREFIX: &str = "container:";
/// systemd 服务实例的 id 前缀
pub const SERVICE_PREFIX: &str = "service:";

/// 管理员定义 unit 的落盘目录。软件包自带的单元在 /usr/lib/systemd/system 下，
/// 属于「服务」页的范畴，不是被托管的负载。
const ADMIN_UNIT_DIRS: [&str; 2] = ["/etc/systemd/system/", "/run/systemd/system/"];

/// 构造容器实例 id
pub fn container_id(short: &str) -> String {
    format!("{CONTAINER_PREFIX}{short}")
}

/// 构造服务实例 id
pub fn service_id(unit: &str) -> String {
    format!("{SERVICE_PREFIX}{unit}")
}

/// unit 在管理员目录下的候选路径。模板单元（`foo@bar.service`）还要看模板文件
/// `foo@.service`，因为实例是从模板生成的，磁盘上只有模板。
fn admin_unit_paths(unit: &str) -> Vec<String> {
    let mut names = vec![unit.to_string()];
    if let (Some(at), Some(dot)) = (unit.find('@'), unit.rfind('.')) {
        if at < dot {
            names.push(format!("{}@{}", &unit[..at], &unit[dot..]));
        }
    }
    ADMIN_UNIT_DIRS
        .iter()
        .flat_map(|dir| names.iter().map(move |n| format!("{dir}{n}")))
        .collect()
}

/// unit 是否由管理员定义（unit 文件落在上面两个目录）
fn is_admin_unit(unit: &str) -> bool {
    admin_unit_paths(unit)
        .iter()
        .any(|p| std::path::Path::new(p).is_file())
}

/// 列出全部实例：容器在前，服务在后；同类内按名称排序保证顺序稳定。
pub async fn list(state: &AppState) -> Result<Vec<Instance>> {
    let mut out = Vec::new();

    // Docker 未安装或未启动时静默跳过——这类机器仍应有实例页，只是只有服务。
    // 这里不能把错误往外抛：一个未装 Docker 的服务器不该连实例列表都打不开。
    if let Ok(containers) = crate::docker::containers().await {
        out.extend(containers.into_iter().map(from_container));
    }

    // 只认管理员定义的单元；软件包自带的那些留在「服务」页，不重复进实例
    let units: Vec<crate::opservice::ServiceInfo> = match crate::opservice::list().await {
        Ok(all) => all.into_iter().filter(|u| is_admin_unit(&u.name)).collect(),
        Err(e) => {
            tracing::warn!("列出 systemd 服务失败，实例页仅显示容器：{e}");
            Vec::new()
        }
    };

    if !units.is_empty() {
        let procs = {
            let mut m = state.monitor.lock().await;
            m.processes()
        };
        // 每个进程只读一次 cgroup，再按单元归拢，避免按单元逐个重扫进程表
        let mut by_unit: BTreeMap<String, Vec<&ProcessInfo>> = BTreeMap::new();
        for p in &procs {
            if let Some(unit) = crate::rprocess::cgroup_unit(p.pid) {
                by_unit.entry(unit).or_default().push(p);
            }
        }
        out.extend(units.iter().map(|u| {
            let owned = by_unit.get(&u.name).map(Vec::as_slice).unwrap_or(&[]);
            from_service(u, owned)
        }));
    }

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

/// 服务单元 → 实例。
///
/// 已停止的单元没有归属进程，仍保留在列表里（对应容器的 exited 状态），
/// 此时不编造 CPU / 内存占用。
fn from_service(svc: &crate::opservice::ServiceInfo, procs: &[&ProcessInfo]) -> Instance {
    let mut pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
    pids.sort_unstable();
    let (cpu, mem) = if procs.is_empty() {
        (String::new(), String::new())
    } else {
        let cpu: f64 = procs.iter().map(|p| p.cpu).sum();
        let mem: i64 = procs.iter().map(|p| p.mem).sum();
        (format!("{cpu:.1}%"), fmt_bytes(mem))
    };
    Instance {
        id: service_id(&svc.name),
        kind: InstanceKind::Service,
        name: svc
            .name
            .strip_suffix(".service")
            .unwrap_or(&svc.name)
            .to_string(),
        detail: svc.description.clone(),
        state: if svc.active == "active" { "running" } else { "exited" }.into(),
        ports: String::new(),
        project: String::new(),
        cpu,
        mem,
        pids,
    }
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

    fn proc_at(pid: u32, cpu: f64, mem: i64) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: "java".into(),
            cpu,
            mem,
            user: "mc".into(),
            status: "Run".into(),
            exe: "/usr/bin/java".into(),
        }
    }

    fn svc(name: &str, active: &str) -> crate::opservice::ServiceInfo {
        crate::opservice::ServiceInfo {
            name: name.into(),
            load: "loaded".into(),
            active: active.into(),
            sub: "running".into(),
            description: "Minecraft 服务端".into(),
        }
    }

    #[test]
    fn service_instance_sums_its_processes() {
        let a = proc_at(999_001, 1.5, 1024);
        let b = proc_at(999_002, 2.5, 2048);
        let inst = from_service(&svc("minecraft.service", "active"), &[&a, &b]);

        assert_eq!(inst.id, "service:minecraft.service");
        assert_eq!(inst.kind, InstanceKind::Service);
        assert_eq!(inst.name, "minecraft");
        assert_eq!(inst.state, "running");
        assert_eq!(inst.pids, vec![999_001, 999_002]);
        assert_eq!(inst.cpu, "4.0%");
        assert_eq!(inst.mem, "3.0K");
    }

    #[test]
    fn stopped_service_stays_in_the_list_without_processes() {
        let inst = from_service(&svc("minecraft.service", "inactive"), &[]);
        assert_eq!(inst.state, "exited");
        assert!(inst.pids.is_empty());
        assert_eq!(inst.cpu, "", "没有归属进程时不编造占用");
        assert_eq!(inst.mem, "");
    }

    #[test]
    fn admin_unit_paths_cover_template_units() {
        let paths = admin_unit_paths("foo@bar.service");
        assert!(paths.contains(&"/etc/systemd/system/foo@bar.service".to_string()));
        assert!(paths.contains(&"/etc/systemd/system/foo@.service".to_string()));
        assert!(paths.contains(&"/run/systemd/system/foo@.service".to_string()));
    }

    #[test]
    fn admin_unit_paths_never_point_into_package_dirs() {
        // 发行版自带的单元在 /usr/lib/systemd/system 下，不该被认成实例
        let paths = admin_unit_paths("sshd.service");
        assert!(paths.contains(&"/etc/systemd/system/sshd.service".to_string()));
        assert!(!paths.iter().any(|p| p.starts_with("/usr/lib/")));
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
