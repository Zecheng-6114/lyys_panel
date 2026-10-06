// 3.4 站点可用性探针
//
// 采样循环盯的是主机指标（CPU / 内存 / 磁盘），而「网站打不开」这类应用层故障
// 在这些指标上根本看不出来：CPU 与内存都很闲，页面却是 502 或超时。这里给每个
// 被监控的 URL 起一条探针：周期 GET，状态码或关键字不符记一次失败，**连续失败达
// 阈值**才判定宕机（避免一次网络抖动就误报），恢复时再报一次。
//
// 探针是旁路：独立任务、独立周期（远慢于 2s 的主机采样），绝不阻塞监控循环；
// 通知复用既有告警渠道（[`crate::alerts::send_with_retry`]），因此未配置渠道时
// 只更新状态、不推送。
//
// 运行态只存内存（进程重启后回到「未知」重新探测）：**刻意不写** `alert_events`
// —— 那张表的值/阈值在前端按百分比渲染，塞站点数据进去会显示成「0.0%」误导人。
use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::db::Db;

/// 探测周期。站点可用性以分钟计变化，高频探测只会给对方服务器添麻烦。
const PROBE_PERIOD: Duration = Duration::from_secs(60);
/// 单次请求超时上限（秒），与校验保持一致
pub const TIMEOUT_MAX: u64 = 30;
/// 被监控目标的数量上限
pub const TARGET_MAX: usize = 50;

fn default_true() -> bool {
    true
}
fn default_fail_threshold() -> u32 {
    2
}
fn default_timeout() -> u64 {
    10
}

/// 一个被监控的目标
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeTarget {
    /// 稳定标识：前端生成后不再变，用于跨轮次记住状态
    pub id: String,
    /// 展示名（告警文案里用它）
    pub name: String,
    /// 完整 URL（含协议）
    pub url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 期望状态码：0 = 任意 2xx 即可
    #[serde(default)]
    pub expect_status: u16,
    /// 正文必须包含的关键字（空 = 不校验正文）
    #[serde(default)]
    pub keyword: String,
    /// 连续失败达到该次数才判定宕机
    #[serde(default = "default_fail_threshold")]
    pub fail_threshold: u32,
    /// 单次请求超时（秒）
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    /// 忽略证书错误（被监控站点用自签证书时勾选）
    #[serde(default)]
    pub insecure: bool,
}

/// 一次探测的结果
#[derive(Clone, Debug)]
pub struct Outcome {
    pub ok: bool,
    /// HTTP 状态码，0 = 未拿到响应
    pub status: u16,
    pub ms: u64,
    /// 失败原因（成功时为空）
    pub error: String,
}

/// 单个目标的当前状态（接口返回给前端）
#[derive(Clone, Debug, Serialize)]
pub struct ProbeStatus {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    /// 当前是否判定宕机
    pub down: bool,
    /// 已连续失败次数
    pub failures: u32,
    pub last_status: u16,
    pub last_ms: u64,
    pub last_error: String,
    /// 最近一次探测时间（unix 秒；0 = 尚未探测）
    pub last_check: i64,
    /// 累计探测次数 / 累计失败次数（进程重启后归零）
    pub checks: u64,
    pub failures_total: u64,
}

/// 状态翻转事件（宕机 / 恢复）
#[derive(Clone, Debug)]
pub struct ProbeEvent {
    pub name: String,
    pub url: String,
    pub firing: bool,
    pub status: u16,
    pub ms: u64,
    /// 宕机原因（恢复事件为空）
    pub reason: String,
}

/// 每个目标的运行态
#[derive(Default)]
struct TargetState {
    down: bool,
    failures: u32,
    last_status: u16,
    last_ms: u64,
    last_error: String,
    last_check: i64,
    checks: u64,
    failures_total: u64,
}

/// 探针引擎：跨轮次保存状态，推进滞回（连续失败达阈值才翻转为宕机）
#[derive(Default)]
pub struct ProbeEngine {
    states: HashMap<String, TargetState>,
}

