//! Trae（字节 SOLO / Trae Work）adapter —— 纯 HTTP，Cloud-IDE-JWT + SOLO 自定义 SSE。
//!
//! 一手来源：_refs/JeffHu0912-trae2api（Go，纯协议实现，未依赖客户端）。
//! 链路：
//! 1. `POST https://trae-api-cn.mchost.guru/api/agent/v3/llm_utils_chat`，头组见 `headers()`
//!    （Authorization: "Cloud-IDE-JWT <jwt>" + X-Cloudide-Token / X-Ide-Token 同值 + X-App-Id 等）；
//! 2. 请求体把 OpenAI 形状改写成 SOLO 形状：`function="solo_work_lite"`，`stream` 强制 true，
//!    `model` 同时写进 `config_name`，messages[].content 字符串 → `[{"type":"text","text":…}]`，
//!    tools[].function.parameters 对象 → **JSON 字符串**（上游 Go struct 是 string 类型）；
//! 3. 响应是 SOLO 自定义 SSE：`event: output` 携带 response / reasoning_content / tool_calls 增量，
//!    `event: token_usage`、`event: done`（finish_reason）、`event: error`（code+message）。
//!
//! 登录（跳转回调 18080 + ExchangeToken）已在 auth_login.rs 的 trae_start / trae_exchange 实现。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

const TRAE_AGENT: &str = "https://trae-api-cn.mchost.guru";
const TRAE_OAUTH: &str = "https://api.trae.com.cn";
const TRAE_CLIENT_ID: &str = "en1oxy7wnw8j9n";
const TRAE_APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
const TRAE_IDE_VERSION: &str = "0.1.52";
const TRAE_IDE_VERSION_CODE: &str = "20260811";
const TRAE_FUNCTION: &str = "solo_work_lite";
const TRAE_DEFAULT_MODEL: &str = "glm-5.2";
const EP_CHAT: &str = "/api/agent/v3/llm_utils_chat";
const EP_EXCHANGE: &str = "/cloudide/api/v3/trae/oauth/ExchangeToken";
const EP_USERINFO: &str = "/cloudide/api/v3/trae/GetUserInfo";

fn er(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError { status, body: body.into(), class }
}

fn secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rid() -> String {
    let a: u64 = rand::random();
    let b: u64 = rand::random();
    format!("{a:016x}{b:016x}")
}

fn trunc(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() > n { t.chars().take(n).collect() } else { t.to_string() }
}

// ══════════════════════════════════════════════════════════════
// 凭据
// ══════════════════════════════════════════════════════════════

/// Trae 凭据：accessToken 必填；其余字段能补就补（X-Device-Id / X-Machine-Id / X-Uid）。
#[derive(Debug, Clone, Default)]
pub struct TraeCred {
    pub token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub uid: String,
    pub machine_id: String,
    pub device_id: String,
    pub api_host: String,
}

fn pick(v: &Value, keys: &[&str]) -> Option<String> {
    let o = v.as_object()?;
    for k in keys {
        if let Some(x) = o.get(*k) {
            if let Some(s) = x.as_str() {
                if !s.trim().is_empty() {
                    return Some(s.trim().to_string());
                }
            }
        }
    }
    None
}

fn pick_i64(v: &Value, keys: &[&str]) -> i64 {
    let Some(o) = v.as_object() else { return 0 };
    for k in keys {
        if let Some(x) = o.get(*k) {
            if let Some(n) = x.as_i64() {
                return if n > 1_000_000_000_000 { n / 1000 } else { n };
            }
            if let Some(s) = x.as_str() {
                if let Ok(n) = s.trim().parse::<i64>() {
                    return if n > 1_000_000_000_000 { n / 1000 } else { n };
                }
            }
        }
    }
    0
}

fn cred_from_object(v: &Value) -> TraeCred {
    TraeCred {
        token: pick(v, &["accessToken", "access_token", "token", "ideToken", "ide_token"]).unwrap_or_default(),
        refresh_token: pick(v, &["refreshToken", "refresh_token"]).unwrap_or_default(),
        expires_at: pick_i64(v, &["expiresAt", "expires_at"]),
        uid: pick(v, &["uid", "user_id", "userId", "UserID"]).unwrap_or_default(),
        machine_id: pick(v, &["machineId", "machine_id"]).unwrap_or_default(),
        device_id: pick(v, &["deviceId", "device_id"]).unwrap_or_default(),
        api_host: pick(v, &["apiHost", "api_host"]).unwrap_or_default(),
    }
}

/// 解析凭据：
/// - 嵌套形 `{"auth":{accessToken,…},"account":{uid,…}}`（Trae 客户端 storage.json / 本面板登录产出）
/// - 扁平形 `{"accessToken":…,"uid":…}`
/// - 裸 token 串
pub fn parse_cred(secret: &str) -> Result<TraeCred, String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("Trae 凭据为空：请粘贴 accessToken（Cloud-IDE-JWT）或客户端 storage.json 片段".into());
    }
    let cred = if s.starts_with('{') {
        let v: Value = serde_json::from_str(s).map_err(|e| format!("Trae 凭据不是合法 JSON：{e}"))?;
        if let Some(auth) = v.get("auth").filter(|x| x.is_object()) {
            let mut c = cred_from_object(auth);
            if let Some(acc) = v.get("account") {
                if c.uid.is_empty() {
                    c.uid = pick(acc, &["uid", "userId", "UserID"]).unwrap_or_default();
                }
            }
            c
        } else {
            cred_from_object(&v)
        }
    } else {
        TraeCred { token: s.to_string(), ..Default::default() }
    };
    if cred.token.is_empty() {
        return Err("Trae 凭据里没有 accessToken（需要 Cloud-IDE-JWT）".into());
    }
    Ok(cred)
}

