//! 秘塔 AI 搜索（metaso.cn）adapter——纯 HTTP、无浏览器依赖。
//!
//! 一手来源：rsfree/metaso（Python，docs/UPSTREAM.md 契约，2026-09 实测）。
//! 旧 LLM-Red-Team/metaso-free-api 的对话 searchV2 依赖 puppeteer CDP 拦截，
//! 本项目铁律禁浏览器；rsfree 版证实 `/api/search/chat` 只需：
//! 1. `GET /` 首页 HTML 正则提取 `<meta id="meta-token" content="...">`；
//! 2. `GET /api/my-info` 预热会话（免登录 errCode=401 属正常）；
//! 3. 门票层只校验指纹 cookie（tid / _c_WBKFRo / _nb_ioWEgULi）**存在性**，
//!    随机同形值即可；登录态再带 uid + sid。
//! 4. `POST /api/search/chat`（SSE），内容帧本就是 OpenAI choices[].delta 形态。
//!
//! 额度：匿名 ~13 发/出口 IP/窗口；登录（uid+sid）500/天。
//! 凭据：留空可匿名使用；填 `uid-sid`（旧版格式）或整段含 uid=/sid= 的 Cookie 即登录。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use rand::Rng;
use serde_json::{json, Value};
use std::sync::mpsc;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

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

fn rand_alnum(n: usize) -> String {
    let mut rng = rand::thread_rng();
    const C: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..n).map(|_| C[rng.gen_range(0..C.len())] as char).collect()
}

fn temp_uuid() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    hex16(&b)
}

fn hex16(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// 模型 → (mode, engineType)。
fn route(model: &str) -> (&'static str, &'static str) {
    match model {
        "metaso:concise" | "metaso-concise" | "concise" => ("concise", ""),
        "metaso:research" | "metaso-research" | "research" => ("research", ""),
        "metaso:scholar" | "metaso-scholar" | "scholar" => ("detail", "scholar"),
        "metaso:video" | "metaso-video" | "video" => ("detail", "video"),
        "metaso:deepresearch" | "deepresearch" => ("strong-research", ""),
        // metaso:search / metaso / 默认 → 深入
        _ => ("detail", ""),
    }
}

/// 解析用户凭据，返回登录 cookie 片段（uid/sid），匿名时为空。
fn login_cookie(secret: &str) -> String {
    let s = secret.trim();
    if s.is_empty() {
        return String::new();
    }
    // 整段 Cookie 且含 uid/sid
    if s.contains("uid=") && s.contains("sid=") {
        let mut uid = String::new();
        let mut sid = String::new();
        for part in s.split(';') {
            let p = part.trim();
            if let Some(v) = p.strip_prefix("uid=") {
                uid = v.to_string();
            } else if let Some(v) = p.strip_prefix("sid=") {
                sid = v.to_string();
            }
        }
        if !uid.is_empty() && !sid.is_empty() {
            return format!("uid={}; sid={};", uid, sid);
        }
    }
    // uid-sid 形态
    if !s.contains(' ') && !s.contains('=') && s.contains('-') {
        let (u, sd) = s.split_once('-').unwrap_or(("", ""));
        if !u.is_empty() && !sd.is_empty() {
            return format!("uid={}; sid={};", u, sd);
        }
    }
    String::new()
}

/// 组装完整 Cookie：门票三件套（随机）+ 登录对（若有）。
fn build_cookie(secret: &str) -> String {
    let login = login_cookie(secret);
    format!(
        "tid={}; _c_WBKFRo={}; _nb_ioWEgULi=; {}",
        rand_alnum(36),
        rand_alnum(40),
        login
    )
}

fn base_headers(cookie: &str) -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), UA.into()),
        ("Accept-Language".into(), "zh-CN".into()),
        ("Cookie".into(), cookie.to_string()),
    ]
}

