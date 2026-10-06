// 3.3 告警通知
//
// 设计：规则以 JSON 数组存 settings 表（key=alert_rules），采样循环每次
// 快照后评估。每条规则有滞回状态机（normal → firing → resolved → normal），
// 避免阈值抖动造成通知风暴：
//   - 指标越过阈值且当前非 firing → 触发（写事件 + 发通知）
//   - 指标回落到阈值以下且当前 firing → 恢复（写事件 + 可选发恢复通知）
// 通知渠道为列表（key=alert_channels），支持通用 webhook 与钉钉/企业微信/
// 飞书/Telegram 机器人 —— 各家负载格式不同，由 ChannelKind 决定。发送失败按
// 退避重试有限次；告警是尽力而为的旁路，绝不能拖垮采样循环。
use serde::{Deserialize, Serialize};

use crate::db::Db;

/// 通知渠道类型。除通用 webhook 外，各家机器人的 JSON 负载格式互不相同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    /// 通用 webhook：`{"text": "..."}`
    Webhook,
    /// 钉钉机器人：`{"msgtype":"text","text":{"content":"..."}}`
    Dingtalk,
    /// 企业微信机器人：`{"msgtype":"text","text":{"content":"..."}}`
    Wecom,
    /// 飞书机器人：`{"msg_type":"text","content":{"text":"..."}}`
    Feishu,
    /// Telegram Bot API：需 bot token 与会话 ID
    Telegram,
}

/// 一条通知渠道
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertChannel {
    pub kind: ChannelKind,
    /// webhook 类渠道的地址（Telegram 不用此项）
    #[serde(default)]
    pub url: String,
    /// Telegram：bot token
    #[serde(default)]
    pub token: String,
    /// Telegram：会话 ID（私聊/群为数字，频道可写 @name）
    #[serde(default)]
    pub chat_id: String,
}


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
pub fn metric_value(
    metric: Metric,
    cpu: f64,
    mem_used: i64,
    mem_total: i64,
    disk_used: i64,
    disk_total: i64,
) -> f64 {
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

/// 读取通知渠道列表。旧的单 webhook 设置（alert_webhook_url）自动迁移为
/// 一条通用渠道，避免升级后原有告警静默失效。
pub fn load_channels(db: &Db) -> Vec<AlertChannel> {
    if let Some(list) = db
        .get_setting("alert_channels")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<AlertChannel>>(&s).ok())
    {
        return list;
    }
    match load_legacy_webhook(db) {
        Some(url) => vec![AlertChannel {
            kind: ChannelKind::Webhook,
            url,
            token: String::new(),
            chat_id: String::new(),
        }],
        None => Vec::new(),
    }
}

pub fn save_channels(db: &Db, channels: &[AlertChannel]) -> anyhow::Result<()> {
    let json = serde_json::to_string(channels)?;
    db.set_setting("alert_channels", &json)
}

