use std::path::Path;

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::Serialize;

/// 1.2 保留策略常量：原始 5 秒采样保留 7 天；小时聚合保留 1 年。
/// 采样循环（monitor.rs）与历史查询选表（api.rs）共用，避免口径漂移。
pub const RAW_RETENTION_SECS: i64 = 7 * 24 * 3600;
pub const HOURLY_RETENTION_SECS: i64 = 365 * 24 * 3600;
/// 2.3：审计日志保留 90 天（写操作频率低，量级远小于指标采样）
pub const AUDIT_RETENTION_SECS: i64 = 90 * 24 * 3600;

/// 数据库连接池封装
#[derive(Clone)]
pub struct Db {
    pool: Pool<SqliteConnectionManager>,
}

/// 监控历史采样点。
///
/// `disk_read/disk_write` 是 `Option`：迁移（0014）之前的历史行没有落库过磁盘
/// I/O，读出来是 NULL。**不能退化成 0** —— 那会把「没有采集」伪装成
/// 「当时磁盘 I/O 为零」。序列化成 null，前端据此断开折线。
#[derive(Serialize)]
pub struct MetricPoint {
    pub ts: i64,
    pub cpu: f64,
    pub mem_used: i64,
    pub net_in: i64,
    pub net_out: i64,
    pub disk_read: Option<i64>,
    pub disk_write: Option<i64>,
}

/// 用户行（登录校验用，含密码字段，绝不外泄给 API 响应）
pub struct UserRow {
    pub id: i64,
    pub password_hash: String,
    pub salt: String,
    pub role: String,
    pub must_change: bool,
}

/// 审计日志行
#[derive(Serialize)]
pub struct AuditRow {
    pub ts: i64,
    pub user_id: Option<i64>,
    pub username: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub ip: String,
    /// 操作参数摘要（P1-3）。白名单字段拼接，历史条目为 None
    pub detail: Option<String>,
}

/// 在线会话行（jti 不外泄完整值，API 层裁剪后再返回）
pub struct SessionRow {
    pub jti: String,
    pub user_id: i64,
    pub username: String,
    pub ua: String,
    pub ip: String,
    pub iat: i64,
    pub exp: i64,
}

/// 告警事件行
#[derive(Serialize)]
pub struct AlertEventRow {
    pub ts: i64,
    pub metric: String,
    pub value: f64,
    pub threshold: f64,
    pub state: String,
}

/// AI 助手消息行（4.5 悬浮球单助手，按用户持久化）。
#[derive(Clone, Serialize)]
pub struct AiMsgRow {
    pub id: i64,
    pub role: String,
    pub content: String,
    /// 有序轨迹（思考/工具/正文）的 JSON 文本；用户消息与老数据为空串
    pub parts: String,
    pub ts: i64,
}

/// P2-1 作业行。
///
/// 数据库是作业的**唯一事实来源**：进程内的 JoinHandle 索引只用于取消，
/// 两者以 `id` 对齐。这样面板重启后 frontend 仍能看到历史作业及其输出尾部。
#[derive(Clone, Serialize)]
pub struct JobRow {
    pub id: String,
    /// pkg_install / pkg_remove / pkg_upgrade / pkg_sysupgrade / pkg_update /
    /// backup_create / docker_pull / docker_install
    pub kind: String,
    /// 提交参数 JSON（包名列表、镜像引用等）
    pub payload: String,
    /// pending / running / success / failed / cancelled / interrupted
    pub status: String,
    pub exit_code: Option<i64>,
    /// 最后 200 行输出（环形截断）
    pub stdout_tail: String,
    /// 面板自造的错误文案；不回显命令 stderr 原文（P1-3 口径）
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

/// jobs 表整行映射，供 `job_get` / `job_list` 共用（列顺序必须与查询一致）
fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        payload: row.get(2)?,
        status: row.get(3)?,
        exit_code: row.get(4)?,
        stdout_tail: row.get(5)?,
        error: row.get(6)?,
        created_at: row.get(7)?,
        started_at: row.get(8)?,
        finished_at: row.get(9)?,
    })
}

