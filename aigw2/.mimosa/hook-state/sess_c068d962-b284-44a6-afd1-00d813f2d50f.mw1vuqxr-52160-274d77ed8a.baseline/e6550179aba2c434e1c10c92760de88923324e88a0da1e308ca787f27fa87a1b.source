//! Accio（阿里 Phoenix 网关 / ADK）—— Gemini 风格信封，token 在**请求体**里。
//!
//! ── 上游长什么样（逆向来源：Accio Work 桌面端 app.asar）──────────
//! ```text
//! POST https://phoenix-gw.alibaba.com/api/adk/llm/generateContent?sg_k=<md5(requestId)>
//!   body { model, tenant:"accio-agent", iai_tag:"phoenix-desktop",
//!          request_id, token:<accessToken>, contents:[{role, parts:[{text}]}] }
//!   → SSE：data: {"content":{"parts":[{"text":"你"}]},"partial":true}
//!           data: {"content":{"parts":[{"thought":true,"text":"想…"}]}}
//!           data: {"turn_complete":true,"finish_reason":"STOP","usage_metadata":{…}}
//!           data: [DONE]
//! ```
//!
//! ── 三处与 OpenAI 不同，必须在这里处理 ─────────────────────────
//!   1. **token 不在 Authorization 头**，而在**请求体**的 `token` 字段；
//!   2. **思考是 part 上的标记**（`{"thought":true,"text":…}`），不是独立字段；
//!   3. **业务错误写在帧里**（`error_code` / `error_message`），**HTTP 常是 200**
//!      —— 只看状态码会把失败当成成功。
//!
//! ── 与其它 adapter 的差别 ────────────────────────────────────
//! 请求体不是标准 chat（要转成 `contents` / `parts`），响应不是 OpenAI SSE
//! （要解析 ADK 信封）。因此走自定义 `upstream_kind`，不做通用透传。

use serde_json::{json, Value};

use aigw_registry::ProviderSpec;

use crate::common::{er, nanos_hex, secs, truncate};
use crate::{ErrorClass, RelayError};

/// 固定的租户与来源标识（桌面端两个构建共用）。
const TENANT: &str = "accio-agent";
const IAI_TAG: &str = "phoenix-desktop";
const APP_VERSION: &str = "0.32.6";

