// 3.4 systemd 定时器（计划任务的另一种承载方式）
//
// crontab（见 `crontab.rs`）适合简单周期任务，但依赖 cron 守护进程，且没有执行
// 日志。这里用 systemd timer + oneshot service 承载：由 systemd 调度，输出进
// journald，可在面板里直接看。
//
// 面板只管理自己创建的单元，命名统一为 `lyys-cron-<id>.timer` / `.service`，
// id 经白名单校验后才用于拼路径，绝不触碰 `/etc/systemd/system` 下的其他文件。
//
// 命令不直接写进 unit：systemd 的引用/转义规则与 shell 不同，直接内插容易写出
// 「看起来一样、执行语义不同」的命令。改为落成脚本文件，单元里只做
// `sh <脚本>` —— 脚本内容不参与 systemd 解析，也不受 `%` 说明符展开影响。
//
// 键值元数据（id / 表达式 / 命令 / 说明 / 是否启用）存 `数据目录/timers/index.json`，
// unit 与脚本作为派生产物按需重写；列表直接读注册表，无须反向解析 unit 文件。
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

const UNIT_DIR: &str = "/etc/systemd/system";
const PREFIX: &str = "lyys-cron-";

/// 单条 systemd 定时任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerJob {
    /// 任务标识（小写字母/数字/连字符），用于拼单元名与脚本名
    pub id: String,
    /// systemd `OnCalendar` 表达式
    pub schedule: String,
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
}

/// 生成一个不与现有任务冲突的标识（十六进制时间戳，天然是合法 slug）
pub fn new_id(existing: &[TimerJob]) -> String {
    let mut n = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as u128;
    loop {
        let id = format!("job-{:x}", n);
        if !existing.iter().any(|j| j.id == id) {
            return id;
        }
        n += 1;
    }
}

fn unit_name(id: &str, ext: &str) -> String {
    format!("{PREFIX}{id}.{ext}")
}

fn unit_path(id: &str, ext: &str) -> PathBuf {
    Path::new(UNIT_DIR).join(unit_name(id, ext))
}

fn timers_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("timers")
}

fn script_path(data_dir: &Path, id: &str) -> PathBuf {
    timers_dir(data_dir).join(format!("{id}.sh"))
}

fn index_path(data_dir: &Path) -> PathBuf {
    timers_dir(data_dir).join("index.json")
}

/// 标识白名单：仅小写字母、数字、连字符，字母数字开头，1–40 字符
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `OnCalendar` 表达式：禁换行、限长，字符集覆盖 systemd 日期语法
pub fn valid_schedule(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.len() <= 80
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | '*' | '-' | ':' | '.' | ',' | '/' | '~')
        })
}

fn validate(job: &TimerJob) -> Result<()> {
    if !valid_id(&job.id) {
        bail!("任务标识非法（仅小写字母、数字、连字符，且字母数字开头）");
    }
    if !valid_schedule(&job.schedule) {
        bail!("OnCalendar 表达式非法（示例：*-*-* 03:00:00）");
    }
    if job.command.trim().is_empty() || job.command.contains('\0') {
        bail!("命令不能为空");
    }
    if job.description.contains('\n') {
        bail!("说明不能包含换行");
    }
    Ok(())
}

/// systemd 说明行：`%` 是说明符前缀，必须转义；空说明退回 id
fn desc_text(job: &TimerJob) -> String {
    let d = if job.description.trim().is_empty() {
        job.id.as_str()
    } else {
        job.description.trim()
    };
    d.replace('%', "%%").replace('\n', " ")
}

/// oneshot 服务单元：执行任务脚本
fn service_unit(job: &TimerJob, script: &Path) -> Result<String> {
    let path = script.to_string_lossy();
    if path.contains('"') {
        bail!("脚本路径包含双引号，无法写入单元");
    }
    Ok(format!(
        "[Unit]\n\
         Description=LYYS Panel 定时任务：{}\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart=/bin/sh {}\n",
        desc_text(job),
        path
    ))
}

/// 定时器单元：按 OnCalendar 触发对应服务
fn timer_unit(job: &TimerJob) -> String {
    format!(
        "[Unit]\n\
         Description=LYYS Panel 定时器：{}\n\
         \n\
         [Timer]\n\
         OnCalendar={}\n\
         Persistent=true\n\
         Unit={}\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n",
        desc_text(job),
        job.schedule.trim(),
        unit_name(&job.id, "service")
    )
}

// ---------- 注册表 ----------

