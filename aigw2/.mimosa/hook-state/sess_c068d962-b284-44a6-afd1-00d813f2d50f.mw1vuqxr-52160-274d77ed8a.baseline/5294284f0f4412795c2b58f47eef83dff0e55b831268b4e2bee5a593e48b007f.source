//! MonkeyCode（长亭科技）—— 建任务 → WebSocket 任务流 → ACP 事件翻译。
//!
//! ── 上游长什么样（逆向来源 Acankao/MonkeyCodeReverseEngineer）────
//! ```text
//!   认证   GET  /api/v1/users/status        Cookie: monkeycode_ai_session=…
//!   目录   GET  /api/v1/users/models        → {code:0,data:{models:[…]}}
//!   任务   POST /api/v1/users/tasks         （content + model_id + cli_name + image_id）
//!   任务流 WS   /api/v1/users/tasks/stream?id=<taskId>&mode=new  （ACP 事件）
//! ```
//!
//! ── 为什么必须有状态（不是「一次 HTTP 一次回答」）──────────────
//! 一次对话 = 建任务 + 挂 WebSocket + 等轮次结束；任务 id 由上游生成、
//! 事件由 VM 内的 coding agent 实时推送。单请求构造容纳不了这条链。
//!
//! ── 终态怎么判（不挂死的关键）─────────────────────────────────
//! **四个出口都要给下游一个收尾**（finish 帧 + [DONE]，或错误帧）：
//!   ① task-ended → 正常收尾；
//!   ② task-error → 错误帧收尾；
//!   ③ 连接关闭 / 传输错误 → 错误帧收尾；
//!   ④ 空闲超时（上游每 10 秒有 ping，所以空闲即断线）或总时长超限 → 错误帧收尾。
//! ★ 参考实现在这几处是「静默 resolve、靠客户端等 EOF」；本网关**必须显式收尾**
//!   —— 客户端的 SSE 解析器等的是 [DONE]，只断连接会被渲染成「回答被截断」。
//!
//! ── 自动批准 / 自动回复（这家能不能用的关键）────────────────────
//! 连接建立后**必须先发** auto-approve（{"type":"auto-approve"}）再发 user-input；
//! 不发 auto-approve 的话，任务会停在第一个工具调用等用户确认 —— 永远不动。
//! Agent 提问时自动回 reply-question，任务才能继续。
//!
//! ── ACP 事件映射 ────────────────────────────────────────────
//! agent_message_chunk → content；agent_thought_chunk → reasoning_content；
//! tool_call → tool_calls（整体）；tool_call_update → 参数增量；
//! usage_update → 累计 usage（非零才覆盖）；plan / available_commands_update → 忽略。

use serde_json::{json, Value};

use crate::common::{er, nanos_hex, secs, truncate};
use crate::{ErrorClass, RelayError};

/// 空闲超时：上游每 10 秒有 ping，所以「一段时间没有任何下行」就是断线。
const IDLE_TIMEOUT_SECS: u64 = 300;

/// 单次任务的总时长上限。
///
/// ★ 为什么光有空闲超时不够：上游每 10 秒发一次 ping，而循环里收到 ping 就
///   `continue` —— 那会**重置** `tokio::time::timeout` 的计时器。于是任务在 VM 里
///   卡住（agent 不产出、也不报错）时，ping 一直来，空闲超时**永远不触发**，
///   这条连接会一直挂到客户端自己断开。模块头写的「或总时长超限」原本没有实现。
///   这里补上真正的硬上限：无论有心跳与否，到点就按错误帧收尾。
///
/// 取值理由：这家一次任务 = VM 内建仓 + agent 多轮工具循环，正常 1~5 分钟。
/// 30 分钟足够覆盖大改动的长任务，同时避免卡死连接无限占住一条上游通道。
const MAX_TOTAL_SECS: u64 = 1800;

/// 总时长上限到了没有（提成纯函数以便单测）。
///
/// ★ 这条判据的存在理由见 [`MAX_TOTAL_SECS`]：上游的心跳会让空闲超时永不触发，
///   所以必须有一个与「有没有下行」无关的硬时钟。
fn total_budget_exceeded(elapsed_secs: u64) -> bool {
    elapsed_secs >= MAX_TOTAL_SECS
}

