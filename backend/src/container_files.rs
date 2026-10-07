//! 容器内的文件浏览（只读）。
//!
//! 走 `docker exec` 而不是让用户配宿主机挂载：容器未必有可访问的宿主机路径，
//! 而 `docker exec` 对运行中的容器总是可用。
//!
//! 只做只读。容器内写操作要同时应付挂载卷、只读层和镜像内用户权限三种情况，
//! 收益不足以抵消风险——这一层的用途是「看看里面有什么」。

use anyhow::{ensure, Result};
use tokio::process::Command;

use crate::files::{DirListing, Entry};

/// 单次列目录 / 读文件的超时（秒）。容器内的 ls、cat 都是轻操作。
const FS_TIMEOUT: u64 = 20;

/// 容器内路径校验：必须绝对、不得含 `..`、不得含 NUL。
///
/// `docker exec` 的参数不经过 shell，所以没有命令注入面；但路径穿越仍要把住——
/// 面板用户不应借它跳出容器去看挂载进来的宿主机目录。
fn check_path(path: &str) -> Result<String> {
    ensure!(!path.is_empty(), "路径不能为空");
    ensure!(path.starts_with('/'), "必须使用绝对路径");
    // NUL 会让命令参数的构造直接失败，拦在最前面比让它变成运行期错误清楚
    ensure!(!path.contains('\0'), "路径含非法字符");
    ensure!(!path.split('/').any(|seg| seg == ".."), "路径不得包含 ..");

    // 归一化：折叠重复斜杠与末尾斜杠、丢弃 "." 段，
    // 让展示的路径与随后 ls 出来的条目路径对得上
    let mut norm = String::with_capacity(path.len());
    for seg in path.split('/').filter(|s| !s.is_empty() && *s != ".") {
        norm.push('/');
        norm.push_str(seg);
    }
    if norm.is_empty() {
        norm.push('/');
    }
    Ok(norm)
}

/// 取出剩余文本里的下一个空白分隔字段，并把游标推到字段之后
fn take_field<'a>(rest: &mut &'a str) -> Option<&'a str> {
    let start = rest.find(|c: char| !c.is_whitespace())?;
    *rest = &rest[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let field = &rest[..end];
    *rest = &rest[end..];
    Some(field)
}

/// 一行 `ls -lA` 解析出的各字段
struct LsLine<'a> {
    perms: &'a str,
    size: u64,
    mtime: i64,
    name: String,
    owner: String,
    group: String,
}

/// 解析一行 `ls -lA` 输出。
///
/// `epoch` 为真表示命令带了 `--time-style=+%s`（GNU coreutils），时间占一个字段且
/// 是 Unix 秒；为假则按 POSIX 三段式（月 日 时间）跳过，此时拿不到秒数，返回 0。
///
/// 按字段个数而不是固定字符位置解析：`ls -l` 的列宽随内容浮动，靠位置切会错位。
fn parse_ls_line(line: &str, epoch: bool) -> Option<LsLine<'_>> {
    let mut rest = line.trim_end();

    let perms = take_field(&mut rest)?;
    // 权限串固定 10 位；`total 12` 之类的行首个字段不是权限串，据此跳过
    if perms.len() < 10 || !perms.is_ascii() {
        return None;
    }

    let _links = take_field(&mut rest)?;
    // 属主 / 属组照原样透出：容器里的 ls 已经给了名字，uid 没有对应名字时是数字
    let owner = take_field(&mut rest)?.to_string();
    let group = take_field(&mut rest)?.to_string();
    let size: u64 = take_field(&mut rest).and_then(|s| s.parse().ok()).unwrap_or(0);

    let mtime: i64 = if epoch {
        take_field(&mut rest)?.parse().unwrap_or(0)
    } else {
        let _month = take_field(&mut rest)?;
        let _day = take_field(&mut rest)?;
        let _time = take_field(&mut rest)?;
        0
    };

    let name = rest.trim_start();
    if name.is_empty() {
        return None;
    }
    // 符号链接显示为 `名字 -> 目标`，面板只呈现名字本身
    let name = name.split(" -> ").next().unwrap_or(name).to_string();

    Some(LsLine {
        perms,
        size,
        mtime,
        name,
        owner,
        group,
    })
}

/// `drwxr-xr-x` → `"755"`：三组 rwx 分别按位求和，与宿主机文件页的八进制表示一致
fn mode_from_perms(perms: &str) -> String {
    let p: Vec<char> = perms.chars().collect();
    if p.len() < 10 {
        return String::new();
    }
    let bits = |start: usize| -> u32 {
        let mut v = 0;
        for i in 0..3 {
            if p[start + i] != '-' {
                v |= 1 << (2 - i);
            }
        }
        v
    };
    format!("{}{}{}", bits(1), bits(4), bits(7))
}

