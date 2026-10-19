//! Loomy 讯飞账号服务登录（对齐 Python app/loomy_client.py 的签名协议）。
//!
//! 协议要点（实测，文档没写全的三处）：
//! - 请求体是信封 `{"base":{appid/modelid/version/devid/ua/traceid},"param":{...}}`
//! - HMAC-SHA1 签名串固定九行：METHOD/esc(path)/esc(query)/md5b64(body)/
//!   Content-Type/Date/Nonce/SignedHeaders/CanonicalHeaders
//! - Content-MD5 = base64(md5(body)) 必须同时出现在请求头和签名串
//! - 密码登录先 getPuKey 拿 SPKI 公钥+nonce，RSA PKCS1v15 加密后 byPwd
//!
//! 端点：account.xfinfr.com（登录三方式：短信 / 密码 / 粘贴 session）。

use md5::{Digest, Md5};
use serde_json::{json, Value};
use sha1::{Sha1};
use hmac::{Hmac, Mac};

const ACCOUNT_BASE: &str = "https://account.xfinfr.com";
const AK_ID: &str = "2thryby66wxi53sk";
const AK_SECRET: &str = "zsak6eadrbawz683wf5r3m2snrwj868r";
const APPID: &str = "GM3LOOMY";
const MODELID: &str = "Web";
const CLIENT_VERSION: &str = "1.0.0";
const DEVICE_ID: &str = "web";
const CLIENT_UA: &str = "Loomy|Desktop|Electron|macOS";
const SESSION_EXPIRE: i64 = 14 * 24 * 3600;

fn rfc3986_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn now_gmt() -> String {
    // RFC1123 GMT（email.utils.formatdate(usegmt=True) 等价）
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // civil from days（Howard Hinnant 算法）
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    const MON: [&str; 12] = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    const WD: [&str; 7] = ["Thu","Fri","Sat","Sun","Mon","Tue","Wed"];
    let wd = WD[(days.rem_euclid(7)) as usize];
    format!("{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT", wd, d, MON[(m - 1) as usize], y, h, mi, s)
}

fn uuid_hex() -> String {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut out = String::new();
    for _ in 0..32 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        out.push_str(&format!("{:x}", (x >> 60) & 0xF));
    }
    out
}

/// HMAC-SHA1 签名 → "account <akid>:<sig>"。
fn sign_request(method: &str, path: &str, query: &str, body_text: &str) -> (String, String, String) {
    let date = now_gmt();
    let nonce = uuid_hex();
    let esc_path = path.split('/').map(rfc3986_escape).collect::<Vec<_>>().join("/");
    let esc_query = if query.is_empty() { String::new() } else { rfc3986_escape(query) };
    let md5b64 = if body_text.is_empty() {
        String::new()
    } else {
        let mut h = Md5::new();
        h.update(body_text.as_bytes());
        base64_std(&h.finalize())
    };
    // 无 x-* 头 → SignedHeaders/CanonicalHeaders 都是空串（九行固定）
    let sts = format!(
        "{}\n{}\n{}\n{}\napplication/json\n{}\n{}\n\n",
        method.to_uppercase(), esc_path, esc_query, md5b64, date, nonce
    );
    let mut mac = Hmac::<Sha1>::new_from_slice(AK_SECRET.as_bytes()).expect("hmac key");
    mac.update(sts.as_bytes());
    let sig = base64_std(&mac.finalize().into_bytes());
    (format!("account {}:{}", AK_ID, sig), date, nonce)
}

pub(crate) fn base64_std(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 { out.push(T[(n >> 6) as usize & 63] as char); } else { out.push('='); }
        if chunk.len() > 2 { out.push(T[n as usize & 63] as char); } else { out.push('='); }
    }
    out
}

fn envelope(param: Value) -> Value {
    json!({
        "base": {
            "appid": APPID, "modelid": MODELID, "version": CLIENT_VERSION,
            "devid": DEVICE_ID, "ua": CLIENT_UA,
            "traceid": uuid_hex(),
        },
        "param": param,
    })
}

/// 调账号服务（自动签名 + 信封）。返回解析后的 JSON。
async fn account_post(path: &str, param: Value) -> Value {
    let body = envelope(param);
    let body_text = serde_json::to_string(&body).unwrap_or_default();
    let (auth, date, nonce) = sign_request("POST", path, "", &body_text);
    let mut md5h = Md5::new();
    md5h.update(body_text.as_bytes());
    let body_md5 = base64_std(&md5h.finalize());
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}{}", ACCOUNT_BASE, path))
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .header("Content-MD5", body_md5)
        .header("Accept", "application/json, text/plain, */*")
        .header("Date", date)
        .header("Nonce", nonce)
        .header("User-Agent", CLIENT_UA)
        .header("Origin", "https://loomy.xunfei.cn")
        .header("Referer", "https://loomy.xunfei.cn/")
        .body(body_text)
        .send()
        .await;
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            serde_json::from_str(&txt).unwrap_or(json!({"_http": status, "_raw": txt.chars().take(300).collect::<String>()}))
        }
        Err(e) => json!({"_http": 0, "_err": format!("{}", e)}),
    }
}

