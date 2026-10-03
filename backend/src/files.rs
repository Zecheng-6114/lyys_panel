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
    if let Some(dir) = PROTECTED_DIR.get()
        && p.starts_with(dir) {
            return true;
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

/// 目录列表排序：非目录在前，同类按名称升序。
///
/// 容器文件浏览（`container_files`）复用本函数 —— 两种来源会在同一个页面里切换，
/// 各排各的会让人以为列表跳了。
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| match (b.is_dir, a.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
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
        sort_entries(&mut entries);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 在临时目录建一个唯一命名的沙盒目录，返回路径；调用方负责清理
    fn temp_sandbox(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "lyys_files_test_{tag}_{}_{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// resolve（纯同步入口校验）：非绝对路径与 `..` 穿越（含各种变体）一律拒绝
    #[test]
    fn resolve_rejects_relative_and_traversal() {
        // 非绝对路径
        assert!(resolve("etc/passwd", false).is_err());
        assert!(resolve("./relative", false).is_err());
        // `..` 作为独立路径段的穿越变体（词法层拦截，不依赖路径是否存在）
        for p in [
            "/etc/../etc/passwd",
            "/etc/passwd/../../etc/shadow",
            "/..",
            "/etc/..",
            "/tmp/x/../../..",
        ] {
            assert!(resolve(p, false).is_err(), "应拒绝穿越路径：{p}");
        }
        // 合法绝对路径放行（exists=false 只做词法校验）
        assert!(resolve("/tmp/lyys-should-pass", false).is_ok());
    }

    /// is_protected（纯函数，不依赖全局）：任意位置的 SQLite 数据库及
    /// WAL/SHM 伴生文件都被标记保护，大小写不敏感；普通文件不误伤
    #[test]
    fn db_file_suffixes_are_protected_anywhere() {
        for p in [
            "/tmp/panel.db",
            "/tmp/panel.db-wal",
            "/tmp/panel.db-shm",
            "/var/lib/anything/other.sqlite.db",
            "/tmp/UPPER.DB",
            "/tmp/x.DB-WAL",
        ] {
            assert!(is_protected(Path::new(p)), "应保护：{p}");
        }
        for p in ["/tmp/notes.txt", "/tmp/data.sqlite", "/tmp/x.db.bak"] {
            assert!(!is_protected(Path::new(p)), "不应误伤：{p}");
        }
        // 注意：隐藏文件 ".db" 按现有实现同样命中 ends_with(".db") 规则
        assert!(is_protected(Path::new("/tmp/.db")));
    }

    /// 路径转字符串（测试内统一用 canonicalize 前的原始路径拼字符串）
    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// 受保护数据目录集成测试：`set_protected_dir` 写入进程级 OnceLock 全局，
    /// 一旦设置无法撤销，因此目录内文件读写列目录删改上传的**全部**断言收敛在
    /// 本测试内一次完成（串行友好设计，避免污染其他测试）。
    #[tokio::test]
    async fn protected_dir_blocks_all_operations() {
        let sandbox = temp_sandbox("prot");
        let data_dir = sandbox.join("panel_data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("panel.db"), b"secret").unwrap();
        std::fs::write(data_dir.join("jwt_secret.key"), b"key").unwrap();
        std::fs::create_dir_all(data_dir.join("sub")).unwrap();
        // 数据目录之外、但同名规则的 db 文件（验证后缀拦截独立于目录）
        std::fs::write(sandbox.join("loose.db"), b"db").unwrap();
        // 正常可访问的对照文件
        std::fs::write(sandbox.join("ok.txt"), b"hello").unwrap();

        let sdir = data_dir.canonicalize().unwrap();
        set_protected_dir(sdir.clone());

        // 数据目录本身与目录内文件：列目录/读/下载/删除（exists=true 路径）全拒
        for p in [&sdir, &sdir.join("panel.db"), &sdir.join("jwt_secret.key")] {
            let ps = s(p);
            assert!(list_dir(&ps).await.is_err(), "应禁止列目录：{ps}");
            assert!(read_file(&ps).await.is_err(), "应禁止读取：{ps}");
            assert!(download(&ps).await.is_err(), "应禁止下载：{ps}");
            assert!(remove(&ps).await.is_err(), "应禁止删除：{ps}");
        }
        // 子目录同样在保护范围内（starts_with 前缀匹配）
        assert!(list_dir(&s(&sdir.join("sub"))).await.is_err());

        // 写/建目录/重命名（exists=false 路径，词法校验即拦）：目标在数据目录内全拒
        assert!(write_file(&s(&sdir.join("evil.txt")), "x").await.is_err());
        assert!(mkdir(&s(&sdir.join("evil_dir"))).await.is_err());
        assert!(
            rename(&s(&sdir.join("panel.db")), &s(&sdir.join("stolen.db")))
                .await
                .is_err()
        );
        // 从数据目录向外改名同样被拒（源路径校验）
        assert!(
            rename(&s(&sdir.join("panel.db")), &s(&sandbox.join("out.txt")))
                .await
                .is_err()
        );
        // 向数据目录上传同样被拒
        assert!(save_upload(&s(&sdir), "payload.txt", b"x".to_vec())
            .await
            .is_err());

        // 目录外的 *.db / -wal / -shm：读写列目录下载全拦
        for name in ["loose.db", "loose.db-wal", "loose.db-shm"] {
            let p = sandbox.join(name);
            std::fs::write(&p, b"x").unwrap();
            let ps = s(&p);
            assert!(read_file(&ps).await.is_err(), "应禁止读 db 伴生文件：{ps}");
            assert!(download(&ps).await.is_err(), "应禁止下载 db 伴生文件：{ps}");
            assert!(
                write_file(&ps, "y").await.is_err(),
                "应禁止写 db 伴生文件：{ps}"
            );
        }
        // 把目录本身命名为 *.db：连列目录都不行
        let dbdir = sandbox.join("mydb.db");
        std::fs::create_dir_all(&dbdir).unwrap();
        assert!(list_dir(&s(&dbdir)).await.is_err());

        // 上传文件名伪装成 *.db：最终落点单独拦截
        let target = save_upload(&s(&sandbox), "evil.txt.db", b"x".to_vec()).await;
        assert!(target.is_err(), "禁止上传为数据库文件");
        assert!(!sandbox.join("evil.txt.db").exists());

        // 对照：沙盒内普通文件仍可正常读（保护规则没有一刀切）
        assert!(read_file(&s(&sandbox.join("ok.txt"))).await.is_ok());
        assert!(list_dir(&s(&sandbox)).await.is_ok());

        let _ = std::fs::remove_dir_all(&sandbox);
    }

    /// 路径穿越的端到端验证：即使拼接出指向数据目录的穿越路径，也在
    /// resolve 词法校验一步被拒（`..` 段先于 canonicalize 拦截）
    #[tokio::test]
    async fn traversal_cannot_reach_protected_dir() {
        let sandbox = temp_sandbox("trav");
        let secret_file = sandbox.join("panel_data/panel.db");
        std::fs::create_dir_all(sandbox.join("panel_data")).unwrap();
        std::fs::write(&secret_file, b"top secret").unwrap();
        std::fs::create_dir_all(sandbox.join("pub")).unwrap();

        // 从公开目录出发，用 .. 拼出数据目录里的 db 路径
        let sneaky = format!("{}/pub/../../panel_data/panel.db", s(&sandbox));
        assert!(read_file(&sneaky).await.is_err());
        assert!(download(&sneaky).await.is_err());
        assert!(remove(&sneaky).await.is_err());
        // 绝对路径直接指到数据目录同样被词法校验拒（含 .. 段）
        assert!(read_file("/tmp/../etc/passwd").await.is_err());
        // 不含 .. 的绝对路径逃逸（直接读 /etc/shadow）不在 resolve 的职责内
        // （面板以 root 运行、无根目录限制是既有设计），此处仅确认 db 后缀兜底
        assert!(is_protected(Path::new("/var/lib/panel/panel.db")));

        let _ = std::fs::remove_dir_all(&sandbox);
    }
}