impl Db {
    /// 打开（或创建）SQLite 数据库并执行初始化建表
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).context("创建数据目录失败")?;
            }
        let manager = SqliteConnectionManager::file(path);
        let pool = Pool::builder()
            .max_size(4)
            .build(manager)
            .context("创建数据库连接池失败")?;
        let db = Self { pool };
        db.migrate()?;
        Ok(db)
    }

    /// 迁移列表：(版本号, 文件名, SQL)。SQL 编译期内嵌进二进制，
    /// 运行时不依赖磁盘上的 migrations 目录。
    /// 新增迁移 = 在 backend/migrations/ 建 `NNNN_名字.sql` + 在此追加一行；
    /// 版本号只增不减，已发布迁移的内容不得再修改。
    const MIGRATIONS: &'static [(i64, &'static str, &'static str)] = &[
        (
            1,
            "0001_baseline.sql",
            include_str!("../migrations/0001_baseline.sql"),
        ),
        (
            2,
            "0002_metrics_hourly.sql",
            include_str!("../migrations/0002_metrics_hourly.sql"),
        ),
        (
            3,
            "0003_users_rbac.sql",
            include_str!("../migrations/0003_users_rbac.sql"),
        ),
        (
            4,
            "0004_audit_log.sql",
            include_str!("../migrations/0004_audit_log.sql"),
        ),
        (
            5,
            "0005_sessions.sql",
            include_str!("../migrations/0005_sessions.sql"),
        ),
        (
            6,
            "0006_alerts.sql",
            include_str!("../migrations/0006_alerts.sql"),
        ),
        (
            7,
            "0007_ai_group.sql",
            include_str!("../migrations/0007_ai_group.sql"),
        ),
        (
            8,
            "0008_ai_reasoning.sql",
            include_str!("../migrations/0008_ai_reasoning.sql"),
        ),
        (
            9,
            "0009_ai_assistant.sql",
            include_str!("../migrations/0009_ai_assistant.sql"),
        ),
        (
            10,
            "0010_auth_state.sql",
            include_str!("../migrations/0010_auth_state.sql"),
        ),
        (
            11,
            "0011_audit_detail.sql",
            include_str!("../migrations/0011_audit_detail.sql"),
        ),
        (
            12,
            "0012_jobs.sql",
            include_str!("../migrations/0012_jobs.sql"),
        ),
        (
            13,
            "0013_ai_trace.sql",
            include_str!("../migrations/0013_ai_trace.sql"),
        ),
        (
            14,
            "0014_metrics_disk_io.sql",
            include_str!("../migrations/0014_metrics_disk_io.sql"),
        ),
    ];

    /// 按版本号升序执行未应用的迁移。
    ///
    /// schema_version 表只记录已应用版本号；每个迁移在独立事务里执行，
    /// 任一语句失败整个迁移回滚、启动中止，不会出现半套表结构。
    /// 旧部署（表已存在、无 schema_version）依赖基线迁移全部
    /// IF NOT EXISTS 的整体可重放性，升级路径见测试 legacy_db_upgrades。
    fn migrate(&self) -> Result<()> {
        let mut conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY);",
        )
        .context("创建 schema_version 表失败")?;
        let current: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .context("读取数据库版本号失败")?;
        anyhow::ensure!(
            Self::MIGRATIONS.windows(2).all(|w| w[0].0 < w[1].0),
            "迁移版本号必须严格递增"
        );
        for (ver, name, sql) in Self::MIGRATIONS {
            if *ver <= current {
                continue;
            }
            let tx = conn.transaction().context("开启迁移事务失败")?;
            tx.execute_batch(sql)
                .with_context(|| format!("执行迁移 {name} 失败"))?;
            tx.execute("INSERT INTO schema_version (version) VALUES (?1)", [*ver])
                .with_context(|| format!("记录迁移 {name} 版本失败"))?;
            tx.commit()
                .with_context(|| format!("提交迁移 {name} 失败"))?;
            tracing::info!("已应用数据库迁移 {name}（版本 {ver}）");
        }
        Self::repair_columns(&conn)?;
        Ok(())
    }

    /// 结构自愈：确保「某版本之后应当存在的列」真的在表里。
    ///
    /// 为什么需要：版本号是迁移事务里独立写入的一行，而 `ALTER TABLE` 是 DDL。
    /// 一旦库文件被换掉 / 回滚成更早的快照而 `schema_version` 仍保留较新的版本
    /// （实测 3800 测试库就出现过：版本记到 14，`metrics` 却没有 `disk_read`），
    /// 启动时版本号对得上就跳过迁移，而新代码每 2 秒写一次新列 → 采样全部失败，
    /// 日志被 `table metrics has no column named disk_read` 刷屏。
    ///
    /// 这里按「列是否真的存在」补齐，与迁移是否被跳过无关。代价是启动时
    /// 每条 6 次 `PRAGMA table_info`，可忽略；换来的是一条自愈路径，
    /// 不必再靠人工 `ALTER TABLE` 救场。
    fn repair_columns(conn: &rusqlite::Connection) -> Result<()> {
        /// (表, 列, 类型) —— 只列「加了之后必须存在」的列，与迁移文件一一对应
        const EXPECTED: &[(&str, &str, &str)] = &[
            ("metrics", "disk_read", "INTEGER"),
            ("metrics", "disk_write", "INTEGER"),
            ("metrics_hourly", "disk_read_avg", "INTEGER"),
            ("metrics_hourly", "disk_read_max", "INTEGER"),
            ("metrics_hourly", "disk_write_avg", "INTEGER"),
            ("metrics_hourly", "disk_write_max", "INTEGER"),
        ];
        for (table, column, ty) in EXPECTED {
            // 表本身不存在（老库还没建过）时交给迁移处理，这里跳过
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )?;
            if exists == 0 {
                continue;
            }
            let has_col: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                rusqlite::params![table, column],
                |r| r.get(0),
            )?;
            if has_col == 0 {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))
                    .with_context(|| format!("补齐缺失的列 {table}.{column} 失败"))?;
                tracing::warn!("已补齐缺失的列 {table}.{column}（库结构曾与版本号不一致）");
            }
        }
        Ok(())
    }

    /// 3.1 备份：VACUUM INTO 生成一致性快照（不锁写者、产物去碎片）
    pub fn vacuum_into(&self, target: &Path) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let path_str = target.to_string_lossy().replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{path_str}'"))
            .context("VACUUM INTO 备份失败")?;
        Ok(())
    }

    /// 当前数据库结构版本（供 /api/health 等诊断场景使用）
    pub fn schema_version(&self) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let v = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )?;
        Ok(v)
    }

    /// 读取配置项
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let value = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        Ok(value)
    }

    /// 写入配置项
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(())
    }

    /// 删除配置项（P0-2 迁移用：把遗留的 JWT 密钥从 settings 表移除）
    pub fn remove_setting(&self, key: &str) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(n as u64)
    }

    // ---------- P2-3 认证状态（登录退避 + token 吊销）----------
    //
    // 两类状态都带过期时间，读时按 now 过滤，因此过期条目在被清理任务删除前
    // 也不会产生「已过期却仍然生效」的错误语义（清理只是回收空间）。

    /// 读取未过期的认证状态条目；不存在或已过期均返回 None
    pub fn auth_state_get(&self, key: &str, now: i64) -> Result<Option<String>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let value = conn
            .query_row(
                "SELECT value FROM auth_state WHERE key = ?1 AND expire_at > ?2",
                (key, now),
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value)
    }

    /// 写入/覆盖认证状态条目（`expire_at` 为过期时间戳）
    pub fn auth_state_set(&self, key: &str, value: &str, expire_at: i64) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO auth_state (key, value, expire_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, expire_at = excluded.expire_at",
            (key, value, expire_at),
        )?;
        Ok(())
    }

    /// 删除单条认证状态（登录成功后清除该来源的退避记录）
    pub fn auth_state_remove(&self, key: &str) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM auth_state WHERE key = ?1", [key])?;
        Ok(n as u64)
    }

    /// 清空某一前缀下的全部条目（密钥轮换时清吊销名单用）
    pub fn auth_state_remove_prefix(&self, prefix: &str) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "DELETE FROM auth_state WHERE key LIKE ?1",
            [format!("{prefix}%")],
        )?;
        Ok(n as u64)
    }

    /// 清理过期条目，并把退避记录裁剪到 `throttle_cap` 条上限内（逐出最旧）。
    ///
    /// 上限的意义与内存版 MAX_ENTRIES 相同：海量伪造来源会持续写入新键，
    /// 单靠过期清理在一个小时内不设防。这里按 expire_at 升序保留最新的
    /// `throttle_cap` 条，其余删除。
    pub fn auth_state_prune(&self, now: i64, throttle_cap: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let expired = conn.execute("DELETE FROM auth_state WHERE expire_at <= ?1", [now])?;
        let over = conn.execute(
            "DELETE FROM auth_state WHERE key LIKE 'throttle:%' AND key NOT IN (
                 SELECT key FROM auth_state WHERE key LIKE 'throttle:%'
                 ORDER BY expire_at DESC LIMIT ?1
             )",
            [throttle_cap],
        )?;
        Ok((expired + over) as u64)
    }

    /// 统计用户数量
    pub fn user_count(&self) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(n)
    }

    /// 按用户名查询用户
    pub fn find_user(&self, username: &str) -> Result<Option<(i64, String, String)>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let user = conn
            .query_row(
                "SELECT id, password_hash, salt FROM users WHERE username = ?1",
                [username],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(user)
    }

    /// 创建带角色的用户；must_change=1 表示首次登录需强制改密
    pub fn create_user_role(
        &self,
        username: &str,
        hash: &str,
        salt: &str,
        role: &str,
        must_change: bool,
    ) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO users (username, password_hash, salt, role, must_change)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (username, hash, salt, role, must_change as i64),
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 按 id 查询用户（返回 id/username/role/must_change）
    pub fn user_by_id(&self, id: i64) -> Result<Option<(String, String, bool)>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let r = conn
            .query_row(
                "SELECT username, role, must_change FROM users WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
            )
            .optional()?;
        Ok(r)
    }

    /// 按用户名查询用户（含 id/role/must_change，登录用）
    pub fn find_user_full(&self, username: &str) -> Result<Option<UserRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let r = conn
            .query_row(
                "SELECT id, password_hash, salt, role, must_change FROM users WHERE username = ?1",
                [username],
                |row| {
                    Ok(UserRow {
                        id: row.get(0)?,
                        password_hash: row.get(1)?,
                        salt: row.get(2)?,
                        role: row.get(3)?,
                        must_change: row.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(r)
    }

    /// 列出全部用户（不含密码/盐）
    pub fn list_users(&self) -> Result<Vec<(i64, String, String, bool)>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt =
            conn.prepare("SELECT id, username, role, must_change FROM users ORDER BY id")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, i64>(3)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 更新密码（重设哈希与盐），并清除强制改密标记
    pub fn set_password(&self, id: i64, hash: &str, salt: &str) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "UPDATE users SET password_hash = ?2, salt = ?3, must_change = 0 WHERE id = ?1",
            (id, hash, salt),
        )?;
        Ok(())
    }

    /// 修改用户名与角色（admin 管理他人账号用）
    pub fn update_user(&self, id: i64, username: &str, role: &str) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "UPDATE users SET username = ?2, role = ?3 WHERE id = ?1",
            (id, username, role),
        )?;
        Ok(())
    }

    /// 删除用户
    pub fn delete_user(&self, id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM users WHERE id = ?1", [id])?;
        Ok(n as u64)
    }

    /// 用户是否存在（删除/改名前的自保护检查用）
    pub fn user_exists(&self, id: i64) -> Result<bool> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM users WHERE id = ?1", [id], |r| {
            r.get(0)
        })?;
        Ok(n > 0)
    }

    // ---------- 审计日志（2.3） ----------

    /// 写入一条审计记录
    #[allow(clippy::too_many_arguments)]
    pub fn audit(
        &self,
        ts: i64,
        user_id: Option<i64>,
        username: &str,
        method: &str,
        path: &str,
        status: u16,
        ip: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO audit_log (ts, user_id, username, method, path, status, ip, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                ts,
                user_id,
                username,
                method,
                path,
                status as i64,
                ip,
                detail,
            ),
        )?;
        Ok(())
    }

    /// 分页读取审计日志（按时间倒序）
    pub fn audit_list(&self, limit: i64, offset: i64) -> Result<Vec<AuditRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT ts, user_id, username, method, path, status, ip, detail
             FROM audit_log ORDER BY ts DESC, id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit, offset], |row| {
                Ok(AuditRow {
                    ts: row.get(0)?,
                    user_id: row.get(1)?,
                    username: row.get(2)?,
                    method: row.get(3)?,
                    path: row.get(4)?,
                    status: row.get::<_, i64>(5)? as u16,
                    ip: row.get(6)?,
                    detail: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 清理早于 before 的审计记录
    pub fn audit_prune(&self, before: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM audit_log WHERE ts < ?1", [before])?;
        Ok(n as u64)
    }

    // ---------- 告警事件（3.3） ----------

    /// 写入一条告警事件（firing / resolved）
    pub fn alert_event_add(
        &self,
        ts: i64,
        metric: &str,
        value: f64,
        threshold: f64,
        state: &str,
    ) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO alert_events (ts, metric, value, threshold, state) VALUES (?1, ?2, ?3, ?4, ?5)",
            (ts, metric, value, threshold, state),
        )?;
        Ok(())
    }

    /// 分页读取告警事件（按时间倒序）
    pub fn alert_event_list(&self, limit: i64, offset: i64) -> Result<Vec<AlertEventRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT ts, metric, value, threshold, state
             FROM alert_events ORDER BY ts DESC, id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit, offset], |row| {
                Ok(AlertEventRow {
                    ts: row.get(0)?,
                    metric: row.get(1)?,
                    value: row.get(2)?,
                    threshold: row.get(3)?,
                    state: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 清理早于 before 的告警事件
    pub fn alert_event_prune(&self, before: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM alert_events WHERE ts < ?1", [before])?;
        Ok(n as u64)
    }

    // ---------- 会话登记（2.4） ----------

    /// 登录时登记会话
    #[allow(clippy::too_many_arguments)]
    pub fn session_add(
        &self,
        jti: &str,
        user_id: i64,
        username: &str,
        ua: &str,
        ip: &str,
        iat: i64,
        exp: i64,
    ) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT OR REPLACE INTO sessions (jti, user_id, username, ua, ip, iat, exp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (jti, user_id, username, ua, ip, iat, exp),
        )?;
        Ok(())
    }

    /// 登出/踢出时移除会话
    pub fn session_remove(&self, jti: &str) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM sessions WHERE jti = ?1", [jti])?;
        Ok(n as u64)
    }

    /// 会话是否仍在登记表中（鉴权时校验，被踢出/改密失效的 token 查不到）
    pub fn session_exists(&self, jti: &str) -> Result<bool> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM sessions WHERE jti = ?1", [jti], |r| {
                r.get(0)
            })?;
        Ok(n > 0)
    }

    /// 移除某用户的全部会话（改密/删除用户时失效其所有登录）
    pub fn session_remove_user(&self, user_id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
        Ok(n as u64)
    }

    /// 移除某用户除指定会话外的全部会话（本人改密时保留当前登录）
    pub fn session_remove_user_except(&self, user_id: i64, keep_jti: &str) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND jti != ?2",
            rusqlite::params![user_id, keep_jti],
        )?;
        Ok(n as u64)
    }

    /// 列出未过期会话（按签发时间倒序）
    pub fn session_list(&self, now: i64) -> Result<Vec<SessionRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT jti, user_id, username, ua, ip, iat, exp
             FROM sessions WHERE exp > ?1 ORDER BY iat DESC",
        )?;
        let rows = stmt
            .query_map([now], |row| {
                Ok(SessionRow {
                    jti: row.get(0)?,
                    user_id: row.get(1)?,
                    username: row.get(2)?,
                    ua: row.get(3)?,
                    ip: row.get(4)?,
                    iat: row.get(5)?,
                    exp: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 清理过期会话
    pub fn session_prune(&self, now: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM sessions WHERE exp <= ?1", [now])?;
        Ok(n as u64)
    }

    /// 写入一条监控采样
    pub fn insert_metric(&self, p: &crate::monitor::Snapshot) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out, disk_read, disk_write)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                p.ts,
                p.cpu,
                p.mem_used,
                p.net_in_per_sec,
                p.net_out_per_sec,
                p.disk_read_per_sec,
                p.disk_write_per_sec,
            ),
        )?;
        Ok(())
    }

    /// 读取最近 limit 条监控采样（按时间升序）
    pub fn recent_metrics(&self, limit: i64) -> Result<Vec<MetricPoint>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT ts, cpu, mem_used, net_in, net_out, disk_read, disk_write FROM metrics
             ORDER BY ts DESC LIMIT ?1",
        )?;
        let mut rows = stmt
            .query_map([limit], |row| {
                Ok(MetricPoint {
                    ts: row.get(0)?,
                    cpu: row.get(1)?,
                    mem_used: row.get(2)?,
                    net_in: row.get(3)?,
                    net_out: row.get(4)?,
                    disk_read: row.get(5)?,
                    disk_write: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// 1.2 保留策略：把早于 raw_before 的原始采样按小时聚合进
    /// metrics_hourly（avg/max），删除这些原始行，并清理早于 hourly_before
    /// 的聚合行。三条语句在一个事务里执行，避免「删了原始、聚合失败」丢数据。
    /// 小时对齐按 UTC 整点（ts % 3600）：本地时区偏移为整小时时，
    /// UTC 整点即本地整点，前端展示的小时边界一致。
    pub fn rollup_and_prune(&self, raw_before: i64, hourly_before: i64) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute_batch("BEGIN")?;
        let r = (|| -> Result<()> {
            // 磁盘两列用 COALESCE 兜住：迁移前的小时桶这两列是 NULL，
            // 而 INSERT OR REPLACE 会把整行换成新值 —— 若新聚合里磁盘是 NULL，
            // 就会把已经回填好的数字抹回 NULL。COALESCE(新, 旧) 保证只前进不后退。
            // MAX() 的 NULL 语义正好合用：SQLite 的 MAX 忽略 NULL、
            // 全为 NULL 时才返回 NULL，故「聚合里无磁盘数据」不会污染已有值。
            conn.execute(
                "INSERT OR REPLACE INTO metrics_hourly
                     (hour_ts, cpu_avg, cpu_max, mem_used_avg, mem_used_max,
                      net_in_avg, net_in_max, net_out_avg, net_out_max,
                      disk_read_avg, disk_read_max, disk_write_avg, disk_write_max)
                 SELECT ts - (ts % 3600),
                        AVG(cpu), MAX(cpu),
                        AVG(mem_used), MAX(mem_used),
                        AVG(net_in), MAX(net_in),
                        AVG(net_out), MAX(net_out),
                        COALESCE(CAST(AVG(disk_read) AS INTEGER),
                                 (SELECT m.disk_read_avg FROM metrics_hourly m
                                   WHERE m.hour_ts = metrics.ts - (metrics.ts % 3600))),
                        COALESCE(MAX(disk_read),
                                 (SELECT m.disk_read_max FROM metrics_hourly m
                                   WHERE m.hour_ts = metrics.ts - (metrics.ts % 3600))),
                        COALESCE(CAST(AVG(disk_write) AS INTEGER),
                                 (SELECT m.disk_write_avg FROM metrics_hourly m
                                   WHERE m.hour_ts = metrics.ts - (metrics.ts % 3600))),
                        COALESCE(MAX(disk_write),
                                 (SELECT m.disk_write_max FROM metrics_hourly m
                                   WHERE m.hour_ts = metrics.ts - (metrics.ts % 3600)))
                 FROM metrics WHERE ts < ?1 GROUP BY 1",
                [raw_before],
            )?;
            conn.execute("DELETE FROM metrics WHERE ts < ?1", [raw_before])?;
            conn.execute(
                "DELETE FROM metrics_hourly WHERE hour_ts < ?1",
                [hourly_before],
            )?;
            Ok(())
        })();
        match r {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e).context("聚合监控历史失败");
            }
        }
        Ok(())
    }

    /// 一次性回填：把**仍然留在原始表里**的采样按小时重新聚合出磁盘 I/O，
    /// 补进 metrics_hourly 的 disk_* 四列。
    ///
    /// 为什么需要：0014 只加了列，此前的小时桶磁盘列是 NULL，而原始数据只保留
    /// 7 天 —— 那段仍在本地的历史若不在被 prune 之前补一次，就会永久空着。
    /// 只能补「现在还在原始表里」的部分，更早的已经在历史滚动的过程中删掉了，
    /// 物理上无法重建（前端对 NULL 断线，正好表达这一点）。
    ///
    /// 幂等：可重复执行；每次启动调一次，代价是一条按小时聚合的 SQL。
    /// 返回被更新的行数，便于日志与测试断言。
    pub fn backfill_disk_hourly(&self) -> Result<usize> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        // 用 UPDATE 而不是 INSERT OR REPLACE：回填只该补磁盘四列，不得顺手把
        // cpu / mem / net 按「磁盘有效行的子集」重算一遍 —— 那是在改历史数据，
        // 中间那些磁盘为 NULL 的旧样本会被丢掉。只动该动的那四列。
        // 子查询限定在「原始表里仍有磁盘数据的小时」，避免白扫一遍全表。
        let n = conn.execute(
            "WITH disk_hours AS (
                 SELECT ts - (ts % 3600) AS hour_ts,
                        CAST(AVG(disk_read)  AS INTEGER) AS read_avg,
                        MAX(disk_read)  AS read_max,
                        CAST(AVG(disk_write) AS INTEGER) AS write_avg,
                        MAX(disk_write) AS write_max
                 FROM metrics
                 WHERE disk_read IS NOT NULL
                 GROUP BY 1
             )
             UPDATE metrics_hourly
                SET disk_read_avg  = (SELECT d.read_avg  FROM disk_hours d WHERE d.hour_ts = metrics_hourly.hour_ts),
                    disk_read_max  = (SELECT d.read_max  FROM disk_hours d WHERE d.hour_ts = metrics_hourly.hour_ts),
                    disk_write_avg = (SELECT d.write_avg FROM disk_hours d WHERE d.hour_ts = metrics_hourly.hour_ts),
                    disk_write_max = (SELECT d.write_max FROM disk_hours d WHERE d.hour_ts = metrics_hourly.hour_ts)
              WHERE hour_ts IN (SELECT hour_ts FROM disk_hours)",
            [],
        )?;
        Ok(n)
    }

    /// 历史查询（1.2：自动按时间跨度选表）。
    /// 起点落在原始保留窗口内 → 查 5 秒原始表；更早 → 查小时聚合表。
    ///
    /// 返回按时间升序、**均匀覆盖整个 [from, to] 窗口**、最多 limit 个点。
    ///
    /// 刻意不用 `ORDER BY ts DESC LIMIT`：那样取到的是窗口**尾部**的 limit 条，
    /// limit 决定的是覆盖范围而不是分辨率。10 分钟窗口按 2 秒一条是 300 个点，
    /// 前端要 120 个 → 只回最后 4 分钟，而 x 轴仍按 10 分钟铺开，曲线只占右边
    /// 四成，看上去就是「趋势图没有数据 / 没有变化」。改成按固定时长分桶降采样：
    /// 每桶取平均、时间取桶内最早一条，于是 limit 只管精度，窗口宽度永远画满。
    pub fn history(
        &self,
        from: i64,
        to: i64,
        raw_from: i64,
        limit: i64,
    ) -> Result<Vec<MetricPoint>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        // 分桶时长 = 窗口长 / limit（至少 1 秒）；窗口短于 limit 秒时退化为逐点。
        let bucket = ((to - from).max(1) / limit.max(1)).max(1);
        let sql = if from >= raw_from {
            // 平均是 REAL，整数字段要 CAST 回 INTEGER —— rusqlite 不做隐式转换，
            // 直接把 REAL 读成 i64 会报 InvalidColumnType。
            // 磁盘两列同样要 CAST（AVG 返回 REAL），且保持 NULL 可空：老行为 NULL，
            // 序列化成 null 让前端断线，而不是画成 0。
            "SELECT MIN(ts), AVG(cpu), CAST(AVG(mem_used) AS INTEGER),
                    CAST(AVG(net_in) AS INTEGER), CAST(AVG(net_out) AS INTEGER),
                    CAST(AVG(disk_read) AS INTEGER), CAST(AVG(disk_write) AS INTEGER)
             FROM metrics
             WHERE ts >= ?1 AND ts <= ?2
             GROUP BY (ts - ?1) / ?3 ORDER BY 1"
        } else {
            "SELECT MIN(hour_ts), AVG(cpu_avg), CAST(AVG(mem_used_avg) AS INTEGER),
                    CAST(AVG(net_in_avg) AS INTEGER), CAST(AVG(net_out_avg) AS INTEGER),
                    CAST(AVG(disk_read_avg) AS INTEGER), CAST(AVG(disk_write_avg) AS INTEGER)
             FROM metrics_hourly
             WHERE hour_ts >= ?1 AND hour_ts <= ?2
             GROUP BY (hour_ts - ?1) / ?3 ORDER BY 1"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params![from, to, bucket], |row| {
                Ok(MetricPoint {
                    ts: row.get(0)?,
                    cpu: row.get(1)?,
                    mem_used: row.get(2)?,
                    net_in: row.get(3)?,
                    net_out: row.get(4)?,
                    disk_read: row.get(5)?,
                    disk_write: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---------- AI 助手（4.5 悬浮球，按用户持久化） ----------

    /// 写入一条助手消息，返回 id。`parts` 为有序轨迹 JSON（可为空串）
    pub fn ai_msg_add(
        &self,
        user_id: i64,
        role: &str,
        content: &str,
        parts: &str,
        ts: i64,
    ) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO ai_messages (user_id, role, content, parts, ts) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (user_id, role, content, parts, ts),
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 用户最近 limit 条助手消息（按 id 升序，取窗口尾部）
    pub fn ai_msg_list(&self, user_id: i64, limit: i64) -> Result<Vec<AiMsgRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT id, role, content, parts, ts FROM ai_messages \
             WHERE user_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let mut rows = stmt
            .query_map(rusqlite::params![user_id, limit], |row| {
                Ok(AiMsgRow {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    parts: row.get(3)?,
                    ts: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// 清空用户的助手对话历史
    pub fn ai_msg_clear(&self, user_id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM ai_messages WHERE user_id = ?1", [user_id])?;
        Ok(n as u64)
    }

    // ---------- P2-1 作业表 ----------

    /// 登记作业（初始 pending）。id 由调用方生成的 32 位十六进制串保证唯一
    pub fn job_insert(&self, id: &str, kind: &str, payload: &str, created_at: i64) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO jobs (id, kind, payload, status, stdout_tail, created_at) \
             VALUES (?1, ?2, ?3, 'pending', '', ?4)",
            (id, kind, payload, created_at),
        )?;
        Ok(())
    }

    /// pending → running
    pub fn job_mark_running(&self, id: &str, started_at: i64) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "UPDATE jobs SET status = 'running', started_at = ?2 WHERE id = ?1",
            (id, started_at),
        )?;
        Ok(())
    }

    /// 覆盖输出尾部。执行体按批写（默认 ~300ms 一次），避免逐行落库
    pub fn job_set_tail(&self, id: &str, tail: &str) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute("UPDATE jobs SET stdout_tail = ?2 WHERE id = ?1", (id, tail))?;
        Ok(())
    }

    /// 写入终态：状态、退出码、输出尾部、错误文案、结束时间。
    ///
    /// 条件限制在 pending/running：执行体是被 abort 之后才轮到写终态的，
    /// 若不加这道闸，「已取消」会被随后赶到的「失败」覆盖，用户看到的状态
    /// 就与他刚才点的取消按钮对不上。
    pub fn job_finish(
        &self,
        id: &str,
        status: &str,
        exit_code: Option<i64>,
        tail: &str,
        error: Option<&str>,
        finished_at: i64,
    ) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "UPDATE jobs SET status = ?2, exit_code = ?3, stdout_tail = ?4, \
             error = ?5, finished_at = ?6 \
             WHERE id = ?1 AND status IN ('pending', 'running')",
            (id, status, exit_code, tail, error, finished_at),
        )?;
        Ok(())
    }

    /// 仅改状态（取消：running → cancelled），返回受影响行数。
    /// 返回 0 表示该作业已是终态或不存在，调用方据此判断「是否真的取消了」。
    pub fn job_set_status(&self, id: &str, status: &str, finished_at: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "UPDATE jobs SET status = ?2, finished_at = ?3 \
             WHERE id = ?1 AND status IN ('pending', 'running')",
            (id, status, finished_at),
        )?;
        Ok(n as u64)
    }

    /// 单条作业详情
    pub fn job_get(&self, id: &str) -> Result<Option<JobRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, payload, status, exit_code, stdout_tail, error, \
             created_at, started_at, finished_at FROM jobs WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], job_from_row)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// 作业列表（创建时间倒序分页）
    pub fn job_list(&self, limit: i64, offset: i64) -> Result<Vec<JobRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, payload, status, exit_code, stdout_tail, error, \
             created_at, started_at, finished_at FROM jobs \
             ORDER BY created_at DESC, id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit, offset], job_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 重启收尾：遗留的 pending / running 一律记为 interrupted，返回受影响行数。
    ///
    /// 刻意**不自动续跑**：安装类操作被中断后系统可能停在半完成状态
    /// （包已下载未配置、dpkg 事务未提交），自动重试比重做更危险。
    pub fn job_mark_interrupted(&self, finished_at: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "UPDATE jobs SET status = 'interrupted', finished_at = ?1 \
             WHERE status IN ('pending', 'running')",
            [finished_at],
        )?;
        Ok(n as u64)
    }

    /// 清理过期作业（仅终态行，按结束时间）
    pub fn job_prune(&self, before: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "DELETE FROM jobs WHERE finished_at IS NOT NULL AND finished_at < ?1",
            [before],
        )?;
        Ok(n as u64)
    }
}

