-- 4.5 AI 群聊：思考型上游返回的 reasoning_content 需要持久化展示，
-- 给 ai_messages 增加 reasoning 列（旧行默认空串，可安全重放失败后跳过）。
ALTER TABLE ai_messages ADD COLUMN reasoning TEXT NOT NULL DEFAULT '';
