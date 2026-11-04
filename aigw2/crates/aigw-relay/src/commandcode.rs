//! Command Code（`api.commandcode.ai`）—— AI SDK v5 风格的 NDJSON 上游。
//!
//! ── 与 OpenAI 兼容透传的三个根本差别 ─────────────────────────
//! 1. **路径不是 `/chat/completions`**，是 `POST /alpha/generate`；
//! 2. **请求体不是 chat 信封**，是 `{config, params:{model, messages, max_tokens,
//!    stream, system, ...}}`（system 提到顶层、不在 messages 里）；
//! 3. **响应是 `application/x-ndjson`**：一行一个 JSON 对象、**没有 `data:` 前缀**、
//!    而且 **HTTP 恒 200** —— 认证失败 / 限流 / 余额不足全部以流内 `{"type":"error"}`
//!    事件表达。所以「看状态码判断成败」在这里完全失效，必须解析事件流。
//!
//! ── 事件词表（对齐 Agent2API 的 protocol/commandcode_outbound.rs）──
//! - `text-delta`      → `delta.content`（取 `text`，兼容 `delta` 字段）
//! - `reasoning-delta` → `delta.reasoning_content`
//! - `tool-call`       → `tool_calls`（id/name/arguments 一次给全，与 Anthropic 的
//!                       「宣告帧 + 参数帧」不同）
//! - `finish-step`     → 只记 finishReason/usage，**不发帧**
//! - `finish`          → 终态：finish 帧 + usage 帧 + `data: [DONE]`
//! - `error`           → 错误帧
//! - 其余（start / text-start / text-end / reasoning-start / tool-input-* 等）→ 信号，忽略
//!
//! ── CLI 伪装头 ──────────────────────────────────────────────
//! 上游按「像不像官方 CLI」做风控。实测无 Key 时带不带这几项都回同样的 401
//! （见 `endpoints` 那轮的探测），所以它们不是**鉴权**条件；但真机运行时
//! 「同一账号的两种请求来自两种 UA」是可直接观测的破绽，因此照样逐字对齐：
//! `User-Agent: cli`（不是 reqwest 默认值）、`x-command-code-version`、
//! `x-cli-environment`。

use serde_json::{json, Value};

use aigw_registry::ProviderSpec;

use crate::common::{er, nanos_hex, secs, truncate};
use crate::{ErrorClass, RelayError};

/// 本实现对齐的 CLI 协议版本（自称的方言版本，不是 npm 最新版）。
const CC_PROTOCOL_VERSION: &str = "1.53.1";
/// 官方 CLI 的 UA（一个字都不能改）。
const CC_USER_AGENT: &str = "cli";
/// CLI 环境标识。
const CC_CLI_ENVIRONMENT: &str = "production";

/// CLI 伪装头（生成请求与指纹上报共用同一张表）。
fn cli_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("User-Agent", CC_USER_AGENT),
        ("x-command-code-version", CC_PROTOCOL_VERSION),
        ("x-cli-environment", CC_CLI_ENVIRONMENT),
    ]
}

