-- P2-3 认证状态持久化：登录失败退避计数 + token 吊销名单。
--
-- 这两类状态原先只驻留进程内存，服务一重启就清零：已登出的 token 会重新
-- 可用、爆破方的退避进度归零。改为落 SQLite 后语义跨重启连续。
--
-- 统一用 key/value + expire_at 一表承载，因为两者都是「带过期时间的
-- 临时状态」，清理动作可以共用一条按 expire_at 的删除语句：
--   - 退避计数   key = 'throttle:<ip>|<用户名>'，value = JSON {count,last,next}
--   - 吊销记录   key = 'revoked:<jti>'，value 为空串，expire_at = token 的 exp
CREATE TABLE IF NOT EXISTS auth_state (
    key       TEXT PRIMARY KEY,
    value     TEXT NOT NULL DEFAULT '',
    expire_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_auth_state_expire ON auth_state (expire_at);
