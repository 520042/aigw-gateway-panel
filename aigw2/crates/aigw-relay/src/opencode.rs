//! OpenCode 免费车道（anon-zen）v2 线缆适配器。
//!
//! 参考 _refs/Ebony-Vinyl/dsh-our-free-model/src/upstream.js（2026-10-22 全读）移植：
//! - 端点 /inference/v1/chat/completions（v1 的 /zen/v1 已停用，收到即 403 FreeTierError）；
//! - 请求头带 x-opencode-* 会话组（v2 闸门），**不带** x-opencode-request（v1 头，带即 403）；
//! - 会话 id 必须满足 ses_[0-9a-f]{12}[0-9A-Za-z]{14} 且低 12 位 hex 在 0xfc0-0xfff；
//! - 工具闸门：read/shell 两个占位工具必须逐字声明，调用方同名不同定义改名 <name>_client[N]，
//!   且绝不下发 tool_choice 'none'（v2 见 none 即 403）。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use bytes::Bytes;
use futures_util::StreamExt;

use crate::{RelayError, StreamStat};

/// 上游根（spec.base_url 为空或非注入地址时使用）。
pub const OPENCODE_BASE: &str = "https://opencode.ai";
/// v2 线缆要求的整串 UA（形状 opencode/<channel>/<version>/<client>）。
pub const OPENCODE_UA: &str = "opencode/latest/2.0.26/cli";

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn er(status: u16, msg: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError {
        status,
        body: msg.into(),
        class,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn base_url(spec: &ProviderSpec) -> String {
    let b = spec.base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        OPENCODE_BASE.to_string()
    } else {
        b.to_string()
    }
}

// ---------------------------------------------------------------- 会话 id

/// base62From：每个 byte % 62 映射 BASE62 表（与参考逐字一致）。
fn base62_from(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| BASE62[(b % 62) as usize] as char)
        .collect()
}

/// base62Fixed：10 字节大端整数按 62 进制填满 width 位（canonicalize 尾部）。
fn base62_fixed(bytes: &[u8], width: usize) -> String {
    let mut n = 0u128;
    for b in bytes {
        n = (n << 8) | (*b as u128);
    }
    let mut out = vec![0u8; width];
    let mut i = width;
    while n > 0 && i > 0 {
        i -= 1;
        out[i] = BASE62[(n % 62) as usize];
        n /= 62;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// mintSessionId：~(ts_ms << 12 | seq) 大端 6 字节 + 14 位 base62 随机尾部。
fn mint_session_id(ts: u64, seq: u64) -> String {
    let value = !(ts.wrapping_mul(0x1000).wrapping_add(seq));
    let mut hex = String::new();
    for i in 0..6 {
        hex.push_str(&format!("{:02x}", ((value >> (40 - 8 * i)) & 0xff) as u8));
    }
    let tail: Vec<u8> = (0..14).map(|_| rand::random::<u8>()).collect();
    format!("ses_{}{}", hex, base62_from(&tail))
}

/// canonicalizeSession：sha256('ses\\0' + signal)，确定性重编码为合法会话 id。
fn canonicalize_session(signal: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"ses\\0");
    h.update(signal.as_bytes());
    let d = h.finalize();
    let d = d.as_slice();
    let current = ((((d[0] as u64) << 28)
        | ((d[1] as u64) << 20)
        | ((d[2] as u64) << 12)
        | ((d[3] as u64) << 4)
        | ((d[4] as u64) >> 4))
        << 12)
        | 1;
    let inverted = !current & 0xffffffffffff;
    let time_hex = format!("{:012x}", inverted);
    format!("ses_{}{}", time_hex, base62_fixed(&d[6..16], 14))
}

const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);

/// 进程级兜底会话（无 conversation 信号时共用，避免每请求铸造新会话）。
static FALLBACK_SESSION: Mutex<Option<(String, Instant)>> = Mutex::new(None);

// ---------------------------------------------------------------- 工具闸门