/// 旧版单 webhook 设置（仅供 load_channels 迁移读取）
fn load_legacy_webhook(db: &Db) -> Option<String> {
    db.get_setting("alert_webhook_url")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// 渠道合法性：webhook 类校验地址，Telegram 校验 token 与会话 ID
pub fn validate_channel(ch: &AlertChannel) -> anyhow::Result<()> {
    match ch.kind {
        ChannelKind::Webhook
        | ChannelKind::Dingtalk
        | ChannelKind::Wecom
        | ChannelKind::Feishu => validate_webhook_url(&ch.url),
        ChannelKind::Telegram => {
            let token = ch.token.trim();
            anyhow::ensure!(!token.is_empty(), "Telegram 需要填写 bot token");
            anyhow::ensure!(
                token.len() <= 128
                    && token
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-'),
                "Telegram bot token 含非法字符"
            );
            let chat = ch.chat_id.trim();
            anyhow::ensure!(!chat.is_empty(), "Telegram 需要填写会话 ID");
            anyhow::ensure!(
                chat.len() <= 64
                    && chat
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '@' || c == '_' || c == '-'),
                "Telegram 会话 ID 含非法字符"
            );
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

/// 组装渠道的请求地址与 JSON 负载（各家格式不同）
fn channel_request(ch: &AlertChannel, text: &str) -> (String, serde_json::Value) {
    match ch.kind {
        ChannelKind::Webhook => (ch.url.clone(), serde_json::json!({ "text": text })),
        ChannelKind::Dingtalk | ChannelKind::Wecom => (
            ch.url.clone(),
            serde_json::json!({ "msgtype": "text", "text": { "content": text } }),
        ),
        ChannelKind::Feishu => (
            ch.url.clone(),
            serde_json::json!({ "msg_type": "text", "content": { "text": text } }),
        ),
        ChannelKind::Telegram => (
            format!("https://api.telegram.org/bot{}/sendMessage", ch.token.trim()),
            serde_json::json!({ "chat_id": ch.chat_id.trim(), "text": text }),
        ),
    }
}

/// 业务层是否成功：钉钉/企业微信/飞书/Telegram 都会用 HTTP 200 包裹业务错误，
/// 必须解析响应体才能判定。非 JSON 响应（通用 webhook 常返回纯文本）以 HTTP 状态为准。
fn body_ok(kind: ChannelKind, body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return true;
    };
    let code = |key: &str| v.get(key).and_then(|x| x.as_i64()).unwrap_or(0);
    match kind {
        ChannelKind::Webhook => true,
        ChannelKind::Dingtalk | ChannelKind::Wecom => code("errcode") == 0,
        ChannelKind::Feishu => code("code") == 0,
        ChannelKind::Telegram => v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
    }
}

/// 向单个渠道发送一条文本告警，返回 Ok 表示渠道确认接收。
pub async fn send_channel(
    client: &reqwest::Client,
    ch: &AlertChannel,
    text: &str,
) -> anyhow::Result<()> {
    let (url, body) = channel_request(ch, text);
    let resp = client.post(url).json(&body).send().await?;
    let status = resp.status();
    let text_body = resp.text().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success() && body_ok(ch.kind, &text_body),
        "渠道返回异常（HTTP {status}）：{}",
        text_body.chars().take(200).collect::<String>()
    );
    Ok(())
}

/// 带退避重试地发送：告警是旁路，有限次重试后放弃，绝不无限重试。
pub async fn send_with_retry(client: &reqwest::Client, ch: &AlertChannel, text: &str) {
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        match send_channel(client, ch, text).await {
            Ok(()) => return,
            Err(e) if attempt == MAX_ATTEMPTS => {
                tracing::warn!("告警渠道发送失败（已重试 {attempt} 次，放弃）：{e:#}");
            }
            Err(e) => {
                tracing::warn!("告警渠道发送失败（第 {attempt} 次，稍后重试）：{e:#}");
                tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
            }
        }
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
            let new_state = if over {
                RuleState::Firing
            } else {
                RuleState::Normal
            };
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
        metric_value(
            m,
            self.cpu,
            self.mem_used,
            self.mem_total,
            self.disk_used,
            self.disk_total,
        )
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

    fn channel(kind: ChannelKind, url: &str) -> AlertChannel {
        AlertChannel {
            kind,
            url: url.into(),
            token: String::new(),
            chat_id: String::new(),
        }
    }

    #[test]
    fn channel_payloads_match_each_provider() {
        let text = "CPU 90%";
        let (url, body) = channel_request(&channel(ChannelKind::Webhook, "https://h/h"), text);
        assert_eq!(url, "https://h/h");
        assert_eq!(body["text"], text);

        let (_, body) = channel_request(&channel(ChannelKind::Dingtalk, "https://h/d"), text);
        assert_eq!(body["msgtype"], "text");
        assert_eq!(body["text"]["content"], text);

        let (_, body) = channel_request(&channel(ChannelKind::Feishu, "https://h/f"), text);
        assert_eq!(body["msg_type"], "text");
        assert_eq!(body["content"]["text"], text);

        let tg = AlertChannel {
            kind: ChannelKind::Telegram,
            url: String::new(),
            token: "123:ABC".into(),
            chat_id: "-100".into(),
        };
        let (url, body) = channel_request(&tg, text);
        assert_eq!(url, "https://api.telegram.org/bot123:ABC/sendMessage");
        assert_eq!(body["chat_id"], "-100");
        assert_eq!(body["text"], text);
    }

    #[test]
    fn detects_provider_business_errors() {
        // 这些渠道用 HTTP 200 包裹业务错误，必须看响应体
        assert!(body_ok(ChannelKind::Dingtalk, r#"{"errcode":0}"#));
        assert!(!body_ok(ChannelKind::Dingtalk, r#"{"errcode":310000}"#));
        assert!(body_ok(ChannelKind::Wecom, r#"{"errcode":0}"#));
        assert!(body_ok(ChannelKind::Feishu, r#"{"code":0}"#));
        assert!(!body_ok(ChannelKind::Feishu, r#"{"code":19021}"#));
        assert!(body_ok(ChannelKind::Telegram, r#"{"ok":true}"#));
        assert!(!body_ok(ChannelKind::Telegram, r#"{"ok":false}"#));
        // 非 JSON 正文按 HTTP 状态判定
        assert!(body_ok(ChannelKind::Webhook, "ok"));
    }

    #[test]
    fn validates_channels() {
        let ok = channel(ChannelKind::Feishu, "https://h/h");
        assert!(validate_channel(&ok).is_ok());
        assert!(validate_channel(&channel(ChannelKind::Webhook, "ftp://h")).is_err());
        assert!(validate_channel(&channel(ChannelKind::Dingtalk, "")).is_err());

        let tg = AlertChannel {
            kind: ChannelKind::Telegram,
            url: String::new(),
            token: "12345:AAH-x_y".into(),
            chat_id: "@chan".into(),
        };
        assert!(validate_channel(&tg).is_ok());
        assert!(
            validate_channel(&AlertChannel {
                token: String::new(),
                ..tg.clone()
            })
            .is_err()
        );
        assert!(
            validate_channel(&AlertChannel {
                chat_id: String::new(),
                ..tg.clone()
            })
            .is_err()
        );
        assert!(
            validate_channel(&AlertChannel {
                chat_id: "a b".into(),
                ..tg.clone()
            })
            .is_err()
        );
    }
}
