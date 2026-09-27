-- 2.3 审计日志：记录所有非 GET 业务请求。
-- username 冗余存储（用户删除后审计记录仍可读）；path 只存请求路径，
-- 不存查询串与请求体，避免敏感参数（密码等）落库。
CREATE TABLE IF NOT EXISTS audit_log (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       INTEGER NOT NULL,
    user_id  INTEGER,
    username TEXT    NOT NULL,
    method   TEXT    NOT NULL,
    path     TEXT    NOT NULL,
    status   INTEGER NOT NULL,
    ip       TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);