/// 对话主机：spec.base_url 显式给 http:// 或 127.0.0.1 时用它（测试注入），否则官方。
fn agent_base(spec: &ProviderSpec) -> String {
    let b = spec.base_url.trim();
    if b.starts_with("http://") || b.contains("127.0.0.1") {
        return b.trim_end_matches('/').to_string();
    }
    TRAE_AGENT.to_string()
}

/// SOLO 专属头组（实测必须，缺一个就 4xx）。
fn headers(spec: &ProviderSpec, cred: &TraeCred, stream: bool) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("content-type".into(), "application/json".into()),
        (
            "accept".into(),
            if stream { "text/event-stream".into() } else { "application/json".into() },
        ),
        ("user-agent".into(), format!("Trae/{TRAE_IDE_VERSION}")),
        ("authorization".into(), format!("Cloud-IDE-JWT {}", cred.token)),
        ("x-cloudide-token".into(), cred.token.clone()),
        ("x-ide-token".into(), cred.token.clone()),
        ("x-app-id".into(), TRAE_APP_ID.into()),
        ("x-app-version".into(), "default".into()),
        ("x-ide-version".into(), TRAE_IDE_VERSION.into()),
        ("x-ide-version-code".into(), TRAE_IDE_VERSION_CODE.into()),
        ("x-app-version-code".into(), TRAE_IDE_VERSION_CODE.into()),
        ("x-ide-version-type".into(), "stable".into()),
        ("x-device-type".into(), "macos".into()),
        ("x-os-version".into(), "macOS 15.7.4".into()),
        ("x-device-brand".into(), "Apple".into()),
        ("request-traffic-type".into(), "prod".into()),
    ];
    if !cred.uid.is_empty() {
        hs.push(("x-uid".into(), cred.uid.clone()));
    }
    if !cred.machine_id.is_empty() {
        hs.push(("x-machine-id".into(), cred.machine_id.clone()));
    }
    if !cred.device_id.is_empty() {
        hs.push(("x-device-id".into(), cred.device_id.clone()));
    }
    for (k, v) in &spec.headers {
        let lk = k.to_ascii_lowercase();
        if !v.is_empty() && !hs.iter().any(|(x, _)| *x == lk) {
            hs.push((lk, v.clone()));
        }
    }
    hs
}

fn apply(rb: reqwest::RequestBuilder, hs: &[(String, String)]) -> reqwest::RequestBuilder {
    let mut rb = rb;
    for (k, v) in hs {
        rb = rb.header(k, v);
    }
    rb
}


// ══════════════════════════════════════════════════════════════
// 请求体改写：OpenAI → SOLO
// ══════════════════════════════════════════════════════════════

/// 单 pass 改写（照 JeffHu0912 payload.go PrepareBody）。
pub fn prepare_body(v: &Value) -> Value {
    let mut obj = v.clone();
    if !obj.is_object() {
        return obj;
    }
    obj["stream"] = json!(true);
    obj["function"] = json!(TRAE_FUNCTION);

    // messages：content 字符串 → [{type:text,text}]；assistant.tool_calls.function → function_call
    if let Some(msgs) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for m in msgs.iter_mut() {
            let Some(mo) = m.as_object_mut() else { continue };
            let role = mo.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
            if role == "assistant" {
                if let Some(tcs) = mo.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                    let mut kept: Vec<Value> = Vec::new();
                    for tc in tcs.iter_mut() {
                        let Some(tco) = tc.as_object_mut() else { continue };
                        if let Some(f) = tco.remove("function") {
                            tco.insert("function_call".into(), f);
                        }
                        let named = tco
                            .get("function_call")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(|n| !n.trim().is_empty())
                            .unwrap_or(false);
                        if named {
                            kept.push(tc.clone());
                        }
                    }
                    if kept.is_empty() {
                        mo.remove("tool_calls");
                    } else {
                        mo.insert("tool_calls".into(), Value::Array(kept));
                    }
                }
            }
            if let Some(c) = mo.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()) {
                mo.insert("content".into(), json!([{"type": "text", "text": c}]));
            }
        }
    }

    // model → config_name + model
    let mut model = obj.get("model").and_then(|m| m.as_str()).unwrap_or("").trim().to_string();
    if model.is_empty() {
        model = TRAE_DEFAULT_MODEL.to_string();
    }
    obj["config_name"] = json!(model);
    obj["model"] = json!(model);

    normalize_tool_choice(&mut obj);
    normalize_tools(&mut obj);
    obj
}

