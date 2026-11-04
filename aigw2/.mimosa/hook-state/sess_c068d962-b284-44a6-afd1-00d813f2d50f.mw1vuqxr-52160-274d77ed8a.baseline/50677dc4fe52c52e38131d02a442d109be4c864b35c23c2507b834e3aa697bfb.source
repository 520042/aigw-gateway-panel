//! OfficeAce（华为云果办）的两套请求签名：**SDK-HMAC-SHA256** 与 **V11-HMAC-SHA256**。
//!
//! ── 为什么不复用 codearts 的签名 ────────────────────────────────
//! 两家同属华为云、算法同名，但**规范化规则有三处实测差异**：
//!   1. **未保留集更严**：OfficeAce 的 JS 签名器先 `encodeURIComponent`、再把
//!      `! ' ( ) *` 这五个字符也转义（`rfc3986`）。路径或查询里出现它们时两边签名不同。
//!   2. **查询串按 WHATWG 口径**：JS 走 `URL.searchParams`（键值先解码再重新编码、
//!      `+` 折成空格、按 (键, 值) 排序），不是 Go `ParseQuery` 的「坏 % 丢一对」语义。
//!   3. **V11 的派生密钥按「小写十六进制字符串」参与 HMAC**，不是 32 字节原始 key。
//!
//! ★ 正确性由 `signer_vectors.json` 的 6 个用例客观验证（见本文件测试）——
//!   每个用例都给了 canonical_request / string_to_sign / signature 三层中间值，
//!   所以算错了能立刻知道错在哪一层，不必猜。

use sha2::{Digest, Sha256};

/// 一个待签名请求。
pub struct SignRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub security_token: &'a str,
    pub project_id: &'a str,
    /// 参与签名的额外头（小写名 → 值）。
    pub extra_headers: Vec<(String, String)>,
    pub content_type: Option<&'a str>,
    pub body: &'a str,
    /// `YYYYMMDDTHHMMSSZ`。
    pub sdk_date: &'a str,
    /// V11 用：区域（如 cn-southwest-2）。
    pub region: &'a str,
    /// V11 用：服务名（如 apic）。
    pub service: &'a str,
}

/// V11 的派生范围：<yyyymmdd>/<region>/<service>。
fn v11_scope(sdk_date: &str, region: &str, service: &str) -> String {
    let day: String = sdk_date.chars().take(8).collect();
    format!("{day}/{region}/{service}")
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
}

/// RFC3986 百分号编码（OfficeAce 口径：`! ' ( ) *` **不**保留）。
fn encode_component(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for &b in bytes {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

fn unhex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_component(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (unhex(bytes[i + 1]), unhex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// 规范化 URI：每段「先解码再编码」，末尾**必须**有斜杠。
fn canonical_uri(path: &str) -> String {
    let path = if path.is_empty() { "/" } else { path };
    let escaped = path
        .split('/')
        .map(|seg| encode_component(&decode_component(seg)))
        .collect::<Vec<_>>()
        .join("/");
    if escaped.ends_with('/') {
        escaped
    } else {
        format!("{escaped}/")
    }
}

fn decode_form(input: &str) -> String {
    let bytes = decode_component(&input.replace('+', " "));
    String::from_utf8_lossy(&bytes).to_string()
}

/// 规范化查询串：WHATWG 口径（先解码、按 (键,值) 排序、再编码）。
fn canonical_query(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(String, String)> = raw
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decode_form(k), decode_form(v)),
            None => (decode_form(p), String::new()),
        })
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_component(k.as_bytes()), encode_component(v.as_bytes())))
        .collect::<Vec<_>>()
        .join("&")
}

fn split_path_query(url: &str) -> Result<(String, String), String> {
    let rest = url
        .split_once("://")
        .map(|(_, r)| r)
        .ok_or_else(|| format!("地址缺少 scheme：{url}"))?;
    let no_frag = rest.split('#').next().unwrap_or(rest);
    let (_, path_query) = match no_frag.find('/') {
        Some(i) => no_frag.split_at(i),
        None => (no_frag, "/"),
    };
    Ok(match path_query.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (path_query.to_string(), String::new()),
    })
}

fn authority_of(url: &str) -> Result<String, String> {
    let rest = url
        .split_once("://")
        .map(|(_, r)| r)
        .ok_or_else(|| format!("地址缺少 scheme：{url}"))?;
    let no_frag = rest.split('#').next().unwrap_or(rest);
    let host = match no_frag.find('/') {
        Some(i) => &no_frag[..i],
        None => no_frag,
    };
    Ok(host.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// HMAC-SHA256 的**字节**返回版（V11 派生链要在字节层面截断与 hex 化）。
fn hmac_sha256(key: &[u8], msg: &str) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let mut h = Sha256::new();
        h.update(key);
        k[..32].copy_from_slice(&h.finalize());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg.as_bytes());
    let inner_d = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_d);
    outer.finalize().to_vec()
}

