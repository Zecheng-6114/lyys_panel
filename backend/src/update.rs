// 3.2 面板自更新
//
// 更新源是 GitHub Release（Zecheng-6114/lyys_panel），二进制资产名按运行架构
// 选择：x86_64 → `lyys-panel`，aarch64 → `lyys-panel-aarch64`（见 [`asset_name`]）。
// 流程：检查 → 下载校验 → 替换 → 重启。
//
// 替换采用「先落临时文件再原子 rename」：目标运行中时 rename 会失败
// （ETXTBSY），这正是我们要的安全闸——只有服务已停或二进制不在运行路径
// 上时才会替换成功。下载后做体积下限校验（release 产物远大于 1MB），
// 防止把 GitHub 的 HTML 错误页当成二进制装上。
//
// P1-2：校验升级为三重闸——体积 → ELF magic → sha256 强校验。发布流程对每个
// 资产产出同名 `.sha256`，下载时一并取回比对；**取不到校验和文件即整体失败**，
// 不允许降级成「跳过校验只装文件」——这条链路以 root 身份覆盖正在运行的自身，
// 静默跳过校验等于把它交给任何能污染下载通道的人。
//
// 除 GitHub 检查外，还提供「手动上传」通道：内网环境可能访问不了 GitHub，
// 管理员可在有网机器构建后上传替换（同样走临时文件 + rename）。
use anyhow::{bail, Context, Result};
use ring::digest;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 当前版本 = Cargo.toml 的 version（编译期注入）
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 更新源仓库（发布二进制资产到此 Release 才可被自更新拉取）
const REPO_OWNER: &str = "Zecheng-6114";
const REPO_NAME: &str = "lyys_panel";
/// x86_64 资产名：首期发布即用此固定名，不能改（旧版本的自更新逻辑认它）
const ASSET_X86_64: &str = "lyys-panel";
/// aarch64 资产名：与 x86_64 产物区分，避免把错架构的二进制装到 ARM 机器上
const ASSET_AARCH64: &str = "lyys-panel-aarch64";

/// 当前架构对应的 Release 资产名。
///
/// 用编译期常量 `ARCH` 而非 `uname -m`：自更新二进制本身已按架构编译，
/// 编译目标就是运行架构，不必再 fork 子进程探测。
/// 仅 x86_64 / aarch64 有官方产物，其余架构直接拒绝而不是退回 x86_64。
fn asset_name() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(ASSET_X86_64),
        "aarch64" => Ok(ASSET_AARCH64),
        other => bail!("当前架构 {other} 没有官方构建产物，请走手动上传通道"),
    }
}

/// 二进制体积下限：release 产物约 10MB+，小于 1MB 必是错误页/占位文件
const MIN_BINARY_SIZE: u64 = 1024 * 1024;

/// 校验和资产名后缀：发布流程产出 `<资产名>.sha256`
const CHECKSUM_SUFFIX: &str = ".sha256";

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

/// 自更新下载结果：二进制内容、来源标签、发布方公布的 sha256。
pub struct Download {
    pub bytes: Vec<u8>,
    pub tag: String,
    /// 上游 release 公布的 sha256（十六进制小写）
    pub sha256: String,
}

/// 从资产列表中取出与指定二进制同名的校验和文件。
///
/// 用资产名拼名，而不是只按 `.sha256` 后缀匹配：同一个 release 里还有另一个
/// 架构的校验和，只认后缀会取到另一个架构的哈希 —— 那样校验必然失败，更糟的是
/// 若两个架构产物恰好同源，会「校验通过」却装错了架构。
fn checksum_asset<'a>(release: &'a ReleaseJson, asset: &str) -> Option<&'a AssetJson> {
    let want = format!("{asset}{CHECKSUM_SUFFIX}");
    release.assets.iter().find(|a| a.name == want)
}

/// 解析 `sha256sum` 输出（`<hex>  <文件名>`）。只认 64 位十六进制，
/// 其余一律按格式错误处理：宁可拒绝更新，也不拿一个解析失败的东西去比对。
fn parse_checksum(text: &str) -> Result<String> {
    let sum = text.split_whitespace().next().unwrap_or_default();
    anyhow::ensure!(
        sum.len() == 64 && sum.chars().all(|c| c.is_ascii_hexdigit()),
        "校验和文件格式不正确"
    );
    Ok(sum.to_ascii_lowercase())
}

/// 计算 sha256 的十六进制小写表示
fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let digest = digest::digest(&digest::SHA256, bytes);
    let mut out = String::with_capacity(64);
    for b in digest.as_ref() {
        let _ = write!(out, "{b:02x}");
    }
    out
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

/// 下载 GitHub 最新版中**当前架构**对应的资产，连同发布方公布的 sha256 一并返回。
/// 校验和是必需项：取不到文件或格式不对一律失败，而不是降级为「不带校验地装」。
pub async fn download_github(client: &reqwest::Client) -> Result<Download> {
    let asset_name = asset_name()?;
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
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| anyhow::anyhow!("最新 release 中没有 {asset_name} 二进制资产"))?;
    // P1-2：校验和缺失即整体失败，这里不做任何降级。
    let checksum = checksum_asset(&release, asset_name).ok_or_else(|| {
        anyhow::anyhow!("最新 release 中没有 {asset_name}{CHECKSUM_SUFFIX}，无法校验，已拒绝更新")
    })?;
    let tag = release.tag_name.clone();

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

    let sum_text = client
        .get(&checksum.browser_download_url)
        .header(reqwest::header::USER_AGENT, "lyys-panel-updater")
        .send()
        .await
        .context("下载校验和失败")?
        .error_for_status()
        .context("下载校验和返回错误状态")?
        .text()
        .await
        .context("读取校验和内容失败")?;

    Ok(Download {
        bytes: bytes.to_vec(),
        tag,
        sha256: parse_checksum(&sum_text)?,
    })
}

