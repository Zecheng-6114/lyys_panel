use anyhow::Context;
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// 读取文件内容的最大字节数（1MB），超过则拒绝
const MAX_READ_SIZE: u64 = 1024 * 1024;
/// 下载的最大字节数（50MB）
const MAX_DOWNLOAD_SIZE: u64 = 50 * 1024 * 1024;
/// 二进制探测时读取的头部字节数
const PROBE_SIZE: usize = 8192;

/// 面板数据目录（panel.db / JWT 密钥文件 / 初始密码文件所在），进程启动时
/// 设置一次。该目录内的文件包含凭证等敏感数据，一律禁止经文件管理接口读写
/// （P0-2：防止已登录用户经面板自身功能「拖库提权」的纵深防御）。
static PROTECTED_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 设置受保护的数据目录（main.rs 启动时调用，需传入 canonicalize 后的绝对路径）
pub fn set_protected_dir(dir: PathBuf) {
    let _ = PROTECTED_DIR.set(dir);
}

/// 判断路径是否受保护：位于数据目录内，或本身是 SQLite 数据库文件
/// （`*.db` / `*.db-wal` / `*.db-shm`，后者是 SQLite 写前日志与共享内存伴生文件）。
fn is_protected(p: &Path) -> bool {
    if let Some(dir) = PROTECTED_DIR.get() {
        if p.starts_with(dir) {
            return true;
        }
    }
    let name = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.ends_with(".db") || name.ends_with(".db-wal") || name.ends_with(".db-shm")
}

/// 判断一段字节是否为文本内容：含 NUL 字节或不是合法 UTF-8 即视为二进制。
/// 二进制文件按文本读取再回写会损坏原文件，编辑前必须拦截。
fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// 探测已存在的文件是否为二进制（只读头部，避免整文件读入）
fn looks_binary(p: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    let mut buf = vec![0u8; PROBE_SIZE];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    is_binary(&buf[..n])
}

/// 单个目录项
#[derive(Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// 权限位（八进制字符串，如 "755"）
    pub mode: String,
    /// 修改时间（Unix 秒）
    pub mtime: i64,
}

/// 目录列表结果
#[derive(Serialize)]
pub struct DirListing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

/// 路径安全校验：必须是绝对路径，且不含 `..`。
///
/// `exists` 为 true 时 canonicalize 出真实路径再返回（这样后续读写跟随的是
/// 规范化后的位置，符号链接与 `..` 都已消解）；为 false 时用于新建/重命名
/// 目标等尚不存在的路径，此时只做词法校验并原样返回。
///
/// 额外拦截数据目录与数据库文件（P0-2），所有文件管理操作都以本函数为
/// 路径入口，一处拦截即全量生效。
fn resolve(path: &str, exists: bool) -> anyhow::Result<PathBuf> {
    if !path.starts_with('/') {
        anyhow::bail!("必须使用绝对路径");
    }
    if path.split('/').any(|seg| seg == "..") {
        anyhow::bail!("路径中不允许包含 ..");
    }
    let p = PathBuf::from(path);
    let p = if exists {
        p.canonicalize().context("路径不存在")?
    } else {
        p
    };
    if is_protected(&p) {
        anyhow::bail!("该路径属于面板数据目录或数据库文件，禁止通过文件接口访问");
    }
    Ok(p)
}

fn to_string_path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 权限位（八进制字符串，如 "755"）。
///
/// 权限位是 Unix 概念，Windows 没有对应物；这里回退为带只读标记的三位形式，
/// 让接口字段保持非空，同时也让本机（Windows）能够正常编译调试。
/// 面板的实际部署目标是 Debian，走的是上面的 Unix 分支。
#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata) -> String {
    format!("{:o}", meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(meta: &std::fs::Metadata) -> String {
    if meta.permissions().readonly() {
        "444".to_string()
    } else {
        "666".to_string()
    }
}

fn build_entry(e: &std::fs::DirEntry) -> Option<Entry> {
    let name = e.file_name().to_string_lossy().into_owned();
    // Unix 下 DirEntry::metadata() 不跟随符号链接（相当于 lstat）
    let sym = e.metadata().ok()?;
    let is_symlink = sym.is_symlink();
    // 链接目标信息（悬空链接回退到链接自身）
    let meta = if is_symlink {
        std::fs::metadata(e.path()).unwrap_or_else(|_| sym.clone())
    } else {
        sym.clone()
    };
    let full = to_string_path(&e.path());
    Some(Entry {
        name,
        path: full,
        is_dir: meta.is_dir(),
        is_symlink,
        size: meta.len(),
        mode: file_mode(&sym),
        mtime: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    })
}

/// 列出目录内容（目录在前，按名称排序）
pub async fn list_dir(path: &str) -> anyhow::Result<DirListing> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<DirListing> {
        let dir = resolve(&path, true)?;
        let mut entries: Vec<Entry> = Vec::new();
        let rd = std::fs::read_dir(&dir).context("读取目录失败")?;
        for e in rd {
            let e = e.context("读取目录项失败")?;
            if let Some(entry) = build_entry(&e) {
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| match (b.is_dir, a.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });
        let cur = to_string_path(&dir);
        let parent = if cur == "/" {
            None
        } else {
            Some(
                Path::new(&cur)
                    .parent()
                    .map(to_string_path)
                    .unwrap_or_else(|| "/".into()),
            )
        };
        Ok(DirListing {
            path: cur,
            parent,
            entries,
        })
    })
    .await
    .context("目录列表任务失败")?
}

