//! Kiro（AWS CodeWhisperer / Amazon Q Developer）adapter —— 纯 HTTP，凭据导入 + 自动续期。
//!
//! 一手来源：justlovemaki/AIClient2API 的 src/auth/kiro-oauth.js 与
//! src/providers/claude/claude-kiro.js（AWS Event Stream 帧由 _readEventTypeHeader 解析）。
//! 该平台没有 OpenAI 兼容端点：
//! 1. POST https://q.{region}.amazonaws.com/generateAssistantResponse（x-amz-target 指定 action）；
//! 2. 响应是 AWS Event Stream 二进制帧（4B totalLen / 4B headersLen / 4B preludeCRC / headers / payload / 4B msgCRC），
//!    :event-type=assistantResponseEvent 的 payload.content 才是正文，reasoningContentEvent 是思考；
//! 3. 凭据来自 kiro.dev 社交登录或 AWS SSO OIDC（Builder ID），refreshToken 可长期续期。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const KIRO_TARGET: &str = "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";
const KIRO_UA: &str = "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererstreaming/0.1.17593 os/linux lang/rust/1.92.0 md/appVersion-2.10.0 app/AmazonQ-For-CLI";
const KIRO_X_AMZ_UA: &str = "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererstreaming/0.1.17593 os/linux lang/rust/1.92.0 m/F app/AmazonQ-For-CLI";

fn er(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError { status, body: body.into(), class }
}

fn secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

/// 导入的 Kiro 凭据（kiro.dev 社交登录 / AWS SSO OIDC 两种形态都吃）。
#[derive(Clone, Debug)]
pub struct KiroCred {
    pub access_token: String,
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: String,
    pub region: String,
    /// social（kiro.dev）| idc（AWS SSO OIDC / Builder ID）
    pub auth_method: String,
    pub profile_arn: Option<String>,
    pub expires_at: Option<i64>,
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
/// 解析凭据 JSON。字段名兼容 AIClient2API / Kiro IDE / AWS SSO 几种写法。
pub fn parse_cred(secret: &str) -> Result<KiroCred, String> {
    let raw = secret.trim();
    if raw.is_empty() {
        return Err("请粘贴 Kiro 凭据 JSON（含 accessToken / refreshToken / region；Builder ID 还要 clientId + clientSecret）".into());
    }
    let v: Value = serde_json::from_str(raw)
        .map_err(|_| "Kiro 凭据必须是 JSON（Kiro 客户端 ~/.kiro 目录里的那份，或 AIClient2API 导出的凭据）".to_string())?;
    let access_token = pick_one(&v, &["accessToken", "access_token", "token"]).unwrap_or_default();
    let refresh_token = pick_one(&v, &["refreshToken", "refresh_token"]).unwrap_or_default();
    if access_token.is_empty() && refresh_token.is_empty() {
        return Err("凭据里 accessToken 与 refreshToken 都是空，无法使用".into());
    }
    let client_id = pick_one(&v, &["clientId", "client_id"]).unwrap_or_default();
    let client_secret = pick_one(&v, &["clientSecret", "client_secret"]).unwrap_or_default();
    let region = pick_one(&v, &["region", "ssoRegion", "sso_region"]).unwrap_or_else(|| "us-east-1".to_string());
    let mut auth_method = pick_one(&v, &["authMethod", "auth_method", "method"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    if auth_method.is_empty() {
        auth_method = if !client_id.is_empty() && !client_secret.is_empty() {
            "idc".to_string()
        } else {
            "social".to_string()
        };
    }
    if auth_method.contains("idc") || auth_method.contains("builder") {
        auth_method = "idc".to_string();
    } else {
        auth_method = "social".to_string();
    }
    let expires_at = v
        .get("expiresAt")
        .or_else(|| v.get("expires_at"))
        .and_then(|x| x.as_i64())
        .map(|x| if x > 100_000_000_000 { x / 1000 } else { x });
    Ok(KiroCred {
        access_token,
        refresh_token,
        client_id,
        client_secret,
        region,
        auth_method,
        profile_arn: pick_one(&v, &["profileArn", "profile_arn"]),
        expires_at,
    })
}

fn base_q(region: &str) -> String {
    format!("https://q.{region}.amazonaws.com")
}
/// 模型名归一：上游只认点号版本（claude-sonnet-4.5），用户常写连字符形式。
pub fn normalize_model(m: &str) -> String {
    let t = m.trim();
    if let Some(rest) = t.strip_prefix("claude-") {
        for fam in ["haiku", "sonnet", "opus"] {
            let pref = format!("{fam}-");
            if let Some(r2) = rest.strip_prefix(&pref) {
                let dotted = r2.replace('-', ".");
                if !dotted.is_empty() && dotted.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    return format!("claude-{fam}-{dotted}");
                }
            }
        }
    }
    t.to_string()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

fn headers(spec: &ProviderSpec, token: &str) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("Authorization".into(), format!("Bearer {token}")),
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/json".into()),
        ("x-amz-target".into(), KIRO_TARGET.into()),
        ("amz-sdk-invocation-id".into(), uuid_like()),
        ("amz-sdk-request".into(), "attempt=1; max=3".into()),
        ("x-amz-user-agent".into(), KIRO_X_AMZ_UA.into()),
        ("user-agent".into(), KIRO_UA.into()),
    ];
    for (k, v) in &spec.headers {
        if !hs.iter().any(|(a, _)| a.eq_ignore_ascii_case(k)) {
            hs.push((k.clone(), v.clone()));
        }
    }
    hs
}

fn apply(rb: reqwest::RequestBuilder, hs: &[(String, String)]) -> reqwest::RequestBuilder {
    let mut b = rb;
    for (k, v) in hs {
        b = b.header(k, v);
    }
    b
}

/// AWS Event Stream 帧解码器（4B totalLen / 4B headersLen / 4B preludeCRC / headers / payload / 4B msgCRC）。
/// 分片安全：不足一帧就留在缓冲里等下一块，非法帧头逐字节重同步。
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<(String, Value)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 12 {
                break;
            }
            let total = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            let hlen = u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize;
            if total < 16 || total > 16 * 1024 * 1024 || hlen > total - 16 {
                self.buf.drain(..1);
                continue;
            }
            if self.buf.len() < total {
                break;
            }
            let frame: Vec<u8> = self.buf.drain(..total).collect();
            let et = event_type_of(&frame[12..12 + hlen]);
            let payload = &frame[12 + hlen..total - 4];
            if let Ok(v) = serde_json::from_slice::<Value>(payload) {
                out.push((et, v));
            }
        }
        out
    }
}