fn rand_hex(n: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `sg_k` 参数 = md5(requestId) 的小写十六进制。
fn sg_k(request_id: &str) -> String {
    use md5::{Digest as _, Md5};
    let mut h = Md5::new();
    h.update(request_id.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 把 OpenAI messages 转成 ADK 的 `contents`（role: user/model）。
pub fn build_contents(v: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let Some(arr) = v.get("messages").and_then(|m| m.as_array()) else {
        return vec![json!({"role": "user", "parts": [{"text": "你好"}]})];
    };
    for m in arr {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        // Gemini/ADK 只有 user 与 model 两种角色；system 折进 user
        let adk_role = if role == "assistant" { "model" } else { "user" };
        let text = content_text(m);
        if text.trim().is_empty() {
            continue;
        }
        out.push(json!({"role": adk_role, "parts": [{"text": text}]}));
    }
    if out.is_empty() {
        out.push(json!({"role": "user", "parts": [{"text": "你好"}]}));
    }
    out
}

fn content_text(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .or_else(|| p.get("input_text"))
                    .and_then(|t| t.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn build_payload(v: &Value, token: &str, request_id: &str) -> Value {
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("");
    json!({
        "model": model,
        "tenant": TENANT,
        "iai_tag": IAI_TAG,
        "request_id": request_id,
        "token": token,
        "contents": build_contents(v),
    })
}

fn headers(spec: &ProviderSpec) -> Vec<(&'static str, String)> {
    let region = match spec.region.as_str() {
        "cn" => "CN",
        _ => "GLOBAL",
    };
    vec![
        ("x-app-version", APP_VERSION.to_string()),
        ("x-package-region", region.to_string()),
        ("version", APP_VERSION.to_string()),
        ("Accept", "text/event-stream".to_string()),
    ]
}

/// ADK 帧 → 增量的解析状态机。
#[derive(Default)]
pub struct AdkAcc {
    pub text: String,
    pub reasoning: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub finish_reason: Option<String>,
    pub done: bool,
    pub error: Option<String>,
}

impl AdkAcc {
    /// 吃一行 SSE `data:` 载荷，返回要下发的 delta（None = 无内容）。
    pub fn feed(&mut self, data: &str) -> Option<Value> {
        let d = data.trim();
        if d.is_empty() {
            return None;
        }
        if d == "[DONE]" {
            self.done = true;
            return None;
        }
        let Ok(v) = serde_json::from_str::<Value>(d) else {
            return None;
        };
        // ★ 业务错误写在帧里，HTTP 常是 200 —— 不判这里就等于把失败当成功。
        if let Some(code) = v
            .get("error_code")
            .or_else(|| v.get("errorCode"))
            .and_then(|x| x.as_i64())
        {
            if code != 0 {
                let msg = v
                    .get("error_message")
                    .or_else(|| v.get("errorMessage"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("上游返回错误");
                self.error = Some(format!("code={code}：{msg}"));
                self.done = true;
                return None;
            }
        }
        if v.get("turn_complete").and_then(|x| x.as_bool()) == Some(true)
            || v.get("turnComplete").and_then(|x| x.as_bool()) == Some(true)
        {
            self.done = true;
            if let Some(fr) = v
                .get("finish_reason")
                .or_else(|| v.get("finishReason"))
                .and_then(|x| x.as_str())
            {
                self.finish_reason = Some(if fr == "MAX_TOKENS" || fr == "length" {
                    "length".to_string()
                } else {
                    "stop".to_string()
                });
            }
            if let Some(u) = v
                .get("usage_metadata")
                .or_else(|| v.get("usageMetadata"))
            {
                if let Some(n) = u
                    .get("prompt_token_count")
                    .or_else(|| u.get("promptTokenCount"))
                    .and_then(|x| x.as_i64())
                {
                    self.prompt_tokens = n;
                }
                if let Some(n) = u
                    .get("candidates_token_count")
                    .or_else(|| u.get("candidatesTokenCount"))
                    .and_then(|x| x.as_i64())
                {
                    self.completion_tokens = n;
                }
            }
        }
        // 内容：content.parts[] 里逐个 part 看
        let Some(parts) = v
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
        else {
            return None;
        };
        let mut text_out = String::new();
        let mut think_out = String::new();
        for p in parts {
            let is_thought = p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false);
            if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                if !t.is_empty() {
                    if is_thought {
                        think_out.push_str(t);
                    } else {
                        text_out.push_str(t);
                    }
                }
            }
        }
        if !think_out.is_empty() {
            self.reasoning.push_str(&think_out);
            return Some(json!({"reasoning_content": think_out}));
        }
        if !text_out.is_empty() {
            self.text.push_str(&text_out);
            return Some(json!({"content": text_out}));
        }
        None
    }
}

/// 发一次 generateContent，拿回 SSE 响应。
async fn send(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<reqwest::Response, RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let request_id = rand_hex(16);
    let payload = build_payload(&v, secret, &request_id);
    let base = spec.base_url.trim_end_matches('/');
    let url = format!("{base}/generateContent?sg_k={}", sg_k(&request_id));
    let mut rb = http
        .post(&url)
        .header("Content-Type", "application/json")
        .body(payload.to_string());
    for (k, val) in headers(spec) {
        rb = rb.header(k, val);
    }
    let resp = rb
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(er(
            401,
            "Accio 登录态已失效：请重新登录或粘贴新的 accessToken".to_string(),
            ErrorClass::Auth,
        ));
    }
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        return Err(er(
            status,
            format!("Accio 返回 HTTP {status}：{}", truncate(&t, 240)),
            ErrorClass::Transient,
        ));
    }
    Ok(resp)
}

/// 非流式：聚合整条 SSE。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let resp = send(http, spec, secret, body).await?;
    let raw = resp
        .text()
        .await
        .map_err(|e| er(502, format!("读取流失败：{e}"), ErrorClass::Transient))?;
    let mut acc = AdkAcc::default();
    for line in raw.lines() {
        let Some(rest) = line.trim_start().strip_prefix("data:") else {
            continue;
        };
        acc.feed(rest);
        if acc.error.is_some() {
            break;
        }
    }
    if let Some(err) = acc.error.clone() {
        return Err(er(502, format!("Accio 上游错误：{err}"), ErrorClass::Transient));
    }
    if acc.text.trim().is_empty() && acc.reasoning.trim().is_empty() {
        return Err(er(
            502,
            format!(
                "上游没有返回任何内容：{}",
                truncate(&raw, 200)
            ),
            ErrorClass::Transient,
        ));
    }
    let model = {
        let v: Value = serde_json::from_slice(body).unwrap_or(json!({}));
        v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string()
    };
    let tin = if acc.prompt_tokens > 0 { acc.prompt_tokens } else { (body.len() / 4).max(1) as i64 };
    let tout = if acc.completion_tokens > 0 {
        acc.completion_tokens
    } else {
        (acc.text.chars().count() as i64 / 2).max(1)
    };
    let mut msg = json!({"role": "assistant", "content": acc.text});
    if !acc.reasoning.is_empty() {
        msg["reasoning_content"] = json!(acc.reasoning);
    }
    Ok(json!({
        "id": format!("chatcmpl-accio-{}", nanos_hex()),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{"index": 0, "message": msg,
                     "finish_reason": acc.finish_reason.unwrap_or_else(|| "stop".to_string())}],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout},
    }))
}

