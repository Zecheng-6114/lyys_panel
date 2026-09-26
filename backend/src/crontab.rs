use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// 单条计划任务（对应 crontab 的一行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronEntry {
    /// 分
    pub minute: String,
    /// 时
    pub hour: String,
    /// 日
    pub day: String,
    /// 月
    pub month: String,
    /// 周
    pub weekday: String,
    /// 命令
    pub command: String,
    /// 注释（可选，显示在该任务上方）
    #[serde(default)]
    pub comment: String,
}

fn valid_field(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 32
        && f.chars().all(|c| c.is_ascii_digit() || matches!(c, '*' | '/' | ',' | '-' | '?'))
}

impl CronEntry {
    fn validate(&self) -> anyhow::Result<()> {
        for (name, v) in [
            ("分", &self.minute),
            ("时", &self.hour),
            ("日", &self.day),
            ("月", &self.month),
            ("周", &self.weekday),
        ] {
            if !valid_field(v) {
                anyhow::bail!("非法的时间字段（{name}）：{v}");
            }
        }
        if self.command.trim().is_empty() || self.command.contains('\n') {
            anyhow::bail!("命令不能为空且不能包含换行");
        }
        if self.comment.contains('\n') {
            anyhow::bail!("注释不能包含换行");
        }
        Ok(())
    }

    fn to_line(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            self.minute, self.hour, self.day, self.month, self.weekday, self.command
        )
    }
}

/// 读取当前 root crontab 原始文本
async fn read_raw() -> anyhow::Result<String> {
    let out = Command::new("crontab").arg("-l").output().await.context("调用 crontab 失败")?;
    // 没有 crontab 时返回非 0，视为空
    if !out.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 写回 root crontab
async fn write_raw(text: &str) -> anyhow::Result<()> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("启动 crontab 失败")?;
    use tokio::io::AsyncWriteExt;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(text.as_bytes())
        .await
        .context("写入 crontab 失败")?;
    child.stdin.as_mut().unwrap().flush().await.ok();
    drop(child.stdin.take());
    let out = child.wait_with_output().await.context("等待 crontab 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显（防内部细节泄露）
        tracing::warn!("crontab stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
        anyhow::bail!("写入 crontab 失败，详见服务端日志");
    }
    Ok(())
}

/// 解析结果：条目列表 + 原样保留的环境设置行（写回时不丢失）
struct CronData {
    entries: Vec<CronEntry>,
    env_lines: Vec<String>,
}

/// 解析 crontab 文本
fn parse(text: &str) -> CronData {
    let mut list = Vec::new();
    let mut env_lines = Vec::new();
    let mut pending_comment = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(c) = t.strip_prefix('#') {
            // 紧邻任务的注释作为该任务的说明；否则忽略
            pending_comment = c.trim().to_string();
            continue;
        }
        // 保留 KEY=VALUE 形式的环境设置行
        if t.contains('=') && !t.contains(' ') {
            env_lines.push(t.to_string());
            pending_comment.clear();
            continue;
        }
        let mut it = t.split_whitespace();
        let (Some(minute), Some(hour), Some(day), Some(month), Some(weekday)) =
            (it.next(), it.next(), it.next(), it.next(), it.next())
        else {
            pending_comment.clear();
            continue;
        };
        let command = t
            .splitn(6, char::is_whitespace)
            .nth(5)
            .unwrap_or("")
            .trim_start()
            .to_string();
        list.push(CronEntry {
            minute: minute.to_string(),
            hour: hour.to_string(),
            day: day.to_string(),
            month: month.to_string(),
            weekday: weekday.to_string(),
            command,
            comment: std::mem::take(&mut pending_comment),
        });
    }
    CronData {
        entries: list,
        env_lines,
    }
}

/// 将解析结果序列化为 crontab 文本
fn serialize(data: &CronData) -> String {
    let mut s = String::new();
    for e in &data.env_lines {
        s.push_str(e);
        s.push('\n');
    }
    for e in &data.entries {
        if !e.comment.is_empty() {
            s.push_str(&format!("# {}\n", e.comment));
        }
        s.push_str(&e.to_line());
        s.push('\n');
    }
    s
}

/// 列出计划任务
pub async fn list() -> anyhow::Result<Vec<CronEntry>> {
    let raw = read_raw().await?;
    Ok(parse(&raw).entries)
}

/// 新增一条任务
pub async fn add(entry: &CronEntry) -> anyhow::Result<()> {
    entry.validate()?;
    let raw = read_raw().await?;
    let mut data = parse(&raw);
    data.entries.push(entry.clone());
    write_raw(&serialize(&data)).await
}

/// 按索引更新一条任务
pub async fn update(index: usize, entry: &CronEntry) -> anyhow::Result<()> {
    entry.validate()?;
    let raw = read_raw().await?;
    let mut data = parse(&raw);
    if index >= data.entries.len() {
        anyhow::bail!("任务不存在");
    }
    data.entries[index] = entry.clone();
    write_raw(&serialize(&data)).await
}

/// 按索引删除一条任务
pub async fn delete(index: usize) -> anyhow::Result<()> {
    let raw = read_raw().await?;
    let mut data = parse(&raw);
    if index >= data.entries.len() {
        anyhow::bail!("任务不存在");
    }
    data.entries.remove(index);
    write_raw(&serialize(&data)).await
}
