//! 各平台 adapter 共用的零碎工具。
//!
//! 2026-10-26 第二十一轮：这些函数此前在 13 个 adapter 里各抄一份（`er` 13 份、
//! `secs` 7 份、`uuid_like` 4 份……）。抄写本身没什么成本，成本在**改的时候**：
//! 想给 RelayError 加一个字段，得去 13 个文件里改 13 遍，漏一处就是一个静默的
//! 行为差异。集中到这里，改一处即全量。
//!
//! ★ 只收**语义完全一致**的。名字相同但行为不同的（例如有的 `trunc` 加省略号、
//!   有的不加、有的先 trim）**不合并** —— 合并会把「截断到 200 字」和
//!   「截断到 200 字并提示被截断了」混成一件事，日志与错误信息就不可信了。

use crate::RelayError;
use aigw_core::ErrorClass;
use std::time::{SystemTime, UNIX_EPOCH};

/// 造一个中继错误。`body` 接受 String / &str 两种（各平台手上有什么就传什么）。
pub fn er(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError {
        status,
        body: body.into(),
        class,
    }
}

/// 当前 unix 秒（时钟异常时给 0，不让时间问题变成 panic）。
pub fn secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 截断到 n 个字符，超长补省略号（错误信息要给上游原话，但不能一页 HTML 全塞进来）。
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push_str("…");
        out
    }
}

/// 当前纳秒的 32 位十六进制（各平台拿它当请求 id / 会话 id）。
/// 不是密码学随机数，只用于「同一进程内不撞车」。
pub fn nanos_hex() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:032x}")
}

/// 随机的 UUID 形状字符串（v4 的两个版本位按 RFC 4122 置好）。
/// 上游常拿它当客户端指纹，形状不对会被当成异常流量。
pub fn uuid_like() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut b = [0u8; 16];
    rng.fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_only_marks_when_actually_cut() {
        assert_eq!(truncate("短", 10), "短");
        // 恰好等于上限：不截断，也不该画蛇添足加省略号
        assert_eq!(truncate("abc", 3), "abc");
        let out = truncate("abcdef", 3);
        assert_eq!(out, "abc…");
    }

    #[test]
    fn uuid_like_has_version_and_variant_bits() {
        let u = uuid_like();
        assert_eq!(u.len(), 36, "{u}");
        assert_eq!(u.as_bytes()[14], b'4', "版本位必须是 4：{u}");
        let variant = u.as_bytes()[19];
        assert!(matches!(variant, b'8' | b'9' | b'a' | b'b'), "变体位不对：{u}");
        assert_ne!(uuid_like(), uuid_like(), "两次不应相同");
    }

    #[test]
    fn er_carries_class_through() {
        let e = er(429, "slow down", ErrorClass::Rate);
        assert_eq!(e.status, 429);
        assert_eq!(e.body, "slow down");
        assert_eq!(e.class, ErrorClass::Rate);
    }
}
