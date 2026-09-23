use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::process::Command;

/// 读取 systemd journal 最近 n 行
pub async fn journal(unit: Option<&str>, lines: u32) -> Result<String> {
    let lines = lines.clamp(1, 2000);
    let mut cmd = Command::new("journalctl");
    cmd.args(["--no-pager", "--reverse", "--lines", &lines.to_string()]);
    if let Some(u) = unit {
        // 单元名安全校验，防止参数注入
        if u.is_empty()
            || u.len() > 128
            || !u
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '@' | '-' | '_'))
        {
            anyhow::bail!("非法单元名");
        }
        cmd.arg("-u");
        cmd.arg(u);
    }
    let out = cmd.output().await.context("调用 journalctl 失败")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    // journalctl 输出是倒序的，反转行序恢复为时间正序
    let ordered: Vec<&str> = text.lines().rev().collect();
    text = ordered.join("\n");
    Ok(text)
}

/// 读取日志文件尾部 n 行（限制在 /var/log 下）
pub async fn tail_file(path: &str, lines: u32) -> Result<String> {
    let lines = lines.clamp(1, 2000);
    let p = PathBuf::from(path);
    // 安全约束：只允许读取 /var/log 下的文件，且不允许路径穿越
    if !p.starts_with("/var/log") || path.contains("..") {
        anyhow::bail!("仅允许读取 /var/log 下的日志文件");
    }
    let out = Command::new("tail")
        .args(["-n", &lines.to_string()])
        .arg(&p)
        .output()
        .await
        .context("调用 tail 失败")?;
    if !out.status.success() {
        anyhow::bail!(
            "读取文件失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 列出 /var/log 下的可读日志文件
pub async fn list_files() -> Result<Vec<String>> {
    let mut list = Vec::new();
    for entry in std::fs::read_dir("/var/log").context("读取 /var/log 目录失败")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry
            .metadata()
            .map(|m| m.is_file() || m.is_symlink())
            .unwrap_or(false)
        {
            list.push(name);
        }
    }
    list.sort();
    Ok(list)
}
