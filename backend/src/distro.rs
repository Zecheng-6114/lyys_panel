use std::sync::OnceLock;

/// 发行版家族：v1 仅支持 Debian 系与 Arch 系
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Debian,
    Arch,
}

/// 启动时的检测结果
pub struct Distro {
    pub family: Family,
    /// /etc/os-release 的 PRETTY_NAME，如 "Debian GNU/Linux 13 (trixie)"
    pub pretty: String,
}

static CURRENT: OnceLock<Distro> = OnceLock::new();

/// 读取 /etc/os-release 的 KEY=VALUE 字段（去除引号）
fn field(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(v) = rest.strip_prefix('=') {
                let v = v.trim().trim_matches('"').to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// 按 ID / ID_LIKE 归类家族。ID_LIKE 表达继承关系（如 ubuntu 的
/// ID_LIKE="debian"、manjaro 的 ID_LIKE="arch"），派生版靠它归类，不必逐个枚举。
/// 返回 None 表示不支持。Debian 判定优先：个别发行版 ID_LIKE 同时含 debian 与别的词。
fn classify(id: &str, like: &str) -> Option<Family> {
    let joined = format!("{id} {like}");
    let tokens: Vec<&str> = joined.split_whitespace().collect();
    if tokens.contains(&"debian") || tokens.contains(&"ubuntu") {
        Some(Family::Debian)
    } else if tokens.contains(&"arch") {
        Some(Family::Arch)
    } else {
        None
    }
}

/// 检测当前发行版并写入全局。不支持的发行版返回 Err，
/// main 据此打印错误并退出；后续各模块通过 family() 分派包管理命令。
pub fn init() -> Result<(), String> {
    let content =
        std::fs::read_to_string("/etc/os-release").map_err(|e| format!("无法读取 /etc/os-release：{e}"))?;
    let id = field(&content, "ID").unwrap_or_default();
    let like = field(&content, "ID_LIKE").unwrap_or_default();
    let pretty = field(&content, "PRETTY_NAME")
        .unwrap_or(if id.is_empty() { "未知系统".to_string() } else { id.clone() });
    let family = classify(&id, &like).ok_or_else(|| {
        format!("当前系统 {pretty} 暂不支持（目前支持 Debian/Ubuntu 系与 Arch 系），服务停止启动")
    })?;
    let _ = CURRENT.set(Distro { family, pretty });
    Ok(())
}

/// 启动时检测到的发行版家族（main 保证 init 先于一切请求处理）
pub fn family() -> Family {
    CURRENT
        .get()
        .map(|d| d.family)
        .expect("distro::init 必须在启动时先行调用")
}

/// 发行版描述（PRETTY_NAME），用于启动日志
pub fn pretty() -> &'static str {
    CURRENT.get().map(|d| d.pretty.as_str()).unwrap_or("未知系统")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debian_family() {
        assert_eq!(classify("debian", ""), Some(Family::Debian));
        // ubuntu 通常 ID_LIKE="debian"
        assert_eq!(classify("ubuntu", "debian"), Some(Family::Debian));
        // 派生版：ID 不在支持列表，但 ID_LIKE 含 debian
        assert_eq!(classify("linuxmint", "ubuntu debian"), Some(Family::Debian));
        assert_eq!(classify("pop", "ubuntu debian"), Some(Family::Debian));
    }

    #[test]
    fn arch_family() {
        assert_eq!(classify("arch", ""), Some(Family::Arch));
        // manjaro ID_LIKE="arch"
        assert_eq!(classify("manjaro", "arch"), Some(Family::Arch));
    }

    #[test]
    fn unsupported() {
        // RHEL 系、openSUSE、Alpine 均不支持
        assert_eq!(classify("fedora", "rhel centos fedora"), None);
        assert_eq!(classify("centos", "rhel fedora"), None);
        assert_eq!(classify("opensuse-leap", "suse"), None);
        assert_eq!(classify("alpine", ""), None);
        assert_eq!(classify("", ""), None);
    }

    #[test]
    fn field_parsing() {
        let content = "NAME=\"Debian GNU/Linux\"\nID=debian\nID_LIKE=\"\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\n";
        assert_eq!(field(content, "ID").as_deref(), Some("debian"));
        assert_eq!(field(content, "PRETTY_NAME").as_deref(), Some("Debian GNU/Linux 13 (trixie)"));
        // 空值应视为 None
        assert_eq!(field(content, "ID_LIKE"), None);
        // 不存在的键
        assert_eq!(field(content, "HOME_URL"), None);
    }
}
