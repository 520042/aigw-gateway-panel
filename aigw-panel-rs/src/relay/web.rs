//! 网页版 AI 反代中继（非 OpenAI 原生格式的公有云网页端）。
//!
//! 全部逻辑从 Python `app/web_relays.py` 一手移植（同一份社区逆向来源），不再猜测：
//!
//! - **web-glm**（智谱清言 chatglm.cn）
//!   来源：glm2api（yxc0915）`build_sign` + X-Sign 头组。
//!   - X-Sign / X-Timestamp / X-Nonce 三签名头，缺一即 403；
//!   - `SIGN_SECRET = "8a1317a7468aa3ad86e997d08f3f31cb"`；
//!   - timestamp 倒数第二位是校验位 `(各位和 - 倒数第二位) % 10`；
//!   - 拿到的 refresh_token 须先经 `/user-api/user/refresh` 换 access_token 才能调 stream。
//!
//! - **web-trae**（Trae 国内版直连）
//!   来源：dsh-trae-connect（lament-z）`upstream.ts`。
//!   - 真实 base = `https://trae-api-cn.mchost.guru`（不是 byteintlapi）；
//!   - 真实 CN appId = `6eefa01c-1036-4c7e-9ca5-d891f63bfcd8`；
//!   - 私有 chat 协议：`user_input/intent_name/variables/chat_history/session_id/
//!     model_name/function='chat'`，缺 `function` 报 "function is empty"；
//!   - 响应是 `event: response` 增量 delta（须累加，不能当全量快照）。
//!
//! - **web-deepseek**（DeepSeek 网页版）
//!   来源：deepseek-free-api（qiaojinxia）`deepseek_client.py` + `pow_worker.js`。
//!   - 先 `POST /api/v0/chat_session/create` 拿 chat_session_id；
//!   - 先 `POST /api/v0/chat/create_pow_challenge` 拿挑战，用随包 Node+WASM
//!     （`webpow/`）解出 answer，base64 编码放进 `x-ds-pow-response` 头；
//!   - 响应是 p/v/o 分片协议（response/fragments APPEND），不是标准 choices。
//!
//! 凭据取自账号池（与 Native 同级）。三平台对外都收敛成 OpenAI `chat.completion`
//! 信封（非流式）或等价的 SSE 流（流式），便于下游客户端无感接入。
use md5::Digest;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

// 与 Python 一致：出站直连、不吃系统代理（避免把聊天凭据泄露给代理），
// 且开启证书校验（native-tls/Schannel 默认即校验）。
fn http_client(timeout: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .expect("reqwest client 构建失败")
}

const UA_CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// 随机 32 位 hex（替代 Python 的 uuid4().hex，无 uuid 依赖）。
fn nonce_hex() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static C: AtomicU64 = AtomicU64::new(0);
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let c = C.fetch_add(1, Ordering::Relaxed);
    let s = format!("{:x}{:x}", n, c);
    if s.len() >= 32 {
        s[..32].to_string()
    } else {
        format!("{:0<32}", s)
    }
}

/// 把整段 SSE 文本解析为 (event, data) 事件列表（与 Python `_read_sse` 同语义）。
fn parse_sse(text: &str) -> Vec<(Option<String>, String)> {
    let mut out: Vec<(Option<String>, String)> = Vec::new();
    let mut ev: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            ev = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            out.push((ev.clone(), rest.trim().to_string()));
        }
    }
    out
}

// ==================================================================== 智谱清言
const GLM_BASE: &str = "https://chatglm.cn/chatglm";
const GLM_SIGN_SECRET: &str = "8a1317a7468aa3ad86e997d08f3f31cb"; // glm_auth.py 一手常量
const GLM_ASSISTANT_ID: &str = "65940acff94777010aa6b796";

/// refresh_token → access_token 缓存（按 refresh_token，30min TTL）。
type TokCache = HashMap<String, (String, i64)>;
fn glm_tok_cache() -> &'static Mutex<TokCache> {
    use std::sync::OnceLock;
    static C: OnceLock<Mutex<TokCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 复刻 glm2api `build_sign()`：timestamp 倒数第二位是校验位。