fn normalize_tool_choice(obj: &mut Value) {
    let Some(mo) = obj.as_object_mut() else { return };
    let Some(tc) = mo.get("tool_choice").cloned() else { return };
    let drop_tools = |mo: &mut serde_json::Map<String, Value>| {
        mo.remove("tools");
        mo.remove("functions");
    };
    match &tc {
        Value::String(s) => {
            if s.trim().eq_ignore_ascii_case("none") {
                mo.remove("tool_choice");
                drop_tools(mo);
            }
        }
        Value::Object(o) => {
            let typ = o
                .get("type")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            match typ.as_str() {
                "none" => {
                    mo.remove("tool_choice");
                    drop_tools(mo);
                }
                "auto" | "required" => {
                    mo.insert("tool_choice".into(), json!(typ));
                }
                "function" => {
                    let name = o
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .or_else(|| o.get("name").and_then(|n| n.as_str()))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if name.is_empty() {
                        mo.insert("tool_choice".into(), json!("auto"));
                    } else {
                        mo.insert("tool_choice".into(), json!(name));
                    }
                }
                _ => {
                    mo.remove("tool_choice");
                }
            }
        }
        _ => {
            mo.remove("tool_choice");
        }
    }
}

/// tools[].function.parameters 对象 → JSON 字符串（上游 Go struct 是 string 类型）。
fn normalize_tools(obj: &mut Value) {
    let Some(mo) = obj.as_object_mut() else { return };
    let Some(list) = mo.get("tools").and_then(|t| t.as_array()).cloned() else { return };
    if list.is_empty() {
        return;
    }
    let mut out: Vec<Value> = Vec::new();
    for item in list {
        let Some(mut t) = item.as_object().cloned() else { continue };
        let Some(f) = t.get_mut("function").and_then(|f| f.as_object_mut()) else { continue };
        if let Some(p) = f.get("parameters") {
            if p.is_object() {
                let s = serde_json::to_string(p).unwrap_or_default();
                f.insert("parameters".into(), json!(s));
            }
        }
        out.push(Value::Object(t));
    }
    if out.is_empty() {
        mo.remove("tools");
    } else {
        mo.insert("tools".into(), Value::Array(out));
    }
}


// ══════════════════════════════════════════════════════════════
// SOLO SSE 解析
// ══════════════════════════════════════════════════════════════

/// 一行 SSE 的状态机（event/data 跨行累积，空行触发派发）。
#[derive(Default)]
struct SoloState {
    event: String,
    data: String,
}

/// 处理一行；返回 (event 名, data 原文) 当且仅当遇到事件边界。
fn scan_line(st: &mut SoloState, line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        if st.event.is_empty() && st.data.is_empty() {
            return None;
        }
        let ev = st.event.trim().to_string();
        let data = st.data.trim().to_string();
        st.event.clear();
        st.data.clear();
        return Some((ev, data));
    }
    if let Some(rest) = line.strip_prefix("event:") {
        st.event = rest.trim().to_string();
    } else if let Some(rest) = line.strip_prefix("data:") {
        if !st.data.is_empty() {
            st.data.push('\n');
        }
        st.data.push_str(rest);
    }
    None
}

/// 单帧解析结果。
#[derive(Default, Debug)]
pub struct SoloEvent {
    pub event: String,
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Option<Value>,
    pub usage: Option<Value>,
    pub finish_reason: String,
    pub error_code: i64,
    pub error_message: String,
}

/// 把 (event, data) 解析成 SoloEvent。
pub fn parse_event(event: &str, data: &str) -> SoloEvent {
    let mut ev = SoloEvent { event: event.trim().to_string(), ..Default::default() };
    if data.is_empty() {
        return ev;
    }
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return ev;
    };
    match ev.event.as_str() {
        "output" => {
            if let Some(s) = v.get("response").and_then(|x| x.as_str()) {
                ev.content = s.to_string();
            }
            if let Some(s) = v.get("reasoning_content").and_then(|x| x.as_str()) {
                ev.reasoning = s.to_string();
            }
            if let Some(tc) = v.get("tool_calls") {
                if !tc.is_null() {
                    ev.tool_calls = Some(tc.clone());
                }
            }
        }
        "token_usage" => ev.usage = Some(v),
        "done" => {
            if let Some(s) = v.get("finish_reason").and_then(|x| x.as_str()) {
                ev.finish_reason = s.to_string();
            }
        }
        "error" => {
            ev.error_code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
            ev.error_message = v.get("message").and_then(|x| x.as_str()).unwrap_or("").to_string();
        }
        _ => {}
    }
    ev
}

/// SOLO 业务错误码 → 错误类（照 JeffHu0912 SOLOStreamError.Kind）。
fn solo_code_class(code: i64) -> ErrorClass {
    match code {
        1005 | 4008 => ErrorClass::Quota,
        4011 => ErrorClass::Rate,
        _ => ErrorClass::Rejected,
    }
}

fn relay_class(status: u16) -> ErrorClass {
    match status {
        401 | 403 => ErrorClass::Auth,
        402 => ErrorClass::Quota,
        429 => ErrorClass::Rate,
        400..=499 => ErrorClass::Rejected,
        _ => ErrorClass::Transient,
    }
}

