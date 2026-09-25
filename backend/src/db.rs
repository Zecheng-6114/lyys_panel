use std::path::Path;

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::Serialize;

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
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS users (
                 id            INTEGER PRIMARY KEY,
                 username      TEXT NOT NULL UNIQUE,
                 password_hash TEXT NOT NULL,
                 salt          TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS metrics (
                 ts       INTEGER NOT NULL,
                 cpu      REAL NOT NULL,
                 mem_used INTEGER NOT NULL,
                 net_in   INTEGER NOT NULL,
                 net_out  INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_metrics_ts ON metrics (ts);
             CREATE TABLE IF NOT EXISTS ai_memory (
                 id        INTEGER PRIMARY KEY,
                 ts        INTEGER NOT NULL,
                 content   TEXT NOT NULL,
                 embedding BLOB
             );
             CREATE TABLE IF NOT EXISTS ai_session (
                 id        INTEGER PRIMARY KEY,
                 created   INTEGER NOT NULL,
                 updated   INTEGER NOT NULL,
                 title     TEXT NOT NULL,
                 msg_count INTEGER NOT NULL,
                 messages  TEXT NOT NULL
             );",
        )
        .context("初始化数据库表结构失败")?;
        Ok(())
    }

    /// 读取配置项
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let value = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
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

    /// 创建用户
    pub fn create_user(&self, username: &str, hash: &str, salt: &str) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO users (username, password_hash, salt) VALUES (?1, ?2, ?3)",
            (username, hash, salt),
        )?;
        Ok(())
    }

    /// 写入一条监控采样
    pub fn insert_metric(&self, p: &crate::monitor::Snapshot) -> Result<()> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        conn.execute(
            "INSERT INTO metrics (ts, cpu, mem_used, net_in, net_out) VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                p.ts,
                p.cpu,
                p.mem_used,
                p.net_in_per_sec,
                p.net_out_per_sec,
            ),
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

    // ---------- AI 记忆（AI 助手功能暂时停用，整段注释；恢复时连同下方 async 包装与结构体一起放开） ----------
    /*
    /// 新增一条记忆，返回它的 id
    pub fn ai_memory_add(&self, ts: i64, content: &str, embedding: Option<&[f32]>) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let blob = embedding.map(|v| {
            let mut b = Vec::with_capacity(v.len() * 4);
            for f in v {
                b.extend_from_slice(&f.to_le_bytes());
            }
            b
        });
        conn.execute(
            "INSERT INTO ai_memory (ts, content, embedding) VALUES (?1, ?2, ?3)",
            rusqlite::params![ts, content, blob],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 记忆列表（新的在前），不回传向量本身
    pub fn ai_memory_list(&self, limit: i64) -> Result<Vec<AiMemory>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, content, embedding IS NOT NULL FROM ai_memory
                 ORDER BY ts DESC, id DESC LIMIT ?1",
            )
            .context("准备查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(AiMemory {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    content: row.get(2)?,
                    has_embedding: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 取出所有带向量的记忆，供检索时算相似度
    pub fn ai_memory_vectors(&self) -> Result<Vec<(i64, String, Vec<f32>)>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn
            .prepare("SELECT id, content, embedding FROM ai_memory WHERE embedding IS NOT NULL")
            .context("准备查询失败")?;
        let rows = stmt
            .query_map([], |row| {
                let blob: Vec<u8> = row.get(2)?;
                // as_chunks::<4>().0 为完整的 4 字节块，尾部残字节（数据损坏时）自动丢弃
                let vec: Vec<f32> = blob
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .copied()
                    .map(f32::from_le_bytes)
                    .collect();
                Ok((row.get(0)?, row.get(1)?, vec))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 按 id 删除记忆
    pub fn ai_memory_delete(&self, id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM ai_memory WHERE id = ?1", [id])?;
        Ok(n as u64)
    }

    // ---------- AI 会话 ----------

    /// 保存会话：给 id 就更新，不给就新建。返回会话 id。
    pub fn ai_session_save(
        &self,
        id: Option<i64>,
        title: &str,
        msg_count: i64,
        messages: &str,
    ) -> Result<i64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if let Some(id) = id {
            let n = conn.execute(
                "UPDATE ai_session SET updated = ?1, title = ?2, msg_count = ?3, messages = ?4
                 WHERE id = ?5",
                rusqlite::params![now, title, msg_count, messages, id],
            )?;
            if n > 0 {
                return Ok(id);
            }
            // 更新不到（会话被删过）就当作新建，避免前端报错
        }
        conn.execute(
            "INSERT INTO ai_session (created, updated, title, msg_count, messages)
             VALUES (?1, ?1, ?2, ?3, ?4)",
            rusqlite::params![now, title, msg_count, messages],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 会话列表（最近更新的在前），只回元信息不回正文
    pub fn ai_session_list(&self, limit: i64) -> Result<Vec<AiSessionMeta>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let mut stmt = conn
            .prepare(
                "SELECT id, created, updated, title, msg_count FROM ai_session
                 ORDER BY updated DESC LIMIT ?1",
            )
            .context("准备查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(AiSessionMeta {
                    id: row.get(0)?,
                    created: row.get(1)?,
                    updated: row.get(2)?,
                    title: row.get(3)?,
                    msg_count: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 读一个会话的正文
    pub fn ai_session_get(&self, id: i64) -> Result<Option<(String, String)>> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let row = conn
            .query_row(
                "SELECT title, messages FROM ai_session WHERE id = ?1",
                [id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    pub fn ai_session_delete(&self, id: i64) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute("DELETE FROM ai_session WHERE id = ?1", [id])?;
        Ok(n as u64)
    }

    /// 删除内容里包含指定文字的记忆（「忘掉这件事」用）
    pub fn ai_memory_forget(&self, needle: &str) -> Result<u64> {
        let conn = self.pool.get().context("获取数据库连接失败")?;
        let n = conn.execute(
            "DELETE FROM ai_memory WHERE content LIKE '%' || ?1 || '%'",
            [needle],
        )?;
        Ok(n as u64)
    }
    */
}

