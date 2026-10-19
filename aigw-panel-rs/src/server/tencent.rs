//! 腾讯 CodeBuddy / WorkBuddy 直连层（移植自 app/tencent.py + app/tlogin.py 的关键部分）。
//!
//! 目的：让 `/api/tencent` 的 7 个 action（probe/login/poll/status/quota/checkin/
//! catalog/trae_local/refresh）在 Rust 面板里真实可用，**不依赖 workbuddy-gateway EXE**。
//!
//! 规格（实测，见 Python 版顶部注释）：
//!   User-Agent : CLI/2.143.1 CodeBuddy/2.143.1
//!   X-IDE-Type : production
//!   X-IDE-Name : workbuddy
//!   上游基址    : https://copilot.tencent.com（国内）/ https://www.codebuddy.ai（国际）
//!
//! 登录链路（Rust 无 CDP 浏览器，采用**服务端签发 state** 的设备授权，和 wb-gateway
//! 登录 action 同源）：
//!   POST {base}/v2/plugin/auth/state?platform=CLI → {state, authUrl}
//!   用户在授权页完成登录 → 轮询 GET {base}/v2/plugin/auth/token?state= → accessToken
//!   落盘 data/tencent_cred.json + 账号池（platform=wb-gateway）。全自动，无需手动粘贴。

use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

pub const UA: &str = "CLI/2.143.1 CodeBuddy/2.143.1";
pub const BASE_CN: &str = "https://copilot.tencent.com";
pub const BASE_INTL: &str = "https://www.codebuddy.ai";

/// 登记的端点（对齐 Python tencent.ENDPOINTS，仅用于 probe_all）。
pub const ENDPOINTS: &[(&str, &str, &str)] = &[
    // (name, method, path_template)
    ("oauth_token", "GET", "/v2/plugin/auth/token?state={state}"),
    ("oauth_refresh", "POST", "/v2/plugin/auth/token/refresh"),
    ("login_account", "POST", "/v2/plugin/login/account?state={state}"),
    ("rates", "GET", "/v3/config"),
    ("models", "GET", "/console/enterprises/personal/models"),
    ("chat", "POST", "/v2/chat/completions"),
    ("quota", "POST", "/v2/billing/meter/get-user-resource"),
    ("daily_checkin", "POST", "/v2/billing/meter/daily-checkin"),
    ("growth_buddy", "POST", "/activity/growth/buddy/first"),
    ("growth_buddy_info", "GET", "/activity/growth/buddy/info"),
    ("growth_heatmap", "GET", "/activity/growth/heatmap"),
    ("growth_lottery", "POST", "/activity/growth/lottery/draw"),
    ("growth_makeup", "POST", "/activity/growth/makeup-cards/use"),
    ("invite_bind", "POST", "/activity/workbuddy/invitation/v2/bind"),
];

