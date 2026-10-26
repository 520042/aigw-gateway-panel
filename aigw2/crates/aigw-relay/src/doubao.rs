//! 豆包（doubao.com samantha）adapter——纯 HTTP，不依赖无头浏览器/扫码。
//!
//! 一手来源：aigw-panel-rs/src/relay/doubao.rs（线上实测可用），旧版又移植自
//! wangchuxiaoji-oss/doubao2api 但**刻意绕开了它的浏览器签名端点**：
//! doubao2api 走 `/chat/completion`（必须浏览器内 fetch 注入 a_bogus/msToken），
//! 本项目走更老的 `/samantha/chat/completion` 端点，只需登录 Cookie + 客户端自派生
//! 的 19 位 device_id/web_id，纯 reqwest 即可。
//!
//! 凭据：账号池 `apk-doubao` 的 secret 为**整段登录 Cookie**（浏览器登录 doubao.com
//! 后在 DevTools→Application→Cookies 复制，或直接复制 Cookie 请求头，手动粘贴）。
//!
//! 协议要点：SSE 逐行 `data:` 外层 JSON（event_data 是内嵌 JSON 字符串需再 parse）；
//! event_type=2001 且 content_type ∈ (10000,2001,2008,2071) 取 text/thinking；
//! conversation_id 30 分钟内续接多轮。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::sync::mpsc;
use std::sync::{Mutex, OnceLock};

const BOT_ID: &str = "7338286299411103781";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const CONV_TTL: f64 = 1800.0;

/// 模型 → samantha 模式开关（对齐旧项目 MODE_FLAGS）。
fn mode_flags(model: &str) -> Value {
    match model {
        "doubao-think" => json!({"use_deep_think": true}),
        "doubao-expert" => json!({"use_deep_think": true, "use_auto_cot": true, "use_search": true}),
        "doubao-coding" => json!({"use_auto_cot": true}),
        "doubao-2.1-pro" => json!({"use_deep_think": true}),
        _ => json!({}),
    }
}

pub fn is_doubao_model(model: &str) -> bool {
    matches!(
        model,
        "doubao-pro" | "doubao-fast" | "doubao-lite" | "doubao-think" | "doubao-expert"
            | "doubao-coding" | "doubao-2.1-turbo" | "doubao-2.1-turbo-alt" | "doubao-2.1-pro"
            | "doubao-2.1-lite"
    )
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn now_secs() -> f64 {
    nanos() as f64 / 1e9
}

/// 稳定指纹（替代旧项目 MD5，服务端不校验算法，只用于本地会话续接与 device 派生）。
fn fp_hex(secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    let b = h.finalize();
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// 由凭据派生稳定 19 位 device_id/web_id（豆包要求 19 位数字）。
fn device_ids(secret: &str) -> (String, String) {
    let mut h = Sha256::new();
    h.update(format!("doubao{}", secret).as_bytes());
    let hex: String = h.finalize().iter().map(|x| format!("{:02x}", x)).collect();
    let n: String = i128::from_str_radix(&hex[..14], 16)
        .unwrap_or(0)
        .to_string()
        .chars()
        .take(19)
        .collect();
    let n = format!("{:0<19}", n);
    (n.clone(), n)
}

fn url_params(secret: &str) -> String {
    let (dev, web) = device_ids(secret);
    let params: Vec<String> = vec![
        "aid=497858".to_string(),
        format!("device_id={}", dev),
        "device_platform=web".to_string(),
        "language=zh".to_string(),
        "pc_version=3.17.3".to_string(),
        "pkg_type=release_version".to_string(),
        "real_aid=497858".to_string(),
        "region=CN".to_string(),
        "samantha_web=1".to_string(),
        "sys_region=CN".to_string(),
        format!("tea_uuid={}", dev),
        "use-olympus-account=1".to_string(),
        "version_code=20800".to_string(),
        format!("web_id={}", web),
    ];
    params.join("&")
}

fn headers(secret: &str) -> Vec<(String, String)> {
    vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "text/event-stream".into()),
        ("agw-js-conv".into(), "str".into()),
        ("cookie".into(), secret.to_string()),
        ("origin".into(), "https://www.doubao.com".into()),
        ("referer".into(), "https://www.doubao.com/chat/".into()),
        ("user-agent".into(), UA.into()),
        (
            "x-flow-trace".into(),
            json!({
                "trace_id": format!("{:032x}", nanos()),
                "span_id": format!("{:032x}", nanos() ^ 0xdead),
            })
            .to_string(),
        ),
    ]
}

// ── 会话续接：fp(secret) → (conversation_id, ts)，30 分钟 TTL ──
fn conv_map() -> &'static Mutex<HashMap<String, (String, f64)>> {
    static MAP: OnceLock<Mutex<HashMap<String, (String, f64)>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