/// 从首页 HTML 提取 meta-token（字符串查找，不依赖 regex crate）。
fn extract_meta_token(html: &str) -> Option<String> {
    let key = "meta-token";
    let idx = html.find(key)?;
    let rest = &html[idx..];
    let ci = rest.find("content=\"")?;
    let start = ci + "content=\"".len();
    let tail = &rest[start..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

struct Meta {
    token: String,
    cookie: String,
}

/// 获取 meta-token（GET 首页 + 预热 my-info）。
async fn fetch_meta(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> Result<Meta, RelayError> {
    let cookie = build_cookie(secret);
    let mut hb = http.get(format!("{}/", spec.base_url));
    for (k, v) in base_headers(&cookie) {
        hb = hb.header(k, v);
    }
    let home = hb
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(0, format!("秘塔首页请求失败: {e}"), ErrorClass::Transient))?;
    let status = home.status().as_u16();
    let html = home.text().await.unwrap_or_default();
    if status != 200 {
        return Err(er(status, "秘塔首页返回非 200（可能被 WAF 拦截）", ErrorClass::Transient));
    }
    let token = extract_meta_token(&html)
        .ok_or_else(|| er(502, "秘塔首页未找到 meta-token（站点改版或验证页）", ErrorClass::Transient))?;
    // 预热会话（免登录 401 正常，忽略结果）
    let mut ib = http.get(format!("{}/api/my-info", spec.base_url));
    for (k, v) in base_headers(&cookie) {
        ib = ib.header(k, v);
    }
    let _ = ib.timeout(std::time::Duration::from_secs(20)).send().await;
    Ok(Meta { token, cookie })
}

fn build_chat_body(query: &str, token: &str, mode: &str, engine: &str) -> Value {
    let temp = format!("temp-{}", temp_uuid());
    let msg = json!({
        "id": temp,
        "key": format!("temp-{}", temp_uuid()),
        "conversationId": format!("temp-{}", temp_uuid()),
        "role": "user",
        "content": query,
        "markdownContent": query,
        "engineType": engine,
        "filter": "all",
        "contentType": 0,
        "outputHtml": false,
        "mode": mode,
        "model": null,
        "outputStyle": "正常"
    });
    json!({
        "model": null,
        "stream": true,
        "messages": [msg],
        "engineType": engine,
        "mode": mode,
        "filter": "all",
        "outputHtml": false,
        "outputStyle": "正常",
        "darkMode": false,
        "outputLanguage": "中文",
        "htmlNoDisplayEnable": true,
        "displayContent": query,
        "metaso-pc": "pc",
        "token": token
    })
}

/// 把多轮 messages 拼成单条 query（秘塔是搜索型，首轮单轮）。
fn build_query(messages: &[Value]) -> String {
    let mut sys = String::new();
    let mut turns: Vec<String> = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
        let text = match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|x| x.as_str()))
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        if text.trim().is_empty() {
            continue;
        }
        if role == "system" || role == "developer" {
            sys.push_str(&text);
            sys.push('\n');
        } else {
            turns.push(text);
        }
    }
    let q = turns.pop().unwrap_or_default();
    if sys.trim().is_empty() {
        q
    } else {
        format!("{}\n\n{}", sys.trim(), q)
    }
}

/// 解析一帧秘塔 SSE JSON：返回 (content, reasoning, citations, error)。
fn parse_frame(v: &Value) -> Result<(String, String, Option<Value>), Option<RelayError>> {
    // 控制帧
    if let Some(typ) = v.get("type").and_then(|x| x.as_str()) {
        match typ {
            "heartbeat" => return Ok((String::new(), String::new(), None)),
            "error" => {
                let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
                let msg = v.get("msg").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let class = match code {
                    429 => ErrorClass::Rate,
                    401 | 403 => ErrorClass::Auth,
                    4001 => ErrorClass::Quota,
                    _ => ErrorClass::Transient,
                };
                return Err(Some(er(502, format!("秘塔错误 code={} {}", code, msg), class)));
            }
            _ => return Ok((String::new(), String::new(), None)), // conversation_init 等
        }
    }
    // 内容帧：OpenAI choices 形态
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut citations = None;
    if let Some(choices) = v.get("choices").and_then(|x| x.as_array()) {
        for ch in choices {
            if let Some(delta) = ch.get("delta") {
                if let Some(c) = delta.get("content").and_then(|x| x.as_str()) {
                    content.push_str(c);
                }
                if let Some(r) = delta.get("reasoning_content").and_then(|x| x.as_str()) {
                    reasoning.push_str(r);
                }
                if let Some(cit) = delta.get("citations") {
                    if cit.is_array() && !cit.as_array().unwrap().is_empty() {
                        citations = Some(cit.clone());
                    }
                }
            }
        }
    }
    Ok((content, reasoning, citations))
}

fn post_chat(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    meta: &Meta,
    body: Value,
) -> reqwest::RequestBuilder {
    let url = format!("{}/api/search/chat", spec.base_url);
    let mut rb = http
        .post(&url)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json")
        .header("metaso-pc", "pc")
        .header("Origin", spec.base_url.clone())
        .header("Referer", format!("{}/", spec.base_url));
    for (k, v) in base_headers(&meta.cookie) {
        rb = rb.header(k, v);
    }
    rb.json(&body)
}