fn glm_build_sign() -> (String, String, String) {
    let now = format!("{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis());
    let digits: Vec<i64> = now.chars().filter_map(|c| c.to_digit(10).map(|d| d as i64)).collect();
    let checksum = (digits.iter().sum::<i64>() - digits[digits.len() - 2]) % 10;
    let timestamp = format!("{}{}{}", &now[..now.len() - 2], checksum, &now[now.len() - 1..]);
    let nonce = nonce_hex();
    let sign = format!(
        "{:x}",
        md5::Md5::digest(format!("{}-{}-{}", timestamp, nonce, GLM_SIGN_SECRET).as_bytes())
    );
    (timestamp, nonce, sign)
}

/// refresh_token → access_token（带缓存）。
async fn glm_access_token(refresh_token: &str) -> Result<String, String> {
    // 淘汰过期项
    {
        let mut cache = glm_tok_cache().lock().unwrap();
        if cache.len() > 64 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            cache.retain(|_, v| now < v.1);
        }
        if let Some((at, exp)) = cache.get(refresh_token) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            if now < *exp {
                return Ok(at.clone());
            }
        }
    }
    let client = http_client(30);
    let resp = client
        .post(format!("{}/user-api/user/refresh", GLM_BASE))
        .header("Authorization", format!("Bearer {}", refresh_token))
        .header("Content-Type", "application/json")
        .header("User-Agent", UA_CHROME)
        .body("{}")
        .send()
        .await
        .map_err(|e| format!("刷新 token 请求失败：{}", e))?;
    let status = resp.status().as_u16() as i64;
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("刷新 token HTTP {}：{}", status, &text[..text.len().min(200)]));
    }
    let d: Value = serde_json::from_str(&text).map_err(|_| format!("刷新 token 返回非 JSON：{}", &text[..text.len().min(200)]))?;
    let at = d.pointer("/data/access_token").and_then(|v| v.as_str()).unwrap_or("");
    if at.is_empty() {
        return Err(format!("刷新 token 未返回 access_token：{}", &text[..text.len().min(200)]));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    glm_tok_cache()
        .lock()
        .unwrap()
        .insert(refresh_token.to_string(), (at.to_string(), now + 1800));
    Ok(at.to_string())
}

async fn glm_chat(messages: &[Value], secret: &str, model: &str, timeout: u64) -> Result<Value, (i64, String)> {
    if secret.is_empty() {
        return Err((401, "缺少 chatglm_refresh_token".to_string()));
    }
    if model.contains("image") || model.contains("cogview") {
        return Err((400, "智谱图片生成不支持通过 /v1/chat/completions，请用专用图片接口".to_string()));
    }
    let at = match glm_access_token(secret).await {
        Ok(at) => at,
        Err(e) => return Err((401, e)),
    };
    let (think, search, actual) = glm_variants(model);
    let url = format!("{}/backend-api/assistant/stream", GLM_BASE);
    let conv: Vec<Value> = messages
        .iter()
        .filter(|m| {
            let r = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            r == "user" || r == "assistant"
        })
        .cloned()
        .collect();
    let mut body = json!({
        "assistant_id": GLM_ASSISTANT_ID,
        "conversation_id": "",
        "messages": conv.iter().map(|m| json!({
            "role": m.get("role").and_then(|v| v.as_str()).unwrap_or(""),
            "content": m.get("content").and_then(|v| v.as_str()).unwrap_or(""),
        })).collect::<Vec<_>>(),
        "model": actual,
        "stream": true,
        "prompt": "",
        "tools": [],
    });
    if think {
        body["thinking"] = json!({"type": "enabled"});
    }
    if search {
        body["search"] = json!({"enable": true});
    }
    let (timestamp, nonce, sign) = glm_build_sign();
    let headers = vec![
        ("Authorization".into(), format!("Bearer {}", at)),
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "text/event-stream".into()),
        ("User-Agent".into(), UA_CHROME.into()),
        ("X-Nonce".into(), nonce),
        ("X-Sign".into(), sign),
        ("X-Timestamp".into(), timestamp),
    ];
    let (status, text) = post_collect(&url, &headers, Some(body.to_string().into_bytes()), timeout).await?;
    if !(200..300).contains(&status) {
        return Err((status, format!("智谱上游 HTTP {}：{}", status, &text[..text.len().min(300)])));
    }
    // chatglm assistant/stream：parts 是全量快照，取最后一个有 content 的 part
    let mut last = String::new();
    for (_ev, data) in parse_sse(&text) {
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let d: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(parts) = d.get("parts").and_then(|v| v.as_array()) {
            for p in parts {
                if let Some(c) = p.get("content").and_then(|v| v.as_str()) {
                    if !c.is_empty() {
                        last = c.to_string();
                    }
                } else if let Some(c) = p.as_str() {
                    if !c.is_empty() {
                        last = c.to_string();
                    }
                }
            }
        }
    }
    if last.is_empty() {
        return Err((502, "智谱上游未返回可解析内容（签名或请求体不符）".to_string()));
    }
    Ok(envelope(model, &[json!({"message": {"role": "assistant", "content": last}})]))
}