/// read 占位工具逐字定义（v2 闸门要求，必须与参考一字不差）。
const READ_TOOL: &str = r#"{"name":"read","description":"Read the contents of a file or directory. Supports text files, images, and PDFs. Images and PDFs are presented directly to the model. Each text line is prefixed by its 1-based line number as <line>: <content>. The prefix is for reference and is not part of the file content. Directory entries are returned one per line. Use offset and limit to read large files or directories in sections. Prefer one larger read over many small slices, and use grep to find specific content in large files.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"File or directory to read"},"offset":{"type":"integer","minimum":0,"description":"The line or directory entry to start reading from (1-based)"},"limit":{"type":"integer","minimum":0,"description":"The maximum number of lines or directory entries to read (defaults to and capped at 2000)"}},"required":["path"],"additionalProperties":false}}"#;
/// shell 占位工具逐字定义。
const SHELL_TOOL: &str = r#"{"name":"shell","description":"Execute a shell command and return its output. Commands run on Linux using bash. Quote file paths containing spaces or special characters. Prefer dedicated tools over shell commands when possible. When output is large, the full result is saved to a file and a truncated preview is returned. Rely on automatic truncation unless filtering the output is more useful. Commands accept an optional timeout, background commands have no timeout by default. Background commands return immediately, and you will be notified when they complete.","parameters":{"type":"object","properties":{"command":{"type":"string","description":"Shell command string to execute"},"workdir":{"type":"string","description":"Working directory to execute the command in. Defaults to the current working directory. When possible, avoid changing directories in the command and set the working directory here instead."},"timeout":{"type":"integer","minimum":0,"description":"Timeout in milliseconds. Set to 0 to disable the timeout. Defaults to 120000 for foreground commands. Background commands have no timeout by default."},"background":{"type":"boolean","description":"Run the command in the background and return immediately (useful for dev servers and long-running builds). You do not need to use '&' at the end of the command when using this parameter. You will be notified when it completes. DO NOT poll for completion."}},"required":["command"],"additionalProperties":false}}"#;

static GATE_TOOLS: OnceLock<Vec<Value>> = OnceLock::new();

fn gate_tools() -> &'static Vec<Value> {
    GATE_TOOLS.get_or_init(|| {
        let read: Value = serde_json::from_str(READ_TOOL).expect("read tool def");
        let shell: Value = serde_json::from_str(SHELL_TOOL).expect("shell tool def");
        vec![read, shell]
    })
}

/// 取工具名（兼容 {type,function:{name}} 与 {name} 两种形状）。
fn tool_name(t: &Value) -> String {
    t.get("function")
        .and_then(|f| f.get("name"))
        .and_then(|n| n.as_str())
        .or_else(|| t.get("name").and_then(|n| n.as_str()))
        .unwrap_or("")
        .to_string()
}

/// 改工具名（两种形状都改）。
fn tool_set_name(t: &mut Value, name: &str) {
    if let Some(f) = t.get_mut("function") {
        f["name"] = json!(name);
    } else {
        t["name"] = json!(name);
    }
}

/// 是否携带与门卫逐字一致的定义（description + parameters JSON 相等）。
fn carries_gate_def(t: &Value, want: &Value) -> bool {
    let (desc, params) = if let Some(f) = t.get("function") {
        (f.get("description"), f.get("parameters"))
    } else {
        (t.get("description"), t.get("parameters"))
    };
    let (wd, wp) = (want.get("description"), want.get("parameters"));
    match (desc, params, wd, wp) {
        (Some(d), Some(p), Some(wd), Some(wp)) => d == wd && p == wp,
        _ => false,
    }
}

