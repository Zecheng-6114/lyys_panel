//! Docker 控制：容器、镜像、Compose 项目。
//!
//! 设计要点：
//! - 所有输出解析都走 `docker ... --format` 的固定模板，不解析人类可读的表格，
//!   避免列宽随内容变化导致错位；
//! - 面板以 root 运行，直接访问 /var/run/docker.sock，不需要把用户加进 docker 组；
//! - 每个外部命令都带超时，避免 daemon 卡住时请求永远挂着。

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

/// Docker 环境状态。前端据此决定显示控制面板还是安装入口。
#[derive(Serialize)]
pub struct DockerStatus {
    /// docker 命令行是否存在
    pub installed: bool,
    /// daemon 是否可访问
    pub running: bool,
    /// 守护进程单元是否在运行（用于识别「装了 daemon 但缺 CLI」这种半装状态）
    pub daemon: bool,
    /// `docker --version` 的原始输出
    pub version: String,
    /// compose 可用形式：v2（docker compose 子命令）/ v1（独立 docker-compose）/ 空
    pub compose: String,
    /// 不可用时的原因（installed 与 running 都为 true 时为空）
    pub error: String,
}

/// 容器信息
#[derive(Serialize)]
pub struct ContainerInfo {
    /// 短 ID
    pub id: String,
    pub name: String,
    pub image: String,
    /// running / exited / paused ...
    pub state: String,
    /// 人类可读状态，如 Up 3 hours
    pub status: String,
    /// 端口映射，如 0.0.0.0:8080->80/tcp
    pub ports: String,
    /// 运行时长，如 Up 3 hours 中的相对描述
    pub running_for: String,
    /// CPU 占用百分比（仅运行中容器有值）
    pub cpu: String,
    /// 内存用量（仅运行中容器有值）
    pub mem: String,
    /// 所属 compose 项目（无则空）
    pub project: String,
}

/// 镜像信息
#[derive(Serialize)]
pub struct ImageInfo {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size: String,
    /// 创建于多久之前
    pub created: String,
}

/// Compose 项目
#[derive(Serialize)]
pub struct ComposeProject {
    pub name: String,
    /// 状态描述
    pub status: String,
    /// compose 配置文件路径
    pub config_files: String,
    /// 工作目录（compose v1 回退路径下由容器 label 推导）
    pub working_dir: String,
    /// 容器数量
    pub containers: usize,
}

/// 容器 / compose 操作
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Start,
    Stop,
    Restart,
    Remove,
    Up,
    Down,
}

/// 列表类命令的超时（秒）
const LIST_TIMEOUT: u64 = 20;
/// 可能长时间运行的操作（安装、拉取镜像、compose up）的超时（秒）
const LONG_TIMEOUT: u64 = 900;

/// 执行命令并返回 stdout。失败时 stderr 只写入日志，不进错误消息（P1-3）。
async fn run(program: &str, args: &[&str], secs: u64, what: &str) -> Result<String> {
    let fut = Command::new(program).args(args).output();
    let res = timeout(Duration::from_secs(secs), fut)
        .await
        .map_err(|_| anyhow::anyhow!("{what}超时（{} 秒）", secs))?;
    let out = res.context(format!("调用 {program} 失败，请确认已安装并启动 Docker"))?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显（防内部细节泄露）
        tracing::warn!("{what}失败，{program} stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
        anyhow::bail!("{what}失败，详见服务端日志");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 执行 docker 命令
async fn docker(args: &[&str], secs: u64) -> Result<String> {
    run("docker", args, secs, "docker 命令执行").await
}

/// systemd 单元是否处于 active 状态
async fn unit_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", unit])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 探测 Docker 环境。任何一步失败都不算错误 —— 状态本身就是给前端看的。
pub async fn status() -> Result<DockerStatus> {
    let daemon = unit_active("docker").await;

    // 1) docker 命令在不在
    let version = match Command::new("docker").arg("--version").output().await {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            // Debian 把 docker-cli 列为 docker.io 的「推荐」而非「依赖」，
            // 用 --no-install-recommends 安装会只装到守护进程。这种情况要和
            // 「完全没装」区分开，否则用户看着一个正在运行的 daemon 却被告知
            // 「未检测到 Docker」，再点一次安装也看不懂发生了什么。
            let error = if daemon {
                "检测到 Docker 守护进程正在运行，但缺少 docker 命令行工具（docker-cli）"
            } else {
                "未检测到 Docker"
            };
            return Ok(DockerStatus {
                installed: false,
                running: false,
                daemon,
                version: String::new(),
                compose: String::new(),
                error: error.to_string(),
            });
        }
    };

    // 2) daemon 能不能连上。docker info 需要真正与 daemon 通信，
    //    比 ps 更早失败，是判断「装了但没启动」的可靠依据。
    let info = timeout(Duration::from_secs(LIST_TIMEOUT), Command::new("docker").args(["info", "--format", "{{.ServerVersion}}"]).output()).await;
    let running = matches!(&info, Ok(Ok(o)) if o.status.success());
    let error = if running {
        String::new()
    } else {
        "Docker 已安装但守护进程未运行，请启动 docker 服务".to_string()
    };

    // 3) compose 用哪种形式
    let compose = match compose_prefix().await {
        Ok(p) if p.first().map(|s| s.as_str()) == Some("docker") => "v2".to_string(),
        Ok(_) => "v1".to_string(),
        Err(_) => String::new(),
    };

    Ok(DockerStatus {
        installed: true,
        running,
        daemon,
        version,
        compose,
        error,
    })
}

/// compose 的调用前缀。
///
/// 优先 `docker compose`（CLI 插件）；Debian 13 源里没有 docker-compose-plugin，
/// 所以通常要走回退的独立 `docker-compose` 命令 —— 注意它在 Debian 里同样是
/// v2 内核（只是不以插件形式存在），`ls` 之类的子命令一样能用。
async fn compose_prefix() -> Result<Vec<String>> {
    if Command::new("docker")
        .args(["compose", "version", "--short"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(vec!["docker".to_string(), "compose".to_string()]);
    }
    if Command::new("docker-compose")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(vec!["docker-compose".to_string()]);
    }
    bail!("未检测到 docker compose（docker compose 与 docker-compose 都不可用）")
}

/// 容器 ID / 名称校验，防参数注入
fn check_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || id.starts_with('-')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        bail!("非法容器标识：{id}");
    }
    Ok(())
}

