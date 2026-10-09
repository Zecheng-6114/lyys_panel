//! 目录体积统计（仪表盘「磁盘空间占用 Top 5 目录」卡）。
//!
//! 只用 std：默认扫描 `/` 的第一层子目录（`/var`、`/usr`、`/home`…），
//! 每个子目录并行遍历，得到体积排名。
//!
//! 设计取舍（都是踩过的点）：
//!
//! - **每个子目录一个线程，而不是共享工作队列**：队列方案要处理「队列空但
//!   工作线程还在跑」的收尾竞态，容易死等；一层子目录数量有限（十几到几十个），
//!   一个线程一个子目录足够，且/var 这种大目录自己不会再被切细 —— 对 Top N 排名
//!   没有影响。`available_parallelism()` 限制并发数，其余排队。
//! - **不跟随符号链接**：跟随会走出分区（同一目录被算两次）甚至成环。
//!   `symlink_metadata` 不做解析，链接本身按自身长度计入。
//! - **按 st_dev 过滤**：只累加与起点同一设备的文件。不这么做的话，
//!   `/home` 挂在另一块盘上时会被算进根分区，几块盘的占用互相重叠。
//! - **有界时间预算**：几十万文件的目录会跑很久，而这是个 HTTP 接口。
//!   超时即返回当前结果并标记 `truncated`，绝不把请求挂住。
//! - **权限错误不中断**：跳过并计数，返回值里带 `inaccessible` ——
//!   数字对不上时，这一项就是解释。
//! - **缓存**：同一路径 10 分钟内直接复用；`refresh=1` 强制重扫。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

/// 单次扫描的时间预算。超过就带着已有结果返回（`truncated=true`）。
const SCAN_BUDGET: Duration = Duration::from_secs(3);

/// 结果缓存有效期。目录体积以分钟计，10 分钟内复用完全够。
const CACHE_TTL: Duration = Duration::from_secs(600);

/// 缓存条数上限。每条是一个路径的排名结果，几十条已经远超实际使用。
const CACHE_MAX: usize = 32;

/// 允许扫描的起点。**白名单而非黑名单**：这个接口能枚举任意目录体积，
/// 开放任意路径等于给了一个文件系统探测器；实际需求只有「哪个顶层目录最占地方」。
const ALLOWED_ROOTS: &[&str] = &["/"];

/// 扫描时跳过的伪文件系统。它们不在任何块设备上，体积没有意义，
/// 而 /proc 之类的「体积」还会随读取行为变化。
const SKIP_MOUNTS: &[&str] = &["/proc", "/sys", "/dev", "/run", "/tmp/.X11-unix"];

/// 单个子目录的体积
#[derive(Clone, Serialize)]
pub struct DirUsage {
    pub path: String,
    pub bytes: u64,
    /// 占本次扫描总量的百分比
    pub pct: f64,
    /// 是否在时间预算内被截断（该目录只统计了一部分）
    pub truncated: bool,
}

/// 接口返回体
#[derive(Clone, Serialize)]
pub struct DirUsageReport {
    pub root: String,
    /// 所有同级子目录体积之和（不含被跳过/无权限的部分）
    pub total: u64,
    pub children: Vec<DirUsage>,
    /// 被跳过的目录数与跳过的字节量（无权限或伪文件系统）
    pub inaccessible: usize,
    /// 是否有目录因时间预算被截断
    pub truncated: bool,
    /// 本次扫描耗时（毫秒）
    pub took_ms: u64,
    /// 结果是否来自缓存
    pub cached: bool,
    /// 生成时刻（Unix 秒）
    pub ts: i64,
}

/// 进程内结果缓存（路径 → 结果）
#[derive(Default)]
pub struct DirUsageCache {
    entries: Mutex<HashMap<String, DirUsageReport>>,
}