/// Command Code 的 Key 形态：`user_` 开头、其后至少一个合法字符、整串只在字符集内。
///
/// ★ 这是**唯一的本地校验**（零网络）：`sk-xxx` 之类会在发请求前就被拒。
///   不校验的代价是：发出去只换回一条语焉不详的 401，用户以为是「登录态失效」
///   去反复重新登录，而真正该做的是**换一把正确的 Key**。
fn key_shape_ok(key: &str) -> bool {
    let Some(rest) = key.trim().strip_prefix("user_") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 脱敏形态（只露前缀与尾 4 位；错误文案里用它，别把 Key 打进日志）。
fn mask_key(key: &str) -> String {
    let k = key.trim();
    if k.chars().count() <= 12 {
        return "user_…".to_string();
    }
    let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("user_…{tail}")
}
/// 从 OpenAI chat 请求体造出 CC 的 `{config, params}` 信封。
pub fn build_request_body(v: &Value) -> Value {
    let mut params = serde_json::Map::new();
    if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
        params.insert("model".to_string(), json!(m));
    }
    // system 必须提到顶层，且**不能留在 messages 里**（上游按两条独立字段处理）。
    let mut system: Vec<Value> = Vec::new();
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(arr) = v.get("messages").and_then(|x| x.as_array()) {
        for m in arr {
            if m.get("role").and_then(|r| r.as_str()) == Some("system") {
                let text = content_text(m.get("content"));
                if !text.is_empty() {
                    system.push(json!({"type": "text", "text": text}));
                }
            } else {
                msgs.push(m.clone());
            }
        }
    }
    // 没有 system 时发一个空格占位：上游要求该字段存在（硬要求，不是美化）
    if system.is_empty() {
        system.push(json!({"type": "text", "text": " "}));
    }
    params.insert("system".to_string(), Value::Array(system));
    params.insert("messages".to_string(), Value::Array(msgs));
    // ★ 上游**只支持流式**（非流式由我们缓冲聚合），这一项恒为 true。
    params.insert("stream".to_string(), json!(true));
    if let Some(n) = v.get("max_tokens").and_then(|x| x.as_i64()) {
        params.insert("max_tokens".to_string(), json!(n));
    }
    for k in ["temperature", "top_p", "reasoning_effort"] {
        if let Some(x) = v.get(k) {
            if x.is_number() || x.is_string() {
                params.insert(k.to_string(), x.clone());
            }
        }
    }
    if let Some(tools) = v.get("tools") {
        if tools.is_array() {
            params.insert("tools".to_string(), tools.clone());
        }
    }
    json!({"config": {}, "params": Value::Object(params)})
}

/// 从 content（字符串或 Anthropic 风格的 blocks 数组）取出纯文本。
fn content_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// 上游事件 → 一帧 chat 增量（或 None 表示「信号事件，无内容」）。
#[derive(Default)]
pub struct NdjsonAcc {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<Value>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub error: Option<(u16, String)>,
    pub saw_finish: bool,
}