impl ProbeEngine {
    /// 记录一次探测结果，仅在状态**翻转**时返回事件
    pub fn apply(&mut self, t: &ProbeTarget, out: &Outcome, now: i64) -> Option<ProbeEvent> {
        let st = self.states.entry(t.id.clone()).or_default();
        st.checks += 1;
        st.last_check = now;
        st.last_status = out.status;
        st.last_ms = out.ms;
        st.last_error = if out.ok {
            String::new()
        } else {
            out.error.clone()
        };
        if out.ok {
            st.failures = 0;
            if st.down {
                st.down = false;
                return Some(ProbeEvent {
                    name: t.name.clone(),
                    url: t.url.clone(),
                    firing: false,
                    status: out.status,
                    ms: out.ms,
                    reason: String::new(),
                });
            }
        } else {
            st.failures += 1;
            st.failures_total += 1;
            if !st.down && st.failures >= t.fail_threshold.max(1) {
                st.down = true;
                return Some(ProbeEvent {
                    name: t.name.clone(),
                    url: t.url.clone(),
                    firing: true,
                    status: out.status,
                    ms: out.ms,
                    reason: out.error.clone(),
                });
            }
        }
        None
    }

    /// 组装状态视图，顺序与配置一致；未探测过的目标各字段为 0（前端据此显示「未知」）
    pub fn snapshot(&self, targets: &[ProbeTarget]) -> Vec<ProbeStatus> {
        targets
            .iter()
            .map(|t| {
                let st = self.states.get(&t.id);
                ProbeStatus {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    url: t.url.clone(),
                    enabled: t.enabled,
                    down: st.is_some_and(|s| s.down),
                    failures: st.map(|s| s.failures).unwrap_or(0),
                    last_status: st.map(|s| s.last_status).unwrap_or(0),
                    last_ms: st.map(|s| s.last_ms).unwrap_or(0),
                    last_error: st.map(|s| s.last_error.clone()).unwrap_or_default(),
                    last_check: st.map(|s| s.last_check).unwrap_or(0),
                    checks: st.map(|s| s.checks).unwrap_or(0),
                    failures_total: st.map(|s| s.failures_total).unwrap_or(0),
                }
            })
            .collect()
    }

    /// 丢弃已从配置里删除的目标的残留状态（已禁用的目标保留，重新启用时状态可续）
    pub fn retain(&mut self, targets: &[ProbeTarget]) {
        self.states
            .retain(|id, _| targets.iter().any(|t| &t.id == id));
    }
}

/// 探测单个目标。跟随重定向（reqwest 默认），故期望码按**最终**响应判定。
pub async fn check(client: &reqwest::Client, t: &ProbeTarget) -> Outcome {
    let started = std::time::Instant::now();
    let elapsed = move || started.elapsed().as_millis() as u64;
    let timeout = Duration::from_secs(t.timeout_s.clamp(1, TIMEOUT_MAX));
    let resp = match client.get(t.url.trim()).timeout(timeout).send().await {
        Ok(r) => r,
        Err(e) => {
            return Outcome {
                ok: false,
                status: 0,
                ms: elapsed(),
                error: short_err(&e),
            };
        }
    };
    let status = resp.status().as_u16();
    let status_ok = if t.expect_status == 0 {
        resp.status().is_success()
    } else {
        status == t.expect_status
    };
    if !status_ok {
        return Outcome {
            ok: false,
            status,
            ms: elapsed(),
            error: format!("状态码不符（HTTP {status}）"),
        };
    }
    let kw = t.keyword.trim();
    if !kw.is_empty() {
        let body = resp.text().await.unwrap_or_default();
        if !body.contains(kw) {
            return Outcome {
                ok: false,
                status,
                ms: elapsed(),
                error: format!("正文未包含「{kw}」"),
            };
        }
    }
    Outcome {
        ok: true,
        status,
        ms: elapsed(),
        error: String::new(),
    }
}

/// 把 reqwest 的错误压成一句可读短文案（原文末尾常带宽泛的 source，截断）
fn short_err(e: &reqwest::Error) -> String {
    let prefix = if e.is_timeout() {
        "请求超时："
    } else if e.is_connect() {
        "连接失败："
    } else {
        ""
    };
    let mut s = e.to_string();
    if s.chars().count() > 120 {
        s = s.chars().take(120).collect::<String>() + "…";
    }
    format!("{prefix}{s}")
}

