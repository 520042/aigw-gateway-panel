use crate::server::state::AppState;
pub(crate) fn now_ms_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 极简 percent-encode（回调 URL 塞进 query 用）。
pub(crate) fn urlencoding_lite(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 标准 UUID v4 字符串（36 位带横线）—— 对齐 Python `str(uuid.uuid4())`。
/// ★ 授权页服务端按 UUID 形态校验 state；裸 hex 会直接「登录失败」。
pub(crate) fn uuid_v4() -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(format!(
        "{}|{}|{}",
        nanos_hex(),
        std::process::id(),
        nanos_hex() ^ 0x9E3779B97F4A7C15
    ));
    let mut b = h.finalize();
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    let x: String = b.iter().map(|byte| format!("{:02x}", byte)).collect();
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

/// 当前本地时间串 "YYYY-MM-DD HH:MM:SS"。
/// ★ 2026-10-16 修：原来直接用 UTC 秒数格式化，面板在中文环境（UTC+8）下
///   所有时间戳都少 8 小时（用户 01:06 的动作显示成"前一天 17:06"）。
///   这里固定按 UTC+8 折算 —— 面板只面向国内用户，不做多时区。
pub(crate) fn now_local() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + 8 * 3600; // UTC+8（北京时间）
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let sod = (secs % 86400) as u32;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60
    )
}

/// 当前 Unix 时间戳（秒，带小数）—— 缓存新鲜度用
pub(crate) fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(crate) fn nanos_hex() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub(crate) fn base64url_nopad(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 { out.push(T[(n >> 6) as usize & 63] as char); }
        if chunk.len() > 2 { out.push(T[n as usize & 63] as char); }
    }
    out
}

/// 取平台在 catalog 里声明的 hosts（CDP 抓 cookie 时按域名过滤用）
pub(crate) fn platform_hosts(st: &AppState, pid: &str) -> Vec<String> {
    st.inner
        .catalog
        .platforms()
        .get(pid)
        .and_then(|p| p.get("hosts"))
        .and_then(|h| h.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// 当前面板端口（main 装配时写入；回调/登录 URL 用）。
pub fn port_of_panel() -> u16 {
    *PANEL_PORT.get_or_init(|| 8800)
}

static PANEL_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

pub fn set_panel_port(p: u16) {
    let _ = PANEL_PORT.set(p);
}
