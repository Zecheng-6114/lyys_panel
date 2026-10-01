-- 4.5 AI 重构：取消多会话/群聊/成员/调度链，改为全局悬浮球单助手。
-- 删除旧会话体系全部表，新建按用户持久化的助手消息表。
-- 人格提示词与技能说明随 API 配置一起存 settings 表 `ai_api_config`（JSON），无需建表。
DROP TABLE IF EXISTS ai_messages;
DROP TABLE IF EXISTS ai_members;
DROP TABLE IF EXISTS ai_room_users;
DROP TABLE IF EXISTS ai_rooms;

CREATE TABLE ai_messages (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id  INTEGER NOT NULL,
    role     TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content  TEXT NOT NULL,
    ts       INTEGER NOT NULL
);
CREATE INDEX idx_ai_messages_user ON ai_messages (user_id, id);