/// 解析 glm 模型变体（-think / -search / -think-search）。
fn glm_variants(model: &str) -> (bool, bool, String) {
    let mut think = false;
    let mut search = false;
    let mut actual = model.to_string();
    if model.ends_with("-think-search") {
        think = true;
        search = true;
        actual = model[..model.len() - "-think-search".len()].to_string();
    } else if model.ends_with("-think") {
        think = true;
        actual = model[..model.len() - "-think".len()].to_string();
    } else if model.ends_with("-search") {
        search = true;
        actual = model[..model.len() - "-search".len()].to_string();
    }
    (think, search, actual)
}

// ==================================================================== Trae 直连
const TRAE_BASE: &str = "https://trae-api-cn.mchost.guru"; // 一手（dsh-trae-connect）
const TRAE_CHAT_PATH: &str = "/api/ide/v1/chat";
const TRAE_APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8"; // CN 网关要 UUID
const TRAE_IDE_VERSION: &str = "1.2.10";
const TRAE_IDE_VERSION_CODE: &str = "20260401";
const TRAE_MAP: &[(&str, &str)] = &[
    ("claude-3-5-sonnet", "claude3.5"),
    ("claude-3-7-sonnet", "aws_sdk_claude37_sonnet"),
    ("gpt-4o", "gpt-4o"),
    ("gpt-4.1", "gpt-4.1-2025-04-14"),
    ("deepseek-chat", "deepseek-V3"),
    ("deepseek-v3", "deepseek-V3"),
    ("deepseek-r1", "deepseek-R1"),
    ("deepseek-reasoner", "deepseek-R1"),
    ("gemini-2.5-pro", "gemini-2.5-pro-preview-03-25"),
    ("gemini-2.5-flash", "gemini_2.5_flash"),
];

fn trae_model_map(model: &str) -> String {
    for (k, v) in TRAE_MAP {
        if *k == model {
            return v.to_string();
        }
    }
    model.to_string()
}

