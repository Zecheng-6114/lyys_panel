//! 备份远端投递：把本地备份复制到 WebDAV 异地存储。
//!
//! 只做 PUT，不列目录（列目录要 PROPFIND + XML 解析，收益不抵复杂度）；需要读回时
//! 由管理员从远端下载文件，再用「上传导入」还原（加密副本则填解密口令）。上传由调用
//! 方在备份成功后触发，失败只记日志，绝不影响本地备份本身。
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::backup;

/// 把指定本地备份投递到远端。`Ok(true)` 表示已上传，`Ok(false)` 表示未启用远端。
pub async fn upload_backup(data_dir: &Path, name: &str) -> Result<bool> {
    let cfg = backup::load_config(data_dir).remote;
    if !cfg.enabled {
        return Ok(false);
    }
    let path = backup::resolve_backup(data_dir, name)?;
    let raw = tokio::fs::read(&path)
        .await
        .with_context(|| format!("读取备份失败：{}", path.display()))?;
    // 启用加密时远端只留密文，扩展名标成 .enc 便于识别
    let (target_name, payload) = if cfg.encrypt {
        let blob = backup::encrypt_bytes(&cfg.passphrase, &raw)?;
        (format!("{name}.enc"), blob)
    } else {
        (name.to_string(), raw)
    };
    // 大库上传给足时间；连接层失败由 reqwest 自己退避重试（默认 0 次，靠超时兜底）
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .context("构建上传客户端失败")?;
    put(
        &client,
        &cfg.url,
        &cfg.username,
        &cfg.password,
        &target_name,
        payload,
    )
    .await?;
    Ok(true)
}

/// WebDAV PUT：用户名非空时带 Basic 认证
async fn put(
    client: &reqwest::Client,
    base: &str,
    user: &str,
    pass: &str,
    name: &str,
    bytes: Vec<u8>,
) -> Result<()> {
    let target = join_url(base, name);
    let mut req = client.put(&target).body(bytes);
    if !user.is_empty() {
        req = req.basic_auth(user, Some(pass));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("连接 WebDAV 失败：{target}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "WebDAV 返回 {status}：{}",
            body.chars().take(200).collect::<String>()
        );
    }
    Ok(())
}

/// 拼接目标地址：基址去掉尾部斜杠再拼文件名（文件名是白名单 ASCII，无需转义）
fn join_url(base: &str, name: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), name)
}

#[cfg(test)]
mod tests {
    use super::join_url;

    #[test]
    fn joins_base_and_name() {
        assert_eq!(join_url("https://dav/x", "a.db"), "https://dav/x/a.db");
        assert_eq!(join_url("https://dav/x/", "a.db"), "https://dav/x/a.db");
        assert_eq!(
            join_url("https://dav/x///", "a.db.enc"),
            "https://dav/x/a.db.enc"
        );
    }
}
