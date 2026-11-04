//! 豆包 samantha 原生中继（协议逆向移植自 app/doubao_relay.py）。
//!
//! 端点：`POST https://www.doubao.com/samantha/chat/completion?aid=497858&...`
//! 凭据：账号池 `apk-doubao` 平台的 Cookie（手动粘贴入库）。
//! 协议要点：SSE 逐行 `data:` 外层 JSON（event_data 内嵌字符串再解一层）；
//! event_type=2001 且 content_type ∈ (10000,2001,2008,2071) 取
//! text/thinking；conversation_id 30 分钟内续接多轮。
use md5::{Digest, Md5};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const API: &str = "https://www.doubao.com/samantha/chat/completion";
const BOT_ID: &str = "7338286299411103781";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// 模型 → samantha 模式开关（对齐 MODE_FLAGS）。
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
            | "doubao-2.1-lite" | "0" | "3" | "4" | "5" | "9" | "seed-lite-7b"
    )
}

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// 由凭据派生稳定 19 位 device_id/web_id（豆包要求 19 位数字）。
fn device_ids(secret: &str) -> (String, String) {
    let h = {
        let mut hasher = Md5::new();
        hasher.update(format!("doubao{}", secret));
        hex(&hasher.finalize())
    };
    let n: String = i128::from_str_radix(&h[..14.min(h.len())], 16)
        .unwrap_or(0)
        .to_string()
        .chars()
        .take(19)
        .collect();
    let n = format!("{:0<19}", n);
    (n.clone(), n)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn url_params(secret: &str) -> String {
    let (dev, web) = device_ids(secret);
    [
        "aid=497858", &format!("device_id={}", dev), "device_platform=web", "language=zh",
        "pc_version=3.17.3", "pkg_type=release_version", "real_aid=497858", "region=CN",
        "samantha_web=1", "sys_region=CN", &format!("tea_uuid={}", dev),
        "use-olympus-account=1", "version_code=20800", &format!("web_id={}", web),
    ]
    .join("&")
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
            json!({"trace_id": format!("{:032x}", nanos()), "span_id": format!("{:032x}", nanos() ^ 0xdead)})
                .to_string(),
        ),
    ]
}

// ── 会话续接：fp(secret) → (conversation_id, ts)，30 分钟 TTL ──
fn conv_map() -> &'static Mutex<HashMap<String, (String, f64)>> {
    static MAP: std::sync::OnceLock<Mutex<HashMap<String, (String, f64)>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> f64 {
    nanos() as f64 / 1e9
}