fn event_type_of(hs: &[u8]) -> String {
    let mut p = 0usize;
    while p < hs.len() {
        let nl = hs[p] as usize;
        p += 1;
        if p + nl > hs.len() {
            break;
        }
        let name = String::from_utf8_lossy(&hs[p..p + nl]).to_string();
        p += nl;
        if p >= hs.len() {
            break;
        }
        let vt = hs[p];
        p += 1;
        match vt {
            0 | 1 => {
                p += 1;
            }
            2 => {
                p += 1;
            }
            3 => {
                p += 2;
            }
            4 => {
                p += 4;
            }
            5 | 8 => {
                p += 8;
            }
            9 => {
                p += 16;
            }
            6 | 7 => {
                if p + 2 > hs.len() {
                    break;
                }
                let vl = u16::from_be_bytes([hs[p], hs[p + 1]]) as usize;
                p += 2;
                if p + vl > hs.len() {
                    break;
                }
                let val = String::from_utf8_lossy(&hs[p..p + vl]).to_string();
                p += vl;
                if name == ":event-type" {
                    return val;
                }
            }
            _ => break,
        }
    }
    String::new()
}

/// 构造一帧 AWS Event Stream（生产用不到，测试与调试用）。
pub fn encode_frame(event_type: &str, payload: &Value) -> Vec<u8> {
    let mut hs: Vec<u8> = Vec::new();
    for (name, val) in [(":message-type", "event"), (":event-type", event_type), (":content-type", "application/json")] {
        hs.push(name.len() as u8);
        hs.extend_from_slice(name.as_bytes());
        hs.push(7);
        hs.extend_from_slice(&(val.len() as u16).to_be_bytes());
        hs.extend_from_slice(val.as_bytes());
    }
    let body = payload.to_string().into_bytes();
    let total = 12 + hs.len() + body.len() + 4;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(hs.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&hs);
    out.extend_from_slice(&body);
    out.extend_from_slice(&[0u8; 4]);
    out
}
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn content_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            let mut out = String::new();
            for part in arr {
                if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                    out.push_str(t);
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// CodeWhisperer 要求 tools 非空；注入一个明确禁止调用的占位工具。
fn placeholder_tool() -> Value {
    json!({
        "toolSpecification": {
            "name": "no_tool_available",
            "description": "Internal no-op placeholder. Never call this tool (or any tool) in this turn. Do not announce or promise actions such as reading files or running commands. Instead, write your full and complete answer directly as natural-language text in this single reply, based on the information already available to you.",
            "inputSchema": {"json": {"type": "object", "properties": {}}}
        }
    })
}

/// OpenAI 请求 -> CodeWhisperer conversationState（最后一条 user 当 currentMessage，其余进 history）。
pub fn build_payload(cred: &KiroCred, v: &Value, model: &str, profile: Option<&str>) -> Value {
    let mut system = String::new();
    let mut turns: Vec<(String, String)> = Vec::new();
    if let Some(msgs) = v.get("messages").and_then(|x| x.as_array()) {
        for m in msgs {
            let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
            let text = content_text(m.get("content"));
            match role {
                "system" => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&text);
                }
                "assistant" => turns.push(("assistant".to_string(), text)),
                _ => turns.push(("user".to_string(), text)),
            }
        }
    }
    let mut current = String::new();
    let mut i = turns.len();
    while i > 0 {
        i -= 1;
        if turns[i].0 == "user" {
            current = turns.remove(i).1;
            break;
        }
    }
    let mut history: Vec<Value> = Vec::new();
    for (role, text) in turns {
        if role == "assistant" {
            history.push(json!({"assistantResponseMessage": {"content": text}}));
        } else {
            history.push(json!({"userInputMessage": {"content": text, "modelId": model, "origin": "AI_EDITOR"}}));
        }
    }
    if current.trim().is_empty() {
        current = "Continue".to_string();
    }
    if !system.is_empty() {
        current = format!("{system}\n\n{current}");
    }
    // CodeWhisperer 在 tools 列表为空时会直接报 API 错误（REQUEST_BODY_INVALID），
    // 因此客户端没提供工具时注入一个禁止调用的占位工具。
    let uim = json!({
        "content": current,
        "modelId": model,
        "origin": "AI_EDITOR",
        "userInputMessageContext": {"tools": [placeholder_tool()]},
    });
    let mut cs = json!({
        "agentTaskType": "vibe",
        "chatTriggerType": "MANUAL",
        "conversationId": uuid_like(),
        "currentMessage": {"userInputMessage": uim},
    });
    if !history.is_empty() {
        cs["history"] = Value::Array(history);
    }
    let _ = cred;
    let mut req = json!({"conversationState": cs});
    if let Some(pa) = profile.filter(|s| !s.trim().is_empty()) {
        req["profileArn"] = json!(pa);
    }
    req
}

