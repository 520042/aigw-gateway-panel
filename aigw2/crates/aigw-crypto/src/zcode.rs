//! ZCode 客户端凭据信封 enc:v1 编解码（批3）。
//!
//! 一手来源：_refs/zcode2api-main/docs/development/03-data-formats.md §3
//! （从 zsw `src-tauri/src/zcrypto.rs` 全文核对定稿）。
//!
//! 格式：`enc:v1:{nonce_b64url}.{tag_b64url}.{ct_b64url}`（URL_SAFE_NO_PAD）
//! 算法：AES-256-GCM，key = SHA256(secret)（32B 无 KDF 迭代）；
//! secret 默认 = `zcode-credential-fallback:{platform}:{home}:{username}`
//! （platform ∈ win32/darwin/linux，node os.platform() 语义）；
//! env 覆盖：ZCODE_CREDENTIAL_SECRET。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "enc:v1:";

/// 派生主密钥：SHA256(secret)。
pub fn derive_key(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// 默认 secret（对齐 node os.platform() 语义的 platform 由调用方给）。
pub fn default_secret(platform: &str, home: &str, username: &str) -> String {
    format!("zcode-credential-fallback:{platform}:{home}:{username}")
}

/// Windows 本机默认 secret（%USERNAME% 语义）。
#[cfg(windows)]
pub fn default_secret_local() -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "unknown".into());
    default_secret("win32", &home, &user)
}

/// 解密 `enc:v1:n.t.c` → 明文（tag 拼回 ct 尾部喂 GCM）。
pub fn decrypt_with_secret(value: &str, secret: &str) -> Result<String, String> {
    let body = value
        .strip_prefix(PREFIX)
        .ok_or("非 enc:v1 前缀")?;
    let parts: Vec<&str> = body.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("应为三段 n.t.c，得到 {} 段", parts.len()));
    }
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let nonce_b = b64
        .decode(parts[0])
        .map_err(|e| format!("nonce 解码失败：{e}"))?;
    let tag = b64.decode(parts[1]).map_err(|e| format!("tag 解码失败：{e}"))?;
    let ct = b64.decode(parts[2]).map_err(|e| format!("ct 解码失败：{e}"))?;
    if nonce_b.len() != 12 {
        return Err(format!("nonce 应 12 字节，得 {}", nonce_b.len()));
    }
    let key = derive_key(secret);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut sealed = ct;
    sealed.extend_from_slice(&tag);
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce_b), Payload { msg: &sealed, aad: &[] })
        .map_err(|_| "GCM 认证失败（secret 不符或数据被改）".to_string())?;
    String::from_utf8(plain).map_err(|e| format!("明文非 UTF-8：{e}"))
}

/// 加密 → `enc:v1:n.t.c`（ct 尾切 16 字节作 tag）。
pub fn encrypt_with_secret(plain: &str, secret: &str) -> Result<String, String> {
    let key = derive_key(secret);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_b);
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce_b),
            Payload { msg: plain.as_bytes(), aad: &[] },
        )
        .map_err(|e| format!("GCM 加密失败：{e}"))?;
    let (ct, tag) = sealed.split_at(sealed.len() - 16);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    Ok(format!(
        "{PREFIX}{}",
        [b64.encode(nonce_b), b64.encode(tag), b64.encode(ct)].join(".")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let secret = "test-secret";
        let cases: Vec<String> =
            vec!["{}".into(), "你好世界".into(), "a".repeat(100)];
        for plain in &cases {
            let enc = encrypt_with_secret(plain, secret).unwrap();
            assert!(enc.starts_with(PREFIX));
            assert_eq!(decrypt_with_secret(&enc, secret).unwrap(), *plain);
        }
    }

    #[test]
    fn wrong_secret_fails() {
        let enc = encrypt_with_secret("data", "s1").unwrap();
        assert!(decrypt_with_secret(&enc, "s2").is_err(), "错误密钥必须认证失败");
    }

    #[test]
    fn default_secret_shape_and_local() {
        let s = default_secret("win32", r"C:\Users\zhaol", "zhaol");
        assert_eq!(
            s,
            "zcode-credential-fallback:win32:C:\\Users\\zhaol:zhaol"
        );
        // 本机形态可生成且可往返
        let s2 = default_secret_local();
        let enc = encrypt_with_secret("x", &s2).unwrap();
        assert_eq!(decrypt_with_secret(&enc, &s2).unwrap(), "x");
    }

    #[test]
    fn nonce_must_be_12() {
        // 手工构造 11 字节 nonce → 解密报错
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let bad = format!("{PREFIX}{}.{}.{}", b64.encode([0u8; 11]), b64.encode([0u8; 16]), b64.encode([1u8; 16]));
        assert!(decrypt_with_secret(&bad, "s").unwrap_err().contains("12"));
    }
}
