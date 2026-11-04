//! 每个接入源「最近一次失败」的台账（面板接入源卡片用）。
//!
//! 从 lib.rs 拆出（2026-10-26 第二十一轮）。
//!
//! ★ 为什么需要它：usage 台账只有 ok/failed 计数。一个公益站挂了两周，面板只会
//!   显示「近24h 失败 40%」，用户完全看不到是 **401 凭据过期**、**402 额度耗尽**
//!   还是 **域名解析不了** —— 这三种的处理办法完全不同（粘贴 Key / 换站 / 查 DNS）。
//!   出错那一刻的信息只在响应体里，不记下来就永久消失。
//!
//! ★ 只保留最近一次（按 spec id 一条）：这是「现在为什么不能用」的看板，
//!   不是历史日志；历史归 usage_events。

use aigw_core::ErrorClass;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// 每个接入源「最近一次失败」的原始原因（面板接入源卡片用）。
///
/// usage 台账只有 ok/failed 计数：一个公益站挂了两周，面板只会显示「近24h 失败 40%」，
/// 用户完全看不到是 **401 凭据过期**、**402 额度耗尽** 还是 **域名解析不了** —— 这三种
/// 的处理方式完全不同（粘贴 Key / 换站 / 查 DNS）。出错那一刻的信息只在响应体里，
/// 不记下来就永久消失，所以在这里留一份（按 spec id 一条，只保留最近一次）。
pub struct LastErrors {
    inner: Mutex<HashMap<String, ErrNote>>,
}

/// 一条「最近一次失败」。
#[derive(Debug, Clone)]
pub struct ErrNote {
    /// unix 秒。
    pub at: u64,
    /// 上游 HTTP 状态；0 = 连接层失败（DNS/拒绝连接/超时），本地错误记调用方状态。
    pub status: u16,
    /// 上游原话（已压缩/截断）。
    pub text: String,
}

/// 记下来的原因最长多少字符：上游有时回一整页 HTML，全存下来会让面板卡片爆掉。
pub(crate) const LAST_ERR_MAX_CHARS: usize = 300;

/// 主动「验活」失败时的统一说法（既做响应文案，也做平台卡片上的失败原因）。
///
/// 验活只回 bool（各平台 wrapper 的端点不同，具体原因拿不到），所以这里只能说
/// 「凭据可能已过期」这一句；对 cookie 平台补一句该怎么做，避免用户去翻日志。
pub const VERIFY_FAIL_MSG: &str = "验活未通过：凭据可能已过期（Cookie 平台请重新登录后复制完整字段）";

impl Default for LastErrors {
    fn default() -> Self {
        Self::new()
    }
}

