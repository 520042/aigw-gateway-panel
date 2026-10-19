//! Qoder COSY 推理签名与设备指纹（批3）。
//!
//! 一手来源：_refs/qoder2api-hub-main/qoder_sign.py + qoder_fingerprint.py
//! （逆向自 Qoder 桌面/CLI 官方客户端，1028 测试的项目）。
//!
//! 要点：
//! - 指纹：`md5("{salt}:{uid}")` 稳定派生 machineId/sessionId（同账号恒同虚拟设备）；
//! - 自定义 Base64：标准 b64 → 尾/中/首三段轮转（a=n//3）→ 自定义字母表 + `=`→`$`；
//! - COSY Bearer：tempKey(16 hex ASCII) → RSA PKCS#1v15 包裹（官方 1024bit 公钥）
//!   → AES-128-CBC(key==iv) 加身份体 → payload 紧凑 JSON → md5 五段签名；
//! - 头组：cosy-* 九件 + `Bearer COSY.{payload}.{sig}`，path 去掉 `/algo` 前缀。

use aes::cipher::{BlockEncryptMut, KeyIvInit};
use base64::Engine;
use md5::Digest;
use rand::RngCore;
use rsa::pkcs8::DecodePublicKey;
use serde_json::{json, Value};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

/// COSY 协议版本（官方 0.4.3 客户端内置常量）。
pub const COSY_VERSION: &str = "1.1.64";
/// 官方客户端硬编码 RSA 公钥（1024bit，与官方私钥配对，协议层不可换）。
pub const SERVER_PUB_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----";

const CUSTOM_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STD_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 由 uid+salt 稳定派生 32 位 hex 设备/会话标识（幂等）。
pub fn derive_id(uid: &str, salt: &str) -> String {
    let seed = format!("{salt}:{}", if uid.is_empty() { "anonymous" } else { uid });
    let mut h = md5::Md5::new();
    h.update(seed.as_bytes());
    format!("{:x}", h.finalize())
}

/// 18 位去横线 machine_type。
pub fn derive_machine_type(uid: &str) -> String {
    derive_id(uid, "machinetype").replace('-', "").chars().take(18).collect()
}

/// 43 字符 b64url 风格 machine_token。
pub fn derive_machine_token(uid: &str) -> String {
    use sha2::{Digest as _, Sha512};
    let raw: [u8; 64] = Sha512::digest(format!("machinetoken:{uid}").as_bytes()).into();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(raw)
        .chars()
        .take(43)
        .collect()
}

/// Qoder 自定义 Base64 出站编码。
pub fn qoder_encode(plain: &[u8]) -> String {
    let std = base64::engine::general_purpose::STANDARD.encode(plain);
    let n = std.len();
    let a = n / 3;
    let rearranged: String = format!("{}{}{}", &std[n - a..], &std[a..n - a], &std[..a]);
    rearranged
        .bytes()
        .map(|b| {
            let idx = STD_ALPHABET.iter().position(|&c| c == b);
            match idx {
                Some(i) => CUSTOM_ALPHABET[i] as char,
                None if b == b'=' => '$',
                None => b as char,
            }
        })
        .collect()
}

/// 逆运算（测试/调试）。
pub fn qoder_decode(encoded: &str) -> Vec<u8> {
    let std: String = encoded
        .chars()
        .map(|c| {
            let cc = c as u8;
            let idx = CUSTOM_ALPHABET.iter().position(|&a| a == cc);
            match idx {
                Some(i) => STD_ALPHABET[i] as char,
                None if c == '$' => '=',
                None => c,
            }
        })
        .collect();
    let n = std.len();
    let a = n / 3;
    let original = format!("{}{}{}", &std[n - a..], &std[a..n - a], &std[..a]);
    base64::engine::general_purpose::STANDARD
        .decode(original.as_bytes())
        .unwrap_or_default()
}

