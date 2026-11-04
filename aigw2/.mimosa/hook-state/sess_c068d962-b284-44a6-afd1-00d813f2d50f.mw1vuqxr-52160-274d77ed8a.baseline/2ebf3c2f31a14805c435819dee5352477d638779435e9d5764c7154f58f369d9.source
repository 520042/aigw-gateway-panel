//! AutoClaw（智谱 autoglm）—— 标准 OpenAI 协议，但有**两道上游闸门**。
//!
//! ── 上游长什么样（对齐 Agent2API 的 providers/autoclaw/adapter.rs + prompt.rs）──
//! ```text
//! POST {base}/chat/completions
//!   头：X-Authorization: Bearer <token>   ← 注意**不是** Authorization！
//!       X-Request-Model: <routeId>
//!       X-Request-Id: <uuid>
//! ```
//!
//! ── 三处必须照做，否则稳定失败 ───────────────────────────────
//! 1. **认证头是 `X-Authorization`**：发成 `Authorization` 会稳定 401，
//!    而错误文案不会告诉你「头名写错了」——排查成本极高。
//! 2. **system 提示必须过白名单闸**（上游 2026-09-22 起）：实测结论是
//!    必须以身份句开头且带 `## Tooling` 段，且**不能**出现外来 harness 的身份句
//!    （`You are ZCode…` / `You are Claude Code…` 这类**字面**判定）。
//!    没有 system、或只有中性句 → **406 空响应体**（连错误体都不给）。
//! 3. **`max_tokens` 的小正整数一律 406**（30~8192 实测；不传或 ≥10000 则 200）
//!    → 出站前归一成 10000。
//!
//! ── 为什么身份句要「照抄」而不是自己编 ──────────────────────
//! 前缀取自官方客户端自带的运行时（`buildSystemPrompt` 的开头两行），
//! 照抄的好处是出站形态与官方客户端一致，而不是编一句「看起来像」的话。
//! 改写外来身份句时**把产品名整个去掉**（插一个词不够——闸门连 sanitize 改写过
//! 的 "You are Claude Code, …" 都拦），且匹配**忽略大小写**（闸门自己不敏感）。

use serde_json::{json, Value};

use aigw_registry::ProviderSpec;

use crate::common::{er, truncate};
use crate::{ErrorClass, RelayError};

/// 官方客户端的身份前缀（前两行逐字照抄自 `system-prompt-config-*.js`）。
const IDENTITY_PREFIX: &str = "You are a personal assistant running inside OpenClaw.\n\n\
## Tooling\n\
Available tools are policy-filtered. Names are case-sensitive; call exactly as listed.\n";

/// 判定「已带前缀」用的首行。
const IDENTITY_LINE: &str = "You are a personal assistant running inside OpenClaw.";

/// 外来身份句 → 中性说法（**长的在前**）。
///
/// ★ 顺序是纪律不是巧合：短的在前面会把长匹配串切碎，于是「改写后仍含产品名」
///   这种半吊子结果会被漏掉（先命中 `You are ZCode` 就再也匹配不到
///   `You are ZCode, an interactive coding agent`）。
/// ★ 每条替换串都**不含产品名**：闸门拦的就是产品名本身。
const FOREIGN_IDENTITIES: &[(&str, &str)] = &[
    (
        "You are ZCode, an interactive coding agent",
        "You are an interactive coding agent",
    ),
    (
        "You are a coding agent running in the Codex CLI tool",
        "You are a coding agent running in a terminal CLI tool",
    ),
    (
        "You are a coding agent running in the Codex CLI",
        "You are a coding agent running in a terminal CLI",
    ),
    (
        "You are running as a coding agent in the Codex CLI",
        "You are running as a coding agent in a terminal CLI",
    ),
    ("You are Claude Code", "You are a coding assistant"),
    ("You are ZCode", "You are an interactive coding agent"),
    (
        "You are an AI agent powered by DeepSeek Harness",
        "You are an AI agent powered by a local coding harness",
    ),
    ("You are Codex", "You are a coding agent"),
];

/// 忽略 ASCII 大小写的整体替换。
fn replace_all_ci(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_string();
    }
    let lower = text.to_ascii_lowercase();
    let from_lower = from.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&from_lower) {
        let start = i + pos;
        out.push_str(&text[i..start]);
        out.push_str(to);
        i = start + from.len();
    }
    out.push_str(&text[i..]);
    out
}

