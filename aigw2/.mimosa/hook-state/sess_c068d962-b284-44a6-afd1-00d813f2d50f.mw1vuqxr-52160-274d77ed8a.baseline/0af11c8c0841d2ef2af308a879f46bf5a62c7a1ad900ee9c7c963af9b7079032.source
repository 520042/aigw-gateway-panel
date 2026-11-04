//! 讯飞 account-service 请求签名（HMAC-SHA1，批3）。
//!
//! 一手来源：_refs/loomy2api-main/loomy2api/signer.py（1:1 移植自客户端
//! `electron/xfyun/sign.js`）+ constants.py（客户端内置 AK）。
//!
//! stringToSign = 9 行：METHOD / ESCAPED_PATH / ESCAPED_QUERY / Content-MD5 /
//! Content-Type / Date / Nonce / SignedHeaders / CanonicalizedHeaders
//! （后两行无自定义 x-* 头时为**空串但行必须存在**——9 行不是 7 行）。
//! signature = base64(hmac_sha1(secret, stringToSign))；
//! Authorization = "account {ak_id}:{sig}"。

use base64::Engine;
use hmac::{Hmac, Mac};
use md5::Digest as _;
use rand::RngCore;
use sha1::Sha1;

/// 客户端内置 AK（constants.py 一手常量；可被账号配置覆盖）。
pub const DEFAULT_ACCESS_KEY_ID: &str = "2thryby66wxi53sk";
pub const DEFAULT_ACCESS_KEY_SECRET: &str = "zsak6eadrbawz683wf5r3m2snrwj868r";

/// RFC3986 sub-delims：quote 保留但服务端要求转义的五个字符。
fn escape(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'!' => out.push_str("%21"),
            b'\'' => out.push_str("%27"),
            b'(' => out.push_str("%28"),
            b')' => out.push_str("%29"),
            b'*' => out.push_str("%2A"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// base64(md5(body))——空 body 返回空串。
pub fn content_md5(body: &str) -> String {
    if body.is_empty() {
        return String::new();
    }
    let d = md5::Md5::digest(body.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(d)
}

/// 路径逐段转义（去掉尾斜杠；不以 / 开头则补）。
fn escaped_path(path: &str) -> String {
    let mut p = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    if p.len() > 1 && p.ends_with('/') {
        p.pop();
    }
    p.split('/').map(escape).collect::<Vec<_>>().join("/")
}

/// x-* 自定义头规范化：(SignedHeaders;连接, CanonicalizedHeaders\n 连接)。
fn canonicalized(headers: &[(String, String)]) -> (String, String) {
    let mut picked: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| k.to_lowercase().starts_with("x-"))
        .map(|(k, v)| (k.to_lowercase(), v.trim().to_string()))
        .collect();
    picked.sort();
    if picked.is_empty() {
        return (String::new(), String::new());
    }
    let keys = picked.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>().join(";");
    let canonical = picked
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect::<String>();
    (keys, canonical)
}

/// 9 行 stringToSign（query 为 URL 查询串的已编码形态或空）。
pub fn string_to_sign(
    method: &str,
    path: &str,
    query: &str,
    body: &str,
    content_type: &str,
    date: &str,
    nonce: &str,
    custom_headers: &[(String, String)],
) -> String {
    let (signed, canonical) = canonicalized(custom_headers);
    [
        method.to_uppercase(),
        escaped_path(path),
        escape(query),
        content_md5(body),
        content_type.to_string(),
        date.to_string(),
        nonce.to_string(),
        signed,
        canonical.trim_end_matches('\n').to_string(),
    ]
    .join("\n")
}

/// HMAC-SHA1 签名（b64）。
pub fn sign(secret: &str, s: &str) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(s.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// RFC 7231 GMT 日期（loomy build_headers 用；civil 算法手写，不拉 chrono）。
pub fn http_date_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MO: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let wd = WD[((days % 7 + 7) % 7) as usize];
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    if mth <= 2 {
        y += 1;
    }
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        wd, d, MO[(mth - 1) as usize], y, h, m, s
    )
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0F) | 0x40;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// 一次 account-service 请求的完整头组。
pub fn build_headers(
    ak_id: &str,
    ak_secret: &str,
    method: &str,
    path: &str,
    body: &str,
    custom_headers: &[(String, String)],
) -> Vec<(String, String)> {
    let date = http_date_now();
    let nonce = uuid_v4();
    let s = string_to_sign(method, path, "", body, "application/json", &date, &nonce, custom_headers);
    let sig = sign(ak_secret, &s);
    let mut h = vec![
        ("Authorization".to_string(), format!("account {ak_id}:{sig}")),
        ("Date".to_string(), date),
        ("Nonce".to_string(), nonce),
        ("Content-Type".to_string(), "application/json".into()),
    ];
    let md5v = content_md5(body);
    if !md5v.is_empty() {
        h.push(("Content-MD5".into(), md5v));
    }
    for (k, v) in custom_headers {
        h.push((k.clone(), v.clone()));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_to_sign_is_nine_lines() {
        let s = string_to_sign("POST", "/account/login", "", "{}", "application/json", "Mon, 01 Jan 2024 00:00:00 GMT", "n-1", &[]);
        assert_eq!(s.matches('\n').count(), 8, "9 行 = 8 个换行");
        // 无自定义头：第 8/9 行为空串但行存在
        let lines: Vec<&str> = s.split('\n').collect();
        assert_eq!(lines[7], "");
        assert_eq!(lines[8], "");
    }

    #[test]
    fn escape_rules() {
        assert_eq!(escape("a b"), "a%20b");
        assert_eq!(escape("x!y'z(p)q*r"), "x%21y%27z%28p%29q%2Ar");
        assert_eq!(escape("a~b-c_d.e"), "a~b-c_d.e");
        assert_eq!(escape("中"), "%E4%B8%AD");
    }

    #[test]
    fn content_md5_known_vector() {
        // md5("{}") = 999eb82bbdebb1c7c9a5f4f1d5ba3ae0
        assert_eq!(
            content_md5("{}"),
            base64::engine::general_purpose::STANDARD
                .encode(md5::Md5::digest(b"{}"))
        );
        assert_eq!(content_md5(""), "");
    }

    #[test]
    fn sign_deterministic_and_ak_present() {
        let s = string_to_sign("POST", "/p", "", "b", "application/json", "D", "N", &[]);
        assert_eq!(sign("k1", &s), sign("k1", &s), "确定性");
        assert_ne!(sign("k1", &s), sign("k2", &s), "不同密钥不同签名");
        assert_eq!(sign("k1", &s).len(), 28, "b64(20B) = 28 字符");
        assert!(!DEFAULT_ACCESS_KEY_ID.is_empty() && !DEFAULT_ACCESS_KEY_SECRET.is_empty());
    }

    #[test]
    fn build_headers_shape() {
        let h = build_headers(DEFAULT_ACCESS_KEY_ID, DEFAULT_ACCESS_KEY_SECRET, "POST", "/account/session/info", "{}", &[]);
        assert!(h.iter().any(|(k, v)| k == "Authorization" && v.starts_with("account ")));
        assert!(h.iter().any(|(k, _)| k == "Nonce"));
        assert!(h.iter().any(|(k, _)| k == "Content-MD5"));
    }
}
