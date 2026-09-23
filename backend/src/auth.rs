use anyhow::Result;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, SaltString},
    Argon2, PasswordHasher, PasswordVerifier,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
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
}

/// 生成随机 JWT 密钥（64 位十六进制字符串）
fn random_secret() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 32] = rng.gen();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从数据库读取或首次生成 JWT 密钥
pub fn load_or_create_secret(db: &Db) -> Result<[u8; 32]> {
    let secret = match db.get_setting("jwt_secret")? {
        Some(s) => s,
        None => {
            let s = random_secret();
            db.set_setting("jwt_secret", &s)?;
            s
        }
    };
    let mut key = [0u8; 32];
    anyhow::ensure!(secret.len() == 64, "JWT 密钥长度异常");
    hex::decode_to_slice(&secret, &mut key).map_err(|_| anyhow::anyhow!("解析 JWT 密钥失败"))?;
    Ok(key)
}

/// 签发 token，有效期 24 小时
pub fn issue_token(secret: &[u8; 32], user_id: i64) -> Result<String> {
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 60 * 60 * 24;
    let claims = Claims {
        sub: user_id,
        exp: exp as usize,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )?;
    Ok(token)
}

/// 校验 token 并返回用户 id
pub fn verify_token(secret: &[u8; 32], token: &str) -> Result<i64> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::default(),
    )?;
    Ok(data.claims.sub)
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

/// 首次启动时引导管理员账号（用户名 admin，密码从环境变量 PANEL_ADMIN_PASSWORD 读取，
/// 未设置则生成随机密码并打印到日志）。
pub fn ensure_admin(db: &Db) -> Result<()> {
    if db.user_count()? > 0 {
        return Ok(());
    }
    let password = std::env::var("PANEL_ADMIN_PASSWORD").unwrap_or_else(|_| {
        let pwd: String = (0..16)
            .map(|_| rand::thread_rng().gen_range('a'..='z'))
            .collect();
        tracing::warn!("未设置 PANEL_ADMIN_PASSWORD，已生成随机管理员密码：{pwd}");
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