/// FNV-1a 32bit → 'dsh-<base36>'，同会话稳定（保多轮上下文）。
fn trae_session_id(messages: &[Value]) -> String {
    let seed: String = messages
        .iter()
        .map(|m| {
            format!(
                "{}:{}",
                m.get("role").and_then(|v| v.as_str()).unwrap_or(""),
                m.get("content").and_then(|v| v.as_str()).unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\u{0}");
    let mut h: u32 = 0x811c9dc5;
    for ch in seed.chars() {
        h ^= (ch as u32) & 0xFF;
        h = h.wrapping_mul(0x01000193);
    }
    format!("dsh-{}", to_base36(h))
}

fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let digits: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = String::new();
    while n > 0 {
        out.push(digits[(n % 36) as usize] as char);
        n /= 36;
    }
    out.chars().rev().collect()
}

async fn trae_chat(messages: &[Value], secret: &str, model: &str, timeout: u64) -> Result<Value, (i64, String)> {
    if secret.is_empty() {
        return Err((401, "缺少 Trae x-ide-token".to_string()));
    }
    let url = format!("{}{}", TRAE_BASE, TRAE_CHAT_PATH);
    let conv: Vec<Value> = messages
        .iter()
        .filter(|m| {
            let r = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            r == "user" || r == "assistant"
        })
        .cloned()
        .collect();
    let last = conv
        .last()
        .and_then(|m| m.get("content").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    let sid = trae_session_id(&conv);
    let variables = json!({
        "language": "", "locale": "zh-cn", "input": last,
        "version_code": TRAE_IDE_VERSION_CODE.parse::<i64>().unwrap_or(20260401),
        "is_inline_chat": false, "is_command": false,
        "raw_input": last, "problem": "", "current_filename": "",
        "is_select_code_before_chat": false,
        "last_select_time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
        "last_turn_session": "", "hash_workspace": false,
        "hash_file": 0, "hash_code": 0, "use_filepath": true,
        "current_time": chrono_now(),
        "badge_clickable": true, "workspace_path": "",
        "brand": "Trae", "system_type": "Windows",
    });
    let mut last_info: Option<Value> = None;
    for i in (0..conv.len().saturating_sub(1)).rev() {
        if conv[i].get("role").and_then(|v| v.as_str()).unwrap_or("") == "assistant" {
            last_info = Some(json!({
                "turn": i, "is_error": false,
                "response": conv[i].get("content").and_then(|v| v.as_str()).unwrap_or(""),
            }));
            break;
        }
    }
    let body = json!({
        "user_input": last,
        "intent_name": "general_qa_intent",
        "variables": variables.to_string(),
        "context_resolvers": [
            {"resolver_id": "project-labels", "variables": "{\"labels\":\"\"}"},
            {"resolver_id": "terminal_context", "variables": "{\"terminal_context\":[]}"},
        ],
        "generate_suggested_questions": false,
        "chat_history": conv.iter().take(conv.len().saturating_sub(1)).map(|m| json!({
            "role": m.get("role").and_then(|v| v.as_str()).unwrap_or(""),
            "session_id": sid,
            "locale": if m.get("role").and_then(|v| v.as_str()).unwrap_or("") == "assistant" { "zh-cn" } else { "" },
            "content": m.get("content").and_then(|v| v.as_str()).unwrap_or(""),
            "status": "success",
        })).collect::<Vec<_>>(),
        "session_id": sid,
        "conversation_id": sid,
        "current_turn": conv.len().saturating_sub(1) as i64,
        "valid_turns": (0..conv.len().saturating_sub(1) as i64).collect::<Vec<_>>(),
        "multi_media": [],
        "model_name": trae_model_map(model),
        "last_llm_response_info": last_info,
        "is_preset": true,
        "provider": "",
        "function": "chat",
    });
    let headers = vec![
        ("Content-Type".into(), "application/json".into()),
        ("x-app-id".into(), TRAE_APP_ID.into()),
        ("x-ide-version".into(), TRAE_IDE_VERSION.into()),
        ("x-ide-version-code".into(), TRAE_IDE_VERSION_CODE.into()),
        ("x-ide-version-type".into(), "stable".into()),
        ("x-device-cpu".into(), "0".into()),
        ("x-device-id".into(), "unknown".into()),
        ("x-machine-id".into(), "unknown".into()),
        ("x-device-brand".into(), "0".into()),
        ("x-device-type".into(), "0".into()),
        ("x-ide-token".into(), secret.to_string()),
        ("accept".into(), "*/*".into()),
        ("Connection".into(), "keep-alive".into()),
        ("User-Agent".into(), String::new()),
    ];
    let (status, text) = post_collect(&url, &headers, Some(body.to_string().into_bytes()), timeout).await?;
    if !(200..300).contains(&status) {
        return Err((status, format!("Trae 上游 HTTP {}：{}", status, &text[..text.len().min(300)])));
    }
    let mut acc: Vec<String> = Vec::new();
    let mut reasoning: Vec<String> = Vec::new();
    for (_ev, data) in parse_sse(&text) {
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let d: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(r) = d.get("response").and_then(|v| v.as_str()) {
            if !r.is_empty() {
                acc.push(r.to_string());
            }
        }
        if let Some(r) = d.get("reasoning_content").and_then(|v| v.as_str()) {
            if !r.is_empty() {
                reasoning.push(r.to_string());
            }
        }
    }
    let last_text = acc.concat();
    if last_text.is_empty() {
        return Err((502, "Trae 上游未返回可解析内容（请求体/签名不符，需抓包校准）".to_string()));
    }
    let mut msg = json!({"role": "assistant", "content": last_text});
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning.concat());
    }
    Ok(envelope(model, &[msg]))
}

fn chrono_now() -> String {
    // 等效 Python time.strftime("%Y/%m/%d %H:%M:%S")（用 UTC，时区不影响会话标识稳定性）
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut days = t / 86400;
    let mut year = 1970u64;
    loop {
        let leap: u64 = if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
            366
        } else {
            365
        };
        if days >= leap {
            days -= leap;
            year += 1;
        } else {
            break;
        }
    }
    let leap: u64 = if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
        29
    } else {
        28
    };
    let month_days: [u64; 12] = [31, leap, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1usize;
    let mut d = days;
    loop {
        if d < month_days[month - 1] {
            break;
        }
        d -= month_days[month - 1];
        month += 1;
    }
    let day = d + 1;
    let sod = t % 86400;
    let hh = sod / 3600;
    let mm = (sod % 3600) / 60;
    let ss = sod % 60;
    format!(
        "{:04}/{:02}/{:02} {:02}:{:02}:{:02}",
        year, month, day, hh, mm, ss
    )
}

