use anyhow::{Context, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, SaltString},
    Argon2, PasswordHasher, PasswordVerifier,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::db::Db;

/// JWT 载荷
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// 用户 id
    pub sub: i64,
    /// 过期时间（Unix 秒）
    pub exp: usize,
    /// 本枚 token 的唯一 id（登出吊销用的黑名单键，P1-1）
    pub jti: String,
}

/// 生成 n 个随机字节
fn random_bytes(n: usize) -> Vec<u8> {
    (0..n).map(|_| rand::thread_rng().gen()).collect()
}

/// 随机 JWT 密钥（64 位十六进制字符串 = 32 字节）
fn random_secret() -> String {
    random_bytes(32).iter().map(|b| format!("{b:02x}")).collect()
}

/// 随机初始密码字符集：小写 + 大写 + 数字，剔除易混淆的 0/O/1/l/I（P0-1）。
const PWD_CHARSET: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// 生成随机密码：字符集混合大小写与数字，长度 16。
fn random_password() -> String {
    (0..16)
        .map(|_| PWD_CHARSET[rand::thread_rng().gen_range(0..PWD_CHARSET.len())] as char)
        .collect()
}

/// 以 0600 权限写入敏感文件（密钥/初始密码），防止同机其他用户读取（P0-1/P0-2）。
/// 非 Unix 平台退化为普通写入（面板实际部署目标是 Linux）。
fn write_private(path: &Path, content: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("创建文件 {} 失败", path.display()))?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("写入文件 {} 失败", path.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, content).with_context(|| format!("写入文件 {} 失败", path.display()))?;
    Ok(())
}

/// 加载 JWT 密钥（P0-2：密钥绝不落数据库）。
///
/// 优先级：
/// 1. 环境变量 `PANEL_JWT_SECRET`（≥32 字节，部署方显式管理密钥/容器环境注入用）；
/// 2. 密钥文件 `<data_dir>/jwt_secret.key`（64 位十六进制，权限 0600）；
/// 3. 都没有则生成新密钥并写入密钥文件（同时打印提示）。
///
/// 历史上密钥存在 settings 表里，可被「文件接口拖库 → 提取密钥 → 伪造长效 token」
/// 打穿，因此改为文件/环境变量加载，main.rs 启动时负责把库里的遗留密钥删掉。
pub fn load_jwt_secret(data_dir: &Path) -> Result<Vec<u8>> {
    // 1) 环境变量
    if let Ok(s) = std::env::var("PANEL_JWT_SECRET") {
        anyhow::ensure!(
            s.len() >= 32,
            "PANEL_JWT_SECRET 至少需要 32 字节"
        );
        tracing::info!("JWT 密钥已从环境变量 PANEL_JWT_SECRET 加载");
        return Ok(s.into_bytes());
    }

    // 2) 密钥文件
    let key_path = data_dir.join("jwt_secret.key");
    if key_path.exists() {
        let secret = std::fs::read_to_string(&key_path)
            .with_context(|| format!("读取密钥文件 {} 失败", key_path.display()))?
            .trim()
            .to_string();
        anyhow::ensure!(secret.len() == 64, "密钥文件格式错误（应为 64 位十六进制）");
        let mut key = vec![0u8; 32];
        hex::decode_to_slice(&secret, &mut key)
            .map_err(|_| anyhow::anyhow!("解析密钥文件失败"))?;
        tracing::info!("JWT 密钥已从 {} 加载", key_path.display());
        return Ok(key);
    }

    // 3) 首次启动：生成并落盘（0600）
    let secret = random_secret();
    let mut key = vec![0u8; 32];
    hex::decode_to_slice(&secret, &mut key).map_err(|_| anyhow::anyhow!("解析密钥失败"))?;
    write_private(&key_path, &secret)?;
    tracing::info!("已生成新的 JWT 密钥文件：{}（权限 0600，请妥善备份）", key_path.display());
    Ok(key)
}