/// 发送短信验证码 → msgid。
pub async fn send_sms(phone: &str) -> Result<String, String> {
    let (ok, ph) = norm_phone(phone);
    if !ok {
        return Err(ph);
    }
    let d = account_post("/login/phone/sendMsgCode", json!({"ccode": "86", "phone": ph, "expire": 300})).await;
    let msgid = d.pointer("/data/msgid").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !msgid.is_empty() {
        return Ok(msgid);
    }
    let code = d.get("code").and_then(|v| v.as_str()).or_else(|| d.get("code").and_then(|v| v.as_i64()).map(|_| "")).unwrap_or("");
    if code == "010003" {
        return Err("账号服务返回「请求参数错误」——多数是手机号号段不存在（确认号码无误）或该号码未注册讯飞账号".to_string());
    }
    Err(d.to_string().chars().take(200).collect())
}

/// 短信验证码校验 → (session, userid)。
pub async fn verify_sms(phone: &str, code: &str, msgid: &str) -> Result<(String, String), String> {
    let (ok, ph) = norm_phone(phone);
    if !ok {
        return Err(ph);
    }
    let d = account_post("/login/phone/checkCode", json!({
        "ccode": "86", "phone": ph, "mcode": code, "msgid": msgid, "expire": SESSION_EXPIRE
    })).await;
    let session = d.pointer("/data/session").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !session.is_empty() {
        let uid = d.pointer("/data/userid").and_then(|v| v.as_str()).unwrap_or("").to_string();
        return Ok((session, uid));
    }
    Err(d.to_string().chars().take(220).collect())
}

/// 密码登录 → (session, userid)。getPuKey → RSA PKCS1v15 → byPwd。
pub async fn pwd_login(phone: &str, password: &str) -> Result<(String, String), String> {
    let (ok, ph) = norm_phone(phone);
    if !ok {
        return Err(ph);
    }
    let d = account_post("/login/account/getPuKey", json!({})).await;
    let pukey = d.pointer("/data/pukey").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let rcode = d.pointer("/data/rcode").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if pukey.is_empty() || rcode.is_empty() {
        return Err(format!("getPuKey 失败：{}", d.to_string().chars().take(200).collect::<String>()));
    }
    let enc = rsa_encrypt_pkcs1v15(&pukey, password).map_err(|e| format!("RSA 加密失败: {}", e))?;
    let d2 = account_post("/login/account/byPwd", json!({
        "loginid": ph, "password": enc, "rcode": rcode, "type": 1, "expire": SESSION_EXPIRE
    })).await;
    let session = d2.pointer("/data/session").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !session.is_empty() {
        let uid = d2.pointer("/data/userid").and_then(|v| v.as_str()).unwrap_or("").to_string();
        return Ok((session, uid));
    }
    Err(d2.to_string().chars().take(220).collect())
}

