// 3.3 告警通知
//
// 设计：规则以 JSON 数组存 settings 表（key=alert_rules），采样循环每次
// 快照后评估。每条规则有滞回状态机（normal → firing → resolved → normal），
// 避免阈值抖动造成通知风暴：
//   - 指标越过阈值且当前非 firing → 触发（写事件 + 发通知）
//   - 指标回落到阈值以下且当前 firing → 恢复（写事件 + 可选发恢复通知）
// 通知渠道为通用 webhook（飞书/钉钉/Slack 兼容格式：POST JSON），URL 存
// settings（key=alert_webhook_url）。发送失败只记日志不重试——告警是尽力
// 而为的旁路，绝不能拖垮采样循环。
use serde::{Deserialize, Serialize};

use crate::db::Db;

/// 规则可监控的指标（与 Snapshot 字段一一对应）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// CPU 使用率（%）
    Cpu,
    /// 内存使用率（%）
    Mem,
    /// 磁盘使用率（%）
    Disk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub metric: Metric,
    /// 阈值（百分比 0-100）
    pub threshold: f64,
    /// 是否在恢复时也发 webhook（事件始终记录）
    #[serde(default)]
    pub notify_resolve: bool,
}

/// 规则运行态（滞回状态）：与 AlertRule 按顺序一一对应
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleState {
    Normal,
    Firing,
}

/// 评估输入：从快照提取三个指标的当前值（%）
pub fn metric_value(metric: Metric, cpu: f64, mem_used: i64, mem_total: i64, disk_used: i64, disk_total: i64) -> f64 {
    match metric {
        Metric::Cpu => cpu,
        Metric::Mem => {
            if mem_total > 0 {
                mem_used as f64 / mem_total as f64 * 100.0
            } else {
                0.0
            }
        }
        Metric::Disk => {
            if disk_total > 0 {
                disk_used as f64 / disk_total as f64 * 100.0
            } else {
                0.0
            }
        }
    }
}

/// 规则集持久化读写
pub fn load_rules(db: &Db) -> Vec<AlertRule> {
    db.get_setting("alert_rules")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_rules(db: &Db, rules: &[AlertRule]) -> anyhow::Result<()> {
    let json = serde_json::to_string(rules)?;
    db.set_setting("alert_rules", &json)
}

pub fn load_webhook(db: &Db) -> Option<String> {
    db.get_setting("alert_webhook_url")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

pub fn save_webhook(db: &Db, url: Option<&str>) -> anyhow::Result<()> {
    match url {
        Some(u) if !u.is_empty() => {
            validate_webhook_url(u)?;
            db.set_setting("alert_webhook_url", u)
        }
        _ => {
            let _ = db.remove_setting("alert_webhook_url");
            Ok(())
        }
    }
}

/// webhook URL 白名单：仅 http/https + 合法 URL 字符（ASCII），
/// 拒绝 userinfo（防把凭据藏在 URL 里外发）与空白/控制字符。
pub fn validate_webhook_url(url: &str) -> anyhow::Result<()> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| anyhow::anyhow!("webhook 仅支持 http/https"))?;
    if rest.is_empty() {
        anyhow::bail!("webhook 缺少主机名");
    }
    if !rest.bytes().all(|b| b.is_ascii_graphic() && b != b' ') {
        anyhow::bail!("webhook 含非法字符");
    }
    if rest.contains('@') {
        anyhow::bail!("webhook 不允许包含用户名密码");
    }
    Ok(())
}

/// 规则合法性：阈值 1-99
pub fn validate_rule(rule: &AlertRule) -> anyhow::Result<()> {
    anyhow::ensure!(
        rule.threshold >= 1.0 && rule.threshold <= 99.0,
        "阈值必须在 1-99 之间"
    );
    Ok(())
}

/// 发送 webhook 通知（fire-and-forget 由调用方 spawn）。
/// 通用文本格式，飞书/钉钉/Slack 都能直接渲染。
pub async fn send_webhook(client: &reqwest::Client, url: &str, text: &str) {
    let body = serde_json::json!({ "text": text });
    match client.post(url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => tracing::warn!("告警 webhook 返回异常状态：{}", resp.status()),
        Err(e) => tracing::warn!("告警 webhook 发送失败：{e}"),
    }
}

/// 一次采样评估：推进滞回状态机，返回需要持久化/发送的事件。
/// states 与 rules 一一对应，调用方负责保持跨采样连续。
pub struct AlertEngine {
    pub states: Vec<RuleState>,
}