/// 把 OpenAI messages 翻成 MonkeyCode 的 prompt + system_prompt。
///
/// 规则（对齐参考的 messagesToPrompt）：
///   · 第一条**非空** system 抽出来单独走 `system_prompt` 字段（只取一条）；
///   · 其余按角色加前缀：`[User]` / `[Assistant]`，段间以空行连接；
///   · `tools` / `tool_choice` **不透传** —— 上游 Agent 在 VM 内自带工具循环，
///     建任务接口没有接收客户端工具表的字段。客户端带工具表是常态，不为此报错。
pub fn build_prompt(v: &Value) -> (String, Option<String>) {
    let mut system_prompt: Option<String> = None;
    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = v.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let text = content_text(m);
            if text.trim().is_empty() {
                continue;
            }
            if role == "system" {
                if system_prompt.is_none() {
                    system_prompt = Some(text);
                }
                continue;
            }
            let tag = if role == "assistant" { "[Assistant]" } else { "[User]" };
            parts.push(format!("{tag}\n{text}"));
        }
    }
    if parts.is_empty() {
        parts.push("[User]\n你好".to_string());
    }
    (parts.join("\n\n"), system_prompt)
}

fn content_text(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .or_else(|| p.get("input_text"))
                    .and_then(|t| t.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// `interface_type` → CLI 名（建任务要这个字段）。
pub fn cli_name_for(interface_type: &str) -> &'static str {
    match interface_type {
        "anthropic" => "claude",
        "openai_responses" => "codex",
        _ => "opencode",
    }
}

/// ACP 事件翻译状态机：一条下行帧进，零到多条 chat delta 出。
#[derive(Default)]
pub struct AcpAcc {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<Value>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub done: bool,
    pub error: Option<String>,
}

impl AcpAcc {
    /// 吃一条下行 WS 帧，返回要下发的 delta 列表（可能为空）。
    pub fn feed(&mut self, frame: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        let ty = frame.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match ty {
            // 心跳：由调用方决定回不回，这里只标一下（不产生 delta）
            "ping" | "task-started" => {}
            "task-ended" => {
                self.done = true;
                // 终帧可能带最终 usage
                if let Some(u) = frame.get("data").and_then(|d| d.get("usage")) {
                    self.read_usage(u);
                }
            }
            "task-error" => {
                let msg = frame
                    .get("data")
                    .and_then(|d| d.get("message").or_else(|| d.get("error")))
                    .and_then(|m| m.as_str())
                    .or_else(|| frame.get("message").and_then(|m| m.as_str()))
                    .unwrap_or("上游任务出错");
                self.error = Some(msg.to_string());
                self.done = true;
            }
            "task-running" => {
                let kind = frame.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                let data = frame.get("data");
                match kind {
                    "acp_event" => {
                        // data 是 **JSON 字符串**（不是对象）—— 二次解析
                        if let Some(s) = data.and_then(|d| d.as_str()) {
                            if let Ok(ev) = serde_json::from_str::<Value>(s) {
                                self.feed_acp(&ev, &mut out);
                            }
                        } else if let Some(ev) = data {
                            self.feed_acp(ev, &mut out);
                        }
                    }
                    // Agent 提问：调用方要回 reply-question，这里不产生内容
                    "acp_ask_user_question" => {}
                    _ => {}
                }
            }
            _ => {}
        }
        out
    }

    fn feed_acp(&mut self, ev: &Value, out: &mut Vec<Value>) {
        let kind = ev
            .get("sessionUpdate")
            .or_else(|| ev.get("type"))
            .or_else(|| ev.get("kind"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        match kind {
            "agent_message_chunk" => {
                let t = ev
                    .get("content")
                    .and_then(|c| c.get("text").or_else(|| Some(c)))
                    .and_then(|c| c.as_str())
                    .or_else(|| ev.get("text").and_then(|t| t.as_str()))
                    .unwrap_or("");
                if !t.is_empty() {
                    self.content.push_str(t);
                    out.push(json!({"content": t}));
                }
            }
            "agent_thought_chunk" => {
                let t = ev
                    .get("content")
                    .and_then(|c| c.get("text").or_else(|| Some(c)))
                    .and_then(|c| c.as_str())
                    .or_else(|| ev.get("text").and_then(|t| t.as_str()))
                    .unwrap_or("");
                if !t.is_empty() {
                    self.reasoning.push_str(t);
                    out.push(json!({"reasoning_content": t}));
                }
            }
            "tool_call" => {
                let call = json!({
                    "index": self.tool_calls.len(),
                    "id": ev.get("toolCallId").or_else(|| ev.get("id")).and_then(|x| x.as_str()).unwrap_or(""),
                    "type": "function",
                    "function": {
                        "name": ev.get("title").or_else(|| ev.get("name")).and_then(|x| x.as_str()).unwrap_or(""),
                        "arguments": "{}",
                    }
                });
                self.tool_calls.push(call.clone());
                out.push(json!({"tool_calls": [call]}));
            }
            "tool_call_update" => {
                // 参数增量：路径在 rawInput / input 上
                let args = ev
                    .get("rawInput")
                    .or_else(|| ev.get("input"))
                    .map(|a| {
                        if a.is_string() {
                            a.as_str().unwrap_or("{}").to_string()
                        } else {
                            a.to_string()
                        }
                    });
                if let Some(a) = args {
                    let idx = self.tool_calls.len().saturating_sub(1);
                    out.push(json!({"tool_calls": [{
                        "index": idx,
                        "function": {"arguments": a}
                    }]}));
                }
            }
            "usage_update" => {
                if let Some(u) = ev.get("usage").or_else(|| Some(ev)) {
                    self.read_usage(u);
                }
            }
            _ => {}
        }
    }

    fn read_usage(&mut self, u: &Value) {
        for k in ["inputTokens", "input_tokens", "prompt_tokens", "promptTokens"] {
            if let Some(n) = u.get(k).and_then(|x| x.as_i64()) {
                if n > 0 {
                    self.prompt_tokens = n;
                }
            }
        }
        for k in ["outputTokens", "output_tokens", "completion_tokens", "completionTokens"] {
            if let Some(n) = u.get(k).and_then(|x| x.as_i64()) {
                if n > 0 {
                    self.completion_tokens = n;
                }
            }
        }
    }
}

/// user-input 载荷：content 是 **base64 编码**的 prompt（上游这么要的）。
fn user_input_payload(prompt: &str) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(prompt.as_bytes());
    json!({"content": encoded, "attachments": []}).to_string()
}

/// 自动回复 Agent 提问（上游要求这个形状才会继续）。
fn reply_question_payload() -> String {
    json!({"type": "reply-question", "data": {"answers_json": "", "cancelled": false}}).to_string()
}

/// 从模型目录条目反查建任务需要的字段（model_id = UUID、cli_name）。
pub fn task_target(models: &[Value], requested: &str) -> Option<(String, String)> {
    models
        .iter()
        .find(|m| {
            m.get("id")
                .and_then(|x| x.as_str())
                .map(|id| id.eq_ignore_ascii_case(requested))
                .unwrap_or(false)
        })
        .map(|m| {
            let uuid = m.get("upstreamId").and_then(|x| x.as_str()).unwrap_or("");
            let cli = m
                .get("cliName")
                .and_then(|x| x.as_str())
                .unwrap_or("opencode")
                .to_string();
            (uuid.to_string(), cli)
        })
}

/// 建任务（POST /api/v1/users/tasks），返回 task id。
async fn create_task(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    prompt: &str,
    system_prompt: Option<&str>,
    model_id: &str,
    cli_name: &str,
) -> Result<String, RelayError> {
    let url = format!("{}/api/v1/users/tasks", spec.base_url.trim_end_matches('/'));
    let body = json!({
        "content": prompt,
        "system_prompt": system_prompt.unwrap_or(""),
        "host_id": "public_host",
        "image_id": "",
        "model_id": model_id,
        "cli_name": cli_name,
        "resource": {"core": 1, "memory": 1073741824, "life": 3600},
        "repo": {"repo_url": "", "branch": "master", "repo_filename": "", "zip_url": ""},
    });
    let rb = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Cookie", cookie_header(secret))
        .header("User-Agent", ORCA_UA)
        .body(body.to_string());
    let resp = rb
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if status == 401 || status == 403 {
        return Err(er(
            401,
            "MonkeyCode 登录态已失效：请重新粘贴 session cookie".to_string(),
            ErrorClass::Auth,
        ));
    }
    if !(200..300).contains(&status) {
        return Err(er(
            status,
            format!("MonkeyCode 建任务返回 HTTP {status}：{}", truncate(&text, 240)),
            ErrorClass::Transient,
        ));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| er(502, format!("建任务响应不是 JSON：{e}"), ErrorClass::Transient))?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if code != 0 {
        let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("未知错误");
        return Err(er(
            502,
            format!("MonkeyCode 建任务失败（code={code}：{msg}）"),
            ErrorClass::Transient,
        ));
    }
    v.get("data")
        .and_then(|d| d.get("id").or_else(|| d.get("task_id")))
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| er(502, "建任务没返回 task id".to_string(), ErrorClass::Transient))
}

const ORCA_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
use aigw_registry::ProviderSpec;

/// 把用户粘贴的凭据整形成 Cookie 头的值。
///
/// ★ 三种输入都要能活：面板 cookie 分支的通用提示是「粘贴整段 Cookie」，
///   用户照做时 secret 是一整串 "a=1; monkeycode_ai_session=xxx; b=2"。
///   旧实现直接拼 monkeycode_ai_session={secret}，整段粘贴会变成
///   "monkeycode_ai_session=a=1; monkeycode_ai_session=xxx; …" —— 必然 401，
///   而且报错只会说「未授权」，用户不知道是粘贴形态的问题。
///   · 含 = 且能提出目标键 → 只用提出来的值；
///   · 含 = 但没有目标键 → 原样整段发出去（交给上游判，不自作聪明）；
///   · 不含 =（裸值）→ 拼成 monkeycode_ai_session=<值>。
pub fn cookie_header(secret: &str) -> String {
    let s = secret.trim();
    if !s.contains('=') {
        return format!("monkeycode_ai_session={s}");
    }
    for part in s.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            if k.trim().eq_ignore_ascii_case("monkeycode_ai_session") {
                let v = v.trim().trim_matches('"');
                if !v.is_empty() {
                    return format!("monkeycode_ai_session={v}");
                }
            }
        }
    }
    s.to_string()
}