/// 键排序 + 无空格紧凑 JSON；**None → ""**（Python 参考实现语义，服务端字节强绑定）。
pub fn json_sorted_compact(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    let val = if map[*k].is_null() {
                        Value::String(String::new())
                    } else {
                        map[*k].clone()
                    };
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        serde_json::to_string(&val).unwrap()
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn aes128_cbc_encrypt_pkcs7(plain: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let pad = 16 - plain.len() % 16;
    let mut data = plain.to_vec();
    data.extend(std::iter::repeat(pad as u8).take(pad));
    let enc = Aes128CbcEnc::new(key.into(), iv.into());
    enc.encrypt_padded_vec_mut::<aes::cipher::block_padding::NoPadding>(&data)
}

/// 单账号 COSY 会话（token 轮换后重建）。
pub struct CosySession {
    pub uid: String,
    pub machine_id: String,
    pub machine_token: String,
    pub machine_type: String,
    temp_key: [u8; 16],
    cosy_key: String,
    info: String,
}

impl CosySession {
    /// access_token 轮换即重建会话（对齐 qoder_sign.py 语义）。
    pub fn new(uid: &str, access_token: &str) -> Result<Self, String> {
        // tempKey = uuid4().hex[:16] —— **16 个 ASCII hex 字符**（16 字节 UTF-8），
        // 整串充当 AES-128 key/iv（协议要求与服务端解包逐字节一致，风险 R3 不可改熵）
        let mut rnd = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut rnd);
        let hex_key: String = rnd.iter().map(|b| format!("{b:02x}")).collect();
        let tk = hex_key.as_bytes();

        let rsa_key = rsa::RsaPublicKey::from_public_key_pem(SERVER_PUB_PEM)
            .map_err(|e| format!("RSA 公钥解析失败：{e}"))?;
        let mut rng = rand::thread_rng();
        let sealed = rsa_key
            .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, tk)
            .map_err(|e| format!("RSA 包裹失败：{e}"))?;
        let cosy_key = base64::engine::general_purpose::STANDARD.encode(&sealed);

        let identity = json!({
            "name": "",
            "aid": uid,
            "uid": uid,
            "yx_uid": "",
            "organization_id": "",
            "organization_name": "",
            "user_type": "personal_professional_trial",
            "security_oauth_token": access_token,
            "refresh_token": "",
        });
        let plain = json_sorted_compact(&identity);
        let mut k16 = [0u8; 16];
        k16.copy_from_slice(tk);
        let info = base64::engine::general_purpose::STANDARD
            .encode(aes128_cbc_encrypt_pkcs7(plain.as_bytes(), &k16, &k16));
        Ok(CosySession {
            uid: uid.to_string(),
            machine_id: derive_id(uid, "machine"),
            machine_token: derive_machine_token(uid),
            machine_type: derive_machine_type(uid),
            temp_key: k16,
            cosy_key,
            info,
        })
    }

    /// 一次推理请求的完整 COSY 头组。`raw_url` 的 path 去掉 `/algo` 前缀参与签名。
    pub fn headers(&self, body: &str, raw_url: &str, model_key: &str) -> Vec<(String, String)> {
        use md5::Md5;
        let mut path = raw_url
            .split("://")
            .nth(1)
            .and_then(|r| r.split_once('/'))
            .map(|(_, p)| format!("/{p}"))
            .unwrap_or_else(|| "/".into());
        if let Some(stripped) = path.strip_prefix("/algo") {
            path = stripped.to_string();
        }
        let payload = json!({
            "cosyVersion": COSY_VERSION,
            "ideVersion": "",
            "info": self.info,
            "requestId": crypto_uuid(),
            "version": "v1",
        });
        let payload_b64 = base64::engine::general_purpose::STANDARD
            .encode(json_sorted_compact(&payload).as_bytes());
        let date = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .to_string();
        let raw = format!("{payload_b64}\n{}\n{date}\n{body}\n{path}", self.cosy_key);
        let sig = format!("{:x}", Md5::digest(raw.as_bytes()));
        let bearer = format!("Bearer COSY.{payload_b64}.{sig}");
        let mut h = vec![
            ("cosy-data-policy".to_string(), "AGREE".into()),
            ("content-type".to_string(), "application/json".into()),
            ("cosy-machinetype".to_string(), self.machine_type.clone()),
            ("cosy-clienttype".to_string(), "5".into()),
            ("cosy-date".to_string(), date),
            ("cosy-user".to_string(), self.uid.clone()),
            ("cosy-key".to_string(), self.cosy_key.clone()),
            ("authorization".to_string(), bearer),
            ("cosy-version".to_string(), COSY_VERSION.into()),
            ("cosy-machineid".to_string(), self.machine_id.clone()),
            ("cosy-machinetoken".to_string(), self.machine_token.clone()),
            ("user-agent".to_string(), "Go-http-client/2.0".into()),
        ];
        if !model_key.is_empty() {
            h.push(("x-model-key".into(), model_key.into()));
            h.push(("x-model-source".into(), "system".into()));
        }
        h
    }

    /// 会话 AES key（测试可见）。
    pub fn temp_key(&self) -> &[u8; 16] {
        &self.temp_key
    }
}

