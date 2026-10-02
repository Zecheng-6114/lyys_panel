//! 统一命令执行器：全后端唯一的外部进程出口。
//!
//! 所有对外部命令的一次性调用（捕获输出、等待退出）都必须经由本模块，统一获得：
//! - 超时预算：超时即杀进程并回收，绝不无限挂起（dpkg 锁被占、磁盘 IO 阻塞不再拖死请求）；
//! - 分组互斥：包管理器 / Docker / systemctl 写操作各自全局串行，消除 dpkg 锁并发抢占；
//! - 输出上限：单流超过上限即截断并终止，防 journalctl 之类无界输出撑爆内存；
//! - 进程回收：kill 之后必 wait，不留僵尸子进程。
//!
//! 调用方负责参数、env、工作目录的构造（沿用原 Command 构造代码），
//! 本模块只接管「怎么执行」：锁、超时、截断、回收。
//!
//! 两类场景不走本模块，保持原实现：
//! - 流式长连接（容器日志 WebSocket 泵，见 ops::container_log_stream）；
//! - best-effort 探测中需要「失败即 false」语义的（走 Budget::query 后同样兼容）。
//!
//! 超时预算参考（秒）：
//! | 命令类别                      | 预算  | 互斥组   |
//! |-------------------------------|-------|----------|
//! | dpkg-query / apt-cache / pacman 查询 | 15 | None |
//! | apt-get -s（模拟升级）        | 60    | None     |
//! | journalctl / tail             | 15    | None     |
//! | ip / ss                       | 10    | None     |
//! | crontab 读 / 写               | 10    | None     |
//! | systemctl 读操作              | 20    | None     |
//! | systemctl 写操作              | 20    | Systemd  |
//! | apt-get / pacman 安装卸载升级 | 1800  | Package  |
//! | docker 列表                   | 20    | Docker   |
//! | docker 拉取 / compose 长操作  | 900   | Docker   |

use anyhow::{Context, Result};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, MutexGuard};

/// 并发互斥分组。同组命令全局串行，跨组互不影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockGroup {
    /// 无互斥（只读查询、探测）
    None,
    /// 包管理器全家（apt-get / pacman / dpkg / apt-cache），消除 dpkg 锁抢占
    Package,
    /// Docker 业务命令（容器 / 镜像 / compose）
    Docker,
    /// systemctl 写操作（start / stop / restart / enable 等）
    Systemd,
}

/// 单条命令的执行预算
#[derive(Debug, Clone)]
pub struct Budget {
    /// 超时预算：超时即杀进程并返回错误
    pub timeout: Duration,
    /// 单流（stdout 或 stderr）最大捕获字节数，超过即截断并终止命令
    pub max_output: usize,
    /// 并发互斥分组
    pub group: LockGroup,
}

/// 查询类输出上限：2 MiB
const QUERY_CAP: usize = 2 * 1024 * 1024;
/// 包管理器输出上限：64 MiB（安装日志量大，保留完整输出）
const PKG_CAP: usize = 64 * 1024 * 1024;

impl Budget {
    /// 只读查询 / 探测：无互斥，2 MiB 上限
    pub fn query(secs: u64) -> Self {
        Budget {
            timeout: Duration::from_secs(secs),
            max_output: QUERY_CAP,
            group: LockGroup::None,
        }
    }

    /// 包管理器操作：Package 组串行，64 MiB 上限
    pub fn package(secs: u64) -> Self {
        Budget {
            timeout: Duration::from_secs(secs),
            max_output: PKG_CAP,
            group: LockGroup::Package,
        }
    }

    /// Docker 业务命令：Docker 组串行
    pub fn docker(secs: u64) -> Self {
        Budget {
            timeout: Duration::from_secs(secs),
            max_output: QUERY_CAP,
            group: LockGroup::Docker,
        }
    }

    /// systemctl 写操作：Systemd 组串行
    pub fn systemd(secs: u64) -> Self {
        Budget {
            timeout: Duration::from_secs(secs),
            max_output: QUERY_CAP,
            group: LockGroup::Systemd,
        }
    }
}

/// 命令结果。字段口径与 std::process::Output 对齐，调用方可平滑迁移。
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// 任一流被截断时为 true（命令已被终止，stdout / stderr 为截断前内容）。
    /// 截断已在本模块内 warn 落日志；该标记供单测与后续调用方观察截断行为，
    /// 生产路径暂不读取，故显式豁免 dead_code（CI 以 -D warnings 编译）。
    #[allow(dead_code)]
    pub truncated: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status.success()
    }
}

