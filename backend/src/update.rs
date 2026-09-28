// 3.2 面板自更新
//
// 更新源是 GitHub Release（Zecheng-6114/lyys_panel），二进制资产名固定为
// lyys-panel。流程：检查 → 下载校验 → 替换 → 重启。
//
// 替换采用「先落临时文件再原子 rename」：目标运行中时 rename 会失败
// （ETXTBSY），这正是我们要的安全闸——只有服务已停或二进制不在运行路径
// 上时才会替换成功。下载后做体积下限校验（release 产物远大于 1MB），
// 防止把 GitHub 的 HTML 错误页当成二进制装上。
//
// 除 GitHub 检查外，还提供「手动上传」通道：内网环境可能访问不了 GitHub，
// 管理员可在有网机器构建后上传替换（同样走临时文件 + rename）。
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 当前版本 = Cargo.toml 的 version（编译期注入）
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 更新源仓库（发布二进制资产到此 Release 才可被自更新拉取）
const REPO_OWNER: &str = "Zecheng-6114";
const REPO_NAME: &str = "lyys_panel";
const ASSET_NAME: &str = "lyys-panel";

/// 二进制体积下限：release 产物约 10MB+，小于 1MB 必是错误页/占位文件
const MIN_BINARY_SIZE: u64 = 1024 * 1024;

#[derive(Serialize)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub has_update: bool,
    /// 检查失败时的说明（离线/限流等），None = 检查成功
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
}

/// 去掉 tag 前缀 v 做纯版本号比较（点分段数字比较，缺段补 0）
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let segs = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (x, y) = (segs(a), segs(b));
    let n = x.len().max(y.len());
    for i in 0..n {
        let (xi, yi) = (
            x.get(i).copied().unwrap_or(0),
            y.get(i).copied().unwrap_or(0),
        );
        if xi != yi {
            return xi.cmp(&yi);
        }
    }
    std::cmp::Ordering::Equal
}

/// 检查 GitHub 是否有新版本（不下载）
pub async fn check(client: &reqwest::Client) -> Result<UpdateStatus> {
    let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let resp = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, "lyys-panel-updater")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("请求 GitHub API 失败")?;
    let status = resp.status();
    if !status.is_success() {
        // 404 = 还没有发过 release，属正常状态而非错误
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(UpdateStatus {
                current: CURRENT_VERSION.into(),
                latest: None,
                has_update: false,
                error: None,
            });
        }
        bail!("GitHub API 返回 {status}");
    }
    let release: ReleaseJson = resp.json().await.context("解析 GitHub 响应失败")?;
    let has_update = version_cmp(&release.tag_name, CURRENT_VERSION) == std::cmp::Ordering::Greater;
    Ok(UpdateStatus {
        current: CURRENT_VERSION.into(),
        latest: Some(release.tag_name),
        has_update,
        error: None,
    })
}

/// 下载 GitHub 最新版的 lyys-panel 资产。返回 (数据, 来源标签)。
pub async fn download_github(client: &reqwest::Client) -> Result<(Vec<u8>, String)> {
    let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let release: ReleaseJson = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, "lyys-panel-updater")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("请求 GitHub API 失败")?
        .error_for_status()
        .context("GitHub API 返回错误状态")?
        .json()
        .await
        .context("解析 GitHub 响应失败")?;
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == ASSET_NAME)
        .ok_or_else(|| anyhow::anyhow!("最新 release 中没有 {ASSET_NAME} 二进制资产"))?;
    let bytes = client
        .get(&asset.browser_download_url)
        .header(reqwest::header::USER_AGENT, "lyys-panel-updater")
        .send()
        .await
        .context("下载二进制失败")?
        .error_for_status()
        .context("下载二进制返回错误状态")?
        .bytes()
        .await
        .context("读取二进制内容失败")?;
    Ok((bytes.to_vec(), release.tag_name))
}

/// 二进制基本校验：ELF magic + 体积下限
pub fn validate_binary(bytes: &[u8]) -> Result<()> {
    anyhow::ensure!(
        bytes.len() as u64 >= MIN_BINARY_SIZE,
        "文件过小（{} 字节），不是有效二进制",
        bytes.len()
    );
    anyhow::ensure!(bytes.starts_with(b"\x7fELF"), "文件不是 ELF 可执行格式");
    Ok(())
}

/// 替换自身二进制：写临时文件 → 赋可执行位 → rename 覆盖。
/// Linux 允许 rename 覆盖正在运行的可执行文件（旧 inode 由进程持有），
/// 因此服务运行中也能完成替换；新版本需重启服务才生效。
/// 成功返回后需 systemctl restart 才切换到新版本。
pub fn install_binary(bytes: &[u8]) -> Result<PathBuf> {
    validate_binary(bytes)?;
    let exe = std::env::current_exe().context("无法定位当前二进制路径")?;
    let dir = exe.parent().context("二进制路径无父目录")?;
    let tmp = dir.join("lyys-panel.new");
    std::fs::write(&tmp, bytes).context("写入临时文件失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &exe).map_err(|e| {
        std::fs::remove_file(&tmp).ok();
        anyhow::anyhow!("替换二进制失败（服务可能仍在运行）：{e}")
    })?;
    tracing::warn!("二进制已更新，重启服务后生效");
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert_eq!(version_cmp("v0.2.0", "0.1.0"), std::cmp::Ordering::Greater);
        assert_eq!(version_cmp("0.1.0", "v0.1.0"), std::cmp::Ordering::Equal);
        assert_eq!(
            version_cmp("v1.0.0", "v0.99.9"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(version_cmp("0.9", "0.9.1"), std::cmp::Ordering::Less);
        assert_eq!(version_cmp("v10.0", "v9.0"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn binary_validation() {
        assert!(validate_binary(b"not an elf").is_err());
        assert!(validate_binary(b"\x7fELF small").is_err());
        let fake = vec![0x7f, b'E', b'L', b'F', 2, 1]
            .into_iter()
            .chain(std::iter::repeat_n(0, 2 * 1024 * 1024))
            .collect::<Vec<u8>>();
        assert!(validate_binary(&fake).is_ok());
    }
}