/// 合并流式 tool_call 片段（照 JeffHu0912 mergeToolCallDelta）。
pub fn merge_tool_calls(acc: &mut Vec<Value>, incoming: &Value) {
    let arr: Vec<Value> = if let Some(a) = incoming.as_array() {
        a.clone()
    } else if incoming.is_object() {
        vec![incoming.clone()]
    } else {
        return;
    };
    for call in arr {
        let Some(co) = call.as_object() else { continue };
        let idx = co.get("index").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as usize;
        while acc.len() <= idx {
            acc.push(Value::Null);
        }
        if !acc[idx].is_object() {
            acc[idx] = json!({"index": idx});
        }
        let merged = acc[idx].as_object_mut().unwrap();
        if let Some(s) = co.get("id").and_then(|x| x.as_str()) {
            if !s.is_empty() {
                merged.insert("id".into(), json!(s));
            }
        }
        if let Some(s) = co.get("type").and_then(|x| x.as_str()) {
            if !s.is_empty() {
                merged.insert("type".into(), json!(s));
            }
        }
        let df = co
            .get("function")
            .or_else(|| co.get("function_call"))
            .and_then(|f| f.as_object());
        let Some(df) = df else { continue };
        let mf = merged
            .entry("function".to_string())
            .or_insert_with(|| json!({}));
        let Some(mfo) = mf.as_object_mut() else { continue };
        if let Some(n) = df.get("name").and_then(|x| x.as_str()) {
            if !n.is_empty() {
                mfo.insert("name".into(), json!(n));
            }
        }
        if let Some(a) = df.get("arguments").and_then(|x| x.as_str()) {
            if !a.is_empty() {
                let prev = mfo.get("arguments").and_then(|x| x.as_str()).unwrap_or("").to_string();
                mfo.insert("arguments".into(), json!(format!("{prev}{a}")));
            }
        }
        // 清掉 SOLO 专属字段
        mfo.remove("namespace");
        mfo.remove("partial_arguments");
    }
}


// ══════════════════════════════════════════════════════════════
// 上游调用
// ══════════════════════════════════════════════════════════════

/// OAuth 头组（登录态接口用，只有 UA + Content-Type）。
fn oauth_headers() -> Vec<(String, String)> {
    vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/json".into()),
        ("user-agent".into(), format!("Trae/{TRAE_IDE_VERSION}")),
    ]
}

/// 把 TraeCred 序列化回账号池用的 secret 字符串。
///
/// 只写非空字段：账号池里存的是用户粘进来的原文（可能是裸 token、也可能是
/// 客户端 storage.json 的嵌套片段），刷新后整段重写会把用户没提供的字段（如
/// machine_id）抹成空串，反而让上游拒绝。
pub fn cred_to_secret(c: &TraeCred) -> String {
    let mut m = serde_json::Map::new();
    m.insert("accessToken".into(), json!(c.token));
    if !c.refresh_token.is_empty() {
        m.insert("refreshToken".into(), json!(c.refresh_token));
    }
    if c.expires_at > 0 {
        m.insert("expiresAt".into(), json!(c.expires_at));
    }
    if !c.uid.is_empty() {
        m.insert("uid".into(), json!(c.uid));
    }
    if !c.machine_id.is_empty() {
        m.insert("machineId".into(), json!(c.machine_id));
    }
    if !c.device_id.is_empty() {
        m.insert("deviceId".into(), json!(c.device_id));
    }
    if !c.api_host.is_empty() {
        m.insert("apiHost".into(), json!(c.api_host));
    }
    Value::Object(m).to_string()
}

/// 强制续期一次并返回**可直接写回账号池**的新 secret。
///
/// 与 `ensure_token` 不同：不管 expires_at 到没到都续（401/403 自愈场景就是要无条件换）。
/// server 层拿到 401/403 时调它，成功后 `AccountStore::update_secret` 落盘再重试转发。
pub async fn refresh_secret(http: &reqwest::Client, secret: &str) -> Result<String, String> {
    let cred = parse_cred(secret).map_err(|e| e)?;
    let (new_cred, _) = refresh_token(http, &cred).await.map_err(|e| e.body)?;
    Ok(cred_to_secret(&new_cred))
}