/// 拉一次模型目录（live 清单）。上游回 {"code":0,"data":{"models":[…]}}。
pub async fn fetch_models(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
) -> Result<Vec<Value>, String> {
    let url = format!("{}/api/v1/users/models", spec.base_url.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .header("Cookie", cookie_header(secret))
        .header("User-Agent", ORCA_UA)
        .send()
        .await
        .map_err(|e| format!("network: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("bad json: {e}"))?;
    let arr = v
        .get("data")
        .and_then(|d| d.get("models"))
        .or_else(|| v.get("models"))
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    // 归一：下游 id 用 {provider}/{model}，并保留 UUID 与 cliName 供建任务用
    Ok(arr
        .into_iter()
        .filter_map(|m| {
            let uuid = m.get("id").and_then(|x| x.as_str())?;
            let provider = m.get("provider").and_then(|x| x.as_str()).unwrap_or("");
            let model = m.get("model").and_then(|x| x.as_str())?;
            let id = if provider.is_empty() {
                model.to_string()
            } else {
                format!("{provider}/{model}")
            };
            let itype = m.get("interface_type").and_then(|x| x.as_str()).unwrap_or("");
            Some(json!({
                "id": id,
                "upstreamId": uuid,
                "cliName": cli_name_for(itype),
                "name": m.get("display_name").and_then(|x| x.as_str()).unwrap_or(model),
            }))
        })
        .collect())
}

/// 验活：拉一次模型目录（只读、不建任务、不消耗额度）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    fetch_models(http, spec, secret).await.is_ok()
}

/// 挂上任务流 WebSocket，跑完一轮，把 ACP 事件翻成 chat delta。
///
/// ★ 四个出口都必须返回值（见模块头）：task-ended / task-error / 连接断 /
///   空闲超时。**不能"什么都不做地结束"** —— 客户端的 SSE 解析器等的是
///   `[DONE]`，只断连接会被渲染成"回答被截断"。
async fn run_task_stream(
    spec: &ProviderSpec,
    secret: &str,
    task_id: &str,
    prompt: &str,
    collect: Option<tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>>,
) -> Result<AcpAcc, RelayError> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let ws_base = spec
        .base_url
        .trim_end_matches('/')
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let url = format!("{ws_base}/api/v1/users/tasks/stream?id={task_id}&mode=new");
    let req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        url.as_str(),
    )
    .map_err(|e| er(502, format!("WS 请求构造失败：{e}"), ErrorClass::Transient))?;
    let mut req = req;
    req.headers_mut().insert(
        "Cookie",
        cookie_header(secret)
            .parse()
            .map_err(|_| er(502, "Cookie 头非法".to_string(), ErrorClass::Rejected))?,
    );
    req.headers_mut().insert(
        "User-Agent",
        ORCA_UA
            .parse()
            .map_err(|_| er(502, "UA 头非法".to_string(), ErrorClass::Rejected))?,
    );

    let (mut ws, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| er(502, format!("MonkeyCode 任务流连接失败：{e}"), ErrorClass::Transient))?;

    // ★ 连接建立后**必须先发**这两条，上游才开始跑。
    //   不发 auto-approve，任务会停在第一个工具调用等用户确认 —— 永远不动。
    ws.send(Message::text(json!({"type": "auto-approve"}).to_string()))
        .await
        .map_err(|e| er(502, format!("发送 auto-approve 失败：{e}"), ErrorClass::Transient))?;
    ws.send(Message::text(
        json!({"type": "user-input", "data": user_input_payload(prompt)}).to_string(),
    ))
    .await
    .map_err(|e| er(502, format!("发送 user-input 失败：{e}"), ErrorClass::Transient))?;

    let mut acc = AcpAcc::default();
    let idle = std::time::Duration::from_secs(IDLE_TIMEOUT_SECS);
    // 总时长上限的起点：从开始收流算起（建任务与挂 WS 的时间不计入 agent 的实际工作）。
    let started = std::time::Instant::now();

    loop {
        // 出口④b：总时长上限。
        // ★ 必须放在每次循环的最前面，且**不能**被 ping 的 continue 绕过 ——
        //   上游每 10 秒一次 ping 会让空闲超时永远不触发（见 MAX_TOTAL_SECS 注释）。
        if total_budget_exceeded(started.elapsed().as_secs()) {
            acc.error = Some(format!(
                "上游任务超过 {} 秒仍未结束（已按超时中止，避免连接无限占用）",
                MAX_TOTAL_SECS
            ));
            acc.done = true;
            break;
        }
        let next = tokio::time::timeout(idle, ws.next()).await;
        let msg = match next {
            Err(_) => {
                // 出口④：空闲超时
                acc.error = Some(format!(
                    "上游 {IDLE_TIMEOUT_SECS} 秒没有任何消息（任务可能卡住或断线）"
                ));
                acc.done = true;
                break;
            }
            Ok(None) => {
                // 出口③：连接被关
                if !acc.done {
                    acc.error = Some("上游任务流意外关闭（没等到 task-ended）".to_string());
                    acc.done = true;
                }
                break;
            }
            Ok(Some(Err(e))) => {
                acc.error = Some(format!("任务流传输错误：{e}"));
                acc.done = true;
                break;
            }
            Ok(Some(Ok(m))) => m,
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => {
                if !acc.done {
                    acc.error = Some("上游主动关闭了任务流".to_string());
                    acc.done = true;
                }
                break;
            }
            _ => continue,
        };

        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let ty = frame.get("type").and_then(|x| x.as_str()).unwrap_or("");

        // 心跳要回一条，否则上游会断开
        if ty == "ping" {
            let _ = ws.send(Message::text(json!({"type": "ping"}).to_string())).await;
            continue;
        }
        // Agent 提问要自动回，否则任务停住
        if ty == "task-running"
            && frame.get("kind").and_then(|k| k.as_str()) == Some("acp_ask_user_question")
        {
            let _ = ws
                .send(Message::text(reply_question_payload()))
                .await;
            continue;
        }

        for delta in acc.feed(&frame) {
            if let Some(tx) = collect.as_ref() {
                let f = format!(
                    "data: {}\n\n",
                    json!({
                        "id": "chatcmpl-mc", "object": "chat.completion.chunk", "created": secs(),
                        "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}]
                    })
                );
                if tx.send(Ok(bytes::Bytes::from(f))).await.is_err() {
                    return Ok(acc); // 客户端断开
                }
            }
        }
        if acc.done {
            break;
        }
    }

    let _ = ws.close(None).await;
    Ok(acc)
}