/// 把一个同步的数据库操作挪到阻塞线程池执行。
///
/// rusqlite 是纯同步 API，且 SQLite 写入会 fsync 落盘 —— 直接在 async 上下文里
/// 调用会占住 tokio 的工作线程。web 请求路径上的调用一律走这里。
///
/// 之所以不像 `files.rs` 那样在各处手写 `spawn_blocking`：数据库方法数量多、
/// 参数形态不一，集中一处包装能让调用点保持 `db.xxx_async().await` 的简单形式，
/// 也不会把 `MutexGuard` / `Statement` 这类非 `Send` 的东西跨过 await。
async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .context("数据库任务调度失败")?
}

/// 数据库的异步调用面：每个方法内部把对应的同步实现丢进阻塞线程池。
///
/// 保留同步版本是因为启动阶段（`Db::open` / `ensure_admin`）本就运行在
/// runtime 之外，那里不需要异步包装。
impl Db {
    // 主题定制（/api/theme）使用
    pub async fn get_setting_async(&self, key: &str) -> Result<Option<String>> {
        let db = self.clone();
        let key = key.to_string();
        blocking(move || db.get_setting(&key)).await
    }

    pub async fn set_setting_async(&self, key: &str, value: &str) -> Result<()> {
        let db = self.clone();
        let key = key.to_string();
        let value = value.to_string();
        blocking(move || db.set_setting(&key, &value)).await
    }