/// q.* 端点强制要求 profileArn；没有时降级到不需要它的 legacy 端点。
fn legacy_base(base: &str) -> String {
    base.replace("//q.", "//codewhisperer.")
}

/// profileArn 缺失时自举：ListAvailableProfiles 本身不需要 profileArn。
async fn discover_profile(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &KiroCred,
    token: &str,
) -> Option<String> {
    let base = if spec.base_url.trim().is_empty() || spec.base_url.contains("amazonaws.com") {
        format!("https://codewhisperer.{}.amazonaws.com", cred.region)
    } else {
        // 测试注入的 mock base：让自举也打同一个 mock，保证可离线验证。
        spec.base_url.trim().trim_end_matches('/').to_string()
    };
    let url = format!("{base}/ListAvailableProfiles");
    let resp = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("amz-sdk-invocation-id", uuid_like())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("x-amz-user-agent", KIRO_X_AMZ_UA)
        .header("user-agent", KIRO_UA)
        .json(&json!({"maxResults": 10}))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    let arr = v.get("profiles")?.as_array()?;
    let want = format!(":{}:", cred.region);
    let pick = arr
        .iter()
        .find(|p| {
            p.get("arn")
                .and_then(|a| a.as_str())
                .map(|a| a.contains(&want))
                .unwrap_or(false)
        })
        .or_else(|| {
            arr.iter()
                .find(|p| p.get("arn").and_then(|a| a.as_str()).is_some())
        })?;
    pick.get("arn").and_then(|a| a.as_str()).map(|s| s.to_string())
}

