//! Devin（Cognition）会话型 Agent 适配器。
//!
//! 为什么需要：Devin **没有** chat completions 端点（实测 2026-10-22：
//! `GET https://api.devin.ai/v1/sessions` 返回 403 = 端点存在但需鉴权，
//! 而 `/v1/chat/completions` 并不存在）。它的形态是「一次调用 = 开一个 Agent 会话」，
//! 由 Devin 在云端干活，完成后从会话消息里取回复。
//!
//! 链路：
//! 1) `POST {base}/sessions` body `{"prompt": "<system + 多轮对话合并文本>"}`，
//!    头 `Authorization: Bearer <token>`；返回 `session_id` 与 `url`；
//! 2) 轮询 `GET {base}/sessions/{id}`，`status_enum` ∈
//!    {working, blocked, finished, expired}；
//! 3) 取最后一条 `type == "devin_message"` 的 `message` 作为回复正文；
//! 4) 流式模式下：会话跑完后把正文按 48 字符切块吐出（Devin 上游本身不流式）。
//!
//! ⚠ 成本提示：每一次 `/v1/chat/completions` 都会**新建一个 Devin 会话**（消耗额度），
//! 所以本平台只声明一个模型名 `devin`，必须显式写 `devin@sub-devin` 才会走到这里。

use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::Stream;
use serde_json::{json, Value};
use std::pin::Pin;
use std::time::Duration;

use crate::RelayError;

const DEVIN_BASE: &str = "https://api.devin.ai/v1";
const DEVIN_UA: &str = "aigw-panel/1.0";
const DEVIN_POLL_SECS: u64 = 3;
const DEVIN_DEADLINE_SECS: u64 = 300;

fn er(status: u16, body: String, class: ErrorClass) -> RelayError {
    RelayError { status, body, class }
}

pub fn trunc(s: &str, n: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.chars().count() >= n {
            break;
        }
        out.push(c);
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn nanos_hex() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

fn pick_one(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

fn cookie_value(s: &str, key: &str) -> Option<String> {
    for part in s.split(';') {
        let part = part.trim();
        let Some((k, v)) = part.split_once('=') else { continue };
        if k.trim().eq_ignore_ascii_case(key) {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Devin 凭据：API token（`devin-` 开头或组织 Key），可选 org_id。
#[derive(Debug, Default)]
pub struct DevinCred {
    pub token: String,
    pub org_id: Option<String>,
}

pub fn parse_cred(secret: &str) -> Result<DevinCred, String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("Devin 凭据为空：请粘贴 API token（app.devin.ai → Settings → API Keys）".to_string());
    }
    if s.starts_with('{') {
        let v: Value = serde_json::from_str(s).map_err(|e| format!("Devin 凭据不是合法 JSON：{e}"))?;
        let token = pick_one(&v, &["token", "api_key", "apiKey", "access_token", "accessToken", "secret"])
            .ok_or_else(|| "Devin 凭据 JSON 里缺 token 字段".to_string())?;
        let org_id = pick_one(&v, &["org_id", "orgId", "organization_id"]);
        return Ok(DevinCred { token, org_id });
    }
    if s.contains('=') || s.contains(';') {
        if let Some(t) = cookie_value(s, "token").or_else(|| cookie_value(s, "api_key")) {
            return Ok(DevinCred { token: t, org_id: None });
        }
        return Err("Devin 凭据像 Cookie 但没找到 token= / api_key= 字段".to_string());
    }
    Ok(DevinCred { token: s.to_string(), org_id: None })
}

fn base_of(spec: &ProviderSpec) -> String {
    let b = spec.base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        DEVIN_BASE.to_string()
    } else {
        b.to_string()
    }
}

fn headers(spec: &ProviderSpec, cred: &DevinCred) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("Authorization".to_string(), format!("Bearer {}", cred.token)),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), DEVIN_UA.to_string()),
    ];
    if let Some(org) = &cred.org_id {
        hs.push(("X-Org-Id".to_string(), org.clone()));
    }
    for (k, v) in spec.headers.iter() {
        hs.retain(|(hk, _)| !hk.eq_ignore_ascii_case(k));
        hs.push((k.clone(), v.clone()));
    }
    hs
}