impl NdjsonAcc {
    /// 吃一行 NDJSON，返回要下发的 chat delta（None = 无用户可见内容）。
    pub fn feed(&mut self, line: &str) -> Option<Value> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match ty {
            "text-delta" => {
                let t = v
                    .get("text")
                    .or_else(|| v.get("delta"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if t.is_empty() {
                    return None;
                }
                self.text.push_str(t);
                Some(json!({"content": t}))
            }
            "reasoning-delta" => {
                let t = v
                    .get("text")
                    .or_else(|| v.get("delta"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if t.is_empty() {
                    return None;
                }
                self.reasoning.push_str(t);
                Some(json!({"reasoning_content": t}))
            }
            "tool-call" => {
                // CC 的工具调用是**整体**事件：id/name/arguments 一次给全。
                let call = json!({
                    "index": self.tool_calls.len(),
                    "id": v.get("toolCallId").or_else(|| v.get("id")).and_then(|x| x.as_str()).unwrap_or(""),
                    "type": "function",
                    "function": {
                        "name": v.get("toolName").or_else(|| v.get("name")).and_then(|x| x.as_str()).unwrap_or(""),
                        "arguments": v.get("args").or_else(|| v.get("arguments")).map(|a| {
                            if a.is_string() { a.as_str().unwrap_or("{}").to_string() }
                            else { a.to_string() }
                        }).unwrap_or_else(|| "{}".to_string()),
                    }
                });
                self.tool_calls.push(call.clone());
                Some(json!({"tool_calls": [call]}))
            }
            "finish-step" => {
                if let Some(fr) = v.get("finishReason").and_then(|x| x.as_str()) {
                    self.finish_reason = Some(map_finish(fr));
                }
                read_usage(self, &v);
                None
            }
            "finish" => {
                self.saw_finish = true;
                if let Some(fr) = v.get("finishReason").and_then(|x| x.as_str()) {
                    self.finish_reason = Some(map_finish(fr));
                }
                read_usage(self, &v);
                None
            }
            "error" => {
                let msg = v
                    .get("message")
                    .or_else(|| v.get("error").and_then(|e| e.get("message")))
                    .and_then(|x| x.as_str())
                    .unwrap_or("上游返回错误");
                let status = v
                    .get("status")
                    .or_else(|| v.get("error").and_then(|e| e.get("status")))
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0) as u16;
                self.error = Some((status, msg.to_string()));
                None
            }
            _ => None,
        }
    }
}

fn read_usage(acc: &mut NdjsonAcc, v: &Value) {
    let u = v.get("usage").or_else(|| v.get("totalUsage"));
    let Some(u) = u else { return };
    for k in ["inputTokens", "promptTokens", "input_tokens", "prompt_tokens"] {
        if let Some(n) = u.get(k).and_then(|x| x.as_i64()) {
            if n > 0 {
                acc.prompt_tokens = n;
            }
        }
    }
    for k in ["outputTokens", "completionTokens", "output_tokens", "completion_tokens"] {
        if let Some(n) = u.get(k).and_then(|x| x.as_i64()) {
            if n > 0 {
                acc.completion_tokens = n;
            }
        }
    }
}

fn map_finish(fr: &str) -> String {
    match fr {
        "length" | "max_tokens" => "length".to_string(),
        "tool-calls" | "tool_calls" => "tool_calls".to_string(),
        "content-filter" | "content_filter" => "content_filter".to_string(),
        _ => "stop".to_string(),
    }
}

/// 上游 402（余额不足）按 429 处理（对齐参考的「付费失败按限流」，换账号比报错有用）。
pub fn class_of(status: u16) -> ErrorClass {
    match status {
        401 | 403 => ErrorClass::Auth,
        402 | 429 => ErrorClass::Quota,
        400 | 422 => ErrorClass::Rejected,
        _ => ErrorClass::Transient,
    }
}

/// 非流式：把整条 NDJSON 缓冲起来聚合成一个 chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    if !key_shape_ok(secret) {
        return Err(er(
            401,
            format!(
                "Command Code 的 Key 形态不对（应为 user_ 开头，当前 {}）：请在 commandcode.ai/studio 重新获取后粘贴",
                mask_key(secret)
            ),
            ErrorClass::Auth,
        ));
    }
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let wire = build_request_body(&v);
    let url = format!("{}{}", spec.base_url, spec.chat_path());
    let mut rb = http
        .post(&url)
        .header("Authorization", format!("Bearer {secret}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/x-ndjson")
        .body(wire.to_string());
    for (k, val) in cli_headers() {
        rb = rb.header(k, val);
    }
    let resp = rb.send().await.map_err(|e| {
        er(0, format!("network: {e}"), ErrorClass::Transient)
    })?;
    let status = resp.status().as_u16();
    let raw = resp.text().await.map_err(|e| {
        er(status, format!("读取响应失败：{e}"), ErrorClass::Transient)
    })?;
    // ★ 上游 HTTP 恒 200：真错误在流内。但连接层的 4xx/5xx 仍要先看。
    if !(200..300).contains(&status) {
        return Err(er(status, truncate(&raw, 400), class_of(status)));
    }
    let mut acc = NdjsonAcc::default();
    for line in raw.lines() {
        acc.feed(line);
        if acc.error.is_some() {
            break;
        }
    }
    if let Some((st, msg)) = acc.error.clone() {
        let st = if st == 0 { 502 } else { st };
        return Err(er(st, msg, class_of(st)));
    }
    if acc.text.is_empty() && acc.tool_calls.is_empty() && !acc.saw_finish {
        return Err(er(
            502,
            format!("上游没有给出完成信号（响应被截断）：{}", truncate(&raw, 200)),
            ErrorClass::Transient,
        ));
    }
    let tin = if acc.prompt_tokens > 0 {
        acc.prompt_tokens
    } else {
        (body.len() / 4).max(1) as i64
    };
    let tout = if acc.completion_tokens > 0 {
        acc.completion_tokens
    } else {
        (acc.text.chars().count() as i64 / 2).max(1)
    };
    let mut msg = json!({
        "role": "assistant",
        "content": if acc.text.is_empty() { Value::Null } else { json!(acc.text) },
    });
    if !acc.reasoning.is_empty() {
        msg["reasoning_content"] = json!(acc.reasoning);
    }
    if !acc.tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(acc.tool_calls.clone());
    }
    Ok(json!({
        "id": format!("chatcmpl-cc-{}", nanos_hex()),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": msg,
            "finish_reason": acc.finish_reason.unwrap_or_else(|| {
                if acc.tool_calls.is_empty() { "stop".to_string() } else { "tool_calls".to_string() }
            }),
        }],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout},
    }))
}

