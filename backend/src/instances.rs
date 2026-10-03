//! 实例视图：把「被托管的东西」从系统里单独提出来。
//!
//! 两个来源都对应显式的管理边界——容器由 Docker 管理，服务由管理员写进 systemd
//! 单元；进程扫描不参与判定，避免漏掉「跑在系统目录下的解释器型应用」（如 java
//! 起的服务端），也避免把恰好在 /opt 下的进程凭空认成应用。
//!
//! 每块都能直达它自己的日志与进程。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

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
    if let (Some(at), Some(dot)) = (unit.find('@'), unit.rfind('.'))
        && at < dot {
            names.push(format!("{}@{}", &unit[..at], &unit[dot..]));
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

/// cgroup v2 挂载点（systemd 服务单元在 system.slice 下）
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// cgroup v1 的 memory 控制器目录。Debian 11 等仍是 hybrid，文件同名不同路径；
/// 只为内存用量保留这条兜底。
const CGROUP_V1_MEMORY: &str = "/sys/fs/cgroup/memory";

/// 单元的 cgroup 目录候选：v2 优先，v1 兜底
fn cgroup_dirs(unit: &str) -> [String; 2] {
    [
        format!("{CGROUP_ROOT}/system.slice/{unit}"),
        format!("{CGROUP_V1_MEMORY}/system.slice/{unit}"),
    ]
}

/// 读单元的内存用量（字节）。v2 = memory.current，v1 = memory.usage_in_bytes。
///
/// **不用「所属进程 RSS 求和」**：RSS 把共享页按映射方个数重复计入 ——
/// 同一段共享内存被 N 个进程映射就统计 N 次，java 这类多进程/多线程应用的
/// 求和值能远超物理内存。cgroup 记账每个页只算一次，与 systemd 的
/// `MemoryCurrent`、`docker stats` 同源，读不到就返回 None（不编造数字）。
fn cgroup_memory_bytes(unit: &str) -> Option<u64> {
    let [v2, v1] = cgroup_dirs(unit);
    for (dir, file) in [(v2, "memory.current"), (v1, "memory.usage_in_bytes")] {
        if let Ok(s) = std::fs::read_to_string(format!("{dir}/{file}"))
            && let Ok(n) = s.trim().parse::<u64>() {
                return Some(n);
            }
    }
    None
}

/// 容器列表的后台采样周期。
///
/// `docker ps` + `docker stats` 各要 fork 一次 CLI，这台机器上光 CLI 启动就 ~170ms，
/// 让实例页每次请求都实打实付这一遍，请求就跟着 CLI 一起抖。改成后台按这个周期
/// 采样、接口只发快照后，接口延迟与容器数据的刷新速度解耦：想看多新就调这个周期，
/// 接口始终零成本返回快照。
const CONTAINER_SAMPLE_PERIOD: Duration = Duration::from_secs(2);

/// 后台采到的容器列表；一次都没成功过是 `None`。
static CONTAINER_SNAPSHOT: OnceLock<std::sync::Mutex<Option<Vec<crate::docker::ContainerInfo>>>>
    = OnceLock::new();

fn container_slot() -> &'static std::sync::Mutex<Option<Vec<crate::docker::ContainerInfo>>> {
    CONTAINER_SNAPSHOT.get_or_init(|| std::sync::Mutex::new(None))
}

/// docker 是否至少失败过一次。
///
/// 这不是「多少秒内不重试」那种时间窗口节流，而是一个状态位：CLI 不可达就是不可达
/// （缺权限 / 没装 / daemon 没起，都得人去处理），接口再同步试同一遍也只是重复白跑
/// 一遍 170ms。后台采样成功时自动清零， docker 一恢复就走回快照路径。
static CONTAINER_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

/// 取容器列表：有后台快照就直接发（这一次请求基本不花时间）；
/// 一次都没成功过时退回同步查询，保证首次打开实例页容器区不是空的。
async fn containers_cached() -> Result<Vec<crate::docker::ContainerInfo>> {
    // 用 std 的锁，守卫不会跨 await：取快照 + 克隆这个 vec 是纯内存操作，
    // 持锁期间不可能被别的任务抢走，不存在死锁面。
    if let Ok(g) = container_slot().lock()
        && let Some(list) = g.as_ref() {
            return Ok(list.clone());
        }
    // 后台已经试过并且失败：docker 不可达，这里再同步试一遍也是同一遍白跑，按空列表走
    if CONTAINER_UNAVAILABLE.load(Ordering::Relaxed) {
        return Ok(Vec::new());
    }
    // 进程刚启动、后台还没跑完第一轮：同步查一次，保证首次打开实例页容器区不是空的
    match crate::docker::containers().await {
        Ok(list) => {
            if let Ok(mut g) = container_slot().lock() {
                *g = Some(list.clone());
            }
            CONTAINER_UNAVAILABLE.store(false, Ordering::Relaxed);
            Ok(list)
        }
        Err(_) => {
            CONTAINER_UNAVAILABLE.store(true, Ordering::Relaxed);
            Ok(Vec::new())
        }
    }
}

/// 后台按周期采样容器。失败时保留上一次的快照 —— 刚停掉一个容器的那一两秒里，
/// 页面不该整块容器区消失；只有成功过才会有快照可言。
pub fn spawn_container_sampler() {
    let handle = tokio::spawn(async {
        loop {
            match crate::docker::containers().await {
                Ok(list) => {
                    if let Ok(mut g) = container_slot().lock() {
                        *g = Some(list);
                    }
                    CONTAINER_UNAVAILABLE.store(false, Ordering::Relaxed);
                }
                Err(e) => {
                    tracing::warn!("后台采样容器列表失败：{e}");
                    CONTAINER_UNAVAILABLE.store(true, Ordering::Relaxed);
                }
            }
            tokio::time::sleep(CONTAINER_SAMPLE_PERIOD).await;
        }
    });
    // 句柄一 drop 任务就被取消，让它活到进程结束（句柄本身之后不再被查询）
    let _sampler = Box::leak(Box::new(handle));
}

