//! DeepSeek 网页版（chat.deepseek.com）适配器。
//!
//! 私有协议链路（参考 xiaoY233/Chat2API 的 deepseek 适配器逆向结果）：
//! 1. GET  /v0/users/current            刷新 access token（1 小时缓存）
//! 2. POST /v0/chat_session/create      新建会话，拿 chat_session_id
//! 3. POST /v0/chat/create_pow_challenge 拿工作量证明挑战（DeepSeekHashV1）
//! 4. 用内嵌的 wasm（sha3_wasm_bg）求解 → 组装 X-Ds-Pow-Response 头
//! 5. POST /v0/chat/completion          SSE 补丁流（fragments / APPEND / BATCH）

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use base64::Engine as _;
use serde_json::{json, Value};
use wasmi::core::F64;
use wasmi::{Engine, Linker, Module, Store};

use bytes::Bytes;
use futures_util::StreamExt;

use crate::{RelayError, StreamStat};

/// 网页版 API 根（实际路径是 /v0/...）。
pub const DS_DEFAULT_BASE: &str = "https://chat.deepseek.com/api";

pub const DS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";

/// 参考项目自带的 sha3 + pow 求解 wasm（26 KB，无 import）。
static WASM_BYTES: &[u8] = include_bytes!("deepseek_sha3.wasm");
static WASM: OnceLock<Result<(Engine, Module), String>> = OnceLock::new();

fn wasm() -> Result<&'static (Engine, Module), String> {
    let slot = WASM.get_or_init(|| {
        let engine = Engine::default();
        let module = Module::new(&engine, &mut &WASM_BYTES[..])
            .map_err(|e| format!("wasm 编译失败：{e}"))?;
        Ok((engine, module))
    });
    slot.as_ref().map_err(|e| e.clone())
}

/// 调用 wasm_solve 求解 POW，返回 answer（浮点）。
fn solve_pow(challenge: &str, prefix: &str, difficulty: f64) -> Result<f64, String> {
    let (engine, module) = wasm()?;
    let mut store = Store::new(engine, ());
    let linker = Linker::new(engine);
    let pre = linker
        .instantiate(&mut store, module)
        .map_err(|e| format!("wasm 实例化失败：{e}"))?;
    let instance = pre
        .start(&mut store)
        .map_err(|e| format!("wasm 启动失败：{e}"))?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or_else(|| "wasm 缺少 memory 导出".to_string())?;
    let malloc = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "__wbindgen_export_0")
        .map_err(|e| format!("wasm 缺少 malloc：{e}"))?;
    let stack = instance
        .get_typed_func::<i32, i32>(&mut store, "__wbindgen_add_to_stack_pointer")
        .map_err(|e| format!("wasm 缺少栈指针函数：{e}"))?;
    let solve = instance
        .get_typed_func::<(i32, i32, i32, i32, i32, F64), ()>(&mut store, "wasm_solve")
        .map_err(|e| format!("wasm 缺少 wasm_solve：{e}"))?;

    let p0 = malloc
        .call(&mut store, (challenge.len() as i32, 1))
        .map_err(|e| format!("wasm malloc 失败：{e}"))? as u32;
    memory
        .write(&mut store, p0 as usize, challenge.as_bytes())
        .map_err(|e| format!("写 wasm 内存失败：{e}"))?;
    let p1 = malloc
        .call(&mut store, (prefix.len() as i32, 1))
        .map_err(|e| format!("wasm malloc 失败：{e}"))? as u32;
    memory
        .write(&mut store, p1 as usize, prefix.as_bytes())
        .map_err(|e| format!("写 wasm 内存失败：{e}"))?;

    let ret = stack
        .call(&mut store, -16)
        .map_err(|e| format!("wasm 取栈失败：{e}"))? as u32;
    solve
        .call(
            &mut store,
            (
                ret as i32,
                p0 as i32,
                challenge.len() as i32,
                p1 as i32,
                prefix.len() as i32,
                F64::from(difficulty),
            ),
        )
        .map_err(|e| format!("wasm 求解失败：{e}"))?;
    let mut buf = [0u8; 16];
    memory
        .read(&store, ret as usize, &mut buf)
        .map_err(|e| format!("读 wasm 内存失败：{e}"))?;
    let _ = stack.call(&mut store, 16);
    let status = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let value = f64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    if status == 0 {
        return Err("POW 未求得答案（挑战可能已过期）".to_string());
    }
    Ok(value)
}

fn er(status: u16, msg: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError {
        status,
        body: msg.into(),
        class,
    }
}

fn secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn nanos_hex() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:032x}")
}

