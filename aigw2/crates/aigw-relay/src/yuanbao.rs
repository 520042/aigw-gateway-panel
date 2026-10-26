//! 元宝（yuanbao.tencent.com）adapter——纯 HTTP 多步私有协议，不依赖无头浏览器。
//!
//! 一手来源：aigw-panel-rs/src/relay/yuanbao.rs（移植自 juzeon/yuanbao-chat2api）。
//! 真实链路（非 OpenAI 透明）：
//! 1. `POST {base}/api/user/agent/conversation/create` body `{"agentId":"naQivTmsDa"}` → 取 `id`；
//! 2. `POST {base}/api/chat/{conversation_id}`，body 含 chatModelId（deep_seek=R1 /
//!    deep_seek_v3=V3）、prompt（多轮拼成单 prompt）；Cookie 需含 hy_user/hy_token；
//! 3. 响应为 SSE（`event: message`，data 内 type=think/text），**只支持流式**。
//!
//! 凭据：secret 为 yuanbao.tencent.com 的登录 Cookie，必须含 `hy_user` 与 `hy_token`。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const AGENT_ID: &str = "naQivTmsDa";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

fn er(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError {
        status,
        body: body.into(),
        class,
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 从 Cookie 串解析 hy_user / hy_token。
fn parse_cookie(secret: &str) -> Result<(String, String), String> {
    let mut hy_user = String::new();
    let mut hy_token = String::new();
    for part in secret.split(';') {
        let p = part.trim();
        if let Some(v) = p.strip_prefix("hy_user=") {
            hy_user = v.to_string();
        } else if let Some(v) = p.strip_prefix("hy_token=") {
            hy_token = v.to_string();
        }
    }
    if hy_user.is_empty() || hy_token.is_empty() {
        return Err("元宝凭据需含 hy_user / hy_token（登录 yuanbao.tencent.com 后从浏览器 Cookie 复制）".into());
    }
    Ok((hy_user, hy_token))
}

/// 模型 → 元宝 chatModelId。
fn chat_model_id(model: &str) -> &'static str {
    if model.contains("r1") {
        "deep_seek"
    } else {
        "deep_seek_v3"
    }
}

fn build_prompt(messages: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|p| {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .or_else(|| p.as_str())
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        if content.trim().is_empty() {
            continue;
        }
        if parts.is_empty() {
            parts.push(content.trim().to_string());
        } else {
            parts.push(format!("#[{}]\n{}\n\n", role.trim(), content.trim()));
        }
    }
    parts.concat()
}

fn headers(hy_user: &str, hy_token: &str) -> Vec<(String, String)> {
    vec![
        (
            "cookie".into(),
            format!("hy_source=web; hy_user={}; hy_token={}", hy_user, hy_token),
        ),
        ("origin".into(), "https://yuanbao.tencent.com".into()),
        ("referer".into(), format!("https://yuanbao.tencent.com/chat/{}", AGENT_ID)),
        ("x-agentid".into(), AGENT_ID.to_string()),
        ("user-agent".into(), UA.to_string()),
    ]
}

fn chat_body(prompt: &str, model: &str) -> Value {
    json!({
        "model": "gpt_175B_0404",
        "prompt": prompt,
        "plugin": "Adaptive",
        "displayPrompt": prompt,
        "displayPromptType": 1,
        "options": {"imageIntention": {"needIntentionModel": true, "backendUpdateFlag": 2, "intentionStatus": true}},
        "multimedia": [],
        "agentId": AGENT_ID,
        "supportHint": 1,
        "version": "v2",
        "chatModelId": chat_model_id(model),
    })
}

/// 解析一帧元宝 SSE：`event: message` + `data: {...}`。
fn parse_sse_frame(buf: &str) -> Option<(String, Value)> {
    let mut ev = String::new();
    let mut data = String::new();
    for line in buf.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("event:") {
            ev = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data = rest.trim().to_string();
        }
    }
    if data.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&data).ok()?;
    let et = if ev.is_empty() {
        v.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string()
    } else {
        ev
    };
    Some((et, v))
}

struct Open {
    url: String,
    body: Value,
    headers: Vec<(String, String)>,
}

