// 3.1 面板自备份与恢复
//
// 备份对象是 SQLite 主库（配置、用户、审计、指标全在里面）。用
// `VACUUM INTO` 生成一致性快照——它不锁写者太久、产物天然压缩去碎片，
// 且不需要额外的 rusqlite backup feature。
//
// 恢复采用「标记 + 重启生效」：恢复会整体覆盖运行中的库，无法在线做。
// 管理员点恢复 → 写 pending_restore 标记文件 → 重启服务 → 启动流程在
// 打开数据库**之前**应用标记（覆盖主库并清理 -wal/-shm，否则旧 WAL
// 会叠加到新库上造成损坏）。标记存文件而非 settings 表，因为表本身就是
// 被覆盖的对象。
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::db::Db;

/// 保留份数默认值（可被配置覆盖）
pub const BACKUP_KEEP: usize = 7;
/// 保留份数上限
const BACKUP_KEEP_MAX: usize = 100;

/// 备份配置：目录与保留份数。
///
/// 存文件而非 settings 表 —— 恢复流程在打开数据库**之前**就要解析备份
/// 目录（见 [`apply_pending_restore`]），那时读不到表，只能读文件。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupConfig {
    /// 自定义备份目录；空 = data_dir/backups
    #[serde(default)]
    pub dir: String,
    /// 保留份数（1–100）
    #[serde(default = "default_keep")]
    pub keep: usize,
}

fn default_keep() -> usize {
    BACKUP_KEEP
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            dir: String::new(),
            keep: BACKUP_KEEP,
        }
    }
}

fn config_file(data_dir: &Path) -> PathBuf {
    data_dir.join("backup.conf")
}