/* AI 助手功能暂时停用，以下结构体（会话元信息 / 记忆）一并注释
#[derive(Serialize)]
pub struct AiSessionMeta {
    pub id: i64,
    pub created: i64,
    pub updated: i64,
    pub title: String,
    pub msg_count: i64,
}

/// 一条 AI 记忆（不回传向量）
#[derive(Serialize)]
pub struct AiMemory {
    pub id: i64,
    pub ts: i64,
    pub content: String,
    /// 是否已算出向量。没有向量的记忆仍可被关键词召回
    pub has_embedding: bool,
}
*/

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
    // 主题定制（/api/theme）使用；AI 段恢复时同样依赖这两个包装
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

    // ---------- AI 异步包装（AI 助手功能暂时停用，整段注释；恢复时与同步段一起放开） ----------
    /*
    pub async fn ai_memory_add_async(
        &self,
        ts: i64,
        content: String,
        embedding: Option<Vec<f32>>,
    ) -> Result<i64> {
        let db = self.clone();
        blocking(move || db.ai_memory_add(ts, &content, embedding.as_deref())).await
    }

    pub async fn ai_memory_list_async(&self, limit: i64) -> Result<Vec<AiMemory>> {
        let db = self.clone();
        blocking(move || db.ai_memory_list(limit)).await
    }

    pub async fn ai_memory_vectors_async(&self) -> Result<Vec<(i64, String, Vec<f32>)>> {
        let db = self.clone();
        blocking(move || db.ai_memory_vectors()).await
    }

    pub async fn ai_memory_delete_async(&self, id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_memory_delete(id)).await
    }

    pub async fn ai_memory_forget_async(&self, needle: String) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_memory_forget(&needle)).await
    }

    pub async fn ai_session_save_async(
        &self,
        id: Option<i64>,
        title: String,
        msg_count: i64,
        messages: String,
    ) -> Result<i64> {
        let db = self.clone();
        blocking(move || db.ai_session_save(id, &title, msg_count, &messages)).await
    }

    pub async fn ai_session_list_async(&self, limit: i64) -> Result<Vec<AiSessionMeta>> {
        let db = self.clone();
        blocking(move || db.ai_session_list(limit)).await
    }

    pub async fn ai_session_get_async(&self, id: i64) -> Result<Option<(String, String)>> {
        let db = self.clone();
        blocking(move || db.ai_session_get(id)).await
    }

    pub async fn ai_session_delete_async(&self, id: i64) -> Result<u64> {
        let db = self.clone();
        blocking(move || db.ai_session_delete(id)).await
    }
    */
}

/// rusqlite 没有 re-export this trait，这里引入供 `.optional()` 使用
use rusqlite::OptionalExtension;