/// 建会话 + 组装对话请求。
async fn open(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    messages: &[Value],
    model: &str,
) -> Result<Open, RelayError> {
    let (hy_user, hy_token) = parse_cookie(secret)
        .map_err(|e| er(401, e, ErrorClass::Auth))?;
    let create_url = format!("{}/api/user/agent/conversation/create", spec.base_url);
    let create_resp = http
        .post(&create_url)
        .timeout(std::time::Duration::from_secs(20))
        .header("content-type", "application/json")
        .header(
            "cookie",
            format!("hy_source=web; hy_user={}; hy_token={}", hy_user, hy_token),
        )
        .header("origin", "https://yuanbao.tencent.com")
        .header("user-agent", UA)
        .json(&json!({"agentId": AGENT_ID}))
        .send()
        .await
        .map_err(|e| er(0, format!("建会话网络错误：{e}"), ErrorClass::Transient))?;
    let status = create_resp.status().as_u16();
    if !(200..300).contains(&status) {
        let t = create_resp.text().await.unwrap_or_default();
        return Err(er(
            status,
            format!("元宝建会话失败：{}", t.chars().take(300).collect::<String>()),
            ErrorClass::from_status(status),
        ));
    }
    let create_v: Value = create_resp.json().await.unwrap_or(json!({}));
    let conv_id = create_v.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if conv_id.is_empty() {
        return Err(er(
            status,
            format!("元宝建会话响应缺少 id（Cookie 可能失效）：{create_v}"),
            ErrorClass::Auth,
        ));
    }
    let prompt = build_prompt(messages);
    Ok(Open {
        url: format!("{}/api/chat/{}", spec.base_url, conv_id),
        body: chat_body(&prompt, model),
        headers: headers(&hy_user, &hy_token),
    })
}