/// 签发 token，有效期 24 小时。jti 为随机唯一 id，供登出吊销使用（P1-1）。
pub fn issue_token(secret: &[u8], user_id: i64) -> Result<String> {
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 60 * 60 * 24;
    let claims = Claims {
        sub: user_id,
        exp: exp as usize,
        jti: random_bytes(16).iter().map(|b| format!("{b:02x}")).collect(),
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )?;
    Ok(token)
}

/// 校验 token 并返回完整载荷（含 jti/exp，供吊销检查）
pub fn verify_token(secret: &[u8], token: &str) -> Result<Claims> {
    // leeway 显式置 0：jsonwebtoken 默认容忍 60 秒时钟偏差，即 token 过期后
    // 一分钟内仍可通过校验。面板自签自验、同一台机器时钟，不存在时钟偏差
    // 场景，没必要留这个窗口。
    let mut validation = Validation::default();
    validation.leeway = 0;
    let data = decode::<Claims>(token, &DecodingKey::from_secret(secret), &validation)?;
    Ok(data.claims)
}

/// 用 argon2 校验密码
pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// 首次启动时引导管理员账号（用户名 admin，密码从环境变量 PANEL_ADMIN_PASSWORD 读取）。
///
/// 未设置环境变量时生成随机密码（大小写+数字混合），**不打印到日志**（P0-1），
/// 而是写入数据目录下的 `initial_admin_password.txt`（权限 0600），日志只提示
/// 文件位置；首次登录成功后该文件会被自动删除（见 [`cleanup_initial_password`]）。
pub fn ensure_admin(db: &Db, data_dir: &Path) -> Result<()> {
    if db.user_count()? > 0 {
        return Ok(());
    }
    let password = std::env::var("PANEL_ADMIN_PASSWORD").unwrap_or_else(|_| {
        let pwd = random_password();
        let path = data_dir.join("initial_admin_password.txt");
        let content = format!(
            "初始管理员账号 admin 的随机密码：{pwd}\n\
             \x20\x20\x20\x20此文件权限 0600，首次登录成功后会自动删除。\n"
        );
        if let Err(e) = write_private(&path, &content) {
            // 文件写失败（如目录只读）时退回打印密码：宁可日志暴露一次，
            // 也不能让管理员永远登不进面板
            tracing::error!("写入初始密码文件失败：{e:#}，改为直接打印：admin / {pwd}");
        } else {
            // 只提示位置，绝不输出密码本身
            tracing::warn!(
                "未设置 PANEL_ADMIN_PASSWORD，已生成随机管理员密码，请查看 {}（权限 0600，首次登录后自动删除）",
                path.display()
            );
        }
        pwd
    });

    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("生成密码哈希失败：{e}"))?
        .to_string();

    db.create_user("admin", &hash, salt.as_str())?;
    tracing::info!("已创建初始管理员账号：admin");
    Ok(())
}

/// 首次登录成功后删除初始密码文件（P0-1：一次性文件方案）。
/// 文件不存在视为已完成，任何失败都不阻断登录流程。
pub fn cleanup_initial_password(data_dir: &Path) {
    let path = data_dir.join("initial_admin_password.txt");
    if !path.exists() {
        return;
    }
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::info!("已删除初始密码文件（首次登录完成）"),
        Err(e) => {
            // 删不掉（权限等）只提示，不影响登录；管理员可手动删除
            tracing::warn!("删除初始密码文件失败：{e}，请手动删除 {}", path.display())
        }
    }
}

/// Token 吊销名单（P1-1：服务端登出）。
///
/// key = token 的 jti，value = 该 token 的 exp（Unix 秒）。条目到期后清理，
/// 因此名单只覆盖「仍有效的吊销」，内存占用有界（单管理员面板，量级极小）。
///
/// 联动约定（改密/密钥轮换）：本名单只对当前密钥签发的 token 有意义 ——
/// 密钥一旦轮换，旧 token 在签名校验一步就会被拒；引入改密功能时应同步
/// 轮换密钥并清空本名单（`clear()`），让所有存量会话立即失效。
pub struct TokenRevocations {
    entries: Mutex<HashMap<String, i64>>,
}