impl LastErrors {
    pub fn new() -> LastErrors {
        LastErrors {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 记一次失败（空 id 忽略：没有平台就无处归属）。
    pub fn note(&self, id: &str, status: u16, text: &str) {
        let id = id.trim();
        if id.is_empty() {
            return;
        }
        let t = compact_err_text(text);
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(
            id.to_string(),
            ErrNote {
                at: aigw_store::now_unix() as u64,
                status,
                text: t,
            },
        );
    }

    /// 这个平台成功了一次：旧的失败原因不再代表现状，必须清掉。
    ///
    /// 不注意这点就会出现「刚跑通的车道卡片上还挂着上周的 402」——
    /// 比没有信息更糟，因为它会让人去修一个已经不存在的问题。
    pub fn clear(&self, id: &str) {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(id);
    }

    /// 面板展示用：at / s_ago / status / text；没有记录返回 Null。
    pub fn json(&self, id: &str) -> Value {
        let now = aigw_store::now_unix() as u64;
        let m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(id) {
            Some(n) => json!({
                "at": n.at,
                "s_ago": now.saturating_sub(n.at),
                "status": n.status,
                "text": n.text,
            }),
            None => Value::Null,
        }
    }
}

/// 把上游错误文本压成一行可读的原因：能解析成 JSON 就取 message 字段，
/// 否则取原文；换行压成空格；超长截断。
pub(crate) fn compact_err_text(raw: &str) -> String {
    let raw = raw.trim();
    let mut t = raw.to_string();
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        for path in [
            ["error", "message"],
            ["error", "msg"],
            ["message", ""],
            ["msg", ""],
            ["detail", ""],
        ] {
            let mut cur = &v;
            let mut hit = true;
            for k in path.iter().filter(|k| !k.is_empty()) {
                match cur.get(*k) {
                    Some(n) => cur = n,
                    None => {
                        hit = false;
                        break;
                    }
                }
            }
            if hit {
                if let Some(x) = cur.as_str() {
                    if !x.trim().is_empty() {
                        t = x.trim().to_string();
                        break;
                    }
                }
            }
        }
    }
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.is_empty() {
        return "（上游没有给出原因）".to_string();
    }
    let mut out: String = one.chars().take(LAST_ERR_MAX_CHARS).collect();
    if one.chars().count() > LAST_ERR_MAX_CHARS {
        out.push('…');
    }
    out
}

/// 把**流式**结果写进「最近失败原因」台账。
///
/// 流式的结局不是主循环拿到的：网关把 body 交出去之后就返回了 Ok，成败要等后台
/// 统计任务从 stat_rx 里读。少了这一步，流式失败（真机上最常见的一类）在卡片上
/// 只剩「近24h 失败 N 次」，说不出上游原话。
pub(crate) fn note_stream_outcome(le: &LastErrors, spec_id: &str, error: Option<ErrorClass>, msg: &str) {
    match error {
        None => le.clear(spec_id),
        Some(cl) => {
            // 流里没有 HTTP 状态码：status 记 0，面板据此不显示「HTTP xxx」。
            let text = if msg.trim().is_empty() {
                format!("流式响应中途失败（上游断开或回了错误帧；分类 {cl:?}）")
            } else {
                msg.to_string()
            };
            le.note(spec_id, 0, &text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_err_text_prefers_nested_message() {
        // 上游三种常见摆放方式，都要拿到人话而不是整坨 JSON
        assert_eq!(
            compact_err_text(r#"{"error":{"message":"invalid api key"}}"#),
            "invalid api key"
        );
        assert_eq!(compact_err_text(r#"{"message":"boom"}"#), "boom");
        assert_eq!(compact_err_text(r#"{"detail":"nope"}"#), "nope");
    }

    #[test]
    fn compact_err_text_flattens_and_truncates() {
        // 换行压成空格：卡片上一行放得下
        assert_eq!(compact_err_text("line1\nline2"), "line1 line2");
        // 超长截断并留省略号（上游有时回一整页 HTML）
        let long = "x".repeat(LAST_ERR_MAX_CHARS + 50);
        let out = compact_err_text(&long);
        assert!(out.ends_with("…"), "超长应带省略号");
        assert!(out.chars().count() <= LAST_ERR_MAX_CHARS + 1, "实际 {}", out.len());
        // 空白串不能变空串（面板上会是一块空白，看的人以为没数据）
        assert_eq!(compact_err_text("   "), "（上游没有给出原因）");
    }

    #[test]
    fn errlog_keeps_only_the_latest_per_platform() {
        let le = LastErrors::new();
        le.note("p", 401, "first");
        le.note("p", 429, "second");
        let v = le.json("p");
        assert_eq!(v.get("text").and_then(|x| x.as_str()), Some("second"), "只留最近一次");
        assert_eq!(v.get("status").and_then(|x| x.as_u64()), Some(429));
        // 空 id 记不进去：没有平台就无处归属，记了面板上也显示不出来
        le.note("  ", 500, "orphan");
        assert!(le.json("").is_null(), "空 id 应被忽略");
    }

    #[test]
    fn note_stream_outcome_clears_on_success() {
        let le = LastErrors::new();
        le.note("p", 401, "old");
        // 流式成功：清掉旧账，否则卡片会一直挂着上一次的失败
        note_stream_outcome(&le, "p", None, "");
        assert!(le.json("p").is_null(), "成功后应清空");
        // 失败但 msg 为空也要留下可读原因（不能记空串：面板上是一块空白）
        note_stream_outcome(&le, "p", Some(ErrorClass::Transient), "  ");
        let v = le.json("p");
        let e = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
        assert!(!e.trim().is_empty(), "空 msg 也要给出可读原因");
    }
}
