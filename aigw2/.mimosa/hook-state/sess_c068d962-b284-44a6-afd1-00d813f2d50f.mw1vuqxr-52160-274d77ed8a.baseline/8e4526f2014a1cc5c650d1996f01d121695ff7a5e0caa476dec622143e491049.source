//! 流式响应的「首帧体检」：在下发第一个字节之前判断这一路值不值得继续。
//!
//! 从 lib.rs 拆出（2026-10-26 第二十五轮）。
//!
//! ★ 为什么需要它：上游常见「HTTP 200 + 流里报错」——连接打通了、响应头也正常，
//!   但正文第一帧就是错误帧，或直接空转到超时。原样透传的话客户端拿到 200 和解析
//!   不了的东西，换号（failover）也无从触发。
//!
//! ★ 预算是**总预算**，不是每次读取的预算：把 30s 当成「每次 s.next() 的超时」时，
//!   上游只要每 29 秒挤一个字节就能让循环无限续命，HTTP 200 迟迟不提交、客户端一直
//!   挂着，而我们永远判不出首帧好坏（对应 cherry-studio 的「延迟提交」纪律：
//!   pending 态必须有天花板）。

use aigw_core::ErrorClass;
use axum::body::Bytes;
use serde_json::Value;
use std::time::{Duration, Instant};

/// 流式首帧体检的等待预算：**总预算**，不是每次读取的预算。
///
/// 旧实现把 30s 当成「每次 s.next() 的超时」——上游只要每 29 秒挤一个字节，就能让这个
/// 循环无限续命，HTTP 200 迟迟不提交、客户端一直挂在那里，而我们又永远判不出首帧好坏
/// （对应 cherry-studio 的「延迟提交」纪律：pending 态必须有天花板）。
pub(crate) const STREAM_HEAD_WAIT: Duration = Duration::from_secs(30);
/// 首帧体检的宽限：读到过哪怕是半截帧/心跳的上游，说明它确实在干活（只是慢），
/// 给到更长的总预算；一个字节都没给的上游没有任何理由拖过 STREAM_HEAD_WAIT。
pub(crate) const STREAM_HEAD_WAIT_AFTER_BYTES: Duration = Duration::from_secs(60);
/// 首帧窗口上限：超过这么多字节还没判定，说明上游在正常流式吐内容，放行。
pub(crate) const STREAM_HEAD_MAX: usize = 16 * 1024;

/// 把预算时长写成人类读得懂的样子：整秒就写秒，亚秒（只有测试会给）写毫秒。
///
/// ★ 踩过：直接用 as_secs() 在毫秒级预算下会被截成 0，文案变成「上游 0 秒内没有发出
///   任何流式内容」——既不准确，也让测试断言无从下手。
pub(crate) fn fmt_budget(d: Duration) -> String {
    if d.as_secs() > 0 {
        format!("{} 秒", d.as_secs())
    } else {
        format!("{} 毫秒", d.as_millis())
    }
}

/// 首帧体检结论。
#[derive(Debug)]
pub(crate) enum HeadProbe {
    /// 正常流：带 choices 的帧、或非 SSE 的正常 JSON。
    Pass,
    /// 坏流：把上游真实原因、错误分类、对应 HTTP 状态带出来（分类决定要不要换号）。
    Fail(String, ErrorClass, u16),
}

/// 上游「HTTP 200 + 流里当场报错」是聚合站/中转站最常见的坏法：状态码骗人，
/// 真实原因（凭据失效、模型不可用、额度耗尽）藏在第一个 SSE 帧里。
///
/// 旧实现原样透传 → 客户端拿到 200 和一段它解析不了的东西，`failover` 也无从触发
/// （它只挂在 `forward_stream` 返回 Err 的时候）。此处把判定提前到「下发第一个字节之前」，
/// 于是这仍然是一次**可以换号重试**的失败。判定只看「首个带 choices 的帧之前」的字节，
/// 无论结论如何，已读到的字节都原样接回流首，不吞不改。
pub(crate) async fn sniff_stream_head(
    s: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
) -> (
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    Option<(String, ErrorClass, u16)>,
) {
    sniff_stream_head_within(s, STREAM_HEAD_WAIT, STREAM_HEAD_WAIT_AFTER_BYTES).await
}