/// 镜像引用校验（允许 registry/name:tag 与 @digest）
fn check_image_ref(r: &str) -> Result<()> {
    if r.is_empty()
        || r.len() > 256
        || r.starts_with('-')
        || !r
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | ':' | '.' | '_' | '-' | '@'))
    {
        bail!("非法镜像名：{r}");
    }
    Ok(())
}

/// 从 `docker ps` 的 Labels 字段里取出 compose 项目名
fn project_from_labels(labels: &str) -> String {
    labels
        .split(',')
        .find_map(|kv| kv.trim().strip_prefix("com.docker.compose.project="))
        .unwrap_or("")
        .to_string()
}

/// 容器列表（含 CPU / 内存占用）
pub async fn containers() -> Result<Vec<ContainerInfo>> {
    let text = docker(
        &[
            "ps",
            "-a",
            "--format",
            "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.State}}\t{{.Status}}\t{{.Ports}}\t{{.RunningFor}}\t{{.Labels}}",
        ],
        LIST_TIMEOUT,
    )
    .await?;

    // docker stats 只统计运行中的容器，先取一份按 ID 索引，缺失即填空
    let mut stats: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();
    if let Ok(s) = docker(
        &["stats", "--no-stream", "--format", "{{.ID}}\t{{.CPUPerc}}\t{{.MemUsage}}"],
        LIST_TIMEOUT,
    )
    .await
    {
        for line in s.lines() {
            let mut it = line.split('\t');
            if let (Some(id), Some(cpu), Some(mem)) = (it.next(), it.next(), it.next()) {
                stats.insert(id.to_string(), (cpu.to_string(), mem.to_string()));
            }
        }
    }

    let mut list = Vec::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(id), Some(name), Some(image), Some(state), Some(status), Some(ports), Some(running_for)) = (
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
        ) else {
            continue;
        };
        let labels = it.next().unwrap_or("");
        let (cpu, mem) = stats.remove(id).unwrap_or_default();
        list.push(ContainerInfo {
            id: id.to_string(),
            name: name.to_string(),
            image: image.to_string(),
            state: state.to_string(),
            status: status.to_string(),
            ports: ports.to_string(),
            running_for: running_for.to_string(),
            cpu,
            mem,
            project: project_from_labels(labels),
        });
    }
    Ok(list)
}

/// 对容器执行 start / stop / restart / remove
pub async fn container_action(id: &str, act: Action) -> Result<String> {
    check_id(id)?;
    let args: Vec<&str> = match act {
        Action::Start => vec!["start", id],
        Action::Stop => vec!["stop", id],
        Action::Restart => vec!["restart", id],
        // 运行中的容器必须先 -f 才能删除，否则 docker 会拒绝
        Action::Remove => vec!["rm", "-f", id],
        _ => bail!("容器不支持该操作"),
    };
    docker(&args, LIST_TIMEOUT).await
}

