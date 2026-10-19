//! aigw-crypto：各平台签名/加密算法库。
//!
//! - [`glm_build_sign`]：智谱清言 X-Sign 头组（M2，一手：web.rs/glm_auth.py）。
//! - [`qoder`]：COSY 推理签名 + 设备指纹（批3，一手：qoder2api-hub qoder_sign.py/fingerprint.py）。
//! - [`loomy`]：account-service HMAC-SHA1 签名（批3，一手：loomy2api signer.py/constants.py）。
//! - [`zcode`]：enc:v1 凭据信封 AES-256-GCM（批3，一手：zcode2api docs/03 定稿）。
//! 待补：SDK-HMAC-SHA256（codearts）。

pub mod loomy;
pub mod qoder;
pub mod zcode;

use md5::{Digest, Md5};

/// 智谱清言签名密钥（glm_auth.py 一手常量）。
pub const GLM_SIGN_SECRET: &str = "8a1317a7468aa3ad86e997d08f3f31cb";

/// 随机 32 位 hex nonce（替代 uuid4().hex，无 uuid 依赖；纳秒+进程内计数器）。
pub fn nonce_hex32() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = C.fetch_add(1, Ordering::Relaxed);
    let s = format!("{n:x}{c:x}");
    if s.len() >= 32 {
        s[..32].to_string()
    } else {
        format!("{:0<32}", s)
    }
}

/// 复刻 glm2api `build_sign()`：毫秒时间戳，**倒数第二位是校验位**。
///
/// 校验位 = (各位数字和 - 倒数第二位原值) % 10；替换后作为 X-Timestamp。
/// 返回 `(timestamp, nonce, sign)`；sign = md5("{ts}-{nonce}-{SECRET}") 小写 hex。
pub fn glm_build_sign() -> (String, String, String) {
    glm_build_sign_at(now_millis())
}

/// 给定毫秒时间戳的确定性版本（测试友好）。
pub fn glm_build_sign_at(now_ms: u128) -> (String, String, String) {
    let now = format!("{now_ms}");
    let digits: Vec<i64> = now
        .chars()
        .filter_map(|c| c.to_digit(10).map(|d| d as i64))
        .collect();
    let checksum = (digits.iter().sum::<i64>() - digits[digits.len() - 2]) % 10;
    let timestamp = format!(
        "{}{}{}",
        &now[..now.len() - 2],
        checksum,
        &now[now.len() - 1..]
    );
    let nonce = nonce_hex32();
    let sign = format!(
        "{:x}",
        Md5::digest(format!("{}-{}-{}", timestamp, nonce, GLM_SIGN_SECRET).as_bytes())
    );
    (timestamp, nonce, sign)
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_replaces_second_to_last_digit() {
        let ts = 1792135877123u128; // 13 位
        let (timestamp, _, _) = glm_build_sign_at(ts);
        let orig = ts.to_string();
        assert_eq!(timestamp.len(), orig.len());
        // 除倒数第二位外全部相同
        assert_eq!(&timestamp[..timestamp.len() - 2], &orig[..orig.len() - 2]);
        assert_eq!(&timestamp[timestamp.len() - 1..], &orig[orig.len() - 1..]);
        // 校验位 = (原串各位和 - 原串倒数第二位) % 10（glm2api Rust 复刻语义）
        let od: Vec<i64> = orig.chars().filter_map(|c| c.to_digit(10).map(|d| d as i64)).collect();
        let expected = (od.iter().sum::<i64>() - od[od.len() - 2]) % 10;
        let nd: Vec<i64> = timestamp.chars().filter_map(|c| c.to_digit(10).map(|d| d as i64)).collect();
        assert_eq!(nd[nd.len() - 2], expected);
    }

    #[test]
    fn sign_is_deterministic_md5_hex() {
        let (ts1, n1, s1) = glm_build_sign_at(1234567890123);
        let (_, _, s1b) = {
            // 同 ts + 同 nonce → 同 sign：用 nonce_hex32 不可控，改为直接验证格式 + 与手工 md5 一致
            let sign = format!(
                "{:x}",
                Md5::digest(format!("{}-{}-{}", ts1, n1, GLM_SIGN_SECRET).as_bytes())
            );
            (ts1, n1, sign)
        };
        assert_eq!(s1, s1b);
        assert_eq!(s1.len(), 32);
        assert!(s1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn nonce_is_32_hex() {
        let n = nonce_hex32();
        assert_eq!(n.len(), 32);
        assert!(n.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
