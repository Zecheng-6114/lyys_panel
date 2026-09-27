-- 2.1 多用户 + RBAC：users 表加角色与强制改密标记。
-- role：admin（全部权限）/ operator（业务写操作）/ viewer（只读）。
-- 既有 admin 账号默认升为 admin 角色、不强制改密（must_change=0），
-- 避免升级部署把当前登录锁死；新建用户默认 must_change=1（首登强制改密）。
ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'admin';
ALTER TABLE users ADD COLUMN must_change INTEGER NOT NULL DEFAULT 0;
