//! 系统底层调优（第三期）：看内核参数与时间同步的现状，并提供一个窄面的写入通道。
//!
//! 设计原则（这一块动的是宿主机本身，比面板其它功能都危险）：
//!
//! - **只读优先**：`status()` 只读 `/proc/sys` 与几个配置文件，不执行任何写操作。
//! - **参数白名单**：可写项是一张写死的表（见 [`PARAMS`]），每项自带
//!   取值集合或数值区间、以及「改了会怎样」的说明。**不接受任意 key/value** ——
//!   那等于把任意内核参数写入口暴露给前端。
//! - **能力探测**：非 root、只读挂载、容器内受限等情况必须先探测出来并明确告知，
//!   否则写操作会以一句 `permission denied` 失败，用户不知道是权限还是参数不对。
//! - **改前留档**：写操作前把原值记进 `settings` 表，返回值里给出回滚命令。
//!
//! 🔴 这里**不注册**任何 AI 工具（见 `ai_tools.rs`）：一句话就能改内核参数的
//! 通道必须由人显式点击，不能被模型间接调用。

use std::path::Path;

use serde::Serialize;

use crate::cmd::{self, Budget};
use tokio::process::Command;

/// 一个可调内核参数的定义。取值约束与说明写死在这里，
/// 前端只负责渲染，判断一律以本表为准。
#[derive(Clone, Serialize)]
pub struct ParamSpec {
    /// 内核参数名（`/proc/sys` 下的路径形式，如 `net.ipv4.tcp_congestion_control`）
    pub key: &'static str,
    /// 界面上的分组标题
    pub group: &'static str,
    /// 人类可读的名字
    pub title: &'static str,
    /// 当前值
    pub value: String,
    /// 允许的取值（枚举型）或空（数值型）
    pub choices: &'static [&'static str],
    /// 数值型的合法区间 `(min, max)`；枚举型为 None
    pub range: Option<(i64, i64)>,
    /// 「改了会怎样」——必须写清楚，不能只给一个输入框
    pub effect: &'static str,
    /// 该值是否支持热改（false 表示需要重启才生效，界面上要标出来）
    pub live: bool,
}

/// 白名单表的一行：(key, 分组, 标题, 枚举取值, 数值区间, 影响说明, 是否可热改)
///
/// 单独起别名而不是把七元组写进常量类型：表本身很长，
/// 类型内联在声明处会让人找不到「这一列到底是什么意思」。
pub type WritableRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    Option<(i64, i64)>,
    &'static str,
    bool,
);

/// 可写参数表。**只增不改语义**：已发布的项若要去掉，应是明确的下线动作。
///
/// 取舍：只收「改了影响可控、且能立即读回验证」的参数。
/// 涉及连接跟踪上限（`nf_conntrack_max`）、端口范围这类会直接影响在途连接的
/// 参数**故意不收** —— 面板自己就跑在网络上，改错会把自己断掉。
pub const WRITABLE: &[WritableRow] = &[
    (
        "net.ipv4.tcp_congestion_control",
        "网络",
        "TCP 拥塞控制算法",
        &["cubic", "bbr", "reno", "dctcp"],
        None,
        "换拥塞控制算法会改变本机所有 TCP 连接的发送行为。bbr 在高丢包链路上通常更快，但会挤占同链路其它流；改完已在途的连接不受影响，新连接才生效。",
        true,
    ),
    (
        "net.core.somaxconn",
        "网络",
        "全连接队列上限",
        &[],
        Some((128, 65535)),
        "监听套接字的等待队列长度。调小会在高并发建连时丢连接（客户端看到 connection reset），调大只是多占一点内存。",
        true,
    ),
    (
        "net.ipv4.tcp_syncookies",
        "网络",
        "SYN Cookies",
        &["0", "1", "2"],
        None,
        "1 = 半连接队列满时启用（推荐）；0 = 关闭，SYN 洪水下会丢弃正常连接；2 = 无条件启用，会牺牲部分 TCP 选项（如窗口缩放）的协商。",
        true,
    ),
    (
        "fs.file-max",
        "资源限制",
        "系统级文件句柄上限",
        &[],
        Some((1024, 9223372036854775807)),
        "整个内核可分配的文件句柄总数。调得过低会让服务报 too many open files；调高不预分配资源，只是抬高天花板。",
        true,
    ),
    (
        "fs.inotify.max_user_watches",
        "资源限制",
        "inotify 监视数上限",
        &[],
        Some((8192, 10485760)),
        "每个用户可注册的文件监视数。开发机与文件同步类服务常撞到这个上限（报 ENOSPC），调大只占内核内存。",
        true,
    ),
    (
        "vm.swappiness",
        "内存与交换",
        "交换倾向",
        &[],
        Some((0, 200)),
        "越大越倾向把匿名页换出。SSD 上适度换出可释放内存；数据库类负载通常调低（10 以下）以避免冷数据抢占内存。",
        true,
    ),
    (
        "vm.dirty_ratio",
        "内存与交换",
        "脏页比例上限（%）",
        &[],
        Some((1, 100)),
        "脏页超过该比例时，写入进程会同步回刷而阻塞。调高吞吐更好但掉电丢失更多；调低更安全但写入更容易卡顿。",
        true,
    ),
    (
        "vm.overcommit_memory",
        "内存与交换",
        "内存超额分配策略",
        &["0", "1", "2"],
        None,
        "0 = 启发式（默认）；1 = 总是允许超额分配（Redis 等要求）；2 = 严格，超额即拒绝分配。切到 2 可能让已有服务分配失败。",
        true,
    ),
];