/// 流式：逐帧翻译成 chat SSE。
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

    let resp = send(http, spec, secret, body).await?;
    let model = {
        let v: Value = serde_json::from_slice(body).unwrap_or(json!({}));
        v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string()
    };
    let body_len = body.len();
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(8);
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let mut acc = AdkAcc::default();
        let mut buf = String::new();
        let mut upstream = resp.bytes_stream();
        while let Some(chunk) = upstream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    let f = err_frame(&format!("读取上游失败：{e}"));
                    let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
                    break;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let Some(rest) = line.trim_start().strip_prefix("data:") else {
                    continue;
                };
                if let Some(delta) = acc.feed(rest) {
                    let f = format!(
                        "data: {}\n\n",
                        json!({"id": "chatcmpl-accio", "object": "chat.completion.chunk",
                               "created": secs(), "model": model,
                               "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}]})
                    );
                    if out_tx.send(Ok(bytes::Bytes::from(f))).await.is_err() {
                        return;
                    }
                }
                if let Some(err) = acc.error.clone() {
                    let _ = out_tx.send(Ok(bytes::Bytes::from(err_frame(&err)))).await;
                    let _ = out_tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
                    return;
                }
            }
        }
        let tin = if acc.prompt_tokens > 0 { acc.prompt_tokens } else { (body_len / 4).max(1) as i64 };
        let tout = if acc.completion_tokens > 0 {
            acc.completion_tokens
        } else {
            (acc.text.chars().count() as i64 / 2).max(1)
        };
        let fr = acc.finish_reason.clone().unwrap_or_else(|| "stop".to_string());
        let fin = json!({"id": "chatcmpl-accio", "object": "chat.completion.chunk",
                         "created": secs(), "model": model,
                         "choices": [{"index": 0, "delta": {}, "finish_reason": fr}]});
        let _ = out_tx.send(Ok(bytes::Bytes::from(format!("data: {}\n\n", fin)))).await;
        let usg = json!({"id": "chatcmpl-accio", "object": "chat.completion.chunk",
                         "created": secs(), "model": model, "choices": [],
                         "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout}});
        let _ = out_tx.send(Ok(bytes::Bytes::from(format!("data: {}\n\n", usg)))).await;
        let _ = tx
            .send(crate::StreamStat {
                tokens_in: tin,
                tokens_out: tout,
                saw_done: true,
                ..Default::default()
            })
            .await;
        let _ = out_tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
    });

    let s = tokio_stream::wrappers::ReceiverStream::new(out_rx);
    Ok((Box::pin(s), rx))
}