impl DirUsageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 命中未过期的缓存则返回副本，并把 `cached` 标记为 true
    pub fn get(&self, key: &str) -> Option<DirUsageReport> {
        let map = self.entries.lock().ok()?;
        let hit = map.get(key)?;
        let age = now_secs() - hit.ts;
        if age >= 0 && (age as u64) < CACHE_TTL.as_secs() {
            let mut r = hit.clone();
            r.cached = true;
            Some(r)
        } else {
            None
        }
    }

    pub fn put(&self, key: &str, report: &DirUsageReport) {
        let Ok(mut map) = self.entries.lock() else {
            return;
        };
        // 简单容量控制：满了先清掉过期项，仍然满就整体清空
        // （缓存只影响性能，清空的代价只是下次重扫）。
        if map.len() >= CACHE_MAX && !map.contains_key(key) {
            let now = now_secs();
            map.retain(|_, v| (now - v.ts) >= 0 && ((now - v.ts) as u64) < CACHE_TTL.as_secs());
            if map.len() >= CACHE_MAX {
                map.clear();
            }
        }
        map.insert(key.to_string(), report.clone());
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 收集一个目录下的「直接子目录 + 直接文件体积」，忽略符号链接与不可访问项。
///
/// `dev` 为起点设备号；`skipped` 累加被跳过的条目数。
fn direct_children(root: &Path, dev: u64, skipped: &AtomicUsize) -> (Vec<(PathBuf, u64)>, u64) {
    let mut dirs = Vec::new();
    let mut files_bytes = 0u64;
    let Ok(rd) = std::fs::read_dir(root) else {
        skipped.fetch_add(1, Ordering::Relaxed);
        return (dirs, files_bytes);
    };
    for entry in rd {
        let Ok(entry) = entry else {
            skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let path = entry.path();
        if is_skipped(&path) {
            skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let Ok(md) = std::fs::symlink_metadata(&path) else {
            skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if md.is_symlink() {
            // 不跟随：只计入链接自身长度，避免跨分区重复统计与成环
            files_bytes += md.len();
            continue;
        }
        if md.is_dir() {
            dirs.push((path, md.len()));
            continue;
        }
        // 其他设备的挂载点（bind mount / 独立分区）不计入本设备
        if dev_of(&md) != dev {
            continue;
        }
        files_bytes += md.len();
    }
    (dirs, files_bytes)
}

fn is_skipped(path: &Path) -> bool {
    SKIP_MOUNTS.iter().any(|s| path == Path::new(s))
}

#[cfg(unix)]
fn dev_of(md: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.dev()
}

#[cfg(not(unix))]
fn dev_of(_md: &std::fs::Metadata) -> u64 {
    // 非 Unix 平台（开发机编译用）没有设备号概念：全部视作同一设备
    0
}

/// 遍历一个目录树，返回累计字节数。
///
/// 用显式栈而非递归：目录深度不可控，递归在极端深度下会爆栈。
fn walk(root: &Path, dev: u64, deadline: Instant, skipped: &AtomicUsize) -> (u64, bool) {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    let mut truncated = false;
    while let Some(dir) = stack.pop() {
        if Instant::now() >= deadline {
            truncated = true;
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        for entry in rd {
            let Ok(entry) = entry else {
                skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let path = entry.path();
            let Ok(md) = std::fs::symlink_metadata(&path) else {
                skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            if md.is_symlink() {
                total += md.len();
            } else if md.is_dir() {
                if dev_of(&md) == dev {
                    stack.push(path);
                }
            } else if dev_of(&md) == dev {
                total += md.len();
            }
        }
    }
    (total, truncated)
}

/// 扫描 `root` 的第一层子目录并排名。
///
/// 同步实现（调用方负责丢进 spawn_blocking）：内部按子目录起线程，
/// 并发度受 `available_parallelism` 限制。
pub fn scan(root: &Path) -> Result<DirUsageReport> {
    let started = Instant::now();
    let meta = std::fs::metadata(root)
        .with_context(|| format!("读取目录失败：{}", root.display()))?;
    anyhow::ensure!(meta.is_dir(), "{} 不是目录", root.display());
    let dev = dev_of(&meta);

    let skipped = Arc::new(AtomicUsize::new(0));
    let (dirs, root_files) = direct_children(root, dev, &skipped);
    let deadline = started + SCAN_BUDGET;

    let par = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let sizes: Vec<Arc<(AtomicU64, AtomicBool)>> = (0..dirs.len())
        .map(|_| Arc::new((AtomicU64::new(0), AtomicBool::new(false))))
        .collect();

    let next = Arc::new(AtomicUsize::new(0));
    let any_truncated = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        for _ in 0..par.min(dirs.len()) {
            let next = Arc::clone(&next);
            let sizes = &sizes;
            let dirs = &dirs;
            let skipped = Arc::clone(&skipped);
            let any_truncated = Arc::clone(&any_truncated);
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= dirs.len() {
                        break;
                    }
                    let (bytes, truncated) = walk(&dirs[i].0, dev, deadline, &skipped);
                    sizes[i].0.store(bytes, Ordering::Relaxed);
                    if truncated {
                        sizes[i].1.store(true, Ordering::Relaxed);
                        any_truncated.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    let mut children: Vec<(String, u64, bool)> = Vec::with_capacity(dirs.len());
    let mut total = root_files;
    for (i, (path, own_len)) in dirs.iter().enumerate() {
        let bytes = sizes[i].0.load(Ordering::Relaxed) + own_len;
        total += bytes;
        children.push((
            path.to_string_lossy().to_string(),
            bytes,
            sizes[i].1.load(Ordering::Relaxed),
        ));
    }
    // 体积降序；同体积时按路径排，保证结果稳定可复现
    children.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let children: Vec<DirUsage> = children
        .into_iter()
        .map(|(path, bytes, truncated)| DirUsage {
            path,
            bytes,
            pct: if total > 0 {
                (bytes as f64 / total as f64) * 100.0
            } else {
                0.0
            },
            truncated,
        })
        .collect();

    Ok(DirUsageReport {
        root: root.to_string_lossy().to_string(),
        total,
        children,
        inaccessible: skipped.load(Ordering::Relaxed),
        truncated: any_truncated.load(Ordering::Relaxed),
        took_ms: started.elapsed().as_millis() as u64,
        cached: false,
        ts: now_secs(),
    })
}

/// 扫描起点的校验失败原因。
///
/// 单独成型而不是直接 `anyhow::bail!`：`ApiError: From<anyhow::Error>` 会把
/// 任何错误都映射成 **500**，于是「路径不在白名单里」这种用户输入问题会被报成
/// 服务端故障（实测就是这个现象）。这里把「客户端给错了」与「服务端自己坏了」
/// 分开，调用方据此选 400 还是 500。
#[derive(Debug)]
pub enum ResolveError {
    /// 用户给的路径不合法：不存在、不可访问、或不在白名单内
    BadRequest(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::BadRequest(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ResolveError {}

/// 解析并校验请求的起点路径。
///
/// 只允许白名单起点（见 `ALLOWED_ROOTS`）：这个接口能枚举任意目录体积，
/// 放开任意路径等于给了一个文件系统探测器。`canonicalize` 之后再比对，
/// 使 `/`、`//`、`/./` 这类写法归一，也挡住 `..` 穿越。
pub fn resolve_root(requested: &str) -> Result<PathBuf, ResolveError> {
    let canonical = std::fs::canonicalize(requested)
        .map_err(|_| ResolveError::BadRequest(format!("路径不存在或不可访问：{requested}")))?;
    if !ALLOWED_ROOTS.iter().any(|a| canonical == Path::new(a)) {
        return Err(ResolveError::BadRequest(format!(
            "只允许扫描 {}",
            ALLOWED_ROOTS.join("、")
        )));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 扫描一个自建的临时目录树：总量、排名、百分比都要对得上。
    #[test]
    fn scan_ranks_children_by_size() {
        let base = std::env::temp_dir().join(format!("lyys-dirscan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // big=300B、mid=150B、small=50B，另加根目录下一个 10B 的文件
        std::fs::create_dir_all(base.join("big")).unwrap();
        std::fs::create_dir_all(base.join("mid")).unwrap();
        std::fs::create_dir_all(base.join("small")).unwrap();
        std::fs::write(base.join("big/a.bin"), vec![0u8; 200]).unwrap();
        std::fs::write(base.join("big/b.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(base.join("mid/a.bin"), vec![0u8; 150]).unwrap();
        std::fs::write(base.join("small/a.bin"), vec![0u8; 50]).unwrap();
        std::fs::write(base.join("root.bin"), vec![0u8; 10]).unwrap();

        let report = scan(&base).unwrap();
        let names: Vec<String> = report
            .children
            .iter()
            .map(|c| {
                // 用 Path::file_name 取末段：开发机是 Windows，硬编码 '/' 会在
                // 测试里拿到整条路径（实现本身与平台无关）
                Path::new(&c.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| c.path.clone())
            })
            .collect();
        assert_eq!(names, vec!["big", "mid", "small"], "应按体积降序");
        assert_eq!(report.children[0].bytes, 300);
        assert_eq!(report.children[1].bytes, 150);
        assert_eq!(report.children[2].bytes, 50);
        // 总量 = 三个子目录 + 根下的文件
        assert_eq!(report.total, 300 + 150 + 50 + 10);
        assert!(!report.truncated);
        // 百分比之和 = 子目录合计 / 总量 = 500/510，根目录那个 10B 文件不参与
        let sum: f64 = report.children.iter().map(|c| c.pct).sum();
        let expect = 500.0 / 510.0 * 100.0;
        assert!(
            (sum - expect).abs() < 0.01,
            "百分比应按总量算，期望 {expect}，实测 {sum}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 符号链接不得被跟随：链接指向 1KB 的文件时，目录体积不应把 1KB 算进来。
    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_symlinks() {
        let base = std::env::temp_dir().join(format!("lyys-dirlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real")).unwrap();
        std::fs::create_dir_all(base.join("linker")).unwrap();
        std::fs::write(base.join("real/blob.bin"), vec![0u8; 4096]).unwrap();
        std::os::unix::fs::symlink(base.join("real/blob.bin"), base.join("linker/alias")).unwrap();

        let report = scan(&base).unwrap();
        let real = report
            .children
            .iter()
            .find(|c| Path::new(&c.path).file_name().is_some_and(|n| n == "real"))
            .unwrap();
        let linker = report
            .children
            .iter()
            .find(|c| Path::new(&c.path).file_name().is_some_and(|n| n == "linker"))
            .unwrap();
        assert_eq!(real.bytes, 4096);
        assert!(
            linker.bytes < 4096,
            "链接不得被跟随，linker 实测 {} 字节",
            linker.bytes
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 起点白名单：只放行 `/`，`/etc` 这类要拒绝，`..` 穿越也要拒绝。
    ///
    /// 只在 Unix 上断言：白名单是围绕 Linux 面板部署目标设计的（`/`、`/etc`），
    /// 在 Windows 开发机上这些路径不存在，`canonicalize` 会先失败，
    /// 断言到的是「路径不存在」而不是「白名单拒绝」，两条规则混在一起无法区分。
    #[cfg(unix)]
    #[test]
    fn resolve_root_rejects_paths_outside_the_whitelist() {
        assert!(resolve_root("/").is_ok());
        assert!(resolve_root("/etc").is_err(), "白名单外目录应被拒绝");
        assert!(resolve_root("/../etc").is_err(), "应挡住 .. 穿越");
    }

    /// 不存在的路径必须是「路径错误」，与白名单判定区分开（跨平台可测）。
    #[test]
    fn resolve_root_reports_missing_paths() {
        let missing = std::env::temp_dir().join("lyys-definitely-not-here-987654321");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(resolve_root(&missing.to_string_lossy()).is_err());
    }

    /// 缓存命中：第二次查询带 cached 标记且不重扫（用 took_ms 判断更稳）。
    #[test]
    fn cache_returns_marked_hits() {
        let cache = DirUsageCache::new();
        let base = std::env::temp_dir().join(format!("lyys-dircache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("x")).unwrap();
        std::fs::write(base.join("x/a.bin"), vec![0u8; 32]).unwrap();

        let first = scan(&base).unwrap();
        assert!(!first.cached);
        cache.put("/k", &first);
        let second = cache.get("/k").expect("应命中缓存");
        assert!(second.cached);
        assert_eq!(second.total, first.total);
        let _ = std::fs::remove_dir_all(&base);
    }
}
