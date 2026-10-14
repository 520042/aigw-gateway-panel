//! 元宝（腾讯 YuanBao）原生中继——多步私有协议移植自 `juzeon/yuanbao-chat2api`（Rust/Axum 同栈）。
//!
//! 真实链路（非 OpenAI 透明，社区逆向已验证）：
//! 1. `POST https://yuanbao.tencent.com/api/user/agent/conversation/create`
//!    body `{"agentId": <agent_id>}` → 取 `id` 作为 conversation_id
//! 2. `POST https://yuanbao.tencent.com/api/chat/{conversation_id}`
//!    body 含 `chatModelId`（deep_seek=DeepSeek R1 / deep_seek_v3=DeepSeek V3）、
//!    `prompt`（多轮消息拼成单 prompt）、Cookie=`hy_source=web; hy_user=...; hy_token=...`
//! 3. 响应为 SSE（`event: message`，data 内 `type:think/text`），**只支持流式**。
//!
//! 凭据：账号池 `apk-yuanbao` 的 secret 为浏览器 Cookie 串；从中解析 `hy_user`/`hy_token`。
//! agent_id 固定用元宝网页公开 agent `naQivTmsDa`（与 bundled_yuanbao.ENDPOINTS 的
//! `/chat/naQivTmsDa` 一致；也是各 free-api 项目的默认 agent）。
use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode},
    response::Response,
};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{json, Value};

const CREATE_URL: &str = "https://yuanbao.tencent.com/api/user/agent/conversation/create";
const CHAT_URL: &str = "https://yuanbao.tencent.com/api/chat/{}";
const AGENT_ID: &str = "naQivTmsDa";
const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

/// 从 Cookie 串里解析 hy_user / hy_token（缺哪个都如实报错）。
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
        return Err("元宝凭据需含 hy_user / hy_token（从 yuanbao.tencent.com 浏览器 Cookie 复制）".to_string());
    }
    Ok((hy_user, hy_token))
}

/// 模型 → 元宝 chatModelId（deepseek-r1 → deep_seek，其余归 deep_seek_v3）。
fn chat_model_id(model: &str) -> String {
    if model.contains("r1") {
        "deep_seek".to_string()
    } else if model.contains("v3") || model.contains("deepseek") {
        "deep_seek_v3".to_string()
    } else if model.contains("deep_seek") {
        model.to_string()
    } else {
        "deep_seek_v3".to_string()
    }
}

/// 多轮消息拼成元宝单 prompt（对齐 juzeon ChatMessages Display）。
fn build_prompt(messages: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
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
        (
            "referer".into(),
            format!("https://yuanbao.tencent.com/chat/{}", AGENT_ID),
        ),
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

/// 解析元宝 SSE 帧：`event: message` 紧跟 `data: {...}`；返回 (event_type, data_json)。
/// 非 message 事件（如没有 event: 前缀的兜底）也尝试解析 data。
fn parse_sse_frame(buf: &str) -> Option<(String, Value)> {
    let mut ev = String::new();
    let mut data = String::new();
    for line in buf.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
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

/// 发起「建会话 + 流式对话」，把上游 SSE 转成统一事件流给调用方消费。
/// 返回 (conversation_id, 上游 bytes stream 句柄) 不便直接暴露，这里直接在 chat/chat_stream 内消费。
async fn open_stream(
    secret: &str,
    messages: &[Value],
    model: &str,
    timeout: u64,
) -> Result<(String, String, Vec<(String, String)>, Value), (i64, String)> {
    let (hy_user, hy_token) = match parse_cookie(secret) {
        Ok(v) => v,
        Err(e) => return Err((0, e)),
    };
    let client = reqwest::Client::new();
    // 1) 建会话
    let create_resp = client
        .post(CREATE_URL)
        .timeout(std::time::Duration::from_secs(20))
        .header("content-type", "application/json")
        .header("cookie", format!("hy_source=web; hy_user={}; hy_token={}", hy_user, hy_token))
        .header("origin", "https://yuanbao.tencent.com")
        .header("user-agent", UA)
        .json(&json!({"agentId": AGENT_ID}))
        .send()
        .await
        .map_err(|e| (0, e.to_string()))?;
    let status = create_resp.status().as_u16() as i64;
    if !create_resp.status().is_success() {
        let t = create_resp.text().await.unwrap_or_default();
        return Err((status, format!("建会话失败：{}", t.chars().take(300).collect::<String>())));
    }
    let create_v: Value = create_resp.json().await.map_err(|e| (0, e.to_string()))?;
    let conv_id = create_v
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if conv_id.is_empty() {
        return Err((0, format!("建会话响应缺少 id：{}", create_v)));
    }
    // 2) 对话
    let prompt = build_prompt(messages);
    let body = chat_body(&prompt, model);
    let url = CHAT_URL.replace("{}", &conv_id);
    let hdrs = headers(&hy_user, &hy_token);
    Ok((url, conv_id, hdrs, body))
}

/// 非流式：内部强制流式后聚合为单个 OpenAI chat.completion。
pub async fn chat(
    messages: &[Value],
    secret: &str,
    model: &str,
    timeout: u64,
) -> Result<Value, (i64, String)> {
    let (url, _conv_id, hdrs, body) = open_stream(secret, messages, model, timeout).await?;
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout))
        .header("content-type", "application/json");
    for (k, v) in &hdrs {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.body(body.to_string()).send().await.map_err(|e| (0, e.to_string()))?;
    let status = resp.status().as_u16() as i64;
    let stream = resp
        .bytes_stream()
        .map(|r| r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>));
    futures_util::pin_mut!(stream);
    let mut text = String::new();
    let mut thinking = String::new();
    let mut finish = "stop".to_string();
    let mut buf = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(bytes) => {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(pos) = buf.find("\n\n") {
                    let frame: String = buf.drain(..pos + 2).collect();
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
            Err(e) => return Err((status, format!("上游流中断：{}", e))),
        }
    }
    let full = text.trim().to_string();
    if full.is_empty() && thinking.trim().is_empty() {
        return Err((status, "元宝上游未返回内容（凭据可能失效或模型不支持）".to_string()));
    }
    let mut msg = json!({"role": "assistant", "content": full});
    if !thinking.trim().is_empty() {
        msg["reasoning_content"] = json!(thinking.trim());
    }
    Ok(json!({
        "id": format!("chatcmpl-{:024x}", nanos()),
        "object": "chat.completion",
        "created": nanos() / 1_000_000_000,
        "model": model,
        "choices": [{"index": 0, "message": msg, "finish_reason": finish}],
        "usage": {
            "prompt_tokens": (prompt_tokens_est(messages)).max(1),
            "completion_tokens": (full.len() / 3 + thinking.len() / 3).max(1),
            "total_tokens": ((prompt_tokens_est(messages) + full.len() + thinking.len()) / 3).max(1),
        },
    }))
}

