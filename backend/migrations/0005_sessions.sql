-- 2.4 会话管理：在线 token 登记表。
-- 登录时插入（jti 为主键），登出/踢出/过期时删除；吊销名单仍是
-- 强制失效的执行者，本表只负责「看得见」。ua/ip 用于识别会话来源。
CREATE TABLE IF NOT EXISTS sessions (
    jti      TEXT    PRIMARY KEY,
    user_id  INTEGER NOT NULL,
    username TEXT    NOT NULL,
    ua       TEXT    NOT NULL DEFAULT '',
    ip       TEXT    NOT NULL DEFAULT '',
    iat      INTEGER NOT NULL,
    exp      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_exp ON sessions(exp);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
