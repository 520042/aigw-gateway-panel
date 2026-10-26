//! Qwen AI 国际版（chat.qwen.ai）adapter —— 纯 HTTP，网页登录态（JWT token + Cookie）。
//!
//! 一手来源：xiaoY233/Chat2API 的 src/main/proxy/adapters/qwen-ai.ts（其基于 qwen3-reverse）。
//! 该平台**没有** OpenAI 兼容端点，网页对话是私有协议，一轮需要两步：
//! 1. POST /api/v2/chats/new 建会话 → data.data.id 即 chat_id；
//! 2. POST /api/v2/chat/completions?chat_id=… 带 envelope（stream=true 恒开），
//!    SSE 帧里 delta.phase 区分 think / thinking_summary / answer。
//!
//! 凭据是浏览器登录态：Bearer token（Local Storage 的 token）+ 一组 Cookie
//! （cnaui/aui/sca/xlly_s/cna/token/_bl_uid/x-ap 等）。只给 token 不给 Cookie
//! 上游通常回 Bad_Request，所以 parse_cred 同时接受「整段 Cookie」形态。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const QWEN_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";
const QWEN_VERSION: &str = "0.2.7";
const QWEN_BX_V: &str = "2.5.36";

fn er(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError {
        status,
        body: body.into(),
        class,
    }
}

fn secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn uuid_v4ish() -> String {
    use rand::Rng;
    let mut b = [0u8; 16];
    rand::thread_rng().fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// 解析凭据，三种形态都接受：
/// - JSON：{"token":"…","cookies":"…"}
/// - 整段 Cookie：cnaui=…; token=…; x-ap=…（token 从 token= 里取）
/// - 纯 token 字符串（只发 Bearer；上游可能回 Bad_Request）
fn parse_cred(secret: &str) -> Result<(String, String), String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("Qwen AI 凭据为空：请在 chat.qwen.ai 登录后，从 Local Storage 复制 token 并连同 Cookie 一起粘贴".into());
    }
    if s.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(s) {
            let token = v
                .get("token")
                .or_else(|| v.get("accessToken"))
                .or_else(|| v.get("access_token"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let cookies = v
                .get("cookies")
                .or_else(|| v.get("cookie"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if token.is_empty() {
                return Err("Qwen AI 凭据 JSON 缺少 token 字段".into());
            }
            return Ok((token, cookies));
        }
    }
    if s.contains('=') && s.contains(';') {
        let mut token = String::new();
        for part in s.split(';') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix("token=") {
                token = rest.trim().to_string();
            }
        }
        if token.is_empty() {
            return Err("Qwen AI Cookie 里没有 token 字段：请确认复制了 chat.qwen.ai 的完整 Cookie".into());
        }
        return Ok((token, s.to_string()));
    }
    Ok((s.to_string(), String::new()))
}

/// 模型别名（对齐 Chat2API qwen-ai.ts 的 MODEL_ALIASES）。
///
/// 后缀约定：`-thinking` / `-fast` 是「开关」而不是模型名的一部分，
/// 必须先剥掉再查别名，否则会把非法 id 发给上游。
fn map_model(model: &str) -> String {
    let base = model
        .strip_suffix("-thinking")
        .or_else(|| model.strip_suffix("-fast"))
        .unwrap_or(model);
    let lower = base.to_lowercase();
    match lower.as_str() {
        "qwen" | "qwen3" | "qwen3.7" | "qwen3-7" => return "qwen3.7-max".to_string(),
        "qwen3.6" | "qwen3-6" => return "qwen3.6-plus".to_string(),
        "qwen3.6-35b" => return "qwen3.6-35b-a3b".to_string(),
        "qwen3.6-27b" => return "qwen3.6-27b".to_string(),
        "qwen3-coder" => return "qwen3-coder-plus".to_string(),
        _ => {}
    }
    base.to_string()
}

/// 请求头：对齐 Chat2API DEFAULT_HEADERS + getHeaders（含浏览器指纹与反爬头）。
fn headers(spec: &ProviderSpec, token: &str, cookies: &str, chat_id: Option<&str>) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("Accept".into(), "application/json".into()),
        ("Accept-Language".into(), "zh-CN,zh;q=0.9".into()),
        ("Content-Type".into(), "application/json".into()),
        ("source".into(), "web".into()),
        ("User-Agent".into(), QWEN_UA.into()),
        ("sec-ch-ua".into(), r#""Not:A-Brand";v="99", "Google Chrome";v="145", "Chromium";v="145""#.into()),
        ("sec-ch-ua-mobile".into(), "?0".into()),
        ("sec-ch-ua-platform".into(), r#""macOS""#.into()),
        ("Sec-Fetch-Dest".into(), "empty".into()),
        ("Sec-Fetch-Mode".into(), "cors".into()),
        ("Sec-Fetch-Site".into(), "same-origin".into()),
        ("bx-v".into(), QWEN_BX_V.into()),
        ("Version".into(), QWEN_VERSION.into()),
        ("Timezone".into(), "Mon Feb 23 2026 22:06:02 GMT+0800".into()),
        ("Origin".into(), "https://chat.qwen.ai".into()),
        ("X-Request-Id".into(), uuid_v4ish()),
        ("Authorization".into(), format!("Bearer {token}")),
    ];
    if !cookies.is_empty() {
        hs.push(("Cookie".into(), cookies.to_string()));
    }
    if let Some(id) = chat_id {
        if !id.is_empty() {
            hs.push(("Referer".into(), format!("https://chat.qwen.ai/c/{id}")));
        }
    }
    for (k, v) in &spec.headers {
        hs.retain(|(hk, _)| !hk.eq_ignore_ascii_case(k));
        hs.push((k.clone(), v.clone()));
    }
    hs
}

fn apply(rb: reqwest::RequestBuilder, hs: &[(String, String)]) -> reqwest::RequestBuilder {
    let mut rb = rb;
    for (k, v) in hs {
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb
}

/// 请求体里的 messages → (system 文本, user 文本)：网页协议一次只收一条 user。
fn flatten_messages(v: &Value) -> (String, String) {
    let mut system = String::new();
    let mut user = String::new();
    if let Some(arr) = v.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
            let text = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            match role {
                "system" => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&text);
                }
                "assistant" => {
                    if !user.is_empty() {
                        user.push_str("\n\n");
                    }
                    user.push_str(&format!("Assistant: {text}"));
                }
                _ => {
                    if !user.is_empty() {
                        user.push_str("\n\n");
                    }
                    user.push_str(&text);
                }
            }
        }
    }
    (system, user)
}