/// 非流式：内部强制流式聚合。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    if secret.is_empty() {
        return Err(er(401, "缺少元宝登录 Cookie", ErrorClass::Auth));
    }
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = openai.get("model").and_then(|m| m.as_str()).unwrap_or("yuanbao-deepseek").to_string();
    let messages = openai.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let o = open(http, spec, secret, &messages, &model).await?;

    let mut rb = http.post(&o.url).header("content-type", "application/json");
    for (k, v) in &o.headers {
        rb = rb.header(k, v);
    }
    let resp = rb
        .body(o.body.to_string())
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let mut buf = String::new();
    let mut text = String::new();
    let mut dec = crate::Utf8Decoder::new();
    let mut thinking = String::new();
    let mut finish = "stop".to_string();
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        let b = item.map_err(|e| er(0, format!("上游流中断：{e}"), ErrorClass::Transient))?;
        {
            let text = dec.push(&b);
            buf.push_str(&text);
        }
        while let Some((pos, sep)) = crate::find_frame_end(&buf) {
            let frame: String = buf.drain(..pos + sep).collect();
            if let Some((et, v)) = parse_sse_frame(&frame) {
                if et == "message" {
                    match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                        "think" => {
                            if let Some(c) = v.get("content").and_then(|x| x.as_str()) {
                                thinking.push_str(c);
                            }
                        }
                        "text" => {
                            if let Some(c) = v.get("msg").and_then(|x| x.as_str()) {
                                text.push_str(c);
                            }
                        }
                        _ => {
                            if let Some(s) = v.get("stopReason").and_then(|x| x.as_str()) {
                                if !s.is_empty() {
                                    finish = s.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let full = text.trim().to_string();
    if full.is_empty() && thinking.trim().is_empty() {
        return Err(er(
            status,
            "元宝上游未返回内容（Cookie 可能失效或模型不支持）",
            ErrorClass::Auth,
        ));
    }
    let mut msg = json!({"role": "assistant", "content": full});
    if !thinking.trim().is_empty() {
        msg["reasoning_content"] = json!(thinking.trim());
    }
    let in_tok: usize = messages
        .iter()
        .map(|m| {
            m.get("content")
                .map(|c| match c {
                    Value::String(s) => s.len(),
                    other => other.to_string().len(),
                })
                .unwrap_or(0)
        })
        .sum();
    Ok(json!({
        "id": format!("chatcmpl-{:024x}", nanos()),
        "object": "chat.completion",
        "created": (nanos() / 1_000_000_000) as u64,
        "model": model,
        "choices": [{"index": 0, "message": msg, "finish_reason": finish}],
        "usage": {
            "prompt_tokens": in_tok.max(1),
            "completion_tokens": ((full.len() + thinking.len()) / 3).max(1),
            "total_tokens": (in_tok + (full.len() + thinking.len()) / 3).max(1),
        },
    }))
}

/// 流式：元宝 SSE → OpenAI SSE chunk 帧。
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
    if secret.is_empty() {
        return Err(er(401, "缺少元宝登录 Cookie", ErrorClass::Auth));
    }
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = openai.get("model").and_then(|m| m.as_str()).unwrap_or("yuanbao-deepseek").to_string();
    let messages = openai.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let o = open(http, spec, secret, &messages, &model).await?;

    let mut rb = http.post(&o.url).header("content-type", "application/json");
    for (k, v) in &o.headers {
        rb = rb.header(k, v);
    }
    let resp = rb
        .timeout(crate::NONSTREAM_TOTAL)
        .body(o.body.to_string())
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        return Err(er(status, t.chars().take(400).collect::<String>(), ErrorClass::from_status(status)));
    }

    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let model_owned = model.clone();
    let in_bytes = body.len();
    tokio::spawn(async move {
        let chat_id = format!("chatcmpl-{:024x}", nanos());
        let mk = |delta: Value, finish: Option<&str>| -> String {
            format!(
                "data: {}\n\n",
                json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": (nanos() / 1_000_000_000) as u64,
                    "model": model_owned,
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
                })
            )
        };
        let mut stat = StreamStat::default();
        let mut out_chars: usize = 0;
        let mut buf = String::new();
        let mut dec = crate::Utf8Decoder::new();
        let mut sent_role = false;
        let mut sent_thinking = false;
        let mut got_content = false;
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
            let text = dec.push(&chunk);
            buf.push_str(&text);
            let mut out = String::new();
            while let Some((pos, sep)) = crate::find_frame_end(&buf) {
                let frame: String = buf.drain(..pos + sep).collect();
                if let Some((et, v)) = parse_sse_frame(&frame) {
                    if et == "message" {
                        match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                            "think" => {
                                if let Some(c) = v.get("content").and_then(|x| x.as_str()) {
                                    if !c.is_empty() {
                                        got_content = true;
                                    }
                                    if !sent_role {
                                        out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                                        sent_role = true;
                                    }
                                    out_chars += c.chars().count();
                                    if !sent_thinking {
                                        out.push_str(&mk(json!({"reasoning_content": c}), None));
                                        sent_thinking = true;
                                    } else {
                                        out.push_str(&mk(json!({"reasoning_content": c}), None));
                                    }
                                }
                            }
                            "text" => {
                                if let Some(c) = v.get("msg").and_then(|x| x.as_str()) {
                                    if !c.is_empty() {
                                        got_content = true;
                                    }
                                    if !sent_role {
                                        out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                                        sent_role = true;
                                    }
                                    out_chars += c.chars().count();
                                    out.push_str(&mk(json!({"content": c}), None));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break 'outer;
            }
        }
        let mut tail = String::new();
        // 无内容兜底：建会话成功但流里无正文（Cookie 失效但建会话未报错 / 模型不支持）
        if stat.error.is_none() && !got_content {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "元宝未返回正文（Cookie 可能已失效，请重新登录 yuanbao.tencent.com 粘贴含 hy_user/hy_token 的整段 Cookie；或该模型暂不支持）".into();
        }
        if let Some(ec) = stat.error {
            // HTTP 头已 200（元宝 SSE 建连即 200），用 OpenAI 流内 error 帧透传，避免空响应
            let etype = match ec {
                ErrorClass::Rate => "rate_limit_error",
                ErrorClass::Auth => "authentication_error",
                ErrorClass::Quota => "insufficient_quota",
                _ => "api_error",
            };
            let msg = stat.error_msg.replace('"', "'");
            tail.push_str(&format!("data: {}\n\n", json!({ "error": { "message": msg, "type": etype } })));
        } else {
            tail.push_str(&mk(json!({}), Some("stop")));
            stat.finish_reason = Some("stop".into());
            stat.saw_done = true;
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

/// 验活：建会话能返回 id 即 Cookie 有效（最小代价）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok((hy_user, hy_token)) = parse_cookie(secret) else {
        return false;
    };
    let url = format!("{}/api/user/agent/conversation/create", spec.base_url);
    match http
        .post(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("content-type", "application/json")
        .header("cookie", format!("hy_source=web; hy_user={}; hy_token={}", hy_user, hy_token))
        .header("user-agent", UA)
        .json(&json!({"agentId": AGENT_ID}))
        .send()
        .await
    {
        Ok(r) => {
            let v: Value = r.json().await.unwrap_or(json!({}));
            v.get("id").and_then(|x| x.as_str()).is_some_and(|s| !s.is_empty())
        }
        Err(_) => false,
    }
}