/// 同 sniff_stream_head，但超时预算可注入（测试要能在毫秒级验证「拖不出首帧」的判定）。
///
/// ★ 预算语义是**总预算**：从进函数开始计时，读到过字节后放宽到 after_bytes，两者都不随
///   读次数刷新。旧实现把 timeout 写在循环里，等于「每读一次就给 30 秒」，上游一秒挤一个
///   字节就能把提交无限拖后；客户端那边是「连上了、什么也没有」，这种半开状态比直接 5xx
///   更难排查（对应 cherry-studio 的「延迟提交」纪律：pending 态必须有天花板）。
/// 首帧预算用光时的判定（两条路径共用：`left` 减到 0、以及某次等待超时）。
///
/// ★ 措辞按「已收到多少字节」定性，而不是按「哪条分支先触发」——两者对运维是同一件事：
///   上游回了 200 却一直凑不出首帧。旧实现两条分支各写一套文案，同一现象会报出
///   「只挤了 N 字节」或「卡住了」两种说法，日志里根本没法聚合统计。
pub(crate) fn head_timeout_verdict(buf_len: usize, first_wait: Duration, after_bytes: Duration) -> (String, ErrorClass, u16) {
    let msg = if buf_len == 0 {
        format!(
            "上游 {}内没有发出任何流式内容（HTTP 200，连接打开但一直空转）",
            fmt_budget(first_wait)
        )
    } else {
        format!(
            "上游 {}内只挤了 {} 字节、始终凑不出一个完整的首帧（疑似被限速或连接半死）",
            fmt_budget(after_bytes),
            buf_len
        )
    };
    (msg, ErrorClass::Transient, 504)
}

pub(crate) async fn sniff_stream_head_within(
    mut s: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
    >,
    first_wait: Duration,
    after_bytes: Duration,
) -> (
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    Option<(String, ErrorClass, u16)>,
) {
    use futures_util::StreamExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut verdict: Option<(String, ErrorClass, u16)> = None;
    let started = Instant::now();
    loop {
        if let Some(probe) = scan_stream_head(&buf, false) {
            if let HeadProbe::Fail(m, c, st) = probe {
                verdict = Some((m, c, st));
            }
            break;
        }
        if buf.len() > STREAM_HEAD_MAX {
            break;
        }
        // 总预算：读到过字节的按 after_bytes 算，一个字节没给的按 first_wait 算。
        let budget = if buf.is_empty() { first_wait } else { after_bytes };
        let left = match budget.checked_sub(started.elapsed()) {
            Some(d) if !d.is_zero() => d,
            // 预算用光：不许再无限等下去。
            _ => {
                verdict = Some(head_timeout_verdict(buf.len(), first_wait, after_bytes));
                break;
            }
        };
        match tokio::time::timeout(left, s.next()).await {
            Ok(Some(Ok(chunk))) => buf.extend_from_slice(&chunk),
            Ok(Some(Err(e))) => {
                // 还没吐出任何有效帧就读失败：对客户端来说与「上游立刻挂了」等价，
                // 此刻仍可换号，比下发一个半截流更有用。
                verdict = Some((
                    format!("上游流式读取失败（首帧前）：{e}"),
                    ErrorClass::Transient,
                    502,
                ));
                break;
            }
            Ok(None) => {
                // 流结束：最后一次判定按 eof 语义（空流/只有心跳 = 失败）。
                if let Some(HeadProbe::Fail(m, c, st)) = scan_stream_head(&buf, true) {
                    verdict = Some((m, c, st));
                }
                break;
            }
            Err(_) => {
                // 这一次等满了剩余预算还没有新字节：不再「放行」（旧实现如此），
                // 因为放行的结果就是客户端挂在 200 上什么都收不到。
                verdict = Some(head_timeout_verdict(buf.len(), first_wait, after_bytes));
                break;
            }
        }
    }
    let prefix = futures_util::stream::once(async move {
        Ok::<Bytes, std::io::Error>(Bytes::from(buf))
    });
    (Box::pin(prefix.chain(s)), verdict)
}

