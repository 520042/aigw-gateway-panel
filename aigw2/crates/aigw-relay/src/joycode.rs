//! 京东云 JoyCode 上游适配（对齐 magpie-community/plugins packages/joycode）。
//!
//! ## 为什么需要专门的 adapter
//! JoyCode 不是标准 OpenAI 兼容：对话走一个**带签名的查询串**网关
//! `POST https://api-ai.jd.com/api?appid=…&functionId=…&t=…&sign=…`，
//! 响应体才是 OpenAI 形态。签名算法（从 JoyCode IDE 的 extension.js module 68327 抄出）：
//!
//! ```text
//! merged = { ...extra, appid, functionId, t: Date.now() }
//! h      = sortedKeys.map(k => merged[k]).filter(非空).join('&')
//! sign   = hex(HMAC-SHA256(h, SIGN_KEY))
//! ```
//!
//! **时间戳 t 是签名材料的一部分**，所以每次请求都必须重算，绝不能缓存。
//!
//! ## SIGN_KEY 的性质
//! 它是**公开协议常量**（打在每份 IDE 里，只用来标识客户端），不是密钥——
//! 单靠它无法授权任何请求。真正的账号凭据是 `ptKey` 头。
//! JoyCode 若轮换该 key，从 extension.js 重新提取并更新这里即可。

use crate::HttpClient;
use serde_json::Value;

const BASE: &str = "https://api-ai.jd.com";
const GATEWAY_PATH: &str = "/api";
const APPID: &str = "joycode_ide";
const FUNCTION_ID: &str = "chat_completions";
const LOGIN_TYPE: &str = "PIN_JD_CLOUD";
/// 公开协议常量（打在每份 JoyCode IDE 里，非密钥）。
const SIGN_KEY: &str = "0691a3f0b37b4a85aeb63ad0fc7db3ed";

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 构造带签名的网关 URL。
///
/// 签名材料的键按字典序排序后取值、过滤空值、用 `&` 连接——
/// 与 IDE 的实现逐字一致（顺序错了上游就验不过）。
pub fn signed_url() -> String {
    let t = now_ms().to_string();
    // 固定四元组：appid / functionId / t（extra 为空时就是这三个）
    let mut pairs: Vec<(&str, &str)> = vec![
        ("appid", APPID),
        ("functionId", FUNCTION_ID),
        ("t", t.as_str()),
    ];
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    let material = pairs
        .iter()
        .map(|(_, v)| *v)
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("&");
    let sig = hmac_sha256_hex(SIGN_KEY, &material);
    format!(
        "{BASE}{GATEWAY_PATH}?appid={APPID}&functionId={FUNCTION_ID}&t={t}&sign={sig}"
    )
}

fn hmac_sha256_hex(key: &str, msg: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac key");
    mac.update(msg.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 上游需要的凭据头：`ptKey` 是账号凭据，`loginType` 固定。
fn headers(rb: reqwest::RequestBuilder, pt_key: &str, tenant: &str) -> reqwest::RequestBuilder {
    let mut rb = rb
        .header("ptKey", pt_key)
        .header("loginType", LOGIN_TYPE)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json");
    if !tenant.is_empty() {
        rb = rb.header("tenant", tenant);
    }
    rb
}

/// 凭据解析：支持 `ptKey` 裸值，或 JSON `{"ptKey":"…","tenant":"…"}`。
fn split_secret(secret: &str) -> (String, String) {
    let s = secret.trim();
    if s.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(s) {
            let key = v
                .get("ptKey")
                .or_else(|| v.get("pt_key"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let tenant = v.get("tenant").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if !key.is_empty() {
                return (key, tenant);
            }
        }
    }
    (s.to_string(), String::new())
}

/// 非流式对话。
pub async fn chat_nonstream(
    http: &HttpClient,
    secret: &str,
    body: &[u8],
) -> Result<Value, String> {
    let (pt_key, tenant) = split_secret(secret);
    if pt_key.is_empty() {
        return Err("JoyCode 凭据为空（需要 ptKey）".into());
    }
    let url = signed_url();
    let rb = headers(http.post(&url), &pt_key, &tenant)
        .timeout(std::time::Duration::from_secs(60))
        .body(body.to_vec());
    let resp = rb.send().await.map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("JoyCode HTTP {status}：{}", text.chars().take(300).collect::<String>()));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|_| format!("JoyCode 响应非 JSON：{}", text.chars().take(200).collect::<String>()))
}

/// 验活：发一条极小请求（max_tokens=1），能拿到 choices 即算通过。
pub async fn verify(http: &HttpClient, secret: &str) -> bool {
    let (pt_key, tenant) = split_secret(secret);
    if pt_key.is_empty() {
        return false;
    }
    let body = serde_json::json!({
        "model": "JoyAI-Code-1.6",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1,
        "stream": false
    });
    chat_nonstream(http, secret, body.to_string().as_bytes())
        .await
        .map(|v| v.get("choices").is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_url_has_all_params() {
        let u = signed_url();
        assert!(u.starts_with("https://api-ai.jd.com/api?"));
        assert!(u.contains("appid=joycode_ide"));
        assert!(u.contains("functionId=chat_completions"));
        assert!(u.contains("&t="));
        assert!(u.contains("&sign="));
        // sign 是 64 位 hex
        let sign = u.split("&sign=").nth(1).unwrap();
        assert_eq!(sign.len(), 64, "HMAC-SHA256 应为 64 hex：{sign}");
        assert!(sign.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn signed_url_changes_between_calls() {
        // t 参与签名 → 两次调用必须不同（时间戳变了）
        let a = signed_url();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = signed_url();
        assert_ne!(a, b, "时间戳参与签名，结果不该相同");
    }

    #[test]
    fn hmac_matches_known_vector() {
        // HMAC-SHA256("key", "The quick brown fox jumps over the lazy dog")
        let got = hmac_sha256_hex("key", "The quick brown fox jumps over the lazy dog");
        assert_eq!(
            got,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn split_secret_plain_and_json() {
        assert_eq!(split_secret("ptkey123"), ("ptkey123".to_string(), String::new()));
        let (k, t) = split_secret(r#"{"ptKey":"abc","tenant":"t1"}"#);
        assert_eq!(k, "abc");
        assert_eq!(t, "t1");
    }
}