/// 流式：元宝 SSE → OpenAI SSE chunk 帧。
pub async fn chat_stream(
    messages: &[Value],
    secret: &str,
    model: &str,
    timeout: u64,
) -> Result<Response, (i64, String)> {
    let (url, _conv_id, hdrs, body) = open_stream(secret, messages, model, timeout).await?;
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout))
        .header("content-type", "application/json");
    for (k, v) in &hdrs {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.body(body.to_string()).send().await.map_err(|e| (0, e.to_string()))?;
    let status = resp.status();
    if !(200..300).contains(&status.as_u16()) {
        let t = resp.text().await.unwrap_or_default();
        return Err((status.as_u16() as i64, t.chars().take(400).collect()));
    }
    let chat_id = format!("chatcmpl-{:024x}", nanos());
    let model_owned = model.to_string();
    let mk = move |delta: Value, finish: Option<&str>| -> String {
        format!(
            "data: {}\n\n",
            json!({
                "id": chat_id, "object": "chat.completion.chunk",
                "created": nanos() / 1_000_000_000, "model": model_owned,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            })
        )
    };
    let mk = std::sync::Arc::new(mk);
    let mk1 = mk.clone();
    let stream = resp.bytes_stream().map(|r| {
        r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    });
    let transform = move |state: &mut (String, bool, bool), item: Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>>| -> String {
        let (buf, sent_role, sent_thinking) = state;
        let mut out = String::new();
        match item {
            Ok(b) => {
                buf.push_str(&String::from_utf8_lossy(&b));
                while let Some(pos) = buf.find("\n\n") {
                    let frame: String = buf.drain(..pos + 2).collect();
                    if let Some((et, v)) = parse_sse_frame(&frame) {
                        if et == "message" {
                            match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                                "think" => {
                                    if let Some(c) = v.get("content").and_then(|x| x.as_str()) {
                                        if !*sent_role {
                                            out.push_str(&mk1(json!({"role": "assistant", "content": ""}), None));
                                            *sent_role = true;
                                        }
                                        if !*sent_thinking {
                                            out.push_str(&mk1(json!({"reasoning_content": c}), None));
                                            *sent_thinking = true;
                                        } else {
                                            out.push_str(&mk1(json!({"reasoning_content": c}), None));
                                        }
                                    }
                                }
                                "text" => {
                                    if let Some(c) = v.get("msg").and_then(|x| x.as_str()) {
                                        if !*sent_role {
                                            out.push_str(&mk1(json!({"role": "assistant", "content": ""}), None));
                                            *sent_role = true;
                                        }
                                        out.push_str(&mk1(json!({"content": c}), None));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                out
            }
            Err(e) => format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"aigw_error": true, "message": format!("上游流中断：{}", e)})
            ),
        }
    };
    let stream = stream.scan((String::new(), false, false), move |state, item| {
        let out: Result<String, Box<dyn std::error::Error + Send + Sync>> =
            Ok(transform(state, item));
        std::future::ready(Some(out))
    });
    let mk2 = mk.clone();
    let stream = stream.chain(futures_util::stream::once(async move {
        Ok::<String, Box<dyn std::error::Error + Send + Sync>>(format!(
            "{}data: [DONE]\n\n",
            mk2(json!({}), Some("stop"))
        ))
    }));
    let mut resp = Response::new(Body::from_stream(stream));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    Ok(resp)
}

fn prompt_tokens_est(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|m| m.get("content").map(|c| c.to_string().len()).unwrap_or(0))
        .sum()
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