/// 命令以非零退出码结束。
///
/// 单独成型而不是只 `bail!` 一段文案，是为了让调用方拿得到**结构化**的退出码：
/// 作业队列要把它落进 `jobs.exit_code`，而错误文案里的数字是给人看的，
/// 让程序去正则里捞退出码是错的。`anyhow` 的 `downcast_ref` 能穿透 `.context()`
/// 包装取回本类型，故各调用方无需改签名。
#[derive(Debug)]
pub struct CommandFailed {
    /// 进程退出码；被信号杀死时为 None
    pub code: Option<i32>,
    /// 面向用户的原因（不含命令原始输出，P1-3）
    pub message: String,
}

impl std::fmt::Display for CommandFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandFailed {}

static PKG_LOCK: Mutex<()> = Mutex::const_new(());
static DOCKER_LOCK: Mutex<()> = Mutex::const_new(());
static SYSTEMD_LOCK: Mutex<()> = Mutex::const_new(());

/// 获取分组互斥锁。None 组不加锁（返回 None）。
/// 锁在 run 返回前一直持有，粒度恰好覆盖命令执行期。
async fn acquire(group: LockGroup) -> Option<MutexGuard<'static, ()>> {
    match group {
        LockGroup::None => None,
        LockGroup::Package => Some(PKG_LOCK.lock().await),
        LockGroup::Docker => Some(DOCKER_LOCK.lock().await),
        LockGroup::Systemd => Some(SYSTEMD_LOCK.lock().await),
    }
}

/// 读取上限内的输出。返回 (内容, 是否截断)。
async fn read_capped<R: AsyncRead + Unpin>(reader: &mut R, max: usize) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => return (buf, false),
            Ok(n) => {
                if buf.len() + n > max {
                    let keep = max - buf.len();
                    buf.extend_from_slice(&chunk[..keep]);
                    return (buf, true);
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(_) => return (buf, false),
        }
    }
}

/// 超时兜底：杀进程并等待回收，不留僵尸。返回统一格式的超时错误。
async fn kill_and_bail(mut child: tokio::process::Child, secs: u64) -> anyhow::Error {
    let _ = child.start_kill();
    let _ = child.wait().await;
    anyhow::anyhow!("命令超时（{secs} 秒），已强制终止")
}

/// 执行命令并捕获输出（stdin 关闭）。
/// stdout / stderr 分流捕获，超时或超限即终止命令。
pub async fn run(cmd: &mut Command, budget: Budget) -> Result<Output> {
    let _guard = acquire(budget.group).await;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().context("启动外部命令失败")?;
    collect_and_wait(child, budget).await
}

/// 执行命令，向 stdin 写入数据后关闭（crontab - 之类「管道喂内容」的场景）。
pub async fn run_with_stdin(cmd: &mut Command, input: &[u8], budget: Budget) -> Result<Output> {
    let _guard = acquire(budget.group).await;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("启动外部命令失败")?;
    // 先写 stdin 再关管道；写失败（对端提前退出）不致命，继续等命令结果
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(input).await {
            tracing::warn!("写入命令 stdin 失败（对端可能已退出）：{e}");
        }
    }
    collect_and_wait(child, budget).await
}