/// 列出全部实例：容器在前，服务在后；同类内按名称排序保证顺序稳定。
pub async fn list(state: &AppState) -> Result<Vec<Instance>> {
    let mut out = Vec::new();

    // Docker 未安装或未启动时静默跳过——这类机器仍应有实例页，只是只有服务。
    // 这里不能把错误往外抛：一个未装 Docker 的服务器不该连实例列表都打不开。
    if let Ok(containers) = containers_cached().await {
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
        // CPU 用单元内进程之和：跟着上面这一次数过的进程表走，不再额外等一个窗口。
        // 速率要「两次采样求差」，那就得阻塞一个窗口才出结果 —— 实例页每次轮询都
        // 为几个百分点的估算付 300ms，不划算；而进程页显示的也是同一批进程、同一个
        // 采样窗口，两处的数对得上才不会让人怀疑。
        out.extend(units.iter().map(|u| {
            let owned = by_unit.get(&u.name).map(Vec::as_slice).unwrap_or(&[]);
            let cpu = Some(owned.iter().map(|p| p.cpu).sum::<f64>());
            from_service(u, owned, cgroup_memory_bytes(&u.name), cpu)
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
///
/// 两个占用口径不同，别混着读：
///
/// - **内存走 cgroup 记账**（`mem_bytes` 由调用方读 `memory.current` 传入）。
///   不用「归属进程 RSS 求和」：RSS 把共享页按映射方个数重复计入 —— 同一段
///   共享内存被 N 个进程映射就统计 N 次，java 这类多进程/多线程应用的求和值
///   能远超物理内存。cgroup 每个页只算一次，与 systemd 的 `MemoryCurrent`、
///   `docker stats` 同源。
/// - **CPU 走单元内进程求和**（`cpu_pct`），而不是 cgroup 的 `cpu.stat`。速率
///   要「两次采样求差」，那就得阻塞一个窗口才出结果；实例页每次轮询都为几个
///   百分点的估算付 300ms 不划算，而进程页显示的正是同一批进程、同一个采样
///   窗口，两处取同一口径数字才对得上、不会让人怀疑。
///
/// 两个参数取不到就留空 —— 宁可没有数字，也不给一个错的。
fn from_service(
    svc: &crate::opservice::ServiceInfo,
    procs: &[&ProcessInfo],
    mem_bytes: Option<u64>,
    cpu_pct: Option<f64>,
) -> Instance {
    let mut pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
    pids.sort_unstable();
    let (cpu, mem) = if procs.is_empty() {
        (String::new(), String::new())
    } else {
        (
            cpu_pct.map(|c| format!("{c:.1}%")).unwrap_or_default(),
            mem_bytes.map(fmt_bytes).unwrap_or_default(),
        )
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

/// 字节数转可读文本。单位带 `i`（KiB / MiB / GiB）—— 同一页的容器卡片直接来自
/// `docker stats`，它用的就是二进制单位，两侧写法必须一致，否则「12.7M」与
/// 「35.92MiB」并排出现，会让人以为是两套口径。
fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
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
    fn service_instance_reports_cgroup_usage_and_its_processes() {
        let a = proc_at(999_001, 1.5, 1024);
        let b = proc_at(999_002, 2.5, 2048);
        // 内存与 CPU 由调用方从 cgroup 取好传入（3.0KiB = 3072B）
        let inst = from_service(&svc("minecraft.service", "active"), &[&a, &b], Some(3072), Some(4.0));

        assert_eq!(inst.id, "service:minecraft.service");
        assert_eq!(inst.kind, InstanceKind::Service);
        assert_eq!(inst.name, "minecraft");
        assert_eq!(inst.state, "running");
        assert_eq!(inst.pids, vec![999_001, 999_002]);
        assert_eq!(inst.cpu, "4.0%");
        assert_eq!(inst.mem, "3.0KiB");
    }

    #[test]
    fn cgroup_usage_missing_leaves_fields_empty_instead_of_lying() {
        // 取不到 cgroup 数据时留空 —— 宁可没有数字，也不把进程 RSS 求和当答案
        let a = proc_at(999_003, 1.0, 4096);
        let inst = from_service(&svc("x.service", "active"), &[&a], None, None);
        assert_eq!(inst.cpu, "");
        assert_eq!(inst.mem, "");
        assert_eq!(inst.pids, vec![999_003]);
    }

    #[test]
    fn stopped_service_stays_in_the_list_without_processes() {
        let inst = from_service(&svc("minecraft.service", "inactive"), &[], None, None);
        assert_eq!(inst.state, "exited");
        assert!(inst.pids.is_empty());
        assert_eq!(inst.cpu, "", "没有归属进程时不编造占用");
        assert_eq!(inst.mem, "");
    }

    #[test]
    fn fmt_bytes_uses_binary_units_so_it_matches_docker_stats() {
        assert_eq!(fmt_bytes(512), "512B");
        assert_eq!(fmt_bytes(3072), "3.0KiB");
        assert_eq!(fmt_bytes(19_976_192), "19.1MiB");
        assert_eq!(fmt_bytes(2_470_000_000), "2.3GiB");
    }

    #[test]
    fn cgroup_dirs_cover_v2_then_v1() {
        let [v2, v1] = cgroup_dirs("minecraft.service");
        assert_eq!(v2, "/sys/fs/cgroup/system.slice/minecraft.service");
        assert_eq!(v1, "/sys/fs/cgroup/memory/system.slice/minecraft.service");
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