/// 出站 system 提示规范化：前置身份前缀 + 改写外来身份句。
///
/// 幂等：正文已以身份句开头时**只改写、不再前置**（官方客户端的请求本身已带
/// OpenClaw 提示词，重复前缀会把提示词写两遍 —— 降级/重试路径上必然发生）。
pub fn normalize_system(text: &str) -> String {
    let mut s = text.to_string();
    for (from, to) in FOREIGN_IDENTITIES {
        s = replace_all_ci(&s, from, to);
    }
    let head = s.trim_start().to_ascii_lowercase();
    if head.starts_with(&IDENTITY_LINE.to_ascii_lowercase()) {
        s
    } else {
        format!("{IDENTITY_PREFIX}\n{s}")
    }
}

/// 改写请求体：规范化 system + 归一 max_tokens。
pub fn normalize_request(v: &mut Value) {
    let Some(o) = v.as_object_mut() else { return };
    // 1) system：找到第一条 system/developer 就地规范化；没有就插一条
    let mut found = false;
    if let Some(arr) = o.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for m in arr.iter_mut() {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role == "system" || role == "developer" {
                let cur = m
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                let new = normalize_system(&cur);
                if let Some(obj) = m.as_object_mut() {
                    obj.insert("content".to_string(), Value::String(new));
                }
                found = true;
                break;
            }
        }
        // ★ 没有 system 时**必须**补一条：实测「完全没有 system 消息」= 406 空响应体。
        if !found {
            arr.insert(
                0,
                json!({"role": "system", "content": normalize_system("")}),
            );
        }
    }
    // 2) max_tokens 的小正整数一律 406（实测 30~8192），归一成 10000
    if let Some(mt) = o.get("max_tokens").and_then(|x| x.as_i64()) {
        if (1..10_000).contains(&mt) {
            o.insert("max_tokens".to_string(), json!(10_000));
        }
    }
}

fn rand_hex(n: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 该平台的路由 ID（`X-Request-Model` 要它）。
///
/// 上游有两层模型标识：`X-Request-Model` 填**路由 ID**、`body.model` 填模型 ID。
/// 这里没有远程目录映射，就把请求里的模型名同时当两者 —— 与「模型@平台」后缀
/// 路由一致；若上游要别的形态，用户可在 spec 里用固定头覆盖。
fn route_id(spec: &ProviderSpec, model: &str) -> String {
    spec.headers
        .get("X-Request-Model")
        .cloned()
        .unwrap_or_else(|| model.to_string())
}

/// 发送前的共同准备：改写 body、加上四件头。
fn prepare(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<reqwest::RequestBuilder, RelayError> {
    let mut v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    normalize_request(&mut v);
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let url = format!("{}{}", spec.base_url.trim_end_matches('/'), "/chat/completions");
    let mut rb = http
        .post(&url)
        .header("Content-Type", "application/json")
        // ★ X-Authorization（不是 Authorization）—— 发错稳定 401
        .header("X-Authorization", format!("Bearer {secret}"))
        .header("X-Request-Model", route_id(spec, &model))
        .header("X-Request-Id", rand_hex(16))
        .body(v.to_string());
    for (k, val) in &spec.headers {
        if k.eq_ignore_ascii_case("X-Authorization") {
            continue; // 别把凭据覆盖掉
        }
        rb = rb.header(k.as_str(), val.as_str());
    }
    Ok(rb)
}

/// 非流式：上游是标准 OpenAI，直接转发。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let rb = prepare(http, spec, secret, body)?;
    let resp = rb
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(er(status, classify_hint(status, &text), class_of(status)));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|e| er(502, format!("上游响应不是 JSON：{e}"), ErrorClass::Transient))
}

/// 流式：直接把上游 SSE 透传给下游。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<(
    futures_util::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
    tokio::sync::mpsc::Receiver<crate::StreamStat>,
), RelayError> {
    use futures_util::StreamExt;
    let rb = prepare(http, spec, secret, body)?;
    let resp = rb
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        return Err(er(status, classify_hint(status, &t), class_of(status)));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(8);
    let stream = resp.bytes_stream().map(move |item| {
        item.map_err(|e| std::io::Error::other(format!("上游流错误：{e}")))
    });
    // 记账：先给个空统计（上游 SSE 里自带 usage，通用层会补）
    let _ = tx.try_send(crate::StreamStat::default());
    Ok((Box::pin(stream), rx))
}

fn class_of(status: u16) -> ErrorClass {
    match status {
        401 | 403 => ErrorClass::Auth,
        402 | 429 => ErrorClass::Quota,
        400 | 406 | 422 => ErrorClass::Rejected,
        _ => ErrorClass::Transient,
    }
}

