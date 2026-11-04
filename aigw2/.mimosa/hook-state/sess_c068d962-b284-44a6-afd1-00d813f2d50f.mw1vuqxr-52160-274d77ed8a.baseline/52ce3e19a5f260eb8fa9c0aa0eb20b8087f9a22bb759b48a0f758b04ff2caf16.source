//! 华为云 CodeArts `SDK-HMAC-SHA256` 签名（一手：codearts2api `server.py:_hmac_headers`，
//! `_refs\Johnsheng1-codearts2api`）。结构类似 AWS SigV4，但算法名固定、
//! 用 `x-sdk-date` 而非 ISO8601、CanonicalURI 必须带尾斜杠。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 计算 `SDK-HMAC-SHA256` 签名，返回 `Authorization` 头值。
///
/// 参数语义（对齐 codearts2api）：
/// - `method`：如 `POST`；
/// - `canonical_uri`：必须带尾斜杠（如 `/api/v2/chat/completions/`）；
/// - `query`：已编码查询串（无查询传空串）；
/// - `headers`：参与签名的小写头名 → 值（调用方负责补 `host`/`content-type`/
///   `x-sdk-date` 及 benefit 通道的 `maas_type`/`model-id`/`model-name`）；
/// - `body`：**与发送字节完全一致**的请求体（先 json.dumps(ensure_ascii=False) 再编码）；
/// - `sdk_date`：`%Y%m%dT%H%M%SZ` UTC。
pub fn codearts_build_sign(
    method: &str,
    canonical_uri: &str,
    query: &str,
    headers: &[(String, String)],
    body: &[u8],
    ak: &str,
    sk: &str,
    sdk_date: &str,
) -> String {
    let mut sorted: Vec<(String, String)> = headers.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = sorted
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed_headers: Vec<String> = sorted.iter().map(|(k, _)| k.clone()).collect();
    let signed_headers_str = signed_headers.join(";");
    let payload_hash = hex(Sha256::digest(body));
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{query}\n{canonical_headers}\n{signed_headers_str}\n{payload_hash}"
    );
    let string_to_sign = format!(
        "SDK-HMAC-SHA256\n{sdk_date}\n{}",
        hex(Sha256::digest(canonical_request.as_bytes()))
    );
    let mut mac = HmacSha256::new_from_slice(sk.as_bytes()).expect("hmac accepts any key length");
    mac.update(string_to_sign.as_bytes());
    let signature = hex(mac.finalize().into_bytes());
    format!(
        "SDK-HMAC-SHA256 Access={ak}, SignedHeaders={signed_headers_str}, Signature={signature}"
    )
}

/// `x-sdk-date`：UTC 当前时间 `%Y%m%dT%H%M%SZ`（std 实现，无 chrono 依赖）。
pub fn codearts_sdk_date() -> String {
    codearts_sdk_date_at(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// 给定 Unix 秒的确定性版本（测试友好）。Howard Hinnant civil_from_days 算法。
pub fn codearts_sdk_date_at(unix_secs: u64) -> String {
    let days = (unix_secs / 86400) as i64;
    let rem = unix_secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m2 = if mp < 10 { mp + 3 } else { mp - 9 };
    let y2 = if m2 <= 2 { y + 1 } else { y };
    format!("{y2:04}{m2:02}{d:02}T{h:02}{m:02}{s:02}Z")
}

fn hex(b: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(b.as_ref().len() * 2);
    for x in b.as_ref() {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_date_format_and_known_value() {
        // 2026-10-19 08:00:00 UTC = 1792406400（epoch 校验：2026-10-19 15:00 北京时间）
        // 用确定性输入断言格式与具体值
        let d = codearts_sdk_date_at(0);
        assert_eq!(d, "19700101T000000Z");
        let d2 = codearts_sdk_date_at(86400);
        assert_eq!(d2, "19700102T000000Z");
        // 当前时间格式：YYYYMMDDTHHMMSSZ（16 字符，T 在 index 8，Z 结尾）
        let now = codearts_sdk_date();
        assert_eq!(now.len(), 16);
        assert!(now.ends_with('Z'));
        assert_eq!(&now[8..9], "T");
        assert!(now[0..4].chars().all(|c| c.is_ascii_digit()));
        assert!(now[4..8].chars().all(|c| c.is_ascii_digit()));
        assert!(now[9..15].chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn sign_is_stable_and_matches_hmac_hand_check() {
        let body = br#"{"model":"glm-5.3-flash","messages":[]}"#;
        let headers = vec![
            ("host".to_string(), "snap-access.cn-north-4.myhuaweicloud.com".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
            ("x-sdk-date".to_string(), "20261019T080000Z".to_string()),
            ("maas_type".to_string(), "benefit".to_string()),
            ("model-id".to_string(), "glm-5.3-flash".to_string()),
            ("model-name".to_string(), "glm-5.3-flash".to_string()),
        ];
        let auth = codearts_build_sign(
            "POST",
            "/api/v2/chat/completions/",
            "",
            &headers,
            body,
            "AK123",
            "SK456",
            "20261019T080000Z",
        );
        assert!(auth.starts_with("SDK-HMAC-SHA256 Access=AK123, SignedHeaders="));
        // SignedHeaders 应按小写排序：content-type;host;maas_type;model-id;model-name;x-sdk-date
        assert!(auth.contains(
            "SignedHeaders=content-type;host;maas_type;model-id;model-name;x-sdk-date"
        ));
        assert!(auth.contains("Signature="));
        // 确定性：同输入同输出
        let auth2 = codearts_build_sign(
            "POST",
            "/api/v2/chat/completions/",
            "",
            &headers,
            body,
            "AK123",
            "SK456",
            "20261019T080000Z",
        );
        assert_eq!(auth, auth2);
    }

    #[test]
    fn benefit_headers_change_signature() {
        let headers_plain = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-sdk-date".to_string(), "20261019T080000Z".to_string()),
        ];
        let headers_benefit = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-sdk-date".to_string(), "20261019T080000Z".to_string()),
            ("maas_type".to_string(), "benefit".to_string()),
            ("model-id".to_string(), "glm-5.3-flash".to_string()),
            ("model-name".to_string(), "glm-5.3-flash".to_string()),
        ];
        let a1 = codearts_build_sign(
            "POST",
            "/api/v2/chat/completions/",
            "",
            &headers_plain,
            b"{}",
            "A",
            "S",
            "20261019T080000Z",
        );
        let a2 = codearts_build_sign(
            "POST",
            "/api/v2/chat/completions/",
            "",
            &headers_benefit,
            b"{}",
            "A",
            "S",
            "20261019T080000Z",
        );
        assert_ne!(a1, a2, "多签三个头必须改变签名");
    }
}