fn body_of(messages: &[Value], model: &str, secret: &str) -> Value {
    let flags = mode_flags(model);
    let fp = format!("{:x}", {
        let mut h = Md5::new();
        h.update(secret.as_bytes());
        h.finalize()[..8].iter().fold(0u64, |a, b| (a << 8) | *b as u64)
    });
    let cid = {
        let mut m = conv_map().lock().unwrap();
        match m.get_mut(&fp) {
            Some((c, ts)) if now_secs() - *ts < 1800.0 && !c.is_empty() => {
                *ts = now_secs();
                c.clone()
            }
            _ => {
                m.remove(&fp);
                "0".to_string()
            }
        }
    };
    let mut msgs: Vec<Value> = Vec::new();
    for m in messages {
        let t = match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|x| x.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if t.is_empty() {
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
            "is_regen": false, "with_suggest": false,
            "need_create_conversation": cid == "0", "launch_stage": 1,
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

/// 解析一行上游 SSE：返回 (event_type, data)。
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
    Some((outer.get("event_type").and_then(|v| v.as_i64()).unwrap_or(0), inner))
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
                        serde_json::from_str(s).unwrap_or(json!({"text": s}))
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

fn save_conv(secret: &str, cid: &str) {
    if !cid.is_empty() {
        let fp = format!("{:x}", {
            let mut h = Md5::new();
            h.update(secret.as_bytes());
            h.finalize()[..8].iter().fold(0u64, |a, b| (a << 8) | *b as u64)
        });
        conv_map().lock().unwrap().insert(fp, (cid.to_string(), now_secs()));
    }
}

/// 非流式聚合：返回 OpenAI chat.completion 形状（或 Err(status, body)）。
pub async fn chat(messages: &[Value], secret: &str, model: &str, timeout: u64) -> Result<Value, (i64, String)> {
    let url = format!("{}?{}", API, url_params(secret));
    let body = body_of(messages, model, secret).to_string().into_bytes();
    let mut req = super::out_client(false, timeout, false).post(&url);
    for (k, v) in headers(secret) {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.body(body).send().await.map_err(|e| (0, e.to_string()))?;
    let status = resp.status().as_u16() as i64;
    let raw = resp.text().await.map_err(|e| (0, e.to_string()))?;
    if !(200..300).contains(&status) {
        return Err((status, raw.chars().take(400).collect()));
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
        return Err((status, raw.chars().take(400).collect()));
    }
    save_conv(secret, &cid);
    let approx_in: usize = messages
        .iter()
        .map(|m| m.get("content").map(|c| c.to_string().len()).unwrap_or(0))
        .sum();
    let mut msg = json!({"role": "assistant", "content": full});
    let reasoning = thinking.trim().to_string();
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    Ok(json!({
        "id": format!("chatcmpl-{:024x}", nanos()),
        "object": "chat.completion",
        "created": nanos() / 1_000_000_000,
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

/// 流式：逐行解析上游 SSE → OpenAI chunk 帧（data: 前缀 + 空行，[DONE] 收尾）。
pub async fn chat_stream(
    messages: &[Value],
    secret: &str,
    model: &str,
    timeout: u64,
) -> Result<axum::response::Response, (i64, String)> {
    use axum::body::Body;
    use axum::http::{header, StatusCode};
    use axum::response::Response;

    let url = format!("{}?{}", API, url_params(secret));
    let body = body_of(messages, model, secret).to_string().into_bytes();
    let _ = timeout; // 流式：不设总时长（S3 四项分设）
    let mut req = super::out_client(true, 0, false).post(&url);
    for (k, v) in headers(secret) {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.body(body).send().await.map_err(|e| (0, e.to_string()))?;
    let status = resp.status();
    if !(200..300).contains(&status.as_u16()) {
        let t = resp.text().await.unwrap_or_default();
        return Err((status.as_u16() as i64, t.chars().take(400).collect()));
    }
    let chat_id = format!("chatcmpl-{:024x}", nanos());
    let model_owned = model.to_string();
    let mk_chunk = move |delta: Value, finish: Option<&str>| -> String {
        format!(
            "data: {}\n\n",
            json!({
                "id": chat_id, "object": "chat.completion.chunk",
                "created": nanos() / 1_000_000_000, "model": model_owned,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            })
        )
    };
    let mk_chunk = std::sync::Arc::new(mk_chunk);

    let cid_out = std::sync::Arc::new(Mutex::new(String::new()));
    let cid_out2 = cid_out.clone();
    let secret_owned = secret.to_string();
    let mk1 = mk_chunk.clone();
    let transform = move |state: &mut (String, bool), item: Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync>>| -> String {
        let (buf, sent_role) = state;
        let mut out = String::new();
        match item {
            Ok(b) => {
                buf.push_str(&String::from_utf8_lossy(&b));
                for line in buf.lines() {
                    let line = line.trim();
                    let Some(p) = sse_parse(line) else { continue };
                    let (t, th, c) = extract(&p);
                    if !*sent_role {
                        out.push_str(&mk1(json!({"role": "assistant", "content": ""}), None));
                        *sent_role = true;
                    }
                    if !th.is_empty() {
                        out.push_str(&mk1(json!({"reasoning_content": th}), None));
                    }
                    if !t.is_empty() {
                        out.push_str(&mk1(json!({"content": t}), None));
                    }
                    if !c.is_empty() {
                        *cid_out2.lock().unwrap() = c;
                    }
                }
                // 保留可能被截断的最后一行
                if let Some(pos) = buf.rfind('\n') {
                    buf.drain(..pos + 1);
                }
                out
            }
            Err(e) => format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"aigw_error": true, "message": format!("上游流中断：{}", e)})
            ),
        }
    };
    let stream = resp
        .bytes_stream()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        .scan((String::new(), false), move |state, item| {
            let out: Result<String, Box<dyn std::error::Error + Send + Sync>> = Ok(transform(state, item));
            std::future::ready(Some(out))
        });
    // 收尾：上游结束后补 finish+DONE（复用同一 chat_id）
    let cid_final = cid_out.clone();
    let mk2 = mk_chunk.clone();
    let stream = stream.chain(futures_util::stream::once(async move {
        let cid = cid_final.lock().unwrap().clone();
        save_conv(&secret_owned, &cid);
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

fn chat_id_stub() -> String {
    format!("chatcmpl-{:024x}", nanos())
}

#[allow(dead_code)]
fn unused_stub() {}

/// 验活：/passport/account/info/v2 返回 user_id 即有效（社区实测口径）。
pub async fn verify(secret: &str) -> bool {
    let url = "https://www.doubao.com/passport/account/info/v2?account_sdk_source=web&aid=497858";
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", UA)
        .header("Cookie", secret)
        .header("Content-Type", "application/json")
        .send()
        .await;
    match resp {
        Ok(r) => {
            let v: Value = serde_json::from_str(&r.text().await.unwrap_or_default()).unwrap_or(json!({}));
            v.pointer("/data/user_id").is_some_and(|x| !x.is_null())
                || v.get("user_id").is_some_and(|x| !x.is_null())
        }
        Err(_) => false,
    }
}
