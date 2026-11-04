//! 会话键：把客户端可控的会话标识折成**定长**哈希再当粘性路由的键。
//!
//! 从 lib.rs 拆出（2026-10-26 第二十一轮）。全是纯函数，可单测。
//!
//! ★ 为什么必须定长：`conversation_id` 是请求体里的字符串，长度由调用方决定
//!   （实测有人在 metadata.conversation_id 里塞整段对话历史）。原样存进粘性表的
//!   HashMap 键，一个 1MB 的 id 就是 1MB 驻留内存，几万个不同 id 直接把进程撑爆。
//!   折成 17 字节后，条目大小与客户端发什么完全无关（条目数量另有上限）。

use serde_json::Value;

/// FNV-1a 64 位（std 实现，零新增依赖；仅作稳定压缩，不需加密强度）。
pub(crate) fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 客户端可控的会话标识一律折成定长哈希再当键。
///
/// 为什么不直接用原文：`conversation_id` 是请求体里的字符串，长度由调用方决定
/// （实测有人在 `metadata.conversation_id` 里塞整段对话历史）。把它原样存进粘性表
/// 的 HashMap 键，一个 1MB 的 id 就变成 1MB 的驻留内存，几万个不同 id 直接把进程撑爆。
pub(crate) fn key_id(s: &str) -> String {
    format!("k{:016x}", fnv1a(s.as_bytes()))
}

/// 是否是本服务自己生成的会话键形状（`k` + 16 位小写十六进制）。
pub(crate) fn is_key_id(s: &str) -> bool {
    s.len() == 17
        && s.starts_with('k')
        && s.as_bytes()[1..].iter().all(|b| b.is_ascii_hexdigit())
}

/// 会话粘性键（对齐 workbuddy2api 键优先级 + loomy2api「最后一条 user」修正）：
/// ① `metadata.conversation_id` / `conversationId` / `conversation_id` /
///    `prompt_cache_key`（02 号五键优先级）；
/// ② 都没有 → **最后一条** `role=user` 消息内容 FNV-1a。
///    为什么取最后一条：通用客户端重发全历史时首条 user 恒定，以首条为键会让
///    整个会话永久钉在同一账号（单号霸占、其余饿死）；最后一条随对话推进变化。
///
/// 返回值恒为 17 字节以内（`k` + 16 位十六进制，或 `u` + 16 位，或 `anon`）：幂等且定长，
/// 粘性表的条目大小与客户端发什么都无关（条目数量另有上限，见 `STICKY_MAX_ENTRIES`）。
pub(crate) fn session_key(v: &Value) -> String {
    // 协议转换（/v1/messages、/v1/responses）会重建请求体，原始会话标识必须由
    // 转换函数搬到这个私有字段里，否则粘性路由退化成「按最后一条 user 内容哈希」，
    // 多轮对话每轮都换账号，上游的 prompt cache 全丢。
    if let Some(s) = v.get("_aigw_session").and_then(|x| x.as_str()) {
        if !s.is_empty() {
            // 这个字段是 carry_session 写进去的，已经是折过的定长键；**再折一次**会让
            // `/v1/messages`（经 carry_session）与 `/v1/chat/completions`（直接给
            // conversation_id）对同一个会话算出两个不同的键 —— 同一会话跨协议混用时
            // 粘性就断了，上游 prompt cache 全丢。所以自家形状原样放行。
            // 但客户端也能伪造这个顶层字段，故只放行形状，其余照样折（否则又是无界键）。
            if is_key_id(s) {
                return s.to_string();
            }
            return key_id(s);
        }
    }
    if let Some(md) = v.get("metadata").and_then(|m| m.as_object()) {
        for k in ["conversation_id", "conversationId", "prompt_cache_key"] {
            if let Some(s) = md.get(k).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return key_id(s);
                }
            }
        }
    }
    for k in ["conversation_id", "conversationId"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return key_id(s);
            }
        }
    }
    if let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) {
        for m in msgs.iter().rev() {
            if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                let text = m
                    .get("content")
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                return format!("u{:016x}", fnv1a(text.as_bytes()));
            }
        }
    }
    "anon".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_key_is_bounded_no_matter_what_client_sends() {
        // 有人在 metadata.conversation_id 里塞整段对话历史：
        // 原样当键会让粘性表条目随请求体膨胀，几万个 id 就能把进程撑爆。
        let huge = "x".repeat(100_000);
        let v = json!({"metadata": {"conversation_id": huge}});
        let k = session_key(&v);
        assert!(k.len() <= 17, "会话键必须定长，实际 {} 字节", k.len());
        assert!(is_key_id(&k), "折出来的键应是本服务形状：{k}");
    }

    #[test]
    fn session_key_is_stable_and_sensitive() {
        // 幂等：同一请求体两次得到同一个键（否则粘性每次都换号，prompt cache 全丢）
        let v = json!({"metadata": {"conversation_id": "abc"}});
        assert_eq!(session_key(&v), session_key(&v));
        // 敏感：不同会话必须落到不同键
        let w = json!({"metadata": {"conversation_id": "abd"}});
        assert_ne!(session_key(&v), session_key(&w));
    }

    #[test]
    fn private_session_field_wins_over_body_heuristics() {
        // 协议转换把原始会话标识搬进 _aigw_session：它优先于「最后一条 user 内容」
        // 兜底，否则多轮对话每轮都换账号。
        let v = json!({"_aigw_session": "k0123456789abcdef"});
        assert_eq!(session_key(&v), "k0123456789abcdef");
        // 没有标识、也没有 user 消息时回落到 anon（不能 panic，也不能是空键）
        assert_eq!(session_key(&json!({})), "anon");
    }

    #[test]
    fn fnv1a_is_a_stable_compression() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), fnv1a(b"a"));
        assert_ne!(fnv1a(b"a"), fnv1a(b"b"));
        assert_eq!(key_id("x"), format!("k{:016x}", fnv1a(b"x")));
    }
}