/// 读取文本文件内容（限制大小，且拒绝二进制文件）
pub async fn read_file(path: &str) -> anyhow::Result<String> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let p = resolve(&path, true)?;
        let meta = std::fs::metadata(&p).context("读取文件信息失败")?;
        if !meta.is_file() {
            anyhow::bail!("目标不是普通文件");
        }
        if meta.len() > MAX_READ_SIZE {
            anyhow::bail!(
                "文件过大（{} 字节），仅支持编辑 {} 字节以内的文件",
                meta.len(),
                MAX_READ_SIZE
            );
        }
        let bytes = std::fs::read(&p).context("读取文件失败")?;
        // 二进制文件会以 lossy 方式变成乱码，保存后不可逆地损坏原文件，直接拒绝
        if is_binary(&bytes) {
            anyhow::bail!("目标疑似二进制文件，在线编辑会损坏内容，已拒绝读取");
        }
        String::from_utf8(bytes).context("文件内容不是合法的 UTF-8 文本")
    })
    .await
    .context("读取文件任务失败")?
}

/// 写入文本文件（覆盖）；目标已存在且为二进制时拒绝写入
pub async fn write_file(path: &str, content: &str) -> anyhow::Result<()> {
    let path = path.to_string();
    let content = content.to_string();
    tokio::task::spawn_blocking(move || {
        let p = resolve(&path, false)?;
        if p.exists() && !p.is_file() {
            anyhow::bail!("目标已存在且不是普通文件");
        }
        // 覆盖已存在的二进制文件同样会损坏它（例如绕过界面直接调用接口）
        if p.exists() && looks_binary(&p) {
            anyhow::bail!("目标疑似二进制文件，拒绝覆盖写入");
        }
        // P1-3：错误消息不回显路径/errno，完整原因进 tracing 日志（见 ApiError）
        std::fs::write(&p, content).context("写入文件失败")?;
        Ok(())
    })
    .await
    .context("写入文件任务失败")?
}

/// 创建目录
pub async fn mkdir(path: &str) -> anyhow::Result<()> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        let p = resolve(&path, false)?;
        std::fs::create_dir(&p).context("创建目录失败")?;
        Ok(())
    })
    .await
    .context("创建目录任务失败")?
}

/// 删除文件或目录（目录递归删除）
pub async fn remove(path: &str) -> anyhow::Result<()> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        let p = resolve(&path, true)?;
        if p == Path::new("/") {
            anyhow::bail!("拒绝删除根目录");
        }
        let meta = std::fs::symlink_metadata(&p).context("读取目标失败")?;
        if meta.is_dir() && !meta.is_symlink() {
            std::fs::remove_dir_all(&p).context("删除目录失败")?;
        } else {
            std::fs::remove_file(&p).context("删除文件失败")?;
        }
        Ok(())
    })
    .await
    .context("删除任务失败")?
}

/// 重命名 / 移动（from、to 均为绝对路径）
pub async fn rename(from: &str, to: &str) -> anyhow::Result<()> {
    let from = from.to_string();
    let to = to.to_string();
    tokio::task::spawn_blocking(move || {
        let src = resolve(&from, true)?;
        let dst = resolve(&to, false)?;
        if dst.exists() {
            anyhow::bail!("目标已存在");
        }
        std::fs::rename(&src, &dst).context("重命名失败")?;
        Ok(())
    })
    .await
    .context("重命名任务失败")?
}

/// 下载文件：返回 (文件名, 字节)
pub async fn download(path: &str) -> anyhow::Result<(String, Vec<u8>)> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<(String, Vec<u8>)> {
        let p = resolve(&path, true)?;
        let meta = std::fs::metadata(&p).context("读取文件信息失败")?;
        if !meta.is_file() {
            anyhow::bail!("只能下载普通文件");
        }
        if meta.len() > MAX_DOWNLOAD_SIZE {
            anyhow::bail!("文件过大，无法下载");
        }
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".into());
        let bytes = std::fs::read(&p).context("读取文件失败")?;
        Ok((name, bytes))
    })
    .await
    .context("下载任务失败")?
}

/// 在 dir 下保存上传的文件（重名自动加数字后缀），返回最终路径
pub async fn save_upload(dir: &str, filename: &str, bytes: Vec<u8>) -> anyhow::Result<String> {
    let dir = dir.to_string();
    let filename = filename.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let d = resolve(&dir, true)?;
        // 文件名只取最后一截，防止携带路径
        let safe_name = Path::new(&filename)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty() && s != "." && s != "..")
            .context("非法文件名")?;
        let mut target = d.join(&safe_name);
        if target.exists() {
            let stem = Path::new(&safe_name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| safe_name.clone());
            let ext = Path::new(&safe_name)
                .extension()
                .map(|s| format!(".{}", s.to_string_lossy()))
                .unwrap_or_default();
            for i in 1..1000 {
                let cand = d.join(format!("{stem}({i}){ext}"));
                if !cand.exists() {
                    target = cand;
                    break;
                }
            }
        }
        // P0-2：目标目录虽已校验，但文件名本身仍可能是 *.db（含 WAL/SHM 伴生文件），
        // 最终落点单独再拦一道，防止上传创建数据库文件
        if is_protected(&target) {
            anyhow::bail!("不允许上传为数据库文件（.db）");
        }
        std::fs::write(&target, bytes).context("保存上传文件失败")?;
        Ok(to_string_path(&target))
    })
    .await
    .context("上传任务失败")?
}