/// 流式：把 NDJSON 逐行翻成 chat SSE 帧。
///
/// ★ 与 `chat_nonstream` 的关键差别：错误可能**已经发出去了半截内容**
///   （上游 HTTP 恒 200，错误帧夹在中间）。所以错误不是 `Err` 返回，而是**发一帧
///   错误事件再收尾** —— 客户端至少能看到「前面那些是真的，后面断了」。
///
/// ★ 为什么用 channel + spawn 而不是 `async_stream`：本 crate 没有那个依赖，
///   而现有 adapter（devin / kuku）都用 mpsc + spawn 的手法，照着来保持一致。
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

    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    if !key_shape_ok(secret) {
        return Err(er(
            401,
            format!(
                "Command Code 的 Key 形态不对（应为 user_ 开头，当前 {}）：请在 commandcode.ai/studio 重新获取后粘贴",
                mask_key(secret)
            ),
            ErrorClass::Auth,
        ));
    }
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let wire = build_request_body(&v);
    let url = format!("{}{}", spec.base_url, spec.chat_path());
    let mut rb = http
        .post(&url)
        .header("Authorization", format!("Bearer {secret}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/x-ndjson")
        .body(wire.to_string());
    for (k, val) in cli_headers() {
        rb = rb.header(k, val);
    }
    let resp = rb.send().await.map_err(|e| {
        er(0, format!("network: {e}"), ErrorClass::Transient)
    })?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let raw = resp.text().await.unwrap_or_default();
        return Err(er(status, truncate(&raw, 400), class_of(status)));
    }

    let id = format!("chatcmpl-cc-{}", nanos_hex());
    let created = secs();
    let body_len = body.len();
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(8);
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let mut acc = NdjsonAcc::default();
        let mut buf = String::new();
        let mut upstream = resp.bytes_stream();
        let mut sent_error = false;

        while let Some(chunk) = upstream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    let f = error_frame(&id, &model, created, 502, &format!("读取上游失败：{e}"));
                    let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
                    sent_error = true;
                    break;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                if let Some(delta) = acc.feed(&line) {
                    let f = chunk_frame(&id, &model, created, delta);
                    if out_tx.send(Ok(bytes::Bytes::from(f))).await.is_err() {
                        return; // 客户端断开
                    }
                }
                if let Some((st, msg)) = acc.error.clone() {
                    let st = if st == 0 { 502 } else { st };
                    let f = error_frame(&id, &model, created, st, &msg);
                    let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
                    sent_error = true;
                    break;
                }
            }
            if sent_error {
                break;
            }
        }

        if !sent_error {
            if !buf.trim().is_empty() {
                if let Some(delta) = acc.feed(&buf) {
                    let f = chunk_frame(&id, &model, created, delta);
                    let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
                }
            }
            if let Some((st, msg)) = acc.error.clone() {
                let st = if st == 0 { 502 } else { st };
                let f = error_frame(&id, &model, created, st, &msg);
                let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
            } else {
                if !acc.saw_finish && acc.text.is_empty() && acc.tool_calls.is_empty() {
                    let f = error_frame(&id, &model, created, 502, "上游没有给出完成信号（响应被截断）");
                    let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
                }
                let fr = acc.finish_reason.clone().unwrap_or_else(|| {
                    if acc.tool_calls.is_empty() { "stop".to_string() } else { "tool_calls".to_string() }
                });
                let tin = if acc.prompt_tokens > 0 {
                    acc.prompt_tokens
                } else {
                    (body_len / 4).max(1) as i64
                };
                let tout = if acc.completion_tokens > 0 {
                    acc.completion_tokens
                } else {
                    (acc.text.chars().count() as i64 / 2).max(1)
                };
                let fin = json!({
                    "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": fr}]
                });
                let _ = out_tx.send(Ok(bytes::Bytes::from(format!("data: {}\n\n", fin)))).await;
                let usg = json!({
                    "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
                    "choices": [],
                    "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout}
                });
                let _ = out_tx.send(Ok(bytes::Bytes::from(format!("data: {}\n\n", usg)))).await;
                let _ = tx
                    .send(crate::StreamStat {
                        tokens_in: tin,
                        tokens_out: tout,
                        saw_done: true,
                        ..Default::default()
                    })
                    .await;
            }
        }
        let _ = out_tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
    });

    let s = tokio_stream::wrappers::ReceiverStream::new(out_rx);
    Ok((Box::pin(s), rx))
}