/// 第一步：建会话，返回 chat_id。
async fn create_chat(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    hs: &[(String, String)],
    model: &str,
) -> Result<String, RelayError> {
    let url = format!("{}/api/v2/chats/new", spec.base_url.trim_end_matches('/'));
    let payload = json!({
        "title": "OpenAI_API_Chat",
        "models": [model],
        "chat_mode": "normal",
        "chat_type": "t2t",
        "timestamp": secs() * 1000,
        "project_id": "",
    });
    let resp = apply(http.post(&url), hs)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(er(
            status,
            text.chars().take(400).collect::<String>(),
            ErrorClass::from_status(status),
        ));
    }
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    if let Some(id) = v.get("data").and_then(|d| d.get("id")).and_then(|x| x.as_str()) {
        if !id.is_empty() {
            return Ok(id.to_string());
        }
    }
    Err(er(
        401,
        format!(
            "Qwen AI 开会话失败（token/Cookie 可能失效）：{}",
            text.chars().take(300).collect::<String>()
        ),
        ErrorClass::Auth,
    ))
}

/// 第二步的 envelope。
fn completions_payload(model: &str, chat_id: &str, system: &str, user: &str, thinking: bool) -> Value {
    let mut text = user.to_string();
    if !system.is_empty() {
        text = format!("{system}\n\nUser: {user}");
    }
    let fid = uuid_v4ish();
    let child = uuid_v4ish();
    let ts = secs();
    json!({
        "stream": true,
        "version": "2.1",
        "incremental_output": true,
        "chat_id": chat_id,
        "chat_mode": "normal",
        "model": model,
        "parent_id": Value::Null,
        "messages": [{
            "fid": fid,
            "parentId": Value::Null,
            "childrenIds": [child],
            "role": "user",
            "content": text,
            "user_action": "chat",
            "files": [],
            "timestamp": ts,
            "models": [model],
            "chat_type": "t2t",
            "feature_config": {
                "thinking_enabled": thinking,
                "output_schema": "phase",
                "research_mode": "normal",
                "auto_thinking": thinking,
                "thinking_format": "summary",
                "auto_search": false,
            },
            "extra": {"meta": {"subChatType": "t2t"}},
            "sub_chat_type": "t2t",
            "parent_id": Value::Null,
        }],
        "timestamp": ts + 1,
    })
}

