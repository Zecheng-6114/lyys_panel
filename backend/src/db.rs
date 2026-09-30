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

/// 监控历史采样点
#[derive(Serialize)]
pub struct MetricPoint {
    pub ts: i64,
    pub cpu: f64,
    pub mem_used: i64,
    pub net_in: i64,
    pub net_out: i64,
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

/// AI 群聊房间行（4.5）
#[derive(Clone, Serialize)]
pub struct AiRoomRow {
    pub id: i64,
    pub name: String,
    pub creator_id: i64,
    pub visibility: String,
    pub created: i64,
}

/// AI 群聊成员行（4.5）。persona 为角色设定（system prompt 素材）；
/// model / api_base 为空串表示回退全局配置。
#[derive(Clone, Serialize)]
pub struct AiMemberRow {
    pub id: i64,
    pub room_id: i64,
    pub name: String,
    pub persona: String,
    pub is_admin: bool,
    pub model: String,
    pub api_base: String,
    pub sort: i64,
}

/// ai_member_get 的返回元组：(room_id, name, persona, is_admin, model, api_base)
pub type AiMemberTuple = (i64, String, String, bool, String, String);

/// AI 群聊消息行（4.5）。sender_type：user / ai / system。
#[derive(Serialize)]
pub struct AiMessageRow {
    pub id: i64,
    pub room_id: i64,
    pub sender_type: String,
    pub sender_id: i64,
    pub sender_name: String,
    pub content: String,
    pub ts: i64,
}

impl Db {
    /// 打开（或创建）SQLite 数据库并执行初始化建表
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).context("创建数据目录失败")?;
            }
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

    /// 创建用户（RBAC：带角色与强制改密标记）
    pub fn create_user(&self, username: &str, hash: &str, salt: &str) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO users (username, password_hash, salt) VALUES (?1, ?2, ?3)",
            (username, hash, salt),
        )?;
        Ok(())
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
    ) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO audit_log (ts, user_id, username, method, path, status, ip)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (ts, user_id, username, method, path, status as i64, ip),
        )?;
        Ok(())
    }

    /// 分页读取审计日志（按时间倒序）
    pub fn audit_list(&self, limit: i64, offset: i64) -> Result<Vec<AuditRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT ts, user_id, username, method, path, status, ip
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
            "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out) VALUES (?1, ?2, ?3, ?4, ?5)",
            (p.ts, p.cpu, p.mem_used, p.net_in_per_sec, p.net_out_per_sec),
        )?;
        Ok(())
    }

    /// 读取最近 limit 条监控采样（按时间升序）
    pub fn recent_metrics(&self, limit: i64) -> Result<Vec<MetricPoint>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT ts, cpu, mem_used, net_in, net_out FROM metrics
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
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// 删除早于 before 时间戳的监控历史，返回删除行数
    pub fn prune_metrics(&self, before: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM metrics WHERE ts < ?1", [before])?;
        Ok(n as u64)
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
            conn.execute(
                "INSERT OR REPLACE INTO metrics_hourly
                     (hour_ts, cpu_avg, cpu_max, mem_used_avg, mem_used_max,
                      net_in_avg, net_in_max, net_out_avg, net_out_max)
                 SELECT ts - (ts % 3600),
                        AVG(cpu), MAX(cpu),
                        AVG(mem_used), MAX(mem_used),
                        AVG(net_in), MAX(net_in),
                        AVG(net_out), MAX(net_out)
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

    /// 历史查询（1.2：自动按时间跨度选表）。
    /// 起点落在原始保留窗口内 → 查 5 秒原始表；更早 → 查小时聚合表。
    /// 返回按时间升序、最多 limit 个点（取窗口尾部）。
    pub fn history(
        &self,
        from: i64,
        to: i64,
        raw_from: i64,
        limit: i64,
    ) -> Result<Vec<MetricPoint>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let sql = if from >= raw_from {
            "SELECT ts, cpu, mem_used, net_in, net_out FROM metrics
             WHERE ts >= ?1 AND ts <= ?2 ORDER BY ts DESC LIMIT ?3"
        } else {
            "SELECT hour_ts, cpu_avg, mem_used_avg, net_in_avg, net_out_avg FROM metrics_hourly
             WHERE hour_ts >= ?1 AND hour_ts <= ?2 ORDER BY hour_ts DESC LIMIT ?3"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt
            .query_map(rusqlite::params![from, to, limit], |row| {
                Ok(MetricPoint {
                    ts: row.get(0)?,
                    cpu: row.get(1)?,
                    mem_used: row.get(2)?,
                    net_in: row.get(3)?,
                    net_out: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    // ---------- AI 群聊（4.5） ----------

    /// 创建房间，返回新房间 id
    pub fn ai_room_add(
        &self,
        name: &str,
        creator_id: i64,
        visibility: &str,
        created: i64,
    ) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO ai_rooms (name, creator_id, visibility, created) VALUES (?1, ?2, ?3, ?4)",
            (name, creator_id, visibility, created),
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 房间是否存在（返回 (id, creator_id, visibility)）
    pub fn ai_room_get(&self, id: i64) -> Result<Option<(i64, i64, String)>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let r = conn
            .query_row(
                "SELECT id, creator_id, visibility FROM ai_rooms WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(r)
    }

    /// 列出用户可见房间：public 全员 + 自己创建的 + 受邀的（admin 全可见）
    pub fn ai_room_list(&self, user_id: i64, is_admin: bool) -> Result<Vec<AiRoomRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let sql = if is_admin {
            "SELECT id, name, creator_id, visibility, created FROM ai_rooms ORDER BY id"
        } else {
            "SELECT id, name, creator_id, visibility, created FROM ai_rooms
             WHERE visibility = 'public' OR creator_id = ?1
                OR id IN (SELECT room_id FROM ai_room_users WHERE user_id = ?1)
             ORDER BY id"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = if is_admin {
            stmt.query_map([], |row| {
                Ok(AiRoomRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    creator_id: row.get(2)?,
                    visibility: row.get(3)?,
                    created: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map([user_id], |row| {
                Ok(AiRoomRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    creator_id: row.get(2)?,
                    visibility: row.get(3)?,
                    created: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    /// 删除房间（级联清成员/受邀/消息）
    pub fn ai_room_delete(&self, id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute_batch("BEGIN")?;
        let r = (|| -> Result<()> {
            conn.execute("DELETE FROM ai_messages WHERE room_id = ?1", [id])?;
            conn.execute("DELETE FROM ai_members WHERE room_id = ?1", [id])?;
            conn.execute("DELETE FROM ai_room_users WHERE room_id = ?1", [id])?;
            conn.execute("DELETE FROM ai_rooms WHERE id = ?1", [id])?;
            Ok(())
        })();
        match r {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(1)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// 添加 AI 成员，返回新成员 id
    #[allow(clippy::too_many_arguments)]
    pub fn ai_member_add(
        &self,
        room_id: i64,
        name: &str,
        persona: &str,
        is_admin: bool,
        model: &str,
        api_base: &str,
        sort: i64,
    ) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO ai_members (room_id, name, persona, is_admin, model, api_base, sort)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                room_id,
                name,
                persona,
                is_admin as i64,
                model,
                api_base,
                sort
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 列出房间全部 AI 成员（按 sort、id 升序）
    pub fn ai_member_list(&self, room_id: i64) -> Result<Vec<AiMemberRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT id, room_id, name, persona, is_admin, model, api_base, sort
             FROM ai_members WHERE room_id = ?1 ORDER BY sort, id",
        )?;
        let rows = stmt
            .query_map([room_id], |row| {
                Ok(AiMemberRow {
                    id: row.get(0)?,
                    room_id: row.get(1)?,
                    name: row.get(2)?,
                    persona: row.get(3)?,
                    is_admin: row.get::<_, i64>(4)? != 0,
                    model: row.get(5)?,
                    api_base: row.get(6)?,
                    sort: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 单个 AI 成员（返回 (room_id, name, persona, is_admin, model, api_base)）
    pub fn ai_member_get(&self, id: i64) -> Result<Option<AiMemberTuple>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let r = conn
            .query_row(
                "SELECT room_id, name, persona, is_admin, model, api_base
                 FROM ai_members WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        Ok(r)
    }

    /// 按名字查成员（返回 (id, is_admin)），用于解析 @目标
    pub fn ai_member_by_name(&self, room_id: i64, name: &str) -> Result<Option<(i64, bool)>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let r = conn
            .query_row(
                "SELECT id, is_admin FROM ai_members WHERE room_id = ?1 AND name = ?2",
                rusqlite::params![room_id, name],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()?;
        Ok(r)
    }

    /// 更新 AI 成员（名字/人设/管理员标记/覆盖项）
    pub fn ai_member_update(
        &self,
        id: i64,
        name: &str,
        persona: &str,
        is_admin: bool,
        model: &str,
        api_base: &str,
    ) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "UPDATE ai_members SET name = ?2, persona = ?3, is_admin = ?4, model = ?5, api_base = ?6
             WHERE id = ?1",
            rusqlite::params![id, name, persona, is_admin as i64, model, api_base],
        )?;
        Ok(n as u64)
    }

    /// 删除 AI 成员
    pub fn ai_member_delete(&self, id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM ai_members WHERE id = ?1", [id])?;
        Ok(n as u64)
    }

    /// 邀请用户进私有房间（幂等）
    pub fn ai_room_invite(&self, room_id: i64, user_id: i64) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT OR IGNORE INTO ai_room_users (room_id, user_id) VALUES (?1, ?2)",
            (room_id, user_id),
        )?;
        Ok(())
    }

    /// 撤销邀请
    pub fn ai_room_uninvite(&self, room_id: i64, user_id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "DELETE FROM ai_room_users WHERE room_id = ?1 AND user_id = ?2",
            (room_id, user_id),
        )?;
        Ok(n as u64)
    }

    /// 受邀用户 id 列表
    pub fn ai_room_users(&self, room_id: i64) -> Result<Vec<i64>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt =
            conn.prepare("SELECT user_id FROM ai_room_users WHERE room_id = ?1 ORDER BY user_id")?;
        let rows = stmt
            .query_map([room_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(rows)
    }

    /// 用户是否可访问房间：public / 创建者 / admin（调用方传 is_admin）/ 受邀
    pub fn ai_room_can_access(
        &self,
        room_id: i64,
        user_id: i64,
        is_admin: bool,
    ) -> Result<bool> {
        if let Some((_, creator, vis)) = self.ai_room_get(room_id)? {
            if vis == "public" || creator == user_id || is_admin {
                return Ok(true);
            }
            let conn = self.pool.get().context("获取数据库连接失败")?;
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM ai_room_users WHERE room_id = ?1 AND user_id = ?2",
                (room_id, user_id),
                |r| r.get(0),
            )?;
            return Ok(n > 0);
        }
        Ok(false)
    }

    /// 写入一条房间消息，返回 (id, ts)
    pub fn ai_message_add(
        &self,
        room_id: i64,
        sender_type: &str,
        sender_id: i64,
        sender_name: &str,
        content: &str,
        ts: i64,
    ) -> Result<(i64, i64)> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO ai_messages (room_id, sender_type, sender_id, sender_name, content, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (room_id, sender_type, sender_id, sender_name, content, ts),
        )?;
        Ok((conn.last_insert_rowid(), ts))
    }

    /// 房间最近 limit 条消息（按 id 升序，取窗口尾部）
    pub fn ai_message_list(&self, room_id: i64, limit: i64) -> Result<Vec<AiMessageRow>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn.prepare(
            "SELECT id, room_id, sender_type, sender_id, sender_name, content, ts
             FROM ai_messages WHERE room_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let mut rows = stmt
            .query_map(rusqlite::params![room_id, limit], |row| {
                Ok(AiMessageRow {
                    id: row.get(0)?,
                    room_id: row.get(1)?,
                    sender_type: row.get(2)?,
                    sender_id: row.get(3)?,
                    sender_name: row.get(4)?,
                    content: row.get(5)?,
                    ts: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
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
    ) -> Result<()> {
        let db = self.clone();
        let (u, m, p, i) = (
            username.to_string(),
            method.to_string(),
            path.to_string(),
            ip.to_string(),
        );
        blocking(move || db.audit(ts, user_id, &u, &m, &p, status, &i)).await
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

    pub async fn insert_metric_async(&self, p: &crate::monitor::Snapshot) -> Result<()> {
        let db = self.clone();
        let p = p.clone();
        blocking(move || db.insert_metric(&p)).await
    }

    pub async fn recent_metrics_async(&self, limit: i64) -> Result<Vec<MetricPoint>> {
        let db = self.clone();
        blocking(move || db.recent_metrics(limit)).await
    }

    pub async fn prune_metrics_async(&self, before: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.prune_metrics(before)).await
    }

    pub async fn rollup_and_prune_async(&self, raw_before: i64, hourly_before: i64) -> Result<()> {
        let db = self.clone();
        blocking(move || db.rollup_and_prune(raw_before, hourly_before)).await
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

    // ---------- AI 群聊（4.5）异步包装 ----------

    pub async fn ai_room_add_async(
        &self,
        name: String,
        creator_id: i64,
        visibility: String,
        created: i64,
    ) -> Result<i64> {
        let db = self.clone();
        blocking(move || db.ai_room_add(&name, creator_id, &visibility, created)).await
    }

    pub async fn ai_room_get_async(&self, id: i64) -> Result<Option<(i64, i64, String)>> {
        let db = self.clone();
        blocking(move || db.ai_room_get(id)).await
    }

    pub async fn ai_room_list_async(
        &self,
        user_id: i64,
        is_admin: bool,
    ) -> Result<Vec<AiRoomRow>> {
        let db = self.clone();
        blocking(move || db.ai_room_list(user_id, is_admin)).await
    }

    pub async fn ai_room_delete_async(&self, id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_room_delete(id)).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ai_member_add_async(
        &self,
        room_id: i64,
        name: String,
        persona: String,
        is_admin: bool,
        model: String,
        api_base: String,
        sort: i64,
    ) -> Result<i64> {
        let db = self.clone();
        blocking(move || {
            db.ai_member_add(room_id, &name, &persona, is_admin, &model, &api_base, sort)
        })
        .await
    }

    pub async fn ai_member_list_async(&self, room_id: i64) -> Result<Vec<AiMemberRow>> {
        let db = self.clone();
        blocking(move || db.ai_member_list(room_id)).await
    }

    pub async fn ai_member_get_async(&self, id: i64) -> Result<Option<AiMemberTuple>> {
        let db = self.clone();
        blocking(move || db.ai_member_get(id)).await
    }

    pub async fn ai_member_by_name_async(&self, room_id: i64, name: String) -> Result<Option<(i64, bool)>> {
        let db = self.clone();
        blocking(move || db.ai_member_by_name(room_id, &name)).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ai_member_update_async(
        &self,
        id: i64,
        name: String,
        persona: String,
        is_admin: bool,
        model: String,
        api_base: String,
    ) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_member_update(id, &name, &persona, is_admin, &model, &api_base)).await
    }

    pub async fn ai_member_delete_async(&self, id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_member_delete(id)).await
    }

    pub async fn ai_room_invite_async(&self, room_id: i64, user_id: i64) -> Result<()> {
        let db = self.clone();
        blocking(move || db.ai_room_invite(room_id, user_id)).await
    }

    pub async fn ai_room_uninvite_async(&self, room_id: i64, user_id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_room_uninvite(room_id, user_id)).await
    }

    pub async fn ai_room_users_async(&self, room_id: i64) -> Result<Vec<i64>> {
        let db = self.clone();
        blocking(move || db.ai_room_users(room_id)).await
    }

    pub async fn ai_room_can_access_async(
        &self,
        room_id: i64,
        user_id: i64,
        is_admin: bool,
    ) -> Result<bool> {
        let db = self.clone();
        blocking(move || db.ai_room_can_access(room_id, user_id, is_admin)).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ai_message_add_async(
        &self,
        room_id: i64,
        sender_type: String,
        sender_id: i64,
        sender_name: String,
        content: String,
        ts: i64,
    ) -> Result<(i64, i64)> {
        let db = self.clone();
        blocking(move || {
            db.ai_message_add(room_id, &sender_type, sender_id, &sender_name, &content, ts)
        })
        .await
    }

    pub async fn ai_message_list_async(&self, room_id: i64, limit: i64) -> Result<Vec<AiMessageRow>> {
        let db = self.clone();
        blocking(move || db.ai_message_list(room_id, limit)).await
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

    /// 全新库：迁移建出全部表，版本号为最新（7）；再次打开幂等（不重复执行）
    #[test]
    fn fresh_db_gets_baseline() {
        let path = temp_db_path("fresh");
        {
            let db = Db::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), 7);
            assert!(table_exists(&db, "settings"));
            assert!(table_exists(&db, "users"));
            assert!(table_exists(&db, "metrics"));
            assert!(table_exists(&db, "metrics_hourly"));
            assert!(table_exists(&db, "audit_log"));
            assert!(table_exists(&db, "sessions"));
            assert!(table_exists(&db, "alert_events"));
            assert!(table_exists(&db, "ai_rooms"));
            assert!(table_exists(&db, "ai_members"));
            assert!(table_exists(&db, "ai_room_users"));
            assert!(table_exists(&db, "ai_messages"));
        }
        let db2 = Db::open(&path).unwrap();
        assert_eq!(db2.schema_version().unwrap(), 7);
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
            assert_eq!(db.schema_version().unwrap(), 7);
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
            assert_eq!(db.schema_version().unwrap(), 7, "失败的迁移不得推进版本");
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
                "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out) VALUES
                 (0,    10, 100, 1, 2),
                 (1800, 20, 200, 3, 4),
                 (3600, 30, 300, 5, 6);",
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
            // 幂等：再跑一次同样参数不新增/不报错（INSERT OR REPLACE）
            db.rollup_and_prune(3600, 0).unwrap();
            let n2: i64 = conn
                .query_row("SELECT COUNT(*) FROM metrics_hourly", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n2, 1, "重放聚合不应产生重复小时行");
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
                "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out) VALUES
                 (1000, 1, 1, 1, 1),
                 (2000, 2, 2, 2, 2);",
            )
            .unwrap();
            conn.execute_batch(
                "INSERT INTO metrics_hourly (hour_ts, cpu_avg, cpu_max, mem_used_avg, mem_used_max, net_in_avg, net_in_max, net_out_avg, net_out_max)
                 VALUES (0, 5, 5, 5, 5, 5, 5, 5, 5);",
            )
            .unwrap();
            // raw_from=100000：起点 0 早于保留期 → 查聚合表，返回 hour_ts=0 那条
            let old = db.history(0, 100000, 100000, 100).unwrap();
            assert_eq!(old.len(), 1);
            assert_eq!(old[0].cpu, 5.0, "应命中小时聚合表的值");
            // 起点 1000 在保留期内（raw_from=500 ≤ 1000）→ 查原始表，返回两条
            let fresh = db.history(1000, 100000, 500, 100).unwrap();
            assert_eq!(fresh.len(), 2);
            assert_eq!(fresh[0].ts, 1000, "结果按时间升序");
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

    /// 2.3：审计写入与分页读取（时间倒序），prune 按时间清理
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
            )
            .unwrap();
            db.audit(200, None, "-", "POST", "/api/login", 401, "9.9.9.9")
                .unwrap();
            let rows = db.audit_list(10, 0).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].ts, 200, "按时间倒序");
            assert_eq!(rows[0].user_id, None);
            assert_eq!(rows[1].username, "admin");
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