/// 非流式：聚合 SSE。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = openai.get("model").and_then(|x| x.as_str()).unwrap_or("metaso:search").to_string();
    let (mode, engine) = route(&model);
    let messages = openai.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let query = build_query(&messages);
    if query.trim().is_empty() {
        return Err(er(400, "需要一条非空 user 消息", ErrorClass::Transient));
    }
    let meta = fetch_meta(http, spec, secret).await?;
    let resp = post_chat(http, spec, &meta, build_chat_body(&query, &meta.token, mode, engine))
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        let low = t.to_lowercase();
        if low.contains("html") || low.contains("captcha") || low.contains("验证") {
            return Err(er(status, "秘塔 WAF 人机验证（可在设置里配置代理后重试）", ErrorClass::Rate));
        }
        return Err(er(status, t.chars().take(300).collect::<String>(), ErrorClass::from_status(status)));
    }
    let mut text = String::new();
    let mut thinking = String::new();
    let mut citations = json!([]);
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    while let Some(item) = stream.next().await {
        let b = item.map_err(|e| er(0, format!("上游流中断: {e}"), ErrorClass::Transient))?;
        buf.push_str(&String::from_utf8_lossy(&b));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let raw = line.trim();
            if raw.is_empty() {
                continue;
            }
            let ds = raw.strip_prefix("data:").unwrap_or(raw).trim();
            if ds == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(ds) else { continue };
            match parse_frame(&v) {
                Ok((c, r, cit)) => {
                    text.push_str(&c);
                    thinking.push_str(&r);
                    if let Some(cv) = cit {
                        citations = cv;
                    }
                }
                Err(Some(e)) => return Err(e),
                Err(None) => {}
            }
        }
    }
    if text.trim().is_empty() {
        return Err(er(
            502,
            "秘塔未返回内容（匿名额度可能耗尽，约 13 发/窗口；可粘贴登录 uid-sid 获得 500/天，或配置代理）",
            ErrorClass::Quota,
        ));
    }
    let mut msg = json!({"role":"assistant","content":text});
    if !thinking.trim().is_empty() {
        msg["reasoning_content"] = json!(thinking.trim());
    }
    if citations.is_array() && !citations.as_array().unwrap().is_empty() {
        msg["citations"] = citations;
    }
    Ok(json!({
        "id": format!("chatcmpl-{}", temp_uuid()),
        "object": "chat.completion",
        "created": (nanos()/1_000_000_000) as u64,
        "model": model,
        "choices":[{"index":0,"message":msg,"finish_reason":"stop"}],
        "usage":{"prompt_tokens":1,"completion_tokens":(text.len()/2).max(1),"total_tokens":(text.len()/2).max(2)},
    }))
}

/// 流式：秘塔 SSE → OpenAI chunk。
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
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = openai.get("model").and_then(|x| x.as_str()).unwrap_or("metaso:search").to_string();
    let (mode, engine) = route(&model);
    let messages = openai.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    let query = build_query(&messages);
    if query.trim().is_empty() {
        return Err(er(400, "需要一条非空 user 消息", ErrorClass::Transient));
    }
    let meta = fetch_meta(http, spec, secret).await?;
    let resp = post_chat(http, spec, &meta, build_chat_body(&query, &meta.token, mode, engine))
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        return Err(er(status, t.chars().take(400).collect::<String>(), ErrorClass::from_status(status)));
    }

    let (tx, rx) = mpsc::channel::<StreamStat>();
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let model_owned = model.clone();
    tokio::spawn(async move {
        let chat_id = format!("chatcmpl-{}", temp_uuid());
        let mk = |delta: Value, finish: Option<&str>| -> String {
            format!(
                "data: {}\n\n",
                json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": (nanos()/1_000_000_000) as u64,
                    "model": model_owned,
                    "choices":[{"index":0,"delta":delta,"finish_reason":finish}],
                })
            )
        };
        let mut stat = StreamStat::default();
        let mut buf = String::new();
        let mut sent_role = false;
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
            buf.push_str(&String::from_utf8_lossy(&chunk));
            let mut out = String::new();
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let raw = line.trim();
                if raw.is_empty() {
                    continue;
                }
                let ds = raw.strip_prefix("data:").unwrap_or(raw).trim();
                if ds == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(ds) else { continue };
                match parse_frame(&v) {
                    Ok((c, r, _cit)) => {
                        if (!c.is_empty() || !r.is_empty()) && !sent_role {
                            out.push_str(&mk(json!({"role":"assistant","content":""}), None));
                            sent_role = true;
                        }
                        if !r.is_empty() {
                            out.push_str(&mk(json!({"reasoning_content": r}), None));
                        }
                        if !c.is_empty() {
                            got_content = true;
                            out.push_str(&mk(json!({"content": c}), None));
                        }
                    }
                    Err(Some(e)) => {
                        stat.error = Some(e.class);
                        stat.error_msg = e.body;
                        break 'outer;
                    }
                    Err(None) => {}
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break 'outer;
            }
        }
        let mut tail = String::new();
        // 无内容兜底：上游直接 DONE 且未显式报错时，判为额度耗尽
        if stat.error.is_none() && !got_content {
            stat.error = Some(ErrorClass::Quota);
            stat.error_msg = "秘塔未返回内容（匿名额度可能耗尽，约 13 发/窗口；可粘贴登录 uid-sid 获 500/天，或更换代理出口）".into();
        }
        if let Some(ec) = stat.error {
            // HTTP 头已 200（秘塔先发 init 帧才报 error），用 OpenAI 流内 error 帧透传，避免空响应
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

/// 验活：GET /api/my-info，errCode=0 为登录态；匿名（401）也算"服务可用"。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let cookie = build_cookie(secret);
    let mut rb = http.get(format!("{}/api/my-info", spec.base_url));
    for (k, v) in base_headers(&cookie) {
        rb = rb.header(k, v);
    }
    match rb.timeout(std::time::Duration::from_secs(15)).send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let v: Value = r.json().await.unwrap_or_else(|_| json!({}));
            // 登录：errCode=0；匿名：401/403 但服务可达
            if v.get("errCode").and_then(|x| x.as_i64()) == Some(0) {
                return true;
            }
            status == 401 || status == 403 || status == 200
        }
        Err(_) => false,
    }
}