async fn post_completions(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    hs: &[(String, String)],
    payload: Value,
    chat_id: &str,
) -> Result<reqwest::Response, RelayError> {
    let url = format!(
        "{}/api/v2/chat/completions?chat_id={}",
        spec.base_url.trim_end_matches('/'),
        chat_id
    );
    let resp = apply(http.post(&url), hs)
        .header("x-accel-buffering", "no")
        .json(&payload)
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let txt = resp.text().await.unwrap_or_default();
        return Err(er(
            status,
            txt.chars().take(400).collect::<String>(),
            ErrorClass::from_status(status),
        ));
    }
    Ok(resp)
}

/// 从一帧 SSE data JSON 取 (正文, 思考/摘要, 结束原因, 是否结束, 错误)。
fn frame_delta(v: &Value) -> (String, String, Option<String>, bool, Option<String>) {
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|x| x.as_str())
            .or_else(|| err.as_str())
            .unwrap_or("Qwen AI 上游返回错误")
            .to_string();
        return (String::new(), String::new(), None, false, Some(msg));
    }
    if v.get("success").and_then(|x| x.as_bool()) == Some(false) {
        let code = v.get("code").and_then(|x| x.as_str()).unwrap_or("Bad_Request");
        return (
            String::new(),
            String::new(),
            None,
            false,
            Some(format!(
                "Qwen AI 上游拒绝请求：{code}（常见原因：Cookie 缺失或登录态失效）"
            )),
        );
    }
    let Some(choice) = v.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) else {
        return (String::new(), String::new(), None, false, None);
    };
    let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
    let phase = delta.get("phase").and_then(|x| x.as_str()).unwrap_or("");
    let status = delta.get("status").and_then(|x| x.as_str()).unwrap_or("");
    let content = delta.get("content").and_then(|x| x.as_str()).unwrap_or("");
    let finish = choice
        .get("finish_reason")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let done = status == "finished" && (phase == "answer" || phase.is_empty());
    if phase == "think" {
        if status == "finished" {
            return (String::new(), String::new(), finish, done, None);
        }
        return (String::new(), content.to_string(), finish, done, None);
    }
    if phase == "thinking_summary" {
        let joined = delta
            .get("extra")
            .and_then(|e| e.get("summary_thought"))
            .and_then(|s| s.get("content"))
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        return (String::new(), joined, finish, done, None);
    }
    (content.to_string(), String::new(), finish, done, None)
}

fn parse_data_line(block: &str) -> Option<&str> {
    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            let d = rest.trim();
            if !d.is_empty() {
                return Some(d);
            }
        }
    }
    None
}

/// 从请求体解析出 (上游模型 id, 是否开思考)。
fn read_request(v: &Value) -> (String, bool) {
    let requested = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("qwen3.7-max")
        .to_string();
    let lower = requested.to_lowercase();
    let thinking = lower.ends_with("-thinking")
        || lower.contains("think")
        || lower.contains("r1")
        || v.get("enable_thinking").and_then(|x| x.as_bool()) == Some(true);
    (map_model(&requested), thinking)
}

