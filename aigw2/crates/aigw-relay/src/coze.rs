//! 扣子（Coze）开放平台 v3 chat 适配器。
//!
//! 为什么需要：扣子**没有** OpenAI 兼容端点（实测 2026-10-22：
//! `POST https://api.coze.cn/v1/chat/completions` → 404 `{"code":4000,"msg":
//! "The requested API endpoint POST /v1/chat/completions does not exist"}`；
//! 而 `POST /v3/chat` 存在）。对话对象其实是**用户自己的 bot**，
//! 所以「模型名」在本适配器里就是 **bot_id**（也接受 `bot:` 前缀写法）。
//!
//! 链路（全走流式，非流式在本地聚合 —— 少一次 retrieve/message_list 往返，
//! 也避免轮询超时这类额外失败面）：
//! 1) `POST {base}/v3/chat` body `{bot_id, user_id, stream:true,
//!    auto_save_history:true, additional_messages:[{role:"user",content,
//!    content_type:"text"}]}`，头 `Authorization: Bearer <PAT>`；
//! 2) SSE `event:` 行给类型，`data:` 行给 JSON：
//!    - `conversation.message.delta`：`type=="answer"` 的增量正文；
//!    - `conversation.message.completed`：`type=="answer"` 的最终全文；
//!    - `conversation.chat.completed`：带 `usage.token_count/input_count/output_count`；
//!    - `conversation.chat.failed`：`last_error.msg` / `msg` 是错因；
//!    - `done`（data 常为 `"[DONE]"`）：流结束。
//! 3) 业务错误常见 HTTP 200 + `{"code":4xxx,"msg":"…"}`，必须当错误处理。

use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::Stream;
use serde_json::{json, Value};
use std::pin::Pin;
use std::time::Duration;

use crate::RelayError;

const COZE_BASE: &str = "https://api.coze.cn";
const COZE_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";
const DEFAULT_USER_ID: &str = "aigw-panel";

fn er(status: u16, body: String, class: ErrorClass) -> RelayError {
    RelayError { status, body, class }
}

pub fn trunc(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if t.chars().count() < s.chars().count() {
        format!("{t}…")
    } else {
        t
    }
}

/// 凭据：`<PAT>` 或 `{"token":"…","user_id":"…","bot_id":"…","base":"…"}`。
#[derive(Clone, Debug, Default)]
pub struct CozeCred {
    pub token: String,
    pub user_id: String,
    pub bot_id: String,
    pub base: String,
}

fn pick_one(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn parse_cred(secret: &str) -> Result<CozeCred, String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("Coze 凭据为空：请粘贴扣子开放平台的个人访问令牌（PAT，pat_ 开头）".to_string());
    }
    if s.starts_with('{') {
        let v: Value = serde_json::from_str(s)
            .map_err(|e| format!("Coze 凭据不是合法 JSON：{e}"))?;
        let token = pick_one(&v, &["token", "access_token", "accessToken", "pat", "api_key", "apiKey"])
            .unwrap_or_default();
        if token.trim().is_empty() {
            return Err("Coze 凭据 JSON 里缺 token 字段（PAT）".to_string());
        }
        return Ok(CozeCred {
            token: token.trim().to_string(),
            user_id: pick_one(&v, &["user_id", "userId", "uid"]).unwrap_or_default(),
            bot_id: pick_one(&v, &["bot_id", "botId", "bot"]).unwrap_or_default(),
            base: pick_one(&v, &["base", "base_url", "baseUrl"]).unwrap_or_default(),
        });
    }
    if s.contains('=') || s.contains(';') {
        for key in ["token", "pat", "access_token", "cookie"] {
            if let Some(v) = cookie_value(s, key) {
                return Ok(CozeCred { token: v, ..Default::default() });
            }
        }
    }
    Ok(CozeCred { token: s.to_string(), ..Default::default() })
}