/// 目标持久化读写（存 settings 表，与告警规则/渠道同机制）
pub fn load_targets(db: &Db) -> Vec<ProbeTarget> {
    db.get_setting("alert_probes")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_targets(db: &Db, targets: &[ProbeTarget]) -> anyhow::Result<()> {
    let json = serde_json::to_string(targets)?;
    db.set_setting("alert_probes", &json)
}

/// 目标合法性：地址白名单复用 webhook 那套（仅 http/https、拒绝内嵌 userinfo）
pub fn validate_target(t: &ProbeTarget) -> anyhow::Result<()> {
    let name = t.name.trim();
    anyhow::ensure!(!name.is_empty(), "名称不能为空");
    anyhow::ensure!(name.chars().count() <= 64, "名称过长（上限 64 字符）");
    anyhow::ensure!(!name.chars().any(char::is_control), "名称含控制字符");
    anyhow::ensure!(
        !t.id.is_empty()
            && t.id.len() <= 64
            && t.id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "目标 id 非法"
    );
    crate::alerts::validate_webhook_url(t.url.trim())
        .map_err(|e| anyhow::anyhow!("地址不合法：{e}"))?;
    anyhow::ensure!(
        t.expect_status == 0 || (100..=599).contains(&t.expect_status),
        "期望状态码须为 0（任意 2xx）或 100-599"
    );
    anyhow::ensure!(t.keyword.chars().count() <= 128, "关键字过长（上限 128 字符）");
    anyhow::ensure!((1..=10).contains(&t.fail_threshold), "连续失败次数须在 1-10 之间");
    anyhow::ensure!(
        (1..=TIMEOUT_MAX).contains(&t.timeout_s),
        "超时须在 1-{TIMEOUT_MAX} 秒之间"
    );
    Ok(())
}

/// 后台探针任务：每 PROBE_PERIOD 把启用目标探一遍，状态翻转时经既有渠道通知。
/// 同时监听设置页的配置变更信号，新增/修改的目标不必等满一个周期。
pub fn spawn_prober(state: AppState) {
    tokio::spawn(async move {
        let build = |insecure: bool| {
            reqwest::Client::builder()
                .user_agent("lyys-panel-probe/1.0")
                .danger_accept_invalid_certs(insecure)
                .build()
        };
        let (Ok(client), Ok(client_insecure)) = (build(false), build(true)) else {
            tracing::warn!("站点探针：HTTP 客户端创建失败，探针未启动");
            return;
        };
        let db = state.db.clone();
        let engine = state.probes.clone();
        let mut reload = state.alert_reload.subscribe();
        let mut reload_live = true;
        loop {
            let targets = load_targets(&db);
            for t in targets.iter().filter(|t| t.enabled) {
                let client = if t.insecure { &client_insecure } else { &client };
                let out = check(client, t).await;
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                // 锁只在这几行内持有，绝不跨 await（std 锁跨 await 会阻塞执行器）
                let ev = {
                    let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
                    e.apply(t, &out, now)
                };
                if let Some(ev) = ev {
                    if ev.firing {
                        tracing::info!("站点探针：{} 不可访问（{}）", ev.name, ev.reason);
                    } else {
                        tracing::info!("站点探针：{} 已恢复（HTTP {}）", ev.name, ev.status);
                    }
                    spawn_notify(&db, ev);
                }
            }
            {
                let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
                e.retain(&targets);
            }
            tokio::select! {
                _ = tokio::time::sleep(PROBE_PERIOD) => {}
                changed = reload.changed(), if reload_live => {
                    // sender 意外消失就关掉该分支，否则 changed() 会立即返回 Err 造成忙循环
                    if changed.is_err() {
                        reload_live = false;
                    }
                }
            }
        }
    });
}

/// 翻转时经所有已配置渠道推送（复用 3.3 的渠道与退避重试）
fn spawn_notify(db: &Db, ev: ProbeEvent) {
    let db = db.clone();
    tokio::spawn(async move {
        let channels = crate::alerts::load_channels(&db);
        if channels.is_empty() {
            return;
        }
        let msg = if ev.firing {
            format!(
                "[LYYS 面板告警] 站点「{}」不可访问：{}（{}）",
                ev.name, ev.reason, ev.url
            )
        } else {
            format!(
                "[LYYS 面板告警] 站点「{}」已恢复：HTTP {}，{}ms（{}）",
                ev.name, ev.status, ev.ms, ev.url
            )
        };
        let client = reqwest::Client::new();
        for ch in &channels {
            crate::alerts::send_with_retry(&client, ch, &msg).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(fail_threshold: u32) -> ProbeTarget {
        ProbeTarget {
            id: "t1".into(),
            name: "示例站".into(),
            url: "https://example.com/".into(),
            enabled: true,
            expect_status: 0,
            keyword: String::new(),
            fail_threshold,
            timeout_s: 5,
            insecure: false,
        }
    }

    fn outcome(ok: bool) -> Outcome {
        Outcome {
            ok,
            status: if ok { 200 } else { 0 },
            ms: 12,
            error: if ok { String::new() } else { "连接失败".into() },
        }
    }

    /// 滞回：连续失败到阈值才翻转一次，之后持续失败不再重复报
    #[test]
    fn hysteresis_reports_only_on_flip() {
        let t = target(2);
        let mut e = ProbeEngine::default();
        assert!(e.apply(&t, &outcome(false), 1).is_none(), "首次失败未达阈值");
        let ev = e.apply(&t, &outcome(false), 2).expect("第二次失败应翻转");
        assert!(ev.firing && ev.reason.contains("连接失败"));
        assert!(e.apply(&t, &outcome(false), 3).is_none(), "持续失败不重复报");

        let ev = e.apply(&t, &outcome(true), 4).expect("成功应恢复");
        assert!(!ev.firing && ev.status == 200);
        assert!(e.apply(&t, &outcome(true), 5).is_none(), "持续正常不重复报");
    }

    /// 中途成功应清零失败计数：抖动不该累积成宕机
    #[test]
    fn success_resets_failure_streak() {
        let t = target(3);
        let mut e = ProbeEngine::default();
        assert!(e.apply(&t, &outcome(false), 1).is_none());
        assert!(e.apply(&t, &outcome(true), 2).is_none(), "成功清零");
        assert!(e.apply(&t, &outcome(false), 3).is_none());
        assert!(e.apply(&t, &outcome(false), 4).is_none(), "仅累计 2 次，未达 3");
        assert!(e.apply(&t, &outcome(false), 5).is_some(), "第 3 次连续失败才翻转");
    }

    /// 状态视图按配置顺序输出，未探测过的目标显示为「未知」（last_check = 0）
    #[test]
    fn snapshot_follows_config_order_and_marks_unknown() {
        let t1 = target(1);
        let mut t2 = target(1);
        t2.id = "t2".into();
        t2.name = "第二个".into();
        let mut e = ProbeEngine::default();
        e.apply(&t1, &outcome(false), 7);
        let snap = e.snapshot(&[t2.clone(), t1]);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].name, "第二个");
        assert_eq!(snap[0].last_check, 0, "未探测目标应为未知");
        assert!(snap[1].down, "阈值 1 且已失败一次，应为宕机");
    }

    /// 删除目标后残留状态被清掉（重新添加同名 id 时不会带着旧状态复活）
    #[test]
    fn retain_drops_removed_targets() {
        let t = target(1);
        let mut e = ProbeEngine::default();
        e.apply(&t, &outcome(false), 1);
        e.retain(&[]);
        let snap = e.snapshot(&[t]);
        assert_eq!(snap[0].last_check, 0);
        assert!(!snap[0].down);
    }

    /// 校验：地址、期望码、阈值、超时
    #[test]
    fn validation_rejects_bad_input() {
        let mut t = target(2);
        assert!(validate_target(&t).is_ok());

        t.url = "ftp://example.com".into();
        assert!(validate_target(&t).is_err(), "非 http/https 应拒绝");

        t.url = "https://user:pass@example.com/".into();
        assert!(validate_target(&t).is_err(), "内嵌凭据应拒绝");

        t.url = "https://example.com/".into();
        t.expect_status = 99;
        assert!(validate_target(&t).is_err(), "期望码超范围应拒绝");

        t.expect_status = 0;
        t.fail_threshold = 0;
        assert!(validate_target(&t).is_err(), "阈值 0 应拒绝");

        t.fail_threshold = 2;
        t.timeout_s = TIMEOUT_MAX + 1;
        assert!(validate_target(&t).is_err(), "超时应拒绝");

        t.timeout_s = 5;
        t.name = "坏\n名字".into();
        assert!(validate_target(&t).is_err(), "名称含控制字符应拒绝");
    }
}