// ==================================================================== DeepSeek 网页
const DS_BASE: &str = "https://chat.deepseek.com";
const DS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn ds_headers(secret: &str) -> Vec<(String, String)> {
    vec![
        ("accept".into(), "*/*".into()),
        ("accept-language".into(), "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7".into()),
        ("authorization".into(), format!("Bearer {}", secret)),
        ("content-type".into(), "application/json".into()),
        ("origin".into(), DS_BASE.into()),
        ("user-agent".into(), DS_UA.into()),
        ("x-app-version".into(), "20241129.1".into()),
        ("x-client-locale".into(), "en_US".into()),
        ("x-client-platform".into(), "web".into()),
        ("x-client-timezone-offset".into(), "28800".into()),
        ("x-client-version".into(), "1.7.0".into()),
    ]
}

async fn ds_create_session(secret: &str, timeout: u64) -> String {
    let client = http_client(timeout);
    if let Ok(resp) = client
        .post(format!("{}/api/v0/chat_session/create", DS_BASE))
        .headers(headermap(&ds_headers(secret)))
        .body("{\"character_id\":null}")
        .send()
        .await
    {
        if let Ok(text) = resp.text().await {
            if let Ok(d) = serde_json::from_str::<Value>(&text) {
                if let Some(sid) = d.pointer("/data/biz_data/id").and_then(|v| v.as_str()) {
                    return sid.to_string();
                }
            }
        }
    }
    nonce_hex()
}

/// 定位随包分发的 webpow 目录（相对 exe，对齐 Python `_pow_dir()`）。
fn pow_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?;
    let cand = parent.join("webpow");
    if cand.join("pow_worker.js").is_file() {
        return Some(cand);
    }
    None
}

/// 定位 node（PATH 优先，再猜常见安装位；对齐 Python `_node_exe()`）。
fn node_exe() -> Option<std::path::PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(';') {
            let cand = std::path::Path::new(dir).join("node.exe");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    let guesses = [
        "C:\\Program Files\\nodejs\\node.exe",
        "C:\\Program Files (x86)\\nodejs\\node.exe",
        "%LOCALAPPDATA%\\Programs\\nodejs\\node.exe",
        "%APPDATA%\\nvm\\current\\node.exe",
        "%ProgramFiles%\\nodejs\\node.exe",
    ];
    for g in guesses {
        let expanded = if g.contains('%') {
            expand_env(g)
        } else {
            g.to_string()
        };
        if std::path::Path::new(&expanded).is_file() {
            return Some(std::path::PathBuf::from(expanded));
        }
    }
    None
}

fn expand_env(s: &str) -> String {
    let mut out = s.to_string();
    for (k, v) in [("LOCALAPPDATA", "Local"), ("APPDATA", "AppData"), ("ProgramFiles", "Program Files")] {
        let key = format!("%{}%", k);
        if let Ok(val) = std::env::var(k) {
            out = out.replace(&key, &val);
        } else if let Ok(val) = std::env::var(format!("ProgramFiles")) {
            out = out.replace(&key, &val);
        }
        let _ = v;
    }
    out
}

