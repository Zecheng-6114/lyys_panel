//! 3.5 Let's Encrypt 自动证书（ACME v2）
//!
//! 面板自管的 Nginx 站点原本只能用自签或上传证书；自签浏览器要手动信任、上传要
//! 人工续期。这里补上一条正路：以 RFC 8555 直接向 Let's Encrypt 申请证书，并按
//! 有效期自动续期。
//!
//! 设计：
//! - **自包含客户端**：账户密钥（ES256）、订单、http-01 质询、签发与全链下载全部
//!   自己实现；签名复用 `ring`、密钥与 CSR 复用 `rcgen`，**不引入新依赖**。
//! - **协议与站解耦**：本模块只负责「拿到域名 → 换回证书/私钥」，质询文件的落盘
//!   目录是 [`webroot`]；由 [`crate::websites`] 在站点配置里注入 location 让它对外
//!   可达。调用方需保证该目录已被 Nginx 提供（否则 http-01 必然失败）。
//! - **账户私钥**落 `<data_dir>/acme/account.key`（0600，与 JWT/证书私钥同口径），
//!   首次使用时生成并持久化；后续请求复用同一账户（`newAccount` 对同一密钥幂等）。
//! - **测试可控**：目录端点可用 `PANEL_ACME_DIR` 覆盖（配合 Pebble 等测试服务器），
//!   `PANEL_ACME_INSECURE=1` 跳过 TLS 校验（测试服务器的证书不受信）。
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::db::Db;

/// Let's Encrypt 生产目录
const PROD_DIRECTORY: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Let's Encrypt 预发目录：签发不消耗正式配额，但证书浏览器不信任（仅测试）
const STAGING_DIRECTORY: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
/// 单次 ACME HTTP 请求超时
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// 轮询授权 / 订单状态的上限（每次间隔 1s）
const POLL_TRIES: u32 = 30;
/// 证书缓期：签发满该天数即自动续期（LE 证书 90 天，留足重试与失败排查的余量）
pub const RENEW_AFTER_DAYS: i64 = 60;
/// 质询轮询间隔
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// ACME 账户设置（存 settings 表，key=acme_settings）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AcmeSettings {
    /// 账户联系邮箱；留空则不提交 contact（Pebble 等测试服务器不校验）
    #[serde(default)]
    pub email: String,
    /// 使用预发环境（测试用；正式签发请关闭）
    #[serde(default)]
    pub staging: bool,
}

pub fn load_settings(db: &Db) -> AcmeSettings {
    db.get_setting("acme_settings")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(db: &Db, s: &AcmeSettings) -> Result<()> {
    let json = serde_json::to_string(s)?;
    db.set_setting("acme_settings", &json)
}

/// 质询文件的根目录：站点配置里的 location 指向它，文件写在 `.well-known/acme-challenge/`
pub fn webroot(data_dir: &Path) -> PathBuf {
    data_dir.join("acme-webroot")
}

fn challenge_dir(webroot: &Path) -> PathBuf {
    webroot.join(".well-known").join("acme-challenge")
}

/// 一次成功签发的产物
pub struct Issued {
    /// 全链证书（叶证书 + 中间证书）PEM
    pub cert_pem: String,
    /// 站点私钥 PEM（PKCS#8）
    pub key_pem: String,
}

// ---------- base64url（无填充，RFC 4648 §5） ----------

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn b64url(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64URL[((n >> 18) & 63) as usize] as char);
        out.push(B64URL[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64URL[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[(n & 63) as usize] as char);
        }
    }
    out
}

/// base64url 解码（仅供单测做完整性验证）
#[cfg(test)]
fn b64url_decode(s: &str) -> Result<Vec<u8>> {
    let val = |c: u8| -> Result<u32> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a') as u32 + 26),
            b'0'..=b'9' => Ok((c - b'0') as u32 + 52),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => bail!("非法 base64url 字符"),
        }
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for chunk in s.as_bytes().chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= val(c)? << (18 - 6 * i as u32);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    Ok(out)
}