/// 容器日志（合并 stdout 与 stderr）
pub async fn logs(id: &str, tail: usize) -> Result<String> {
    check_id(id)?;
    let tail = tail.clamp(1, 5000).to_string();
    docker(&["logs", "--tail", &tail, id], LIST_TIMEOUT).await
}

/// 镜像列表
pub async fn images() -> Result<Vec<ImageInfo>> {
    let text = docker(
        &["images", "--format", "{{.ID}}\t{{.Repository}}\t{{.Tag}}\t{{.Size}}\t{{.CreatedSince}}"],
        LIST_TIMEOUT,
    )
    .await?;
    let mut list = Vec::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(id), Some(repo), Some(tag), Some(size), Some(created)) =
            (it.next(), it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        list.push(ImageInfo {
            id: id.to_string(),
            repository: repo.to_string(),
            tag: tag.to_string(),
            size: size.to_string(),
            created: created.to_string(),
        });
    }
    Ok(list)
}

/// 拉取镜像
pub async fn pull(reference: &str) -> Result<String> {
    check_image_ref(reference)?;
    docker(&["pull", reference], LONG_TIMEOUT).await
}

/// 删除镜像（强制，允许被容器占用时一并移除）
pub async fn remove_image(id: &str) -> Result<String> {
    if id.is_empty() || id.len() > 256 || id.starts_with('-') {
        bail!("非法镜像标识：{id}");
    }
    docker(&["rmi", "-f", id], LIST_TIMEOUT).await
}

/// Compose 项目列表。
///
/// 优先用 v2 的 `docker compose ls`（能列出已停止的项目）；v1 没有 ls，
/// 只能从容器的 compose label 反推 —— 代价是「容器全删掉的项目」不可见。
pub async fn compose_projects() -> Result<Vec<ComposeProject>> {
    if let Ok(prefix) = compose_prefix().await {
        let prog = prefix[0].clone();
        let mut args: Vec<String> = prefix[1..].to_vec();
        args.extend(["ls", "--all", "--format", "json"].map(String::from));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if let Ok(text) = run(&prog, &refs, LIST_TIMEOUT, "列 compose 项目").await {
            if let Ok(projects) = parse_compose_json(&text) {
                return Ok(projects);
            }
        }
    }
    compose_projects_from_labels().await
}

fn parse_compose_json(text: &str) -> Result<Vec<ComposeProject>> {
    let mut list = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).context("解析 compose ls 输出失败")?;
        list.push(ComposeProject {
            name: v.get("Name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status: v.get("Status").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            config_files: v
                .get("ConfigFiles")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            working_dir: String::new(),
            containers: 0,
        });
    }
    Ok(list)
}

/// 回退方案：从容器 label 聚合出项目
async fn compose_projects_from_labels() -> Result<Vec<ComposeProject>> {
    let text = docker(
        &[
            "ps",
            "-a",
            "--filter",
            "label=com.docker.compose.project",
            "--format",
            "{{.State}}\t{{.Labels}}",
        ],
        LIST_TIMEOUT,
    )
    .await?;

    let mut map: std::collections::BTreeMap<String, (usize, usize, String, String)> =
        std::collections::BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let (Some(state), Some(labels)) = (it.next(), it.next()) else {
            continue;
        };
        let project = project_from_labels(labels);
        if project.is_empty() {
            continue;
        }
        let dir = labels
            .split(',')
            .find_map(|kv| kv.trim().strip_prefix("com.docker.compose.project.working_dir="))
            .unwrap_or("")
            .to_string();
        let cfg = labels
            .split(',')
            .find_map(|kv| kv.trim().strip_prefix("com.docker.compose.project.config_files="))
            .unwrap_or("")
            .to_string();
        let entry = map.entry(project).or_insert((0, 0, dir, cfg));
        entry.0 += 1;
        if state == "running" {
            entry.1 += 1;
        }
    }

    Ok(map
        .into_iter()
        .map(|(name, (total, running, dir, cfg))| ComposeProject {
            name,
            status: format!("{running}/{total} 运行中"),
            config_files: cfg,
            working_dir: dir,
            containers: total,
        })
        .collect())
}