/// 调随包 Node+WASM 解 POW → answer(int)；找不到 node/wasm 返回 None。
fn ds_solve_pow(challenge: &Value) -> Option<i64> {
    let dir = pow_dir()?;
    let node = node_exe()?;
    let payload = json!({
        "challenge": challenge.get("challenge").and_then(|v| v.as_str()).unwrap_or(""),
        "salt": challenge.get("salt").and_then(|v| v.as_str()).unwrap_or(""),
        "difficulty": challenge.get("difficulty").and_then(|v| v.as_i64()).unwrap_or(0),
        "expire_at": challenge.get("expire_at").and_then(|v| v.as_i64()).unwrap_or(0),
    });
    let worker = dir.join("pow_worker.js");
    let input = serde_json::to_vec(&payload).unwrap_or_default();
    let result = std::process::Command::new(&node)
        .arg(&worker)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&input);
            }
            child.wait_with_output().ok()
        });
    let out = result?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(s.trim()).ok()?;
    if v.get("error").is_some() {
        return None;
    }
    v.get("answer").and_then(|a| a.as_i64())
}

async fn ds_fetch_pow(secret: &str, timeout: u64) -> Option<Value> {
    let client = http_client(timeout);
    let resp = client
        .post(format!("{}/api/v0/chat/create_pow_challenge", DS_BASE))
        .headers(headermap(&ds_headers(secret)))
        .body("{\"target_path\":\"/api/v0/chat/completion\"}")
        .send()
        .await
        .ok()?;
    let text = resp.text().await.ok()?;
    let d: Value = serde_json::from_str(&text).ok()?;
    let ch = d.pointer("/data/biz_data/challenge")?;
    if ch.get("challenge").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
        return None;
    }
    let ans = ds_solve_pow(ch)?;
    Some(json!({
        "algorithm": ch.get("algorithm").and_then(|v| v.as_str()),
        "challenge": ch.get("challenge").and_then(|v| v.as_str()),
        "salt": ch.get("salt").and_then(|v| v.as_str()),
        "answer": ans,
        "signature": ch.get("signature").and_then(|v| v.as_str()),
        "target_path": ch.get("target_path").and_then(|v| v.as_str()),
    }))
}

async fn deepseek_chat(messages: &[Value], secret: &str, model: &str, timeout: u64) -> Result<Value, (i64, String)> {
    if secret.is_empty() {
        return Err((401, "缺少 DeepSeek userToken".to_string()));
    }
    let prompt = ds_build_prompt(messages);
    if prompt.is_empty() {
        return Err((400, "没有可发送的消息内容".to_string()));
    }
    let session_id = ds_create_session(secret, timeout).await;
    let mut headers = ds_headers(secret);
    match ds_fetch_pow(secret, timeout).await {
        Some(pow) => {
            let b64 = base64_encode(&pow.to_string());
            headers.push(("x-ds-pow-response".into(), b64));
        }
        None => {
            return Err((
                400,
                "POW 挑战求解失败：需随包 Node+wasm（webpow/）。确认系统有 node 且未被沙箱禁用。".to_string(),
            ));
        }
    }
    let body = json!({
        "chat_session_id": session_id,
        "parent_message_id": Value::Null,
        "prompt": prompt,
        "ref_file_ids": [],
        "thinking_enabled": model.contains("reasoner") || model.to_lowercase().contains("r1") || model.to_lowercase().contains("think"),
        "search_enabled": model.to_lowercase().contains("search"),
        "preempt": false,
    });
    let url = format!("{}/api/v0/chat/completion", DS_BASE);
    let (status, text) = post_collect(&url, &headers, Some(body.to_string().into_bytes()), timeout).await?;
    if !(200..300).contains(&status) {
        return Err((status, format!("DeepSeek 上游 HTTP {}：{}", status, &text[..text.len().min(300)])));
    }
    let mut text_buf: Vec<String> = Vec::new();
    let mut cur = "text";
    for (_ev, data) in parse_sse(&text) {
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let d: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let p = d.get("p").and_then(|v| v.as_str()).unwrap_or("");
        let v = &d["v"];
        let op = d.get("o").and_then(|v| v.as_str()).unwrap_or("");
        if p == "response/fragments" && op == "APPEND" {
            if let Some(arr) = v.as_array() {
                for frag in arr {
                    if let Some(ft) = frag.get("type").and_then(|v| v.as_str()) {
                        cur = if ft == "THINKING" { "thinking" } else { "text" };
                    }
                    if let Some(c) = frag.get("content").and_then(|v| v.as_str()) {
                        if cur == "text" && !c.is_empty() {
                            text_buf.push(c.to_string());
                        }
                    }
                }
                continue;
            }
        }
        if p == "response/fragments/-1/content" {
            if let Some(c) = v.as_str() {
                if cur == "text" {
                    text_buf.push(c.to_string());
                }
            }
            continue;
        }
        // 兜底：标准 choices 增量
        if let Some(arr) = d.get("choices").and_then(|v| v.as_array()) {
            for ch in arr {
                if let Some(c) = ch.pointer("/delta/content").and_then(|v| v.as_str()) {
                    if !c.is_empty() {
                        text_buf.push(c.to_string());
                    }
                }
            }
        }
    }
    let last = text_buf.concat();
    if last.is_empty() {
        return Err((502, "DeepSeek 上游未返回可解析内容（POW/凭据可能失效）".to_string()));
    }
    Ok(envelope(model, &[json!({"message": {"role": "assistant", "content": last}})]))
}