// ---------- ACME 数据结构 ----------

#[derive(Deserialize)]
struct Directory {
    #[serde(rename = "newNonce")]
    new_nonce: String,
    #[serde(rename = "newAccount")]
    new_account: String,
    #[serde(rename = "newOrder")]
    new_order: String,
}

#[derive(Deserialize)]
struct Problem {
    #[serde(default, rename = "type")]
    typ: String,
    #[serde(default)]
    detail: String,
}

impl Problem {
    /// 转成一句可读的中文错误（`type` 只取末段，如 `badNonce`）
    fn text(&self) -> String {
        let short = self.typ.rsplit(':').next().unwrap_or("");
        match (self.detail.is_empty(), short.is_empty()) {
            (false, false) => format!("{short}：{}", self.detail),
            (false, true) => self.detail.clone(),
            (true, false) => short.to_string(),
            (true, true) => "未知错误".to_string(),
        }
    }
}

#[derive(Deserialize)]
struct Order {
    status: String,
    #[serde(default)]
    finalize: String,
    #[serde(default)]
    authorizations: Vec<String>,
    #[serde(default)]
    certificate: Option<String>,
    #[serde(default)]
    error: Option<Problem>,
}

#[derive(Deserialize)]
struct Authorization {
    status: String,
    #[serde(default)]
    challenges: Vec<Challenge>,
    #[serde(default)]
    error: Option<Problem>,
}

#[derive(Deserialize)]
struct Challenge {
    #[serde(rename = "type")]
    typ: String,
    url: String,
    #[serde(default)]
    token: String,
}

/// 一次 ACME 响应
struct Resp {
    status: u16,
    location: Option<String>,
    body: String,
}

impl Resp {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, what: &str) -> Result<T> {
        serde_json::from_str(&self.body)
            .with_context(|| format!("解析 ACME {what} 响应失败：{}", snippet(&self.body)))
    }
}

/// 把响应体压成短文案（错误详情里带上，避免日志被整页 HTML 淹没）
fn snippet(s: &str) -> String {
    let t = s.trim();
    let mut out: String = t.chars().take(200).collect();
    if t.chars().count() > 200 {
        out.push('…');
    }
    out
}

/// 从错误响应里提炼可读信息（LE 的 problem JSON 带 detail，优先用它）
fn acme_error(r: &Resp) -> String {
    if let Ok(p) = serde_json::from_str::<Problem>(&r.body) {
        let t = p.text();
        if t != "未知错误" {
            return format!("HTTP {}（{t}）", r.status);
        }
    }
    format!("HTTP {}：{}", r.status, snippet(&r.body))
}

// ---------- 客户端 ----------

struct AcmeClient {
    http: reqwest::Client,
    rng: ring::rand::SystemRandom,
    key: ring::signature::EcdsaKeyPair,
    /// 公钥 JWK（newAccount 的 protected 头里用它，其余请求用 kid）
    jwk: serde_json::Value,
    /// JWK SHA-256 指纹的 base64url（拼 keyAuthorization 用）
    thumbprint: String,
    nonce: Option<String>,
    new_nonce_url: String,
    new_account_url: String,
    new_order_url: String,
    /// 账户 URL（`kid`），newAccount 成功后从 Location 头取得
    kid: String,
}