fn http_client(timeout: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn headers(token: &str) -> Vec<(String, String)> {
    let mut h = vec![
        ("User-Agent".to_string(), UA.to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("X-IDE-Type".to_string(), "production".to_string()),
        ("X-IDE-Name".to_string(), "workbuddy".to_string()),
    ];
    if !token.is_empty() {
        h.push(("Authorization".to_string(), format!("Bearer {}", token)));
        let head60: &str = if token.len() > 60 { &token[..60] } else { token };
        if head60.contains('=') && !head60.contains(' ') {
            h.push(("Cookie".to_string(), token.to_string()));
        }
    }
    h
}

/// 底层调用：`name` 取自 ENDPOINTS；返回 (ok, Value)。
pub async fn call(
    base: &str,
    token: &str,
    name: &str,
    state: &str,
    body: Option<Value>,
    timeout: u64,
) -> (bool, Value) {
    let spec = ENDPOINTS.iter().find(|(n, _, _)| *n == name);
    let Some((_, method, tpl)) = spec else {
        return (false, json!({"error": format!("未知端点 {}", name)}));
    };
    let path = tpl.replace("{state}", &state.to_string());
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let client = http_client(timeout);
    let mut req = if *method == "POST" {
        client.post(&url)
    } else {
        client.get(&url)
    };
    for (k, v) in headers(token) {
        req = req.header(k, v);
    }
    if *method != "GET" {
        let payload = body.clone().unwrap_or_else(|| json!({}));
        req = req
            .header("Content-Type", "application/json")
            .body(payload.to_string());
    }
    match req.send().await {
        Ok(r) => {
            let code = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            let parsed: Value = serde_json::from_str(&txt)
                .unwrap_or_else(|_| json!({"http": code, "raw": txt.chars().take(500).collect::<String>()}));
            if code >= 400 {
                (false, parsed)
            } else {
                (true, parsed)
            }
        }
        Err(e) => (false, json!({"error": format!("{}", e)})),
    }
}

/// 探一遍所有端点（未登录时看哪些活着）。
pub async fn probe_all(token: &str, base: &str, timeout: u64) -> Vec<Value> {
    let mut out = Vec::new();
    for (name, method, _) in ENDPOINTS {
        let (ok, d) = call(base, token, name, "", None, timeout).await;
        let code = d
            .get("http")
            .and_then(|v| v.as_i64())
            .or_else(|| d.get("code").and_then(|v| v.as_i64()))
            .unwrap_or(if ok { 200 } else { 0 });
        let note = d
            .get("msg")
            .or_else(|| d.get("note"))
            .or_else(|| d.get("raw"))
            .or_else(|| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push(json!({
            "name": name, "ok": ok, "code": code,
            "note": note.chars().take(90).collect::<String>(),
            "method": method,
        }));
    }
    out
}

/// 官方倍率（/v3/config 的 data）。
pub async fn rates(token: &str, base: &str) -> (bool, Value) {
    let (ok, d) = call(base, token, "rates", "", None, 20).await;
    if !ok {
        return (false, d);
    }
    (true, d.get("data").cloned().unwrap_or(d))
}

/// 额度明细（必须 POST body={}）。
pub async fn quota(token: &str, base: &str) -> (bool, Value) {
    let (ok, d) = call(base, token, "quota", "", Some(json!({})), 20).await;
    if !ok {
        return (false, d);
    }
    if let Some(c) = d.get("code").and_then(|v| v.as_i64()) {
        if c != 0 {
            return (false, d);
        }
    }
    let node = d
        .pointer("/data/Response/Data")
        .cloned()
        .unwrap_or(Value::Null);
    (
        true,
        json!({
            "totalCount": node.get("TotalCount").and_then(|v| v.as_i64()).unwrap_or(0),
            "totalDosage": node.get("TotalDosage").cloned().unwrap_or(json!(0)),
            "accounts": node.get("Accounts").cloned().unwrap_or(json!([])),
        }),
    )
}

/// 把资源聚合成摘要（按 package+unit 去重）。
pub async fn quota_summary(token: &str, base: &str) -> (bool, Vec<Value>) {
    let (ok, q) = quota(token, base).await;
    if !ok {
        return (false, vec![q]);
    }
    let accounts = q.get("accounts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut by: std::collections::BTreeMap<(String, String), (f64, f64, f64, i64)> =
        std::collections::BTreeMap::new();
    for a in &accounts {
        let name = a
            .get("PackageName")
            .or_else(|| a.get("ProductName"))
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let unit = a.get("CapacityUnit").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let e = by.entry((name, unit)).or_insert((0.0, 0.0, 0.0, 0));
        let f = |k: &str| {
            a.get(k)
                .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok())))
                .unwrap_or(0.0)
        };
        e.0 += f("CapacitySize");
        e.1 += f("CapacityRemain");
        e.2 += f("CapacityUsed");
        e.3 += 1;
    }
    let mut rows: Vec<Value> = by
        .into_iter()
        .map(|((name, unit), (size, remain, used, cycles))| {
            json!({"name": name, "unit": unit, "size": size, "remain": remain, "used": used, "cycles": cycles})
        })
        .collect();
    rows.sort_by(|a, b| {
        b.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0)
            .partial_cmp(&a.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (true, rows)
}

/// 每日签到（400「今天已签到」不是失败）。
pub async fn daily_checkin(token: &str, base: &str) -> (bool, Value) {
    call(base, token, "daily_checkin", "", Some(json!({})), 20).await
}

/// 在线模型目录（/console/.../models，带 credits）。
pub async fn catalog(token: &str, base: &str) -> Vec<Value> {
    let (ok, d) = call(base, token, "models", "", None, 20).await;
    if !ok {
        return Vec::new();
    }
    parse_models(&d)
}

/// 把 /console/.../models 或 /v3/config 的返回归一成统一结构。
pub fn parse_models(d: &Value) -> Vec<Value> {
    let src = d.pointer("/data/models").and_then(|v| v.as_array()).cloned();
    let src = match src {
        Some(a) => a,
        None => return Vec::new(),
    };
    src.iter()
        .filter_map(|m| {
            let id = m.get("id").or_else(|| m.get("name")).and_then(|v| v.as_str())?;
            if id.is_empty() {
                return None;
            }
            Some(json!({
                "id": id,
                "name": m.get("name").and_then(|v| v.as_str()).unwrap_or(id),
                "credits": m.get("credits").cloned().unwrap_or(json!("")),
                "vendor": m.get("vendor").and_then(|v| v.as_str()).unwrap_or(""),
                "maxInputTokens": m.get("maxInputTokens").and_then(|v| v.as_i64()).unwrap_or(0),
                "maxOutputTokens": m.get("maxOutputTokens").and_then(|v| v.as_i64()).unwrap_or(0),
                "maxAllowedSize": m.get("maxAllowedSize").and_then(|v| v.as_i64()).unwrap_or(0),
                "supportsImages": m.get("supportsImages").and_then(|v| v.as_bool()).unwrap_or(false),
                "supportsToolCall": m.get("supportsToolCall").and_then(|v| v.as_bool()).unwrap_or(false),
                "supportsReasoning": m.get("supportsReasoning").and_then(|v| v.as_bool()).unwrap_or(false),
                "onlyReasoning": m.get("onlyReasoning").and_then(|v| v.as_bool()).unwrap_or(false),
                "source": "online",
            }))
        })
        .collect()
}

/// 刷新 accessToken（关键坑：refreshToken 走 header `X-Refresh-Token`，不是 body/query）。
/// 返回 (ok, {accessToken, refreshToken, expire} | 错误信息)。
pub async fn refresh(refresh_token: &str, base: &str) -> (bool, Value) {
    if refresh_token.is_empty() {
        return (false, json!("没有 refreshToken"));
    }
    let url = format!("{}/v2/plugin/auth/token/refresh", base.trim_end_matches('/'));
    let client = http_client(20);
    let mut req = client.post(&url);
    for (k, v) in headers("") {
        req = req.header(k, v);
    }
    req = req
        .header("X-Refresh-Token", refresh_token)
        .header("Content-Type", "application/json")
        .body("{}");
    match req.send().await {
        Ok(r) => {
            let code = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            if code != 200 {
                return (false, json!(txt.chars().take(300).collect::<String>()));
            }
            let d: Value = match serde_json::from_str(&txt) {
                Ok(v) => v,
                Err(_) => return (false, json!(txt.chars().take(300).collect::<String>())),
            };
            if d.get("code").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
                return (
                    false,
                    json!(d.get("msg").and_then(|v| v.as_str()).unwrap_or(&format!("{}", d))),
                );
            }
            let data = d.get("data").cloned().unwrap_or(json!({}));
            let at = data
                .get("accessToken")
                .or_else(|| data.get("access_token"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let rt = data
                .get("refreshToken")
                .or_else(|| data.get("refresh_token"))
                .and_then(|v| v.as_str())
                .unwrap_or(refresh_token);
            if at.is_empty() {
                return (false, json!(format!("刷新响应里没有 accessToken：{}", d)));
            }
            (
                true,
                json!({
                    "accessToken": at,
                    "refreshToken": rt,
                    "expire": data.get("expireIn").or_else(|| data.get("expiresIn")).cloned().unwrap_or(json!("")),
                }),
            )
        }
        Err(e) => (false, json!(format!("{}", e))),
    }
}

/// 验活：返回 {rates: status, models: status}（对齐 Python tlogin.verify）。
pub async fn verify(token: &str, base: &str) -> Value {
    let mut out = serde_json::Map::new();
    for (name, path) in [("rates", "/v3/config"), ("models", "/console/enterprises/personal/models")] {
        let url = format!("{}{}", base.trim_end_matches('/'), path);
        let client = http_client(20);
        let mut req = client.get(&url);
        for (k, v) in headers(token) {
            req = req.header(k, v);
        }
        let code = match req.send().await {
            Ok(r) => r.status().as_u16() as i64,
            Err(_) => 0,
        };
        out.insert(name.to_string(), json!(code));
    }
    Value::Object(out)
}

// ------------------------------------------------------------------ 登录 state 存储
// 服务端签发 state 后，前端轮询 poll 需要带上它；Rust 无 CDP，用内存表登记
// state → (base, ts)，poll 时据此打对应域名。
fn login_states() -> &'static Mutex<std::collections::HashMap<String, (String, u64)>> {
    static M: OnceLock<Mutex<std::collections::HashMap<String, (String, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn remember_state(state: &str, base: &str) {
    if let Ok(mut m) = login_states().lock() {
        m.insert(state.to_string(), (base.to_string(), now_secs()));
    }
}

pub fn base_of_state(state: &str) -> Option<String> {
    login_states()
        .lock()
        .ok()
        .and_then(|m| m.get(state).map(|(b, _)| b.clone()))
}

/// 生成 UUID v4 形态的本地 state（授权页服务端按 UUID 形态校验）。
pub fn new_state() -> String {
    use md5::{Digest, Md5};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Md5::new();
    h.update(format!("{}|{}|{}", nanos, std::process::id(), nanos ^ 0x9E37_79B9_7F4A_7C15));
    let mut b = h.finalize();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let x: String = b.iter().map(|byte| format!("{:02x}", byte)).collect();
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

/// 用授权页地址打开浏览器（复用 server::open_browser）。
pub fn login_url(state: &str) -> String {
    format!("{}/login?platform=CLI&state={}", BASE_CN, state)
}

// ------------------------------------------------------------------ 凭据落盘
fn cred_path() -> std::path::PathBuf {
    crate::core::config::data_dir().join("tencent_cred.json")
}

pub fn load_cred() -> Value {
    std::fs::read_to_string(cred_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null)
}

pub fn save_cred(cred: &Value) -> String {
    let p = cred_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        &p,
        serde_json::to_string_pretty(cred).unwrap_or_default(),
    );
    p.display().to_string()
}

// ------------------------------------------------------------------ Trae 本机登录态
const TRAE_CAND_REL: &[&str] = &[
    "Trae CN",
    "Trae",
    "Trae Global",
    "Trae Studio",
    "Trae CN/User",
    "Trae/User",
    ".trae",
    "Trae Global/User",
];

const AUTH_KEYS: &[&str] = &[
    "x-ide-token",
    "idetoken",
    "authtoken",
    "token",
    "accesstoken",
    "icubeauthinfo",
    "authinfo",
    "auth_info",
    "authorization",
];

fn candidate_dirs() -> Vec<String> {
    let mut bases: Vec<String> = Vec::new();
    if let Ok(v) = std::env::var("APPDATA") {
        bases.push(v);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        bases.push(format!("{}/.config", home));
        bases.push(format!("{}/.local/share", home));
        bases.push(home.clone());
        bases.push(format!("{}/AppData/Roaming", home));
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for b in bases {
        for rel in TRAE_CAND_REL {
            let d = std::path::Path::new(&b).join(rel);
            let s = d.display().to_string();
            if d.is_dir() && seen.insert(s.clone()) {
                out.push(s);
            }
        }
    }
    out
}

fn search_auth(obj: &Value, depth: usize, account: &mut Option<String>) -> Option<String> {
    if depth > 12 {
        return None;
    }
    match obj {
        Value::Object(m) => {
            for k in ["account", "email", "username", "nick", "userId", "user", "name", "loginName"] {
                if account.is_none() {
                    if let Some(v) = m.get(k).and_then(|v| v.as_str()) {
                        if !v.is_empty() {
                            *account = Some(v.to_string());
                        }
                    }
                }
            }
            for (k, v) in m {
                let kl = k.to_lowercase();
                if AUTH_KEYS.contains(&kl.as_str()) {
                    if let Some(s) = v.as_str() {
                        if s.len() >= 16 {
                            if let Some(t) = maybe_json_token(s) {
                                return Some(t);
                            }
                        }
                    }
                }
                if v.is_object() || v.is_array() {
                    if let Some(t) = search_auth(v, depth + 1, account) {
                        return Some(t);
                    }
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                if let Some(t) = search_auth(v, depth + 1, account) {
                    return Some(t);
                }
            }
        }
        _ => {}
    }
    None
}

fn maybe_json_token(v: &str) -> Option<String> {
    if v.len() < 16 {
        return None;
    }
    if let Ok(d) = serde_json::from_str::<Value>(v) {
        if let Some(m) = d.as_object() {
            for k in ["token", "accessToken", "x-ide-token", "ideToken", "authToken", "access_token"] {
                if let Some(tv) = m.get(k).and_then(|v| v.as_str()) {
                    if tv.len() >= 16 {
                        return Some(tv.to_string());
                    }
                }
            }
        }
    }
    Some(v.to_string())
}

fn parse_storage_json(path: &std::path::Path) -> Option<(String, Option<String>)> {
    let s = std::fs::read_to_string(path).ok()?;
    let data: Value = serde_json::from_str(&s).ok()?;
    let mut acc = None;
    let t = search_auth(&data, 0, &mut acc)?;
    Some((t, acc))
}

fn scan_leveldb(dir: &std::path::Path) -> Option<(String, Option<String>)> {
    let ldb = dir.join("Local Storage").join("leveldb");
    if !ldb.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(&ldb).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("log") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&p) {
            for line in content.lines() {
                if line.contains("x-ide-token")
                    || line.contains("iCubeAuthInfo")
                    || line.contains("authInfo")
                    || line.contains("ideToken")
                {
                    if let Some(tok) = extract_ldb_token(line) {
                        return Some((tok, None));
                    }
                }
            }
        }
    }
    None
}

/// 从 leveldb 行里抠 token（对齐 _LDB_KEY_RE）。
fn extract_ldb_token(line: &str) -> Option<String> {
    for key in ["x-ide-token", "ideToken", "authToken", "iCubeAuthInfo", "authInfo"] {
        if let Some(pos) = line.find(key) {
            let rest = &line[pos + key.len()..];
            let rest = rest.trim_start_matches(|c: char| c == '\t' || c == '=' || c == ':' || c == ' ');
            let tok: String = rest
                .chars()
                .take_while(|c| *c != '\n' && *c != '\r' && *c != '\t')
                .collect();
            let tok = tok.trim().trim_matches('"').trim_matches('\'').to_string();
            if tok.len() >= 16 {
                return Some(tok);
            }
        }
    }
    None
}

/// 读取本机 Trae 登录态。
pub fn read_trae_token() -> (bool, Value) {
    let dirs = candidate_dirs();
    if dirs.is_empty() {
        return (
            false,
            json!({
                "error": "未在当前系统找到 Trae 的用户数据目录（已排查 Windows AppData / macOS Application Support / Linux ~/.config、~/.local/share、~/.trae 下的 Trae/Trae CN 等候选）。",
                "candidates": [],
                "hint": "请确认本机已安装并至少登录过一次 Trae；或告诉我它的安装路径。",
            }),
        );
    }
    for d in &dirs {
        let dp = std::path::Path::new(d);
        for cand in [dp.join("storage.json"), dp.join("User").join("storage.json")] {
            if cand.is_file() {
                if let Some((tok, acc)) = parse_storage_json(&cand) {
                    return (
                        true,
                        json!({"token": tok, "account": acc.unwrap_or_else(|| "Trae 本机登录态".to_string()),
                               "source": format!("storage.json @ {}", cand.display())}),
                    );
                }
            }
        }
        if let Some((tok, acc)) = scan_leveldb(dp) {
            return (
                true,
                json!({"token": tok, "account": acc.unwrap_or_else(|| "Trae 本机登录态".to_string()),
                       "source": format!("Local Storage leveldb @ {}", d)}),
            );
        }
    }
    (
        false,
        json!({
            "error": "在已找到的 Trae 目录里没能提取到登录 token（storage.json / Local Storage 都试过）。可能你的版本改了存储位置或字段名。",
            "candidates": dirs,
            "hint": "把下面某个目录路径发我，我帮你确认 token 存在哪个键。",
        }),
    )
}