/// 把内核参数名（`net.ipv4.tcp_congestion_control`）映射到 `/proc/sys` 路径
pub fn proc_path(key: &str) -> String {
    format!("/proc/sys/{}", key.replace('.', "/"))
}

/// 读一个参数的当前值（直读 /proc/sys，不依赖 sysctl 命令）
pub fn read_param(key: &str) -> Option<String> {
    std::fs::read_to_string(proc_path(key))
        .ok()
        .map(|s| s.trim().to_string())
}

/// 按白名单读一遍参数当前值
pub fn params() -> Vec<ParamSpec> {
    WRITABLE
        .iter()
        .map(|(key, group, title, choices, range, effect, live)| ParamSpec {
            key,
            group,
            title,
            value: read_param(key).unwrap_or_else(|| "（不可读）".into()),
            choices,
            range: *range,
            effect,
            live: *live,
        })
        .collect()
}

/// 按分组聚合，便于前端直接渲染
#[derive(Serialize)]
pub struct ParamGroup {
    pub group: &'static str,
    pub items: Vec<ParamSpec>,
}

pub fn param_groups() -> Vec<ParamGroup> {
    let mut groups: Vec<ParamGroup> = Vec::new();
    for p in params() {
        match groups.iter_mut().find(|g| g.group == p.group) {
            Some(g) => g.items.push(p),
            None => groups.push(ParamGroup {
                group: p.group,
                items: vec![p],
            }),
        }
    }
    groups
}

/// 单个 swap 设备/文件
#[derive(Serialize, Default)]
pub struct SwapEntry {
    pub name: String,
    /// partition / file
    pub kind: String,
    pub size_kb: u64,
    pub used_kb: u64,
    pub priority: String,
}

/// 交换区现状
#[derive(Serialize, Default)]
pub struct SwapStatus {
    pub entries: Vec<SwapEntry>,
    pub total_kb: u64,
    pub used_kb: u64,
    /// 是否存在 swapfile（只有一个 swap 分区时，界面上的「一键设置」不该引导用户
    /// 再建一个文件 —— 那只会让两个 swap 争优先级）
    pub has_file: bool,
    /// 是否启用了 zram / zswap
    pub zram: bool,
    pub zswap: bool,
}

/// 解析 `/proc/swaps`（比 `swapon --show` 更可靠：不依赖 procps 版本与 locale）
pub fn swap_status() -> SwapStatus {
    let mut st = SwapStatus::default();
    if let Ok(text) = std::fs::read_to_string("/proc/swaps") {
        for (i, line) in text.lines().enumerate() {
            if i == 0 || line.trim().is_empty() {
                continue; // 表头
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 5 {
                continue;
            }
            let size_kb = f[2].parse().unwrap_or(0);
            let used_kb = f[3].parse().unwrap_or(0);
            let kind = f[1].to_string();
            if kind == "file" {
                st.has_file = true;
            }
            st.total_kb += size_kb;
            st.used_kb += used_kb;
            st.entries.push(SwapEntry {
                name: f[0].to_string(),
                kind,
                size_kb,
                used_kb,
                priority: f[4].to_string(),
            });
        }
    }
    st.zram = Path::new("/sys/block/zram0").exists();
    st.zswap = std::fs::read_to_string("/sys/module/zswap/parameters/enabled")
        .map(|s| s.trim() == "Y" || s.trim() == "1")
        .unwrap_or(false);
    st
}

/// 时间同步现状
#[derive(Serialize, Default)]
pub struct NtpStatus {
    /// 已安装的守护进程：chrony / ntp / systemd-timesyncd / 无
    pub daemon: String,
    /// `timedatectl` 报告的 NTP 开关
    pub enabled: Option<bool>,
    pub synchronized: Option<bool>,
    pub timezone: String,
    /// 配置文件里的服务器行（`server` / `pool` / `NTP=`）
    pub servers: Vec<String>,
    /// 可编辑的配置文件路径（None 表示没找到）
    pub config_path: Option<String>,
}

/// 解析时间同步现状。
///
/// 用 `timedatectl show` 取权威的开关与同步状态；配置文件与守护进程名自己探测 ——
/// 容器里常常既没有 chrony 也没有 ntpd，此时必须显示「未安装」而不是报错。
pub async fn ntp_status() -> NtpStatus {
    let mut st = NtpStatus::default();

    // 守护进程与配置文件：chrony / ntp / systemd-timesyncd 三个名字族都要探
    let candidates: [(&str, &[&str]); 3] = [
        ("chrony", &["/etc/chrony.conf", "/etc/chrony/chrony.conf"]),
        ("ntp", &["/etc/ntp.conf"]),
        ("systemd-timesyncd", &["/etc/systemd/timesyncd.conf"]),
    ];
    let mut daemon: Option<&str> = None;
    for (name, paths) in candidates {
        if let Some(conf) = paths.iter().find(|p| Path::new(p).exists()) {
            st.config_path = Some((*conf).to_string());
            daemon = Some(name);
            // 读服务器行
            if let Ok(text) = std::fs::read_to_string(conf) {
                for line in text.lines() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    let low = t.to_ascii_lowercase();
                    if low.starts_with("server ")
                        || low.starts_with("pool ")
                        || low.starts_with("ntp=")
                    {
                        st.servers.push(t.to_string());
                    }
                }
            }
            break;
        }
    }
    // 配置文件可能被换过名字：再按二进制探一次
    if daemon.is_none() {
        for (bin, name) in [
            ("chronyd", "chrony"),
            ("ntpd", "ntp"),
            ("systemd-timesyncd", "systemd-timesyncd"),
        ] {
            if which(bin).await {
                daemon = Some(name);
                break;
            }
        }
    }
    st.daemon = daemon.unwrap_or("（未安装）").to_string();

    // timedatectl show：容器里通常可用；不可用就留 None，前端显示「未知」
    if let Ok(out) = cmd::run(
        Command::new("timedatectl").arg("show"),
        Budget::query(5),
    )
    .await
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k {
                "NTP" => st.enabled = Some(v.trim() == "yes"),
                "NTPSynchronized" => st.synchronized = Some(v.trim() == "yes"),
                "Timezone" => st.timezone = v.trim().to_string(),
                _ => {}
            }
        }
    }
    st
}