/// 名单上限，防止异常路径把内存撑爆（对齐 LoginThrottle 的做法）
const MAX_REVOKED: usize = 4096;

impl TokenRevocations {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 吊销一枚 token（以 jti 为键，有效期至 exp）
    pub fn revoke(&self, jti: &str, exp: i64) {
        let Ok(mut map) = self.entries.lock() else {
            return;
        };
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        // 顺手清理已过期的旧条目，避免长期运行缓慢增长
        map.retain(|_, e| *e > now);
        if map.len() >= MAX_REVOKED {
            // 名单已满（retain 后仍满，说明全是未到期条目）：逐出最早过期的
            // 一条腾位。此前是整体 clear——那会把全部真实吊销记录一并冲掉，
            // 让已登出的 token 复活，是吊销语义的漏洞。
            if let Some(oldest) = map.iter().min_by_key(|(_, e)| *e).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
        map.insert(jti.to_string(), exp);
    }

    /// 该 jti 是否处于吊销状态（未到期）
    pub fn is_revoked(&self, jti: &str) -> bool {
        let Ok(map) = self.entries.lock() else {
            // 锁中毒：宁可放行（下一步还有签名/exp 校验兜底），不锁死用户
            return false;
        };
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        matches!(map.get(jti), Some(e) if *e > now)
    }

    /// 清空名单（密钥轮换/改密联动时使用）
    pub fn clear(&self) {
        if let Ok(mut map) = self.entries.lock() {
            map.clear();
        }
    }
}

/// 登录失败退避器。
///
/// 面板默认监听 `0.0.0.0` 且走明文 HTTP，若不限制失败次数，局域网内任何人都能
/// 对管理员密码做无限次尝试。这里采取**指数退避**而非账号锁定：每次失败后，
/// 该来源的下一次尝试需要等待一段时间，等待时长逐次翻倍并封顶。
///
/// 选择退避而不是锁定，是为了避免「自己记错几次密码把自己关在门外」——
/// 合法用户只会觉得响应变慢，而爆破方的成本随尝试次数指数上升。
///
/// 状态保存在内存中，进程重启即清空。这与面板的单机定位相符：
/// 重启需要登录服务器（此时已具备更强的系统级权限），因此重置退避不构成绕过。
pub struct LoginThrottle {
    /// key = 来源 IP + 用户名，value = 失败状态
    entries: Mutex<HashMap<String, Failure>>,
}

/// 单个来源的失败状态
#[derive(Clone, Copy)]
struct Failure {
    /// 连续失败次数
    count: u32,
    /// 记为「连续」的最后一次失败时间，超过窗口即重新计数
    last: Instant,
    /// 下一次允许尝试的时刻
    next_allowed: Instant,
}

/// 失败计数窗口：距上次失败超过这个时长，视为重新开始计数
const FAILURE_WINDOW: Duration = Duration::from_secs(300);
/// 首次失败后的退避时长
const BASE_DELAY: Duration = Duration::from_secs(1);
/// 退避时长上限
const MAX_DELAY: Duration = Duration::from_secs(30);
/// 记录表上限，防止海量伪造来源把内存撑爆
const MAX_ENTRIES: usize = 4096;

impl LoginThrottle {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 构造限流键：来源 IP 与用户名组合。
    /// 带上用户名是为了避免同一 NAT 后的合法用户被他人牵连。
    fn key(ip: IpAddr, username: &str) -> String {
        format!("{ip}|{username}")
    }

    /// 当前需要等待多久才能再次尝试；返回 `Duration::ZERO` 表示可以立即尝试。
    pub fn retry_after(&self, ip: IpAddr, username: &str) -> Duration {
        let Ok(map) = self.entries.lock() else {
            // 锁中毒（某次 panic 遗留）：放行，不因为限流器自身故障把用户锁死
            return Duration::ZERO;
        };
        let Some(f) = map.get(&Self::key(ip, username)) else {
            return Duration::ZERO;
        };
        f.next_allowed.saturating_duration_since(Instant::now())
    }

