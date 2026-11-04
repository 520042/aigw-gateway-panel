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
use crate::common::{er};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use rand::Rng;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";


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
    // uid-sid 形态（扫码路径的产出：auth_login 里是 format!("{uid}-{sid}")）
    //
    // ★ 必须从**右边**切：uid 与 sid 都可能自带 '-'（实测秘塔的 uid 是
    // `u-<hex>` 这种形状）。用 split_once 从左切会把 "u-123-s-456" 解成
    // uid="u" + sid="123-s-456"，转发时带的是半个 uid —— 上游按匿名处理，
    // 额度从登录档掉回窗口档，而且不报任何错，极难被发现。
    if !s.contains(' ') && !s.contains('=') && s.contains('-') {
        let (u, sd) = s.rsplit_once('-').unwrap_or(("", ""));
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
///
/// ★ 必须在**整个标签**里找 `content="`，不能从 `meta-token` 出现的位置往后找：
/// 属性顺序在 HTML 里不固定，`<meta content="X" name="meta-token">` 是合法写法，
/// 旧实现从 key 处向后搜 content 会直接漏掉这种排列，表现为「首页未找到 meta-token」，
/// 整条车道用不了。
fn extract_meta_token(html: &str) -> Option<String> {
    let key = "meta-token";
    let at = html.find(key)?;
    // 回退到该标签的开头、前进到它的结尾，把整个 <meta …> 圈出来。
    let tag_start = html[..at].rfind('<').unwrap_or(0);
    let tag_end = html[at..].find('>').map(|o| at + o).unwrap_or(html.len());
    let tag = &html[tag_start..tag_end];
    let ci = tag.find("content=\"")?;
    let tail = &tag[ci + "content=\"".len()..];
    let end = tail.find('"')?;
    let tok = tail[..end].to_string();
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
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
                let st = crate::status_for_class(class);
                return Err(Some(er(st, format!("秘塔错误 code={} {}", code, msg), class)));
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
                    if cit.as_array().is_some_and(|a| !a.is_empty()) {
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
    let mut dec = crate::Utf8Decoder::new();
        let b = item.map_err(|e| er(0, format!("上游流中断: {e}"), ErrorClass::Transient))?;
        {
            let text = dec.push(&b);
            buf.push_str(&text);
        }
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
    if citations.as_array().is_some_and(|a| !a.is_empty()) {
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
        .timeout(crate::NONSTREAM_TOTAL)
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
        let mut out_chars: usize = 0;
        let mut buf = String::new();
        let mut dec = crate::Utf8Decoder::new();
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
            let text = dec.push(&chunk);
            buf.push_str(&text);
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
                        out_chars += c.chars().count() + r.chars().count();
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
        let (tin, tout) = crate::estimate_stream_tokens(in_bytes, out_chars);
        stat.tokens_in = tin;
        stat.tokens_out = tout;
        let _ = tx.send(stat).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 凭据解析是扫码路径与手动粘贴路径的汇合点：微信扫码换回来的就是 "uid-sid"
    /// 这一种形态，手动粘贴则可能整段 Cookie。两种都必须落到同一条 cookie 片段，
    /// 否则扫码成功但转发仍然匿名（额度从 500/天 掉回 13/窗口），且没有任何报错。
    #[test]
    fn login_cookie_accepts_both_paste_shapes() {
        // uid-sid（扫码路径产出的形态，两侧都是不带 '-' 的 hex）
        assert_eq!(
            login_cookie("8f3a91c2e04b7d65-1a2b3c4d5e6f7089"),
            "uid=8f3a91c2e04b7d65; sid=1a2b3c4d5e6f7089;"
        );
        // uid 自带 '-' 时也要切对：秘塔的 uid 存在 `u-<hex>` 形状，
        // 从左切会把 uid 截成 "u"，转发就退化成匿名了。
        assert_eq!(login_cookie("u-8f3a91c2-1a2b3c4d"), "uid=u-8f3a91c2; sid=1a2b3c4d;");
        // 整段 Cookie（手动粘贴路径）
        assert_eq!(
            login_cookie("other=1; uid=U9; sid=S9; trailing=2"),
            "uid=U9; sid=S9;"
        );
        // 空 / 无凭据 → 匿名（空片段，不是错误）
        assert_eq!(login_cookie(""), "");
        assert_eq!(login_cookie("   "), "");
        // 既不是整段 Cookie 也不是 uid-sid → 匿名，不能拼出半个 cookie
        assert_eq!(login_cookie("garbage"), "");
        // 只有 uid 没有 sid → 匿名（半对凭据上游会 401，不如直说没有）
        assert_eq!(login_cookie("uid=only"), "");
    }

    /// 匿名 Cookie 也必须带齐门票三件套，且每次调用都不同（随机）。
    #[test]
    fn build_cookie_always_carries_ticket_triplet() {
        let a = build_cookie("");
        assert!(a.contains("tid="), "{a}");
        assert!(a.contains("_c_WBKFRo="), "{a}");
        assert!(a.contains("_nb_ioWEgULi="), "{a}");
        assert!(!a.contains("uid="), "匿名不该带 uid: {a}");
        // 两次调用随机值不同
        assert_ne!(a, build_cookie(""));
        // 有凭据时门票仍在
        let b = build_cookie("u1-s1");
        assert!(b.contains("uid=u1; sid=s1;"), "{b}");
        assert!(b.contains("tid="), "{b}");
    }

    /// 模型名 → (mode, engine) 的路由表。写错一项的后果是「选了 research
    /// 却按 detail 发」——不报错，只是结果不对，所以逐项钉住。
    #[test]
    fn route_maps_every_documented_alias() {
        assert_eq!(route("metaso:concise"), ("concise", ""));
        assert_eq!(route("concise"), ("concise", ""));
        assert_eq!(route("metaso:research"), ("research", ""));
        assert_eq!(route("metaso:scholar"), ("detail", "scholar"));
        assert_eq!(route("metaso:video"), ("detail", "video"));
        assert_eq!(route("metaso:deepresearch"), ("strong-research", ""));
        // 未知 / 默认 → 深入搜索
        assert_eq!(route("metaso:search"), ("detail", ""));
        assert_eq!(route("whatever"), ("detail", ""));
    }

    /// system 提示必须前置拼进 query，且多轮历史只取最后一轮作为提问 ——
    /// 秘塔的 query 是「一次搜索」，把整段历史塞进去会污染检索词。
    #[test]
    fn build_query_hoists_system_and_keeps_last_turn() {
        let msgs = vec![
            json!({"role": "system", "content": "你是搜索助手"}),
            json!({"role": "user", "content": "第一轮问题"}),
            json!({"role": "assistant", "content": "第一轮回答"}),
            json!({"role": "user", "content": "最后一问"}),
        ];
        let q = build_query(&msgs);
        assert!(q.starts_with("你是搜索助手"), "{q}");
        assert!(q.ends_with("最后一问"), "{q}");
        assert!(!q.contains("第一轮问题"), "历史提问不该进检索词: {q}");
        assert!(!q.contains("第一轮回答"), "历史回答不该进检索词: {q}");
    }

    /// 没有 system、或 system 只有空白时，query 就是纯提问，不能多出空行。
    #[test]
    fn build_query_without_system_is_just_the_question() {
        let msgs = vec![json!({"role": "user", "content": "问题"})];
        assert_eq!(build_query(&msgs), "问题");
        let msgs = vec![
            json!({"role": "system", "content": "   "}),
            json!({"role": "user", "content": "问题"}),
        ];
        assert_eq!(build_query(&msgs), "问题");
    }

    /// 内容帧解析：正文与思考走不同字段，citations 只在非空数组时才带出来。
    #[test]
    fn parse_frame_separates_content_reasoning_and_citations() {
        let f = json!({"choices": [{"delta": {
            "content": "正文",
            "reasoning_content": "思考",
            "citations": [{"title": "来源"}]
        }}]});
        let (c, r, cit) = parse_frame(&f).unwrap();
        assert_eq!(c, "正文");
        assert_eq!(r, "思考");
        assert!(cit.is_some(), "非空 citations 应当带出");

        // 空 citations 数组 → None（面板不该显示一个空来源列表）
        let f = json!({"choices": [{"delta": {"content": "x", "citations": []}}]});
        let (_, _, cit) = parse_frame(&f).unwrap();
        assert!(cit.is_none());
    }

    /// 控制帧：心跳与 conversation_init 都不产生内容，也不能当错误。
    #[test]
    fn parse_frame_ignores_control_frames() {
        for typ in ["heartbeat", "conversation_init", "anything_else"] {
            let f = json!({"type": typ});
            let (c, r, cit) = parse_frame(&f).unwrap();
            assert!(c.is_empty() && r.is_empty() && cit.is_none(), "帧 {typ} 应当静默");
        }
    }

    /// 错误帧按 code 分类：429 限速、401/403 凭据、4001 额度，其余按瞬时故障。
    /// 分类错了会让「等一下就好」被当成「重新登录」，或反过来。
    #[test]
    fn parse_frame_classifies_error_codes() {
        let cases = [
            (429, ErrorClass::Rate),
            (401, ErrorClass::Auth),
            (403, ErrorClass::Auth),
            (4001, ErrorClass::Quota),
            (5000, ErrorClass::Transient),
        ];
        for (code, want) in cases {
            let f = json!({"type": "error", "code": code, "msg": "boom"});
            let err = parse_frame(&f).unwrap_err().expect("错误帧必须带 RelayError");
            assert!(
                std::mem::discriminant(&err.class) == std::mem::discriminant(&want),
                "code={code} 分类错了: {:?}",
                err.class
            );
            assert!(err.body.contains("boom"), "错误原因要透传: {}", err.body);
        }
    }

    /// 从首页 HTML 提取 meta-token：属性顺序无关，缺了要明确返回 None。
    #[test]
    fn extract_meta_token_handles_attribute_order() {
        assert_eq!(
            extract_meta_token(r#"<meta name="meta-token" content="TOK123">"#).as_deref(),
            Some("TOK123")
        );
        assert_eq!(
            extract_meta_token(r#"<meta content="TOK456" name="meta-token">"#).as_deref(),
            Some("TOK456")
        );
        // 没有该标签（改版 / 验证页）
        assert_eq!(extract_meta_token("<html><body>验证中</body></html>"), None);
        // 有 key 但没有 content 属性
        assert_eq!(extract_meta_token(r#"<meta name="meta-token">"#), None);
    }
}