/// applyFingerprint：注入 read/shell 门卫工具、改名冲突、删除 tool_choice none。
/// 返回 (alias -> original) 映射，响应侧需还原。
fn apply_fingerprint(v: &mut Value) -> Vec<(String, String)> {
    let mut tools: Vec<Value> = v
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let mut seen: Vec<String> = Vec::new();
    let mut present: Vec<String> = Vec::new();
    let mut alias: Vec<(String, String)> = Vec::new();
    let gate = gate_tools();
    let mut out: Vec<Value> = Vec::new();
    for t in tools.drain(..) {
        let name = tool_name(&t);
        if name.is_empty() {
            continue;
        }
        if seen.iter().any(|s| s == &name) {
            continue; // 同名重复只留第一个
        }
        seen.push(name.clone());
        let mut t = t;
        let is_gate = name == "read" || name == "shell";
        if is_gate {
            let want = gate
                .iter()
                .find(|g| tool_name(g) == name)
                .cloned()
                .unwrap_or(Value::Null);
            if !want.is_null() && carries_gate_def(&t, &want) {
                present.push(name.clone());
            } else {
                // 定义不同 → 改名 <name>_client[N]，原工具名让给门卫
                let mut n = 1;
                let mut cand = format!("{}_client", name);
                while seen.iter().any(|s| s == &cand) {
                    n += 1;
                    cand = format!("{}_client{}", name, n);
                }
                alias.push((cand.clone(), name.clone()));
                tool_set_name(&mut t, &cand);
                seen.push(cand);
            }
        }
        out.push(t);
    }
    // 门卫工具必须声明：缺失即注入（定义逐字）
    let mut injected: Vec<String> = Vec::new();
    for g in gate.iter() {
        let gn = tool_name(g);
        if !present.iter().any(|p| p == &gn) {
            // 以 OpenAI chat 形状声明（{type:"function", function:{name,description,parameters}}）
            out.push(json!({ "type": "function", "function": g }));
            injected.push(gn);
        }
    }
    v["tools"] = json!(out);
    // 绝不下发 tool_choice 'none'
    let drop_choice = match v.get("tool_choice") {
        Some(Value::String(s)) => s == "none",
        Some(Value::Object(o)) => o.get("type").and_then(|t| t.as_str()) == Some("none"),
        _ => false,
    };
    if drop_choice {
        if let Some(obj) = v.as_object_mut() {
            obj.remove("tool_choice");
        }
    }
    // 注入占位说明（追加到 system 消息）
    if !injected.is_empty() {
        let note = if injected.len() == 2 {
            "Do not call the `read` or `shell` tools; they are placeholders that do nothing. Use the other tools you have been given instead."
        } else {
            &format!(
                "Do not call the `{}` tool; they are placeholders that do nothing. Use the other tools you have been given instead.",
                injected[0]
            )
        };
        let mut placed = false;
        if let Some(msgs) = v.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for m in msgs.iter_mut() {
                if m.get("role").and_then(|r| r.as_str()) == Some("system") {
                    if let Some(c) = m.get("content").and_then(|c| c.as_str()) {
                        m["content"] = json!(format!("{} {}", c, note));
                        placed = true;
                        break;
                    }
                }
            }
            if !placed {
                msgs.push(json!({ "role": "system", "content": note }));
            }
        }
    }
    alias
}