fn chunk_frame(id: &str, model: &str, created: u64, delta: Value) -> String {
    let v = json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}]
    });
    format!("data: {}\n\n", v)
}

fn error_frame(id: &str, model: &str, created: u64, status: u16, msg: &str) -> String {
    let v = json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [],
        "error": {"code": status, "message": msg, "type": "upstream_error"}
    });
    format!("data: {}\n\n", v)
}


/// 验活：GET /alpha/billing/credits（额度端点，要真 key 才回 200）。
///
/// ★ 不要用公开的 /provider/v1/models 验活：它**不带凭据也回 200**，
///   错的 key 会被误报为「验活通过」，用户以为接好了，一对话才 401。
///   billing 端点是参考实现（endpoints.rs 注释）标注的「本家只用它做 key 探活」。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    if !key_shape_ok(secret) {
        return false;
    }
    let url = format!("{}/alpha/billing/credits", spec.base_url.trim_end_matches('/'));
    let mut rb = http
        .get(&url)
        .header("Authorization", format!("Bearer {secret}"))
        .header("Accept", "application/json");
    for (k, val) in cli_headers() {
        rb = rb.header(k, val);
    }
    match rb.send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 请求体必须把 system 提到顶层、且从 messages 里拿走。
    ///
    /// ★ 这是本家最容易写错的一处：OpenAI 把 system 放在 messages 里，
    ///   CC 把它当**独立字段**，留在 messages 里会变成一条「未知角色」的消息。
    #[test]
    fn build_promotes_system_to_top_level() {
        let v = json!({
            "model": "claude-sonnet-5-5",
            "messages": [
                {"role": "system", "content": "你是助手"},
                {"role": "user", "content": "hi"},
            ],
        });
        let w = build_request_body(&v);
        let p = &w["params"];
        assert_eq!(p["model"], json!("claude-sonnet-5-5"));
        assert_eq!(p["system"][0]["text"], json!("你是助手"));
        let msgs = p["messages"].as_array().expect("messages 数组");
        assert_eq!(msgs.len(), 1, "system 必须从 messages 里移除");
        assert_eq!(msgs[0]["role"], json!("user"));
        assert_eq!(p["stream"], json!(true), "上游只支持流式");
    }

    /// 没有 system 时必须发一个空格占位（上游要求该字段存在）。
    #[test]
    fn build_fills_empty_system_placeholder() {
        let v = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let w = build_request_body(&v);
        assert_eq!(w["params"]["system"][0]["text"], json!(" "));
    }

    /// 三条关键事件的折法：text-delta / reasoning-delta / finish。
    #[test]
    fn acc_parses_text_and_reasoning_deltas() {
        let mut acc = NdjsonAcc::default();
        assert_eq!(
            acc.feed(r#"{"type":"text-delta","text":"你"}"#),
            Some(json!({"content": "你"}))
        );
        assert_eq!(
            acc.feed(r#"{"type":"reasoning-delta","text":"想"}"#),
            Some(json!({"reasoning_content": "想"}))
        );
        assert_eq!(acc.text, "你");
        assert_eq!(acc.reasoning, "想");
    }

    /// ★ 上游 HTTP 恒 200，错误以流内事件表达 —— 不解析事件就完全看不见失败。
    #[test]
    fn acc_captures_in_stream_error() {
        let mut acc = NdjsonAcc::default();
        acc.feed(r#"{"type":"text-delta","text":"半截"}"#);
        acc.feed(r#"{"type":"error","status":429,"message":"rate limited"}"#);
        let (st, msg) = acc.error.expect("应捕获流内错误");
        assert_eq!(st, 429);
        assert!(msg.contains("rate limited"));
    }

    /// finish 事件记下完成信号与 usage。
    #[test]
    fn acc_reads_finish_and_usage() {
        let mut acc = NdjsonAcc::default();
        acc.feed(r#"{"type":"finish","finishReason":"stop","usage":{"inputTokens":120,"outputTokens":34}}"#);
        assert!(acc.saw_finish);
        assert_eq!(acc.finish_reason.as_deref(), Some("stop"));
        assert_eq!(acc.prompt_tokens, 120);
        assert_eq!(acc.completion_tokens, 34);
    }

    /// 工具调用是**整体**事件（id/name/args 一次给全），与 Anthropic 的分片不同。
    #[test]
    fn acc_parses_tool_call_as_whole() {
        let mut acc = NdjsonAcc::default();
        let d = acc
            .feed(r#"{"type":"tool-call","toolCallId":"c1","toolName":"get_weather","args":{"city":"北京"}}"#)
            .expect("应有 tool_calls 帧");
        let calls = d["tool_calls"].as_array().expect("tool_calls");
        assert_eq!(calls[0]["id"], json!("c1"));
        assert_eq!(calls[0]["function"]["name"], json!("get_weather"));
        assert!(calls[0]["function"]["arguments"].as_str().unwrap().contains("北京"));
    }

    /// 402（余额不足）按 429 处理；400/422 是请求问题；401/403 是凭据问题。
    #[test]
    fn error_class_mapping() {
        assert_eq!(class_of(402), ErrorClass::Quota);
        assert_eq!(class_of(429), ErrorClass::Quota);
        assert_eq!(class_of(401), ErrorClass::Auth);
        assert_eq!(class_of(400), ErrorClass::Rejected);
        assert_eq!(class_of(500), ErrorClass::Transient);
    }

    /// 空行 / 非 JSON 行不能 panic（NDJSON 里可能有心跳空行）。
    #[test]
    fn acc_ignores_blank_and_garbage_lines() {
        let mut acc = NdjsonAcc::default();
        assert_eq!(acc.feed(""), None);
        assert_eq!(acc.feed("   "), None);
        assert_eq!(acc.feed("not json"), None);
        assert_eq!(acc.feed(r#"{"type":"start-step"}"#), None);
    }
    /// ★ 形态不对要**在发请求前**拒绝：发出去只换回一条语焉不详的 401，
    ///   用户会以为是「登录态失效」反复重登，而真正该做的是换一把正确的 Key。
    #[test]
    fn key_shape_rejects_wrong_prefix() {
        assert!(key_shape_ok("user_abc123"));
        assert!(key_shape_ok("  user_abc-123_XYZ  "), "前后空白要容忍");
        assert!(!key_shape_ok("sk-abc123"), "sk- 开头必须拒");
        assert!(!key_shape_ok("user_"), "只有前缀不算");
        assert!(!key_shape_ok(""), "空串");
        assert!(!key_shape_ok("user_ab c"), "含空格");
    }

    /// 脱敏不能泄漏完整 Key。
    #[test]
    fn mask_keeps_only_tail() {
        let m = mask_key("user_abcdefghijklmnop1234");
        assert!(m.starts_with("user_…"), "m={m}");
        assert!(m.ends_with("1234"), "m={m}");
        assert!(!m.contains("abcdefghij"), "不能泄漏中段：{m}");
        assert_eq!(mask_key("user_short"), "user_…");
    }
}