fn conv_get(secret: &str) -> String {
    let fp = fp_hex(secret);
    let mut m = conv_map().lock().unwrap_or_else(|e| e.into_inner());
    match m.get_mut(&fp) {
        Some((c, ts)) if now_secs() - *ts < CONV_TTL && !c.is_empty() => {
            *ts = now_secs();
            c.clone()
        }
        _ => {
            m.remove(&fp);
            "0".to_string()
        }
    }
}

fn save_conv(secret: &str, cid: &str) {
    if !cid.is_empty() {
        let fp = fp_hex(secret);
        conv_map()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(fp, (cid.to_string(), now_secs()));
    }
}

fn extract_text(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .and_then(|x| x.as_str())
                    .or_else(|| p.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn body_of(messages: &[Value], model: &str, secret: &str) -> Value {
    let flags = mode_flags(model);
    let cid = conv_get(secret);
    let mut msgs: Vec<Value> = Vec::new();
    for m in messages {
        let t = extract_text(m);
        if t.trim().is_empty() {
            continue;
        }
        msgs.push(json!({
            "content": json!({"text": t}).to_string(),
            "content_type": 2001,
            "attachments": [],
            "references": [],
        }));
    }
    let mut ext = json!({});
    if flags.get("use_deep_think").and_then(|v| v.as_bool()).unwrap_or(false) {
        ext["use_deep_think"] = json!("1");
    }
    if flags.get("use_search").and_then(|v| v.as_bool()).unwrap_or(false) {
        ext["use_search"] = json!("1");
    }
    json!({
        "bot_id": BOT_ID,
        "completion_option": {
            "is_regen": false,
            "with_suggest": false,
            "need_create_conversation": cid == "0",
            "launch_stage": 1,
            "use_auto_cot": flags.get("use_auto_cot").and_then(|v| v.as_bool()).unwrap_or(false),
            "use_deep_think": flags.get("use_deep_think").and_then(|v| v.as_bool()).unwrap_or(false),
        },
        "conversation_id": cid,
        "local_conversation_id": format!("local_{}", nanos() % 100_000_000_000_000_000),
        "local_message_id": format!("{:x}", nanos()),
        "messages": msgs.last().cloned().map(|m| vec![m]).unwrap_or_default(),
        "ext": ext,
    })
}

/// 解析一行上游 SSE：返回 (event_type, inner data)。
fn sse_parse(line: &str) -> Option<(i64, Value)> {
    let ds = line.strip_prefix("data:")?.trim();
    if ds == "[DONE]" {
        return None;
    }
    let outer: Value = serde_json::from_str(ds).ok()?;
    let mut inner = outer.get("event_data").cloned().unwrap_or(Value::Null);
    if let Value::String(s) = &inner {
        if !s.is_empty() {
            inner = serde_json::from_str(s).unwrap_or(json!({}));
        }
    }
    Some((
        outer.get("event_type").and_then(|v| v.as_i64()).unwrap_or(0),
        inner,
    ))
}

/// 从 inner data 抽 (text, thinking, conversation_id)。
fn extract(p: &(i64, Value)) -> (String, String, String) {
    let (et, data) = p;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut cid = String::new();
    if *et == 2001 {
        if let Some(msg) = data.get("message") {
            let ct = msg.get("content_type").and_then(|v| v.as_i64()).unwrap_or(0);
            if matches!(ct, 10000 | 2001 | 2008 | 2071) {
                let rc = msg.get("content").cloned().unwrap_or(json!(""));
                let cc: Value = match &rc {
                    Value::String(s) if !s.is_empty() => {
                        serde_json::from_str(s).unwrap_or_else(|_| json!({"text": s}))
                    }
                    other => other.clone(),
                };
                let mut t = cc.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let mut th = cc
                    .get("thinking")
                    .or_else(|| cc.get("reasoning_content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if ct == 2008 && th.is_empty() {
                    th = t;
                    t = String::new();
                }
                text.push_str(&t);
                thinking.push_str(&th);
            }
        }
    }
    if let Some(c) = data.get("conversation_id").and_then(|v| v.as_str()) {
        cid = c.to_string();
    }
    (text, thinking, cid)
}

fn endpoint(spec: &ProviderSpec, secret: &str) -> String {
    let path = if spec.chat_path().is_empty() {
        "/samantha/chat/completion"
    } else {
        spec.chat_path()
    };
    format!("{}{}?{}", spec.base_url, path, url_params(secret))
}

fn err(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError {
        status,
        body: body.into(),
        class,
    }
}

/// 非流式：收齐 SSE 聚合 → OpenAI chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    if secret.is_empty() {
        return Err(err(401, "缺少豆包登录 Cookie（请在接入源粘贴 doubao.com 的 Cookie）", ErrorClass::Auth));
    }
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = openai.get("model").and_then(|m| m.as_str()).unwrap_or("doubao-pro");
    let messages = openai.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let req_body = body_of(&messages, model, secret).to_string();

    let mut rb = http.post(endpoint(spec, secret));
    for (k, v) in headers(secret) {
        rb = rb.header(k, v);
    }
    let resp = rb
        .body(req_body)
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| err(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let raw = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(err(
            status,
            raw.chars().take(400).collect::<String>(),
            ErrorClass::from_status(status),
        ));
    }
    let (mut text, mut thinking, mut cid) = (String::new(), String::new(), String::new());
    for line in raw.lines() {
        if let Some(p) = sse_parse(line.trim()) {
            let (t, th, c) = extract(&p);
            text.push_str(&t);
            thinking.push_str(&th);
            if !c.is_empty() {
                cid = c;
            }
        }
    }
    let full = text.trim().to_string();
    if full.is_empty() {
        return Err(err(
            status,
            format!("豆包上游未返回内容（Cookie 可能失效）：{}", raw.chars().take(300).collect::<String>()),
            ErrorClass::Auth,
        ));
    }
    save_conv(secret, &cid);
    let approx_in: usize = messages.iter().map(|m| extract_text(m).len()).sum();
    let mut msg = json!({"role": "assistant", "content": full});
    let reasoning = thinking.trim().to_string();
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    Ok(json!({
        "id": format!("chatcmpl-{:024x}", nanos()),
        "object": "chat.completion",
        "created": (nanos() / 1_000_000_000) as u64,
        "model": model,
        "conversation_id": cid,
        "choices": [{"index": 0, "message": msg, "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": (approx_in / 2).max(1),
            "completion_tokens": (full.len() / 2).max(1),
            "total_tokens": ((approx_in + full.len()) / 2).max(1),
        },
    }))
}

/// 流式：豆包 SSE → OpenAI chunk 帧（结尾 finish + [DONE]）。
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
        return Err(err(401, "缺少豆包登录 Cookie", ErrorClass::Auth));
    }
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = openai
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("doubao-pro")
        .to_string();
    let messages = openai.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let req_body = body_of(&messages, &model, secret).to_string();

    let mut rb = http.post(endpoint(spec, secret));
    for (k, v) in headers(secret) {
        rb = rb.header(k, v);
    }
    // 非流式必须给**总时长**：只靠 client 的 read_timeout（300s）兜底时，
    // 上游每隔几十秒吐一个字节就能一直挂着，用户端没有任何反馈。
    let resp = rb
        .timeout(crate::NONSTREAM_TOTAL)
        .body(req_body)
        .send()
        .await
        .map_err(|e| err(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        return Err(err(status, t.chars().take(400).collect::<String>(), ErrorClass::from_status(status)));
    }

    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let secret_owned = secret.to_string();
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
        let mut sent_role = false;
        let mut dec = crate::Utf8Decoder::new();
        let mut cid_out = String::new();
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
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                let Some(p) = sse_parse(line) else { continue };
                let (t, th, c) = extract(&p);
                if !sent_role && (!t.is_empty() || !th.is_empty()) {
                    out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                    sent_role = true;
                }
                if !th.is_empty() {
                    out.push_str(&mk(json!({"reasoning_content": th}), None));
                }
                if !t.is_empty() {
                    out_chars += t.chars().count() + th.chars().count();
                    out.push_str(&mk(json!({"content": t}), None));
                } else {
                    out_chars += th.chars().count();
                }
                if !c.is_empty() {
                    cid_out = c;
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break 'outer;
            }
        }
        save_conv(&secret_owned, &cid_out);
        let mut tail = String::new();
        // 无内容兜底：samantha 流里无正文（Cookie 失效 / 被风控 / 模型不支持）
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "豆包未返回正文（Cookie 可能已失效，请重新登录 www.doubao.com 粘贴含 sessionid 的整段 Cookie；或被风控）".into();
        }
        if let Some(ec) = stat.error {
            // HTTP 头已 200（豆包 SSE 建连即 200），用 OpenAI 流内 error 帧透传，避免空响应
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

/// 验活：/passport/account/info/v2 返回 user_id 即有效。
pub async fn verify(http: &reqwest::Client, _spec: &ProviderSpec, secret: &str) -> bool {
    let url = "https://www.doubao.com/passport/account/info/v2?account_sdk_source=web&aid=497858";
    match http
        .post(url)
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", UA)
        .header("Cookie", secret)
        .header("Content-Type", "application/json")
        .send()
        .await
    {
        Ok(r) => {
            let v: Value = serde_json::from_str(&r.text().await.unwrap_or_default()).unwrap_or(json!({}));
            v.pointer("/data/user_id").is_some_and(|x| !x.is_null())
                || v.get("user_id").is_some_and(|x| !x.is_null())
        }
        Err(_) => false,
    }
}