/// 评估产生的事件（state=firing/resolved）
#[derive(Debug, Clone)]
pub struct AlertEvent {
    pub metric: Metric,
    pub value: f64,
    pub threshold: f64,
    pub firing: bool,
}

impl AlertEngine {
    pub fn new(rules: &[AlertRule]) -> Self {
        Self {
            states: vec![RuleState::Normal; rules.len()],
        }
    }

    /// 规则集变更后重建状态（旧状态尽量保留，长度对齐新规则）
    pub fn resync(&mut self, rules: &[AlertRule]) {
        self.states.resize(rules.len(), RuleState::Normal);
    }

    /// 评估当前采样值，返回状态发生翻转的事件
    pub fn evaluate(&mut self, rules: &[AlertRule], metrics: Metrics) -> Vec<AlertEvent> {
        let mut events = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            let value = metrics.value(rule.metric);
            let over = value >= rule.threshold;
            let state = self.states.get(i).copied().unwrap_or(RuleState::Normal);
            let new_state = if over { RuleState::Firing } else { RuleState::Normal };
            // 只在状态翻转（触发/恢复）时出事件；持续超阈值不重复，
            // 从未触发过的正常波动不记录，避免历史被噪声淹没
            if new_state != state {
                events.push(AlertEvent {
                    metric: rule.metric,
                    value,
                    threshold: rule.threshold,
                    firing: over,
                });
            }
            if let Some(s) = self.states.get_mut(i) {
                *s = new_state;
            }
        }
        events
    }
}

/// 评估输入打包
#[derive(Clone, Copy)]
pub struct Metrics {
    pub cpu: f64,
    pub mem_used: i64,
    pub mem_total: i64,
    pub disk_used: i64,
    pub disk_total: i64,
}

impl Metrics {
    pub fn value(&self, m: Metric) -> f64 {
        metric_value(m, self.cpu, self.mem_used, self.mem_total, self.disk_used, self.disk_total)
    }
}

pub fn metric_label(m: Metric) -> &'static str {
    match m {
        Metric::Cpu => "CPU",
        Metric::Mem => "内存",
        Metric::Disk => "磁盘",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(metric: Metric, threshold: f64) -> AlertRule {
        AlertRule {
            metric,
            threshold,
            notify_resolve: false,
        }
    }

    fn metrics(cpu: f64) -> Metrics {
        Metrics {
            cpu,
            mem_used: 50,
            mem_total: 100,
            disk_used: 90,
            disk_total: 100,
        }
    }

    #[test]
    fn hysteresis_only_edges() {
        let rules = vec![rule(Metric::Cpu, 80.0)];
        let mut eng = AlertEngine::new(&rules);
        // 正常波动：无事件
        assert!(eng.evaluate(&rules, metrics(50.0)).is_empty());
        // 越过阈值：触发一次
        let ev = eng.evaluate(&rules, metrics(85.0));
        assert_eq!(ev.len(), 1);
        assert!(ev[0].firing);
        // 持续超阈值：不重复触发
        assert!(eng.evaluate(&rules, metrics(90.0)).is_empty());
        // 回落：恢复事件一次
        let ev = eng.evaluate(&rules, metrics(30.0));
        assert_eq!(ev.len(), 1);
        assert!(!ev[0].firing);
        // 再次回落不重复
        assert!(eng.evaluate(&rules, metrics(20.0)).is_empty());
    }

    #[test]
    fn disk_metric_percent() {
        let rules = vec![rule(Metric::Disk, 85.0)];
        let mut eng = AlertEngine::new(&rules);
        let ev = eng.evaluate(&rules, metrics(10.0)); // 磁盘 90% > 85%
        assert_eq!(ev.len(), 1);
        assert!(ev[0].firing);
        assert!((ev[0].value - 90.0).abs() < 1e-9);
    }

    #[test]
    fn webhook_url_validation() {
        assert!(validate_webhook_url("https://open.feishu.cn/bot?v=1").is_ok());
        assert!(validate_webhook_url("http://192.168.1.9:8080/hook").is_ok());
        assert!(validate_webhook_url("ftp://x/h").is_err());
        assert!(validate_webhook_url("https://").is_err());
        assert!(validate_webhook_url("https://user:pw@host/h").is_err());
        assert!(validate_webhook_url("https://host/a b").is_err());
        assert!(validate_webhook_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn rules_json_roundtrip() {
        let rules = vec![rule(Metric::Cpu, 90.0), rule(Metric::Mem, 80.0)];
        let json = serde_json::to_string(&rules).unwrap();
        let back: Vec<AlertRule> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert!((back[1].threshold - 80.0).abs() < 1e-9);
    }
}