/// 解析本次请求要用的 (profileArn, base)：缺失先自举，仍拿不到就降级到 legacy 端点。
async fn resolve_target(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &KiroCred,
    token: &str,
) -> Result<(Option<String>, String), RelayError> {
    let mut base = base_url(spec, cred);
    if let Some(p) = cred.profile_arn.clone().filter(|s| !s.trim().is_empty()) {
        return Ok((Some(p), base));
    }
    if let Some(p) = discover_profile(http, spec, cred, token).await {
        return Ok((Some(p), base));
    }
    if base.contains("//q.") {
        base = legacy_base(&base);
    }
    // 实测：带 AmazonQ-For-CLI 客户端标识时，两个端点都会在鉴权前就要求 profileArn，
    // 缺了必然 400「profileArn is required for this request」。直接给出可执行提示，别让用户
    // 对着上游原文猜。
    let _ = base;
    Err(er(
        401,
        "Kiro 凭据缺少 profileArn，且用当前 accessToken 自动获取（ListAvailableProfiles）失败。\
         请从 Kiro 客户端导出的凭据里带上 profileArn（形如 arn:aws:codewhisperer:<region>:<account>:profile/<id>），\
         或确认 accessToken 未过期后重试。",
        ErrorClass::Auth,
    ))
}

/// 拿一个可用 access token：没过期就直接用，过期或有 refreshToken 时尝试续期。
///
/// 注意：续期只影响这一次请求 —— 新 token 不会写回账号池（账号池由 server 层的
/// `force_refresh` 负责回写，它拿得到 AccountStore）。这里只保证「本次能用」。
async fn ensure_token(http: &reqwest::Client, cred: &KiroCred) -> Result<String, RelayError> {
    let fresh = !cred.access_token.is_empty()
        && match cred.expires_at {
            Some(e) => e > (secs() as i64) + 60,
            None => true,
        };
    if fresh {
        return Ok(cred.access_token.clone());
    }
    Ok(refresh_token(http, cred, None).await?.0)
}

/// 续期一次。返回 `(新 accessToken, 上游可能轮换出的新 refreshToken)`。
async fn refresh_token(
    http: &reqwest::Client,
    cred: &KiroCred,
    host_override: Option<&str>,
) -> Result<(String, Option<String>), RelayError> {
    if cred.refresh_token.is_empty() {
        return Err(er(401, "Kiro 凭据已过期且没有 refreshToken，请在 Kiro 客户端重新登录后重新导出凭据", ErrorClass::Auth));
    }
    let (url, body) = if cred.auth_method == "idc" {
        (
            match host_override {
                Some(h) => format!("{}/token", h.trim_end_matches('/')),
                None => format!("https://oidc.{}.amazonaws.com/token", cred.region),
            },
            json!({
                "clientId": cred.client_id,
                "clientSecret": cred.client_secret,
                "refreshToken": cred.refresh_token,
                "grantType": "refresh_token",
            }),
        )
    } else {
        (
            match host_override {
                Some(h) => format!("{}/refreshToken", h.trim_end_matches('/')),
                None => format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", cred.region),
            },
            json!({"refreshToken": cred.refresh_token}),
        )
    };
    let resp = http
        .post(&url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(502, format!("Kiro 续期请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let txt = resp.text().await.unwrap_or_default();
    if status != 200 {
        return Err(er(401, format!("Kiro 续期失败（{status}）：{}", truncate(&txt, 240)), ErrorClass::Auth));
    }
    let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
    let at = v
        .get("accessToken")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| er(401, "Kiro 续期响应里没有 accessToken", ErrorClass::Auth))?;
    // 上游续期接口大概率会同时换发 refreshToken（两形态都会）。旧 rt 一旦被作废，
    // 账号池里那条就再也换不出新 token，只能重新登录 —— 所以新 rt 必须回写。
    let rt = v
        .get("refreshToken")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Ok((at, rt))
}
/// 把续期结果写回 secret 原文（保留用户原本粘进来的其它字段，只覆盖 token 二元组）。
fn patch_secret(secret: &str, at: &str, rt: Option<&str>) -> String {
    let mut v: Value = serde_json::from_str(secret.trim()).unwrap_or_else(|_| json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("accessToken".into(), json!(at));
        if let Some(r) = rt {
            o.insert("refreshToken".into(), json!(r));
        }
    }
    v.to_string()
}

/// 强制续期一次并返回**可直接写回账号池**的新 secret。
///
/// server 层在拿到 401/403 时调它：成功后 `AccountStore::update_secret` 落盘，
/// 再重试一次转发。没有 refreshToken 或上游拒绝时返回 Err（错误文案可直接给用户看）。
pub async fn refresh_secret(http: &reqwest::Client, secret: &str) -> Result<String, String> {
    refresh_secret_at(http, secret, None).await
}

/// 同上，但允许指定续期端点主机（测试用；生产走 `cred.region` 推出的官方域名）。
pub async fn refresh_secret_at(
    http: &reqwest::Client,
    secret: &str,
    host_override: Option<&str>,
) -> Result<String, String> {
    let cred = parse_cred(secret).map_err(|e| e)?;
    let (at, rt) = refresh_token(http, &cred, host_override).await.map_err(|e| e.body)?;
    Ok(patch_secret(secret, &at, rt.as_deref()))
}

fn base_url(spec: &ProviderSpec, cred: &KiroCred) -> String {
    let b = spec.base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        base_q(&cred.region)
    } else {
        b.to_string()
    }
}

fn push_event(et: &str, p: &Value, content: &mut String, reasoning: &mut String) {
    match et {
        "assistantResponseEvent" => {
            if let Some(c) = p.get("content").and_then(|x| x.as_str()) {
                content.push_str(c);
            }
        }
        "reasoningContentEvent" => {
            if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                reasoning.push_str(t);
            }
        }
        _ => {
            if p.get("contextUsagePercentage").is_some() {
                return;
            }
            if let Some(c) = p.get("content").and_then(|x| x.as_str()) {
                content.push_str(c);
            } else if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                reasoning.push_str(t);
            }
        }
    }
}