/// 用 refreshToken 换新 accessToken（会轮换整族，谨慎调用）。
///
/// 返回 `(新凭据, 是否真的续期了)`：trae 上游会轮换 RefreshToken，新的一族必须
/// 回写账号池，否则下次请求还拿旧 rt 去换 —— 旧 rt 一旦被上游作废，这个账号就
/// 只能重新走一遍跳转登录。
async fn refresh_token(http: &reqwest::Client, cred: &TraeCred) -> Result<(TraeCred, bool), RelayError> {
    if cred.refresh_token.is_empty() {
        return Err(er(401, "Trae 凭据已失效且没有 refreshToken，请重新走跳转登录", ErrorClass::Auth));
    }
    let host = if cred.api_host.is_empty() { TRAE_OAUTH.to_string() } else { cred.api_host.clone() };
    let url = format!("{}{EP_EXCHANGE}", host.trim_end_matches('/'));
    let body = json!({
        "ClientID": TRAE_CLIENT_ID,
        "RefreshToken": cred.refresh_token,
        "ClientSecret": "-",
        "UserID": ""
    });
    let resp = apply(
        http.post(&url)
            .header("x-app-id", TRAE_APP_ID)
            .timeout(std::time::Duration::from_secs(30)),
        &oauth_headers(),
    )
    .body(body.to_string())
    .send()
    .await
    .map_err(|e| er(502, format!("Trae 续期请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(er(status, format!("Trae 续期失败（{status}）：{}", trunc(&text, 300)), relay_class(status)));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| er(502, format!("Trae 续期响应不是 JSON：{e}；head={}", trunc(&text, 200)), ErrorClass::Transient))?;
    let res = v.get("Result").or_else(|| v.get("result")).unwrap_or(&v);
    let token = pick(res, &["Token", "token", "accessToken", "access_token"]).unwrap_or_default();
    if token.is_empty() {
        return Err(er(401, format!("Trae 续期响应无 token：{}", trunc(&text, 300)), ErrorClass::Auth));
    }
    let mut out = cred.clone();
    out.token = token;
    if let Some(r) = pick(res, &["RefreshToken", "refresh_token"]) {
        out.refresh_token = r;
    }
    let exp = pick_i64(res, &["TokenExpireAt", "tokenExpireAt", "expiresAt", "expires_at"]);
    if exp > 0 {
        out.expires_at = exp;
    }
    Ok((out, true))
}

/// 提前 1 小时续期；过期信息缺失则原样使用。
///
/// 返回值第二项表示「本次真的续期了」——调用方（server 层）据此把新 secret
/// 回写账号池；上游轮换 RefreshToken 时不回写，旧 rt 被作废后账号即死。
pub async fn ensure_token(http: &reqwest::Client, cred: &TraeCred) -> Result<(TraeCred, bool), RelayError> {
    let now = secs() as i64;
    let need = cred.expires_at > 0 && cred.expires_at - now < 3600;
    if need && !cred.refresh_token.is_empty() {
        return refresh_token(http, cred).await;
    }
    Ok((cred.clone(), false))
}

async fn post_chat(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cred: &TraeCred,
    body: &str,
    timeout: std::time::Duration,
) -> Result<reqwest::Response, RelayError> {
    let url = format!("{}{EP_CHAT}", agent_base(spec));
    let rb = http.post(&url).timeout(timeout);
    let rb = apply(rb, &headers(spec, cred, true));
    let resp = rb.body(body.to_string()).send().await.map_err(|e| {
        er(
            502,
            format!("Trae 上游请求失败：{e}"),
            ErrorClass::Transient,
        )
    })?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    Err(er(
        status,
        format!("Trae 上游 {status}：{}", trunc(&text, 300)),
        relay_class(status),
    ))
}


/// 非流式：聚合 SOLO 事件流成 chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    raw: &[u8],
) -> Result<Value, RelayError> {
    let req_model = serde_json::from_slice::<Value>(raw)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let v: Value = serde_json::from_slice(raw)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let (cred, _refreshed) = ensure_token(http, &parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?).await?;
    let prepared = prepare_body(&v);
    let req_model_out = if req_model.is_empty() {
        prepared.get("model").and_then(|m| m.as_str()).unwrap_or(TRAE_DEFAULT_MODEL).to_string()
    } else {
        req_model
    };
    let resp = post_chat(
        http,
        spec,
        &cred,
        &prepared.to_string(),
        std::time::Duration::from_secs(180),
    )
    .await?;
    let text = resp.text().await.unwrap_or_default();

    let mut st = SoloState::default();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut usage: Option<Value> = None;
    let mut finish: Option<String> = None;
    for line in text.lines() {
        let Some((ev_name, data)) = scan_line(&mut st, line) else { continue };
        let ev = parse_event(&ev_name, &data);
        if ev.error_code != 0 || !ev.error_message.is_empty() {
            return Err(er(
                if ev.error_code == 4011 { 429 } else { 502 },
                format!("Trae 上游错误 {}：{}", ev.error_code, ev.error_message),
                solo_code_class(ev.error_code),
            ));
        }
        content.push_str(&ev.content);
        reasoning.push_str(&ev.reasoning);
        if let Some(tc) = &ev.tool_calls {
            merge_tool_calls(&mut calls, tc);
        }
        if let Some(u) = ev.usage {
            usage = Some(u);
        }
        if !ev.finish_reason.is_empty() {
            finish = Some(ev.finish_reason);
        }
    }
    if content.is_empty() && reasoning.is_empty() && calls.is_empty() {
        return Err(er(
            401,
            format!(
                "Trae 未返回正文（token 可能已失效）。上游原文：{}",
                trunc(&text, 300)
            ),
            ErrorClass::Auth,
        ));
    }

    let mut message = json!({"role": "assistant", "content": content});
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    let mut out = json!({
        "id": format!("chatcmpl-trae-{}", rid()),
        "object": "chat.completion",
        "created": secs(),
        "model": req_model_out,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish.unwrap_or_else(|| "stop".to_string()),
        }],
    });
    let c_chars = out
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|m| m.as_str())
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let (tin_est, tout_est) = crate::estimate_stream_tokens(raw.len(), c_chars);
    let u = match usage {
        Some(mut u) => {
            if u.get("prompt_tokens").is_none() {
                u["prompt_tokens"] = json!(tin_est);
            }
            if u.get("completion_tokens").is_none() {
                u["completion_tokens"] = json!(tout_est);
            }
            u
        }
        None => json!({"prompt_tokens": tin_est, "completion_tokens": tout_est, "total_tokens": tin_est + tout_est}),
    };
    out["usage"] = u;
    Ok(out)
}


/// 流式：SOLO 事件 → OpenAI chunk + `[DONE]`。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    raw: &[u8],
) -> Result<
    (
        std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
        mpsc::Receiver<StreamStat>,
    ),
    RelayError,