/// 非流式：网页端点恒为流式，这里聚合成一个 OpenAI 响应。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let (token, cookies) = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let (model, thinking) = read_request(&v);
    let (system, user) = flatten_messages(&v);
    let hs = headers(spec, &token, &cookies, None);
    let chat_id = create_chat(http, spec, &hs, &model).await?;
    let hs2 = headers(spec, &token, &cookies, Some(&chat_id));
    let payload = completions_payload(&model, &chat_id, &system, &user, thinking);
    let resp = post_completions(http, spec, &hs2, payload, &chat_id).await?;

    let mut text = String::new();
    let mut reason = String::new();
    let mut summary = String::new();
    let mut finish = "stop".to_string();
    let mut failure: Option<String> = None;
    let mut buf = String::new();
    let mut dec = crate::Utf8Decoder::new();
    let mut stream = resp.bytes_stream();
    'outer: while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| er(0, format!("上游流中：{e}"), ErrorClass::Transient))?;
        buf.push_str(&dec.push(&chunk));
        while let Some((pos, sep)) = crate::find_frame_end(&buf) {
            let block: String = buf.drain(..pos + sep).collect();
            let Some(data) = parse_data_line(&block) else { continue };
            if data == "[DONE]" {
                break 'outer;
            }
            let Ok(fv) = serde_json::from_str::<Value>(data) else { continue };
            let (t, th, fin, done, err) = frame_delta(&fv);
            if let Some(e) = err {
                failure = Some(e);
                break 'outer;
            }
            if !th.is_empty() {
                if th.len() > summary.len() && th.starts_with(&summary) {
                    reason.push_str(&th[summary.len()..]);
                    summary = th;
                } else {
                    reason.push_str(&th);
                }
            }
            text.push_str(&t);
            if let Some(f) = fin {
                finish = f;
            }
            if done {
                break 'outer;
            }
        }
    }
    if let Some(e) = failure {
        return Err(er(401, e, ErrorClass::Auth));
    }
    if text.trim().is_empty() {
        return Err(er(
            401,
            "Qwen AI 未返回正文（token/Cookie 可能失效，请在 chat.qwen.ai 重新登录后复制 token 与完整 Cookie）",
            ErrorClass::Auth,
        ));
    }
    let mut msg = json!({"role": "assistant", "content": text});
    if !reason.trim().is_empty() {
        msg["reasoning_content"] = json!(reason.trim());
    }
    let tin = (user.chars().count() / 2).max(1) as i64;
    let tout = (text.chars().count() / 2).max(1) as i64;
    Ok(json!({
        "id": format!("chatcmpl-{}", uuid_v4ish()),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{"index": 0, "message": msg, "finish_reason": finish}],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout},
    }))
}

/// 流式：Qwen SSE → OpenAI chunk 帧。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<
    (
        std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
        mpsc::Receiver<StreamStat>,
    ),
    RelayError,
