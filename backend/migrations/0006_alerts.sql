-- 3.3 告警通知：阈值规则与告警事件历史
-- 规则本身存 settings 表（JSON），这里只建事件表；
-- 事件记录触发/恢复两类状态，供前端展示与排障。
CREATE TABLE IF NOT EXISTS alert_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    metric TEXT NOT NULL,
    value REAL NOT NULL,
    threshold REAL NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('firing', 'resolved'))
);
CREATE INDEX IF NOT EXISTS idx_alert_events_ts ON alert_events(ts);