/// 非流式：建任务 → 跑完 WS → 聚合成 chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let (prompt, system_prompt) = build_prompt(&v);

    // 建任务需要 model_id(UUID) 与 cli_name —— 从目录反查
    let models = fetch_models(http, spec, secret)
        .await
        .map_err(|e| er(502, format!("拉取模型目录失败：{e}"), ErrorClass::Transient))?;
    let Some((uuid, cli)) = task_target(&models, &model) else {
        return Err(er(
            404,
            format!("MonkeyCode 没有叫 {model} 的模型（用 /v1/models?platform={} 查看）", spec.id),
            ErrorClass::Rejected,
        ));
    };

    let task_id = create_task(
        http,
        spec,
        secret,
        &prompt,
        system_prompt.as_deref(),
        &uuid,
        &cli,
    )
    .await?;
    let acc = run_task_stream(spec, secret, &task_id, &prompt, None).await?;
    if let Some(err) = acc.error.clone() {
        return Err(er(502, format!("MonkeyCode 任务失败：{err}"), ErrorClass::Transient));
    }
    let tin = if acc.prompt_tokens > 0 { acc.prompt_tokens } else { (body.len() / 4).max(1) as i64 };
    let tout = if acc.completion_tokens > 0 {
        acc.completion_tokens
    } else {
        (acc.content.chars().count() as i64 / 2).max(1)
    };
    let mut msg = json!({"role": "assistant", "content": acc.content});
    if !acc.reasoning.is_empty() {
        msg["reasoning_content"] = json!(acc.reasoning);
    }
    if !acc.tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(acc.tool_calls.clone());
    }
    Ok(json!({
        "id": format!("chatcmpl-mc-{}", nanos_hex()),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": msg,
            "finish_reason": if acc.tool_calls.is_empty() { "stop" } else { "tool_calls" }
        }],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout},
    }))
}