> {
    let (token, cookies) = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let (model, thinking) = read_request(&v);
    let (system, user) = flatten_messages(&v);
    let hs = headers(spec, &token, &cookies, None);
    let chat_id = create_chat(http, spec, &hs, &model).await?;
    let hs2 = headers(spec, &token, &cookies, Some(&chat_id));
    let payload = completions_payload(&model, &chat_id, &system, &user, thinking);
    let resp = post_completions(http, spec, &hs2, payload, &chat_id).await?;

    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let in_bytes = body.len();
    tokio::spawn(async move {
        let rid = format!("chatcmpl-{}", uuid_v4ish());
        let created = secs();
        let model_out = model.clone();
        let mk = move |delta: Value, finish: Option<&str>| -> String {
            format!(
                "data: {}\n\n",
                json!({
                    "id": rid,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model_out,
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
                })
            )
        };
        let mut stat = StreamStat::default();
        let mut out_chars: usize = 0;
        let mut buf = String::new();
        let mut dec = crate::Utf8Decoder::new();
        let mut sent_role = false;
        let mut summary = String::new();
        let mut done_sent = false;
        let mut stream = resp.bytes_stream();
        'outer: while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(b) => b,
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {e}");
                    break;
                }
            };
            buf.push_str(&dec.push(&chunk));
            let mut out = String::new();
            while let Some((pos, sep)) = crate::find_frame_end(&buf) {
                let block: String = buf.drain(..pos + sep).collect();
                let Some(data) = parse_data_line(&block) else { continue };
                if data == "[DONE]" {
                    break;
                }
                let Ok(fv) = serde_json::from_str::<Value>(data) else { continue };
                let (t, th, fin, done, err) = frame_delta(&fv);
                if let Some(e) = err {
                    stat.error = Some(ErrorClass::Auth);
                    stat.error_msg = e;
                    break 'outer;
                }
                let mut piece = th;
                if !piece.is_empty() && piece.len() > summary.len() && piece.starts_with(&summary) {
                    piece = piece[summary.len()..].to_string();
                    summary.push_str(&piece);
                }
                if (!t.is_empty() || !piece.is_empty()) && !sent_role {
                    out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                    sent_role = true;
                }
                if !piece.is_empty() {
                    out_chars += piece.chars().count();
                    out.push_str(&mk(json!({"reasoning_content": piece}), None));
                }
                if !t.is_empty() {
                    out_chars += t.chars().count();
                    out.push_str(&mk(json!({"content": t}), None));
                }
                if done && !done_sent {
                    let fr = fin.unwrap_or_else(|| "stop".to_string());
                    out.push_str(&mk(json!({}), Some(&fr)));
                    stat.finish_reason = Some(fr);
                    done_sent = true;
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break 'outer;
            }
        }
        let mut tail = String::new();
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "Qwen AI 未返回正文（token/Cookie 可能失效，请在 chat.qwen.ai 重新登录后复制 token 与完整 Cookie）".into();
        }
        if let Some(ec) = stat.error {
            let etype = match ec {
                ErrorClass::Rate => "rate_limit_error",
                ErrorClass::Auth => "authentication_error",
                ErrorClass::Quota => "insufficient_quota",
                _ => "api_error",
            };
            let msg = stat.error_msg.replace('"', "'");
            tail.push_str(&format!(
                "data: {}\n\n",
                json!({"error": {"message": msg, "type": etype}})
            ));
        } else if !done_sent {
            tail.push_str(&mk(json!({}), Some("stop")));
            stat.finish_reason = Some("stop".into());
        }
        tail.push_str("data: [DONE]\n\n");
        let _ = bytes_tx.send(Ok(Bytes::from(tail))).await;
        let (tin, tout) = crate::estimate_stream_tokens(in_bytes, out_chars);
        stat.tokens_in = tin;
        stat.tokens_out = tout;
        let _ = tx.send(stat).await;
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    Ok((Box::pin(stream), rx))
}