/// 从整段 Cookie 里取 `key=value`（要求 key 前面是串首 / `;` / 空格 / `&` / 引号，
/// 避免 `pat` 命中 `pat_cookie` 这种子串误判）。
fn cookie_value(s: &str, key: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(idx) = s[from..].find(key) {
        let i = from + idx;
        let ok_before = i == 0 || matches!(s.as_bytes()[i - 1] as char, ';' | ' ' | '&' | '"' | ',');
        if ok_before {
            if let Some(rest) = s[i + key.len()..].strip_prefix('=') {
                let v: String = rest
                    .trim_start()
                    .trim_start_matches('"')
                    .chars()
                    .take_while(|c| !matches!(c, ';' | '&' | '"' | ' '))
                    .collect();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        from = i + key.len();
    }
    None
}

fn base_of(spec: &ProviderSpec, cred: &CozeCred) -> String {
    if spec.base_url.starts_with("http://") || spec.base_url.contains("127.0.0.1") {
        return spec.base_url.trim_end_matches('/').to_string();
    }
    if !cred.base.trim().is_empty() {
        return cred.base.trim_end_matches('/').to_string();
    }
    if !spec.base_url.trim().is_empty() && !spec.base_url.starts_with('@') {
        return spec.base_url.trim_end_matches('/').to_string();
    }
    COZE_BASE.to_string()
}

fn headers(spec: &ProviderSpec, cred: &CozeCred) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("Authorization".to_string(), format!("Bearer {}", cred.token)),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "text/event-stream".to_string()),
        ("User-Agent".to_string(), COZE_UA.to_string()),
    ];
    for (k, v) in &spec.headers {
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

/// 文本拼装：`content` 可能是字符串，也可能是 OpenAI/Anthropic 的块数组。
pub fn content_text(v: Option<&Value>) -> String {
    let Some(v) = v else { return String::new() };
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(arr) = v.as_array() {
        for part in arr {
            if let Some(s) = part.as_str() {
                out.push_str(s);
                continue;
            }
            for k in ["text", "content"] {
                if let Some(s) = part.get(k).and_then(|x| x.as_str()) {
                    out.push_str(s);
                }
            }
        }
    }
    out
}

/// 多轮 → 单条 query：扣子的 bot 是「一问一答」形态，不带 conversation_id 时
/// 无法续接历史，所以这里把历史压成一段可读文本。
pub fn flatten_prompt(v: &Value) -> String {
    let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) else {
        return String::new();
    };
    let mut system = String::new();
    let mut dialog: Vec<String> = Vec::new();
    for m in msgs {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
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
            "assistant" => dialog.push(format!("Assistant: {text}")),
            _ => dialog.push(format!("User: {text}")),
        }
    }
    let mut out = String::new();
    if !system.trim().is_empty() {
        out.push_str(&format!("[系统设定]\n{}\n\n", system.trim()));
    }
    if dialog.len() > 1 {
        out.push_str(&dialog.join("\n"));
        out.push_str("\n\n请针对最后一条 User 消息作答。");
    } else if let Some(last) = dialog.last() {
        out.push_str(last.trim_start_matches("User: "));
    }
    out
}

/// 「模型名」→ bot_id。接受 `bot:<id>` / `bot_<id>` 写法，也接受 cred 里预设的 bot_id。
pub fn bot_of(cred: &CozeCred, model: &str) -> String {
    let m = model.trim();
    if let Some(rest) = m.strip_prefix("bot:") {
        if !rest.trim().is_empty() {
            return rest.trim().to_string();
        }
    }
    if !cred.bot_id.trim().is_empty() && (m.is_empty() || m == "default" || m == "auto" || m == "coze") {
        return cred.bot_id.trim().to_string();
    }
    m.to_string()
}

/// 业务错误信封（HTTP 200 + code != 0）与 4xx JSON 都走这里。
pub fn error_of(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if code == 0 {
        return None;
    }
    let msg = v
        .get("msg")
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("未知错误");
    Some(format!("code={code}：{msg}"))
}

fn status_class(status: u16) -> ErrorClass {
    match status {
        401 | 403 => ErrorClass::Auth,
        402 => ErrorClass::Quota,
        429 => ErrorClass::Rate,
        400..=499 => ErrorClass::Rejected,
        _ => ErrorClass::Transient,
    }
}

/// SSE 行扫描：`event:` / `data:`，空行是事件边界。
#[derive(Default)]
pub struct CozeState {
    pub event: String,
    pub data: Vec<String>,
}

pub fn scan_line(st: &mut CozeState, line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches('\r');
    if line.is_empty() {
        if st.event.is_empty() && st.data.is_empty() {
            return None;
        }
        let ev = std::mem::take(&mut st.event);
        let data = std::mem::take(&mut st.data).join("\n");
        return Some((ev, data));
    }
    if let Some(rest) = line.strip_prefix("event:") {
        st.event = rest.trim().to_string();
        return None;
    }
    if let Some(rest) = line.strip_prefix("data:") {
        st.data.push(rest.trim_start().to_string());
    }
    None
}