fn ds_build_prompt(messages: &[Value]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for m in messages {
        let c = match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        if c.is_empty() {
            continue;
        }
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let prefix = if role == "user" {
            "用户："
        } else if role == "assistant" {
            "助手："
        } else {
            "系统："
        };
        lines.push(format!("{}{}", prefix, c));
    }
    lines.join("\n\n")
}

// ==================================================================== 公共辅助
/// 把 (status, text) 收集结果归一为 Rust 的 Result<(i64,String),(i64,String)>。
async fn post_collect(
    url: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
    timeout: u64,
) -> Result<(i64, String), (i64, String)> {
    let client = http_client(timeout);
    let mut builder = client.post(url).headers(headermap(headers));
    if let Some(b) = body {
        builder = builder.body(b);
    }
    match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16() as i64;
            let text = resp.text().await.unwrap_or_default();
            Ok((status, text))
        }
        Err(e) => Err((502, format!("上游请求失败：{}", e))),
    }
}

fn headermap(headers: &[(String, String)]) -> reqwest::header::HeaderMap {
    let mut hm = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            hm.insert(name, val);
        }
    }
    hm
}

/// 补齐 OpenAI chat.completion 信封（对齐 Python `_envelope`）。
fn envelope(model: &str, choices: &[Value]) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    json!({
        "id": format!("chatcmpl-{}", nonce_hex()[..24].to_string()),
        "object": "chat.completion",
        "created": now,
        "model": model,
        "choices": choices.iter().enumerate().map(|(i, c)| {
            let mut cc = c.clone();
            cc["index"] = json!(i);
            cc
        }).collect::<Vec<_>>(),
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
    })
}