/// 406 要补一句可执行提示：上游连错误体都不给，用户既不知道是网关的问题还是
/// 自己的问题，也不知道下一步改什么。
fn classify_hint(status: u16, body: &str) -> String {
    if status == 406 {
        return format!(
            "上游返回 406（预校验拒收，空响应体）：常见三个方向 —— ① max_tokens 取值（网关已归一，\
             正常客户端无需处理）；② system 首句指纹（网关已前置身份前缀并改写外来身份句，\
             若仍被拒请把 system 原文反馈回来）；③ 请求形态风控。原文：{}",
            truncate(body, 200)
        );
    }
    truncate(body, 400)
}

/// 验活：发一个极小的对话（已带 system 与合规 max_tokens，避免 406 干扰判断）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let probe = json!({
        "model": "glm-5.2",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 10_000,
        "stream": false,
    });
    let Ok(rb) = prepare(http, spec, secret, probe.to_string().as_bytes()) else {
        return false;
    };
    rb.send().await.map(|r| r.status().is_success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ 没有 system 时必须补一条并前置身份句 —— 实测「完全没有 system」= 406。
    #[test]
    fn injects_identity_when_no_system() {
        let mut v = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        normalize_request(&mut v);
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], serde_json::json!("system"));
        let s = msgs[0]["content"].as_str().unwrap();
        assert!(s.starts_with("You are a personal assistant running inside OpenClaw."), "s={s}");
        assert!(s.contains("## Tooling"), "必须带 Tooling 段");
    }

    /// 已有 system 时**前置**而不是替换（客户端提示词逐字保留在后面）。
    #[test]
    fn prefixes_existing_system() {
        let mut v = serde_json::json!({"messages": [
            {"role": "system", "content": "客户端自己的提示词"},
            {"role": "user", "content": "hi"},
        ]});
        normalize_request(&mut v);
        let s = v["messages"][0]["content"].as_str().unwrap();
        assert!(s.starts_with("You are a personal assistant"), "s={s}");
        assert!(s.contains("客户端自己的提示词"), "原提示词要保留");
    }

    /// 幂等：已带身份句时不再重复前置（重复会把提示词写两遍）。
    #[test]
    fn normalize_is_idempotent() {
        let once = normalize_system("随便");
        let twice = normalize_system(&once);
        assert_eq!(once, twice, "第二次不能再加前缀");
        assert_eq!(once.matches("running inside OpenClaw").count(), 1);
    }

    /// 外来身份句要被改写，且改写后不含产品名。
    #[test]
    fn rewrites_foreign_identities() {
        let s = normalize_system("You are Claude Code, Anthropic's official CLI tool.");
        assert!(!s.contains("Claude Code"), "产品名必须被去掉：{s}");
        assert!(s.contains("You are a coding assistant"), "s={s}");
        // 大小写不敏感
        let s2 = normalize_system("you are zcode, an interactive coding agent");
        assert!(!s2.to_ascii_lowercase().contains("zcode"), "s2={s2}");
    }

    /// 长句优先：整句先命中，不能被短串切碎。
    #[test]
    fn longer_identity_wins() {
        let s = normalize_system("You are ZCode, an interactive coding agent");
        assert!(!s.contains("ZCode"), "s={s}");
        assert!(s.contains("You are an interactive coding agent"), "s={s}");
    }

    /// ★ max_tokens 的小正整数一律归一成 10000（30~8192 实测 406）。
    #[test]
    fn normalizes_small_max_tokens() {
        for mt in [1, 30, 1024, 8192, 9999] {
            let mut v = serde_json::json!({"messages": [], "max_tokens": mt});
            normalize_request(&mut v);
            assert_eq!(v["max_tokens"], serde_json::json!(10000), "mt={mt}");
        }
        // >= 10000 不动
        let mut v = serde_json::json!({"messages": [], "max_tokens": 12000});
        normalize_request(&mut v);
        assert_eq!(v["max_tokens"], serde_json::json!(12000));
        // 不传也不补
        let mut v = serde_json::json!({"messages": []});
        normalize_request(&mut v);
        assert!(v.get("max_tokens").is_none());
    }

    /// 忽略大小写的替换。
    #[test]
    fn replace_all_ci_works() {
        assert_eq!(replace_all_ci("AbC abc ABC", "abc", "X"), "X X X");
        assert_eq!(replace_all_ci("none", "", "X"), "none");
    }
}