fn err_frame(msg: &str) -> String {
    format!(
        "data: {}\n\n",
        json!({"error": {"code": 502, "message": msg, "type": "upstream_error"}})
    )
}

/// 验活：用一个极小的 generateContent 探一次（token 就够证明可用）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let probe = json!({"model": "gemini-2.0-flash", "messages": [{"role": "user", "content": "hi"}]});
    send(http, spec, secret, probe.to_string().as_bytes())
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 系统消息要折进 user（ADK 只有 user/model 两种角色）。
    #[test]
    fn contents_map_roles() {
        let v = serde_json::json!({"messages": [
            {"role": "system", "content": "你是助手"},
            {"role": "user", "content": "问题"},
            {"role": "assistant", "content": "回答"},
        ]});
        let c = build_contents(&v);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0]["role"], serde_json::json!("user"), "system 折进 user");
        assert_eq!(c[2]["role"], serde_json::json!("model"), "assistant → model");
    }

    /// 空消息要兜底（上游不接受空 contents）。
    #[test]
    fn contents_fallback() {
        let c = build_contents(&serde_json::json!({}));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0]["role"], serde_json::json!("user"));
    }

    /// ★ 业务错误写在帧里，HTTP 常是 200 —— 不判就是静默成功。
    #[test]
    fn adk_error_frame_is_detected() {
        let mut acc = AdkAcc::default();
        acc.feed(r#"{"error_code":40003,"error_message":"quota exceeded"}"#);
        let e = acc.error.expect("应捕获业务错误");
        assert!(e.contains("quota exceeded"), "e={e}");
        assert!(acc.done);
    }

    #[test]
    fn adk_text_and_thought_parts() {
        let mut acc = AdkAcc::default();
        assert_eq!(
            acc.feed(r#"{"content":{"parts":[{"text":"你"}]},"partial":true}"#),
            Some(serde_json::json!({"content": "你"}))
        );
        assert_eq!(
            acc.feed(r#"{"content":{"parts":[{"thought":true,"text":"想"}]}}"#),
            Some(serde_json::json!({"reasoning_content": "想"}))
        );
        assert_eq!(acc.text, "你");
        assert_eq!(acc.reasoning, "想");
    }

    /// turn_complete 带 finish_reason 与 usage。
    #[test]
    fn adk_turn_complete_reads_usage() {
        let mut acc = AdkAcc::default();
        acc.feed(r#"{"turn_complete":true,"finish_reason":"STOP","usage_metadata":{"prompt_token_count":10,"candidates_token_count":5}}"#);
        assert!(acc.done);
        assert_eq!(acc.finish_reason.as_deref(), Some("stop"));
        assert_eq!((acc.prompt_tokens, acc.completion_tokens), (10, 5));
    }

    /// MAX_TOKENS 要翻成 length（客户端据此判断被截断）。
    #[test]
    fn adk_max_tokens_maps_to_length() {
        let mut acc = AdkAcc::default();
        acc.feed(r#"{"turn_complete":true,"finish_reason":"MAX_TOKENS"}"#);
        assert_eq!(acc.finish_reason.as_deref(), Some("length"));
    }

    /// sg_k 必须是 md5(requestId) 的小写十六进制。
    #[test]
    fn sg_k_is_md5_hex() {
        // md5("abc") = 900150983cd24fb0d6963f7d28e17f72
        assert_eq!(sg_k("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    /// token 必须进请求体（不走 Authorization 头）。
    #[test]
    fn payload_carries_token_in_body() {
        let v = serde_json::json!({"model": "gemini-3-flash", "messages": [{"role": "user", "content": "hi"}]});
        let p = build_payload(&v, "TOKEN-XYZ", "req-1");
        assert_eq!(p["token"], serde_json::json!("TOKEN-XYZ"));
        assert_eq!(p["tenant"], serde_json::json!("accio-agent"));
        assert_eq!(p["request_id"], serde_json::json!("req-1"));
        assert_eq!(p["model"], serde_json::json!("gemini-3-flash"));
    }
}
