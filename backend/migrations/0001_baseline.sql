-- 基线迁移：收编 1697f2c 时代 init_schema() 的全部建表语句。
-- 全部使用 IF NOT EXISTS：已部署的旧库（无 schema_version 表、表已存在）
-- 首次升级时本迁移必须整体可重放；0002 起的增量迁移不再需要这种保护。
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    salt          TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS metrics (
    ts       INTEGER NOT NULL,
    cpu      REAL NOT NULL,
    mem_used INTEGER NOT NULL,
    net_in   INTEGER NOT NULL,
    net_out  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_metrics_ts ON metrics (ts);
-- AI 助手功能停用中，但表结构保留为基线的一部分（与旧 init_schema 行为一致）
CREATE TABLE IF NOT EXISTS ai_memory (
    id        INTEGER PRIMARY KEY,
    ts        INTEGER NOT NULL,
    content   TEXT NOT NULL,
    embedding BLOB
);
CREATE TABLE IF NOT EXISTS ai_session (
    id        INTEGER PRIMARY KEY,
    created   INTEGER NOT NULL,
    updated   INTEGER NOT NULL,
    title     TEXT NOT NULL,
    msg_count INTEGER NOT NULL,
    messages  TEXT NOT NULL
);