    /// 记录一次失败，返回该来源下次需要等待的时长。
    pub fn record_failure(&self, ip: IpAddr, username: &str) -> Duration {
        let now = Instant::now();
        let Ok(mut map) = self.entries.lock() else {
            return Duration::ZERO;
        };

        // 简单容量控制：超上限时清掉所有已过期的记录；仍然超限就整体清空。
        if map.len() >= MAX_ENTRIES {
            map.retain(|_, f| now.duration_since(f.last) < FAILURE_WINDOW);
            if map.len() >= MAX_ENTRIES {
                map.clear();
            }
        }

        let entry = map
            .entry(Self::key(ip, username))
            .or_insert(Failure {
                count: 0,
                last: now,
                next_allowed: now,
            });

        // 距上次失败已超出窗口，视为新一轮
        if now.duration_since(entry.last) >= FAILURE_WINDOW {
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(1);
        entry.last = now;

        // 1s、2s、4s…… 封顶 MAX_DELAY
        let delay = BASE_DELAY
            .saturating_mul(1u32 << (entry.count - 1).min(16))
            .min(MAX_DELAY);
        entry.next_allowed = now + delay;
        delay
    }

    /// 登录成功后清除该来源的失败记录。
    pub fn record_success(&self, ip: IpAddr, username: &str) {
        let Ok(mut map) = self.entries.lock() else {
            return;
        };
        map.remove(&Self::key(ip, username));
    }
}

/// 十六进制编解码（避免额外依赖，仅内部使用）
mod hex {
    /// 将十六进制字符串解码到目标字节切片
    pub fn decode_to_slice(src: &str, dst: &mut [u8]) -> Result<(), ()> {
        if src.len() != dst.len() * 2 {
            return Err(());
        }
        let bytes = src.as_bytes();
        for (i, out) in dst.iter_mut().enumerate() {
            let hi = hex_val(bytes[i * 2])?;
            let lo = hex_val(bytes[i * 2 + 1])?;
            *out = (hi << 4) | lo;
        }
        Ok(())
    }

    fn hex_val(b: u8) -> Result<u8, ()> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用固定密钥（仅测试环境，不构成真实凭证）
    fn test_secret() -> Vec<u8> {
        b"unit-test-secret-key-32-bytes!!!".to_vec()
    }

