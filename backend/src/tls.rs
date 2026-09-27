// 1.3 HTTPS 支持：TLS 模式解析与自签证书生成。
//
// 三种模式（env PANEL_TLS，默认 auto）：
// - auto   ：使用 <data_dir>/tls/{cert,key}.pem，首启自动生成自签证书；
// - custom ：使用 PANEL_TLS_CERT / PANEL_TLS_KEY 指定的已有证书；
// - off    ：不启用 TLS，行为与旧版一致（纯 HTTP）。
//
// 局域网自部署场景下自签证书是主要用法；公网场景配 custom 挂 CA 证书。

use anyhow::{Context, Result};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq)]
pub enum TlsMode {
    Auto,
    Custom,
}

pub struct TlsSettings {
    pub mode: TlsMode,
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// 从环境变量解析 TLS 配置；返回 None 表示关闭 TLS（PANEL_TLS=off）
pub fn settings_from_env(data_dir: &Path) -> Result<Option<TlsSettings>> {
    match std::env::var("PANEL_TLS")
        .unwrap_or_else(|_| "auto".into())
        .as_str()
    {
        "off" | "0" | "false" => Ok(None),
        "custom" => {
            let cert = std::env::var("PANEL_TLS_CERT")
                .context("PANEL_TLS=custom 需要同时设置 PANEL_TLS_CERT")?;
            let key = std::env::var("PANEL_TLS_KEY")
                .context("PANEL_TLS=custom 需要同时设置 PANEL_TLS_KEY")?;
            Ok(Some(TlsSettings {
                mode: TlsMode::Custom,
                cert: cert.into(),
                key: key.into(),
            }))
        }
        _ => {
            let dir = data_dir.join("tls");
            Ok(Some(TlsSettings {
                mode: TlsMode::Auto,
                cert: dir.join("cert.pem"),
                key: dir.join("key.pem"),
            }))
        }
    }
}

/// 自签证书有效期：10 年。局域网面板的自签证书没有轮换机制，
/// 短有效期意味着到期后浏览器直接拒连且无人续期，长比短更不坏。
const SELF_SIGNED_YEARS: i64 = 10;

/// 确保自签证书存在（不存在则生成）。SAN 覆盖 localhost、回环地址、
/// 本机主机名与常见内网网段主机名不便枚举——浏览器反正要手动信任，
/// SAN 齐全只为减少一层警告。
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<()> {
    if cert_path.exists() && key_path.exists() {
        return Ok(());
    }
    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent).context("创建 TLS 证书目录失败")?;
    }

    let now = time::OffsetDateTime::now_utc();
    let mut params = rcgen::CertificateParams::new(vec![
        "localhost".to_string(),
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST).to_string(),
        IpAddr::V6(std::net::Ipv6Addr::LOCALHOST).to_string(),
    ])
    .context("生成自签证书参数失败")?;
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(365 * SELF_SIGNED_YEARS);
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String("LYYS Panel".to_string()),
    );

    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("生成 TLS 私钥失败")?;
    let cert = params.self_signed(&key_pair).context("生成自签证书失败")?;
    std::fs::write(key_path, key_pair.serialize_pem()).context("写入 TLS 私钥失败")?;
    // 私钥 0600：与 JWT 密钥文件同级保护
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
            .context("设置 TLS 私钥权限失败")?;
    }
    std::fs::write(cert_path, cert.pem()).context("写入 TLS 证书失败")?;
    tracing::info!("已生成自签 TLS 证书：{}", cert_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自签生成：产出可解析的 PEM 对，二次调用幂等（不覆盖已有文件）
    #[test]
    fn self_signed_generated_once_and_stable() {
        let dir = std::env::temp_dir().join(format!(
            "lyys_tls_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        let cert_pem = std::fs::read_to_string(&cert).unwrap();
        let key_pem = std::fs::read_to_string(&key).unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN"));
        // 二次调用不得改动已有证书
        ensure_self_signed(&cert, &key).unwrap();
        assert_eq!(std::fs::read_to_string(&cert).unwrap(), cert_pem);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