> {
    let req_model = serde_json::from_slice::<Value>(raw)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let v: Value = serde_json::from_slice(raw)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let (cred, _refreshed) = ensure_token(http, &parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?).await?;
    let prepared = prepare_body(&v);
    let model_out = if req_model.is_empty() {
        prepared.get("model").and_then(|m| m.as_str()).unwrap_or(TRAE_DEFAULT_MODEL).to_string()
    } else {
        req_model
    };
    let resp = post_chat(
        http,
        spec,
        &cred,
        &prepared.to_string(),
        std::time::Duration::from_secs(300),
    )
    .await?;

    let in_bytes = raw.len();
    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let id = format!("chatcmpl-trae-{}", rid());
        let created = secs();
        let model_for_chunk = model_out.clone();
        let mut stat = StreamStat::default();
        let mut out_chars = 0usize;
        let mut sent_role = false;
        let mut done_sent = false;
        let mk = |delta: Value, finish: Option<&str>| -> String {
            let mut choice = json!({"index": 0, "delta": delta});
            if let Some(f) = finish {
                choice["finish_reason"] = json!(f);
            } else {
                choice["finish_reason"] = Value::Null;
            }
            let chunk = json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_for_chunk,
                "choices": [choice],
            });
            format!("data: {chunk}\n\n")
        };

        let mut decoder = crate::Utf8Decoder::new();
        let mut buf = String::new();
        let mut st = SoloState::default();
        let mut calls: Vec<Value> = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut aborted = false;
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {e}");
                    aborted = true;
                    break;
                }
            };
            buf.push_str(&decoder.push(&chunk));
            let mut pending: Vec<String> = Vec::new();
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                pending.push(line);
            }
            for line in pending {
                let Some((ev_name, data)) = scan_line(&mut st, &line) else { continue };
                let ev = parse_event(&ev_name, &data);
                if ev.error_code != 0 || !ev.error_message.is_empty() {
                    let class = solo_code_class(ev.error_code);
                    stat.error = Some(class.clone());
                    stat.error_msg = format!("Trae 上游错误 {}：{}", ev.error_code, ev.error_message);
                    aborted = true;
                    let frame = json!({"error": {"message": stat.error_msg, "type": "upstream_error", "code": 502}});
                    let _ = bytes_tx.send(Ok(Bytes::from(format!("data: {frame}\n\ndata: [DONE]\n\n")))).await;
                    done_sent = true;
                    break;
                }
                let mut delta = serde_json::Map::new();
                if !sent_role {
                    delta.insert("role".into(), json!("assistant"));
                    sent_role = true;
                }
                if !ev.reasoning.is_empty() {
                    delta.insert("reasoning_content".into(), json!(ev.reasoning));
                }
                if !ev.content.is_empty() {
                    out_chars += ev.content.chars().count();
                    delta.insert("content".into(), json!(ev.content));
                }
                if let Some(tc) = &ev.tool_calls {
                    merge_tool_calls(&mut calls, tc);
                    if !calls.is_empty() {
                        delta.insert("tool_calls".into(), Value::Array(calls.clone()));
                    }
                }
                if let Some(u) = &ev.usage {
                    stat.tokens_in = u.get("prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                    stat.tokens_out = u.get("completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                }
                if !delta.is_empty() {
                    if bytes_tx.send(Ok(Bytes::from(mk(Value::Object(delta), None)))).await.is_err() {
                        stat.error = Some(ErrorClass::Transient);
                        stat.error_msg = "下游断开（客户端取消）".to_string();
                        stat.client_gone = true;
                        aborted = true;
                        break;
                    }
                }
                if !ev.finish_reason.is_empty() {
                    stat.finish_reason = Some(ev.finish_reason.clone());
                    let frame = mk(Value::Object(serde_json::Map::new()), Some(&ev.finish_reason));
                    let _ = bytes_tx.send(Ok(Bytes::from(format!("{frame}data: [DONE]\n\n")))).await;
                    done_sent = true;
                    stat.saw_done = true;
                }
            }
            if aborted || done_sent {
                break;
            }
        }
        if !aborted && !done_sent {
            let frame = mk(Value::Object(serde_json::Map::new()), Some("stop"));
            let _ = bytes_tx.send(Ok(Bytes::from(format!("{frame}data: [DONE]\n\n")))).await;
            stat.saw_done = true;
        }
        if stat.tokens_in == 0 && stat.tokens_out == 0 {
            let (tin, tout) = crate::estimate_stream_tokens(in_bytes, out_chars);
            stat.tokens_in = tin;
            stat.tokens_out = tout;
        }
        if stat.finish_reason.is_none() && stat.error.is_none() {
            stat.finish_reason = Some("stop".to_string());
        }
        let _ = tx.send(stat).await;
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    Ok((Box::pin(stream), rx))
}

/// 验活：GetUserInfo（OAuth 头 + X-Cloudide-Token）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok(cred) = parse_cred(secret) else { return false };
    let host = if cred.api_host.is_empty() { TRAE_OAUTH.to_string() } else { cred.api_host.clone() };
    let url = format!("{}{EP_USERINFO}", host.trim_end_matches('/'));
    let _ = spec;
    let body = json!({"ReqSource": "IDE", "IDEVersion": TRAE_IDE_VERSION});
    let rb = http
        .post(&url)
        .header("x-cloudide-token", cred.token.clone())
        .header("x-app-id", TRAE_APP_ID)
        .timeout(std::time::Duration::from_secs(30));
    let rb = apply(rb, &oauth_headers());
    let Ok(resp) = rb.body(body.to_string()).send().await else { return false };
    if !resp.status().is_success() {
        return false;
    }
    let text = resp.text().await.unwrap_or_default();
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return false };
    let res = v.get("Result").or_else(|| v.get("result")).unwrap_or(&v);
    let uid = pick(res, &["UserID", "userId", "uid"]).unwrap_or_default();
    let name = pick(res, &["ScreenName", "screenName", "nickname"]).unwrap_or_default();
    !uid.is_empty() || !name.is_empty()
}


