//! 百度库库 AI（GenFlowPro / kuku.baidu.com）adapter——纯 HTTP，无需无头浏览器。
//!
//! 一手来源：xuzhang0807-cmd/kuku-cliproxy（Go，基于 IKUNHEIHIE/kuku2api 的
//! src/upstream.mjs，0BSD）。库库是百度文库的桌面客户端（GenFlowPro），对话走
//! wenchain 私有协议，凭据是浏览器 Cookie 里的 **BDUSS + STOKEN** 两条。
//!
//! 一轮对话的真实链路（非 OpenAI 透明）：
//! 1. `POST /wenchain/genflow/idallochstr`（body {channel:"genfp",gen_type:3}）
//!    连调两次：第一次取 chat_id（会话），第二次取 query_id（回复）；
//! 2. `POST /wenchain/genflowpro/sendmsg` 发外层 envelope（sub_type=chat_create）；
//! 3. `POST /wenchain/genflowpro/sse/getchatcontent` 开 SSE，逐帧：
//!    TEXT_BLOCK_DELTA=正文、THINKING_BLOCK_DELTA=思考、MODEL_CALL_END=usage/结束。
//!
//! 业务失败是 HTTP 200 + status.code/errno != 0，所以**不能只看 HTTP 状态码**。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use base64::Engine;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::mpsc;

const APP_ID: &str = "123971023";
const CLIENT_TYP: &str = "401";
const VERSION: &str = "1.6.5.130";
const KUKU_UA: &str = "genflow;1.6.5;PC;PC-Windows;10.0.26200;GenFlowPro";
const CLIENT_CHANNEL: &str = "kuku_pc_genflowpro_v1";
const PROTOCOL_V: &str = "cd1755270722192e4536f605";
const Q_CHANNEL: &str = "00000000000000000000000000000000";

const PATH_ID_ALLOC: &str = "/wenchain/genflow/idallochstr";
const PATH_PROBE: &str = "/wenchain/genflowpro/clientmessage/list";
const PATH_MODEL_LIST: &str = "/wenchain/genflowpro/model/list";
const PATH_SENDMSG: &str = "/wenchain/genflowpro/sendmsg";
const PATH_SSE: &str = "/wenchain/genflowpro/sse/getchatcontent";

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

/// 解析凭据：接受整段 Cookie（含 BDUSS=...; STOKEN=...）或 JSON {"bduss","stoken"}。
fn parse_cred(secret: &str) -> Result<(String, String), String> {
    let s = secret.trim();
    // JSON 形态
    if s.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(s) {
            let get = |k: &str| {
                v.get(k)
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let mut bduss = get("bduss");
            if bduss.is_empty() {
                bduss = get("BDUSS");
            }
            let mut stoken = get("stoken");
            if stoken.is_empty() {
                stoken = get("STOKEN");
            }
            if !bduss.is_empty() && !stoken.is_empty() {
                return Ok((bduss, stoken));
            }
        }
    }
    // Cookie 形态
    let mut bduss = String::new();
    let mut stoken = String::new();
    for part in s.split(';') {
        let p = part.trim();
        if let Some(v) = p.strip_prefix("BDUSS=").or_else(|| p.strip_prefix("BDUSS_BFESS=")) {
            if bduss.is_empty() {
                bduss = v.to_string();
            }
        } else if let Some(v) = p.strip_prefix("STOKEN=").or_else(|| p.strip_prefix("STOKEN_BFESS=")) {
            if stoken.is_empty() {
                stoken = v.to_string();
            }
        }
    }
    if bduss.is_empty() || stoken.is_empty() {
        return Err("库库凭据需包含 BDUSS 和 STOKEN 两条 Cookie（登录 kuku.baidu.com 后在 DevTools→Application→Cookies 复制）".into());
    }
    Ok((bduss, stoken))
}