// ---------------------------------------------------------------- 凭据

#[derive(Debug, Clone)]
pub struct DsCred {
    pub token: String,
}

/// 凭据：纯 token、JSON {"token": ...}、或整段 Cookie 里的 token=。
pub fn parse_cred(secret: &str) -> Result<DsCred, String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("DeepSeek 凭据为空：请粘贴 chat.deepseek.com 的 token".to_string());
    }
    if s.starts_with('{') {
        let v: Value = serde_json::from_str(s)
            .map_err(|e| format!("凭据不是合法 JSON：{e}"))?;
        for key in ["token", "access_token", "accessToken", "userToken", "session"] {
            if let Some(t) = v.get(key).and_then(|x| x.as_str()) {
                let t = t.trim();
                if !t.is_empty() {
                    return Ok(DsCred { token: t.to_string() });
                }
            }
        }
        return Err("JSON 里没有 token 字段".to_string());
    }
    if s.contains('=') {
        for part in s.split(';') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix("token=") {
                if !rest.trim().is_empty() {
                    return Ok(DsCred { token: rest.trim().to_string() });
                }
            }
        }
    }
    Ok(DsCred { token: s.to_string() })
}

// ---------------------------------------------------------------- 请求头

fn base_url(spec: &ProviderSpec) -> String {
    let b = spec.base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        DS_DEFAULT_BASE.to_string()
    } else {
        b.to_string()
    }
}

fn headers(spec: &ProviderSpec, token: &str) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("accept".to_string(), "*/*".to_string()),
        ("authorization".to_string(), format!("Bearer {token}")),
        ("content-type".to_string(), "application/json".to_string()),
        ("origin".to_string(), "https://chat.deepseek.com".to_string()),
        ("referer".to_string(), "https://chat.deepseek.com/".to_string()),
        ("user-agent".to_string(), DS_UA.to_string()),
        ("x-app-version".to_string(), "2.0.0".to_string()),
        ("x-client-platform".to_string(), "web".to_string()),
        ("x-client-version".to_string(), "2.0.0".to_string()),
        ("x-client-locale".to_string(), "zh_CN".to_string()),
        ("x-client-timezone-offset".to_string(), "28800".to_string()),
    ];
    for (k, v) in &spec.headers {
        let k = k.to_ascii_lowercase();
        hs.retain(|(hk, _)| *hk != k);
        hs.push((k, v.clone()));
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

// ---------------------------------------------------------------- token 缓存

static TOKENS: Mutex<Vec<(String, String, Instant)>> = Mutex::new(Vec::new());
const TOKEN_TTL: Duration = Duration::from_secs(3600);

fn cached_token(secret: &str) -> Option<String> {
    let mut guard = match TOKENS.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    guard.retain(|(k, _, at)| at.elapsed() < TOKEN_TTL && k != secret);
    guard
        .iter()
        .find(|(k, _, _)| k == secret)
        .map(|(_, v, _)| v.clone())
}

fn store_token(secret: &str, fresh: &str) {
    let mut guard = match TOKENS.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    guard.push((secret.to_string(), fresh.to_string(), Instant::now()));
    if guard.len() > 32 {
        guard.remove(0);
    }
}

fn biz_data(v: &Value) -> Option<&Value> {
    v.get("data").and_then(|d| d.get("biz_data"))
}

fn api_code(v: &Value) -> i64 {
    v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1)
}

fn api_msg(v: &Value) -> String {
    v.get("msg")
        .and_then(|m| m.as_str())
        .filter(|m| !m.is_empty())
        .unwrap_or("未知错误")
        .to_string()
}

