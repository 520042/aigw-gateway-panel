//! aigw-crypto：各平台签名/加密算法库。
//!
//! - [`glm_build_sign`]：智谱清言 X-Sign 头组（M2，一手：web.rs/glm_auth.py）。
//! - [`qoder`]：COSY 推理签名 + 设备指纹（批3，一手：qoder2api-hub qoder_sign.py/fingerprint.py）。
//! - [`loomy`]：account-service HMAC-SHA1 签名（批3，一手：loomy2api signer.py/constants.py）。
//! - [`zcode`]：enc:v1 凭据信封 AES-256-GCM（批3，一手：zcode2api docs/03 定稿）。
//! - [`codearts`]：华为云 SDK-HMAC-SHA256 签名（批4，一手：codearts2api server.py `_hmac_headers`）。

pub mod codearts;
pub mod loomy;
pub mod qoder;
pub mod zcode;

pub use codearts::{codearts_build_sign, codearts_sdk_date};

use md5::{Digest, Md5};

/// 智谱清言签名密钥（glm_auth.py 一手常量）。
pub const GLM_SIGN_SECRET: &str = "8a1317a7468aa3ad86e997d08f3f31cb";

/// 随机 32 位 hex nonce（替代 uuid4().hex，无 uuid 依赖）。
pub fn nonce_hex32() -> String {
    use rand::RngCore;
    // ★ 旧实现是「纳秒 + 进程内计数器」再 `{:0<32}` 右补 '0'：尾部恒定是 0，而且
    //   不是 CSPRNG（同一纳秒窗口内的取值可枚举）。它被用作上游签名的 nonce，
    //   这里直接取 16 字节 CSPRNG 转 hex。
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
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
    // 倒数第二位当校验位：至少要有 3 位才能找到「倒数第二」。
    // 生产时间戳是 13 位毫秒，但入参是裸 u128，测试或误传 0..=99 时会
    // 在 now.len()-2 处 usize 下溢 panic（debug 构建直接 abort）。
    let now = if now.len() < 3 {
        format!("{:0>3}", now)
    } else {
        now
    };
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

    #[test]
    fn glm_sign_tolerates_short_timestamps() {
        // 旧实现 now.len()-2 在 1~2 位入参上 usize 下溢 panic
        for v in [0u128, 5, 42] {
            let (ts, nonce, sign) = glm_build_sign_at(v);
            assert!(ts.chars().all(|c| c.is_ascii_digit()), "ts={ts}");
            assert_eq!(nonce.len(), 32);
            assert_eq!(sign.len(), 32);
        }
        // 正常 13 位仍然只替换倒数第二位、长度不变
        let (ts, _, _) = glm_build_sign_at(1_700_000_000_123);
        assert_eq!(ts.len(), 13);
        assert_eq!(&ts[..11], "17000000001");
    }

    #[test]
    fn nonce_is_32_hex_and_not_padded() {
        let a = nonce_hex32();
        let b = nonce_hex32();
        assert_eq!(a.len(), 32, "nonce 必须是 32 位 hex：{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "nonce 含非 hex 字符：{a}");
        assert_ne!(a, b, "两次 nonce 撞了");
        // 旧实现右补 '0'，低 8 位恒为 0；CSPRNG 版本不该出现这种固定尾部
        assert!(!a.ends_with("00000000"), "nonce 尾部全 0，说明还在用补零实现：{a}");
    }
}