/// 响应侧还原：把 choices[].message.tool_calls[].function.name 的 alias 换回原名。
fn restore_tool_calls(v: &mut Value, alias: &[(String, String)]) {
    if alias.is_empty() {
        return;
    }
    let fix = |n: &mut Value| {
        if let Some(s) = n.as_str() {
            if let Some((_, orig)) = alias.iter().find(|(a, _)| a == s) {
                *n = json!(orig);
            }
        }
    };
    if let Some(choices) = v.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for ch in choices.iter_mut() {
            if let Some(tc) = ch
                .get_mut("message")
                .and_then(|m| m.get_mut("tool_calls"))
                .and_then(|t| t.as_array_mut())
            {
                for t in tc.iter_mut() {
                    if let Some(n) = t.get_mut("function").and_then(|f| f.get_mut("name")) {
                        fix(n);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- 请求头与发信

fn headers(spec: &ProviderSpec, session: &str, stream: bool) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        (
            "accept".to_string(),
            if stream { "text/event-stream" } else { "*/*" }.to_string(),
        ),
        ("authorization".to_string(), "Bearer public".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
        ("user-agent".to_string(), OPENCODE_UA.to_string()),
        ("x-opencode-client".to_string(), "cli".to_string()),
        ("x-opencode-session".to_string(), session.to_string()),
        ("x-opencode-session-id".to_string(), session.to_string()),
        ("x-session-affinity".to_string(), session.to_string()),
        ("x-session-id".to_string(), session.to_string()),
        ("x-opencode-project".to_string(), "global".to_string()),
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

/// 会话信号：优先 metadata.conversation_id，其次 user；都无则用进程级兜底会话。
fn conversation_signal(v: &Value) -> String {
    let sig = v
        .get("metadata")
        .and_then(|m| m.get("conversation_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .or_else(|| v.get("user").and_then(|x| x.as_str()).map(|s| s.to_string()));
    match sig {
        Some(s) if !s.is_empty() => canonicalize_session(&s),
        _ => {
            let mut guard = match FALLBACK_SESSION.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            if let Some((sid, at)) = guard.as_ref() {
                if at.elapsed() < SESSION_TTL {
                    return sid.clone();
                }
            }
            let sid = mint_session_id(now_ms(), 1);
            *guard = Some((sid.clone(), Instant::now()));
            sid
        }
    }
}

/// 统一准备：解析 body、铸造会话、注入门卫工具、组头组 URL。
/// 返回 (Stage, tool alias 映射)。
pub async fn prepare(
    _http: &reqwest::Client,
    spec: &ProviderSpec,
    _secret: &str,
    body: &[u8],
) -> Result<(Stage, Vec<(String, String)>), RelayError> {
    let mut v: Value = serde_json::from_slice(body).map_err(|e| {
        er(400, format!("bad json: {e}"), ErrorClass::Rejected)
    })?;
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let alias = apply_fingerprint(&mut v);
    let session = conversation_signal(&v);
    let stream = v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    let hs = headers(spec, &session, stream);
    let url = format!("{}/inference/v1/chat/completions", base_url(spec));
    Ok((
        Stage {
            session,
            url,
            hs,
            payload: v,
            model,
        },
        alias,
    ))
}

pub struct Stage {
    pub session: String,
    pub url: String,
    pub hs: Vec<(String, String)>,
    pub payload: Value,
    pub model: String,
}

fn data_of(block: &str) -> Option<&str> {
    block
        .lines()
        .find_map(|l| l.strip_prefix("data:").map(|s| s.trim()))
}

/// 按上游 body 精修错误类别。
///
/// 403 在本车道有三种完全不同的含义，靠状态码分不出来，必须看 body：
/// - `FreeTierError` / "free tier can only be used from within" → 车道级封锁（客户端指纹被拒）。
///   归 Auth 会让整条匿名车道被 30 分钟拉黑并提示用户「重新登录」——匿名车道没有凭据可登。
/// - `RegionError` / "not available in your country" → 地域封锁。换号无用，得换出口或车道。
/// - 其余 401/403 → 才是真的凭据问题（本车道用不到，但保持语义一致）。
///
/// 另外 400 的 "Model is unavailable" 是模型侧不可用（不是请求写错），归 Transient；
/// 410 `ModelDeprecated` 归 Rejected（调用方该换模型名）。
fn classify(status: u16, body: &str) -> ErrorClass {
    let low = body.to_ascii_lowercase();
    if status == 403 {
        if low.contains("freetiererror")
            || low.contains("free tier can only be used from within")
        {
            return ErrorClass::LaneLocked;
        }
        if low.contains("regionerror") || low.contains("not available in your country") {
            return ErrorClass::Region;
        }
        return ErrorClass::Auth;
    }
    match status {
        401 => ErrorClass::Auth,
        402 => ErrorClass::Quota,
        429 => ErrorClass::Rate,
        // 410 ModelDeprecated / 400 Model is unavailable：模型本身在变（免费车道模型换得很勤），
        // 前者是「这个 id 没了」，后者是「这个 id 现在不可用」，都不该熔断平台。
        410 => ErrorClass::Rejected,
        400 => {
            if low.contains("model is unavailable") || low.contains("modeldeprecated") {
                ErrorClass::Transient
            } else {
                ErrorClass::Rejected
            }
        }
        _ => ErrorClass::Transient,
    }
}

fn est_tokens(s: &str) -> i64 {
    (s.chars().count() / 2).max(1) as i64
}

/// 非流式：POST /inference/v1/chat/completions。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let (st, alias) = prepare(http, spec, secret, body).await?;
    let resp = apply(http.post(&st.url), &st.hs)
        .json(&st.payload)
        .timeout(Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| er(502, format!("OpenCode 对话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        // 只把上游原文带上，**不要**在这里拼建议文案：
        // 错误信封由 server 层 `upstream_hint` 统一生成（它能看到 class + body），
        // 两层各拼一次会让用户看到「（这不是凭据问题…）（建议：这不是凭据问题…）」
        // 这种重复两遍的 message。
        let class = classify(status, &text);
        return Err(er(
            status,
            format!("OpenCode 上游 {}：{}", status, truncate(&text, 300)),
            class,
        ));
    }
    let trimmed = text.trim_start();
    let mut v: Value = if trimmed.starts_with("data:") || trimmed.starts_with("event:") {
        match crate::aggregate_sse_nonstream(&text) {
            Ok(Some(v)) => v,
            Ok(None) => serde_json::from_str(&text).unwrap_or(json!({})),
            Err(msg) => {
                return Err(er(
                    400,
                    format!("上游流内错误：{}", msg),
                    ErrorClass::Rejected,
                ))
            }
        }
    } else {
        serde_json::from_str(&text).map_err(|e| {
            er(
                if (200..300).contains(&status) { 502 } else { status },
                format!(
                    "bad json: {}; head={}",
                    e,
                    text.chars().take(300).collect::<String>()
                ),
                ErrorClass::Transient,
            )
        })?
    };
    restore_tool_calls(&mut v, &alias);
    // 记账兜底：上游没给 usage 时估算
    if v.get("usage").map(|u| u.is_null()).unwrap_or(true) {
        let content = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|ch| ch.get("message").and_then(|m| m.get("content")))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        let tout = est_tokens(content);
        let tin = est_tokens(
            &st.payload
                .get("messages")
                .map(|m| m.to_string())
                .unwrap_or_default(),
        );
        v["usage"] = json!({
            "prompt_tokens": tin,
            "completion_tokens": tout,
            "total_tokens": tin + tout,
        });
    }
    Ok(v)
}

/// 流式：透传 SSE；tool_calls 别名按需重写；统计通道并行记账。
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
    let (st, alias) = prepare(http, spec, secret, body).await?;
    let resp = apply(http.post(&st.url), &st.hs)
        .json(&st.payload)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| er(502, format!("OpenCode 对话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        let class = classify(status, &text);
        // 同非流式：建议文案由 server 层统一生成，这里不重复拼。
        return Err(er(
            status,
            format!("OpenCode 上游 {}：{}", status, truncate(&text, 300)),
            class,
        ));
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let in_bytes = body.len();
    let alias2 = alias.clone();
    tokio::spawn(async move {
        let mut stat = StreamStat::default();
        let mut buf = String::new();
        let mut dec = crate::Utf8Decoder::new();
        let mut out_chars: usize = 0;
        let mut done_seen = false;
        let mut stream = resp.bytes_stream();
        'outer: while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(b) => b,
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {}", e);
                    break;
                }
            };
            buf.push_str(&dec.push(&chunk));
            let mut out = String::new();
            while let Some((pos, sep)) = crate::find_frame_end(&buf) {
                let block: String = buf.drain(..pos + sep).collect();
                if block.contains("[DONE]") {
                    done_seen = true;
                    out.push_str(&block);
                    continue;
                }
                let Some(data) = data_of(&block) else { continue };
                if data.contains("tool_calls") && !alias2.is_empty() {
                    if let Ok(mut fv) = serde_json::from_str::<Value>(data) {
                        if let Some(choices) = fv.get_mut("choices").and_then(|c| c.as_array_mut()) {
                            for ch in choices.iter_mut() {
                                if let Some(tc) = ch
                                    .get_mut("delta")
                                    .and_then(|d| d.get_mut("tool_calls"))
                                    .and_then(|t| t.as_array_mut())
                                {
                                    for t in tc.iter_mut() {
                                        if let Some(n) = t
                                            .get_mut("function")
                                            .and_then(|f| f.get_mut("name"))
                                        {
                                            if let Some(s) = n.as_str() {
                                                if let Some((_, orig)) =
                                                    alias2.iter().find(|(a, _)| a == s)
                                                {
                                                    *n = json!(orig);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(c) = fv
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|ch| ch.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            out_chars += c.chars().count();
                        }
                        if let Some(fr) = fv
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|ch| ch.get("finish_reason"))
                            .and_then(|f| f.as_str())
                        {
                            stat.finish_reason = Some(fr.to_string());
                        }
                        out.push_str(&format!("data: {}\n\n", fv));
                        continue;
                    }
                }
                if let Ok(fv) = serde_json::from_str::<Value>(data) {
                    if let Some(c) = fv
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|ch| ch.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        out_chars += c.chars().count();
                    }
                    if let Some(fr) = fv
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|ch| ch.get("finish_reason"))
                        .and_then(|f| f.as_str())
                    {
                        stat.finish_reason = Some(fr.to_string());
                    }
                    if let Some(e) = fv.get("error") {
                        stat.error = Some(ErrorClass::from_status(400));
                        stat.error_msg = e.to_string();
                    }
                }
                out.push_str(&block);
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
        let mut tail = String::new();
        if !done_seen && stat.error.is_none() {
            tail.push_str("data: [DONE]\n\n");
        }
        if let Some(ec) = stat.error {
            let etype = match ec {
                ErrorClass::Rate => "rate_limit_error",
                ErrorClass::Auth => "authentication_error",
                ErrorClass::Quota => "insufficient_quota",
                // 车道级封锁/地域封锁走 permission_error：OpenAI SDK 对 401/403 会触发
                // 「重新认证」，而免 Key 车道没有可重登的凭据。
                ErrorClass::LaneLocked | ErrorClass::Region => "permission_error",
                _ => "api_error",
            };
            let msg = stat.error_msg.replace('"', "'");
            tail.push_str(&format!(
                "data: {}\n\n",
                json!({"error": {"message": msg, "type": etype}})
            ));
        }
        let _ = bytes_tx.send(Ok(Bytes::from(tail))).await;
        let (tin, tout) = crate::estimate_stream_tokens(in_bytes, out_chars);
        stat.tokens_in = tin;
        stat.tokens_out = tout;
        let _ = tx.send(stat).await;
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    Ok((Box::pin(stream), rx))
}

/// 验活：GET /inference/v1/models，2xx 即通（匿名车道无需凭据）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, _secret: &str) -> bool {
    let session = mint_session_id(now_ms(), 1);
    let hs = headers(spec, &session, false);
    let url = format!("{}/inference/v1/models", base_url(spec));
    match apply(http.get(&url), &hs)
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// 截断（错误消息用）。
fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::Router;

    fn spec_for(base: &str) -> ProviderSpec {
        serde_json::from_value::<ProviderSpec>(json!({
            "id": "anon-zen",
            "name": "Zen",
            "enabled": true,
            "auth": "anon",
            "base_url": base,
            "format": "openai",
            "models": [],
            "models_policy": "static",
            "stream_dialect": "sse_delta",
            "upstream_kind": "opencode",
            "region": "global",
            "headers": {},
        }))
        .expect("spec")
    }

    #[test]
    fn session_id_shape_and_descending() {
        let sid = mint_session_id(1_700_000_000_000, 1);
        assert!(sid.starts_with("ses_"));
        let hex12 = &sid[4..16];
        assert_eq!(hex12.len(), 12);
        assert!(hex12.chars().all(|c| c.is_ascii_hexdigit()));
        let low12 = u64::from_str_radix(hex12, 16).unwrap() & 0xfff;
        assert!((0xfc0..=0xfff).contains(&low12), "低 12 位必须 0xfc0-0xfff，got {}", hex12);
        let tail = &sid[16..];
        assert_eq!(tail.len(), 14);
        assert!(tail.chars().all(|c| BASE62.contains(&(c as u8))));
    }

    #[test]
    fn canonical_session_is_stable_and_well_formed() {
        let a = canonicalize_session("conv-1");
        let b = canonicalize_session("conv-1");
        assert_eq!(a, b);
        let c = canonicalize_session("conv-2");
        assert_ne!(a, c);
        let hex12 = &a[4..16];
        assert_eq!(hex12.len(), 12);
        let low = u64::from_str_radix(hex12, 16).unwrap();
        assert!(
            (0xfc0..=0xfff).contains(&(low & 0xfff)),
            "低 12 位 0xfc0-0xfff，got {low:#x}"
        );
    }

    #[test]
    fn fingerprint_injects_gate_tools_and_strips_none() {
        let mut v = json!({
            "model": "deepseek-v4.1-flash-free",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "my_tool", "description": "d", "parameters": {"type": "object"}}}],
            "tool_choice": "none",
        });
        let alias = apply_fingerprint(&mut v);
        assert!(alias.is_empty());
        let tools = v["tools"].as_array().unwrap();
        let names: Vec<String> = tools.iter().map(tool_name).collect();
        assert!(names.contains(&"read".to_string()), "缺 read 门卫: {names:?}");
        assert!(names.contains(&"shell".to_string()), "缺 shell 门卫: {names:?}");
        assert!(v.get("tool_choice").is_none(), "none 必须删除");
        let msgs = v["messages"].as_array().unwrap();
        assert!(msgs.iter().any(|m| {
            m.get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.contains("placeholders"))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn fingerprint_renames_conflicting_caller_tools() {
        let mut v = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {"type": "function", "function": {"name": "read", "description": "client own", "parameters": {"type": "object", "properties": {}}}},
                {"type": "function", "function": {"name": "shell", "description": "client own", "parameters": {"type": "object", "properties": {}}}},
            ],
        });
        let alias = apply_fingerprint(&mut v);
        assert_eq!(alias.len(), 2, "两个冲突工具都要改名: {alias:?}");
        let names: Vec<String> = v["tools"].as_array().unwrap().iter().map(tool_name).collect();
        assert!(names.contains(&"read_client".to_string()), "{names:?}");
        assert!(names.contains(&"shell_client".to_string()), "{names:?}");
        assert!(names.contains(&"read".to_string()));
        assert!(names.contains(&"shell".to_string()));
        // 门卫 read/shell 定义逐字
        let read = v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| tool_name(t) == "read")
            .unwrap();
        assert!(carries_gate_def(read, &gate_tools()[0]));
        // 响应侧还原
        let mut resp = json!({
            "choices": [{"message": {"role": "assistant", "content": "", "tool_calls": [{"id": "1", "type": "function", "function": {"name": "read_client", "arguments": "{}"}}]}}]
        });
        restore_tool_calls(&mut resp, &alias);
        let name = resp["choices"][0]["message"]["tool_calls"][0]["function"]["name"]
            .as_str()
            .unwrap();
        assert_eq!(name, "read");
    }

    #[test]
    fn classify_403_by_body_shape() {
        // 实测原文（2026-10-22）：车道级封锁与地域封锁都是 403，只能靠 body 区分。
        let lane = r#"{"type":"error","error":{"type":"FreeTierError","message":"OpenCode's free tier can only be used from within OpenCode. Download it at https://opencode.ai and keep building."}}"#;
        assert_eq!(classify(403, lane), ErrorClass::LaneLocked);
        let region = r#"{"error":{"type":"RegionError","message":"This model is not available in your country."}}"#;
        assert_eq!(classify(403, region), ErrorClass::Region);
        // 真凭据问题仍是 Auth
        assert_eq!(classify(403, r#"{"error":"forbidden"}"#), ErrorClass::Auth);
        assert_eq!(classify(401, ""), ErrorClass::Auth);
        // 429 是限流不是封锁
        assert_eq!(classify(429, r#"{"error":{"type":"FreeUsageLimitError"}}"#), ErrorClass::Rate);
        // 400 "Model is unavailable" 是模型侧不可用 → 上游侧，不熔断账号
        assert_eq!(
            classify(400, r#"{"error":{"type":"server_error","message":"Upstream request failed: Model is unavailable."}}"#),
            ErrorClass::Transient
        );
        // 410 ModelDeprecated：调用方该换模型名
        assert_eq!(
            classify(410, r#"{"type":"error","error":{"type":"ModelDeprecated"}}"#),
            ErrorClass::Rejected
        );
    }

    /// 403 车道级封锁：class 必须是 LaneLocked，且 relay 层**不**拼建议文案。
    ///
    /// 建议文案统一由 server 层 `upstream_hint` 生成（它同时能看到 class 和 body）。
    /// 两层各拼一次，用户会看到「（这不是凭据问题…）（建议：这不是凭据问题…）」
    /// —— 这是 2026-10-22 实测到的真实输出。
    #[tokio::test]
    async fn lane_locked_error_carries_raw_body_only() {
        let body = r#"{"type":"error","error":{"type":"FreeTierError","message":"OpenCode's free tier can only be used from within OpenCode."}}"#;
        let base = spawn_mock_status(body, "application/json", 403).await;
        let spec = spec_for(&base);
        let e = chat_nonstream(
            &reqwest::Client::new(),
            &spec,
            "",
            br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"stream":false}"#,
        )
        .await
        .expect_err("403 必须是错误");
        assert_eq!(e.class, ErrorClass::LaneLocked, "class={:?}", e.class);
        // 上游原文保留
        assert!(e.body.contains("FreeTierError"), "body={}", e.body);
        // 建议文案不能出现在 relay 层：它由 server 层 upstream_hint 负责，
        // 否则最终 message 里同一句话会出现两遍。
        assert!(!e.body.contains("这不是凭据问题"), "body={}", e.body);
        assert!(!e.body.contains("无需也无法重新登录"), "body={}", e.body);
    }

    async fn spawn_mock(body: &'static str, content_type: &'static str) -> String {
        spawn_mock_status(body, content_type, 200).await
    }

    async fn spawn_mock_status(
        body: &'static str,
        content_type: &'static str,
        status: u16,
    ) -> String {
        let app = Router::new().fallback(move |req: axum::extract::Request| async move {
            let path = req.uri().path().to_string();
            if path.contains("/models") {
                return axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .body(Body::from("{\"data\": [{\"id\": \"m-1\"}]}"))
                    .unwrap();
            }
            axum::response::Response::builder()
                .status(status)
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn nonstream_via_mock() {
        let base = spawn_mock(
            "{\"id\":\"cmpl-1\",\"object\":\"chat.completion\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":\"OK\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}",
            "application/json",
        )
        .await;
        let spec = spec_for(&base);
        let out = chat_nonstream(
            &reqwest::Client::new(),
            &spec,
            "",
            br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"stream":false}"#,
        )
        .await
        .expect("nonstream ok");
        assert_eq!(out["choices"][0]["message"]["content"], "OK");
        assert_eq!(out["usage"]["completion_tokens"], 1);
    }

    #[tokio::test]
    async fn stream_via_mock_passthrough() {
        let base = spawn_mock(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        )
        .await;
        let spec = spec_for(&base);
        let (s, mut rx) = chat_stream(
            &reqwest::Client::new(),
            &spec,
            "",
            br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        )
        .await
        .expect("stream ok");
        let mut all = String::new();
        use futures_util::StreamExt;
        tokio::pin!(s);
        while let Some(b) = s.next().await {
            all.push_str(&String::from_utf8_lossy(&b.unwrap()));
        }
        assert!(all.contains("Hi"), "{all}");
        let st = rx.recv().await.expect("stat");
        assert_eq!(st.tokens_out, 1); // Hi=2 字符 / 2
    }
}
