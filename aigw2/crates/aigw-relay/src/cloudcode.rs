//! Antigravity / Google Cloud Code（Gemini 协议）adapter —— 纯 HTTP，OAuth 凭据。
//!
//! 一手来源：CLIProxyAPI internal/runtime/executor/antigravity_executor_request.go 的 buildRequest /
//! geminiToAntigravity，以及 internal/auth/antigravity/auth.go 的 FetchProjectID。该平台没有 OpenAI 兼容端点：
//! 1. 项目号：POST {cloudcode-pa}/v1internal:loadCodeAssist body {"metadata":{"ideType":"ANTIGRAVITY"}}
//!    取 cloudaicompanionProject（字符串或 {id}）；缺失时再 POST daily /v1internal:onboardUser 轮询。
//! 2. 生成：POST {daily-cloudcode-pa}/v1internal:streamGenerateContent?alt=sse，body 是 Gemini 请求的外壳
//!    {model, userAgent:"antigravity", requestType:"agent", project, requestId:"agent-<uuid>",
//!     request:{sessionId, contents, systemInstruction, generationConfig}}。
//! 3. 响应是 SSE，data 里的 JSON 可能再包一层 "response"；正文在 candidates[0].content.parts[].text，
//!    parts[].thought=true 是思考，usageMetadata.promptTokenCount / candidatesTokenCount 是用量。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const CC_UA: &str = "antigravity/hub/2.9.1 darwin/arm64";
const CC_NODE_UA: &str = "google-api-nodejs-client/10.3.0";
const CC_GOOG_API_CLIENT: &str = "gl-node/22.21.1";
const CC_VERSION: &str = "2.9.1";
const CC_PROD: &str = "https://cloudcode-pa.googleapis.com";
const CC_DAILY: &str = "https://daily-cloudcode-pa.googleapis.com";

fn er(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError { status, body: body.into(), class }
}

fn secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push_str("…");
        out
    }
}

fn uuid_like() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut b = [0u8; 16];
    rng.fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

fn relay_status_class(status: u16) -> ErrorClass {
    match status {
        401 | 403 => ErrorClass::Auth,
        402 => ErrorClass::Quota,
        429 => ErrorClass::Rate,
        400 => ErrorClass::Rejected,
        _ => ErrorClass::Transient,
    }
}

fn pick_one(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Antigravity 凭据：OAuth access token（可选带 GCP project id）。
pub struct CcCred {
    pub token: String,
    pub project: Option<String>,
}

/// 解析凭据：纯 access token，或 {"access_token":…,"project_id":…} 形态的 JSON。
pub fn parse_cred(secret: &str) -> Result<CcCred, String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("Antigravity 凭据为空：请粘贴 OAuth access token，或 {\"access_token\":\"…\",\"project_id\":\"…\"}".to_string());
    }
    if s.starts_with("{") {
        let v: Value = serde_json::from_str(s).map_err(|e| format!("Antigravity 凭据不是合法 JSON：{e}"))?;
        let token = pick_one(&v, &["access_token", "accessToken", "token"])
            .ok_or_else(|| "Antigravity 凭据 JSON 里没有 access_token".to_string())?;
        let project = pick_one(&v, &["project_id", "projectId", "project", "cloudaicompanionProject"]);
        return Ok(CcCred { token, project });
    }
    Ok(CcCred { token: s.to_string(), project: None })
}

fn custom_base(spec: &ProviderSpec) -> Option<String> {
    let b = spec.base_url.trim();
    if b.is_empty() || b.contains("googleapis.com") {
        None
    } else {
        Some(b.trim_end_matches("/").to_string())
    }
}

fn gen_base(spec: &ProviderSpec) -> String {
    custom_base(spec).unwrap_or_else(|| CC_DAILY.to_string())
}

fn control_base(spec: &ProviderSpec) -> String {
    custom_base(spec).unwrap_or_else(|| CC_PROD.to_string())
}