/// 对 compose 项目执行 up / down / restart
pub async fn compose_action(name: &str, act: Action) -> Result<String> {
    if name.is_empty()
        || name.len() > 128
        || name.starts_with('-')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        bail!("非法项目名：{name}");
    }

    // 项目可能已停止，需要拿到它的 compose 文件与工作目录
    let (dir, cfg) = locate_project(name).await?;
    let prefix = compose_prefix().await?;

    let prog = prefix[0].clone();
    let mut args: Vec<String> = prefix[1..].to_vec();
    if !cfg.is_empty() {
        args.push("-f".to_string());
        args.push(cfg.clone());
    }
    args.push("-p".to_string());
    args.push(name.to_string());
    match act {
        // up -d：后台运行，避免命令一直挂在前端输出上
        Action::Up => args.extend(["up".to_string(), "-d".to_string()]),
        Action::Down => args.push("down".to_string()),
        Action::Restart => args.push("restart".to_string()),
        _ => bail!("compose 不支持该操作"),
    }

    let mut cmd = Command::new(&prog);
    cmd.args(&args);
    // compose 必须在其工作目录下运行，否则相对路径的 volume / env_file 会找不到
    if !dir.is_empty() {
        cmd.current_dir(&dir);
    }
    let out = timeout(Duration::from_secs(LONG_TIMEOUT), cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("compose 操作超时（{} 秒）", LONG_TIMEOUT))?
        .context("调用 compose 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!("compose 操作失败，stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
        anyhow::bail!("compose 操作失败，详见服务端日志");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 找出项目的 compose 文件与工作目录
async fn locate_project(name: &str) -> Result<(String, String)> {
    for p in compose_projects().await.unwrap_or_default() {
        if p.name == name {
            // v2 的 ConfigFiles 是绝对路径，工作目录取其父目录
            let dir = if p.working_dir.is_empty() && !p.config_files.is_empty() {
                p.config_files.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default()
            } else {
                p.working_dir.clone()
            };
            return Ok((dir, p.config_files));
        }
    }
    Ok((String::new(), String::new()))
}

/// 执行包管理命令并失败重试一次。
///
/// apt 会因「另一个进程持锁」之类的瞬时原因直接返回退出码 100（pacman 同样有锁冲突），
/// 重试一次能挡掉绝大多数偶发失败，比让用户再点一遍按钮友好。
async fn pkg_retry(program: &str, args: &[&str], what: &str) -> Result<String> {
    let mut last = anyhow::anyhow!("{what}失败");
    for attempt in 1..=2 {
        let mut cmd = Command::new(program);
        cmd.args(args);
        if program == "apt-get" {
            cmd.env("DEBIAN_FRONTEND", "noninteractive");
        }
        match crate::packages::run_pkg_cmd(&mut cmd, what).await {
            Ok(mut out) => {
                if attempt > 1 {
                    out.push_str(&format!("\n（第 {attempt} 次尝试成功）\n"));
                }
                return Ok(out);
            }
            Err(e) => {
                last = e;
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    }
    Err(last)
}

/// 一键安装 Docker：按发行版装官方包，然后开机自启并立即启动。
///
/// 各发行版容易踩的点：
/// - Debian 13 把 `docker-cli`（提供 /usr/bin/docker）列为 `docker.io` 的**推荐**包，
///   加了 `--no-install-recommends` 就只装到守护进程，`docker` 命令不会出现 —— 所以必须显式列出；
///   源里没有 `docker-compose-plugin`，只能装独立的 `docker-compose`（Debian 里它同样是 v2 内核）；
/// - Arch 的 `docker-compose` 是官方包，直接可装。
pub async fn install() -> Result<String> {
    use crate::distro::Family;
    let mut log = String::new();

    let (prog, update_args, install_args): (&str, &[&str], &[&str]) = match crate::distro::family() {
        Family::Debian => (
            "apt-get",
            &["update"],
            &[
                "install", "-y", "--no-install-recommends", "docker.io", "docker-cli",
                "docker-compose",
            ],
        ),
        Family::Arch => ("pacman", &["-Sy", "--noconfirm"], &["-S", "--noconfirm", "docker", "docker-compose"]),
    };

    log.push_str("=== 刷新软件源 ===\n");
    log.push_str(&pkg_retry(prog, update_args, "刷新索引").await?);
    log.push('\n');

    log.push_str("\n=== 安装 Docker ===\n");
    log.push_str(&pkg_retry(prog, install_args, "安装 Docker").await?);
    log.push('\n');

    // 启动守护进程：docker.io 装完默认已 enable，这里保证它现在就在跑
    let out = Command::new("systemctl")
        .args(["enable", "--now", "docker"])
        .output()
        .await
        .context("调用 systemctl 失败")?;
    log.push_str("\n=== 启动 docker 服务 ===\n");
    log.push_str(&String::from_utf8_lossy(&out.stdout));
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!("启动 docker 服务失败，systemctl stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
        anyhow::bail!("启动 docker 服务失败，详见服务端日志");
    }

    // 装完自检：CLI 与 daemon 都在才算成功，否则明确告诉用户缺什么
    if !Command::new("docker")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        bail!("安装结束后仍未找到 docker 命令，请检查上方安装输出");
    }
    Ok(log)
}