fn crypto_uuid() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_shapes() {
        assert_eq!(derive_id("u1", "machine").len(), 32);
        assert_eq!(derive_id("u1", "machine"), derive_id("u1", "machine"), "幂等");
        assert_ne!(derive_id("u1", "machine"), derive_id("u2", "machine"));
        assert_eq!(derive_machine_type("u1").len(), 18);
        assert!(!derive_machine_type("u1").contains('-'));
        assert_eq!(derive_machine_token("u1").len(), 43);
    }

    #[test]
    fn custom_base64_roundtrip() {
        for case in [b"hello".to_vec(), b"a".to_vec(), vec![0u8; 32], b"qoder-cosy-identity-json".to_vec()] {
            let enc = qoder_encode(&case);
            assert!(!enc.contains('='), "padding 应映射为 $");
            assert_eq!(qoder_decode(&enc), case);
        }
        // 三段轮转确实改变顺序（与标准 b64 不同）
        let enc = qoder_encode(b"hello world 12345678");
        let std = base64::engine::general_purpose::STANDARD.encode(b"hello world 12345678");
        assert_ne!(enc.chars().take(8).collect::<String>(), std.chars().take(8).collect::<String>());
    }

    #[test]
    fn json_sorted_compact_sorts_keys() {
        let v: Value = serde_json::from_str(r#"{"z":1,"a":"x","m":null}"#).unwrap();
        assert_eq!(json_sorted_compact(&v), r#"{"a":"x","m":"","z":1}"#);
    }

    #[test]
    fn cosy_session_bearer_shape() {
        let s = CosySession::new("uid-test-1", "at-test").unwrap();
        let h = s.headers("{}", "https://api1.qoder.sh/algo/inference", "mdl-1");
        let auth = h.iter().find(|(k, _)| k == "authorization").unwrap();
        assert!(auth.1.starts_with("Bearer COSY."), "{auth:?}");
        // 段位：[0]="Bearer COSY" [1]=payload [2]=sig
        let payload = auth.1.split('.').nth(1).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let txt = String::from_utf8(decoded).unwrap();
        assert!(txt.contains(&format!("\"cosyVersion\":\"{COSY_VERSION}\"")));
        assert!(txt.contains("\"version\":\"v1\""));
        assert!(h.iter().any(|(k, v)| k == "cosy-machineid" && !v.is_empty()));
        assert!(h.iter().any(|(k, v)| k == "x-model-key" && v == "mdl-1"));
        // 签名段 32 hex
        let sig = auth.1.split('.').last().unwrap();
        assert_eq!(sig.len(), 32);
    }
}