fn apply(mut rb: reqwest::RequestBuilder, hs: &[(String, String)]) -> reqwest::RequestBuilder {
    for (k, v) in hs {
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb
}

fn content_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let mut out = String::new();
            for it in items {
                if let Some(s) = it.get("text").and_then(|x| x.as_str()) {
                    out.push_str(s);
                } else if let Some(s) = it.as_str() {
                    out.push_str(s);
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// 把 OpenAI 形状的 messages 压成一段文本：system 置顶，多轮拼 User:/Assistant:。
pub fn flatten_prompt(v: &Value) -> String {
    let mut system = String::new();
    let mut turns: Vec<(String, String)> = Vec::new();
    if let Some(arr) = v.get("messages").and_then(|x| x.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
            let text = content_text(m.get("content"));
            if text.trim().is_empty() {
                continue;
            }
            match role {
                "system" | "developer" => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&text);
                }
                _ => turns.push((role.to_string(), text)),
            }
        }
    }
    if turns.is_empty() {
        if let Some(s) = v.get("prompt").and_then(|x| x.as_str()) {
            turns.push(("user".to_string(), s.to_string()));
        }
    }
    let single = turns.len() <= 1;
    let mut out = String::new();
    if !system.is_empty() {
        out.push_str(&system);
        out.push_str("\n\n");
    }
    for (role, text) in &turns {
        if single {
            out.push_str(text);
        } else {
            let label = if role == "assistant" { "Assistant" } else { "User" };
            out.push_str(label);
            out.push_str(": ");
            out.push_str(text);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

async fn create_session(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &DevinCred,
    prompt: &str,
) -> Result<(String, String), RelayError> {
    let base = base_of(spec);
    let rb = http
        .post(format!("{base}/sessions"))
        .timeout(Duration::from_secs(120))
        .json(&json!({ "prompt": prompt }));
    let rb = apply(rb, &headers(spec, cred));
    let resp = rb
        .send()
        .await
        .map_err(|e| er(502, format!("Devin 建会话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let class = match status {
            401 | 403 => ErrorClass::Auth,
            402 => ErrorClass::Quota,
            429 => ErrorClass::Rate,
            400..=499 => ErrorClass::Rejected,
            _ => ErrorClass::Transient,
        };
        return Err(er(status, format!("Devin 建会话失败（{status}）：{}", trunc(&text, 300)), class));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| er(502, format!("Devin 建会话响应不是 JSON：{e}；原文 {}", trunc(&text, 200)), ErrorClass::Transient))?;
    let id = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("sessionId").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
        .ok_or_else(|| er(502, format!("Devin 建会话响应里没有 session_id：{}", trunc(&text, 200)), ErrorClass::Transient))?;
    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok((id, url))
}

async fn get_session(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &DevinCred,
    id: &str,
) -> Result<Value, RelayError> {
    let base = base_of(spec);
    let rb = http
        .get(format!("{base}/sessions/{id}"))
        .timeout(Duration::from_secs(60));
    let rb = apply(rb, &headers(spec, cred));
    let resp = rb
        .send()
        .await
        .map_err(|e| er(502, format!("Devin 查询会话失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let class = match status {
            401 | 403 => ErrorClass::Auth,
            404 => ErrorClass::Rejected,
            429 => ErrorClass::Rate,
            _ => ErrorClass::Transient,
        };
        return Err(er(status, format!("Devin 查询会话失败（{status}）：{}", trunc(&text, 300)), class));
    }
    serde_json::from_str(&text)
        .map_err(|e| er(502, format!("Devin 会话响应不是 JSON：{e}；原文 {}", trunc(&text, 200)), ErrorClass::Transient))
}

pub fn session_status(v: &Value) -> String {
    v.get("status_enum")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("status").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string()
}

pub fn status_done(s: &str) -> bool {
    matches!(s, "finished" | "blocked" | "expired" | "stopped" | "error" | "failed")
}

/// 取最后一条 devin_message 作为回复；没有就退回最后一条非 user 消息。
pub fn extract_reply(v: &Value) -> String {
    let Some(arr) = v.get("messages").and_then(|x| x.as_array()) else {
        return String::new();
    };
    for m in arr.iter().rev() {
        let ty = m.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if ty == "devin_message" {
            if let Some(s) = m.get("message").and_then(|x| x.as_str()) {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    for m in arr.iter().rev() {
        let ty = m.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if ty != "user_message" {
            if let Some(s) = m.get("message").and_then(|x| x.as_str()) {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    String::new()
}

/// 建会话并轮询到结束，返回 (回复正文, 末态状态)。
async fn run_session(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &DevinCred,
    prompt: &str,
) -> Result<(String, String), RelayError> {
    let (id, url) = create_session(http, spec, cred, prompt).await?;
    let test_mode = base_of(spec).starts_with("http://");
    let interval = if test_mode {
        Duration::from_millis(10)
    } else {
        Duration::from_secs(DEVIN_POLL_SECS)
    };
    let deadline = if test_mode { 60 } else { DEVIN_DEADLINE_SECS };
    let mut elapsed: u64 = 0;
    loop {
        let v = get_session(http, spec, cred, &id).await?;
        let st = session_status(&v);
        let reply = extract_reply(&v);
        if status_done(&st) {
            if reply.trim().is_empty() {
                return Err(er(
                    502,
                    format!("Devin 会话已结束（status={st}）但没有回复，会话地址 {url}"),
                    ErrorClass::Transient,
                ));
            }
            return Ok((reply, st));
        }
        if elapsed >= deadline {
            if !reply.is_empty() {
                return Ok((reply, format!("timeout:{st}")));
            }
            return Err(er(
                504,
                format!("Devin 会话 {id} 在 {deadline} 秒内没有产出（会话地址 {url}）"),
                ErrorClass::Transient,
            ));
        }
        tokio::time::sleep(interval).await;
        elapsed += interval.as_secs().max(1);
    }
}

fn read_model(v: &Value) -> String {
    v.get("model")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "devin".to_string())
}

pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let prompt = flatten_prompt(&v);
    if prompt.trim().is_empty() {
        return Err(er(400, "Devin 请求里没有可用的用户内容".to_string(), ErrorClass::Rejected));
    }
    let model = read_model(&v);
    let (text, st) = run_session(http, spec, &cred, &prompt).await?;
    let tin = crate::estimate_stream_tokens(prompt.len(), 0).0;
    let tout = crate::estimate_stream_tokens(0, text.chars().count()).1;
    Ok(json!({
        "id": format!("chatcmpl-devin-{}", nanos_hex()),
        "object": "chat.completion",
        "created": now_secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout },
        "x_devin_status": st
    }))
}

pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<(
    Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    tokio::sync::mpsc::Receiver<crate::StreamStat>,
), RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let prompt = flatten_prompt(&v);
    if prompt.trim().is_empty() {
        return Err(er(400, "Devin 请求里没有可用的用户内容".to_string(), ErrorClass::Rejected));
    }
    let model = read_model(&v);
    let text = {
        let (t, _st) = run_session(http, spec, &cred, &prompt).await?;
        t
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let (stat_tx, stat_rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(8);
    let in_bytes = prompt.len();
    tokio::spawn(async move {
        let id = format!("chatcmpl-devin-{}", nanos_hex());
        let mk = |delta: Value, finish: Option<&str>| -> String {
            let mut choice = serde_json::Map::new();
            choice.insert("index".to_string(), json!(0));
            choice.insert("delta".to_string(), delta);
            choice.insert(
                "finish_reason".to_string(),
                match finish {
                    Some(f) => json!(f),
                    None => Value::Null,
                },
            );
            let frame = json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": now_secs(),
                "model": model,
                "choices": [Value::Object(choice)]
            });
            format!("data: {frame}\n\n")
        };
        let mut sent_role = false;
        let mut out_chars = 0usize;
        let chars: Vec<char> = text.chars().collect();
        let mut idx = 0usize;
        while idx < chars.len() {
            let end = (idx + 48).min(chars.len());
            let piece: String = chars[idx..end].iter().collect();
            out_chars += piece.chars().count();
            let mut delta = serde_json::Map::new();
            if !sent_role {
                delta.insert("role".to_string(), json!("assistant"));
                sent_role = true;
            }
            delta.insert("content".to_string(), json!(piece));
            let frame = mk(Value::Object(delta), None);
            if tx.send(Ok(Bytes::from(frame))).await.is_err() {
                // 下游断开（客户端取消）：已推送的部分仍算消耗，补一条统计再退出，
                // 否则这次请求在台账里只有「失败 + 0 token」，成本归因偏低。
                let (ti, to) = crate::estimate_stream_tokens(in_bytes, out_chars);
                let _ = stat_tx
                    .send(crate::StreamStat {
                        error: Some(ErrorClass::Transient),
                        finish_reason: None,
                        tokens_in: ti,
                        tokens_out: to,
                        client_gone: true,
                        ..Default::default()
                    })
                    .await;
                return;
            }
            idx = end;
        }
        if !sent_role {
            let mut delta = serde_json::Map::new();
            delta.insert("role".to_string(), json!("assistant"));
            delta.insert("content".to_string(), json!(""));
            let frame = mk(Value::Object(delta), None);
            let _ = tx.send(Ok(Bytes::from(frame))).await;
        }
        let mut delta = serde_json::Map::new();
        delta.insert("content".to_string(), json!(""));
        let frame = mk(Value::Object(delta), Some("stop"));
        let _ = tx.send(Ok(Bytes::from(frame))).await;
        let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
        let (ti, to) = crate::estimate_stream_tokens(in_bytes, out_chars);
        let _ = stat_tx
            .send(crate::StreamStat {
                error: None,
                finish_reason: Some("stop".to_string()),
                tokens_in: ti,
                tokens_out: to,
                ..Default::default()
            })
            .await;
    });
    Ok((Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)), stat_rx))
}

pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok(cred) = parse_cred(secret) else { return false };
    let base = base_of(spec);
    let rb = http
        .get(format!("{base}/sessions?limit=1"))
        .timeout(Duration::from_secs(30));
    let rb = apply(rb, &headers(spec, &cred));
    let Ok(resp) = rb.send().await else { return false };
    if !resp.status().is_success() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use aigw_registry::parse_spec;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn spec_for(base: &str) -> ProviderSpec {
        let raw = format!(
            "id = \"sub-devin\"\nname = \"Devin\"\nenabled = true\nauth = \"api_key\"\nbase_url = \"{base}\"\nformat = \"openai\"\nmodels = [\"devin\"]\nmodels_policy = \"static\"\nstream_dialect = \"sse_delta\"\nupstream_kind = \"devin\"\n"
        );
        parse_spec(&raw).expect("spec")
    }

    async fn spawn_mock() -> String {
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        let app = Router::new()
            .route(
                "/sessions",
                axum::routing::get(|| async { axum::Json(json!([])) }).post(|| async {
                    axum::Json(json!({
                        "session_id": "s-1",
                        "url": "https://app.devin.ai/sessions/s-1",
                        "is_new_session": true
                    }))
                }),
            )
            .route(
                "/sessions/:id",
                axum::routing::get(move || {
                    let h = h2.clone();
                    async move {
                        let n = h.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            axum::Json(json!({
                                "session_id": "s-1",
                                "status_enum": "working",
                                "messages": [{ "type": "user_message", "message": "你好" }]
                            }))
                        } else {
                            axum::Json(json!({
                                "session_id": "s-1",
                                "status_enum": "finished",
                                "messages": [
                                    { "type": "user_message", "message": "你好" },
                                    { "type": "devin_message", "message": "我是 Devin" }
                                ]
                            }))
                        }
                    }
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[test]
    fn parse_cred_accepts_bare_and_json() {
        assert_eq!(parse_cred("devin-abc123").unwrap().token, "devin-abc123");
        let c = parse_cred(r#"{"token":"devin-x","org_id":"org-1"}"#).unwrap();
        assert_eq!(c.token, "devin-x");
        assert_eq!(c.org_id.as_deref(), Some("org-1"));
        assert!(parse_cred("   ").is_err());
    }

    #[test]
    fn flatten_prompt_joins_turns_and_keeps_system() {
        let v = json!({"messages": [
            {"role":"system","content":"你是助手"},
            {"role":"user","content":"第一问"},
            {"role":"assistant","content":"第一答"},
            {"role":"user","content":[{"type":"text","text":"第二问"}]}
        ]});
        let p = flatten_prompt(&v);
        assert!(p.starts_with("你是助手"), "{p}");
        assert!(p.contains("User: 第一问"), "{p}");
        assert!(p.contains("Assistant: 第一答"), "{p}");
        assert!(p.contains("User: 第二问"), "{p}");
        let single = flatten_prompt(&json!({"messages":[{"role":"user","content":"只有一句"}]}));
        assert_eq!(single, "只有一句");
    }

    #[test]
    fn status_and_reply_extraction() {
        assert!(status_done("finished"));
        assert!(status_done("blocked"));
        assert!(!status_done("working"));
        let v = json!({"status_enum":"finished","messages":[
            {"type":"user_message","message":"q"},
            {"type":"devin_message","message":"a1"},
            {"type":"devin_message","message":"a2"}
        ]});
        assert_eq!(session_status(&v), "finished");
        assert_eq!(extract_reply(&v), "a2");
    }

    #[tokio::test]
    async fn devin_nonstream_via_mock() {
        let base = spawn_mock().await;
        let spec = spec_for(&base);
        let body = json!({"model":"devin","messages":[{"role":"user","content":"你好"}]});
        let v = chat_nonstream(&reqwest::Client::new(), &spec, "devin-test", body.to_string().as_bytes())
            .await
            .expect("ok");
        assert_eq!(v["choices"][0]["message"]["content"], "我是 Devin");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["x_devin_status"], "finished");
    }

    #[tokio::test]
    async fn devin_stream_via_mock() {
        let base = spawn_mock().await;
        let spec = spec_for(&base);
        let body = json!({"model":"devin","stream":true,"messages":[{"role":"user","content":"你好"}]});
        let (stream, mut stat_rx) = chat_stream(&reqwest::Client::new(), &spec, "devin-test", body.to_string().as_bytes())
            .await
            .expect("stream");
        use futures_util::StreamExt;
        let mut out = String::new();
        let mut s = stream;
        while let Some(chunk) = s.next().await {
            let bytes = chunk.unwrap();
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        assert!(out.contains("我是 Devin"), "{out}");
        assert!(out.contains("[DONE]"), "{out}");
        let st = stat_rx.recv().await.expect("stat");
        assert_eq!(st.finish_reason.as_deref(), Some("stop"));
        assert!(st.error.is_none());
    }

    #[tokio::test]
    async fn devin_verify_hits_sessions() {
        let base = spawn_mock().await;
        let spec = spec_for(&base);
        let ok = verify(&reqwest::Client::new(), &spec, "devin-test").await;
        assert!(ok);
    }
}