fn load_index(data_dir: &Path) -> Vec<TimerJob> {
    std::fs::read_to_string(index_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_index(data_dir: &Path, jobs: &[TimerJob]) -> Result<()> {
    let dir = timers_dir(data_dir);
    std::fs::create_dir_all(&dir).context("创建定时任务目录失败")?;
    let s = serde_json::to_string_pretty(jobs).context("序列化定时任务失败")?;
    std::fs::write(index_path(data_dir), s).context("写入定时任务注册表失败")?;
    Ok(())
}

/// 任务列表（按 id 排序，顺序稳定）
pub fn list(data_dir: &Path) -> Vec<TimerJob> {
    let mut jobs = load_index(data_dir);
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    jobs
}

// ---------- systemctl ----------

async fn systemctl(args: &[&str]) -> Result<crate::cmd::Output> {
    let mut cmd = Command::new("systemctl");
    cmd.args(args);
    crate::cmd::run(&mut cmd, crate::cmd::Budget::systemd(20)).await
}

/// 执行一条 systemctl 写命令；失败时日志留底、返回统一文案（不回显 stderr，P1-3）
async fn systemctl_ok(args: &[&str], what: &str) -> Result<()> {
    let out = systemctl(args).await.context("调用 systemctl 失败")?;
    if !out.success() {
        tracing::warn!(
            "systemctl {} stderr：{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        bail!("{what}失败，详见服务端日志");
    }
    Ok(())
}

/// 执行一条 systemctl 命令，失败仅告警不中断（用于停用/清理等收尾动作）
async fn systemctl_lenient(args: &[&str]) {
    match systemctl(args).await {
        Ok(out) if !out.success() => tracing::warn!(
            "systemctl {} 返回非零：{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => tracing::warn!("systemctl {} 调用失败：{e:#}", args.join(" ")),
        _ => {}
    }
}

// ---------- 增删改查 ----------

fn write_script(data_dir: &Path, job: &TimerJob) -> Result<()> {
    let dir = timers_dir(data_dir);
    std::fs::create_dir_all(&dir).context("创建定时任务目录失败")?;
    let body = format!("#!/bin/sh\n{}\n", job.command.trim());
    std::fs::write(script_path(data_dir, &job.id), body).context("写入任务脚本失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            script_path(data_dir, &job.id),
            std::fs::Permissions::from_mode(0o700),
        )?;
    }
    Ok(())
}

/// 新建或更新任务。`is_new` 区分两种情况，避免覆盖他人任务或改到不存在的任务。
pub async fn save(data_dir: &Path, job: &TimerJob, is_new: bool) -> Result<()> {
    validate(job)?;
    let mut jobs = load_index(data_dir);
    let exists = jobs.iter().any(|j| j.id == job.id);
    if is_new && exists {
        bail!("任务标识已存在");
    }
    if !is_new && !exists {
        bail!("任务不存在");
    }

    // 先落脚本 + 单元文件，再让 systemd 重新加载；任一步失败都要回滚，避免
    // 磁盘上留下半成品单元。
    if let Err(e) = write_script(data_dir, job).and_then(|()| {
        std::fs::write(unit_path(&job.id, "service"), service_unit(job, &script_path(data_dir, &job.id))?)
            .context("写入服务单元失败")?;
        std::fs::write(unit_path(&job.id, "timer"), timer_unit(job))
            .context("写入定时器单元失败")?;
        Ok(())
    }) {
        cleanup_units(&job.id).await;
        return Err(e);
    }

    if let Err(e) = systemctl_ok(&["daemon-reload"], "重载 systemd 配置").await {
        cleanup_units(&job.id).await;
        return Err(e);
    }

    let timer = unit_name(&job.id, "timer");
    if job.enabled {
        if let Err(e) = systemctl_ok(&["enable", "--now", &timer], "启用定时器").await {
            cleanup_units(&job.id).await;
            return Err(e);
        }
    } else {
        systemctl_lenient(&["disable", "--now", &timer]).await;
    }

    jobs.retain(|j| j.id != job.id);
    jobs.push(job.clone());
    save_index(data_dir, &jobs)?;
    tracing::info!("已保存定时任务 {}（{}）", job.id, job.schedule);
    Ok(())
}

/// 删除任务：停用并移除单元、脚本与注册表条目
pub async fn delete(data_dir: &Path, id: &str) -> Result<()> {
    if !valid_id(id) {
        bail!("任务标识非法");
    }
    let mut jobs = load_index(data_dir);
    if !jobs.iter().any(|j| j.id == id) {
        bail!("任务不存在");
    }
    systemctl_lenient(&["disable", "--now", &unit_name(id, "timer")]).await;
    cleanup_units(id).await;
    let _ = std::fs::remove_file(script_path(data_dir, id));
    jobs.retain(|j| j.id != id);
    save_index(data_dir, &jobs)?;
    tracing::info!("已删除定时任务 {id}");
    Ok(())
}

/// 移除单元文件并重载 systemd（收尾动作，失败只告警）
async fn cleanup_units(id: &str) {
    let _ = std::fs::remove_file(unit_path(id, "timer"));
    let _ = std::fs::remove_file(unit_path(id, "service"));
    systemctl_lenient(&["daemon-reload"]).await;
}

/// 立即执行一次（触发 oneshot 服务，不进调度队列）
pub async fn run_now(data_dir: &Path, id: &str) -> Result<()> {
    if !valid_id(id) {
        bail!("任务标识非法");
    }
    if !load_index(data_dir).iter().any(|j| j.id == id) {
        bail!("任务不存在");
    }
    let svc = unit_name(id, "service");
    systemctl_ok(&["start", &svc], "执行任务").await
}

/// 读取最近的执行日志（journald）。行数上限 1000，避免无界拉取。
pub async fn logs(id: &str, lines: usize) -> Result<String> {
    if !valid_id(id) {
        bail!("任务标识非法");
    }
    let n = lines.clamp(1, 1000).to_string();
    let svc = unit_name(id, "service");
    let mut cmd = Command::new("journalctl");
    cmd.args(["-u", &svc, "-n", &n, "--no-pager", "-o", "short-iso"]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(15))
        .await
        .context("调用 journalctl 失败")?;
    if !out.success() {
        tracing::warn!(
            "journalctl stderr：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        bail!("读取执行日志失败（journald 不可用？）");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lyys_timer_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn job() -> TimerJob {
        TimerJob {
            id: "job-abc".into(),
            schedule: "*-*-* 03:30:00".into(),
            command: "echo hello".into(),
            description: "夜间备份".into(),
            enabled: true,
        }
    }

    #[test]
    fn id_and_schedule_validation() {
        assert!(valid_id("job-abc123"));
        assert!(!valid_id(""));
        assert!(!valid_id("-lead"));
        assert!(!valid_id("UPPER"));
        assert!(!valid_id("has space"));
        assert!(!valid_id("has/slash"));
        assert!(!valid_id(&"a".repeat(41)));

        assert!(valid_schedule("*-*-* 03:00:00"));
        assert!(valid_schedule("Mon..Fri 09:00"));
        assert!(valid_schedule("hourly"));
        assert!(!valid_schedule(""));
        // 表达式里的换行会破坏单元文件结构，必须拒绝（首尾空白会被 trim，属正常）
        assert!(!valid_schedule("*-*-*\n03:00"));
        assert!(!valid_schedule("03:00;rm"));
    }

    #[test]
    fn unit_contents_are_well_formed() {
        let j = job();
        let script = Path::new("/var/lib/lyys-panel/timers/job-abc.sh");
        let svc = service_unit(&j, script).unwrap();
        assert!(svc.contains("Type=oneshot"));
        assert!(svc.contains("ExecStart=/bin/sh /var/lib/lyys-panel/timers/job-abc.sh"));

        let t = timer_unit(&j);
        assert!(t.contains("OnCalendar=*-*-* 03:30:00"));
        assert!(t.contains("Unit=lyys-cron-job-abc.service"));
        assert!(t.contains("WantedBy=timers.target"));

        // 说明里的 % 必须转义，否则 systemd 会当说明符展开
        let mut pct = job();
        pct.description = "进度 100%".into();
        assert!(timer_unit(&pct).contains("100%%"));
    }

    #[test]
    fn validate_rejects_bad_input() {
        let mut j = job();
        j.command = "  ".into();
        assert!(validate(&j).is_err());
        let mut j = job();
        j.schedule = "03:00;rm -rf /".into();
        assert!(validate(&j).is_err());
        let mut j = job();
        j.id = "Bad Id".into();
        assert!(validate(&j).is_err());
        let valid = job();
        assert!(validate(&valid).is_ok());
    }

    #[test]
    fn index_roundtrip_and_new_id() {
        let dir = tmp("index");
        assert!(list(&dir).is_empty());

        let mut a = job();
        a.id = "job-a".into();
        let mut b = job();
        b.id = "job-b".into();
        b.enabled = false;
        save_index(&dir, &[a.clone(), b.clone()]).unwrap();

        let got = list(&dir);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "job-a");
        assert_eq!(got[1].id, "job-b");
        assert!(!got[1].enabled);
        assert_eq!(got[0].command, "echo hello");

        // 新 id 不与现有任务冲突
        let fresh = new_id(&got);
        assert!(valid_id(&fresh));
        assert!(!got.iter().any(|j| j.id == fresh));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