/// GET /v0/users/current，取新鲜 access token。
async fn fresh_token(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &DsCred,
) -> Result<String, RelayError> {
    if let Some(t) = cached_token(&cred.token) {
        return Ok(t);
    }
    let url = format!("{}/v0/users/current", base_url(spec));
    let hs = headers(spec, &cred.token);
    let resp = apply(http.get(&url), &hs)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| er(502, format!("DeepSeek 取用户信息失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(er(401, format!("DeepSeek token 无效（HTTP {status}）：{text}"), ErrorClass::Auth));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| er(502, format!("DeepSeek 返回非 JSON：{e}"), ErrorClass::Transient))?;
    if api_code(&v) != 0 {
        return Err(er(401, format!("DeepSeek token 失效：{}", api_msg(&v)), ErrorClass::Auth));
    }
    let token = biz_data(&v)
        .and_then(|d| d.get("token"))
        .and_then(|t| t.as_str())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| cred.token.clone());
    store_token(&cred.token, &token);
    Ok(token)
}

/// POST /v0/chat_session/create。
async fn create_session(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    hs: &[(String, String)],
) -> Result<String, RelayError> {
    let url = format!("{}/v0/chat_session/create", base_url(spec));
    let resp = apply(http.post(&url), hs)
        .json(&json!({}))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| er(502, format!("DeepSeek 建会话失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(er(401, format!("DeepSeek 建会话失败（HTTP {status}）：{text}"), ErrorClass::Auth));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| er(502, format!("DeepSeek 返回非 JSON：{e}"), ErrorClass::Transient))?;
    if api_code(&v) != 0 {
        return Err(er(401, format!("DeepSeek 建会话失败：{}", api_msg(&v)), ErrorClass::Auth));
    }
    biz_data(&v)
        .and_then(|d| d.get("chat_session"))
        .and_then(|s| s.get("id"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| er(401, "DeepSeek 建会话失败：响应里没有 chat_session.id".to_string(), ErrorClass::Auth))
}

/// POST /v0/chat/create_pow_challenge。
async fn create_challenge(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    hs: &[(String, String)],
) -> Result<Value, RelayError> {
    let url = format!("{}/v0/chat/create_pow_challenge", base_url(spec));
    let resp = apply(http.post(&url), hs)
        .json(&json!({"target_path": "/api/v0/chat/completion"}))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| er(502, format!("DeepSeek 取 POW 挑战失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(er(502, format!("DeepSeek 取 POW 挑战失败（HTTP {status}）：{text}"), ErrorClass::Transient));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| er(502, format!("DeepSeek 返回非 JSON：{e}"), ErrorClass::Transient))?;
    biz_data(&v)
        .and_then(|d| d.get("challenge"))
        .cloned()
        .ok_or_else(|| er(502, "DeepSeek 响应里没有 challenge".to_string(), ErrorClass::Transient))
}

fn field_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// 解出 answer 并组装 X-Ds-Pow-Response。
pub fn pow_header(challenge: &Value) -> Result<String, RelayError> {
    let algorithm = field_str(challenge, "algorithm");
    if algorithm != "DeepSeekHashV1" {
        return Err(er(502, format!("不支持的 POW 算法：{algorithm}"), ErrorClass::Transient));
    }
    let ch = field_str(challenge, "challenge");
    let salt = field_str(challenge, "salt");
    let signature = field_str(challenge, "signature");
    let target_path = field_str(challenge, "target_path");
    let expire_at = challenge.get("expire_at").and_then(|x| x.as_i64()).unwrap_or(0);
    let difficulty = challenge
        .get("difficulty")
        .and_then(|x| x.as_f64())
        .unwrap_or(144000.0);
    if ch.is_empty() || salt.is_empty() {
        return Err(er(502, "POW 挑战缺少 challenge/salt".to_string(), ErrorClass::Transient));
    }
    let prefix = format!("{salt}_{expire_at}_");
    #[cfg(test)]
    let answer = if SKIP_POW.load(std::sync::atomic::Ordering::Relaxed) {
        1.0
    } else {
        solve_pow(&ch, &prefix, difficulty)
            .map_err(|e| er(502, format!("POW 求解失败：{e}"), ErrorClass::Transient))?
    };
    #[cfg(not(test))]
    let answer = solve_pow(&ch, &prefix, difficulty)
        .map_err(|e| er(502, format!("POW 求解失败：{e}"), ErrorClass::Transient))?;
    // answer 必须是**整数**：写成 69573.0（f64 序列化）上游会判 INVALID_POW_RESPONSE（code 40301）。
    let answer_json = if answer.fract() == 0.0 {
        json!(answer as i64)
    } else {
        json!(answer)
    };
    let payload = json!({
        "algorithm": algorithm,
        "challenge": ch,
        "salt": salt,
        "answer": answer_json,
        "signature": signature,
        "target_path": target_path,
    });
    Ok(base64::engine::general_purpose::STANDARD.encode(payload.to_string().as_bytes()))
}
// ---------------------------------------------------------------- 请求整形

fn content_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|it| {
                if let Some(s) = it.as_str() {
                    Some(s.to_string())
                } else {
                    it.get("text").and_then(|t| t.as_str()).map(|t| t.to_string())
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// DeepSeek 网页版只吃一整段 prompt，这里把 messages 摊平成多轮对话文本。
pub fn flatten(v: &Value) -> String {
    let mut system = String::new();
    let mut lines: Vec<String> = Vec::new();
    if let Some(arr) = v.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let text = content_text(m.get("content"));
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            match role {
                "system" | "developer" => {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(text);
                }
                "assistant" => lines.push(format!("Assistant: {text}")),
                _ => lines.push(format!("User: {text}")),
            }
        }
    }
    let convo = lines.join("\n\n");
    if system.is_empty() {
        convo
    } else if convo.is_empty() {
        system
    } else {
        format!("{system}\n\n{convo}")
    }
}

/// 从模型名推断（expert 模式 / 深度思考 / 联网搜索）。
pub fn read_request(v: &Value) -> (String, bool, bool, bool) {
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("deepseek-chat")
        .to_string();
    let low = model.to_ascii_lowercase();
    let expert = low.contains("expert") || low.contains("v4-pro");
    let thinking =
        low.contains("think") || low.contains("reasoner") || low.contains("r1") || low.contains("reasoning");
    let search = low.contains("search");
    (model, expert, thinking, search)
}

fn est_tokens(s: &str) -> i64 {
    let mut cjk = 0i64;
    let mut other = 0i64;
    for ch in s.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if ch as u32 >= 0x2E80 {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    (cjk + other / 4).max(1)
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

// ---------------------------------------------------------------- 补丁流解析

/// DeepSeek 用「对象补丁」协议推流：p=路径、o=操作、v=值；缺 p/o 表示续接上一个操作。
#[derive(Debug, Default, Clone)]
pub struct PatchState {
    /// 当前片段类型：content（正文）或 reasoning_content（思考）。
    pub kind: String,
    pub content: String,
    pub reasoning: String,
    pub usage: i64,
    pub finished: bool,
    pub saw_any: bool,
}

fn fragment_kind(t: &str) -> String {
    if t.eq_ignore_ascii_case("THINK") {
        "reasoning_content".to_string()
    } else {
        "content".to_string()
    }
}

fn push_delta(st: &mut PatchState, out: &mut Vec<(String, String)>, text: &str) {
    if text.is_empty() {
        return;
    }
    let kind = if st.kind.is_empty() {
        "content".to_string()
    } else {
        st.kind.clone()
    };
    st.saw_any = true;
    if kind == "reasoning_content" {
        st.reasoning.push_str(text);
    } else {
        st.content.push_str(text);
    }
    out.push((kind, text.to_string()));
}

/// 处理一帧 data JSON，返回本次新增的 (kind, delta) 列表。
pub fn apply_frame(st: &mut PatchState, data: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return out;
    };
    let p = v.get("p").and_then(|x| x.as_str()).unwrap_or("");
    let o = v.get("o").and_then(|x| x.as_str()).unwrap_or("");
    let val = v.get("v");

    if p.is_empty() {
        let Some(val) = val else { return out };
        if let Some(resp) = val.get("response") {
            if let Some(frags) = resp.get("fragments").and_then(|f| f.as_array()) {
                for f in frags {
                    let t = f.get("type").and_then(|x| x.as_str()).unwrap_or("RESPONSE");
                    st.kind = fragment_kind(t);
                    let c = f.get("content").and_then(|x| x.as_str()).unwrap_or("");
                    push_delta(st, &mut out, c);
                }
            }
            if let Some(u) = resp.get("accumulated_token_usage").and_then(|x| x.as_i64()) {
                if u > 0 {
                    st.usage = u;
                }
            }
            return out;
        }
        if let Some(s) = val.as_str() {
            push_delta(st, &mut out, s);
        }
        return out;
    }

    if p == "response" && o == "BATCH" {
        if let Some(items) = val.and_then(|x| x.as_array()) {
            for it in items {
                let ip = it.get("p").and_then(|x| x.as_str()).unwrap_or("");
                let iv = it.get("v");
                if ip == "accumulated_token_usage" {
                    if let Some(u) = iv.and_then(|x| x.as_i64()) {
                        if u > 0 {
                            st.usage = u;
                        }
                    }
                } else if ip == "quasi_status"
                    && iv.and_then(|x| x.as_str()) == Some("FINISHED")
                {
                    st.finished = true;
                }
            }
        }
        return out;
    }

    if p == "response/status" {
        if val.and_then(|x| x.as_str()) == Some("FINISHED") {
            st.finished = true;
        }
        return out;
    }

    if p == "response/fragments" && (o == "APPEND" || o == "SET") {
        if let Some(items) = val.and_then(|x| x.as_array()) {
            for f in items {
                let t = f.get("type").and_then(|x| x.as_str()).unwrap_or("RESPONSE");
                st.kind = fragment_kind(t);
                let c = f.get("content").and_then(|x| x.as_str()).unwrap_or("");
                push_delta(st, &mut out, c);
            }
        }
        return out;
    }

    if p.ends_with("/content") && (o == "APPEND" || o.is_empty()) {
        if let Some(s) = val.and_then(|x| x.as_str()) {
            push_delta(st, &mut out, s);
        }
    }
    out
}

fn data_of(block: &str) -> Option<&str> {
    for line in block.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            if !rest.is_empty() && rest != "[DONE]" {
                return Some(rest);
            }
        }
    }
    None
}

// ---------------------------------------------------------------- 组装请求

struct Prepared {
    base: String,
    hs: Vec<(String, String)>,
    payload: Value,
    prompt_text: String,
    model: String,
}

async fn prepare(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Prepared, RelayError> {
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let (model, expert, thinking, search) = read_request(&v);
    let prompt_text = flatten(&v);
    if prompt_text.trim().is_empty() {
        return Err(er(400, "DeepSeek 请求里没有可发送的正文".to_string(), ErrorClass::Rejected));
    }
    let token = fresh_token(http, spec, &cred).await?;
    let hs = headers(spec, &token);
    let sid = create_session(http, spec, &hs).await?;
    // 真机挑战必然可解；万一 wasm 未求得答案（挑战过期），换一张重试。
    //
    // POW 是纯 CPU 的 wasm 解释执行（一次 1~2 秒），必须扔到 blocking 线程池：
    // 直接在 async 上下文里算会把 tokio worker 钉死，几个并发请求就能拖垮整个网关。
    let mut pow = String::new();
    let mut last_err: Option<RelayError> = None;
    for _ in 0..3 {
        let ch = create_challenge(http, spec, &hs).await?;
        let ch_for_solve = ch.clone();
        let solved = tokio::task::spawn_blocking(move || pow_header(&ch_for_solve))
            .await
            .map_err(|e| er(502, format!("POW 求解线程失败：{e}"), ErrorClass::Transient))?;
        match solved {
            Ok(p) => {
                pow = p;
                last_err = None;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    let mut hs2 = hs.clone();
    hs2.retain(|(k, _)| k != "x-ds-pow-response");
    hs2.push(("x-ds-pow-response".to_string(), pow));
    let mut payload = json!({
        "chat_session_id": sid,
        "parent_message_id": null,
        "prompt": prompt_text,
        "ref_file_ids": [],
        "thinking_enabled": thinking,
        "search_enabled": search,
        "preempt": false,
    });
    if expert {
        payload["model_type"] = json!("expert");
    }
    Ok(Prepared {
        base: base_url(spec),
        hs: hs2,
        payload,
        prompt_text,
        model,
    })
}
// ---------------------------------------------------------------- 对外入口

/// 测试用短路开关：默认置位，pow_header 不再真正调用 wasm 求解。
/// wasm 只对服务端真实挑战可解，合成挑战会空转到迭代上限（可达数十秒），
/// 所以单测一律跳过真求解；真实可用性由线上实测覆盖。
#[cfg(test)]
pub(crate) static SKIP_POW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 非流式：聚合整条补丁流成 OpenAI chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let p = prepare(http, spec, secret, body).await?;
    let url = format!("{}/v0/chat/completion", p.base);
    let resp = apply(http.post(&url), &p.hs)
        .json(&p.payload)
        .timeout(Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| er(502, format!("DeepSeek 对话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let class = match status {
            401 | 403 => ErrorClass::Auth,
            402 => ErrorClass::Quota,
            429 => ErrorClass::Rate,
            400 => ErrorClass::Rejected,
            _ => ErrorClass::Transient,
        };
        return Err(er(
            status,
            format!("DeepSeek 上游 {status}：{}", truncate(&text, 400)),
            class,
        ));
    }
    let mut st = PatchState::default();
    for block in text.split("\n\n") {
        if let Some(d) = data_of(block) {
            apply_frame(&mut st, d);
        }
    }
    if st.content.is_empty() && st.reasoning.is_empty() {
        return Err(er(
            401,
            format!(
                "DeepSeek 未返回正文（token 可能已失效，请在 chat.deepseek.com 重新登录后复制 token）；上游原始响应：{}",
                truncate(&text, 300)
            ),
            ErrorClass::Auth,
        ));
    }
    let prompt_tokens = est_tokens(&p.prompt_text);
    let completion_tokens = est_tokens(&st.content) + est_tokens(&st.reasoning);
    let total = if st.usage > 0 {
        st.usage
    } else {
        prompt_tokens + completion_tokens
    };
    let mut message = json!({"role": "assistant", "content": st.content});
    if !st.reasoning.is_empty() {
        message["reasoning_content"] = json!(st.reasoning);
    }
    Ok(json!({
        "id": format!("chatcmpl-ds-{}", nanos_hex()),
        "object": "chat.completion",
        "created": secs(),
        "model": p.model,
        "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total,
        },
    }))
}

/// 流式：把补丁流改写成 OpenAI chunk 流。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<
    (
        std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
        tokio::sync::mpsc::Receiver<StreamStat>,
    ),
    RelayError,
> {
    let p = prepare(http, spec, secret, body).await?;
    let url = format!("{}/v0/chat/completion", p.base);
    let resp = apply(http.post(&url), &p.hs)
        .json(&p.payload)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| er(502, format!("DeepSeek 对话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        let class = match status {
            401 | 403 => ErrorClass::Auth,
            402 => ErrorClass::Quota,
            429 => ErrorClass::Rate,
            400 => ErrorClass::Rejected,
            _ => ErrorClass::Transient,
        };
        return Err(er(
            status,
            format!("DeepSeek 上游 {status}：{}", truncate(&text, 400)),
            class,
        ));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let in_bytes = body.len();
    tokio::spawn(async move {
        let rid = format!("chatcmpl-ds-{}", nanos_hex());
        let created = secs();
        let model_out = p.model.clone();
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
        let mut st = PatchState::default();
        let mut buf = String::new();
        let mut dec = crate::Utf8Decoder::new();
        let mut sent_role = false;
        let mut out_chars: usize = 0;
        let mut done_sent = false;
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
                let Some(data) = data_of(&block) else { continue };
                for (kind, delta) in apply_frame(&mut st, data) {
                    if !sent_role {
                        out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                        sent_role = true;
                    }
                    out_chars += delta.chars().count();
                    if kind == "reasoning_content" {
                        out.push_str(&mk(json!({"reasoning_content": delta}), None));
                    } else {
                        out.push_str(&mk(json!({"content": delta}), None));
                    }
                }
                if st.finished && !done_sent {
                    out.push_str(&mk(json!({}), Some("stop")));
                    stat.finish_reason = Some("stop".to_string());
                    done_sent = true;
                }
                if !out.is_empty()
                    && bytes_tx
                        .send(Ok(Bytes::from(std::mem::take(&mut out))))
                        .await
                        .is_err()
                {
                    break 'outer;
                }
            }
        }
        let mut tail = String::new();
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "DeepSeek 未返回正文（token 可能已失效，请在 chat.deepseek.com 重新登录后复制 token）".to_string();
        }
        if let Some(ec) = stat.error {
            let etype = match ec {
                ErrorClass::Rate => "rate_limit_error",
                ErrorClass::Auth => "authentication_error",
                ErrorClass::Quota => "insufficient_quota",
                _ => "api_error",
            };
            let msg = stat.error_msg.replace('"', "'");
            tail.push_str(&format!(
                "data: {}\n\n",
                json!({"error": {"message": msg, "type": etype}})
            ));
        } else if !done_sent {
            tail.push_str(&mk(json!({}), Some("stop")));
            stat.finish_reason = Some("stop".to_string());
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

/// 验活：GET /v0/users/current，2xx 且 code==0 即有效。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok(cred) = parse_cred(secret) else {
        return false;
    };
    let url = format!("{}/v0/users/current", base_url(spec));
    let hs = headers(spec, &cred.token);
    match apply(http.get(&url), &hs)
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => {
            if !r.status().is_success() {
                return false;
            }
            let text = r.text().await.unwrap_or_default();
            serde_json::from_str::<Value>(&text)
                .map(|v| api_code(&v) == 0)
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::Router;
    use std::collections::BTreeMap;

    // 真实抓包形状：快照 → 新 RESPONSE 片段 → 纯增量 → BATCH 收尾。
    const SSE: &str = r#"event: ready
data: {"request_message_id":1,"response_message_id":2,"model_type":"default"}

event: update_session
data: {"updated_at":1792603079.16}

data: {"v":{"response":{"message_id":2,"status":"WIP","accumulated_token_usage":0,"fragments":[{"id":2,"type":"THINK","content":"思"}]}}}

data: {"p":"response/fragments","o":"APPEND","v":[{"id":3,"type":"RESPONSE","content":"你"}]}

data: {"v":"好"}

data: {"p":"response","o":"BATCH","v":[{"p":"accumulated_token_usage","v":42},{"p":"quasi_status","v":"FINISHED"}]}

data: {"p":"response/status","o":"SET","v":"FINISHED"}

event: close
data: {"click_behavior":"none"}

"#;

    async fn spawn_mock(sse: &'static str) -> String {
        let app = Router::new().fallback(move |req: axum::extract::Request| async move {
            let path = req.uri().path().to_string();
            let as_json = |v: Value| {
                axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from(v.to_string()))
                    .unwrap()
            };
            if path.ends_with("/v0/users/current") {
                return as_json(json!({"code": 0, "data": {"biz_data": {"token": "tok-fresh"}}}));
            }
            if path.ends_with("/v0/chat_session/create") {
                return as_json(json!({"code": 0, "data": {"biz_data": {"chat_session": {"id": "sess-1"}}}}));
            }
            if path.ends_with("/v0/chat/create_pow_challenge") {
                return as_json(json!({"code": 0, "data": {"biz_data": {"challenge": {
                    "algorithm": "DeepSeekHashV1",
                    "challenge": "ab".repeat(32),
                    "salt": "cd".repeat(10),
                    "signature": "sig",
                    "difficulty": 144000.0,
                    "expire_at": 1792600000000i64,
                    "target_path": "/api/v0/chat/completion"
                }}}}));
            }
            if path.ends_with("/v0/chat/completion") {
                return axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from(sse.to_string()))
                    .unwrap();
            }
            as_json(json!({"code": 1, "msg": "unknown path"}))
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    fn spec_for(base: &str) -> ProviderSpec {
        ProviderSpec {
            id: "web-deepseek".into(),
            name: "DeepSeek 网页版".into(),
            enabled: true,
            auth: aigw_registry::AuthKind::Cookie,
            login_methods: Vec::new(),
            base_url: base.to_string(),
            format: "openai".into(),
            models: vec!["deepseek-chat".into()],
            models_policy: aigw_registry::ModelsPolicy::Static,
            stream_dialect: aigw_registry::StreamDialect::SseDelta,
            headers: BTreeMap::new(),
            chat_path: String::new(),
            models_path: String::new(),
            models_filter: String::new(),
            upstream_kind: "deepseek-web".into(),
            signer: String::new(),
            biz_err: vec![],
            hosts: vec![],
            login_url: String::new(),
            notes: String::new(),
            chat_supported: true,
            region: "global".into(),
        }
    }

    #[test]
    fn parse_cred_accepts_plain_json_and_cookie() {
        assert_eq!(parse_cred("  tok-abc ").unwrap().token, "tok-abc");
        assert_eq!(parse_cred(r#"{"token":"tok-j"}"#).unwrap().token, "tok-j");
        assert_eq!(
            parse_cred("a=1; token=tok-c; b=2").unwrap().token,
            "tok-c"
        );
        assert!(parse_cred("   ").is_err());
        assert!(parse_cred("{\"nope\":1}").is_err());
    }

    #[test]
    fn flatten_puts_system_first_and_labels_turns() {
        let v = json!({"messages": [
            {"role": "system", "content": "你是助手"},
            {"role": "user", "content": "你好"},
            {"role": "assistant", "content": [{"type": "text", "text": "在"}]},
            {"role": "user", "content": "继续"}
        ]});
        let p = flatten(&v);
        assert!(p.starts_with("你是助手"));
        assert!(p.contains("User: 你好"));
        assert!(p.contains("Assistant: 在"));
        assert!(p.ends_with("User: 继续"));
    }

    #[test]
    fn read_request_detects_expert_thinking_and_search() {
        let (m, expert, thinking, search) = read_request(&json!({"model": "deepseek-v4-pro"}));
        assert_eq!(m, "deepseek-v4-pro");
        assert!(expert && !thinking && !search);
        let (_, expert, thinking, _) = read_request(&json!({"model": "deepseek-reasoner"}));
        assert!(!expert && thinking);
        let (_, _, _, search) = read_request(&json!({"model": "deepseek-web-search"}));
        assert!(search);
    }

    #[test]
    fn apply_frame_handles_snapshot_append_and_batch() {
        let mut st = PatchState::default();
        let d1 = apply_frame(&mut st, r#"{"v":{"response":{"fragments":[{"id":2,"type":"THINK","content":"思"}]}}}"#);
        assert_eq!(d1, vec![("reasoning_content".to_string(), "思".to_string())]);
        let d2 = apply_frame(&mut st, r#"{"p":"response/fragments","o":"APPEND","v":[{"id":3,"type":"RESPONSE","content":"你"}]}"#);
        assert_eq!(d2, vec![("content".to_string(), "你".to_string())]);
        let d3 = apply_frame(&mut st, r#"{"v":"好"}"#);
        assert_eq!(d3, vec![("content".to_string(), "好".to_string())]);
        apply_frame(&mut st, r#"{"p":"response","o":"BATCH","v":[{"p":"accumulated_token_usage","v":42},{"p":"quasi_status","v":"FINISHED"}]}"#);
        assert_eq!(st.content, "你好");
        assert_eq!(st.reasoning, "思");
        assert_eq!(st.usage, 42);
        assert!(st.finished);
    }

    #[test]
    fn data_of_skips_event_headers() {
        assert_eq!(data_of("event: ready\ndata: {\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(data_of("event: close"), None);
        assert_eq!(data_of("data: [DONE]"), None);
    }

    #[test]
    fn pow_header_rejects_unknown_algorithm() {
        let e = pow_header(&json!({"algorithm": "Nope", "challenge": "ab", "salt": "cd"}));
        assert!(e.is_err());
        assert!(e.unwrap_err().body.contains("不支持的 POW 算法"));
    }

    #[test]
    fn wasm_solver_runs_without_trapping() {
        // 实测：wasm 只对服务端真实挑战可解 —— 合成挑战即使把 difficulty 抬到 5,000,000
        // 也稳定返回 status=0（参考实现的 JS 行为完全一致），所以这里只验证调用约定
        // （malloc / 栈指针 / F64 传参 / 内存读写）不 trap，且耗时有界。
        let prefix = format!("{}_{}_", "cd".repeat(10), 1792600000000i64);
        let t0 = std::time::Instant::now();
        let r = solve_pow(&"ab".repeat(32), &prefix, 1000.0);
        assert!(t0.elapsed().as_secs() < 5, "POW 求解耗时过长");
        if let Err(e) = r {
            assert!(e.contains("POW 未求得答案"), "非预期的失败：{e}");
        }
    }

    /// 真机探针：`DS_CHAL_FILE=C:/aigw-target/chal.json cargo test probe_real_challenge`
    /// 用服务端真实挑战校验 wasm 调用约定（合成挑战一律无解，只能这样验）。
    #[test]
    fn probe_real_challenge_from_file() {
        let Ok(path) = std::env::var("DS_CHAL_FILE") else {
            return;
        };
        let raw = std::fs::read_to_string(path).expect("读不到挑战文件");
        let v: Value = serde_json::from_str(&raw).expect("挑战文件不是 JSON");
        let ch = v["challenge"].as_str().expect("缺 challenge");
        let salt = v["salt"].as_str().expect("缺 salt");
        let expire = v["expire_at"].as_i64().expect("缺 expire_at");
        let diff = v["difficulty"].as_f64().expect("缺 difficulty");
        let prefix = format!("{salt}_{expire}_");
        let t0 = std::time::Instant::now();
        let ans = solve_pow(ch, &prefix, diff).expect("真机挑战应当可解");
        println!(
            "PROBE answer={ans} expected={:?} ms={}",
            v["answer"].as_f64(),
            t0.elapsed().as_millis()
        );
    }

    #[tokio::test]
    async fn deepseek_nonstream_via_mock() {
        let base = spawn_mock(SSE).await;
        let http = reqwest::Client::new();
        let spec = spec_for(&base);
        let body = json!({"model": "deepseek-chat", "messages": [{"role": "user", "content": "你好"}]});
        let v = chat_nonstream(&http, &spec, "tok-1", body.to_string().as_bytes())
            .await
            .expect("非流式应成功");
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "你好");
        assert_eq!(v["choices"][0]["message"]["reasoning_content"], "思");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["usage"]["total_tokens"], 42);
    }

    #[tokio::test]
    async fn deepseek_stream_via_mock() {
        let base = spawn_mock(SSE).await;
        let http = reqwest::Client::new();
        let spec = spec_for(&base);
        let body = json!({"model": "deepseek-chat", "stream": true, "messages": [{"role": "user", "content": "你好"}]});
        let (mut stream, mut rx) = chat_stream(&http, &spec, "tok-1", body.to_string().as_bytes())
            .await
            .expect("流式应成功");
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            if let Ok(b) = item {
                text.push_str(&String::from_utf8_lossy(&b));
            }
        }
        assert!(text.contains("chat.completion.chunk"));
        assert!(text.contains("reasoning_content"));
        assert!(text.contains("你"));
        assert!(text.contains("\"finish_reason\":\"stop\""));
        assert!(text.trim_end().ends_with("data: [DONE]"));
        let stat = rx.recv().await.expect("应有记账");
        assert_eq!(stat.finish_reason.as_deref(), Some("stop"));
        assert!(stat.error.is_none());
    }

    #[tokio::test]
    async fn deepseek_verify_hits_users_current() {
        let base = spawn_mock(SSE).await;
        let http = reqwest::Client::new();
        let spec = spec_for(&base);
        assert!(verify(&http, &spec, "tok-1").await);
    }
}