/// 一个事件解析后的产物。
#[derive(Debug, Default, Clone)]
pub struct CozeEvent {
    pub delta: String,
    pub final_text: String,
    pub usage_in: i64,
    pub usage_out: i64,
    pub failed: Option<String>,
    pub done: bool,
}

fn msg_type(v: &Value) -> String {
    v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string()
}

pub fn parse_event(event: &str, data: &str) -> CozeEvent {
    let mut ev = CozeEvent::default();
    let d = data.trim();
    if d == "[DONE]" || event == "done" {
        ev.done = true;
        return ev;
    }
    let Ok(v) = serde_json::from_str::<Value>(d) else {
        return ev;
    };
    match event {
        "conversation.message.delta" => {
            if msg_type(&v) == "answer" {
                if let Some(c) = v.get("content").and_then(|c| c.as_str()) {
                    ev.delta = c.to_string();
                }
            }
        }
        "conversation.message.completed" => {
            if msg_type(&v) == "answer" {
                if let Some(c) = v.get("content").and_then(|c| c.as_str()) {
                    ev.final_text = c.to_string();
                }
            }
        }
        "conversation.chat.completed" => {
            if let Some(u) = v.get("usage") {
                ev.usage_in = u.get("input_count").and_then(|x| x.as_i64()).unwrap_or(0);
                ev.usage_out = u.get("output_count").and_then(|x| x.as_i64()).unwrap_or(0);
            }
            ev.done = true;
        }
        "conversation.chat.failed" => {
            let msg = v
                .get("last_error")
                .and_then(|e| e.get("msg"))
                .or_else(|| v.get("msg"))
                .and_then(|m| m.as_str())
                .unwrap_or("扣子返回对话失败");
            ev.failed = Some(msg.to_string());
            ev.done = true;
        }
        _ => {
            // 有些网关不带 event: 行，直接 data 里塞错误信封
            if let Some(e) = error_of(d) {
                ev.failed = Some(e);
                ev.done = true;
            }
        }
    }
    ev
}

fn nanos_hex() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format!("{:x}{:04x}", d.as_nanos(), std::process::id() & 0xffff),
        Err(_) => "0".to_string(),
    }
}

fn now_secs() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

fn est_tokens(text: &str) -> i64 {
    let mut n = 0i64;
    for ch in text.chars() {
        if (ch as u32) >= 0x2E80 {
            n += 1;
        } else if !ch.is_whitespace() {
            n += 1;
        }
    }
    (n / 2).max(1)
}

struct Prepared {
    base: String,
    hs: Vec<(String, String)>,
    body: String,
    bot: String,
}

async fn prepare(
    spec: &ProviderSpec,
    secret: &str,
    v: &Value,
) -> Result<Prepared, RelayError> {
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let base = base_of(spec, &cred);
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let bot = bot_of(&cred, &model);
    if bot.trim().is_empty() {
        return Err(er(
            400,
            "Coze 需要 bot_id：模型名请写成 `<bot_id>@apk-coze`，或在凭据 JSON 里带 bot_id 字段".to_string(),
            ErrorClass::Rejected,
        ));
    }
    let prompt = flatten_prompt(v);
    if prompt.trim().is_empty() {
        return Err(er(400, "Coze 请求里没有可发送的用户消息".to_string(), ErrorClass::Rejected));
    }
    let uid = if cred.user_id.trim().is_empty() {
        DEFAULT_USER_ID.to_string()
    } else {
        cred.user_id.clone()
    };
    let body = json!({
        "bot_id": bot,
        "user_id": uid,
        "stream": true,
        "auto_save_history": true,
        "additional_messages": [{
            "role": "user",
            "content": prompt,
            "content_type": "text"
        }],
    })
    .to_string();
    Ok(Prepared { base, hs: headers(spec, &cred), body, bot })
}