    pub async fn find_user_async(&self, username: &str) -> Result<Option<(i64, String, String)>> {
        let db = self.clone();
        let username = username.to_string();
        blocking(move || db.find_user(&username)).await
    }

    pub async fn find_user_full_async(&self, username: &str) -> Result<Option<UserRow>> {
        let db = self.clone();
        let username = username.to_string();
        blocking(move || db.find_user_full(&username)).await
    }

    pub async fn create_user_role_async(
        &self,
        username: &str,
        hash: &str,
        salt: &str,
        role: &str,
        must_change: bool,
    ) -> Result<i64> {
        let db = self.clone();
        let (u, h, s, r) = (
            username.to_string(),
            hash.to_string(),
            salt.to_string(),
            role.to_string(),
        );
        blocking(move || db.create_user_role(&u, &h, &s, &r, must_change)).await
    }

    pub async fn user_by_id_async(&self, id: i64) -> Result<Option<(String, String, bool)>> {
        let db = self.clone();
        blocking(move || db.user_by_id(id)).await
    }

    pub async fn list_users_async(&self) -> Result<Vec<(i64, String, String, bool)>> {
        let db = self.clone();
        blocking(move || db.list_users()).await
    }

    pub async fn set_password_async(&self, id: i64, hash: &str, salt: &str) -> Result<()> {
        let db = self.clone();
        let (h, s) = (hash.to_string(), salt.to_string());
        blocking(move || db.set_password(id, &h, &s)).await
    }