/// 验活：GET /api/v2/user，2xx 且带用户信息即有效。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok((token, cookies)) = parse_cred(secret) else {
        return false;
    };
    let url = format!("{}/api/v2/user", spec.base_url.trim_end_matches('/'));
    let hs = headers(spec, &token, &cookies, None);
    match apply(http.get(&url), &hs)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => {
            if !r.status().is_success() {
                return false;
            }
            let txt = r.text().await.unwrap_or_default();
            let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
            if v.get("success").and_then(|x| x.as_bool()) == Some(false) {
                return false;
            }
            v.get("data").is_some() || v.get("email").is_some() || v.get("id").is_some()
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get, post};
    use axum::Router;
    use futures_util::StreamExt;
    use std::collections::BTreeMap;

    const SSE: &str = "data: {\"choices\":[{\"delta\":{\"phase\":\"think\",\"status\":\"typing\",\"content\":\"\u{601d}\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"phase\":\"answer\",\"status\":\"typing\",\"content\":\"\u{4f60}\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"phase\":\"answer\",\"status\":\"typing\",\"content\":\"\u{597d}\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"phase\":\"answer\",\"status\":\"finished\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

    async fn mock() -> String {
        let app = Router::new()
            .route(
                "/api/v2/chats/new",
                post(|| async { axum::Json(json!({"success": true, "data": {"id": "chat-1"}})) }),
            )
            .route(
                "/api/v2/chat/completions",
                post(|| async {
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from(SSE))
                        .unwrap()
                }),
            )
            .route(
                "/api/v2/user",
                get(|| async { axum::Json(json!({"success": true, "data": {"id": "u-1"}})) }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    fn spec_for(base: &str) -> ProviderSpec {
        ProviderSpec {
            id: "web-qwen".into(),
            name: "Qwen AI".into(),
            enabled: true,
            auth: aigw_registry::AuthKind::Cookie,
            login_methods: Vec::new(),
            base_url: base.to_string(),
            format: "openai".into(),
            models: vec!["qwen3.7-max".into()],
            models_policy: aigw_registry::ModelsPolicy::Static,
            stream_dialect: aigw_registry::StreamDialect::SseDelta,
            headers: BTreeMap::new(),
            chat_path: String::new(),
            models_path: String::new(),
            models_filter: String::new(),
            upstream_kind: "qwen-web".into(),
            signer: String::new(),
            biz_err: vec![],
            hosts: vec![],
            login_url: String::new(),
            notes: String::new(),
            chat_supported: true,
            region: "global".into(),
        }
    }

    fn req_body(model: &str) -> Vec<u8> {
        json!({
            "model": model,
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hello"}
            ]
        })
        .to_string()
        .into_bytes()
    }

    #[tokio::test]
    async fn qwen_nonstream_via_mock() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let secret = "cnaui=a; token=tok-1; x-ap=b";
        let v = chat_nonstream(&http, &spec, secret, &req_body("qwen3.7-max"))
            .await
            .unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "\u{4f60}\u{597d}");
        assert_eq!(v["choices"][0]["message"]["reasoning_content"], "\u{601d}");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["model"], "qwen3.7-max");
    }

    #[tokio::test]
    async fn qwen_stream_via_mock() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let (mut s, mut rx) = chat_stream(&http, &spec, "tok-1", &req_body("qwen")).await.unwrap();
        let mut out = String::new();
        while let Some(item) = s.next().await {
            out.push_str(&String::from_utf8_lossy(&item.unwrap()));
        }
        assert!(out.contains("\u{4f60}"), "正文应透出: {out}");
        assert!(out.contains("reasoning_content"), "思考应映射成 reasoning_content: {out}");
        assert!(out.contains("\"stop\""), "应有 finish_reason: {out}");
        assert!(out.trim_end().ends_with("[DONE]"), "应以 [DONE] 收尾: {out}");
        let stat = rx.try_recv().expect("应回传统计");
        assert_eq!(stat.finish_reason.as_deref(), Some("stop"));
        assert!(stat.error.is_none(), "不应有错误: {:?}", stat.error_msg);
    }

    #[tokio::test]
    async fn qwen_verify_and_bad_cred() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        assert!(verify(&http, &spec, "tok-1").await);
        assert!(parse_cred("   ").is_err());
        assert!(parse_cred("a=1; b=2").is_err(), "没有 token 的 Cookie 应报错");
        let (t, c) = parse_cred("a=1; token=tok-9; b=2").unwrap();
        assert_eq!(t, "tok-9");
        assert!(c.contains("a=1"));
        let (t2, c2) = parse_cred("{\"token\":\"tok-json\",\"cookies\":\"k=v\"}").unwrap();
        assert_eq!(t2, "tok-json");
        assert_eq!(c2, "k=v");
    }

    #[test]
    fn qwen_model_alias_and_flatten() {
        assert_eq!(map_model("qwen"), "qwen3.7-max");
        assert_eq!(map_model("Qwen3.6"), "qwen3.6-plus");
        assert_eq!(map_model("qwen3-coder"), "qwen3-coder-plus");
        assert_eq!(map_model("qwen3.6-27b"), "qwen3.6-27b");
        let v = json!({"messages":[{"role":"system","content":"s1"},{"role":"user","content":"u1"}]});
        let (s, u) = flatten_messages(&v);
        assert_eq!(s, "s1");
        assert_eq!(u, "u1");
        let (m, thinking) = read_request(&json!({"model": "qwen3.7-max-thinking"}));
        assert_eq!(m, "qwen3.7-max");
        assert!(thinking);
    }
}