async fn post_chat(
    http: &reqwest::Client,
    prepared: &Prepared,
    timeout: Duration,
) -> Result<reqwest::Response, RelayError> {
    let url = format!("{}/v3/chat", prepared.base);
    let rb = http
        .post(&url)
        .timeout(timeout)
        .body(prepared.body.clone());
    let rb = apply(rb, &prepared.hs);
    let resp = rb
        .send()
        .await
        .map_err(|e| er(502, format!("Coze 上游连接失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        let msg = error_of(&text).unwrap_or_else(|| trunc(&text, 300));
        return Err(er(status, format!("Coze 上游 {status}：{msg}"), status_class(status)));
    }
    Ok(resp)
}

/// 把 SSE 全文折叠成 (正文, 输入 token, 输出 token, 失败原因)。
pub fn aggregate(text: &str) -> (String, i64, i64, Option<String>) {
    let mut st = CozeState::default();
    let mut content = String::new();
    let mut final_text = String::new();
    let mut tin = 0i64;
    let mut tout = 0i64;
    let mut failed = None;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.push("");
    }
    for line in lines {
        if let Some((ev, data)) = scan_line(&mut st, line) {
            let e = parse_event(&ev, &data);
            if !e.delta.is_empty() {
                content.push_str(&e.delta);
            }
            if !e.final_text.is_empty() {
                final_text = e.final_text;
            }
            if e.usage_in > 0 {
                tin = e.usage_in;
            }
            if e.usage_out > 0 {
                tout = e.usage_out;
            }
            if let Some(f) = e.failed {
                failed = Some(f);
                break;
            }
        }
    }
    if content.trim().is_empty() && !final_text.trim().is_empty() {
        content = final_text;
    }
    (content, tin, tout, failed)
}

pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let prepared = prepare(spec, secret, &v).await?;
    let resp = post_chat(http, &prepared, Duration::from_secs(180)).await?;
    let text = resp
        .text()
        .await
        .map_err(|e| er(502, format!("Coze 响应读取失败：{e}"), ErrorClass::Transient))?;
    // 有些错误在 200 里以 JSON 信封返回（不是 SSE）
    if !text.trim_start().starts_with("event:") && !text.contains("data:") {
        if let Some(e) = error_of(&text) {
            return Err(er(401, format!("Coze 返回错误：{e}"), ErrorClass::Auth));
        }
    }
    let (content, tin, tout, failed) = aggregate(&text);
    if let Some(f) = failed {
        return Err(er(502, format!("Coze 对话失败：{f}"), ErrorClass::Transient));
    }
    if content.trim().is_empty() {
        return Err(er(
            401,
            format!(
                "Coze 未返回正文（PAT 可能无效、bot 未发布到 API，或额度用尽）。上游原文：{}",
                trunc(&text, 300)
            ),
            ErrorClass::Auth,
        ));
    }
    let tin = if tin > 0 { tin } else { est_tokens(&prepared.body) };
    let tout = if tout > 0 { tout } else { est_tokens(&content) };
    Ok(json!({
        "id": format!("chatcmpl-coze-{}", nanos_hex()),
        "object": "chat.completion",
        "created": now_secs(),
        "model": prepared.bot,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": tin,
            "completion_tokens": tout,
            "total_tokens": tin + tout
        }
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
    use futures_util::StreamExt;
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let prepared = prepare(spec, secret, &v).await?;
    let resp = post_chat(http, &prepared, Duration::from_secs(300)).await?;
    let model = prepared.bot.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let (stat_tx, stat_rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(8);
    tokio::spawn(async move {
        let mut upstream = resp.bytes_stream();
        let mut st = CozeState::default();
        let mut line_buf: Vec<u8> = Vec::new();
        let mut decoder = crate::Utf8Decoder::new();
        let mut sent_role = false;
        let mut out_chars = 0usize;
        let mut in_bytes = prepared.body.len();
        let mut tin = 0i64;
        let mut tout = 0i64;
        let id = format!("chatcmpl-coze-{}", nanos_hex());
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
        while let Some(chunk) = upstream.next().await {
            let Ok(bytes) = chunk else {
                // ★ 中途断流也要把**已经产生的**用量记进台账：旧实现这里恒发 0/0，
                //   于是「已经吐了 3000 字然后连接被上游掐断」在面板上显示为 0 token，
                //   成本归因偏低。有上游 usage 就用它，没有就按已收字节/已发字符估算。
                let (ti, to) = if tin > 0 || tout > 0 {
                    (tin, tout)
                } else {
                    crate::estimate_stream_tokens(in_bytes, out_chars)
                };
                let _ = stat_tx.send(crate::StreamStat { error: Some(ErrorClass::Transient), finish_reason: None, tokens_in: ti, tokens_out: to, ..Default::default() }).await;
                return;
            };
            in_bytes += bytes.len();
            let text = decoder.push(&bytes);
            line_buf.extend_from_slice(text.as_bytes());
            let mut consumed = 0usize;
            loop {
                let Some(pos) = line_buf[consumed..].iter().position(|b| *b == b'\n') else {
                    break;
                };
                let end = consumed + pos;
                let line = String::from_utf8_lossy(&line_buf[consumed..end]).to_string();
                consumed = end + 1;
                let Some((ev, data)) = scan_line(&mut st, &line) else { continue };
                let e = parse_event(&ev, &data);
                if let Some(f) = e.failed {
                    let frame = format!("data: {}\n\n", json!({"error": {"message": format!("Coze 对话失败：{f}"), "type": "upstream_error"}}));
                    let _ = tx.send(Ok(Bytes::from(frame))).await;
                    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                    let _ = stat_tx.send(crate::StreamStat { error: Some(ErrorClass::Transient), finish_reason: None, tokens_in: tin, tokens_out: tout, ..Default::default() }).await;
                    return;
                }
                if e.usage_in > 0 { tin = e.usage_in; }
                if e.usage_out > 0 { tout = e.usage_out; }
                let mut piece = e.delta;
                if piece.is_empty() && !e.final_text.is_empty() {
                    piece = e.final_text;
                }
                if !piece.is_empty() {
                    out_chars += piece.chars().count();
                    let mut delta = serde_json::Map::new();
                    if !sent_role {
                        delta.insert("role".to_string(), json!("assistant"));
                        sent_role = true;
                    }
                    delta.insert("content".to_string(), json!(piece));
                    let frame = mk(Value::Object(delta), None);
                    if tx.send(Ok(Bytes::from(frame))).await.is_err() {
                        // 下游断开（客户端取消）：仍要把已产生的用量记进台账，
                        // 否则取消一次长回答会被记成 0 token 的成功/失败事件。
                        let (ti, to) = if tin > 0 || tout > 0 {
                            (tin, tout)
                        } else {
                            crate::estimate_stream_tokens(in_bytes, out_chars)
                        };
                        let _ = stat_tx.send(crate::StreamStat { error: Some(ErrorClass::Transient), finish_reason: None, tokens_in: ti, tokens_out: to, client_gone: true, ..Default::default() }).await;
                        return;
                    }
                }
                if e.done {
                    let mut delta = serde_json::Map::new();
                    delta.insert("content".to_string(), json!(""));
                    let frame = mk(Value::Object(delta), Some("stop"));
                    let _ = tx.send(Ok(Bytes::from(frame))).await;
                    let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
                    let (ti, to) = if tin > 0 || tout > 0 {
                        (tin, tout)
                    } else {
                        crate::estimate_stream_tokens(in_bytes, out_chars)
                    };
                    let _ = stat_tx.send(crate::StreamStat { error: None, finish_reason: Some("stop".to_string()), tokens_in: ti, tokens_out: to, ..Default::default() }).await;
                    return;
                }
            }
            line_buf.drain(..consumed);
        }
        {
            let mut delta = serde_json::Map::new();
            delta.insert("content".to_string(), json!(""));
            let frame = mk(Value::Object(delta), Some("stop"));
            let _ = tx.send(Ok(Bytes::from(frame))).await;
            let _ = tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
        }
        let (ti, to) = if tin > 0 || tout > 0 {
            (tin, tout)
        } else {
            crate::estimate_stream_tokens(in_bytes, out_chars)
        };
        let _ = stat_tx.send(crate::StreamStat { error: None, finish_reason: Some("stop".to_string()), tokens_in: ti, tokens_out: to, ..Default::default() }).await;
    });
    Ok((Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)), stat_rx))
}

pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok(cred) = parse_cred(secret) else { return false };
    let base = base_of(spec, &cred);
    let hs = headers(spec, &cred);
    let rb = http
        .get(format!("{base}/v1/workspaces"))
        .timeout(Duration::from_secs(30));
    let rb = apply(rb, &hs);
    let Ok(resp) = rb.send().await else { return false };
    if !resp.status().is_success() {
        return false;
    }
    let text = resp.text().await.unwrap_or_default();
    error_of(&text).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;

    fn spec_for(base: &str) -> ProviderSpec {
        let raw = format!(
            r#"id = "apk-coze"
name = "扣子 Coze"
enabled = true
auth = "api_key"
base_url = "{base}"
format = "openai"
models_policy = "none"
stream_dialect = "sse_delta"
upstream_kind = "coze""#,
        );
        aigw_registry::parse_spec(&raw).expect("spec")
    }

    #[test]
    fn parse_cred_accepts_pat_json_and_cookie() {
        let a = parse_cred("pat_abcdef123").unwrap();
        assert_eq!(a.token, "pat_abcdef123");
        let b = parse_cred(r#"{"token":"pat_1","user_id":"u-9","bot_id":"bot-7"}"#).unwrap();
        assert_eq!(b.token, "pat_1");
        assert_eq!(b.user_id, "u-9");
        assert_eq!(b.bot_id, "bot-7");
        let c = parse_cred("a=b; token=pat_cookie; x=y").unwrap();
        assert_eq!(c.token, "pat_cookie");
        assert!(parse_cred("   ").is_err());
    }

    #[test]
    fn bot_of_prefers_model_and_falls_back_to_cred() {
        let mut cred = CozeCred::default();
        assert_eq!(bot_of(&cred, "7001"), "7001");
        assert_eq!(bot_of(&cred, "bot:7002"), "7002");
        cred.bot_id = "bot-fallback".to_string();
        assert_eq!(bot_of(&cred, "default"), "bot-fallback");
        assert_eq!(bot_of(&cred, "7003"), "7003");
    }

    #[test]
    fn flatten_prompt_prefixes_system_and_keeps_last_user() {
        let v = json!({"messages": [
            {"role": "system", "content": "你是助手"},
            {"role": "user", "content": [{"type": "text", "text": "你好"}]}
        ]});
        let out = flatten_prompt(&v);
        assert!(out.contains("[系统设定]"));
        assert!(out.contains("你是助手"));
        assert!(out.trim_end().ends_with("你好"));
    }

    #[test]
    fn aggregate_reads_deltas_completed_and_usage() {
        let sse = concat!(
            "event: conversation.message.delta\n",
            "data: {\"type\":\"answer\",\"content\":\"你\"}\n\n",
            "event: conversation.message.delta\n",
            "data: {\"type\":\"verbose\",\"content\":\"噪音\"}\n\n",
            "event: conversation.message.delta\n",
            "data: {\"type\":\"answer\",\"content\":\"好\"}\n\n",
            "event: conversation.chat.completed\n",
            "data: {\"usage\":{\"input_count\":7,\"output_count\":2}}\n\n",
            "event: done\n",
            "data: \"[DONE]\"\n\n"
        );
        let (content, tin, tout, failed) = aggregate(sse);
        assert_eq!(content, "你好");
        assert_eq!(tin, 7);
        assert_eq!(tout, 2);
        assert!(failed.is_none());
    }

    #[test]
    fn aggregate_reports_failed_chat() {
        let sse = "event: conversation.chat.failed\ndata: {\"last_error\":{\"msg\":\"bot 未发布\"}}\n\n";
        let (_c, _i, _o, failed) = aggregate(sse);
        assert!(failed.unwrap().contains("bot 未发布"));
    }

    #[test]
    fn error_of_reads_business_envelope() {
        assert!(error_of("{\"code\":0,\"data\":[]}").is_none());
        assert!(error_of("{\"code\":4100,\"msg\":\"authentication is invalid\"}")
            .unwrap()
            .contains("4100"));
    }

    async fn serve(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn mock_sse() -> String {
        [
            "event: conversation.chat.created",
            "data: {\"id\":\"chat-1\",\"conversation_id\":\"conv-1\"}",
            "",
            "event: conversation.message.delta",
            "data: {\"type\":\"answer\",\"content\":\"你\"}",
            "",
            "event: conversation.message.delta",
            "data: {\"type\":\"answer\",\"content\":\"好\"}",
            "",
            "event: conversation.message.completed",
            "data: {\"type\":\"answer\",\"content\":\"你好\"}",
            "",
            "event: conversation.chat.completed",
            "data: {\"usage\":{\"input_count\":5,\"output_count\":2}}",
            "",
            "event: done",
            "data: \"[DONE]\"",
            "",
        ]
        .join("\n")
    }

    #[tokio::test]
    async fn coze_nonstream_via_mock() {
        let body = mock_sse();
        let app = Router::new().fallback(move |_req: axum::extract::Request| {
            let body = body.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    body,
                )
            }
        });
        let base = serve(app).await;
        let spec = spec_for(&base);
        let req = json!({"model": "7001", "messages": [{"role": "user", "content": "你好"}]});
        let v = chat_nonstream(
            &reqwest::Client::new(),
            &spec,
            "pat_test",
            req.to_string().as_bytes(),
        )
        .await
        .unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "你好");
        assert_eq!(v["usage"]["prompt_tokens"], 5);
        assert_eq!(v["usage"]["completion_tokens"], 2);
        assert_eq!(v["model"], "7001");
    }

    #[tokio::test]
    async fn coze_stream_via_mock() {
        use futures_util::StreamExt;
        let body = mock_sse();
        let app = Router::new().fallback(move |_req: axum::extract::Request| {
            let body = body.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    body,
                )
            }
        });
        let base = serve(app).await;
        let spec = spec_for(&base);
        let req = json!({"model": "7002", "stream": true, "messages": [{"role": "user", "content": "hi"}]});
        let (mut s, mut rx) = chat_stream(
            &reqwest::Client::new(),
            &spec,
            "pat_test",
            req.to_string().as_bytes(),
        )
        .await
        .unwrap();
        let mut out = String::new();
        while let Some(Ok(b)) = s.next().await {
            out.push_str(&String::from_utf8_lossy(&b));
        }
        assert!(out.contains("你"), "{out}");
        assert!(out.contains("\"stop\""), "{out}");
        assert!(out.trim_end().ends_with("data: [DONE]"), "{out}");
        let stat = rx.recv().await.unwrap();
        assert_eq!(stat.finish_reason.as_deref(), Some("stop"));
        assert!(stat.error.is_none());
    }

    /// ★ 回归：上游中途断流时，已产生的用量必须进台账。
    /// 旧实现在读 chunk 失败分支恒发 `tokens_in:0, tokens_out:0`，
    /// 于是「已经吐了正文然后连接被掐断」在面板上显示为 0 token。
    #[tokio::test]
    async fn mid_stream_break_keeps_accumulated_usage() {
        use futures_util::StreamExt;
        // 先发一段正文再直接断开连接（不写 Content-Length，模拟上游掐断）。
        // 手写一个「发完一段正文就把连接掐掉」的上游：axum 会在响应体出错时
        // 先把错误吞掉（连接直接 reset），用裸 TCP 才能稳定复现 mid-stream 断流。
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = l.accept().await {
                // 先把请求头读完（否则客户端会以为连接被拒）
                let mut buf = vec![0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
                let head = "event: conversation.message.delta\ndata: {\"type\":\"answer\",\"content\":\"你好世界\"}\n\n";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{head}\r\n",
                    head.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
                // 不发终止块，直接断开 → 客户端读到 chunk 错误
            }
        });
        let base = format!("http://{addr}");
        let spec = spec_for(&base);
        let req = json!({"model": "7001", "stream": true, "messages": [{"role": "user", "content": "hi"}]});
        let (mut s, mut rx) = chat_stream(
            &reqwest::Client::new(),
            &spec,
            "pat_test",
            req.to_string().as_bytes(),
        )
        .await
        .unwrap();
        while let Some(_b) = s.next().await {}
        let stat = rx.recv().await.expect("必须有统计事件");
        assert!(stat.error.is_some(), "中途断流应记为失败");
        assert!(
            stat.tokens_out > 0,
            "断流前已输出正文，tokens_out 不应为 0（实际 {}）",
            stat.tokens_out
        );
    }

    #[tokio::test]
    async fn coze_verify_hits_workspaces() {
        let app = Router::new().route(
            "/v1/workspaces",
            axum::routing::get(|| async { axum::Json(json!({"code": 0, "data": [{"id": "ws-1"}]})) }),
        );
        let base = serve(app).await;
        let spec = spec_for(&base);
        assert!(verify(&reqwest::Client::new(), &spec, "pat_test").await);
        let bad = Router::new().route(
            "/v1/workspaces",
            axum::routing::get(|| async { axum::Json(json!({"code": 4100, "msg": "invalid"})) }),
        );
        let base2 = serve(bad).await;
        let spec2 = spec_for(&base2);
        assert!(!verify(&reqwest::Client::new(), &spec2, "pat_bad").await);
    }
}