fn headers(spec: &ProviderSpec, token: &str) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("authorization".to_string(), format!("Bearer {token}")),
        ("content-type".to_string(), "application/json".to_string()),
        ("accept".to_string(), "*/*".to_string()),
        ("user-agent".to_string(), CC_UA.to_string()),
        ("x-goog-api-client".to_string(), CC_GOOG_API_CLIENT.to_string()),
    ];
    for (k, v) in &spec.headers {
        if !hs.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(k)) {
            hs.push((k.clone(), v.clone()));
        }
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

/// 从 loadCodeAssist / onboardUser 响应里取项目号（顶层或 response 包裹，字符串或 {id}）。
pub fn extract_project(v: &Value) -> Option<String> {
    let root = match v.get("response") {
        Some(r) if r.is_object() => r,
        _ => v,
    };
    for node in [root, v] {
        for k in ["cloudaicompanionProject", "projectId", "project"] {
            match node.get(k) {
                Some(Value::String(s)) => {
                    let t = s.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
                Some(Value::Object(o)) => {
                    if let Some(id) = o.get("id").and_then(|x| x.as_str()) {
                        let t = id.trim();
                        if !t.is_empty() {
                            return Some(t.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

async fn onboard_project(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &CcCred,
    tier: &str,
) -> Result<String, RelayError> {
    let url = format!("{}/v1internal:onboardUser", gen_base(spec));
    let body = json!({
        "tier_id": tier,
        "metadata": {"ide_type": "ANTIGRAVITY", "ide_version": CC_VERSION, "ide_name": "antigravity"}
    });
    let ua = format!("{} {}", CC_UA, CC_NODE_UA);
    let mut last = String::new();
    for attempt in 1..=5u32 {
        let resp = http
            .post(&url)
            .header("authorization", format!("Bearer {}", cred.token))
            .header("content-type", "application/json")
            .header("accept", "*/*")
            .header("user-agent", &ua)
            .header("x-goog-api-client", CC_GOOG_API_CLIENT)
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| er(502, format!("Antigravity onboardUser 请求失败：{e}"), ErrorClass::Transient))?;
        let status = resp.status().as_u16();
        let txt = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(er(
                status,
                format!("Antigravity onboardUser 失败（{status}）：{}", truncate(&txt, 300)),
                relay_status_class(status),
            ));
        }
        let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
        if let Some(p) = extract_project(&v) {
            return Ok(p);
        }
        last = truncate(&txt, 300);
        if attempt < 5 {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
    }
    Err(er(
        401,
        format!(
            "Antigravity 取不到 GCP 项目号：loadCodeAssist 与 onboardUser 都没返回 cloudaicompanionProject。请在凭据 JSON 里带上 project_id，或确认该 Google 账号已开通 Antigravity。原始响应：{last}"
        ),
        ErrorClass::Auth,
    ))
}

/// 取项目号：凭据里有就直接用，否则 loadCodeAssist 自举（必要时 onboardUser）。
pub async fn ensure_project(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &CcCred,
) -> Result<String, RelayError> {
    if let Some(p) = &cred.project {
        let t = p.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    let url = format!("{}/v1internal:loadCodeAssist", control_base(spec));
    let resp = http
        .post(&url)
        .header("authorization", format!("Bearer {}", cred.token))
        .header("content-type", "application/json")
        .header("accept", "*/*")
        .header("user-agent", CC_UA)
        .json(&json!({"metadata": {"ideType": "ANTIGRAVITY"}}))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| er(502, format!("Antigravity loadCodeAssist 请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let txt = resp.text().await.unwrap_or_default();
    if status != 200 {
        let class = if status == 401 || status == 403 { ErrorClass::Auth } else { relay_status_class(status) };
        let code = if status == 401 || status == 403 { 401 } else { status };
        return Err(er(
            code,
            format!("Antigravity 鉴权失败（{status}）：{}", truncate(&txt, 300)),
            class,
        ));
    }
    let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
    if let Some(p) = extract_project(&v) {
        return Ok(p);
    }
    let tier = v
        .get("allowedTiers")
        .and_then(|x| x.as_array())
        .and_then(|a| {
            a.iter()
                .find(|t| t.get("isDefault").and_then(|b| b.as_bool()) == Some(true))
                .or_else(|| a.first())
        })
        .and_then(|t| t.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or("free-tier")
        .to_string();
    onboard_project(http, spec, cred, &tier).await
}

fn content_text(c: Option<&Value>) -> String {
    let c = match c {
        Some(v) => v,
        None => return String::new(),
    };
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(arr) = c.as_array() {
        for b in arr {
            if b.get("type").and_then(|x| x.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                    out.push_str(t);
                }
            }
        }
    }
    out
}

fn system_text(v: &Value) -> String {
    if let Some(s) = v.get("system").and_then(|x| x.as_str()) {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Some(arr) = v.get("system").and_then(|x| x.as_array()) {
        let mut out = String::new();
        for b in arr {
            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                out.push_str(t);
            }
        }
        if !out.trim().is_empty() {
            return out;
        }
    }
    let mut out = String::new();
    if let Some(msgs) = v.get("messages").and_then(|x| x.as_array()) {
        for m in msgs {
            let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("");
            if role == "system" || role == "developer" {
                let t = content_text(m.get("content"));
                if !t.trim().is_empty() {
                    out.push_str(&t);
                    out.push_str("\n");
                }
            }
        }
    }
    out.trim().to_string()
}

/// OpenAI messages -> Gemini contents（连续同角色合并成多个 part）。
pub fn gemini_contents(v: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let msgs = match v.get("messages").and_then(|x| x.as_array()) {
        Some(a) => a,
        None => return out,
    };
    for m in msgs {
        let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
        if role == "system" || role == "developer" {
            continue;
        }
        let text = content_text(m.get("content"));
        if text.trim().is_empty() {
            continue;
        }
        let grole = if role == "assistant" { "model" } else { "user" };
        let same = match out.last() {
            Some(last) => last.get("role").and_then(|x| x.as_str()) == Some(grole),
            None => false,
        };
        if same {
            if let Some(parts) = out
                .last_mut()
                .and_then(|l| l.get_mut("parts"))
                .and_then(|p| p.as_array_mut())
            {
                parts.push(json!({"text": text}));
            }
        } else {
            out.push(json!({"role": grole, "parts": [{"text": text}]}));
        }
    }
    out
}

/// OpenAI 参数 -> Gemini generationConfig（只翻译常用的几个）。
pub fn generation_config(v: &Value) -> Option<Value> {
    let mut cfg = serde_json::Map::new();
    let max = v
        .get("max_tokens")
        .and_then(|x| x.as_i64())
        .or_else(|| v.get("max_completion_tokens").and_then(|x| x.as_i64()));
    if let Some(n) = max {
        if n > 0 {
            cfg.insert("maxOutputTokens".to_string(), json!(n));
        }
    }
    if let Some(t) = v.get("temperature").and_then(|x| x.as_f64()) {
        cfg.insert("temperature".to_string(), json!(t));
    }
    if let Some(p) = v.get("top_p").and_then(|x| x.as_f64()) {
        cfg.insert("topP".to_string(), json!(p));
    }
    if let Some(a) = v.get("stop").and_then(|x| x.as_array()) {
        let seqs: Vec<Value> = a.iter().filter_map(|x| x.as_str().map(|s| json!(s))).collect();
        if !seqs.is_empty() {
            cfg.insert("stopSequences".to_string(), json!(seqs));
        }
    } else if let Some(s) = v.get("stop").and_then(|x| x.as_str()) {
        cfg.insert("stopSequences".to_string(), json!([s]));
    }
    if cfg.is_empty() {
        None
    } else {
        Some(Value::Object(cfg))
    }
}

fn stable_session_id(seed: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("-{}", h & 0x7fff_ffff_ffff_ffff)
}

/// 组 Antigravity 请求体（Gemini 请求套一层外壳）。
pub fn build_payload(v: &Value, model: &str, project: &str) -> Value {
    let contents = gemini_contents(v);
    let seed = v
        .get("messages")
        .and_then(|x| x.as_array())
        .and_then(|a| {
            a.iter()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        })
        .map(|m| content_text(m.get("content")))
        .unwrap_or_default();
    let mut req = serde_json::Map::new();
    let sys = system_text(v);
    if !sys.is_empty() {
        req.insert("systemInstruction".to_string(), json!({"parts": [{"text": sys}]}));
    }
    req.insert("sessionId".to_string(), json!(stable_session_id(&seed)));
    req.insert("contents".to_string(), json!(contents));
    if let Some(g) = generation_config(v) {
        req.insert("generationConfig".to_string(), g);
    }
    json!({
        "model": model,
        "userAgent": "antigravity",
        "requestType": "agent",
        "project": project,
        "requestId": format!("agent-{}", uuid_like()),
        "request": Value::Object(req),
    })
}

fn inner<'a>(v: &'a Value) -> &'a Value {
    match v.get("response") {
        Some(r) if r.is_object() => r,
        _ => v,
    }
}

/// 一帧里的正文与思考。
pub fn frame_parts(v: &Value) -> (String, String) {
    let mut text = String::new();
    let mut think = String::new();
    let parts = inner(v)
        .get("candidates")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array());
    if let Some(parts) = parts {
        for p in parts {
            let t = p.get("text").and_then(|x| x.as_str()).unwrap_or("");
            if t.is_empty() {
                continue;
            }
            if p.get("thought").and_then(|x| x.as_bool()) == Some(true) {
                think.push_str(t);
            } else {
                text.push_str(t);
            }
        }
    }
    (text, think)
}

pub fn frame_finish(v: &Value) -> Option<String> {
    let fr = inner(v)
        .get("candidates")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("finishReason"))
        .and_then(|x| x.as_str())?;
    let out = match fr {
        "MAX_TOKENS" => "length",
        _ => "stop",
    };
    Some(out.to_string())
}

pub fn frame_usage(v: &Value) -> Option<(i64, i64)> {
    let u = match inner(v).get("usageMetadata") {
        Some(u) => u,
        None => return None,
    };
    let tin = u.get("promptTokenCount").and_then(|x| x.as_i64()).unwrap_or(0);
    let tout = u.get("candidatesTokenCount").and_then(|x| x.as_i64()).unwrap_or(0);
    if tin == 0 && tout == 0 {
        None
    } else {
        Some((tin, tout))
    }
}

pub fn parse_data_line(block: &str) -> Option<&str> {
    for line in block.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("data:") {
            let d = rest.trim();
            if d.is_empty() || d == "[DONE]" {
                continue;
            }
            return Some(d);
        }
    }
    None
}

fn frame_error(v: &Value) -> Option<String> {
    let e = inner(v).get("error")?;
    let msg = e.get("message").and_then(|x| x.as_str()).unwrap_or("上游返回错误");
    Some(msg.to_string())
}

async fn open_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &CcCred,
    payload: &Value,
    timeout_secs: u64,
) -> Result<reqwest::Response, RelayError> {
    let project = payload.get("project").and_then(|x| x.as_str()).unwrap_or("");
    let _ = project;
    let url = format!("{}/v1internal:streamGenerateContent?alt=sse", gen_base(spec));
    let hs = headers(spec, &cred.token);
    let resp = apply(http.post(&url), &hs)
        .json(payload)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .send()
        .await
        .map_err(|e| er(502, format!("Antigravity 请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if status != 200 {
        let raw = resp.text().await.unwrap_or_default();
        return Err(er(
            status,
            format!("Antigravity 上游 {status}：{}", truncate(&raw, 300)),
            relay_status_class(status),
        ));
    }
    Ok(resp)
}

/// 非流式：mock 上游也走 SSE 接口，聚合成 OpenAI 的 chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("gemini-3-flash")
        .to_string();
    let project = ensure_project(http, spec, &cred).await?;
    let payload = build_payload(&v, &model, &project);
    let resp = open_stream(http, spec, &cred, &payload, 180).await?;
    let mut dec = crate::Utf8Decoder::new();
    let mut buf = String::new();
    let mut content = String::new();
    let mut think = String::new();
    let mut finish: Option<String> = None;
    let mut usage: Option<(i64, i64)> = None;
    let mut failure: Option<String> = None;
    let mut stream = resp.bytes_stream();
    'outer: while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| er(502, format!("Antigravity 流读取失败：{e}"), ErrorClass::Transient))?;
        buf.push_str(&dec.push(&chunk));
        while let Some((pos, sep)) = crate::find_frame_end(&buf) {
            let block: String = buf.drain(..pos + sep).collect();
            let Some(data) = parse_data_line(&block) else { continue };
            let Ok(fv) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(e) = frame_error(&fv) {
                failure = Some(e);
                break 'outer;
            }
            let (t, th) = frame_parts(&fv);
            content.push_str(&t);
            think.push_str(&th);
            if let Some(f) = frame_finish(&fv) {
                finish = Some(f);
            }
            if let Some(u) = frame_usage(&fv) {
                usage = Some(u);
            }
        }
    }
    if let Some(e) = failure {
        return Err(er(401, e, ErrorClass::Auth));
    }
    if content.trim().is_empty() {
        return Err(er(
            401,
            "Antigravity 未返回正文（access token 可能已过期，或该账号没有 Antigravity / Gemini Code Assist 权限）",
            ErrorClass::Auth,
        ));
    }
    let mut msg = json!({"role": "assistant", "content": content});
    if !think.trim().is_empty() {
        msg["reasoning_content"] = json!(think.trim());
    }
    let (tin, tout) = match usage {
        Some(u) => u,
        None => (
            (body.len() / 4).max(1) as i64,
            (content.chars().count() / 2).max(1) as i64,
        ),
    };
    Ok(json!({
        "id": format!("chatcmpl-{}", uuid_like()),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{"index": 0, "message": msg, "finish_reason": finish.unwrap_or_else(|| "stop".to_string())}],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout},
    }))
}

/// 流式：Gemini SSE 帧改写为 OpenAI chunk（content / reasoning_content + 尾部 [DONE]）。
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
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("gemini-3-flash")
        .to_string();
    let project = ensure_project(http, spec, &cred).await?;
    let payload = build_payload(&v, &model, &project);
    let resp = open_stream(http, spec, &cred, &payload, 300).await?;

    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let in_bytes = body.len();
    tokio::spawn(async move {
        let rid = format!("chatcmpl-{}", uuid_like());
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
        let mut sent_role = false;
        let mut done_sent = false;
        let mut dec = crate::Utf8Decoder::new();
        let mut buf = String::new();
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
                let Ok(fv) = serde_json::from_str::<Value>(data) else { continue };
                if let Some(e) = frame_error(&fv) {
                    stat.error = Some(ErrorClass::Auth);
                    stat.error_msg = e;
                    break 'outer;
                }
                let (t, th) = frame_parts(&fv);
                if th.is_empty() && t.is_empty() {
                    continue;
                }
                if !sent_role {
                    out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                    sent_role = true;
                }
                if !th.is_empty() {
                    out_chars += th.chars().count();
                    out.push_str(&mk(json!({"reasoning_content": th}), None));
                }
                if !t.is_empty() {
                    out_chars += t.chars().count();
                    out.push_str(&mk(json!({"content": t}), None));
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break;
            }
        }
        let mut tail = String::new();
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "Antigravity 未返回正文（access token 可能已过期）".into();
        }
        if let Some(ec) = stat.error {
            let etype = match ec {
                ErrorClass::Rate => "rate_limit_error",
                ErrorClass::Auth => "authentication_error",
                ErrorClass::Quota => "insufficient_quota",
                _ => "api_error",
            };
            let msg = stat.error_msg.replace("\"", "'");
            tail.push_str(&format!("data: {}\n\n", json!({"error": {"message": msg, "type": etype}})));
        } else {
            tail.push_str(&mk(json!({}), Some("stop")));
            stat.finish_reason = Some("stop".into());
            done_sent = true;
        }
        tail.push_str("data: [DONE]\n\n");
        let _ = done_sent;
        let _ = bytes_tx.send(Ok(Bytes::from(tail))).await;
        let (tin, tout) = crate::estimate_stream_tokens(in_bytes, out_chars);
        stat.tokens_in = tin;
        stat.tokens_out = tout;
        let _ = tx.send(stat).await;
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    Ok((Box::pin(stream), rx))
}

/// 验活：loadCodeAssist 2xx 且能取到项目号。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let cred = match parse_cred(secret) {
        Ok(c) => c,
        Err(_) => return false,
    };
    match ensure_project(http, spec, &cred).await {
        Ok(_) => true,
        Err(_) => false,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use aigw_registry::{AuthKind, ModelsPolicy, StreamDialect};
    use axum::response::IntoResponse;
    use axum::Router;

    fn spec_for(base: &str) -> ProviderSpec {
        ProviderSpec {
            id: "sub-antigravity".into(),
            name: "Antigravity".into(),
            enabled: true,
            auth: AuthKind::Cookie,
            login_methods: Vec::new(),
            base_url: base.to_string(),
            format: "openai".into(),
            models: vec!["gemini-3-flash".into()],
            models_policy: ModelsPolicy::Static,
            stream_dialect: StreamDialect::SseDelta,
            headers: std::collections::BTreeMap::new(),
            chat_path: String::new(),
            models_path: String::new(),
            models_filter: String::new(),
            upstream_kind: "cloudcode".into(),
            signer: String::new(),
            biz_err: vec![],
            hosts: vec![],
            login_url: String::new(),
            notes: String::new(),
            chat_supported: true,
            region: "global".into(),
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// mock 上游：loadCodeAssist 返回项目号，streamGenerateContent 把请求里的 project 回显成正文。
    fn sse_body(text: &str) -> axum::response::Response {
        axum::response::Response::builder()
            .status(200)
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from(text.to_string()))
            .unwrap()
    }

    async fn mock_full(uri: axum::http::Uri, body: axum::body::Bytes) -> axum::response::Response {
        let path = uri.path();
        if path.ends_with("loadCodeAssist") {
            return axum::Json(json!({"cloudaicompanionProject": "proj-1"})).into_response();
        }
        if path.ends_with("onboardUser") {
            return axum::Json(json!({"done": false})).into_response();
        }
        let v: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
        let proj = v.get("project").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let f1 = json!({"response": {"candidates": [{"content": {"parts": [{"text": format!("PROJ:{proj}")}]}}]}});
        let f2 = json!({"response": {"candidates": [{"content": {"parts": [{"text": "思", "thought": true}]}}]}});
        let f3 = json!({"response": {"candidates": [{"content": {"parts": [{"text": "好"}]}, "finishReason": "STOP"}], "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 2}}});
        sse_body(&format!("data: {}\n\ndata: {}\n\ndata: {}\n\n", f1, f2, f3))
    }

    async fn mock_no_project(uri: axum::http::Uri) -> axum::response::Response {
        if uri.path().ends_with("loadCodeAssist") {
            return axum::Json(json!({"allowedTiers": [{"id": "free-tier", "isDefault": true}]})).into_response();
        }
        axum::Json(json!({"done": false})).into_response()
    }

    async fn serve(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    async fn spawn_mock() -> String {
        serve(Router::new().fallback(mock_full)).await
    }
    fn req_body(model: &str) -> Vec<u8> {
        json!({
            "model": model,
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": "再次"}
            ],
            "max_tokens": 128,
            "temperature": 0.5
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn parse_cred_accepts_token_and_json() {
        let plain = parse_cred("ya29.token").unwrap();
        assert_eq!(plain.token, "ya29.token");
        assert!(plain.project.is_none());
        let j = parse_cred("{\"accessToken\":\"t1\",\"project_id\":\"p-9\"}").unwrap();
        assert_eq!(j.token, "t1");
        assert_eq!(j.project.unwrap(), "p-9");
        assert!(parse_cred("   ").is_err());
        assert!(parse_cred("{\"project_id\":\"p\"}").is_err());
    }

    #[test]
    fn build_payload_wraps_request_and_maps_config() {
        let v: Value = serde_json::from_slice(&req_body("gemini-3-flash")).unwrap();
        let p = build_payload(&v, "gemini-3-flash", "proj-1");
        assert_eq!(p["model"], "gemini-3-flash");
        assert_eq!(p["project"], "proj-1");
        assert_eq!(p["userAgent"], "antigravity");
        assert_eq!(p["requestType"], "agent");
        assert!(p["requestId"].as_str().unwrap().starts_with("agent-"));
        assert_eq!(p["request"]["generationConfig"]["maxOutputTokens"], 128);
        assert_eq!(p["request"]["generationConfig"]["temperature"], 0.5);
        assert_eq!(p["request"]["systemInstruction"]["parts"][0]["text"], "be brief");
        let contents = p["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["parts"][0]["text"], "再次");
        assert!(p["request"]["sessionId"].as_str().unwrap().starts_with("-"));
    }

    #[test]
    fn nonstream_discovers_project_and_aggregates() {
        block_on(async {
            let base = spawn_mock().await;
            let spec = spec_for(&base);
            let http = reqwest::Client::new();
            let out = chat_nonstream(&http, &spec, "tok-1", &req_body("gemini-3-flash"))
                .await
                .unwrap();
            assert_eq!(out["object"], "chat.completion");
            assert_eq!(out["model"], "gemini-3-flash");
            let text = out["choices"][0]["message"]["content"].as_str().unwrap();
            assert_eq!(text, "PROJ:proj-1好");
            assert_eq!(out["choices"][0]["message"]["reasoning_content"], "思");
            assert_eq!(out["choices"][0]["finish_reason"], "stop");
            assert_eq!(out["usage"]["prompt_tokens"], 3);
            assert_eq!(out["usage"]["completion_tokens"], 2);
        });
    }

    #[test]
    fn stream_rewrites_to_openai_chunks() {
        block_on(async {
            let base = spawn_mock().await;
            let spec = spec_for(&base);
            let http = reqwest::Client::new();
            let (stream, mut rx) = chat_stream(&http, &spec, "tok-1", &req_body("gemini-3-flash"))
                .await
                .unwrap();
            let all = stream
                .fold(String::new(), |mut acc, item| async move {
                    if let Ok(b) = item {
                        acc.push_str(&String::from_utf8_lossy(&b));
                    }
                    acc
                })
                .await;
            assert!(all.contains("chat.completion.chunk"), "{}", all);
            assert!(all.contains("PROJ:proj-1"));
            assert!(all.contains("reasoning_content"));
            assert!(all.contains("\"finish_reason\":\"stop\"") || all.contains("\"finish_reason\": \"stop\""), "{}", all);
            assert!(all.trim_end().ends_with("data: [DONE]"));
            let stat = rx.recv().await.unwrap();
            assert!(stat.error.is_none());
            assert_eq!(stat.finish_reason.as_deref(), Some("stop"));
            assert!(stat.tokens_out > 0);
        });
    }

    #[test]
    fn verify_uses_load_code_assist() {
        block_on(async {
            let base = spawn_mock().await;
            let spec = spec_for(&base);
            let http = reqwest::Client::new();
            assert!(verify(&http, &spec, "tok-1").await);
            assert!(!verify(&http, &spec, "").await);
        });
    }

    #[test]
    fn missing_project_is_actionable() {
        block_on(async {
            let spec = spec_for(&serve(Router::new().fallback(mock_no_project)).await);
            let http = reqwest::Client::new();
            let e = chat_nonstream(&http, &spec, "tok-1", &req_body("gemini-3-flash"))
                .await
                .err()
                .expect("应当失败");
            assert_eq!(e.status, 401);
            assert!(e.body.contains("project_id"), "{}", e.body);
        });
    }
}
