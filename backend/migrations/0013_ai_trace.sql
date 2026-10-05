-- 4.6 AI 助手轨迹：把一轮回答里的「思考 / 工具调用 / 正文」按时间顺序存下来。
-- 此前只存最终正文，重新打开悬浮球就看不到中间过程（工具调用记录、思维链），
-- 与流式输出时的观感对不上。
--
-- 老数据 parts 为空串：前端回退为只渲染 content，不影响既有历史。
ALTER TABLE ai_messages ADD COLUMN parts TEXT NOT NULL DEFAULT '';