    pub async fn update_user_async(&self, id: i64, username: &str, role: &str) -> Result<()> {
        let db = self.clone();
        let (u, r) = (username.to_string(), role.to_string());
        blocking(move || db.update_user(id, &u, &r)).await
    }

    pub async fn delete_user_async(&self, id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.delete_user(id)).await
    }

    pub async fn user_exists_async(&self, id: i64) -> Result<bool> {
        let db = self.clone();
        blocking(move || db.user_exists(id)).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn audit_async(
        &self,
        ts: i64,
        user_id: Option<i64>,
        username: &str,
        method: &str,
        path: &str,
        status: u16,
        ip: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let db = self.clone();
        let (u, m, p, i, d) = (
            username.to_string(),
            method.to_string(),
            path.to_string(),
            ip.to_string(),
            detail.map(str::to_string),
        );
        blocking(move || db.audit(ts, user_id, &u, &m, &p, status, &i, d.as_deref())).await
    }

    pub async fn audit_list_async(&self, limit: i64, offset: i64) -> Result<Vec<AuditRow>> {
        let db = self.clone();
        blocking(move || db.audit_list(limit, offset)).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn session_add_async(
        &self,
        jti: &str,
        user_id: i64,
        username: &str,
        ua: &str,
        ip: &str,
        iat: i64,
        exp: i64,
    ) -> Result<()> {
        let db = self.clone();
        let (j, u, a, i) = (
            jti.to_string(),
            username.to_string(),
            ua.to_string(),
            ip.to_string(),
        );
        blocking(move || db.session_add(&j, user_id, &u, &a, &i, iat, exp)).await
    }

    pub async fn session_remove_async(&self, jti: &str) -> Result<u64> {
        let db = self.clone();
        let j = jti.to_string();
        blocking(move || db.session_remove(&j)).await
    }

    pub async fn session_exists_async(&self, jti: &str) -> Result<bool> {
        let db = self.clone();
        let j = jti.to_string();
        blocking(move || db.session_exists(&j)).await
    }

    pub async fn session_remove_user_async(&self, user_id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.session_remove_user(user_id)).await
    }

    pub async fn session_remove_user_except_async(
        &self,
        user_id: i64,
        keep_jti: &str,
    ) -> Result<u64> {
        let db = self.clone();
        let j = keep_jti.to_string();
        blocking(move || db.session_remove_user_except(user_id, &j)).await
    }

    pub async fn session_list_async(&self, now: i64) -> Result<Vec<SessionRow>> {
        let db = self.clone();
        blocking(move || db.session_list(now)).await
    }

    pub async fn session_prune_async(&self, now: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.session_prune(now)).await
    }

    pub async fn audit_prune_async(&self, before: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.audit_prune(before)).await
    }

    pub async fn alert_event_add_async(
        &self,
        ts: i64,
        metric: String,
        value: f64,
        threshold: f64,
        state: String,
    ) -> Result<()> {
        let db = self.clone();
        blocking(move || db.alert_event_add(ts, &metric, value, threshold, &state)).await
    }

    pub async fn alert_event_list_async(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AlertEventRow>> {
        let db = self.clone();
        blocking(move || db.alert_event_list(limit, offset)).await
    }

    pub async fn alert_event_prune_async(&self, before: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.alert_event_prune(before)).await
    }

    // ---------- P2-3 认证状态（异步包装，供请求路径与清理循环调用）----------

    pub async fn auth_state_get_async(&self, key: &str, now: i64) -> Result<Option<String>> {
        let db = self.clone();
        let key = key.to_string();
        blocking(move || db.auth_state_get(&key, now)).await
    }

    pub async fn auth_state_set_async(&self, key: &str, value: &str, expire_at: i64) -> Result<()> {
        let db = self.clone();
        let key = key.to_string();
        let value = value.to_string();
        blocking(move || db.auth_state_set(&key, &value, expire_at)).await
    }

    pub async fn auth_state_remove_async(&self, key: &str) -> Result<u64> {
        let db = self.clone();
        let key = key.to_string();
        blocking(move || db.auth_state_remove(&key)).await
    }

    pub async fn auth_state_remove_prefix_async(&self, prefix: &str) -> Result<u64> {
        let db = self.clone();
        let prefix = prefix.to_string();
        blocking(move || db.auth_state_remove_prefix(&prefix)).await
    }

    pub async fn auth_state_prune_async(&self, now: i64, throttle_cap: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.auth_state_prune(now, throttle_cap)).await
    }

    // ---------- P2-1 作业表（异步包装） ----------

    pub async fn job_insert_async(
        &self,
        id: &str,
        kind: &str,
        payload: &str,
        created_at: i64,
    ) -> Result<()> {
        let db = self.clone();
        let id = id.to_string();
        let kind = kind.to_string();
        let payload = payload.to_string();
        blocking(move || db.job_insert(&id, &kind, &payload, created_at)).await
    }

    pub async fn job_mark_running_async(&self, id: &str, started_at: i64) -> Result<()> {
        let db = self.clone();
        let id = id.to_string();
        blocking(move || db.job_mark_running(&id, started_at)).await
    }

    pub async fn job_set_tail_async(&self, id: &str, tail: &str) -> Result<()> {
        let db = self.clone();
        let id = id.to_string();
        let tail = tail.to_string();
        blocking(move || db.job_set_tail(&id, &tail)).await
    }

    pub async fn job_finish_async(
        &self,
        id: &str,
        status: &str,
        exit_code: Option<i64>,
        tail: &str,
        error: Option<&str>,
        finished_at: i64,
    ) -> Result<()> {
        let db = self.clone();
        let id = id.to_string();
        let status = status.to_string();
        let tail = tail.to_string();
        let error = error.map(|s| s.to_string());
        blocking(move || {
            db.job_finish(
                &id,
                &status,
                exit_code,
                &tail,
                error.as_deref(),
                finished_at,
            )
        })
        .await
    }

    pub async fn job_set_status_async(
        &self,
        id: &str,
        status: &str,
        finished_at: i64,
    ) -> Result<u64> {
        let db = self.clone();
        let id = id.to_string();
        let status = status.to_string();
        blocking(move || db.job_set_status(&id, &status, finished_at)).await
    }

    pub async fn job_get_async(&self, id: &str) -> Result<Option<JobRow>> {
        let db = self.clone();
        let id = id.to_string();
        blocking(move || db.job_get(&id)).await
    }

    pub async fn job_list_async(&self, limit: i64, offset: i64) -> Result<Vec<JobRow>> {
        let db = self.clone();
        blocking(move || db.job_list(limit, offset)).await
    }

    pub async fn job_mark_interrupted_async(&self, finished_at: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.job_mark_interrupted(finished_at)).await
    }

    pub async fn job_prune_async(&self, before: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.job_prune(before)).await
    }

    pub async fn insert_metric_async(&self, p: &crate::monitor::Snapshot) -> Result<()> {
        let db = self.clone();
        let p = p.clone();
        blocking(move || db.insert_metric(&p)).await
    }

    pub async fn recent_metrics_async(&self, limit: i64) -> Result<Vec<MetricPoint>> {
        let db = self.clone();
        blocking(move || db.recent_metrics(limit)).await
    }

    pub async fn rollup_and_prune_async(&self, raw_before: i64, hourly_before: i64) -> Result<()> {
        let db = self.clone();
        blocking(move || db.rollup_and_prune(raw_before, hourly_before)).await
    }

    pub async fn backfill_disk_hourly_async(&self) -> Result<usize> {
        let db = self.clone();
        blocking(move || db.backfill_disk_hourly()).await
    }

    pub async fn history_async(
        &self,
        from: i64,
        to: i64,
        raw_from: i64,
        limit: i64,
    ) -> Result<Vec<MetricPoint>> {
        let db = self.clone();
        blocking(move || db.history(from, to, raw_from, limit)).await
    }

    // ---------- AI 助手（4.5）异步包装 ----------

    pub async fn ai_msg_add_async(
        &self,
        user_id: i64,
        role: String,
        content: String,
        parts: String,
        ts: i64,
    ) -> Result<i64> {
        let db = self.clone();
        blocking(move || db.ai_msg_add(user_id, &role, &content, &parts, ts)).await
    }

    pub async fn ai_msg_list_async(&self, user_id: i64, limit: i64) -> Result<Vec<AiMsgRow>> {
        let db = self.clone();
        blocking(move || db.ai_msg_list(user_id, limit)).await
    }

    pub async fn ai_msg_clear_async(&self, user_id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_msg_clear(user_id)).await
    }
}