/// 设备号：sha256("kuku-device:"+bduss) 前 8 字节大端 u64 % 10^10。
fn device_id(bduss: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("kuku-device:{}", bduss).as_bytes());
    let d = h.finalize();
    let n = u64::from_be_bytes(d[..8].try_into().unwrap_or([0u8; 8])) % 10_000_000_000;
    if n == 0 {
        "1".to_string()
    } else {
        n.to_string()
    }
}

fn logid() -> String {
    use rand::Rng;
    let unix = (nanos() / 1_000_000_000) as u64;
    let r: u64 = rand::thread_rng().gen_range(1000..90999);
    base64::engine::general_purpose::STANDARD.encode(format!("{},{}", unix, r))
}

/// 统一 query 参数；extra 追加/覆盖。
fn query(extra: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = vec![
        ("app_id".into(), APP_ID.into()),
        ("clienttype".into(), CLIENT_TYP.into()),
        ("channel".into(), Q_CHANNEL.into()),
        ("version".into(), VERSION.into()),
        ("logid".into(), logid()),
    ];
    for (k, v) in extra {
        pairs.push((k.to_string(), v.to_string()));
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlenc(k), urlenc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn headers(bduss: &str, stoken: &str) -> Vec<(String, String)> {
    vec![
        (
            "Cookie".into(),
            format!("BDUSS={}; STOKEN={}; gfprotpl=genflowpro", bduss, stoken),
        ),
        ("User-Agent".into(), KUKU_UA.into()),
        ("Accept".into(), "*/*".into()),
        ("Content-Type".into(), "application/json".into()),
    ]
}

/// 业务码检查：成功信封 status.code==0 或 errno==0；否则归类错误。
fn check_envelope(v: &Value) -> Result<Option<Value>, RelayError> {
    let (code, msg) = if let Some(st) = v.get("status") {
        (
            st.get("code").and_then(|x| x.as_i64()).unwrap_or(0),
            st.get("msg").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        )
    } else if let Some(e) = v.get("errno") {
        (
            e.as_i64().unwrap_or(0),
            v.get("show_msg").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        )
    } else {
        (0, String::new())
    };
    if code != 0 {
        let class = match code {
            1000004 | 200001 | -6 | -7 | -8 => ErrorClass::Auth,
            _ => ErrorClass::Transient,
        };
        return Err(er(502, format!("库库业务错误 code={} {}", code, msg), class));
    }
    Ok(v.get("data").cloned())
}

/// 分配 id：POST idallochstr。
async fn alloc_id(http: &reqwest::Client, base: &str, bduss: &str, stoken: &str) -> Result<Value, RelayError> {
    let url = format!("{}{}?{}", base, PATH_ID_ALLOC, query(&[]));
    let mut rb = http.post(&url);
    for (k, v) in headers(bduss, stoken) {
        rb = rb.header(k, v);
    }
    let resp = rb
        .json(&json!({"channel": "genfp", "gen_type": 3}))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let txt = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
    if !(200..300).contains(&status) {
        return Err(er(status, txt.chars().take(300).collect::<String>(), ErrorClass::from_status(status)));
    }
    check_envelope(&v)?.ok_or_else(|| er(502, "库库分配 id 无 data", ErrorClass::Transient))
}

fn text_block(text: &str) -> Value {
    json!({"id":"textId","version":"1.0","type":"text","text":text,"data":{"content":text}})
}

/// 组装提示词（system→instructions，多轮→CONVERSATION_JSON，单轮→原文）。
fn build_prompt(messages: &[Value]) -> Result<(String, String), RelayError> {
    let mut visible: Vec<Value> = Vec::new();
    let mut instructions: Vec<String> = Vec::new();
    let mut last_user = String::new();
    for m in messages {
        let text = msg_text(m);
        let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
        if (role == "system" || role == "developer") && !text.is_empty() {
            instructions.push(text.clone());
        }
        if role == "user" && !text.is_empty() {
            last_user = text.clone();
        }
        if !text.is_empty() {
            visible.push(json!({"role": role, "content": text}));
        }
    }
    if last_user.trim().is_empty() {
        return Err(er(400, "需要一条非空 user 消息", ErrorClass::Transient));
    }
    let text = if visible.len() > 1 {
        format!(
            "Answer the latest user message using this complete conversation and its system/developer instructions. Do not assume files exist on this computer.\nCONVERSATION_JSON: {}",
            serde_json::to_string(&visible).unwrap_or_default()
        )
    } else {
        last_user
    };
    Ok((text, instructions.join("\n\n")))
}

fn msg_text(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|p| {
                let t = p.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                if t == "text" || t == "input_text" || t == "output_text" {
                    p.get("text").and_then(|x| x.as_str()).map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn think_mode(openai: &Value) -> i64 {
    // reasoning_effort → 库库思考等级
    match openai
        .get("reasoning_effort")
        .and_then(|x| x.as_str())
        .unwrap_or("")
    {
        "minimal" | "low" => 1,
        "medium" => 2,
        "high" => 3,
        "xhigh" => 4,
        _ => 0,
    }
}

struct Turn {
    session_id: String,
    reply_id: String,
    client_session_id: String,
    device_id: String,
    text: String,
    model: String,
    think: i64,
    custom: String,
}

/// 准备一轮：解析凭据 → 分配两个 id → 组装。
async fn prepare(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<(Turn, String, String), RelayError> {
    let (bduss, stoken) = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = openai
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("auto")
        .trim()
        .to_string();
    if model.is_empty() {
        return Err(er(400, "kuku: 请求缺少 model", ErrorClass::Transient));
    }
    let messages = openai.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let (text, custom) = build_prompt(&messages)?;
    let think = think_mode(&openai);

    let s1 = alloc_id(http, &spec.base_url, &bduss, &stoken).await?;
    let s2 = alloc_id(http, &spec.base_url, &bduss, &stoken).await?;
    let session_id = s1.get("chat_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let reply_id = s2.get("query_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if session_id.is_empty() || reply_id.is_empty() {
        return Err(er(502, "库库上游未返回会话 id", ErrorClass::Transient));
    }
    Ok((
        Turn {
            session_id,
            reply_id,
            client_session_id: uuid_v4ish(),
            device_id: device_id(&bduss),
            text,
            model,
            think,
            custom,
        },
        bduss,
        stoken,
    ))
}

fn sendmsg_envelope(t: &Turn) -> Value {
    json!({
        "type": "message",
        "sub_type": "chat_create",
        "client_session_id": t.client_session_id,
        "session_id": t.session_id,
        "reply_id": t.reply_id,
        "uk": "",
        "cid": 0,
        "channel": CLIENT_CHANNEL,
        "device_type": CLIENT_TYP.parse::<i64>().unwrap_or(401),
        "created_at": (nanos() / 1_000_000) as u64,
        "msg_type": 1,
        "sync_type": 0,
        "sync_id": "",
        "v": PROTOCOL_V,
        "data": {
            "project_type": 1,
            "text": t.text,
            "rich_input_params": [text_block(&t.text)],
            "fsid": [],
            "quotes": [],
            "skills": [],
            "experts": [],
            "model_name": t.model,
            "model_display_name": t.model,
            "think_mode": t.think,
            "show_text": "",
            "device_id": t.device_id,
            "premake_data": {},
            "custom_instructions": t.custom,
            "memory_sign": false,
            "skill_dig_sign": false,
            "client_added": {
                "client_session_id": t.client_session_id,
                "permission_type": 0,
                "clienttype": CLIENT_TYP.parse::<i64>().unwrap_or(401),
                "version": VERSION,
                "device_os": "windows"
            }
        }
    })
}

fn sse_payload(t: &Turn) -> Value {
    json!({
        "project_type": 1,
        "text": t.text,
        "rich_input_params": [text_block(&t.text)],
        "fsid": [],
        "quotes": [],
        "skills": [],
        "experts": [],
        "model_name": t.model,
        "model_display_name": t.model,
        "think_mode": t.think,
        "show_text": "",
        "device_id": t.device_id,
        "premake_data": {},
        "session_id": t.session_id,
        "custom_instructions": t.custom,
        "memory_sign": false,
        "skill_dig_sign": false,
        "msg_type": 1,
        "reply_id": t.reply_id,
        "client_session_id": t.client_session_id,
        "traceparent": ""
    })
}

/// 解析一帧 SSE（event:/data:），返回 (event, data JSON)。
fn parse_block(block: &str) -> Option<(String, Value)> {
    let mut event = String::new();
    let mut data_lines: Vec<String> = Vec::new();
    for line in block.split('\n') {
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim().to_string());
        }
    }
    if data_lines.is_empty() {
        return None;
    }
    let joined = data_lines.join("\n");
    let v: Value = serde_json::from_str(&joined).ok()?;
    Some((event, v))
}

/// 帧 → (text, thinking, finish, usage)。
fn frame_delta(v: &Value) -> (String, String, Option<String>, Option<Value>) {
    let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    let inner = v.get("data").cloned().unwrap_or(json!({}));
    match typ {
        "TEXT_BLOCK_DELTA" => (inner.get("delta").and_then(|x| x.as_str()).unwrap_or("").to_string(), String::new(), None, None),
        "THINKING_BLOCK_DELTA" => (String::new(), inner.get("delta").and_then(|x| x.as_str()).unwrap_or("").to_string(), None, None),
        "MODEL_CALL_END" => {
            let it = inner.get("input_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
            let ot = inner.get("output_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
            let mut finish = inner
                .get("finished_reason")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if finish.is_empty() {
                if let Some(arr) = inner.get("finish_reasons").and_then(|x| x.as_array()) {
                    if let Some(f) = arr.first().and_then(|x| x.as_str()) {
                        finish = f.to_string();
                    }
                }
            }
            if finish.is_empty() || finish == "completed" {
                finish = "stop".to_string();
            }
            let usage = json!({
                "prompt_tokens": it,
                "completion_tokens": ot,
                "total_tokens": it + ot
            });
            (String::new(), String::new(), Some(finish), Some(usage))
        }
        _ => (String::new(), String::new(), None, None),
    }
}

/// 非流式：内部走 SSE 聚合。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let (t, bduss, stoken) = prepare(http, spec, secret, body).await?;
    // 1) sendmsg
    let send_url = format!("{}{}?{}", spec.base_url, PATH_SENDMSG, query(&[]));
    let mut rb = http.post(&send_url);
    for (k, v) in headers(&bduss, &stoken) {
        rb = rb.header(k, v);
    }
    let sr = rb
        .json(&sendmsg_envelope(&t))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let sv: Value = serde_json::from_str(&sr.text().await.unwrap_or_default()).unwrap_or_else(|_| json!({}));
    check_envelope(&sv)?;

    // 2) SSE
    let (text, thinking, usage, finish) = collect_sse(http, spec, &t, &bduss, &stoken).await?;
    if text.trim().is_empty() && thinking.trim().is_empty() {
        return Err(er(502, "库库上游未返回内容（Cookie/STOKEN 可能失效）", ErrorClass::Auth));
    }
    let mut msg = json!({"role":"assistant","content":text});
    if !thinking.trim().is_empty() {
        msg["reasoning_content"] = json!(thinking.trim());
    }
    Ok(json!({
        "id": format!("chatcmpl-{}", uuid_v4ish()),
        "object": "chat.completion",
        "created": (nanos()/1_000_000_000) as u64,
        "model": t.model,
        "choices":[{"index":0,"message":msg,"finish_reason": finish}],
        "usage": usage.unwrap_or_else(|| json!({"prompt_tokens":1,"completion_tokens":(text.len()/2).max(1),"total_tokens":(text.len()/2).max(2)})),
    }))
}

async fn collect_sse(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    t: &Turn,
    bduss: &str,
    stoken: &str,
) -> Result<(String, String, Option<Value>, String), RelayError> {
    let url = format!("{}{}?{}", spec.base_url, PATH_SSE, query(&[]));
    let mut rb = http.post(&url);
    for (k, v) in headers(bduss, stoken) {
        rb = rb.header(k, v);
    }
    let resp = rb
        .json(&sse_payload(t))
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let tx = resp.text().await.unwrap_or_default();
        return Err(er(status, tx.chars().take(300).collect::<String>(), ErrorClass::from_status(status)));
    }
    let mut text = String::new();
    let mut thinking = String::new();
    let mut usage = None;
    let mut finish = "stop".to_string();
    let mut completed = false;
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        let b = item.map_err(|e| er(0, format!("上游流中断：{e}"), ErrorClass::Transient))?;
        buf.push_str(&String::from_utf8_lossy(&b));
        while let Some(pos) = buf.find("\n\n") {
            let block: String = buf.drain(..pos + 2).collect();
            let Some((event, v)) = parse_block(&block) else { continue };
            let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
            if event == "done" || typ == "TURN_DONE" || typ == "FINISH" {
                completed = true;
            }
            // 商业/错误帧
            if typ == "COMMERCIAL" || typ == "COMMERCIAL_NOTICE" {
                let reason = v.get("data").and_then(|d| d.get("reason")).and_then(|x| x.as_str()).unwrap_or("");
                let errno = v.get("data").and_then(|d| d.get("errno")).and_then(|x| x.as_i64()).unwrap_or(0);
                if errno == 11002 || reason == "insufficient_balance" {
                    return Err(er(402, "库库账号积分不足", ErrorClass::Quota));
                }
            }
            if typ == "ERROR" || typ == "STREAM_ERROR" {
                return Err(er(502, "库库上游对话生成失败", ErrorClass::Transient));
            }
            let (tx, th, fin, us) = frame_delta(&v);
            text.push_str(&tx);
            thinking.push_str(&th);
            if let Some(f) = fin {
                finish = f;
            }
            if us.is_some() {
                usage = us;
            }
        }
    }
    if !completed {
        // 流结束但无终止帧，仍返回已收到的内容（部分客户端可接受）
    }
    Ok((text, thinking, usage, finish))
}

/// 流式：库库 SSE → OpenAI chunk 帧。
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
    let (t, bduss, stoken) = prepare(http, spec, secret, body).await?;
    let send_url = format!("{}{}?{}", spec.base_url, PATH_SENDMSG, query(&[]));
    let mut rb = http.post(&send_url);
    for (k, v) in headers(&bduss, &stoken) {
        rb = rb.header(k, v);
    }
    let sr = rb
        .json(&sendmsg_envelope(&t))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let sv: Value = serde_json::from_str(&sr.text().await.unwrap_or_default()).unwrap_or_else(|_| json!({}));
    check_envelope(&sv)?;

    let sse_url = format!("{}{}?{}", spec.base_url, PATH_SSE, query(&[]));
    let mut rb2 = http.post(&sse_url);
    for (k, v) in headers(&bduss, &stoken) {
        rb2 = rb2.header(k, v);
    }
    let resp = rb2
        .json(&sse_payload(&t))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let tx = resp.text().await.unwrap_or_default();
        return Err(er(status, tx.chars().take(400).collect::<String>(), ErrorClass::from_status(status)));
    }

    let (tx, rx) = mpsc::channel::<StreamStat>();
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let model = t.model.clone();
    tokio::spawn(async move {
        let chat_id = format!("chatcmpl-{}", uuid_v4ish());
        let mk = |delta: Value, finish: Option<&str>| -> String {
            format!(
                "data: {}\n\n",
                json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": (nanos()/1_000_000_000) as u64,
                    "model": model,
                    "choices":[{"index":0,"delta":delta,"finish_reason":finish}],
                })
            )
        };
        let mut stat = StreamStat::default();
        let mut buf = String::new();
        let mut sent_role = false;
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
            buf.push_str(&String::from_utf8_lossy(&chunk));
            let mut out = String::new();
            while let Some(pos) = buf.find("\n\n") {
                let block: String = buf.drain(..pos + 2).collect();
                let Some((event, v)) = parse_block(&block) else { continue };
                let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
                if typ == "COMMERCIAL" || typ == "COMMERCIAL_NOTICE" {
                    let reason = v.get("data").and_then(|d| d.get("reason")).and_then(|x| x.as_str()).unwrap_or("");
                    let errno = v.get("data").and_then(|d| d.get("errno")).and_then(|x| x.as_i64()).unwrap_or(0);
                    if errno == 11002 || reason == "insufficient_balance" {
                        stat.error = Some(ErrorClass::Quota);
                        stat.error_msg = "库库账号积分不足（BDUSS/STOKEN 对应账号积分耗尽）".into();
                    }
                }
                if typ == "ERROR" || typ == "STREAM_ERROR" {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = "库库上游对话生成失败（Cookie 可能失效或服务异常）".into();
                    break 'outer;
                }
                let (ctx, cth, _fin, _us) = frame_delta(&v);
                if (!ctx.is_empty() || !cth.is_empty()) && !sent_role {
                    out.push_str(&mk(json!({"role":"assistant","content":""}), None));
                    sent_role = true;
                }
                if !cth.is_empty() {
                    out.push_str(&mk(json!({"reasoning_content": cth}), None));
                }
                if !ctx.is_empty() {
                    out.push_str(&mk(json!({"content": ctx}), None));
                }
                let _ = event;
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break 'outer;
            }
        }
        let mut tail = String::new();
        // 无内容兜底：流里无正文（BDUSS/STOKEN 失效 / 积分不足 / 未登录）
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "库库未返回正文（BDUSS/STOKEN 可能失效，请重新登录 kuku.baidu.com 复制含 BDUSS/STOKEN 的整段 Cookie；或账号积分不足）".into();
        }
        if let Some(ec) = stat.error {
            // HTTP 头已 200（库库 SSE 建连即 200），用 OpenAI 流内 error 帧透传，避免空响应
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
        let _ = tx.send(stat);
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    Ok((Box::pin(stream), rx))
}

/// 验活：POST clientmessage/list（未登录返回 errno -6）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok((bduss, stoken)) = parse_cred(secret) else {
        return false;
    };
    let url = format!("{}{}?{}", spec.base_url, PATH_PROBE, query(&[]));
    let mut rb = http.post(&url);
    for (k, v) in headers(&bduss, &stoken) {
        rb = rb.header(k, v);
    }
    match rb.json(&json!({})).timeout(std::time::Duration::from_secs(15)).send().await {
        Ok(r) => {
            let txt = r.text().await.unwrap_or_default();
            let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
            // errno -6/-7/-8 或 status.code 1000004/200001 即未登录
            let code = if let Some(st) = v.get("status") {
                st.get("code").and_then(|x| x.as_i64()).unwrap_or(0)
            } else {
                v.get("errno").and_then(|x| x.as_i64()).unwrap_or(0)
            };
            !matches!(code, -6 | -7 | -8 | 1000004 | 200001)
        }
        Err(_) => false,
    }
}

/// 拉取 live 模型清单：GET model/list?channel=genfp → data.model_list[].model_name。
pub async fn fetch_models(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> Result<Vec<String>, RelayError> {
    let (bduss, stoken) = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let url = format!("{}{}?{}", spec.base_url, PATH_MODEL_LIST, query(&[("channel", "genfp")]));
    let mut rb = http.get(&url);
    for (k, v) in headers(&bduss, &stoken) {
        rb = rb.header(k, v);
    }
    let resp = rb
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let v: Value = resp.json().await.unwrap_or_else(|_| json!({}));
    let data = check_envelope(&v)?;
    let mut out = Vec::new();
    if let Some(list) = data.and_then(|d| d.get("model_list").cloned()).and_then(|x| x.as_array().cloned()) {
        for item in list {
            if let Some(name) = item.get("model_name").and_then(|x| x.as_str()) {
                if !name.trim().is_empty() {
                    out.push(name.trim().to_string());
                }
            }
        }
    }
    Ok(out)
}