/// 拼容器内路径
fn join(base: &str, name: &str) -> String {
    if base == "/" {
        format!("/{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// 求父目录，根目录没有父级
fn parent_of(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    Some(match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => path[..i].to_string(),
    })
}

/// 解析整段 `ls -lA` 输出
fn parse_ls_output(text: &str, epoch: bool, base: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = text
        .lines()
        .filter_map(|line| parse_ls_line(line, epoch))
        .map(|l| Entry {
            path: join(base, &l.name),
            is_dir: l.perms.starts_with('d'),
            is_symlink: l.perms.starts_with('l'),
            mode: mode_from_perms(l.perms),
            size: l.size,
            owner: l.owner,
            group: l.group,
            mtime: l.mtime,
            name: l.name,
        })
        .collect();

    crate::files::sort_entries(&mut entries);
    entries
}

/// 在容器内执行 `ls -lA`。
///
/// 优先用 GNU 的 `--time-style=+%s` 取 Unix 秒；busybox（Alpine 等）的 ls 不认这个
/// 选项，退回 POSIX 格式，此时修改时间取不到。
async fn run_ls(container: &str, path: &str) -> Result<(String, bool)> {
    let mut cmd = Command::new("docker");
    cmd.args(["exec", container, "ls", "-lA", "--time-style=+%s", path]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(FS_TIMEOUT)).await?;
    if out.status.success() {
        return Ok((String::from_utf8_lossy(&out.stdout).into_owned(), true));
    }

    let mut cmd = Command::new("docker");
    cmd.args(["exec", container, "ls", "-lA", path]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(FS_TIMEOUT)).await?;
    if !out.status.success() {
        // 两次都失败时带上第二次的 stderr：路径不存在是最常见的原因，得让用户看见
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!("读取容器目录失败：{err}");
    }
    Ok((String::from_utf8_lossy(&out.stdout).into_owned(), false))
}

/// 列出容器内某个目录
pub async fn list_dir(container: &str, path: &str) -> Result<DirListing> {
    let path = check_path(path)?;
    let (text, epoch) = run_ls(container, &path).await?;
    Ok(DirListing {
        parent: parent_of(&path),
        entries: parse_ls_output(&text, epoch, &path),
        path,
    })
}

/// 读取容器内的文本文件
pub async fn read_file(container: &str, path: &str) -> Result<String> {
    let path = check_path(path)?;
    let mut cmd = Command::new("docker");
    cmd.args(["exec", container, "cat", &path]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(FS_TIMEOUT)).await?;
    ensure!(
        out.status.success(),
        "读取容器文件失败：{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // 二进制内容塞进文本编辑器没有意义，而且会把页面拖死
    ensure!(!text.contains('\0'), "该文件不是文本文件，无法在面板中打开");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gnu_epoch_lines() {
        let l = parse_ls_line("drwxr-xr-x 2 root root 4096 1759393200 bin", true)
            .expect("应解析成功");
        assert_eq!(l.perms, "drwxr-xr-x");
        assert_eq!(l.size, 4096);
        assert_eq!(l.mtime, 1_759_393_200);
        assert_eq!(l.name, "bin");
        assert_eq!(l.owner, "root");
        assert_eq!(l.group, "root");
    }

    #[test]
    fn parses_posix_lines_without_mtime() {
        // busybox 的 ls 不认 --time-style，时间占三段且拿不到 Unix 秒
        let l = parse_ls_line("-rw-r--r-- 1 root root 123 Oct  2 11:00 notes.txt", false)
            .expect("应解析成功");
        assert_eq!(l.perms, "-rw-r--r--");
        assert_eq!(l.size, 123);
        assert_eq!(l.mtime, 0, "取不到时间时置 0，由前端显示为未知");
        assert_eq!(l.name, "notes.txt");
        assert_eq!(l.owner, "root");
        assert_eq!(l.group, "root");
    }

    #[test]
    fn keeps_spaces_in_names_and_strips_symlink_target() {
        let l = parse_ls_line("lrwxrwxrwx 1 root root 7 Oct  2 11:00 my link -> /usr/bin/x", false)
            .expect("应解析成功");
        assert_eq!(l.perms, "lrwxrwxrwx");
        assert_eq!(l.name, "my link", "文件名里的空格要保留，`-> 目标` 要剥掉");
    }

    #[test]
    fn ignores_total_and_truncated_lines() {
        assert!(parse_ls_line("total 12", true).is_none());
        assert!(parse_ls_line("", true).is_none());
        assert!(
            parse_ls_line("drwxr-xr-x 2 root root", true).is_none(),
            "字段不足的行不能解析出条目"
        );
    }

    #[test]
    fn perms_become_octal_mode() {
        assert_eq!(mode_from_perms("drwxr-xr-x"), "755");
        assert_eq!(mode_from_perms("-rw-r--r--"), "644");
        assert_eq!(mode_from_perms("lrwxrwxrwx"), "777");
        assert_eq!(mode_from_perms("-rw-------"), "600");
    }

    #[test]
    fn path_check_rejects_traversal_and_normalises() {
        assert_eq!(check_path("/etc").unwrap(), "/etc");
        assert_eq!(check_path("//etc///").unwrap(), "/etc");
        assert_eq!(check_path("/").unwrap(), "/");
        assert!(check_path("etc").is_err(), "必须是绝对路径");
        assert!(check_path("/etc/../root").is_err(), ".. 必须拒绝");
        assert!(check_path("/etc/\0x").is_err(), "NUL 必须拒绝");
    }

    #[test]
    fn entries_use_the_same_order_as_the_host_file_page() {
        let text = "\
-rw-r--r-- 1 root root 10 1759393201 zeta.txt
drwxr-xr-x 2 root root 4096 1759393200 bin
total 8";
        let entries = parse_ls_output(text, true, "/");
        assert_eq!(entries.len(), 2, "total 行不成条目");
        // 复用 files::sort_entries，顺序必须与宿主机文件页一致
        assert_eq!(entries[0].name, "zeta.txt");
        assert_eq!(entries[0].path, "/zeta.txt");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[1].name, "bin");
        assert!(entries[1].is_dir);
        assert_eq!(entries[1].path, "/bin");
    }
}