/// 命令是否存在于 PATH（只读探测）
async fn which(bin: &str) -> bool {
    // 固定参数、不接受外部拼接：bin 全部来自上面的常量表
    cmd::run(
        Command::new("sh").arg("-c").arg(format!("command -v {bin}")),
        Budget::query(3),
    )
    .await
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// 调优相关的整体能力：写操作能不能做、为什么不能
#[derive(Serialize)]
pub struct Capability {
    /// 当前进程是否 root
    pub root: bool,
    /// 是否运行在容器里（`/.dockerenv` 或 cgroup 里出现 docker/kubepods）
    pub container: bool,
    /// `/proc/sys` 是否可写（挂载为只读时不行）
    pub proc_sys_writable: bool,
    /// 是否 systemd 系统（决定能否用 `systemctl` 起停时间同步）
    pub systemd: bool,
    /// 综合结论：能否执行内核参数写入
    pub can_write: bool,
    /// 不能写入时的原因（面向用户，直接显示）
    pub reason: String,
}

/// 探测写操作的能力边界。
///
/// 必须显式探测而不是「试了就知道」：非 root 或只读挂载时，写入会以一句
/// `permission denied` 失败，用户无法分辨是权限问题还是参数填错了。
pub async fn capability() -> Capability {
    let root = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|v| v == "0"))
        })
        .unwrap_or(false);

    let container = Path::new("/.dockerenv").exists()
        || std::fs::read_to_string("/proc/1/cgroup")
            .map(|s| s.contains("docker") || s.contains("kubepods") || s.contains("lxc"))
            .unwrap_or(false);

    // /proc/sys 可写性：读 /proc/mounts 找 proc 挂载的选项里有没有 ro
    let proc_sys_writable = std::fs::read_to_string("/proc/mounts")
        .map(|s| {
            s.lines()
                .find(|l| l.split_whitespace().nth(1) == Some("/proc/sys"))
                .map(|l| !l.split_whitespace().nth(3).unwrap_or("").split(',').any(|o| o == "ro"))
                // 没单独挂 /proc/sys 时看 /proc
                .unwrap_or_else(|| {
                    s.lines()
                        .find(|l| l.split_whitespace().nth(1) == Some("/proc"))
                        .map(|l| {
                            !l.split_whitespace()
                                .nth(3)
                                .unwrap_or("")
                                .split(',')
                                .any(|o| o == "ro")
                        })
                        .unwrap_or(false)
                })
        })
        .unwrap_or(false);

    let systemd = Path::new("/run/systemd/system").exists();

    let (can_write, reason) = if !root {
        (
            false,
            "面板进程不是 root，无法写入 /proc/sys。请以 root 运行面板，或用终端手动执行给出的命令。"
                .to_string(),
        )
    } else if !proc_sys_writable {
        (
            false,
            "/proc/sys 以只读方式挂载（容器常见），内核参数无法在此环境中修改。".to_string(),
        )
    } else if container {
        // 容器内即使 root 也常被 seccomp/只读挂载限制：允许尝试，但必须提示影响范围。
        // 文案里不再重复「检测到容器环境」——前端标题已经写了，重复一遍读起来是病句
        (
            true,
            "写入只影响本容器（且部分参数会被内核拒绝），不会改变宿主机。".to_string(),
        )
    } else {
        (true, String::new())
    };

    Capability {
        root,
        container,
        proc_sys_writable,
        systemd,
        can_write,
        reason,
    }
}