fn relay_status_class(status: u16) -> ErrorClass {
    match status {
        401 | 403 => ErrorClass::Auth,
        429 => ErrorClass::Rate,
        402 => ErrorClass::Quota,
        400 => ErrorClass::Rejected,
        _ => ErrorClass::Transient,
    }
}

/// 非流式：一次 generateAssistantResponse，把事件流聚合成 OpenAI JSON。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let model = normalize_model(v.get("model").and_then(|x| x.as_str()).unwrap_or("claude-sonnet-4.5"));
    let token = ensure_token(http, &cred).await?;
    let (profile, base) = resolve_target(http, spec, &cred, &token).await?;
    let hs = headers(spec, &token);
    let payload = build_payload(&cred, &v, &model, profile.as_deref());
    let url = format!("{base}/generateAssistantResponse");
    let resp = apply(http.post(&url), &hs)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| er(502, format!("Kiro 请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let raw = resp.bytes().await.unwrap_or_default();
    if status != 200 {
        return Err(er(
            status,
            format!("Kiro 上游 {status}：{}", truncate(&String::from_utf8_lossy(&raw), 300)),
            relay_status_class(status),
        ));
    }
    let mut dec = FrameDecoder::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    for (et, p) in dec.push(&raw) {
        push_event(&et, &p, &mut content, &mut reasoning);
    }
    if content.trim().is_empty() {
        return Err(er(
            401,
            "Kiro 未返回正文（凭据可能已失效，或该账号没有 CodeWhisperer 订阅）",
            ErrorClass::Auth,
        ));
    }
    let tin = ((body.len() / 4).max(1)) as i64;
    let tout = ((content.chars().count() / 2).max(1)) as i64;
    let mut msg = json!({"role": "assistant", "content": content});
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    Ok(json!({
        "id": format!("chatcmpl-{}", uuid_like()),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{"index": 0, "message": msg, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout},
    }))
}
/// 流式：把 Event Stream 帧改写成 OpenAI chunk（content / reasoning_content + 尾部 [DONE]）。
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
    let model = normalize_model(v.get("model").and_then(|x| x.as_str()).unwrap_or("claude-sonnet-4.5"));
    let token = ensure_token(http, &cred).await?;
    let (profile, base) = resolve_target(http, spec, &cred, &token).await?;
    let hs = headers(spec, &token);
    let payload = build_payload(&cred, &v, &model, profile.as_deref());
    let url = format!("{base}/generateAssistantResponse");
    let resp = apply(http.post(&url), &hs)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| er(502, format!("Kiro 请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if status != 200 {
        let raw = resp.bytes().await.unwrap_or_default();
        return Err(er(
            status,
            format!("Kiro 上游 {status}：{}", truncate(&String::from_utf8_lossy(&raw), 300)),
            relay_status_class(status),
        ));
    }

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
        let mut dec = FrameDecoder::new();
        let mut stream = resp.bytes_stream();
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(b) => b,
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {e}");
                    break;
                }
            };
            let mut out = String::new();
            for (et, p) in dec.push(&chunk) {
                let mut content = String::new();
                let mut reasoning = String::new();
                push_event(&et, &p, &mut content, &mut reasoning);
                if reasoning.is_empty() && content.is_empty() {
                    continue;
                }
                if !sent_role {
                    out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                    sent_role = true;
                }
                if !reasoning.is_empty() {
                    out_chars += reasoning.chars().count();
                    out.push_str(&mk(json!({"reasoning_content": reasoning}), None));
                }
                if !content.is_empty() {
                    out_chars += content.chars().count();
                    out.push_str(&mk(json!({"content": content}), None));
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break;
            }
        }
        let mut tail = String::new();
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "Kiro 未返回正文（凭据可能已失效，或该账号没有 CodeWhisperer 订阅）".into();
        }
        if let Some(ec) = stat.error {
            let etype = match ec {
                ErrorClass::Rate => "rate_limit_error",
                ErrorClass::Auth => "authentication_error",
                ErrorClass::Quota => "insufficient_quota",
                _ => "api_error",
            };
            let msg = stat.error_msg.replace('"', "'");
            tail.push_str(&format!("data: {}\n\n", json!({"error": {"message": msg, "type": etype}})));
        } else {
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

/// 验活：getUsageLimits（不需要 profileArn 的接口能自举出可用性）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok(cred) = parse_cred(secret) else {
        return false;
    };
    let Ok(token) = ensure_token(http, &cred).await else {
        return false;
    };
    let mut url = format!("{}/getUsageLimits?isEmailRequired=true&origin=AI_EDITOR", base_url(spec, &cred));
    if let Some(pa) = &cred.profile_arn {
        url.push_str(&format!("&profileArn={}", urlencode(pa)));
    }
    let rb = http
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("user-agent", KIRO_UA)
        .timeout(std::time::Duration::from_secs(20));
    match rb.send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;

    const TOKEN: &str = "at-1";

    fn frames() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(encode_frame(
            "assistantResponseEvent",
            &json!({"content": "\u{4f60}"}),
        ));
        out.extend(encode_frame(
            "reasoningContentEvent",
            &json!({"text": "\u{601d}"}),
        ));
        out.extend(encode_frame(
            "assistantResponseEvent",
            &json!({"content": "\u{597d}"}),
        ));
        out.extend(encode_frame(
            "contextUsageEvent",
            &json!({"contextUsagePercentage": 12.5}),
        ));
        out
    }

    async fn mock() -> String {
        let body = frames();
        let app = Router::new()
            .route(
                "/generateAssistantResponse",
                post(move || {
                    let body = body.clone();
                    async move {
                        axum::response::Response::builder()
                            .header("content-type", "application/vnd.amazon.eventstream")
                            .body(Body::from(body))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/getUsageLimits",
                axum::routing::get(|| async { axum::Json(json!({"used": 1})) }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    /// 续期端点 mock：social 形态走 /refreshToken，返回一个新 at + 新 rt。
    async fn refresh_mock() -> String {
        let app = Router::new().route(
            "/refreshToken",
            post(|| async { axum::Json(json!({"accessToken": "at-new", "refreshToken": "rt-new"})) }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    fn spec_for(base: &str) -> ProviderSpec {
        ProviderSpec {
            id: "sub-kiro".into(),
            name: "Kiro".into(),
            enabled: true,
            auth: aigw_registry::AuthKind::Cookie,
            login_methods: Vec::new(),
            base_url: base.to_string(),
            format: "openai".into(),
            models: vec!["claude-sonnet-4.5".into()],
            models_policy: aigw_registry::ModelsPolicy::Static,
            stream_dialect: aigw_registry::StreamDialect::SseDelta,
            headers: std::collections::BTreeMap::new(),
            chat_path: String::new(),
            models_path: String::new(),
            models_filter: String::new(),
            upstream_kind: "kiro".into(),
            signer: String::new(),
            biz_err: vec![],
            hosts: vec![],
            login_url: String::new(),
            notes: String::new(),
            chat_supported: true,
            region: "global".into(),
        }
    }

    fn secret() -> String {
        json!({"accessToken": TOKEN, "profileArn": "arn:aws:codewhisperer:us-east-1:1:profile/P1"})
            .to_string()
    }

    fn req_body(model: &str) -> Vec<u8> {
        json!({
            "model": model,
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": "again"}
            ]
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn parse_cred_accepts_social_and_idc() {
        let c = parse_cred(&secret()).unwrap();
        assert_eq!(c.access_token, TOKEN);
        assert_eq!(c.auth_method, "social");
        assert_eq!(c.region, "us-east-1");
        assert!(c.profile_arn.unwrap().contains("profile/P1"));
        let idc = parse_cred(
            "{\"refreshToken\":\"rt\",\"clientId\":\"cid\",\"clientSecret\":\"cs\",\"region\":\"eu-west-1\"}",
        )
        .unwrap();
        assert_eq!(idc.auth_method, "idc");
        assert_eq!(idc.region, "eu-west-1");
        assert!(parse_cred("{}").is_err());
    }

    #[test]
    fn normalize_model_uses_dotted_versions() {
        assert_eq!(normalize_model("claude-sonnet-4-5"), "claude-sonnet-4.5");
        assert_eq!(normalize_model("claude-opus-4.5"), "claude-opus-4.5");
        assert_eq!(normalize_model("claude-3-7-sonnet"), "claude-3-7-sonnet");
        assert_eq!(normalize_model("gpt-5.6-sol"), "gpt-5.6-sol");
    }

    #[test]
    fn frame_decoder_handles_split_chunks() {
        let all = frames();
        let mut dec = FrameDecoder::new();
        let mut events = Vec::new();
        for b in &all {
            events.extend(dec.push(&[*b]));
        }
        assert_eq!(events.len(), 4, "逐字节喂入也应出 4 个事件：{events:?}");
        assert_eq!(events[0].0, "assistantResponseEvent");
        assert_eq!(events[1].0, "reasoningContentEvent");
        assert_eq!(events[1].1["text"], "\u{601d}");
    }

    #[test]
    fn build_payload_moves_history_and_prefixes_system() {
        let cred = parse_cred(&secret()).unwrap();
        let v: Value = serde_json::from_slice(&req_body("claude-sonnet-4-5")).unwrap();
        let p = build_payload(&cred, &v, "claude-sonnet-4.5", cred.profile_arn.as_deref());
        let cs = &p["conversationState"];
        assert_eq!(cs["chatTriggerType"], "MANUAL");
        assert_eq!(cs["currentMessage"]["userInputMessage"]["content"], "be brief\n\nagain");
        assert_eq!(cs["currentMessage"]["userInputMessage"]["modelId"], "claude-sonnet-4.5");
        assert_eq!(cs["history"].as_array().unwrap().len(), 2, "最后一条 user 被取为 currentMessage，其余 2 条进 history");
        assert_eq!(cs["history"][1]["assistantResponseMessage"]["content"], "hello");
        assert!(p["profileArn"].as_str().unwrap().contains("profile/P1"));
    }

    #[tokio::test]
    async fn kiro_nonstream_via_mock() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let v = chat_nonstream(&http, &spec, &secret(), &req_body("claude-sonnet-4-5"))
            .await
            .unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "\u{4f60}\u{597d}");
        assert_eq!(v["choices"][0]["message"]["reasoning_content"], "\u{601d}");
        assert_eq!(v["model"], "claude-sonnet-4.5");
        assert!(v["usage"]["prompt_tokens"].as_i64().unwrap() > 0);
        assert!(v["usage"]["completion_tokens"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn kiro_stream_via_mock() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let (mut s, mut rx) = chat_stream(&http, &spec, &secret(), &req_body("claude-sonnet-4-5"))
            .await
            .unwrap();
        let mut all = String::new();
        while let Some(item) = s.next().await {
            all.push_str(std::str::from_utf8(&item.unwrap()).unwrap());
        }
        assert!(all.contains("\u{4f60}"), "流里应含首字: {all}");
        assert!(all.contains("reasoning_content"), "流里应有思考字段: {all}");
        assert!(all.contains("\"finish_reason\":\"stop\""), "应有结束帧: {all}");
        assert!(all.contains("[DONE]"));
        let stat = rx.try_recv().unwrap();
        assert!(stat.error.is_none(), "不应有错误: {:?}", stat.error_msg);
        assert_eq!(stat.finish_reason.as_deref(), Some("stop"));
        assert!(stat.tokens_out > 0);
    }

    /// 续期必须能把轮换出的 refreshToken 写回账号池：旧 rt 一旦被上游作废，
    /// 不回写就等于这个号再也续不了，只能重新登录。
    #[tokio::test]
    async fn refresh_secret_writes_back_rotated_refresh_token() {
        let base = refresh_mock().await;
        let http = reqwest::Client::new();
        let secret = r#"{"accessToken":"at-old","refreshToken":"rt-old","region":"us-east-1"}"#;
        let new_secret = refresh_secret_at(&http, secret, Some(&base)).await.expect("refresh");
        let back = parse_cred(&new_secret).expect("parse back");
        assert_eq!(back.access_token, "at-new", "{new_secret}");
        assert_eq!(back.refresh_token, "rt-new", "轮换的 rt 必须回写：{new_secret}");
        assert_eq!(back.region, "us-east-1", "用户字段要保留：{new_secret}");
    }

    #[tokio::test]
    async fn refresh_secret_without_refresh_token_is_actionable_error() {
        let base = refresh_mock().await;
        let http = reqwest::Client::new();
        let e = refresh_secret_at(&http, r#"{"accessToken":"at-old"}"#, Some(&base)).await.unwrap_err();
        assert!(e.contains("refreshToken"), "{e}");
    }

    #[tokio::test]
    async fn kiro_verify_hits_usage_limits() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        assert!(verify(&http, &spec, &secret()).await);
        assert!(!verify(&http, &spec, "not-json").await);
    }

    #[test]
    fn payload_omits_profile_arn_when_absent() {
        let cred = parse_cred("{\"accessToken\":\"at\"}").unwrap();
        let v: Value = serde_json::from_slice(&req_body("claude-sonnet-4.5")).unwrap();
        let p = build_payload(&cred, &v, "claude-sonnet-4.5", None);
        assert!(p.get("profileArn").is_none(), "无 profileArn 时不应带空字段: {p}");
        assert_eq!(
            p["conversationState"]["currentMessage"]["userInputMessage"]["userInputMessageContext"]["tools"][0]
                ["toolSpecification"]["name"],
            "no_tool_available",
            "CodeWhisperer 要求 tools 非空，必须注入占位工具"
        );
    }

    #[test]
    fn legacy_base_only_rewrites_q_host() {
        assert_eq!(
            legacy_base("https://q.us-east-1.amazonaws.com"),
            "https://codewhisperer.us-east-1.amazonaws.com"
        );
        assert_eq!(legacy_base("http://127.0.0.1:9"), "http://127.0.0.1:9");
    }

    /// 自举用 mock：ListAvailableProfiles 返回一个 arn，生成端点把请求体原样回显成正文。
    async fn echo_mock(profiles: Option<Value>) -> String {
        let app = Router::new()
            .route(
                "/ListAvailableProfiles",
                post(move || {
                    let p = profiles.clone();
                    async move {
                        match p {
                            Some(v) => axum::Json(v).into_response(),
                            None => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
                        }
                    }
                }),
            )
            .route(
                "/generateAssistantResponse",
                post(|body: String| async move {
                    let frame = encode_frame("assistantResponseEvent", &json!({"content": body}));
                    axum::response::Response::builder()
                        .header("content-type", "application/vnd.amazon.eventstream")
                        .body(Body::from(frame))
                        .unwrap()
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn profileless_cred_discovers_profile_then_sends_it() {
        let base = echo_mock(Some(json!({
            "profiles": [
                {"arn": "arn:aws:codewhisperer:eu-west-1:1:profile/OTHER"},
                {"arn": "arn:aws:codewhisperer:us-east-1:9:profile/DISC"}
            ]
        })))
        .await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let secret = json!({"accessToken": TOKEN, "region": "us-east-1"}).to_string();
        let v = chat_nonstream(&http, &spec, &secret, &req_body("claude-sonnet-4.5"))
            .await
            .unwrap();
        let echo = v["choices"][0]["message"]["content"].as_str().unwrap().to_string();
        assert!(
            echo.contains("arn:aws:codewhisperer:us-east-1:9:profile/DISC"),
            "应优先取与 region 匹配的 arn: {echo}"
        );
    }

    #[tokio::test]
    async fn profileless_without_discovery_returns_actionable_error() {
        let base = echo_mock(None).await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let secret = json!({"accessToken": TOKEN, "region": "us-east-1"}).to_string();
        let e = chat_nonstream(&http, &spec, &secret, &req_body("claude-sonnet-4.5"))
            .await
            .unwrap_err();
        assert_eq!(e.status, 401, "缺 profileArn 应给 401 而不是把上游 400 透传: {e:?}");
        assert!(e.body.contains("profileArn"), "错误里要指名 field: {}", e.body);
    }
}