/// 在已读字节里找「首个有效帧」或「错误信号」。返回 `None` 表示信息还不够、继续读。
pub(crate) fn scan_stream_head(buf: &[u8], eof: bool) -> Option<HeadProbe> {
    let text = String::from_utf8_lossy(buf);
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return if eof {
            Some(HeadProbe::Fail(
                "上游流式响应在发出任何内容前就结束了（HTTP 200，但流是空的）".to_string(),
                ErrorClass::Transient,
                502,
            ))
        } else {
            None
        };
    }
    // 有些上游无视 stream:true 直接回一个 JSON：错误体按错误处理，正常体放行。
    if trimmed.starts_with('{') {
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => Some(probe_json(&v)),
            Err(_) => {
                if eof {
                    Some(HeadProbe::Pass)
                } else {
                    None
                }
            }
        };
    }
    let mut saw_frame = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let Some(payload) = line.strip_prefix("data:") else {
            if line.starts_with("event:") && line.to_ascii_lowercase().contains("error") {
                saw_frame = true;
            }
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            continue; // 半截帧：等下一块
        };
        saw_frame = true;
        let probe = probe_json(&v);
        if let HeadProbe::Fail(..) = probe {
            return Some(probe);
        }
        if v.get("choices").is_some() {
            return Some(HeadProbe::Pass);
        }
    }
    if eof && !saw_frame {
        return Some(HeadProbe::Fail(
            "上游流式响应只有心跳/空帧，没有发出任何内容".to_string(),
            ErrorClass::Transient,
            502,
        ));
    }
    None
}

/// 单个 JSON 载荷的判定：有非空 `error` 就是坏流，其余放行。
pub(crate) fn probe_json(v: &Value) -> HeadProbe {
    // ★ error 字段存在 ≠ 出错了。上游（含 OpenAI 兼容层）常把「无错误」写成
    //   {"error": ""} 或 {"error": {}}：那三个形态都得放行。
    //   否则一次完全正常的流会在首帧被掐断 —— 客户端收到 502、
    //   号池还会平白罚一次 Transient（把这个健康号冻一轮）。
    let Some(e) = v.get("error").filter(|x| !x.is_null()) else {
        return HeadProbe::Pass;
    };
    let msg = e
        .as_str()
        .map(str::to_string)
        .or_else(|| e.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| e.to_string());
    if msg.trim().is_empty() || msg == "{}" {
        return HeadProbe::Pass;
    }
    classify_stream_error(&msg)
}

/// 把流内错误文案归类：分类决定「换号有没有用」，状态码决定客户端看到什么。
pub(crate) fn classify_stream_error(msg: &str) -> HeadProbe {
    let m = msg.to_ascii_lowercase();
    let has = |k: &str| m.contains(k);
    let (class, status) = if has("api key")
        || has("apikey")
        || has("unauthorized")
        || has("invalid token")
        || has("token expired")
        || has("认证")
        || has("登录")
        || has("401")
    {
        (ErrorClass::Auth, 401)
    } else if has("quota") || has("额度") || has("余额") || has("balance") || has("402") {
        (ErrorClass::Quota, 402)
    } else if has("rate limit") || has("too many") || has("限流") || has("429") {
        (ErrorClass::Rate, 429)
    } else {
        (ErrorClass::Transient, 502)
    };
    let short: String = msg.chars().take(400).collect();
    HeadProbe::Fail(format!("上游流式响应首帧报错：{short}"), class, status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn probe_json_passes_on_normal_frames() {
        assert!(matches!(
            probe_json(&json!({ "choices": [{ "delta": { "content": "hi" } }] })),
            HeadProbe::Pass
        ));
        assert!(matches!(probe_json(&json!({ "id": "1" })), HeadProbe::Pass));
    }

    #[test]
    fn probe_json_classifies_real_errors_by_class() {
        for (msg, want) in [
            ("invalid api key", 401),
            ("额度不足", 402),
            ("rate limit exceeded", 429),
            ("upstream boom", 502),
        ] {
            let v = json!({ "error": { "message": msg } });
            match probe_json(&v) {
                HeadProbe::Fail(_, _, st) => assert_eq!(st, want),
                other => panic!("应判为失败：{msg} -> {other:?}"),
            }
        }
    }

    #[test]
    fn empty_error_field_is_not_a_failure() {
        assert!(
            matches!(probe_json(&json!({ "error": "" })), HeadProbe::Pass),
            "空 error 不是错误"
        );
        assert!(matches!(probe_json(&json!({ "error": {} })), HeadProbe::Pass));
    }

    #[test]
    fn empty_stream_is_only_a_failure_at_eof() {
        assert!(scan_stream_head(b"", false).is_none());
        assert!(matches!(scan_stream_head(b"", true), Some(HeadProbe::Fail(..))));
        let beat = b"data: {\"keepalive\":true}\n\n";
        assert!(scan_stream_head(beat, false).is_none());
    }

    #[test]
    fn fmt_budget_does_not_round_subsecond_to_zero() {
        assert!(fmt_budget(Duration::from_millis(250)).contains("250"));
        assert!(fmt_budget(Duration::from_secs(30)).contains("30 秒"));
    }
}