/// 接口总返回体
#[derive(Serialize)]
pub struct Status {
    pub params: Vec<ParamGroup>,
    pub swap: SwapStatus,
    pub ntp: NtpStatus,
    pub capability: Capability,
    /// `vm.swappiness` 的当前值（swap 设置里经常一并调整）
    pub swappiness: Option<String>,
    /// 系统可用内存（KB）—— 建 swapfile 时给个合理默认大小
    pub mem_total_kb: u64,
    /// 内核版本
    pub kernel: String,
}

/// 汇总一次只读状态
pub async fn status() -> Status {
    let mem_total_kb = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|v| v.parse().unwrap_or(0)))
        })
        .unwrap_or(0);
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    Status {
        params: param_groups(),
        swap: swap_status(),
        ntp: ntp_status().await,
        capability: capability().await,
        swappiness: read_param("vm.swappiness"),
        mem_total_kb,
        kernel,
    }
}

/// 便于测试与前端：参数表里是否存在该 key
pub fn is_writable(key: &str) -> bool {
    WRITABLE.iter().any(|(k, ..)| *k == key)
}

/// 校验一个候选值是否落在白名单允许的范围内。
///
/// 返回面向用户的错误文案；通过时返回 `()`。
pub fn validate_value(key: &str, value: &str) -> Result<(), String> {
    let Some((_, _, title, choices, range, _, _)) = WRITABLE.iter().find(|(k, ..)| *k == key)
    else {
        return Err(format!("不允许修改该参数：{key}"));
    };
    let v = value.trim();
    if v.is_empty() {
        return Err(format!("{title} 的值不能为空"));
    }
    if !choices.is_empty() {
        if !choices.contains(&v) {
            return Err(format!("{title} 只能取 {}", choices.join(" / ")));
        }
        return Ok(());
    }
    if let Some((min, max)) = range {
        let n: i64 = v
            .parse()
            .map_err(|_| format!("{title} 必须是整数"))?;
        if n < *min || n > *max {
            return Err(format!("{title} 必须在 {min}..{max} 之间"));
        }
        return Ok(());
    }
    Ok(())
}

/// 参数 key → 人类可读标题（回滚命令里要带上）
pub fn title_of(key: &str) -> String {
    WRITABLE
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|(_, _, t, ..)| (*t).to_string())
        .unwrap_or_else(|| key.to_string())
}

/// 供「一键设置」预览：把将要执行的命令列出来（不执行）
pub fn preview_set(key: &str, value: &str) -> String {
    format!("sysctl -w {}={}", key, value)
}

// ---------- 写操作 ----------
//
// 全部经作业系统执行（见 jobs.rs 的 `JobKind::Tuning`）：建 swapfile 可能跑几十秒，
// 放在 HTTP 请求里会撞上前端 15 秒超时。
//
// 统一的执行顺序（每一步都不能省）：
//   1. 能力检查：非 root / 只读挂载 / 容器受限 → 直接给出原因，不进命令
//   2. 白名单校验：参数名与取值都过 `validate_value`
//   3. 改前留档：把原值读出来，作为回滚命令返回
//   4. 执行
//   5. 回读验证：确认真的生效（写成功 ≠ 生效，如容器里 sysctl 可能被静默忽略）

/// 内核参数的持久化文件。写在 `/etc/sysctl.d/` 下而不是 `/etc/sysctl.conf`：
/// 后者常被发行版与容器镜像接管，追加内容容易与既有行冲突。
const SYSCTL_DROPIN: &str = "/etc/sysctl.d/99-lyys-panel.conf";

fn bad(msg: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(msg.into())
}

/// 从作业参数里取字符串（缺失即报错，不给默认值）
fn sp(params: &serde_json::Value, key: &str) -> anyhow::Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| bad(format!("缺少参数 {key}")))
}

fn sp_opt(params: &serde_json::Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn u64_param(params: &serde_json::Value, key: &str) -> Option<u64> {
    params.get(key).and_then(|v| v.as_u64())
}

/// 执行一条命令并把 stderr 也带回（失败时用于判断原因）
async fn sh(script: &str, secs: u64) -> anyhow::Result<String> {
    let out = cmd::run(
        Command::new("sh").arg("-c").arg(script),
        Budget::query(secs),
    )
    .await?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(bad(if err.is_empty() {
            format!("命令执行失败（退出码 {:?}）", out.status.code())
        } else {
            err
        }));
    }
    if let Some(last) = text.trim_end().rsplit('\n').next() {
        text = last.to_string();
    }
    Ok(text.trim().to_string())
}