/// 流式执行命令：stdout 与 stderr 各走一条管道，并发逐行读出并回调 `on_line`，
/// 供作业写 `stdout_tail` 与 SSE 推送。
///
/// 与 [`run`] 的差别只在"边跑边给"：超时 kill + wait、输出上限、进程回收的口径
/// 完全一致。同步调用方想复用它，把 `on_line` 传空闭包即可。
///
/// **刻意不用「重定向到同一文件再尾随」**：文件没有背压，写入方可以全速把磁盘
/// 写满，而读取方每个轮询周期才来看一眼 —— `yes` 这类无限输出的命令能在一个
/// 周期内写下 GB 级数据（实测写爆 /tmp 这个 16G 的 tmpfs，连带拖垮 WSL 服务）。
/// 管道有内核缓冲，读端不消费时子进程会被挡住，这是唯一安全的流式形态。
///
/// 代价是两条流的行序不再严格按时间交错。实际影响很小：apt/pacman 在非 TTY 下
/// 主要输出走 stdout，stderr 只有零星警告。
pub async fn run_streaming(
    cmd: &mut Command,
    budget: Budget,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> Result<Output> {
    let _guard = acquire(budget.group).await;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("启动外部命令失败")?;

    let stdout = child.stdout.take().context("取得命令 stdout 失败")?;
    let stderr = child.stderr.take().context("取得命令 stderr 失败")?;
    let mut out_lines = BufReader::new(stdout).lines();
    let mut err_lines = BufReader::new(stderr).lines();

    let cap = budget.max_output;
    let mut stdout_buf: Vec<u8> = Vec::new();
    let mut stderr_buf: Vec<u8> = Vec::new();
    let mut total: usize = 0;
    let mut truncated = false;
    let mut out_done = false;
    let mut err_done = false;

    // 两路并发读：任一路阻塞都不会让另一路积压
    let pump = async {
        while !(out_done && err_done) {
            tokio::select! {
                r = out_lines.next_line(), if !out_done => match r {
                    Ok(Some(line)) => {
                        total += line.len() + 1;
                        if stdout_buf.len() + line.len() < cap {
                            stdout_buf.extend_from_slice(line.as_bytes());
                            stdout_buf.push(b'\n');
                        }
                        on_line(&line);
                    }
                    _ => out_done = true,
                },
                r = err_lines.next_line(), if !err_done => match r {
                    Ok(Some(line)) => {
                        total += line.len() + 1;
                        if stderr_buf.len() + line.len() < cap {
                            stderr_buf.extend_from_slice(line.as_bytes());
                            stderr_buf.push(b'\n');
                        }
                        on_line(&line);
                    }
                    _ => err_done = true,
                },
            }
            if total > cap {
                truncated = true;
                break;
            }
        }
    };

    let secs = budget.timeout.as_secs();
    let timed_out = tokio::time::timeout(budget.timeout, pump).await.is_err();
    if timed_out || truncated {
        let _ = child.start_kill();
    }
    let status = child.wait().await.context("等待命令退出失败")?;
    if timed_out {
        return Err(anyhow::anyhow!("命令超时（{secs} 秒），已强制终止"));
    }
    if truncated {
        tracing::warn!("命令输出超过 {cap} 字节上限，已截断并终止");
    }
    Ok(Output {
        status,
        stdout: stdout_buf,
        stderr: stderr_buf,
        truncated,
    })
}

/// 共用收尾：并发抽干两路管道（各按上限截断），超时或截断时终止子进程。
async fn collect_and_wait(mut child: tokio::process::Child, budget: Budget) -> Result<Output> {
    let cap = budget.max_output;
    let out_task = child
        .stdout
        .take()
        .map(|mut p| tokio::spawn(async move { read_capped(&mut p, cap).await }));
    let err_task = child
        .stderr
        .take()
        .map(|mut p| tokio::spawn(async move { read_capped(&mut p, cap).await }));

    let wait = async {
        // 两路管道已在 spawn 后立即并发抽干，这里顺序 await 句柄即可，
        // 不改变并发语义（两个 task 早已在后台运行）。
        let o = match out_task {
            Some(h) => h.await.unwrap_or_default(),
            None => (Vec::new(), false),
        };
        let e = match err_task {
            Some(h) => h.await.unwrap_or_default(),
            None => (Vec::new(), false),
        };
        let stdout = o.0;
        let stderr = e.0;
        let truncated = o.1 || e.1;
        // 截断意味着输出已超限：终止命令，避免它继续无界产出
        if truncated {
            let _ = child.start_kill();
        }
        let status = child.wait().await.context("等待命令退出失败")?;
        if truncated {
            tracing::warn!("命令输出超过 {} 字节上限，已截断并终止", budget.max_output);
        }
        Ok(Output {
            status,
            stdout,
            stderr,
            truncated,
        })
    };

    let secs = budget.timeout.as_secs();
    match tokio::time::timeout(budget.timeout, wait).await {
        Ok(res) => res,
        Err(_) => Err(kill_and_bail(child, secs).await),
    }
}

impl Budget {
    /// 测试辅助：调整输出上限（链式）
    #[cfg(test)]
    fn max_output_limited(mut self, cap: usize) -> Self {
        self.max_output = cap;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 超时必须杀进程并返回错误，而不是无限挂起。
    /// 各平台用各自的「睡 30 秒」命令，预算只给 1 秒。
    #[tokio::test]
    async fn timeout_kills_process() {
        let mut cmd = sleep_cmd();
        let started = std::time::Instant::now();
        let r = run(&mut cmd, Budget::query(1)).await;
        assert!(r.is_err(), "超时必须返回错误");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "超时后应尽快返回，实际耗时 {:?}",
            started.elapsed()
        );
    }

    /// 正常命令：成功退出、stdout 捕获、truncated=false。
    #[tokio::test]
    async fn captures_stdout() {
        let (prog, args): (&str, [&str; 2]) = if cfg!(windows) {
            ("cmd", ["/c", "echo lyys-ok"])
        } else {
            ("echo", ["lyys-ok", ""])
        };
        let mut cmd = Command::new(prog);
        cmd.args(args.iter().filter(|a| !a.is_empty()));
        let out = run(&mut cmd, Budget::query(5))
            .await
            .expect("echo 必须成功");
        assert!(out.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("lyys-ok"));
        assert!(!out.truncated);
    }

    /// 输出超限：截断标记为 true 且内容不超过上限。
    #[cfg(unix)]
    #[tokio::test]
    async fn truncates_oversized_output() {
        // yes 无限输出，上限 1 KiB
        let mut cmd = Command::new("yes");
        cmd.arg("x");
        let out = run(&mut cmd, Budget::query(10).max_output_limited(1024))
            .await
            .expect("yes 应被截断后成功返回或至少不报错");
        assert!(out.truncated, "无限输出必须触发截断");
        assert!(out.stdout.len() <= 1024, "截断后不应超过上限");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn truncates_oversized_output() {
        // Windows 侧等价物：ping 输出有限，仅验证小上限下截断路径可用
        let mut cmd = Command::new("ping");
        cmd.args(["-n", "2", "127.0.0.1"]);
        let out = run(&mut cmd, Budget::query(10).max_output_limited(16))
            .await
            .expect("小上限应触发截断而非错误");
        assert!(out.truncated);
        assert!(out.stdout.len() <= 16);
    }

    /// stdin 喂入：echo 管道回显（跨平台用 cat / findstr）。
    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_piped() {
        let mut cmd = Command::new("cat");
        let out = run_with_stdin(&mut cmd, b"lyys-stdin", Budget::query(5))
            .await
            .expect("cat 必须成功");
        assert_eq!(out.stdout, b"lyys-stdin");
    }

    /// 流式：行按顺序回调，完整输出仍可从 Output 取回
    #[tokio::test]
    async fn streaming_reports_lines_in_order() {
        let (prog, args): (&str, Vec<&str>) = if cfg!(windows) {
            ("cmd", vec!["/c", "echo lyys-a&echo lyys-b"])
        } else {
            ("sh", vec!["-c", "echo lyys-a; echo lyys-b"])
        };
        let mut cmd = Command::new(prog);
        cmd.args(args);
        let mut lines: Vec<String> = Vec::new();
        let out = {
            let mut sink = |l: &str| lines.push(l.to_string());
            run_streaming(&mut cmd, Budget::query(10), &mut sink)
                .await
                .expect("命令必须成功")
        };
        assert!(out.success());
        assert_eq!(lines, vec!["lyys-a".to_string(), "lyys-b".to_string()]);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("lyys-a") && text.contains("lyys-b"),
            "合并输出应可从 Output 取回：{text:?}"
        );
    }

    /// 流式：超时与同步路径同口径 —— 杀进程并返回错误，不无限挂起
    #[tokio::test]
    async fn streaming_timeout_kills_process() {
        let mut cmd = sleep_cmd();
        let started = std::time::Instant::now();
        let r = run_streaming(&mut cmd, Budget::query(1), &mut |_: &str| {}).await;
        assert!(r.is_err(), "超时必须返回错误");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(6),
            "超时后应尽快返回，实际耗时 {:?}",
            started.elapsed()
        );
    }

    /// 流式：输出超限时截断，且截断前已有行被回调（不是攒到最后才给）
    #[cfg(unix)]
    #[tokio::test]
    async fn streaming_truncates_oversized_output() {
        let mut cmd = Command::new("yes");
        cmd.arg("x");
        let mut lines = 0usize;
        let mut sink = |_: &str| lines += 1;
        let out = run_streaming(
            &mut cmd,
            Budget::query(10).max_output_limited(4096),
            &mut sink,
        )
        .await
        .expect("yes 应被截断后成功返回");
        assert!(out.truncated, "无限输出必须触发截断");
        assert!(lines > 0, "截断前应有行被回调");
    }

    fn sleep_cmd() -> Command {
        if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        }
    }
}