    /// 在临时目录建一个唯一命名的子目录，返回路径；调用方负责清理
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let unique = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
        let dir = std::env::temp_dir().join(format!(
            "lyys_auth_test_{tag}_{}_{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 签发→校验往返：sub 保留、jti 为 32 位十六进制且两枚 token 互不相同
    #[test]
    fn jwt_issue_verify_roundtrip() {
        let secret = test_secret();
        let t1 = issue_token(&secret, 42).unwrap();
        let t2 = issue_token(&secret, 42).unwrap();
        assert_ne!(t1, t2, "同一用户两次签发的 token 必须不同（jti 随机）");

        let c1 = verify_token(&secret, &t1).unwrap();
        assert_eq!(c1.sub, 42);
        assert_eq!(c1.jti.len(), 32);
        assert!(c1.jti.chars().all(|c| c.is_ascii_hexdigit()));
        // 有效期约 24 小时（允许秒级误差）
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        assert!((c1.exp as i64 - now - 86400).abs() <= 5);

        let c2 = verify_token(&secret, &t2).unwrap();
        assert_ne!(c1.jti, c2.jti, "jti 必须唯一，否则吊销一枚会误伤另一枚");
    }

    /// 过期 token 必须被拒（手工签一枚 exp 在过去的 token）。
    /// verify_token 已显式设 leeway=0，过期即拒，无 60 秒容忍窗口。
    #[test]
    fn jwt_expired_token_rejected() {
        let secret = test_secret();
        let expired = encode(
            &Header::default(),
            &Claims {
                sub: 1,
                exp: (time::OffsetDateTime::now_utc().unix_timestamp() - 3600) as usize,
                jti: "x".repeat(32),
            },
            &EncodingKey::from_secret(&secret),
        )
        .unwrap();
        assert!(verify_token(&secret, &expired).is_err());
    }

    /// 篡改签名 / 换密钥签发的 token 必须被拒
    #[test]
    fn jwt_tampered_signature_rejected() {
        let secret = test_secret();
        let token = issue_token(&secret, 1).unwrap();

        // 1) 另一枚密钥签发的同载荷 token：签名校验失败
        let forged = issue_token(b"a-completely-different-secret!!!", 1).unwrap();
        assert!(verify_token(&secret, &forged).is_err());

        // 2) 原 token 篡改签名段首个字符：签名校验失败
        let mut parts: Vec<String> = token.split('.').map(String::from).collect();
        assert_eq!(parts.len(), 3);
        let sig = parts[2].clone();
        let replacement = if sig.starts_with('a') { 'b' } else { 'a' };
        parts[2] = format!("{replacement}{}", &sig[1..]);
        let tampered = parts.join(".");
        assert!(verify_token(&secret, &tampered).is_err());

        // 3) 载荷段篡改（改 sub 提权）：签名不匹配，同样被拒
        let mut p: Vec<String> = token.split('.').map(String::from).collect();
        p[1] = format!("{}A", p[1]);
        assert!(verify_token(&secret, &p.join(".")).is_err());
    }

    /// 吊销名单：命中拒绝、到期失效、clear 清空；与 verify_token 联动
    #[test]
    fn token_revocation_blocks_valid_token() {
        let secret = test_secret();
        let token = issue_token(&secret, 7).unwrap();
        let claims = verify_token(&secret, &token).unwrap();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();

        let list = TokenRevocations::new();
        assert!(!list.is_revoked(&claims.jti));

        // 吊销未到期条目 → 命中
        list.revoke(&claims.jti, claims.exp as i64);
        assert!(list.is_revoked(&claims.jti));
        // 其他 jti 不受牵连
        assert!(!list.is_revoked(&"y".repeat(32)));

        // exp 已过期的吊销条目视同不存在（名单只覆盖仍有效的吊销）
        list.revoke("stale-jti", now - 1);
        assert!(!list.is_revoked("stale-jti"));

        // clear（密钥轮换/改密联动）后放行
        list.clear();
        assert!(!list.is_revoked(&claims.jti));
    }

    /// 名单打满后新吊销逐出「最早过期」的一条，而不是整体清空：
    /// 已登出的其余 token 必须保持吊销状态（旧实现 clear() 会让它们复活）。
    #[test]
    fn revocation_capacity_evicts_oldest_not_all() {
        let list = TokenRevocations::new();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        // 灌满 MAX_REVOKED 条，exp 递增（第 0 条最早过期）
        for i in 0..MAX_REVOKED {
            list.revoke(&format!("jti-{i:05}"), now + 100 + i as i64);
        }
        assert!(list.is_revoked("jti-00000"));
        // 再吊销一条：只应逐出 jti-00000（最早过期），其余全部保留
        list.revoke("jti-new", now + 9999);
        assert!(!list.is_revoked("jti-00000"), "最早过期的条目被逐出");
        assert!(list.is_revoked("jti-new"), "新吊销生效");
        assert!(list.is_revoked("jti-00001"), "其余吊销记录不得丢失");
        assert!(list.is_revoked("jti-04095"), "最后灌入的条目仍在名单");
    }

    /// argon2 哈希→校验往返：正确密码通过、错误密码与坏哈希串均拒绝
    #[test]
    fn argon2_password_roundtrip() {
        let password = "S3cr3t-Admin-Pwd";
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .unwrap()
            .to_string();

        assert!(verify_password(password, &hash));
        assert!(!verify_password("wrong-password", &hash));
        assert!(!verify_password("", &hash));
        // 哈希串本身非法（如库里被写坏）：不 panic，返回 false
        assert!(!verify_password(password, "not-a-valid-phc-hash"));
    }

    /// load_jwt_secret 全路径覆盖。
    ///
    /// 本测试会读写 `PANEL_JWT_SECRET` 环境变量；Rust 2021 的 env 修改在多线程
    /// 测试下可能竞态，因此把涉及该变量的**全部**场景收进这一个测试函数内串行执行。
    #[test]
    fn load_jwt_secret_env_file_and_generate() {
        // 场景 1：环境变量密钥过短（<32 字节）→ 拒绝
        std::env::set_var("PANEL_JWT_SECRET", "tooshort");
        let dir = temp_dir("jwt");
        assert!(load_jwt_secret(&dir).is_err());

        // 场景 2：合法环境变量密钥 → 原样加载（字节即 env 值）
        let good = "0123456789abcdef0123456789abcdef"; // 恰 32 字节
        std::env::set_var("PANEL_JWT_SECRET", good);
        let loaded = load_jwt_secret(&dir).unwrap();
        assert_eq!(loaded, good.as_bytes());
        // env 优先级最高：不应落盘密钥文件
        assert!(!dir.join("jwt_secret.key").exists());
        std::env::remove_var("PANEL_JWT_SECRET");

        // 场景 3：无 env、无文件 → 生成 32 字节密钥并写盘（0600）
        let generated = load_jwt_secret(&dir).unwrap();
        assert_eq!(generated.len(), 32);
        let key_path = dir.join("jwt_secret.key");
        let on_disk = std::fs::read_to_string(&key_path).unwrap();
        assert_eq!(on_disk.len(), 64, "落盘应为 64 位十六进制");
        assert!(on_disk.chars().all(|c| c.is_ascii_hexdigit()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "密钥文件权限必须是 0600");
        }
        // 二次加载读文件，结果一致（重启不掉会话）
        assert_eq!(load_jwt_secret(&dir).unwrap(), generated);

        // 场景 4：文件内容损坏（长度不对/非十六进制）→ 明确报错而非静默重生成
        std::fs::write(&key_path, "deadbeef").unwrap();
        assert!(load_jwt_secret(&dir).is_err());
        std::fs::write(&key_path, format!("{}z", &on_disk[..63])).unwrap();
        assert!(load_jwt_secret(&dir).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 登录限流：失败后指数退避、按 IP+用户名隔离、成功后清零
    #[test]
    fn login_throttle_backoff_isolation_and_reset() {
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        let other_ip: IpAddr = "10.9.9.9".parse().unwrap();
        let t = LoginThrottle::new();

        // 无失败记录：立即可试
        assert_eq!(t.retry_after(ip, "admin"), Duration::ZERO);

        // 连续失败：退避 1s、2s、4s…… 封顶 30s
        assert_eq!(t.record_failure(ip, "admin"), Duration::from_secs(1));
        assert!(t.retry_after(ip, "admin") > Duration::ZERO);
        assert_eq!(t.record_failure(ip, "admin"), Duration::from_secs(2));
        assert_eq!(t.record_failure(ip, "admin"), Duration::from_secs(4));
        for _ in 0..8 {
            t.record_failure(ip, "admin");
        }
        assert_eq!(t.record_failure(ip, "admin"), MAX_DELAY, "退避必须封顶");

        // 隔离：不同用户名 / 不同 IP 不受牵连
        assert_eq!(t.retry_after(ip, "root"), Duration::ZERO);
        assert_eq!(t.retry_after(other_ip, "admin"), Duration::ZERO);

        // 登录成功清零该来源
        t.record_success(ip, "admin");
        assert_eq!(t.retry_after(ip, "admin"), Duration::ZERO);
    }
}