impl AcmeClient {
    /// 建立会话：拉目录、装载账户密钥、注册（或复用）账户
    async fn connect(
        http: reqwest::Client,
        key_pair: &rcgen::KeyPair,
        directory_url: &str,
        email: &str,
    ) -> Result<Self> {
        let resp = http
            .get(directory_url)
            .send()
            .await
            .with_context(|| format!("访问 ACME 目录失败：{directory_url}"))?;
        if !resp.status().is_success() {
            bail!("ACME 目录不可用（HTTP {}）：{directory_url}", resp.status());
        }
        let dir: Directory = resp.json().await.context("解析 ACME 目录失败")?;

        let raw = key_pair.public_key_raw();
        if raw.len() != 65 || raw[0] != 4 {
            bail!("账户公钥格式异常（期望 P-256 未压缩点）");
        }
        let x = b64url(&raw[1..33]);
        let y = b64url(&raw[33..65]);
        // RFC 7638 规范形式：键按字典序、无空白，恰为 crv/kty/x/y
        let canonical = format!("{{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"{x}\",\"y\":\"{y}\"}}");
        let digest = ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes());
        let thumbprint = b64url(digest.as_ref());

        let rng = ring::rand::SystemRandom::new();
        let der = key_pair.serialize_der();
        let key = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &der,
            &rng,
        )
        .map_err(|e| anyhow::anyhow!("加载 ACME 账户密钥失败：{e}"))?;

        let mut client = Self {
            http,
            rng,
            key,
            jwk: serde_json::json!({ "crv": "P-256", "kty": "EC", "x": x, "y": y }),
            thumbprint,
            nonce: None,
            new_nonce_url: dir.new_nonce,
            new_account_url: dir.new_account,
            new_order_url: dir.new_order,
            kid: String::new(),
        };

        let payload = if email.trim().is_empty() {
            serde_json::json!({ "termsOfServiceAgreed": true }).to_string()
        } else {
            serde_json::json!({
                "termsOfServiceAgreed": true,
                "contact": [format!("mailto:{}", email.trim())],
            })
            .to_string()
        };
        let url = client.new_account_url.clone();
        let r = client.post(&url, &payload, true).await?;
        if !r.ok() {
            bail!("注册 ACME 账户失败：{}", acme_error(&r));
        }
        client.kid = r
            .location
            .context("ACME 账户响应缺少 Location（无法取得账户 URL）")?;
        Ok(client)
    }

    /// 取一个 nonce：优先用上一次响应带回的，否则向 newNonce 要
    async fn take_nonce(&mut self) -> Result<String> {
        if let Some(n) = self.nonce.take() {
            return Ok(n);
        }
        let r = self
            .http
            .head(&self.new_nonce_url)
            .send()
            .await
            .context("获取 ACME nonce 失败")?;
        r.headers()
            .get("replay-nonce")
            .context("ACME nonce 响应缺少 Replay-Nonce 头")?
            .to_str()
            .context("Replay-Nonce 非 ASCII")
            .map(|s| s.to_string())
    }

    /// 发一个 JWS 请求（RFC 8555 §6.2）。`payload` 为空串即 POST-as-GET。
    /// `use_jwk` 为真时 protected 头带 jwk（仅 newAccount），否则带 kid。
    async fn post(&mut self, url: &str, payload: &str, use_jwk: bool) -> Result<Resp> {
        let nonce = self.take_nonce().await?;
        let protected = if use_jwk {
            serde_json::json!({ "alg": "ES256", "jwk": self.jwk, "nonce": nonce, "url": url })
        } else {
            serde_json::json!({ "alg": "ES256", "kid": self.kid, "nonce": nonce, "url": url })
        };
        let prot_b64 = b64url(&serde_json::to_vec(&protected).context("序列化 JWS 头失败")?);
        let pay_b64 = b64url(payload.as_bytes());
        let signing_input = format!("{prot_b64}.{pay_b64}");
        let sig = self
            .key
            .sign(&self.rng, signing_input.as_bytes())
            .map_err(|_| anyhow::anyhow!("ACME 请求签名失败"))?;
        let body = serde_json::json!({
            "protected": prot_b64,
            "payload": pay_b64,
            "signature": b64url(sig.as_ref()),
        });

        let resp = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/jose+json")
            .body(serde_json::to_string(&body).context("序列化 JWS 失败")?)
            .send()
            .await
            .with_context(|| format!("ACME 请求失败：{url}"))?;

        // 复用响应里的新 nonce，省一次 HEAD
        if let Some(n) = resp.headers().get("replay-nonce").and_then(|v| v.to_str().ok()) {
            self.nonce = Some(n.to_string());
        }
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = resp.text().await.unwrap_or_default();
        Ok(Resp {
            status,
            location,
            body,
        })
    }

    /// POST-as-GET（读取资源）
    async fn get(&mut self, url: &str) -> Result<Resp> {
        let r = self.post(url, "", false).await?;
        if !r.ok() {
            bail!("ACME 读取失败：{}", acme_error(&r));
        }
        Ok(r)
    }
}

