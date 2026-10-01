-- 4.5 AI 会话：会话 / AI 成员 / 受邀用户 / 消息流水
-- 第一阶段只做 AI 之间的互相 @（调度链），不含工具调用。
-- 成员级覆盖只允许 model / api_base，密钥一律走全局配置（settings/env），
-- 遵循「密钥不落库」约束。
CREATE TABLE IF NOT EXISTS ai_rooms (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    creator_id  INTEGER NOT NULL,
    visibility  TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public')),
    created     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_members (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    room_id  INTEGER NOT NULL,
    name     TEXT NOT NULL,
    persona  TEXT NOT NULL DEFAULT '',
    is_admin INTEGER NOT NULL DEFAULT 0,
    -- 空串 = 该项回退全局配置（设置页 > 环境变量 > 默认值）
    model    TEXT NOT NULL DEFAULT '',
    api_base TEXT NOT NULL DEFAULT '',
    sort     INTEGER NOT NULL DEFAULT 0,
    UNIQUE (room_id, name)
);
CREATE TABLE IF NOT EXISTS ai_room_users (
    room_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    PRIMARY KEY (room_id, user_id)
);
CREATE TABLE IF NOT EXISTS ai_messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    room_id     INTEGER NOT NULL,
    sender_type TEXT NOT NULL CHECK (sender_type IN ('user', 'ai', 'system')),
    sender_id   INTEGER NOT NULL,
    sender_name TEXT NOT NULL,
    content     TEXT NOT NULL,
    ts          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ai_messages_room ON ai_messages (room_id, id);