fn base64_encode(s: &str) -> String {
    // 标准库无 base64，手写编码（仅用于 x-ds-pow-response 头）。
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

// ==================================================================== 调度入口
/// `model@web-glm` 等网页反代的统一入口。返回 OpenAI 信封或 (HTTP状态码, 错误)。
pub async fn chat(
    pid: &str,
    messages: &[Value],
    secret: &str,
    model: &str,
    timeout: u64,
) -> Result<Value, (i64, String)> {
    match pid {
        "web-glm" => glm_chat(messages, secret, model, timeout).await,
        "web-trae" => trae_chat(messages, secret, model, timeout).await,
        "web-deepseek" => deepseek_chat(messages, secret, model, timeout).await,
        _ => Err((400, format!("未知网页反代平台 {}", pid))),
    }
}

/// 把聚合好的 OpenAI 信封转成单段 SSE 流（供客户端 stream:true 使用）。
/// 先发 role，再发整段 content delta，最后 [DONE]。
pub fn envelope_to_sse(payload: &Value) -> axum::response::Response {
    use axum::body::{Body, Bytes};
    use axum::http::{header, StatusCode};
    let text = payload
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("chatcmpl-0");
    let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let mut s = String::new();
    s.push_str(&format!(
        "data: {}\n\n",
        json!({
            "id": id, "object": "chat.completion.chunk", "created": payload.get("created").and_then(|v| v.as_i64()).unwrap_or(0),
            "model": model,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]
        })
    ));
    if !text.is_empty() {
        s.push_str(&format!(
            "data: {}\n\n",
            json!({
                "id": id, "object": "chat.completion.chunk", "created": payload.get("created").and_then(|v| v.as_i64()).unwrap_or(0),
                "model": model,
                "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
            })
        ));
    }
    s.push_str(&format!(
        "data: {}\n\n",
        json!({
            "id": id, "object": "chat.completion.chunk", "created": payload.get("created").and_then(|v| v.as_i64()).unwrap_or(0),
            "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
    ));
    s.push_str("data: [DONE]\n\n");
    let mut resp = axum::response::Response::new(Body::from(Bytes::from(s)));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, header::HeaderValue::from_static("text/event-stream; charset=utf-8"));
    resp
}

/// 静态模型清单（对话写 模型id@pid）。
pub fn model_list(pid: &str) -> Vec<Value> {
    match pid {
        "web-glm" => {
            let base_models: &[(&str, &str, &str)] = &[
                ("glm-5.2", "GLM-5.2", "最新一代"),
                ("glm-5.1", "GLM-5.1", "最新一代"),
                ("glm-5", "GLM-5", "最新一代"),
                ("glm-5-turbo", "GLM-5 Turbo", "最新一代（快速）"),
                ("glm-5v-turbo", "GLM-5V Turbo", "最新一代（视觉）"),
                ("glm-4.7", "GLM-4.7", "新一代"),
                ("glm-4.7-flash", "GLM-4.7 Flash", "新一代（快速免费）"),
                ("glm-4.6", "GLM-4.6", "新一代"),
                ("glm-4.6v-flash", "GLM-4.6V Flash", "新一代（视觉快速）"),
                ("glm-4", "GLM-4", "经典系列"),
                ("glm-4-plus", "GLM-4 Plus", "经典系列（增强）"),
                ("glm-4-flash", "GLM-4 Flash", "经典系列（免费快速）"),
                ("glm-4-air", "GLM-4 Air", "经典系列（轻量）"),
                ("glm-4-all", "GLM-4 All", "经典系列（全工具）"),
                ("glm-4v", "GLM-4V", "经典系列（视觉）"),
                ("glm-zero-preview", "GLM Zero Preview", "零号模型（深度思考）"),
                ("glm-deep-research", "GLM Deep Research", "深度研究"),
                ("glm-4.1v-thinking-flashx", "GLM-4.1V Thinking FlashX", "视觉+思考"),
            ];
            let mut result = Vec::new();
            for (mid, name, desc) in base_models {
                result.push(json!({"id": mid, "name": name, "desc": format!("{}（对话）", desc)}));
                result.push(json!({"id": format!("{}-think", mid), "name": format!("{} Think", name), "desc": format!("{}（深度思考）", desc)}));
                result.push(json!({"id": format!("{}-search", mid), "name": format!("{} Search", name), "desc": format!("{}（联网搜索）", desc)}));
                result.push(json!({"id": format!("{}-think-search", mid), "name": format!("{} Think+Search", name), "desc": format!("{}（深度思考+联网搜索）", desc)}));
            }
            result.push(json!({"id": "glm-image-1", "name": "GLM Image 1", "desc": "图片生成"}));
            result.push(json!({"id": "cogview-4", "name": "CogView-4", "desc": "图片生成（最新）"}));
            result
        }
        "web-trae" => vec![
            json!({"id": "gpt-4o", "name": "GPT-4o", "desc": "Trae 直连"}),
            json!({"id": "claude-3-7-sonnet", "name": "Claude 3.7 Sonnet", "desc": "Trae 直连"}),
            json!({"id": "claude-3-5-sonnet", "name": "Claude 3.5 Sonnet", "desc": "Trae 直连"}),
            json!({"id": "deepseek-v3", "name": "DeepSeek V3", "desc": "Trae 直连"}),
            json!({"id": "deepseek-r1", "name": "DeepSeek R1", "desc": "Trae 直连（推理）"}),
        ],
        "web-deepseek" => vec![
            json!({"id": "deepseek-chat", "name": "DeepSeek Chat", "desc": "DeepSeek 网页版"}),
            json!({"id": "deepseek-reasoner", "name": "DeepSeek Reasoner", "desc": "DeepSeek 网页版（R1 深度思考）"}),
        ],
        _ => vec![],
    }
}

/// 供 registry 判断 `model@<pid>` 是否走网页反代。
pub fn is_web_pid(pid: &str) -> bool {
    matches!(pid, "web-glm" | "web-trae" | "web-deepseek")
}

/// 网页反代默认模型（resolve 用）。
pub fn default_model(pid: &str) -> &'static str {
    match pid {
        "web-glm" => "glm-4-flash",
        "web-trae" => "gpt-4o",
        "web-deepseek" => "deepseek-chat",
        _ => "",
    }
}