// ---------- 账户密钥 ----------

/// 装载账户私钥；不存在则生成并落盘（0600）
fn load_or_create_account_key(dir: &Path) -> Result<rcgen::KeyPair> {
    let path = dir.join("account.key");
    if let Ok(text) = std::fs::read_to_string(&path) {
        match rcgen::KeyPair::from_pem(&text) {
            Ok(kp) => return Ok(kp),
            Err(e) => tracing::warn!("账户密钥无法解析，将重新生成：{e}"),
        }
    }
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("生成 ACME 账户密钥失败")?;
    std::fs::create_dir_all(dir).context("创建 ACME 目录失败")?;
    std::fs::write(&path, kp.serialize_pem()).context("写入 ACME 账户密钥失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("设置账户密钥权限失败")?;
    }
    tracing::info!("已生成 ACME 账户密钥：{}", path.display());
    Ok(kp)
}

// ---------- 主流程 ----------

/// 为 `domains` 申请一张证书（http-01）。`webroot` 必须已被 Nginx 对外提供。
pub async fn issue(
    data_dir: &Path,
    domains: &[String],
    webroot: &Path,
    email: &str,
    staging: bool,
) -> Result<Issued> {
    let domains: Vec<String> = domains
        .iter()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect();
    if domains.is_empty() {
        bail!("没有可用于签发的域名");
    }

    let directory_url = std::env::var("PANEL_ACME_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if staging {
                STAGING_DIRECTORY.to_string()
            } else {
                PROD_DIRECTORY.to_string()
            }
        });
    let insecure = matches!(
        std::env::var("PANEL_ACME_INSECURE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    );

    let http = reqwest::Client::builder()
        .user_agent("lyys-panel-acme/1.0")
        .timeout(HTTP_TIMEOUT)
        .danger_accept_invalid_certs(insecure)
        .build()
        .context("创建 ACME HTTP 客户端失败")?;

    let account_key = load_or_create_account_key(&data_dir.join("acme"))?;
    let mut client = AcmeClient::connect(http, &account_key, &directory_url, email).await?;

    // 下单
    let order_payload = serde_json::json!({
        "identifiers": domains.iter().map(|d| serde_json::json!({ "type": "dns", "value": d })).collect::<Vec<_>>(),
    })
    .to_string();
    let url = client.new_order_url.clone();
    let r = client.post(&url, &order_payload, false).await?;
    if !r.ok() {
        bail!("创建 ACME 订单失败：{}", acme_error(&r));
    }
    let order_url = r.location.clone().context("订单响应缺少 Location")?;
    let order: Order = r.json("订单")?;

    // 逐个完成授权（http-01）
    for authz_url in &order.authorizations {
        fulfill_authorization(&mut client, authz_url, webroot).await?;
    }

    // 签发：生成站点密钥与 CSR，提交 finalize
    let cert_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("生成站点私钥失败")?;
    let params = rcgen::CertificateParams::new(domains.clone()).context("构造 CSR 参数失败")?;
    let csr = params
        .serialize_request(&cert_key)
        .context("生成 CSR 失败")?;
    let csr_der: Vec<u8> = csr.der().to_vec();

    if order.finalize.is_empty() {
        bail!("ACME 订单未提供 finalize 地址");
    }
    let finalize_payload = serde_json::json!({ "csr": b64url(&csr_der) }).to_string();
    let r = client.post(&order.finalize, &finalize_payload, false).await?;
    if !r.ok() {
        bail!("提交 CSR 失败：{}", acme_error(&r));
    }

    // 等待订单转 valid，再下载全链
    let order = poll_order(&mut client, &order_url).await?;
    let cert_url = order
        .certificate
        .clone()
        .context("订单已通过但未提供证书地址")?;
    let r = client.get(&cert_url).await?;
    if !r.body.contains("BEGIN CERTIFICATE") {
        bail!("证书下载内容异常（非 PEM）：{}", snippet(&r.body));
    }
    tracing::info!("ACME 签发成功：{}", domains.join(", "));
    Ok(Issued {
        cert_pem: r.body,
        key_pem: cert_key.serialize_pem(),
    })
}

/// 完成一条授权：写 http-01 质询文件 → 触发校验 → 轮询到 valid
async fn fulfill_authorization(client: &mut AcmeClient, authz_url: &str, webroot: &Path) -> Result<()> {
    let r = client.get(authz_url).await?;
    let authz: Authorization = r.json("授权")?;
    if authz.status == "valid" {
        return Ok(());
    }
    let ch = authz
        .challenges
        .iter()
        .find(|c| c.typ == "http-01")
        .context("该授权不提供 http-01 质询（通配符域名需 DNS-01，暂不支持）")?;
    if ch.token.is_empty() {
        bail!("http-01 质询缺少 token");
    }

    // 写入质询文件：token → token.thumbprint
    let dir = challenge_dir(webroot);
    std::fs::create_dir_all(&dir).context("创建质询目录失败")?;
    let file = dir.join(&ch.token);
    let content = format!("{}.{}", ch.token, client.thumbprint);
    std::fs::write(&file, content).context("写入质询文件失败")?;

    // 触发校验；无论成败都清理质询文件
    let result = trigger_and_poll(client, &ch.url, authz_url).await;
    let _ = std::fs::remove_file(&file);
    result
}

async fn trigger_and_poll(client: &mut AcmeClient, challenge_url: &str, authz_url: &str) -> Result<()> {
    let r = client.post(challenge_url, "{}", false).await?;
    if !r.ok() {
        bail!("触发 http-01 校验失败：{}", acme_error(&r));
    }
    for _ in 0..POLL_TRIES {
        tokio::time::sleep(POLL_INTERVAL).await;
        let r = client.get(authz_url).await?;
        let authz: Authorization = r.json("授权")?;
        match authz.status.as_str() {
            "valid" => return Ok(()),
            "invalid" => {
                let detail = authz
                    .error
                    .as_ref()
                    .map(|p| p.text())
                    .unwrap_or_else(|| "校验未通过".into());
                bail!("http-01 校验失败：{detail}");
            }
            _ => {}
        }
    }
    bail!("等待 http-01 校验结果超时")
}

/// 轮询订单直到 valid（处理中会短暂返回 processing）
async fn poll_order(client: &mut AcmeClient, order_url: &str) -> Result<Order> {
    for _ in 0..POLL_TRIES {
        let r = client.get(order_url).await?;
        let order: Order = r.json("订单")?;
        match order.status.as_str() {
            "valid" => return Ok(order),
            "invalid" => {
                let detail = order
                    .error
                    .as_ref()
                    .map(|p| p.text())
                    .unwrap_or_else(|| "订单失败".into());
                bail!("ACME 订单未通过：{detail}");
            }
            _ => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
    bail!("等待 ACME 订单完成超时")
}

// ---------- 自动续期 ----------

/// 后台续期任务：每 12 小时扫一遍启用自动证书的站点，到期的重新签发。
/// 首次签发仍需人工点一次「申请证书」——避免刚启用就自动向 CA 下单。
pub fn spawn_renewer(state: AppState) {
    tokio::spawn(async move {
        loop {
            let settings = load_settings(&state.db);
            let sites = crate::websites::list(&state.data_dir);
            for site in sites.into_iter().filter(|s| s.acme && s.enabled) {
                if !crate::websites::acme_due(&state.data_dir, &site) {
                    continue;
                }
                tracing::info!("ACME 自动续期：{}", site.name);
                if let Err(e) =
                    crate::websites::issue_acme(&state.data_dir, &site, &settings).await
                {
                    tracing::warn!("ACME 续期失败（{}）：{e:#}", site.name);
                }
            }
            tokio::time::sleep(Duration::from_secs(12 * 3600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// base64url 与已知向量一致，且能往返解码
    #[test]
    fn base64url_matches_known_vectors() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(b"hello"), "aGVsbG8");
        // URL 安全字符集：标准 base64 的 `+` / `/` 应被替换为 `-` / `_`
        assert_eq!(b64url(&[0xff, 0xff, 0xff]), "____");
        assert_eq!(b64url(&[0xfb, 0xff, 0xbf]), "-_-_");

        for sample in [b"".as_slice(), b"f", b"fo", b"foo", b"hello world", &[0xff, 0x00, 0x7f]] {
            assert_eq!(b64url_decode(&b64url(sample)).unwrap(), sample);
        }
    }

    /// JWK 指纹（RFC 7638）：ES256 的 x/y 提取得当，指纹为 43 字符且稳定
    #[test]
    fn thumbprint_is_stable_and_well_formed() {
        let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let raw = kp.public_key_raw();
        assert_eq!(raw.len(), 65);
        assert_eq!(raw[0], 4, "未压缩点应以 0x04 开头");
        let x = b64url(&raw[1..33]);
        let y = b64url(&raw[33..65]);
        assert_eq!(x.len(), 43, "P-256 坐标 base64url 后为 43 字符");
        assert_eq!(y.len(), 43);

        let canonical = format!("{{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"{x}\",\"y\":\"{y}\"}}");
        let t1 = b64url(ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes()).as_ref());
        let t2 = b64url(ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes()).as_ref());
        assert_eq!(t1, t2);
        // SHA-256 → 32 字节 → base64url 43 字符
        assert_eq!(t1.len(), 43);
    }

    /// 账户密钥 + ES256 签名可用，且签名能被对应公钥验证（64 字节固定长度 r||s）
    #[test]
    fn account_key_signs_and_verifies() {
        let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let rng = ring::rand::SystemRandom::new();
        let signing = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &kp.serialize_der(),
            &rng,
        )
        .expect("应由 PKCS#8 正确加载");
        let msg = b"protected.payload";
        let sig = signing.sign(&rng, msg).unwrap();
        assert_eq!(sig.as_ref().len(), 64, "ES256 JWS 需固定 64 字节签名");

        // ring 的 UnparsedPublicKey 收的是裸公钥（未压缩点），不是 SPKI
        let vk = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_FIXED,
            kp.public_key_raw(),
        );
        vk.verify(msg, sig.as_ref()).expect("签名应可验证");
    }

    /// 账户密钥落盘后能重新装载（PEM 往返）
    #[test]
    fn account_key_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "lyys_acme_key_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let k1 = load_or_create_account_key(&dir).unwrap();
        let p1 = k1.public_key_der();
        let k2 = load_or_create_account_key(&dir).unwrap();
        assert_eq!(p1, k2.public_key_der(), "二次调用应复用同一密钥");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("account.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "账户私钥必须是 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 设置项读写往返
    #[test]
    fn settings_round_trip() {
        let s = AcmeSettings {
            email: "ops@example.com".into(),
            staging: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: AcmeSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.email, "ops@example.com");
        assert!(back.staging);
        // 缺省字段应可反序列化（老数据没有这两个键）
        let empty: AcmeSettings = serde_json::from_str("{}").unwrap();
        assert!(empty.email.is_empty() && !empty.staging);
    }
}