/// HMAC-SHA256（手写，避免为本模块新增 hmac 依赖）。
///
/// 结构就是 RFC 2104：K' 补齐/哈希到块长（64 字节）→ `H((K'^opad) || H((K'^ipad) || msg))`。
fn hmac_sha256_hex(key: &[u8], msg: &str) -> String {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let mut h = Sha256::new();
        h.update(key);
        let d = h.finalize();
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg.as_bytes());
    let inner_d = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_d);
    outer.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 签名结果的三层中间值（测试与排障用）。
pub struct Material {
    pub canonical_request: String,
    pub string_to_sign: String,
    pub signature: String,
}

/// 核心：算 canonical request → string to sign → signature。
pub fn sign_material(req: &SignRequest<'_>, algorithm: &str) -> Result<Material, String> {
    let (path, query) = split_path_query(req.url)?;
    let host = authority_of(req.url)?;

    // 参与签名的头：host / x-project-id / x-sdk-date / x-security-token / 额外头
    let mut headers: Vec<(String, String)> = vec![
        ("host".to_string(), host),
        ("x-project-id".to_string(), req.project_id.to_string()),
        ("x-sdk-date".to_string(), req.sdk_date.to_string()),
        ("x-security-token".to_string(), req.security_token.to_string()),
    ];
    // content-type 存在时必须参与签名（POST JSON 的用例就是这么签的）
    if let Some(ct) = req.content_type {
        if !ct.trim().is_empty() {
            headers.push(("content-type".to_string(), ct.to_string()));
        }
    }
    for (k, v) in &req.extra_headers {
        headers.push((k.to_ascii_lowercase(), v.clone()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed_headers: String = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let payload_hash = sha256_hex(req.body.as_bytes());

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method.to_ascii_uppercase(),
        canonical_uri(&path),
        canonical_query(&query),
        canonical_headers,
        signed_headers,
        payload_hash
    );

    // V11 的 string_to_sign 比 SDK 多一行「派生范围」：<yyyymmdd>/<region>/<service>。
    // SDK 是三行（算法/时间/hash），V11 是四行 —— 向量测试第一遍就抓到了这处差异。
    let string_to_sign = if algorithm == "V11-HMAC-SHA256" {
        format!(
            "{}\n{}\n{}\n{}",
            algorithm,
            req.sdk_date,
            v11_scope(req.sdk_date, req.region, req.service),
            sha256_hex(canonical_request.as_bytes())
        )
    } else {
        format!(
            "{}\n{}\n{}",
            algorithm,
            req.sdk_date,
            sha256_hex(canonical_request.as_bytes())
        )
    };

    let signature = if algorithm == "V11-HMAC-SHA256" {
        // ★ V11 的派生链（逐字对齐参考 signV11）：
        //   ① step1   = HMAC-SHA256(key = **accessKey**, msg = **secretKey**)  ← 注意 key/msg 是反的！
        //   ② derived = HMAC-SHA256(step1, scope ++ 0x01) 取**前 32 字节**
        //   ③ signingKey = derived 的**小写十六进制字符串**（不是那 32 个字节）
        //   ④ signature  = HMAC-SHA256(signingKey_hex, stringToSign)
        //   两处最容易写错：① 的 key 是 AK 而不是 SK；③ 把 hex 串当 key 而不是字节。
        //   向量测试对这两处都会立刻报错（signature 完全不同）。
        let step1 = hmac_sha256(req.access_key.as_bytes(), req.secret_key);
        let scope = v11_scope(req.sdk_date, req.region, req.service);
        let mut seed = scope.into_bytes();
        seed.push(1);
        let derived = hmac_sha256(&step1, &String::from_utf8_lossy(&seed));
        let signing_key_hex: String = derived.iter().take(32).map(|b| format!("{b:02x}")).collect();
        hmac_sha256_hex(signing_key_hex.as_bytes(), &string_to_sign)
    } else {
        hmac_sha256_hex(req.secret_key.as_bytes(), &string_to_sign)
    };

    Ok(Material {
        canonical_request,
        string_to_sign,
        signature,
    })
}

/// 生成要发的 `Authorization` 头值。
pub fn authorization(req: &SignRequest<'_>, algorithm: &str) -> Result<String, String> {
    let m = sign_material(req, algorithm)?;
    let mut names: Vec<String> = vec![
        "host".to_string(),
        "x-project-id".to_string(),
        "x-sdk-date".to_string(),
        "x-security-token".to_string(),
    ];
    if let Some(ct) = req.content_type {
        if !ct.trim().is_empty() {
            names.push("content-type".to_string());
        }
    }
    for (k, _) in &req.extra_headers {
        names.push(k.to_ascii_lowercase());
    }
    names.sort();
    names.dedup();
    if algorithm == "V11-HMAC-SHA256" {
        // V11 的 Authorization 形状与 SDK 不同：Credential=<AK>/<scope> 而不是 Access=<AK>
        Ok(format!(
            "{algorithm} Credential={}/{}, SignedHeaders={}, Signature={}",
            req.access_key,
            v11_scope(req.sdk_date, req.region, req.service),
            names.join(";"),
            m.signature
        ))
    } else {
        Ok(format!(
            "{algorithm} Access={}, SignedHeaders={}, Signature={}",
            req.access_key,
            names.join(";"),
            m.signature
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// 6 个向量，三层中间值逐一比对 —— 算错了能立刻知道错在哪一层。
    ///
    /// 向量来自 `signer_vectors.json`（由参考项目的真实 signV4/signV11 产出）。
    #[test]
    fn matches_all_reference_vectors() {
        let raw = include_str!("officeace_vectors.json");
        let doc: Value = serde_json::from_str(raw).expect("vectors json");
        let cred = &doc["credential"];
        let ak = cred["accessKey"].as_str().unwrap();
        let sk = cred["secretKey"].as_str().unwrap();
        let st = cred["securityToken"].as_str().unwrap();
        let pid = cred["projectId"].as_str().unwrap();
        let cases = doc["cases"].as_array().expect("cases");
        assert!(cases.len() >= 6, "至少 6 个用例");

        for case in cases {
            let name = case["name"].as_str().unwrap_or("?");
            let algorithm = match case["algorithm"].as_str().unwrap_or("SDK") {
                "V11" => "V11-HMAC-SHA256",
                _ => "SDK-HMAC-SHA256",
            };
            let mut extra = Vec::new();
            if let Some(hs) = case["extra_headers"].as_object() {
                for (k, v) in hs {
                    extra.push((k.to_ascii_lowercase(), v.as_str().unwrap_or("").to_string()));
                }
            }
            let req = SignRequest {
                method: case["method"].as_str().unwrap_or("GET"),
                url: case["url"].as_str().unwrap_or(""),
                access_key: ak,
                secret_key: sk,
                security_token: st,
                project_id: pid,
                extra_headers: extra,
                content_type: case["content_type"].as_str(),
                body: case["body"].as_str().unwrap_or(""),
                sdk_date: case["x_sdk_date"].as_str().unwrap_or(""),
                region: doc["region"].as_str().unwrap_or("cn-southwest-2"),
                service: "apic",
            };
            let m = sign_material(&req, algorithm).expect("sign");
            assert_eq!(
                m.canonical_request,
                case["canonical_request"].as_str().unwrap(),
                "[{name}] canonical_request 不一致"
            );
            assert_eq!(
                m.string_to_sign,
                case["string_to_sign"].as_str().unwrap(),
                "[{name}] string_to_sign 不一致"
            );
            assert_eq!(
                m.signature,
                case["signature"].as_str().unwrap(),
                "[{name}] signature 不一致"
            );
            assert_eq!(
                authorization(&req, algorithm).expect("auth"),
                case["authorization"].as_str().unwrap(),
                "[{name}] authorization 不一致"
            );
        }
    }

    /// RFC3986 编码：`! ' ( ) *` 必须被转义（与 codearts 的口径不同）。
    #[test]
    fn encodes_js_reserved_chars() {
        assert_eq!(encode_component(b"!()*'"), "%21%28%29%2A%27");
        assert_eq!(encode_component(b"Az09-_.~"), "Az09-_.~");
    }

    /// 规范化 URI 末尾必须有斜杠。
    #[test]
    fn canonical_uri_trailing_slash() {
        assert_eq!(canonical_uri("/v1/subscription"), "/v1/subscription/");
        assert_eq!(canonical_uri("/v1/subscription/"), "/v1/subscription/");
        assert_eq!(canonical_uri(""), "/");
    }

    /// 查询串按 (键, 值) 排序，`+` 折成空格。
    #[test]
    fn canonical_query_whatwg() {
        assert_eq!(canonical_query("b=2&a=1"), "a=1&b=2");
        assert_eq!(canonical_query("k=a+b"), "k=a%20b");
        assert_eq!(canonical_query(""), "");
    }
}