/// RSA PKCS1 v1.5 加密（SPKI DER 公钥，纯大整数 pow，不引 rsa crate）。
///
/// ★ 2026-10-15 修正：原实现把 `der_skip_len`（返回*内容结束*位置）当成了
///   Python `_der_read_len`（返回*内容起始*位置）用 —— SPKI 解析整体多跳一段，
///   第一步就 index out of bounds **panic**，导致 Loomy 密码登录连接被直接掐断
///   （前端表现为请求失败/空响应）。现严格对齐 Python `_rsa_encrypt` 的读取顺序，
///   并对每处 `der[i]` / 切片加边界保护，畸形公钥返回错误而非 panic。
fn rsa_encrypt_pkcs1v15(pubkey_b64: &str, plaintext: &str) -> Result<String, String> {
    let der = base64_decode(pubkey_b64).ok_or("公钥 base64 非法")?;
    // 边界安全的取字节
    let at = |idx: usize| -> Result<u8, String> {
        der.get(idx).copied().ok_or_else(|| "公钥 DER 越界".to_string())
    };
    // 读 TLV 长度字段，返回 (内容长度, 内容起始下标) —— 对齐 Python `_der_read_len`
    let (ln, ni) = der_read_len(&der, 1)?;
    if at(0)? != 0x30 {
        return Err("公钥 DER 形状不对".into());
    }
    let mut i = ni; // 外层 SEQUENCE 内容起点
    if at(i)? != 0x30 {
        return Err("公钥缺算法块".into());
    }
    let (la, ci) = der_read_len(&der, i + 1)?;
    i = ci + la; // 跳过 AlgorithmIdentifier(OID+NULL)
    if at(i)? != 0x03 {
        return Err("公钥缺 BIT STRING".into());
    }
    let (_lb, bi) = der_read_len(&der, i + 1)?;
    i = bi + 1; // 跳过 BIT STRING 未用位占位字节 → 指向内层 RSAPublicKey
    if at(i)? != 0x30 {
        return Err("公钥缺 RSAPublicKey".into());
    }
    let (_lr, ri) = der_read_len(&der, i + 1)?;
    i = ri;
    if at(i)? != 0x02 {
        return Err("DER 缺 modulus".into());
    }
    let (ln_n, ni_n) = der_read_len(&der, i + 1)?;
    if ni_n + ln_n > der.len() {
        return Err("DER modulus 越界".into());
    }
    let n = BigUint::from_bytes_be(&der[ni_n..ni_n + ln_n]);
    i = ni_n + ln_n;
    if at(i)? != 0x02 {
        return Err("DER 缺 exponent".into());
    }
    let (le, ei) = der_read_len(&der, i + 1)?;
    if ei + le > der.len() {
        return Err("DER exponent 越界".into());
    }
    let e = BigUint::from_bytes_be(&der[ei..ei + le]);
    let _ = ln;

    let k = (n.bits() + 7) / 8;
    let m = plaintext.as_bytes();
    if k < 11 || m.len() > k as usize - 11 {
        return Err("密码过长".into());
    }
    // PKCS1 v1.5: 00 02 + 非零随机填充 + 00 + message
    let ps_len = k as usize - 3 - m.len();
    let mut ps = Vec::with_capacity(ps_len);
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        | 1;
    while ps.len() < ps_len {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let b = (x >> 56) as u8;
        if b != 0 {
            ps.push(b);
        }
    }
    let mut em = vec![0u8, 2];
    em.extend_from_slice(&ps);
    em.push(0);
    em.extend_from_slice(m);
    let c = BigUint::from_bytes_be(&em).modpow(&e, &n);
    let cb = c.to_bytes_be();
    if cb.len() > k as usize {
        return Err("RSA 结果长度异常".into());
    }
    let mut padded = vec![0u8; k as usize - cb.len()];
    padded.extend_from_slice(&cb);
    Ok(base64_std(&padded))
}

/// 手机号归一化（对齐 Python `loomy_client.norm_phone`）：
/// 剥离 +86 / 0086 / 86 前缀与空格横线括号，再用 `1[3-9]\d{9}` 校验。
/// 返回 (ok, 归一化号码或错误说明)。
pub fn norm_phone(phone: &str) -> (bool, String) {
    let cleaned: String = phone
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '(' | ')' | '（' | '）'))
        .collect();
    let s = if let Some(r) = cleaned.strip_prefix("+86") {
        r.to_string()
    } else if let Some(r) = cleaned.strip_prefix("0086") {
        r.to_string()
    } else if cleaned.starts_with("86") && cleaned.len() == 13 {
        cleaned[2..].to_string()
    } else {
        cleaned
    };
    let ok = s.len() == 11
        && s.as_bytes()[0] == b'1'
        && (b'3'..=b'9').contains(&s.as_bytes()[1])
        && s.as_bytes()[2..].iter().all(|b| b.is_ascii_digit());
    if ok {
        (true, s)
    } else {
        (
            false,
            format!(
                "手机号格式不正确（{}）：请填 11 位大陆手机号，支持带 +86/86 前缀，不要带空格或横线",
                phone.chars().take(24).collect::<String>()
            ),
        )
    }
}

fn der_read_len(der: &[u8], mut i: usize) -> Result<(usize, usize), String> {
    if i >= der.len() { return Err("DER 越界".into()); }
    let b = der[i];
    i += 1;
    if b & 0x80 == 0 {
        return Ok((b as usize, i));
    }
    let n = (b & 0x7F) as usize;
    if n == 0 || n > 4 || i + n > der.len() { return Err("DER 长度非法".into()); }
    let mut val = 0usize;
    for k in 0..n { val = (val << 8) | der[i + k] as usize; }
    Ok((val, i + n))
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let s: Vec<char> = s.chars().filter(|c| !c.is_whitespace() && *c != '=').collect();
    let mut out = Vec::new();
    for chunk in s.chunks(4) {
        let mut n = 0u32;
        for (k, c) in chunk.iter().enumerate() {
            let v = T.iter().position(|t| *t as char == *c)?;
            n |= (v as u32) << (18 - 6 * k);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 1 { out.push((n >> 8) as u8); }
        if chunk.len() > 2 { out.push(n as u8); }
    }
    Some(out)
}

use num_bigint::BigUint;