/// 流式：建任务 → WS 逐帧下发（每收到一个 delta 就吐一帧）。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<(
    futures_util::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
    tokio::sync::mpsc::Receiver<crate::StreamStat>,
), RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let (prompt, system_prompt) = build_prompt(&v);

    let models = fetch_models(http, spec, secret)
        .await
        .map_err(|e| er(502, format!("拉取模型目录失败：{e}"), ErrorClass::Transient))?;
    let Some((uuid, cli)) = task_target(&models, &model) else {
        return Err(er(
            404,
            format!("MonkeyCode 没有叫 {model} 的模型（用 /v1/models?platform={} 查看）", spec.id),
            ErrorClass::Rejected,
        ));
    };
    let task_id = create_task(
        http,
        spec,
        secret,
        &prompt,
        system_prompt.as_deref(),
        &uuid,
        &cli,
    )
    .await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(8);
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
    let spec2 = spec.clone();
    let secret2 = secret.to_string();
    let body_len = body.len();

    tokio::spawn(async move {
        let acc = match run_task_stream(&spec2, &secret2, &task_id, &prompt, Some(out_tx.clone())).await {
            Ok(a) => a,
            Err(e) => {
                let _ = out_tx
                    .send(Ok(bytes::Bytes::from(format!(
                        "data: {}\n\n",
                        json!({"error": {"code": 502, "message": e.body, "type": "upstream_error"}})
                    ))))
                    .await;
                let _ = out_tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
                return;
            }
        };
        if let Some(err) = acc.error.clone() {
            let _ = out_tx
                .send(Ok(bytes::Bytes::from(format!(
                    "data: {}\n\n",
                    json!({"error": {"code": 502, "message": err, "type": "upstream_error"}})
                ))))
                .await;
        } else {
            let tin = if acc.prompt_tokens > 0 { acc.prompt_tokens } else { (body_len / 4).max(1) as i64 };
            let tout = if acc.completion_tokens > 0 {
                acc.completion_tokens
            } else {
                (acc.content.chars().count() as i64 / 2).max(1)
            };
            let fin = json!({
                "id": "chatcmpl-mc", "object": "chat.completion.chunk", "created": secs(),
                "choices": [{"index": 0, "delta": {}, "finish_reason": if acc.tool_calls.is_empty() { "stop" } else { "tool_calls" }}]
            });
            let _ = out_tx.send(Ok(bytes::Bytes::from(format!("data: {}\n\n", fin)))).await;
            let usg = json!({
                "id": "chatcmpl-mc", "object": "chat.completion.chunk", "created": secs(),
                "choices": [],
                "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout}
            });
            let _ = out_tx.send(Ok(bytes::Bytes::from(format!("data: {}\n\n", usg)))).await;
            let _ = tx
                .send(crate::StreamStat {
                    tokens_in: tin,
                    tokens_out: tout,
                    saw_done: true,
                    ..Default::default()
                })
                .await;
        }
        let _ = out_tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
    });

    let s = tokio_stream::wrappers::ReceiverStream::new(out_rx);
    Ok((Box::pin(s), rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 第一条非空 system 抽出来单独走 system_prompt，其余按角色加前缀。
    #[test]
    fn build_prompt_splits_system() {
        let v = json!({"messages": [
            {"role": "system", "content": "你是助手"},
            {"role": "user", "content": "问题"},
            {"role": "assistant", "content": "回答"},
        ]});
        let (p, sys) = build_prompt(&v);
        assert_eq!(sys.as_deref(), Some("你是助手"), "system 要单独抽走");
        assert!(p.contains("[User]\n问题"), "p={p}");
        assert!(p.contains("[Assistant]\n回答"));
        assert!(!p.contains("你是助手"), "system 不能留在 prompt 里");
    }

    /// 空的 system 不算（只取第一条**非空**的）。
    #[test]
    fn build_prompt_only_first_non_empty_system() {
        let v = json!({"messages": [
            {"role": "system", "content": "  "},
            {"role": "system", "content": "真正的 system"},
            {"role": "user", "content": "hi"},
        ]});
        let (_p, sys) = build_prompt(&v);
        assert_eq!(sys.as_deref(), Some("真正的 system"));
    }

    /// 空消息要有兜底。
    #[test]
    fn build_prompt_fallback() {
        let (p, sys) = build_prompt(&json!({}));
        assert!(p.contains("[User]"), "p={p}");
        assert!(sys.is_none());
    }

    /// 多模态 content 数组要挑出文本（不能变成 [object Object]）。
    #[test]
    fn build_prompt_extracts_text_parts() {
        let v = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "甲"}, {"type": "input_text", "text": "乙"}]}
        ]});
        let (p, _) = build_prompt(&v);
        assert!(p.contains("甲乙"), "p={p}");
    }

    /// interface_type → CLI 名。
    #[test]
    fn cli_name_mapping() {
        assert_eq!(cli_name_for("anthropic"), "claude");
        assert_eq!(cli_name_for("openai_responses"), "codex");
        assert_eq!(cli_name_for("openai_chat"), "opencode");
        assert_eq!(cli_name_for(""), "opencode");
    }

    /// ACP agent_message_chunk → content 增量。
    #[test]
    fn acp_message_chunk_to_content() {
        let mut acc = AcpAcc::default();
        let f = json!({"type": "task-running", "kind": "acp_event",
            "data": "{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"text\":\"你\"}}"});
        let out = acc.feed(&f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["content"], json!("你"));
        assert_eq!(acc.content, "你");
    }

    /// ★ data 是 **JSON 字符串**（不是对象）—— 忘了二次解析就什么都收不到。
    #[test]
    fn acp_event_data_must_be_parsed_twice() {
        let mut acc = AcpAcc::default();
        // 直接给对象（兼容路径）
        let f = json!({"type": "task-running", "kind": "acp_event",
            "data": {"sessionUpdate": "agent_message_chunk", "content": {"text": "A"}}});
        assert_eq!(acc.feed(&f).len(), 1);
        // 给字符串（真实形态）
        let f2 = json!({"type": "task-running", "kind": "acp_event",
            "data": "{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"text\":\"B\"}}"});
        assert_eq!(acc.feed(&f2).len(), 1);
        assert_eq!(acc.content, "AB");
    }

    /// 思考块走 reasoning_content 通道（不是塞进正文）。
    #[test]
    fn acp_thought_chunk_to_reasoning() {
        let mut acc = AcpAcc::default();
        let f = json!({"type": "task-running", "kind": "acp_event",
            "data": "{\"sessionUpdate\":\"agent_thought_chunk\",\"content\":{\"text\":\"想\"}}"});
        let out = acc.feed(&f);
        assert_eq!(out[0]["reasoning_content"], json!("想"));
        assert!(out[0].get("content").is_none(), "思考不能进正文");
        assert_eq!(acc.content, "");
    }

    /// task-ended 是终态。
    #[test]
    fn task_ended_is_terminal() {
        let mut acc = AcpAcc::default();
        acc.feed(&json!({"type": "task-ended"}));
        assert!(acc.done);
        assert!(acc.error.is_none());
    }

    /// task-error 是终态且带错误。
    #[test]
    fn task_error_is_terminal_with_message() {
        let mut acc = AcpAcc::default();
        acc.feed(&json!({"type": "task-error", "data": {"message": "VM 挂了"}}));
        assert!(acc.done);
        assert_eq!(acc.error.as_deref(), Some("VM 挂了"));
    }

    /// 总时长上限：到点必须中止。
    ///
    /// 这条约束的来历：上游每 10 秒发 ping，而收流循环里 ping 走 `continue`
    /// 会重置空闲计时器 —— 于是任务卡死时（ping 照来、agent 不产出）
    /// 空闲超时**永不触发**，连接一直挂着。模块头承诺的「或总时长超限」
    /// 在补上 MAX_TOTAL_SECS 之前并未实现。
    #[test]
    fn total_budget_has_a_hard_ceiling() {
        assert!(!total_budget_exceeded(0), "刚开始不该超时");
        assert!(!total_budget_exceeded(MAX_TOTAL_SECS - 1), "差一秒不该超时");
        assert!(total_budget_exceeded(MAX_TOTAL_SECS), "到点必须中止");
        assert!(total_budget_exceeded(MAX_TOTAL_SECS + 3600), "超过后仍然要中止");
        // 上限必须比空闲超时大，否则「有空闲但没超总时长」的任务会被误杀
        const _: () = assert!(MAX_TOTAL_SECS > IDLE_TIMEOUT_SECS);
    }

    /// usage_update 按累计值读，非零才覆盖。
    #[test]
    fn usage_update_accumulates() {
        let mut acc = AcpAcc::default();
        acc.feed(&json!({"type": "task-running", "kind": "acp_event",
            "data": "{\"sessionUpdate\":\"usage_update\",\"usage\":{\"inputTokens\":10,\"outputTokens\":5}}"}));
        assert_eq!((acc.prompt_tokens, acc.completion_tokens), (10, 5));
        // 0 不该覆盖已有值
        acc.feed(&json!({"type": "task-running", "kind": "acp_event",
            "data": "{\"sessionUpdate\":\"usage_update\",\"usage\":{\"inputTokens\":0,\"outputTokens\":0}}"}));
        assert_eq!((acc.prompt_tokens, acc.completion_tokens), (10, 5));
    }

    /// 心跳与未知帧不产生内容。
    #[test]
    fn ping_and_unknown_produce_nothing() {
        let mut acc = AcpAcc::default();
        assert!(acc.feed(&json!({"type": "ping"})).is_empty());
        assert!(acc.feed(&json!({"type": "task-started"})).is_empty());
        assert!(acc.feed(&json!({"type": "不存在的类型"})).is_empty());
        assert!(!acc.done);
    }

    /// user-input 的 content 必须 base64 编码。
    #[test]
    fn user_input_payload_is_base64() {
        use base64::Engine;
        let p = user_input_payload("你好");
        let v: Value = serde_json::from_str(&p).unwrap();
        let enc = v["content"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(enc).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "你好");
        assert_eq!(v["attachments"], json!([]));
    }

    /// reply-question 形状固定（上游要这个形状才继续）。
    #[test]
    fn reply_question_shape() {
        let v: Value = serde_json::from_str(&reply_question_payload()).unwrap();
        assert_eq!(v["type"], json!("reply-question"));
        assert_eq!(v["data"]["answers_json"], json!(""));
        assert_eq!(v["data"]["cancelled"], json!(false));
    }

    /// 模型目录归一：下游 id 用 {provider}/{model}，同时保留 UUID 与 cliName。
    #[test]
    fn task_target_lookup() {
        let models = vec![json!({
            "id": "openai/gpt-4o",
            "upstreamId": "uuid-123",
            "cliName": "opencode",
        })];
        let t = task_target(&models, "openai/gpt-4o");
        assert_eq!(t, Some(("uuid-123".to_string(), "opencode".to_string())));
        assert_eq!(task_target(&models, "不存在"), None);
    }

    /// 三种粘贴形态都要能整形成正确的 Cookie 头。
    #[test]
    fn cookie_header_accepts_all_paste_shapes() {
        // 裸值
        assert_eq!(cookie_header("abc123"), "monkeycode_ai_session=abc123");
        // 整段 Cookie：提取目标键
        assert_eq!(
            cookie_header("x=1; monkeycode_ai_session=SESS; y=2"),
            "monkeycode_ai_session=SESS"
        );
        // 键名大小写不敏感
        assert_eq!(
            cookie_header("MonkeyCode_AI_Session=S2"),
            "monkeycode_ai_session=S2"
        );
        // 含 = 但没有目标键：原样发出（交给上游判）
        assert_eq!(cookie_header("other=1; foo=bar"), "other=1; foo=bar");
        // 前后空白
        assert_eq!(cookie_header("  SESS  "), "monkeycode_ai_session=SESS");
    }

}
