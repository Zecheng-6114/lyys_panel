-- P2-1 作业表：把长操作（装包 / 拉镜像 / 备份）从 HTTP 请求生命周期里剥离出来。
--
-- 编号说明：细案原文写的是 0011，但该号已被 0011_audit_detail.sql（P1-3）占用。
-- 迁移按 schema_version 单调整数判定，后补的低版本号会被静默跳过，故顺链尾取 0012。
--
-- status 取值：pending / running / success / failed / cancelled / interrupted
-- interrupted 与 failed 是两回事：前者表示「面板在作业运行中被重启」，
-- 与「命令真的失败了」需要在前端区分展示。
CREATE TABLE IF NOT EXISTS jobs (
    id          TEXT PRIMARY KEY,          -- 32 位十六进制（rand 生成，不引 uuid 依赖）
    kind        TEXT NOT NULL,             -- pkg_install / pkg_remove / pkg_upgrade /
                                           -- pkg_sysupgrade / pkg_update / backup_create /
                                           -- docker_pull / docker_install
    payload     TEXT NOT NULL DEFAULT '',  -- 提交参数 JSON（包名列表、镜像引用等）
    status      TEXT NOT NULL,
    exit_code   INTEGER,                   -- 进程退出码，未结束为 NULL
    stdout_tail TEXT NOT NULL DEFAULT '',  -- 最后 200 行输出，行间以 \n 连接
    error       TEXT,                      -- 面板自造的错误文案（不回显 stderr 原文）
    created_at  INTEGER NOT NULL,
    started_at  INTEGER,
    finished_at INTEGER
);
-- 列表默认按创建时间倒序分页
CREATE INDEX IF NOT EXISTS idx_jobs_created ON jobs (created_at DESC);
-- 重启收尾与前端状态筛选都按 status 过滤
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs (status);