/// 读取配置；文件缺失或损坏一律回退默认值（不能因配置坏了就停摆）
pub fn load_config(data_dir: &Path) -> BackupConfig {
    std::fs::read_to_string(config_file(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(data_dir: &Path, cfg: &BackupConfig) -> Result<()> {
    let s = serde_json::to_string(cfg).context("序列化备份配置失败")?;
    std::fs::write(config_file(data_dir), s).context("写入备份配置失败")?;
    Ok(())
}

/// 保存前校验：目录须为绝对路径且不含 `..`；保留份数 1–100
pub fn validate_config(cfg: &BackupConfig) -> Result<()> {
    let dir = cfg.dir.trim();
    if !dir.is_empty() {
        if !Path::new(dir).is_absolute() {
            bail!("备份目录必须是绝对路径");
        }
        if dir.split(['/', '\\']).any(|seg| seg == "..") {
            bail!("备份目录不能包含 ..");
        }
    }
    if cfg.keep < 1 || cfg.keep > BACKUP_KEEP_MAX {
        bail!("保留份数需在 1–{BACKUP_KEEP_MAX} 之间");
    }
    Ok(())
}

/// 当前生效的备份目录（配置为空则回退 data_dir/backups）
pub fn backups_dir(data_dir: &Path) -> PathBuf {
    let dir = load_config(data_dir).dir;
    let dir = dir.trim();
    if dir.is_empty() {
        data_dir.join("backups")
    } else {
        PathBuf::from(dir)
    }
}

/// 备份文件名校验：panel-YYYYMMDD-HHMMSS.db。
/// 所有按名字操作文件的接口（下载/删除/恢复）都必须先过这里，杜绝路径穿越。
pub fn is_valid_backup_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_prefix("panel-")
        .and_then(|s| s.strip_suffix(".db"))
    else {
        return false;
    };
    let bytes = stem.as_bytes();
    bytes.len() == 15
        && bytes[8] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| {
            if i == 8 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// UTC 时间戳文件名
fn backup_name_now() -> String {
    let now = time::OffsetDateTime::now_utc();
    let m = now.month() as u8;
    format!(
        "panel-{:04}{:02}{:02}-{:02}{:02}{:02}.db",
        now.year(),
        m,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// 立即备份一次，返回文件名。成功后顺带执行保留清理。
pub fn create_backup(db: &Db, data_dir: &Path) -> Result<String> {
    let dir = backups_dir(data_dir);
    let keep = load_config(data_dir).keep;
    std::fs::create_dir_all(&dir).context("创建备份目录失败")?;
    let name = backup_name_now();
    let target = dir.join(&name);
    if target.exists() {
        bail!("备份文件已存在（同一秒内重复触发），请稍后重试");
    }
    db.vacuum_into(&target)?;
    if let Err(e) = prune_backups(&dir, keep) {
        tracing::warn!("备份保留清理失败：{e}");
    }
    tracing::info!("已创建数据库备份：{name}");
    Ok(name)
}

/// 备份列表（按文件名倒序 = 新→旧）
#[derive(Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub size: u64,
    pub mtime: i64,
}

pub fn list_backups(data_dir: &Path) -> Result<Vec<BackupInfo>> {
    let dir = backups_dir(data_dir);
    let mut out = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir).context("读取备份目录失败")? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_valid_backup_name(&name) {
                continue;
            }
            let meta = entry.metadata()?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            out.push(BackupInfo {
                name,
                size: meta.len(),
                mtime,
            });
        }
    }
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

/// 解析并校验备份文件路径（存在性 + 名称白名单）
pub fn resolve_backup(data_dir: &Path, name: &str) -> Result<PathBuf> {
    if !is_valid_backup_name(name) {
        bail!("非法备份文件名");
    }
    let p = backups_dir(data_dir).join(name);
    if !p.is_file() {
        bail!("备份文件不存在");
    }
    Ok(p)
}

pub fn delete_backup(data_dir: &Path, name: &str) -> Result<()> {
    let p = resolve_backup(data_dir, name)?;
    std::fs::remove_file(&p).context("删除备份失败")?;
    tracing::info!("已删除备份：{name}");
    Ok(())
}

/// 导入外部备份文件：校验 SQLite 文件头后落盘到备份目录并分配新名，
/// 返回文件名；管理员在列表中正常点恢复即可。
///
/// 只做文件头校验（与上传字体同一思路）：恢复本身有「先另存当前库」的
/// 反悔备份兜底，不必在这里做完整库校验。
pub fn import_backup(data_dir: &Path, bytes: &[u8]) -> Result<String> {
    if bytes.len() < 100 || !bytes.starts_with(b"SQLite format 3\0") {
        bail!("不是有效的 SQLite 数据库文件");
    }
    let dir = backups_dir(data_dir);
    std::fs::create_dir_all(&dir).context("创建备份目录失败")?;
    let name = backup_name_now();
    let target = dir.join(&name);
    if target.exists() {
        bail!("导入过于频繁（同一秒内重复），请稍后重试");
    }
    std::fs::write(&target, bytes).context("写入导入备份失败")?;
    tracing::info!("已导入备份：{name}");
    Ok(name)
}

/// 保留策略：只留最新 `keep` 份
fn prune_backups(dir: &Path, keep: usize) -> Result<()> {
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| is_valid_backup_name(n))
        .collect();
    names.sort();
    names.reverse();
    for old in names.iter().skip(keep) {
        if let Err(e) = std::fs::remove_file(dir.join(old)) {
            tracing::warn!("删除过期备份 {old} 失败：{e}");
        }
    }
    Ok(())
}

/// 每日自动备份入口（monitor 每小时调用）：距上次成功 ≥24h 才备份。
/// 上次时间记在 settings 表（key=last_backup_ts）。
pub fn maybe_daily_backup(db: &Db, data_dir: &Path) -> Result<()> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let last: i64 = db
        .get_setting("last_backup_ts")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if now - last < 24 * 3600 {
        return Ok(());
    }
    create_backup(db, data_dir)?;
    db.set_setting("last_backup_ts", &now.to_string())?;
    Ok(())
}

// ---------- 恢复：标记文件 + 启动时应用 ----------

pub fn pending_restore_file(data_dir: &Path) -> PathBuf {
    data_dir.join("pending_restore")
}

/// 写入恢复标记（重启后生效）
pub fn set_pending_restore(data_dir: &Path, name: &str) -> Result<()> {
    // 先校验备份存在，避免标记一个不存在的文件让下次启动扑空
    resolve_backup(data_dir, name)?;
    std::fs::write(pending_restore_file(data_dir), name).context("写入恢复标记失败")?;
    tracing::warn!("已登记数据库恢复：{name}（重启服务后生效）");
    Ok(())
}

/// 启动时应用恢复标记（必须在 Db::open 之前调用）。
/// 返回 true 表示本次启动执行了恢复。覆盖前把当前库另存为
/// pre-restore 备份，留一条反悔路。
pub fn apply_pending_restore(data_dir: &Path, db_path: &str) -> Result<bool> {
    let pf = pending_restore_file(data_dir);
    if !pf.exists() {
        return Ok(false);
    }
    let name = std::fs::read_to_string(&pf).context("读取恢复标记失败")?;
    let name = name.trim();
    let src = match resolve_backup(data_dir, name) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("恢复标记指向无效备份，放弃恢复并清除标记：{e}");
            let _ = std::fs::remove_file(&pf);
            return Ok(false);
        }
    };
    let dbp = Path::new(db_path);
    if dbp.exists() {
        let safety = backups_dir(data_dir).join(format!("pre-restore-{name}"));
        std::fs::create_dir_all(backups_dir(data_dir))?;
        std::fs::copy(dbp, &safety)
            .with_context(|| format!("恢复前备份当前库失败：{}", safety.display()))?;
    }
    std::fs::copy(&src, dbp).context("恢复：覆盖主库文件失败")?;
    // 旧 WAL/SHM 属于被覆盖前的库，必须删除，否则 SQLite 会把它们
    // 叠加到恢复出来的库上导致数据错乱
    for suffix in ["-wal", "-shm"] {
        let ws_path = format!("{db_path}{suffix}");
        let p = Path::new(&ws_path);
        if p.exists() {
            let _ = std::fs::remove_file(p);
        }
    }
    std::fs::remove_file(&pf).context("清除恢复标记失败")?;
    tracing::warn!("数据库已从备份 {name} 恢复完成");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_name_validation() {
        assert!(is_valid_backup_name("panel-20260927-120000.db"));
        assert!(!is_valid_backup_name("panel-20260927-12000.db"));
        assert!(!is_valid_backup_name("panel-2026092a-120000.db"));
        assert!(!is_valid_backup_name("../../etc/passwd"));
        assert!(!is_valid_backup_name("panel-20260927-120000.db.sh"));
        assert!(!is_valid_backup_name("other-20260927-120000.db"));
        assert!(!is_valid_backup_name("panel-20260927_120000.db"));
    }

    #[test]
    fn create_list_delete_backup() {
        let tmp = std::env::temp_dir().join(format!("lyys_backup_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = tmp.join("panel.db").to_string_lossy().to_string();
        let db = Db::open(&db_path).unwrap();
        db.set_setting("probe", "hello").unwrap();

        let name = create_backup(&db, &tmp).unwrap();
        assert!(is_valid_backup_name(&name));
        let list = list_backups(&tmp).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].size > 0);

        // 备份内容可查：把备份文件当库打开，设置项还在
        let restored = Db::open(&backups_dir(&tmp).join(&list[0].name).to_string_lossy()).unwrap();
        assert_eq!(
            restored.get_setting("probe").unwrap().as_deref(),
            Some("hello")
        );
        drop(restored);

        delete_backup(&tmp, &name).unwrap();
        assert!(list_backups(&tmp).unwrap().is_empty());
        // 删除不存在的必须报错而不是静默
        assert!(delete_backup(&tmp, &name).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn restore_flow_marks_and_applies() {
        let tmp = std::env::temp_dir().join(format!("lyys_restore_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = tmp.join("panel.db").to_string_lossy().to_string();
        let db = Db::open(&db_path).unwrap();
        db.set_setting("stage", "v1").unwrap();
        let name = create_backup(&db, &tmp).unwrap();
        drop(db);

        // 改坏当前库，然后登记恢复
        let db = Db::open(&db_path).unwrap();
        db.set_setting("stage", "v2-broken").unwrap();
        drop(db);
        set_pending_restore(&tmp, &name).unwrap();
        assert!(pending_restore_file(&tmp).exists());

        // 模拟重启：应用标记 → 重新打开库 → 数据回到 v1
        assert!(apply_pending_restore(&tmp, &db_path).unwrap());
        assert!(!pending_restore_file(&tmp).exists());
        let db = Db::open(&db_path).unwrap();
        assert_eq!(db.get_setting("stage").unwrap().as_deref(), Some("v1"));
        // 反悔备份存在
        assert!(backups_dir(&tmp)
            .join(format!("pre-restore-{name}"))
            .exists());
        // 无标记时 apply 返回 false
        assert!(!apply_pending_restore(&tmp, &db_path).unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_roundtrip_and_validation() {
        let tmp = std::env::temp_dir().join(format!("lyys_backupcfg_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // 默认：落 data_dir/backups，保留 7 份
        let cfg = load_config(&tmp);
        assert_eq!(cfg.keep, BACKUP_KEEP);
        assert!(cfg.dir.is_empty());
        assert!(backups_dir(&tmp).ends_with("backups"));

        // 自定义目录后 backups_dir 跟随
        let custom = BackupConfig {
            dir: tmp.join("mydb").to_string_lossy().to_string(),
            keep: 3,
        };
        validate_config(&custom).unwrap();
        save_config(&tmp, &custom).unwrap();
        assert_eq!(backups_dir(&tmp), tmp.join("mydb"));
        assert_eq!(load_config(&tmp).keep, 3);

        // 非法：相对路径 / 含 .. / 保留份数越界
        assert!(validate_config(&BackupConfig {
            dir: "relative/dir".into(),
            keep: 3
        })
        .is_err());
        assert!(validate_config(&BackupConfig {
            dir: "/data/../etc".into(),
            keep: 3
        })
        .is_err());
        assert!(validate_config(&BackupConfig {
            dir: String::new(),
            keep: 0
        })
        .is_err());
        assert!(validate_config(&BackupConfig {
            dir: String::new(),
            keep: BACKUP_KEEP_MAX + 1
        })
        .is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn import_validates_sqlite_header() {
        let tmp = std::env::temp_dir().join(format!("lyys_backupimp_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // 垃圾数据被拒
        assert!(import_backup(&tmp, b"definitely not a database").is_err());

        // 真实库文件可导入，并且出现在列表中
        let db_path = tmp.join("panel.db").to_string_lossy().to_string();
        let db = Db::open(&db_path).unwrap();
        db.set_setting("probe", "imported").unwrap();
        drop(db);
        let bytes = std::fs::read(&db_path).unwrap();
        let name = import_backup(&tmp, &bytes).unwrap();
        assert!(is_valid_backup_name(&name));
        assert!(list_backups(&tmp).unwrap().iter().any(|b| b.name == name));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