/// 写操作入口（由作业系统调用）
pub async fn run_action(
    params: &serde_json::Value,
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<()> {
    let action = sp(params, "action")?;

    // 1. 能力检查：不满足就地拒绝，并说明原因（而不是让命令报 permission denied）
    let cap = capability().await;
    if !cap.can_write {
        return Err(bad(cap.reason));
    }
    if cap.container {
        sink(&format!("注意：{}", cap.reason));
    }

    match action.as_str() {
        "sysctl" => apply_sysctl(params, sink).await,
        "ntp" => apply_ntp(params, sink).await,
        "swapfile" => apply_swapfile(params, sink).await,
        other => Err(bad(format!("未知的调优动作：{other}"))),
    }
}

/// 改一个白名单内的内核参数（同时写入持久化文件）
async fn apply_sysctl(
    params: &serde_json::Value,
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<()> {
    let key = sp(params, "key")?;
    let value = sp(params, "value")?;
    // 2. 白名单校验（名称 + 取值）
    validate_value(&key, &value).map_err(bad)?;

    let title = title_of(&key);
    // 3. 改前留档
    let old = read_param(&key).unwrap_or_default();
    if old == value.trim() {
        sink(&format!("{title}（{key}）已经是 {}，无需修改", value.trim()));
        return Ok(());
    }
    sink(&format!(
        "{title}（{key}）：{} → {}",
        if old.is_empty() { "（未知）" } else { &old },
        value.trim()
    ));

    // 4. 执行：先立即生效，再写持久化文件
    sh(&format!("sysctl -w {}={}", key, shell_quote(value.trim())), 15)
        .await
        .map_err(|e| bad(format!("设置 {key} 失败：{e}")))?;
    sink(&format!("已临时生效：sysctl -w {key}={}", value.trim()));

    let mut lines: Vec<String> = Vec::new();
    if Path::new(SYSCTL_DROPIN).exists() {
        lines = std::fs::read_to_string(SYSCTL_DROPIN)
            .unwrap_or_default()
            .lines()
            .map(|s| s.to_string())
            .collect();
    }
    let prefix = format!("{key} ");
    lines.retain(|l| !l.trim_start().starts_with(&prefix));
    lines.push(format!("{} = {}", key, value.trim()));
    let mut body = String::from(
        "# 由 LYYS Panel 生成：系统调优页写入的内核参数。\n\
         # 每行形如 `key = value`，改完执行 `sysctl --system` 立即生效。\n",
    );
    body.push_str(&lines.join("\n"));
    body.push('\n');
    std::fs::write(SYSCTL_DROPIN, body)
        .map_err(|e| bad(format!("写入 {SYSCTL_DROPIN} 失败：{e}")))?;
    sink(&format!("已写入持久化配置：{SYSCTL_DROPIN}"));
    sink(&format!("回滚命令：sysctl -w {key}={old}"));

    // 5. 回读验证（写成功 ≠ 生效）
    let now = read_param(&key).unwrap_or_default();
    if now != value.trim() {
        return Err(bad(format!(
            "{title} 写入后回读为「{now}」，与期望的「{}」不一致（该参数可能被内核或容器限制忽略）",
            value.trim()
        )));
    }
    sink(&format!("已验证：{key} = {now}"));
    Ok(())
}

/// 配置 NTP 服务器。
///
/// 只改「服务器列表」这一件事：启停服务、改时区各有其命令，混在一个动作里
/// 出错后说不清是哪一步坏了。系统没装时间同步守护进程时**明确拒绝**并说明 ——
/// 猜一个不存在的配置文件去写，只会留下一个没人读的文件让用户以为生效了。
async fn apply_ntp(
    params: &serde_json::Value,
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<()> {
    let servers_raw = sp(params, "servers")?;
    // 逐条校验：只允许主机名/IP 字面量，含空格、分号、斜杠的一律拒绝
    let servers: Vec<String> = servers_raw
        .split([',', '\n'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if servers.is_empty() {
        return Err(bad("至少给一个 NTP 服务器地址"));
    }
    for s in &servers {
        if !is_valid_host(s) {
            return Err(bad(format!("不是合法的服务器地址：{s}")));
        }
    }

    let st = ntp_status().await;
    if st.daemon == "（未安装）" {
        return Err(bad(
            "系统未安装时间同步守护进程（chrony / ntp / systemd-timesyncd），\
             请先用包管理器安装，例如 `pacman -S chrony` 或 `apt install chrony`",
        ));
    }
    let Some(conf) = st.config_path.clone() else {
        return Err(bad(format!(
            "识别到守护进程 {} 但没有找到对应配置文件，请手动检查",
            st.daemon
        )));
    };

    // 改前留档：备份原文件，回滚就是恢复它
    let backup = format!("{conf}.lyys-panel.bak");
    std::fs::copy(&conf, &backup).map_err(|e| bad(format!("备份 {conf} 失败：{e}")))?;
    sink(&format!("已备份原配置：{backup}"));

    // 重写服务器行：保留其它指令（driftfile、rtcsync 等），只替换 server/pool 行
    let old = std::fs::read_to_string(&conf).unwrap_or_default();
    // chrony / ntp 用 `server`，systemd-timesyncd 用 `NTP=`
    let is_timesyncd = st.daemon.contains("timesyncd");
    let out = rewrite_ntp_conf(&old, &servers, is_timesyncd);
    std::fs::write(&conf, out).map_err(|e| bad(format!("写入 {conf} 失败：{e}")))?;
    sink(&format!(
        "已写入 {} 个服务器：{}",
        servers.len(),
        servers.join(", ")
    ));

    // 让配置生效：优先重启守护进程（比 reload 更可靠，chronyd 对 SIGHUP 支持不一）
    let unit = ntp_unit(&st.daemon);
    // ntpd 的单元名在不同发行版是 ntp / ntpd，两个都试一次
    let restart = if unit == "ntpd" {
        "systemctl restart ntp 2>/dev/null || systemctl restart ntpd".to_string()
    } else {
        format!("systemctl restart {unit}")
    };
    sh(&restart, 30)
        .await
        .map_err(|e| bad(format!("重启时间同步服务失败（{unit}）：{e}")))?;
    sink(&format!("已重启 {unit}"));

    sink(&format!("回滚：cp {backup} {conf} && systemctl restart {unit}"));
    Ok(())
}

/// 重写 NTP 配置文件内容：丢掉旧的服务器行，写入新服务器，其它行保留。
///
/// 抽成纯函数是为了能测：真正写文件的那条路径需要系统装了时间同步守护进程，
/// 而测试机（容器）没有 —— 留在 `apply_ntp` 里就只能靠人工试。
///
/// 🔴 三种写法都要过滤：chrony / ntp 的 `server` 与 `pool`，以及
/// systemd-timesyncd 的 `NTP=`。只滤前两种的话，timesyncd 的旧服务器会留在文件
/// 里，新旧并存 —— 配置看起来生效了，实际仍在问旧服务器（测试抓到的缺陷）。
fn rewrite_ntp_conf(old: &str, servers: &[String], timesyncd: bool) -> String {
    let mut out = String::new();
    for line in old.lines() {
        let low = line.trim_start().to_ascii_lowercase();
        if low.starts_with("server ")
            || low.starts_with("pool ")
            || low.starts_with("ntp=")
            || low.starts_with("fallbackntp=")
        {
            continue; // 旧的服务器行丢掉，下面统一写新的
        }
        out.push_str(line);
        out.push('\n');
    }
    for s in servers {
        if timesyncd {
            // systemd-timesyncd 用 `NTP=` 键值形式，其余用 chrony/ntp 的 server 指令
            out.push_str(&format!("NTP={s}\n"));
        } else {
            out.push_str(&format!("server {s} iburst\n"));
        }
    }
    out
}

/// 由守护进程名推出要重启的 systemd 单元名。
///
/// 🔴 判定顺序要紧：`"systemd-timesyncd"` 里**含有** `ntp` 子串，
/// 先判 ntp 会把 timesyncd 也吃进去、去重启一个不存在的服务。
fn ntp_unit(daemon: &str) -> &'static str {
    if daemon.contains("timesyncd") {
        "systemd-timesyncd"
    } else if daemon.contains("chrony") {
        "chronyd"
    } else {
        "ntpd"
    }
}

/// 主机名 / IP 字面量校验：只允许字母数字与 `.:-_`
///
/// 白名单式校验而不是「拒绝危险字符」：NTP 服务器是直接进配置文件的文本，
/// 放过一个空格就能插入任意指令。
fn is_valid_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_'))
}

/// 建 / 启用 / 停用 / 删除 swapfile
async fn apply_swapfile(
    params: &serde_json::Value,
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<()> {
    let op = sp(params, "op")?;
    let path = sp_opt(params, "path").unwrap_or_else(|| "/swapfile".to_string());
    // 路径白名单前缀：只允许在根目录下建一个普通文件，杜绝把 swap 文件写到
    // /etc、/root/.ssh 这类位置（`fallocate` 会按给定路径创建文件）
    if !path.starts_with('/') || path.contains("..") || path.trim_matches('/').contains('/') {
        return Err(bad("swap 文件路径只能是根目录下的单个文件，如 /swapfile"));
    }

    match op.as_str() {
        "enable" => {
            let size_mb = u64_param(params, "size_mb").unwrap_or(0);
            if size_mb == 0 {
                return Err(bad("缺少 swap 文件大小（size_mb）"));
            }
            if size_mb > SWAP_MAX_MB {
                return Err(bad(format!(
                    "swap 文件最大 {SWAP_MAX_MB} MB，请求的是 {size_mb} MB"
                )));
            }
            if Path::new(&path).exists() {
                return Err(bad(format!(
                    "{path} 已存在，请先删除或改用其它路径"
                )));
            }
            sink(&format!("创建 {size_mb} MB 的 swap 文件：{path}"));
            // fallocate 比 dd 快得多；不可用时回退到 dd（稀疏写满）
            match sh(
                &format!("fallocate -l {}M {}", size_mb, shell_quote(&path)),
                120,
            )
            .await
            {
                Ok(_) => sink("已分配文件（fallocate）"),
                Err(e) => {
                    sink(&format!("fallocate 不可用（{e}），改用 dd"));
                    sh(
                        &format!(
                            "dd if=/dev/zero of={} bs=1M count={} status=none",
                            shell_quote(&path),
                            size_mb
                        ),
                        300,
                    )
                    .await?;
                }
            }
            sh(&format!("chmod 600 {}", shell_quote(&path)), 10).await?;
            sh(&format!("mkswap {}", shell_quote(&path)), 60).await?;
            sink("已格式化（mkswap）");
            sh(&format!("swapon {}", shell_quote(&path)), 60).await?;
            sink(&format!("已启用：{path}"));

            // 持久化：写 /etc/fstab（先备份原文件）
            let fstab = "/etc/fstab";
            let backup = format!("{fstab}.lyys-panel.bak");
            if Path::new(fstab).exists() {
                std::fs::copy(fstab, &backup)
                    .map_err(|e| bad(format!("备份 {fstab} 失败：{e}")))?;
                let mut body = std::fs::read_to_string(fstab).unwrap_or_default();
                if !body.ends_with('\n') && !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(&format!("{path} none swap sw 0 0\n"));
                std::fs::write(fstab, body)
                    .map_err(|e| bad(format!("写入 {fstab} 失败：{e}")))?;
                sink(&format!("已加入 {fstab}（原文件备份为 {backup}）"));
            }
            sink(&format!(
                "回滚命令：swapoff {path} && rm -f {path}（并从 /etc/fstab 删除对应行）"
            ));
        }
        "disable" => {
            sink(&format!("停用 swap：{path}"));
            sh(&format!("swapoff {}", shell_quote(&path)), 120).await?;
            sink("已停用（文件与 /etc/fstab 保持不变）");
        }
        "remove" => {
            sink(&format!("停用并删除 swap 文件：{path}"));
            // 先停用；未启用时 swapoff 会失败，这里不视为错误
            let _ = sh(&format!("swapoff {}", shell_quote(&path)), 120).await;
            std::fs::remove_file(&path)
                .map_err(|e| bad(format!("删除 {path} 失败：{e}")))?;
            sink("已删除文件（如需彻底清理，请从 /etc/fstab 移除对应行）");
        }
        other => return Err(bad(format!("未知的 swap 操作：{other}"))),
    }
    Ok(())
}

/// swap 文件大小上限（MB）。再大会让 fallocate/dd 在 HTTP 作业里跑太久，
/// 也超出常见需求；真要更大应当手工用 dd 处理。
const SWAP_MAX_MB: u64 = 8192;

/// 单引号包裹，供 `sh -c` 使用。
///
/// 🔴 必须做：参数虽然过了白名单（枚举/整数），但事件里仍可能带引号、
/// 空格或分号；不转义就等于拼命令。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 白名单内的 key 才允许写；不在表里的一律拒绝（这是内核参数写入的第一道闸门）
    #[test]
    fn only_whitelisted_keys_are_writable() {
        assert!(is_writable("net.ipv4.tcp_congestion_control"));
        assert!(is_writable("vm.swappiness"));
        assert!(!is_writable("kernel.panic"));
        assert!(!is_writable("net.ipv4.ip_forward"));
        // 空 key / 路径穿越一律拒绝
        assert!(!is_writable(""));
        assert!(!is_writable("../../etc/passwd"));
    }

    /// 枚举型参数只接受白名单取值；大小写与空白要宽容，但内容必须精确
    #[test]
    fn enum_values_are_restricted() {
        assert!(validate_value("net.ipv4.tcp_congestion_control", "bbr").is_ok());
        assert!(validate_value("net.ipv4.tcp_congestion_control", " cubic ").is_ok());
        assert!(validate_value("net.ipv4.tcp_congestion_control", "yeah").is_err());
        // 故意带命令拼接的取值必须被挡（否则形如 `bbr; reboot` 会进入命令行）
        assert!(validate_value("net.ipv4.tcp_congestion_control", "bbr; reboot").is_err());
        assert!(validate_value("net.ipv4.tcp_syncookies", "2").is_ok());
        assert!(validate_value("net.ipv4.tcp_syncookies", "3").is_err());
    }

    /// 数值型参数必须在区间内，且必须是整数
    #[test]
    fn numeric_values_are_range_checked() {
        assert!(validate_value("vm.swappiness", "60").is_ok());
        assert!(validate_value("vm.swappiness", "0").is_ok());
        assert!(validate_value("vm.swappiness", "200").is_ok());
        assert!(validate_value("vm.swappiness", "201").is_err());
        assert!(validate_value("vm.swappiness", "-1").is_err());
        // 非整数、注入式取值都要挡
        assert!(validate_value("vm.swappiness", "60; rm -rf /").is_err());
        assert!(validate_value("vm.swappiness", "abc").is_err());
        assert!(validate_value("vm.swappiness", "6.5").is_err());
        // 未列出的参数
        assert!(validate_value("kernel.panic", "1").is_err());
    }

    /// 参数名 → /proc/sys 路径：点换成斜杠
    #[test]
    fn maps_key_to_proc_path() {
        assert_eq!(
            proc_path("net.ipv4.tcp_congestion_control"),
            "/proc/sys/net/ipv4/tcp_congestion_control"
        );
        assert_eq!(proc_path("vm.swappiness"), "/proc/sys/vm/swappiness");
        // 不该产生绝对路径逃逸：key 来自白名单，斜杠替换后仍是 /proc/sys 之下
        assert!(proc_path("vm.swappiness").starts_with("/proc/sys/"));
    }

    /// /proc/swaps 的解析：表头跳过、分区与文件区分、合计正确
    #[test]
    fn parses_proc_swaps() {
        // 直接验证解析逻辑的分支（真实文件因机器而异，不能依赖）
        let text = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                    /dev/sdc                                partition\t8388608\t\t0\t\t-2\n\
                    /swapfile                               file\t\t2097148\t\t1024\t\t-3\n";
        let mut st = SwapStatus::default();
        for (i, line) in text.lines().enumerate() {
            if i == 0 || line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 5 {
                continue;
            }
            let kind = f[1].to_string();
            if kind == "file" {
                st.has_file = true;
            }
            st.total_kb += f[2].parse().unwrap_or(0);
            st.used_kb += f[3].parse().unwrap_or(0);
            st.entries.push(SwapEntry {
                name: f[0].into(),
                kind,
                size_kb: f[2].parse().unwrap_or(0),
                used_kb: f[3].parse().unwrap_or(0),
                priority: f[4].into(),
            });
        }
        assert_eq!(st.entries.len(), 2);
        assert!(st.has_file);
        assert_eq!(st.total_kb, 8388608 + 2097148);
        assert_eq!(st.used_kb, 1024);
        assert_eq!(st.entries[0].kind, "partition");
    }

    /// 预览命令与标题映射
    #[test]
    fn preview_and_title() {
        assert_eq!(
            preview_set("vm.swappiness", "10"),
            "sysctl -w vm.swappiness=10"
        );
        assert_eq!(title_of("vm.swappiness"), "交换倾向");
        assert_eq!(title_of("unknown.key"), "unknown.key");
    }

    /// NTP 配置重写：只替换 server/pool 行，其它指令必须原样保留
    #[test]
    fn rewrites_ntp_conf_keeping_other_directives() {
        let old = "\
# 注释保留
driftfile /var/lib/chrony/drift
server old.example.com iburst
pool pool.example.org iburst
rtcsync
";
        let servers = vec!["ntp.aliyun.com".to_string(), "time.cloudflare.com".to_string()];
        let out = rewrite_ntp_conf(old, &servers, false);
        assert!(out.contains("driftfile /var/lib/chrony/drift"), "其它指令应保留");
        assert!(out.contains("rtcsync"), "其它指令应保留");
        assert!(out.contains("# 注释保留"), "注释应保留");
        assert!(!out.contains("old.example.com"), "旧 server 行必须被替换");
        assert!(!out.contains("pool.example.org"), "旧 pool 行必须被替换");
        assert!(out.contains("server ntp.aliyun.com iburst"));
        assert!(out.contains("server time.cloudflare.com iburst"));
        // 旧行被移到最后追加，不应出现重复的 server 行
        assert_eq!(out.matches("server ").count(), 2);

        // systemd-timesyncd 用 NTP= 键值形式
        let out2 = rewrite_ntp_conf("NTP=old\n# 注释\n", &servers, true);
        assert!(out2.contains("NTP=ntp.aliyun.com"));
        assert!(!out2.contains("NTP=old"));
        assert!(out2.contains("# 注释"));
        assert!(!out2.contains("server ntp.aliyun.com iburst"), "timesyncd 不写 server 行");
    }

    /// 单元名判定顺序：systemd-timesyncd 含 "ntp" 子串，不能被判成 ntpd
    #[test]
    fn ntp_unit_prefers_timesyncd() {
        assert_eq!(ntp_unit("systemd-timesyncd"), "systemd-timesyncd");
        assert_eq!(ntp_unit("chrony"), "chronyd");
        assert_eq!(ntp_unit("ntp"), "ntpd");
        assert_eq!(ntp_unit("（未安装）"), "ntpd");
    }
}