/// 二进制校验，顺序固定为：体积 → ELF magic → sha256。
///
/// 前两步极便宜，先挡掉 GitHub 的 HTML 错误页与占位文件，不必为明显无效的输入
/// 白算一遍哈希；sha256 是唯一能挡住「体积达标、魔数正确、内容被换掉」的一道闸。
/// `expected_sha256` 为 None 时跳过哈希比对（手动上传通道无上游校验和可依）。
pub fn validate_binary(bytes: &[u8], expected_sha256: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        bytes.len() as u64 >= MIN_BINARY_SIZE,
        "文件过小（{} 字节），不是有效二进制",
        bytes.len()
    );
    anyhow::ensure!(bytes.starts_with(b"\x7fELF"), "文件不是 ELF 可执行格式");
    if let Some(expected) = expected_sha256 {
        let actual = sha256_hex(bytes);
        anyhow::ensure!(
            actual.eq_ignore_ascii_case(expected),
            "sha256 校验不通过（期望 {expected}，实际 {actual}），已放弃安装"
        );
    }
    Ok(())
}

/// 替换自身二进制：写临时文件 → 赋可执行位 → rename 覆盖。
/// Linux 允许 rename 覆盖正在运行的可执行文件（旧 inode 由进程持有），
/// 因此服务运行中也能完成替换；新版本需重启服务才生效。
/// 成功返回后需 systemctl restart 才切换到新版本。
pub fn install_binary(bytes: &[u8], expected_sha256: Option<&str>) -> Result<PathBuf> {
    validate_binary(bytes, expected_sha256)?;
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

    /// 造一段体积达标、magic 正确的假二进制
    fn fake_binary() -> Vec<u8> {
        vec![0x7f, b'E', b'L', b'F', 2, 1]
            .into_iter()
            .chain(std::iter::repeat_n(0, 2 * 1024 * 1024))
            .collect()
    }

    #[test]
    fn binary_validation() {
        assert!(validate_binary(b"not an elf", None).is_err());
        assert!(validate_binary(b"\x7fELF small", None).is_err());
        assert!(validate_binary(&fake_binary(), None).is_ok());
    }

    /// P1-2：体积与 magic 都合格、只有内容被改过一字节时，必须靠 sha256 拦住
    #[test]
    fn sha256_mismatch_is_rejected() {
        let good = fake_binary();
        let digest = sha256_hex(&good);
        assert!(validate_binary(&good, Some(&digest)).is_ok());

        let mut tampered = good.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        // 篡改后前两道闸照旧放行 —— 这正是哈希校验不可省的原因
        assert!(validate_binary(&tampered, None).is_ok());
        assert!(validate_binary(&tampered, Some(&digest)).is_err());

        // 大小写不敏感（sha256sum 输出小写，别让上游换个格式就炸）
        assert!(validate_binary(&good, Some(&digest.to_uppercase())).is_ok());
        assert!(validate_binary(&good, Some(&"0".repeat(64))).is_err());
    }

    /// 校验和解析与资产挑选：必须精确命中当前架构的资产名，
    /// 不能把同一 release 里另一个架构的那份取来
    #[test]
    fn checksum_parsing_and_asset_lookup() {
        let text = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  lyys-panel\n";
        assert_eq!(
            parse_checksum(text).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(parse_checksum("not-a-hash  file").is_err());
        assert!(parse_checksum("").is_err());

        let asset = |name: &str, url: &str| AssetJson {
            name: name.to_string(),
            browser_download_url: url.to_string(),
        };
        let release = ReleaseJson {
            tag_name: "v9.9.9".to_string(),
            assets: vec![
                asset("lyys-panel-aarch64", "https://example.com/arm"),
                asset("lyys-panel-aarch64.sha256", "https://example.com/arm-sum"),
                asset("lyys-panel", "https://example.com/x86"),
                asset("lyys-panel.sha256", "https://example.com/x86-sum"),
            ],
        };
        assert_eq!(
            checksum_asset(&release, ASSET_X86_64)
                .unwrap()
                .browser_download_url,
            "https://example.com/x86-sum"
        );
        assert_eq!(
            checksum_asset(&release, ASSET_AARCH64)
                .unwrap()
                .browser_download_url,
            "https://example.com/arm-sum"
        );

        let missing = ReleaseJson {
            tag_name: "v1".to_string(),
            assets: vec![asset("lyys-panel", "https://example.com/x86")],
        };
        assert!(
            checksum_asset(&missing, ASSET_AARCH64).is_none(),
            "缺当前架构的校验和必须能被发现"
        );
    }

    /// 资产名由编译目标架构决定：x86_64 / aarch64 命中各自产物，
    /// 其余架构拒绝（而不是退回 x86_64 装错架构）
    #[test]
    fn asset_name_follows_target_arch() {
        match std::env::consts::ARCH {
            "x86_64" => assert_eq!(asset_name().unwrap(), ASSET_X86_64),
            "aarch64" => assert_eq!(asset_name().unwrap(), ASSET_AARCH64),
            // 本仓库只发布这两个架构的产物；测试机若为其他架构应返回 Err
            _ => assert!(asset_name().is_err()),
        }
    }
}