/// rusqlite 没有 re-export this trait，这里引入供 `.optional()` 使用
use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(tag: &str) -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "lyys_mig_test_{tag}_{}_{unique}",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned()
    }

    fn table_exists(db: &Db, name: &str) -> bool {
        let conn = db.pool.get().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    /// 迁移链的最新版本号。断言引用它而不是写字面量，
    /// 否则每加一个迁移都要回头改一批测试。
    fn latest_version() -> i64 {
        Db::MIGRATIONS.last().unwrap().0
    }

    /// 全新库：迁移建出全部表，版本号为最新；再次打开幂等（不重复执行）
    #[test]
    fn fresh_db_gets_baseline() {
        let path = temp_db_path("fresh");
        {
            let db = Db::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), latest_version());
            assert!(table_exists(&db, "settings"));
            assert!(table_exists(&db, "users"));
            assert!(table_exists(&db, "metrics"));
            assert!(table_exists(&db, "metrics_hourly"));
            assert!(table_exists(&db, "audit_log"));
            assert!(table_exists(&db, "sessions"));
            assert!(table_exists(&db, "alert_events"));
            assert!(table_exists(&db, "ai_messages"));
            assert!(table_exists(&db, "auth_state"), "P2-3 认证状态表");
            assert!(!table_exists(&db, "ai_rooms"), "旧会话表应被 0009 删除");
            assert!(!table_exists(&db, "ai_members"), "旧成员表应被 0009 删除");
        }
        let db2 = Db::open(&path).unwrap();
        assert_eq!(db2.schema_version().unwrap(), latest_version());
        let _ = std::fs::remove_file(path);
    }

    /// 旧部署升级路径：表已存在但无 schema_version，数据必须原样保留
    #[test]
    fn legacy_db_upgrades_without_data_loss() {
        let path = temp_db_path("legacy");
        {
            // 模拟 1697f2c 时代的库：手工建表 + 写入数据，没有 schema_version
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE users (
                     id INTEGER PRIMARY KEY,
                     username TEXT NOT NULL UNIQUE,
                     password_hash TEXT NOT NULL,
                     salt TEXT NOT NULL);
                 INSERT INTO users (username, password_hash, salt) VALUES ('admin','h','s');",
            )
            .unwrap();
        }
        {
            let db = Db::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), latest_version());
            assert_eq!(db.user_count().unwrap(), 1);
            assert!(db.find_user("admin").unwrap().is_some());
            // 旧库升级后 admin 自动获得默认角色 admin、不强制改密（避免锁死现有部署）
            let u = db.find_user_full("admin").unwrap().unwrap();
            assert_eq!(u.role, "admin");
            assert!(!u.must_change);
        }
        // 二次打开幂等
        let db2 = Db::open(&path).unwrap();
        assert_eq!(db2.user_count().unwrap(), 1);
        let _ = std::fs::remove_file(path);
    }

    /// 迁移 0009：删除旧会话体系全部表，重建按用户持久化的 ai_messages，
    /// 且新旧消息表读写（ai_msg_add / ai_msg_list / ai_msg_clear）可用。
    #[test]
    fn migration_0009_replaces_room_tables_with_assistant() {
        let path = temp_db_path("mig0009");
        {
            // 模拟 v8 库：旧会话体系表存在
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE ai_rooms (
                     id INTEGER PRIMARY KEY, name TEXT, creator_id INTEGER,
                     visibility TEXT, created INTEGER);
                 CREATE TABLE ai_members (id INTEGER PRIMARY KEY, room_id INTEGER, name TEXT);
                 CREATE TABLE ai_room_users (room_id INTEGER, user_id INTEGER);
                 CREATE TABLE ai_messages (
                     id INTEGER PRIMARY KEY, room_id INTEGER, sender_type TEXT,
                     sender_id INTEGER, sender_name TEXT, content TEXT, ts INTEGER,
                     reasoning TEXT);
                 INSERT INTO ai_messages (room_id, sender_type, sender_id, sender_name, content, ts, reasoning)
                     VALUES (1,'user',1,'u','旧消息',0,'');
                 -- v8 库本来就该有 audit_log（0004 建出）。这里补一张最小同名的表：
                 -- 否则后续任何引用该表的迁移都会在这个「最小复现」库上直接炸掉，
                 -- 报出的却是与本次迁移无关的错误。
                 CREATE TABLE audit_log (id INTEGER PRIMARY KEY, ts INTEGER);
                 -- 同理，metrics / metrics_hourly（0001、0002 建出）也要在。
                 -- 0014 对它们做 ALTER TABLE，缺表时会报「no such table: metrics」，
                 -- 与 0009 要验证的事情毫无关系。
                 CREATE TABLE metrics (
                     ts INTEGER NOT NULL, cpu REAL NOT NULL, mem_used INTEGER NOT NULL,
                     net_in INTEGER NOT NULL, net_out INTEGER NOT NULL);
                 CREATE TABLE metrics_hourly (
                     hour_ts INTEGER PRIMARY KEY,
                     cpu_avg REAL NOT NULL, cpu_max REAL NOT NULL,
                     mem_used_avg INTEGER NOT NULL, mem_used_max INTEGER NOT NULL,
                     net_in_avg INTEGER NOT NULL, net_in_max INTEGER NOT NULL,
                     net_out_avg INTEGER NOT NULL, net_out_max INTEGER NOT NULL);",
            )
            .unwrap();
            // 标记已应用 1~8，让 Db::open 只跑 0009
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
                 INSERT INTO schema_version VALUES (8);",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), latest_version());
        assert!(!table_exists(&db, "ai_rooms"), "旧会话表应被删除");
        assert!(!table_exists(&db, "ai_members"), "旧成员表应被删除");
        assert!(!table_exists(&db, "ai_room_users"), "受邀表应被删除");
        // 新表结构：按用户写入/读取/清空（含 0013 加的 parts 轨迹列）
        db.ai_msg_add(7, "user", "你好", "", 1).unwrap();
        db.ai_msg_add(
            7,
            "assistant",
            "你好！",
            r#"[{"kind":"content","text":"你好！"}]"#,
            2,
        )
        .unwrap();
        db.ai_msg_add(8, "user", "别的用户", "", 3).unwrap();
        let msgs = db.ai_msg_list(7, 64).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].content, "你好！");
        assert_eq!(msgs[0].parts, "", "用户消息没有轨迹");
        assert!(
            msgs[1].parts.contains(r#""kind":"content""#),
            "助手消息应带回轨迹：{}",
            msgs[1].parts
        );
        assert_eq!(db.ai_msg_clear(7).unwrap(), 2);
        assert!(db.ai_msg_list(7, 64).unwrap().is_empty());
        assert_eq!(db.ai_msg_list(8, 64).unwrap().len(), 1, "其他用户不受影响");
        let _ = std::fs::remove_file(path);
    }

    /// 结构自愈：版本号已是 14、但列实际缺失时，开库要自动补上。
    /// 复现的是 3800 测试库那次「版本记了 14、DDL 却不在」的不一致 ——
    /// 没有这道自愈，采样会每 2 秒失败一次，只能靠人工 ALTER 救场。
    #[test]
    fn missing_columns_are_repaired_on_open() {
        let path = temp_db_path("repair");
        {
            let db = Db::open(&path).unwrap();
            let conn = db.pool.get().unwrap();
            // 退回到「列不存在」的状态：重建表（丢掉磁盘两列）、保留数据，
            // 且**不动 schema_version**（它仍是 14）——正是出问题的那个组合
            conn.execute_batch(
                "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out) VALUES (5, 1, 2, 3, 4);
                 CREATE TABLE metrics_old AS
                     SELECT ts, cpu, mem_used, net_in, net_out FROM metrics;
                 DROP TABLE metrics;
                 CREATE TABLE metrics (
                     ts INTEGER NOT NULL, cpu REAL NOT NULL, mem_used INTEGER NOT NULL,
                     net_in INTEGER NOT NULL, net_out INTEGER NOT NULL);
                 INSERT INTO metrics SELECT * FROM metrics_old;
                 DROP TABLE metrics_old;",
            )
            .unwrap();
            let cols: Vec<String> = conn
                .prepare("SELECT name FROM pragma_table_info('metrics')")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                !cols.iter().any(|c| c == "disk_read"),
                "前置条件：此时不该有 disk_read"
            );
        }

        // 再开一次：迁移会因版本已是 14 全部跳过，只能靠 repair_columns 补列
        {
            let db = Db::open(&path).unwrap();
            let conn = db.pool.get().unwrap();
            let has: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('metrics') WHERE name='disk_read'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has, 1, "缺失的列应被自动补齐");
            // 原有数据必须还在
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM metrics WHERE ts = 5", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1, "补列不得丢数据");
            // 新列可写
            conn.execute(
                "UPDATE metrics SET disk_read = 42 WHERE ts = 5",
                [],
            )
            .unwrap();
        }
        let _ = std::fs::remove_file(path);
    }

    /// 迁移失败必须整体回滚：事务内半途失败时，已建表不得残留、版本不得推进
    #[test]
    fn failed_migration_rolls_back() {
        let path = temp_db_path("rollback");
        {
            let db = Db::open(&path).unwrap();
            let mut conn = db.pool.get().unwrap();
            // 故意模拟 0002 迁移半途失败：建表后接一条必然报错的语句，同事务
            let tx = conn.transaction().unwrap();
            tx.execute_batch(
                "CREATE TABLE t_half (x INTEGER); INSERT INTO no_such_table VALUES (1);",
            )
            .expect_err("半途失败应中止");
            drop(tx);
            assert!(!table_exists(&db, "t_half"), "回滚后不应残留半途建的表");
            assert_eq!(
                db.schema_version().unwrap(),
                latest_version(),
                "失败的迁移不得推进版本"
            );
        }
        let _ = std::fs::remove_file(path);
    }

    /// 1.2 保留策略：超期原始数据按小时聚合（avg/max）后删除，聚合可重放幂等
    #[test]
    fn rollup_aggregates_and_prunes() {
        let path = temp_db_path("rollup");
        {
            let db = Db::open(&path).unwrap();
            let conn = db.pool.get().unwrap();
            // 第 0 小时：两条 (cpu 10/20 → avg15 max20)；第 1 小时：一条 cpu 30
            conn.execute_batch(
                "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out, disk_read, disk_write) VALUES
                 (0,    10, 100, 1, 2, 100, 200),
                 (1800, 20, 200, 3, 4, 300, 400),
                 (3600, 30, 300, 5, 6, NULL, NULL);",
            )
            .unwrap();
            // raw_before=3600：仅前两行聚合；hourly_before=0：聚合行不被清
            db.rollup_and_prune(3600, 0).unwrap();
            // 原始表只剩第 3 行（ts=3600 未超期）
            let raw: i64 = conn
                .query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0))
                .unwrap();
            assert_eq!(raw, 1, "超期原始行应被删除");
            // 聚合表：hour 0 一行，avg=15 max=20
            let (n, avg, max): (i64, f64, f64) = conn
                .query_row(
                    "SELECT COUNT(*), cpu_avg, cpu_max FROM metrics_hourly WHERE hour_ts=0",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(n, 1);
            assert_eq!(avg, 15.0);
            assert_eq!(max, 20.0);
            // 磁盘两列同口径：AVG(100,300)=200、MAX(100,300)=300
            let (dra, drm, dwa, dwm): (i64, i64, i64, i64) = conn
                .query_row(
                    "SELECT disk_read_avg, disk_read_max, disk_write_avg, disk_write_max
                     FROM metrics_hourly WHERE hour_ts=0",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!((dra, drm, dwa, dwm), (200, 300, 300, 400));
            // 幂等：再跑一次同样参数不新增/不报错（INSERT OR REPLACE）
            db.rollup_and_prune(3600, 0).unwrap();
            let n2: i64 = conn
                .query_row("SELECT COUNT(*) FROM metrics_hourly", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n2, 1, "重放聚合不应产生重复小时行");
            // 重放时磁盘聚合里的 NULL 不得把已有数字抹掉（COALESCE 兜底）
            let dra2: i64 = conn
                .query_row(
                    "SELECT disk_read_avg FROM metrics_hourly WHERE hour_ts=0",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(dra2, 200, "重放聚合不得把已回填的磁盘值抹回 NULL");
        }
        let _ = std::fs::remove_file(path);
    }

    /// 0014 的历史回填：把**仍在原始表里**的采样按小时补出磁盘 I/O 聚合。
    /// 更早的原始行已被保留策略删除，物理上无法重建 —— 那种小时桶保持 NULL。
    #[test]
    fn backfill_disk_hourly_rebuilds_from_live_raw_rows() {
        let path = temp_db_path("backfill_disk");
        {
            let db = Db::open(&path).unwrap();
            let conn = db.pool.get().unwrap();
            // 先造一个「迁移前」的小时桶：磁盘列全 NULL，但 cpu 已有值
            conn.execute_batch(
                "INSERT INTO metrics_hourly
                     (hour_ts, cpu_avg, cpu_max, mem_used_avg, mem_used_max,
                      net_in_avg, net_in_max, net_out_avg, net_out_max)
                 VALUES (0, 7.5, 9, 111, 222, 1, 1, 2, 2);",
            )
            .unwrap();
            // 原始表里还有该小时的两条采样，带磁盘值；另有两条磁盘为 NULL（旧行）
            conn.execute_batch(
                "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out, disk_read, disk_write) VALUES
                 (60,  1, 10, 1, 1, 1000, 2000),
                 (120, 2, 20, 1, 1, 3000, 4000),
                 (600, 3, 30, 1, 1, NULL, NULL);",
            )
            .unwrap();

            db.backfill_disk_hourly().unwrap();

            // 磁盘聚合只统计非 NULL 的采样：读 AVG(1000,3000)=2000、MAX=3000；
            // 写 AVG(2000,4000)=3000、MAX=4000
            let (dra, drm, dwa, dwm): (i64, i64, i64, i64) = conn
                .query_row(
                    "SELECT disk_read_avg, disk_read_max, disk_write_avg, disk_write_max
                     FROM metrics_hourly WHERE hour_ts=0",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!((dra, drm, dwa, dwm), (2000, 3000, 3000, 4000));
            // 回填只动磁盘四列：cpu_avg 必须保持迁移前的 7.5，
            // 不能被「磁盘有效行」的子集重算成 AVG(1,2)=1.5
            let cpu_avg: f64 = conn
                .query_row(
                    "SELECT cpu_avg FROM metrics_hourly WHERE hour_ts=0",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(cpu_avg, 7.5, "回填不得重算非磁盘列");
            // 幂等：再跑一次结果不变
            db.backfill_disk_hourly().unwrap();
            let again: i64 = conn
                .query_row(
                    "SELECT disk_read_avg FROM metrics_hourly WHERE hour_ts=0",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(again, 2000);
        }
        let _ = std::fs::remove_file(path);
    }

    /// 1.2 历史查询选表：窗口起点在保留期内查原始表，更早查小时聚合表
    #[test]
    fn history_switches_table_by_window() {
        let path = temp_db_path("history");
        {
            let db = Db::open(&path).unwrap();
            let conn = db.pool.get().unwrap();
            conn.execute_batch(
                "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out, disk_read, disk_write) VALUES
                 (1000, 1, 1, 1, 1, 500, 600),
                 (2000, 2, 2, 2, 2, NULL, NULL);",
            )
            .unwrap();
            conn.execute_batch(
                "INSERT INTO metrics_hourly (hour_ts, cpu_avg, cpu_max, mem_used_avg, mem_used_max, net_in_avg, net_in_max, net_out_avg, net_out_max, disk_read_avg, disk_read_max, disk_write_avg, disk_write_max)
                 VALUES (0, 5, 5, 5, 5, 5, 5, 5, 5, 700, 900, 1100, 1300);",
            )
            .unwrap();
            // raw_from=100000：起点 0 早于保留期 → 查聚合表，返回 hour_ts=0 那条
            let old = db.history(0, 100000, 100000, 100).unwrap();
            assert_eq!(old.len(), 1);
            assert_eq!(old[0].cpu, 5.0, "应命中小时聚合表的值");
            assert_eq!(
                old[0].disk_read,
                Some(700),
                "小时表要能读出磁盘读速率"
            );
            // 起点 1000 在保留期内（raw_from=500 ≤ 1000）→ 查原始表，返回两条
            let fresh = db.history(1000, 100000, 500, 100).unwrap();
            assert_eq!(fresh.len(), 2);
            assert_eq!(fresh[0].ts, 1000, "结果按时间升序");
            assert_eq!(fresh[0].disk_read, Some(500), "原始表带磁盘值");
            // 该行磁盘列为 NULL → 必须是 None（前端断线），不能退化成 0
            assert_eq!(
                fresh[1].disk_read, None,
                "迁移前的老行没有磁盘数据，应保持 None 而不是 0"
            );
        }
        let _ = std::fs::remove_file(path);
    }

    /// 窗口内的点数多于 limit 时，降采样必须**铺满整个窗口**，而不是只取窗口尾部。
    /// 取尾部正是「趋势图只画了最后几分钟、看着像没数据」的成因。
    #[test]
    fn history_downsampling_still_covers_the_whole_window() {
        let path = temp_db_path("history_span");
        {
            let db = Db::open(&path).unwrap();
            let conn = db.pool.get().unwrap();
            // 1000 起每 2 秒一条、共 300 条，窗口 [1000, 1598]；cpu 依次 0..299
            let mut sql =
                String::from("INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out) VALUES ");
            for i in 0..300 {
                if i > 0 {
                    sql.push(',');
                }
                sql.push_str(&format!("({}, {i}, 0, 0, 0)", 1000 + i * 2));
            }
            conn.execute_batch(&sql).unwrap();

            // 600 秒窗口只要 120 点（每桶 5 秒）→ 宽度必须仍然是 600 秒
            let pts = db.history(1000, 1600, 0, 120).unwrap();
            assert_eq!(pts.len(), 120, "点数应被压到 limit");
            assert_eq!(pts[0].ts, 1000, "首个点要落在窗口起点");
            let last = pts.last().unwrap().ts;
            assert!(last >= 1595, "末个点要落在窗口最后那个桶里，实测 {last}");
            assert!(pts.windows(2).all(|w| w[0].ts < w[1].ts), "时间严格升序");
            // 每桶取平均 → 整段均值应接近中位 149.5，而不是尾部那段的均值
            let avg: f64 = pts.iter().map(|p| p.cpu).sum::<f64>() / pts.len() as f64;
            assert!((100.0..200.0).contains(&avg), "应是整段窗口的均值，实测 {avg}");
        }
        let _ = std::fs::remove_file(path);
    }

    /// 2.1/2.2：带角色的用户 CRUD 与列表（不含密码字段）
    #[test]
    fn user_rbac_crud() {
        let path = temp_db_path("rbac");
        {
            let db = Db::open(&path).unwrap();
            let id = db
                .create_user_role("op1", "h", "s", "operator", true)
                .unwrap();
            db.create_user_role("view1", "h", "s", "viewer", false)
                .unwrap();
            let users = db.list_users().unwrap();
            assert_eq!(users.len(), 2);
            let op = users.iter().find(|u| u.1 == "op1").unwrap();
            assert_eq!(op.0, id);
            assert_eq!(op.2, "operator");
            assert!(op.3, "must_change 应读出为 true");
            // 改名/改角色
            db.update_user(id, "op2", "admin").unwrap();
            let u = db.user_by_id(id).unwrap().unwrap();
            assert_eq!((u.0.as_str(), u.1.as_str(), u.2), ("op2", "admin", true));
            // 改密清除 must_change
            db.set_password(id, "h2", "s2").unwrap();
            let full = db.find_user_full("op2").unwrap().unwrap();
            assert!(!full.must_change);
            assert_eq!(full.password_hash, "h2");
            // 删除
            assert_eq!(db.delete_user(id).unwrap(), 1);
            assert!(!db.user_exists(id).unwrap());
            assert_eq!(db.list_users().unwrap().len(), 1);
        }
        let _ = std::fs::remove_file(path);
    }

    /// 2.4：会话登记/存在性/按用户清除（含保留当前会话）/过期清理
    #[test]
    fn session_lifecycle() {
        let path = temp_db_path("sessions");
        {
            let db = Db::open(&path).unwrap();
            db.session_add("j1", 1, "admin", "ua", "1.2.3.4", 100, 200)
                .unwrap();
            db.session_add("j2", 1, "admin", "ua", "5.6.7.8", 110, 300)
                .unwrap();
            db.session_add("j3", 2, "op", "ua", "9.9.9.9", 120, 400)
                .unwrap();
            assert!(db.session_exists("j1").unwrap());
            // 列出未过期（now=150 全部有效；now=250 时 j1 过期）
            assert_eq!(db.session_list(150).unwrap().len(), 3);
            assert_eq!(db.session_list(250).unwrap().len(), 2);
            // 本人改密：清 user=1 但保留 j2
            assert_eq!(db.session_remove_user_except(1, "j2").unwrap(), 1);
            assert!(!db.session_exists("j1").unwrap());
            assert!(db.session_exists("j2").unwrap());
            // 管理员踢人：user=2 全清
            assert_eq!(db.session_remove_user(2).unwrap(), 1);
            // 过期清理
            assert_eq!(db.session_prune(350).unwrap(), 1);
            assert_eq!(db.session_list(350).unwrap().len(), 0);
        }
        let _ = std::fs::remove_file(path);
    }

    /// 2.3：审计写入与分页读取（时间倒序），prune 按时间清理；顺带覆盖 P1-3 的 detail 列
    #[test]
    fn audit_write_and_list() {
        let path = temp_db_path("audit");
        {
            let db = Db::open(&path).unwrap();
            db.audit(
                100,
                Some(1),
                "admin",
                "POST",
                "/api/files/delete",
                200,
                "1.2.3.4",
                Some("path=/etc/nginx/nginx.conf"),
            )
            .unwrap();
            db.audit(200, None, "-", "POST", "/api/login", 401, "9.9.9.9", None)
                .unwrap();
            let rows = db.audit_list(10, 0).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].ts, 200, "按时间倒序");
            assert_eq!(rows[0].user_id, None);
            assert_eq!(rows[0].detail, None, "未取样的请求 detail 保持为空");
            assert_eq!(rows[1].username, "admin");
            assert_eq!(
                rows[1].detail.as_deref(),
                Some("path=/etc/nginx/nginx.conf"),
                "取样到的摘要必须原样读回"
            );
            // 分页
            let page = db.audit_list(1, 1).unwrap();
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].ts, 100);
            // 清理
            assert_eq!(db.audit_prune(150).unwrap(), 1);
            assert_eq!(db.audit_list(10, 0).unwrap().len(), 1);
        }
        let _ = std::fs::remove_file(path);
    }
}