#[cfg(test)]
mod tests {
    use super::*;

    fn spec_toml(base: &str) -> ProviderSpec {
        let raw = format!(
            r#"
id = "apk-trae"
name = "Trae"
auth = "api_key"
base_url = "{base}"
format = "openai"
models = ["GLM-5.3", "glm-5.2"]
models_policy = "static"
stream_dialect = "sse_delta"
upstream_kind = "trae"
"#
        );
        aigw_registry::parse_spec(&raw).expect("spec")
    }

    /// mock：SOLO 对话端点（SSE）+ GetUserInfo。
    async fn spawn_mock(stream_body: &'static str) -> String {
        use axum::routing::{any, post};
        let app = axum::Router::new()
            .route(
                "/api/agent/v3/llm_utils_chat",
                post(move || async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        stream_body,
                    )
                }),
            )
            .route(
                "/cloudide/api/v3/trae/GetUserInfo",
                any(|| async {
                    axum::Json(json!({"Result": {"UserID": "u-1", "ScreenName": "nick"}}))
                }),
            )
            .route(
                "/cloudide/api/v3/trae/oauth/ExchangeToken",
                any(|| async {
                    axum::Json(json!({"Result": {
                        "Token": "at-new",
                        "RefreshToken": "rt-new",
                        "TokenExpireAt": 1899999999
                    }}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    #[test]
    fn parse_cred_accepts_nested_flat_and_bare() {
        assert_eq!(parse_cred("jwt-abc").unwrap().token, "jwt-abc");
        let flat = parse_cred(r#"{"accessToken":"jwt-x","uid":"u-9","machineId":"m-1"}"#).unwrap();
        assert_eq!(flat.token, "jwt-x");
        assert_eq!(flat.uid, "u-9");
        assert_eq!(flat.machine_id, "m-1");
        let nested = parse_cred(
            r#"{"auth":{"accessToken":"jwt-y","refreshToken":"rt-1","expiresAt":1899999999000},"account":{"uid":"uid-7"}}"#,
        )
        .unwrap();
        assert_eq!(nested.token, "jwt-y");
        assert_eq!(nested.refresh_token, "rt-1");
        assert_eq!(nested.uid, "uid-7");
        assert_eq!(nested.expires_at, 1899999999);
        assert!(parse_cred("  ").is_err());
        assert!(parse_cred(r#"{"uid":"u-1"}"#).is_err());
    }

    #[test]
    fn prepare_body_rewrites_messages_and_tools() {
        let v = json!({
            "model": "GLM-5.3",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "Bash", "arguments": "{}"}},
                    {"id": "c2", "type": "function", "function": {"arguments": "{}"}}
                ]}
            ],
            "tools": [{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}],
            "tool_choice": {"type": "function", "function": {"name": "f"}}
        });
        let out = prepare_body(&v);
        assert_eq!(out["stream"], true);
        assert_eq!(out["function"], TRAE_FUNCTION);
        assert_eq!(out["config_name"], "GLM-5.3");
        assert_eq!(out["model"], "GLM-5.3");
        assert!(out["messages"][0]["content"].is_array());
        // tool_calls.function → function_call，且无 name 的条目被剔除
        let tcs = out["messages"][1]["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert!(tcs[0].get("function_call").is_some());
        assert!(tcs[0].get("function").is_none());
        // parameters 对象 → JSON 字符串
        assert!(out["tools"][0]["function"]["parameters"].is_string());
        // tool_choice 对象 → 函数名字符串
        assert_eq!(out["tool_choice"], "f");
    }

    #[test]
    fn prepare_body_drops_tools_when_choice_none() {
        let v = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}],
            "tool_choice": "none"
        });
        let out = prepare_body(&v);
        assert!(out.get("tool_choice").is_none());
        assert!(out.get("tools").is_none());
    }

    #[test]
    fn scan_line_splits_events_and_parses_output() {
        let mut st = SoloState::default();
        assert!(scan_line(&mut st, "event: output").is_none());
        assert!(scan_line(&mut st, r#"data: {"response":"你"}"#).is_none());
        let (ev, data) = scan_line(&mut st, "").expect("boundary");
        assert_eq!(ev, "output");
        let parsed = parse_event(&ev, &data);
        assert_eq!(parsed.content, "你");
        // 空行且无内容时不派发
        assert!(scan_line(&mut st, "").is_none());
    }

    #[test]
    fn merge_tool_calls_concatenates_arguments() {
        let mut acc: Vec<Value> = Vec::new();
        merge_tool_calls(&mut acc, &json!([{"index": 0, "id": "c1", "type": "function", "function_call": {"name": "Read", "arguments": "{\"p\":"}}]));
        merge_tool_calls(&mut acc, &json!([{"index": 0, "function_call": {"arguments": "1}"}}]));
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0]["id"], "c1");
        assert_eq!(acc[0]["function"]["name"], "Read");
        assert_eq!(acc[0]["function"]["arguments"], "{\"p\":1}");
        assert!(acc[0]["function"].get("namespace").is_none());
    }

    #[tokio::test]
    async fn trae_nonstream_via_mock() {
        let sse = concat!(
            "event: output\ndata: {\"response\":\"你好\"}\n\n",
            "event: output\ndata: {\"response\":\"！\",\"reasoning_content\":\"思\"}\n\n",
            "event: token_usage\ndata: {\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}\n\n",
            "event: done\ndata: {\"finish_reason\":\"stop\"}\n\n"
        );
        let base = spawn_mock(sse).await;
        let spec = spec_toml(&base);
        let http = reqwest::Client::new();
        let body = json!({"model": "GLM-5.3", "messages": [{"role": "user", "content": "hi"}]});
        let out = chat_nonstream(&http, &spec, "jwt-abc", body.to_string().as_bytes())
            .await
            .expect("nonstream");
        assert_eq!(out["choices"][0]["message"]["content"], "你好！");
        assert_eq!(out["choices"][0]["message"]["reasoning_content"], "思");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["prompt_tokens"], 5);
        assert_eq!(out["usage"]["completion_tokens"], 2);
        assert_eq!(out["model"], "GLM-5.3");
    }

    #[tokio::test]
    async fn trae_stream_via_mock() {
        let sse = concat!(
            "event: output\ndata: {\"response\":\"Hi\"}\n\n",
            "event: output\ndata: {\"response\":\"!\"}\n\n",
            "event: done\ndata: {\"finish_reason\":\"stop\"}\n\n"
        );
        let base = spawn_mock(sse).await;
        let spec = spec_toml(&base);
        let http = reqwest::Client::new();
        let body = json!({"model": "glm-5.2", "stream": true, "messages": [{"role": "user", "content": "hi"}]});
        let (mut s, mut rx) = chat_stream(&http, &spec, "jwt-abc", body.to_string().as_bytes())
            .await
            .expect("stream");
        let mut text = String::new();
        while let Some(Ok(b)) = s.next().await {
            text.push_str(&String::from_utf8_lossy(&b));
        }
        assert!(text.contains("Hi"), "{text}");
        assert!(text.contains("data: [DONE]"), "{text}");
        assert!(text.contains("\"finish_reason\":\"stop\""), "{text}");
        let stat = rx.recv().await.expect("stat");
        assert_eq!(stat.finish_reason.as_deref(), Some("stop"));
        assert!(stat.error.is_none());
        assert!(stat.saw_done);
    }

    #[tokio::test]
    async fn trae_stream_error_event_is_reported() {
        let sse = "event: error\ndata: {\"code\":4011,\"message\":\"rate limited\"}\n\n";
        let base = spawn_mock(sse).await;
        let spec = spec_toml(&base);
        let http = reqwest::Client::new();
        let body = json!({"model": "glm-5.2", "messages": [{"role": "user", "content": "hi"}]});
        let err = chat_nonstream(&http, &spec, "jwt-abc", body.to_string().as_bytes())
            .await
            .unwrap_err();
        assert!(matches!(err.class, ErrorClass::Rate));
        assert!(err.body.contains("rate limited"));
    }

    /// 续期必须能序列化回账号池 secret：trae 上游会轮换 RefreshToken，
    /// 新的一族不回写，旧 rt 被作废后这个号就只能重新登录。
    #[tokio::test]
    async fn refreshed_secret_is_serializable_and_keeps_identity() {
        let sse = "event: output\ndata: {\"response\":\"ok\"}\n\n";
        let base = spawn_mock(sse).await;
        let http = reqwest::Client::new();
        let secret = r#"{"accessToken":"jwt-old","refreshToken":"rt-old","machineId":"m-1","apiHost":""#.to_string()
            + &base
            + r#""}"#;
        let new_secret = refresh_secret(&http, &secret).await.expect("refresh");
        let back = parse_cred(&new_secret).expect("parse back");
        assert_eq!(back.token, "at-new", "新 token 必须写回: {new_secret}");
        assert_eq!(back.refresh_token, "rt-new", "轮换后的 refreshToken 必须写回: {new_secret}");
        assert_eq!(back.machine_id, "m-1", "用户原有字段不能被抹掉: {new_secret}");
        assert_eq!(back.api_host, base, "续期用的 host 要留着: {new_secret}");
    }

    #[test]
    fn cred_to_secret_omits_empty_fields() {
        let c = TraeCred { token: "jwt-1".into(), ..Default::default() };
        assert_eq!(cred_to_secret(&c), r#"{"accessToken":"jwt-1"}"#);
    }

    #[tokio::test]
    async fn refresh_secret_without_refresh_token_is_actionable_error() {
        let http = reqwest::Client::new();
        let e = refresh_secret(&http, "jwt-bare").await.unwrap_err();
        assert!(e.contains("refreshToken") && e.contains("跳转登录"), "{e}");
    }

    #[tokio::test]
    async fn trae_verify_hits_userinfo() {
        let base = spawn_mock("").await;
        let spec = spec_toml(&base);
        let http = reqwest::Client::new();
        let cred = format!(r#"{{"accessToken":"jwt-abc","apiHost":"{base}"}}"#);
        assert!(verify(&http, &spec, &cred).await);
        assert!(!verify(&http, &spec, "   ").await);
    }
}

