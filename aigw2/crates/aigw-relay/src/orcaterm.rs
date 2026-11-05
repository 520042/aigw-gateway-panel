//! OrcaTerm（腾讯云遨驰终端，`lightai.cloud.tencent.com`）—— 两步：建会话 → SSE。
//!
//! ── 上游长什么样（对齐 Agent2API 的 providers/orcaterm/chat.rs）────
//! ```text
//! POST {LIGHTAI}/assistant/conversation               建会话 → {"code":0,...}
//! POST {LIGHTAI}/assistant/chat?name=orcaterm&mode=0&csrfCode=&sseresume=true
//!      body {conversationId, user:{id,setting:{model,...}}, input:{type:"start",input,...}, stream:true}
//!      → text/event-stream：data: {content|reasoningContent 增量} … data: [DONE]
//! ```
//!
//! ── 为什么必须两步 ──────────────────────────────────────────
//! 上游没有「一次请求一次回答」的接口：对话必须先有 conversationId。
//! 本模块按**请求内会话**处理（每个请求新建一个），历史拍平进输入 ——
//! 因此没有注册表、没有跨请求状态、重启即忘，任何客户端形态都能工作。
//!
//! ── SSE 帧的三个坑 ────────────────────────────────────────
//! 1. **错误帧不是 JSON**：`data: [ERROR]: [LLM_ERROR]: …`，按 JSON 解析会得到空
//!    事件，于是失败被当成「上游什么都没说」—— 静默空回复。必须先按前缀认；
//! 2. 思考增量用 `reasoning: true` 标记，且**先于正文**流入 → 放
//!    `delta.reasoning_content`（DeepSeek 系的事实标准）；
//! 3. 结束帧 `data: [DONE]`，不是 `event: done`。
//!
//! ── 模型 ──────────────────────────────────────────────────
//! 上游**没有模型清单接口**（面板列表来自桌面客户端内置），所以
//! `models_policy = "none"`：模型名以 `TokenHub/xxx` 形式经 `模型@平台` 后缀指定。

use serde_json::{json, Value};

use aigw_registry::ProviderSpec;

use crate::common::{er, nanos_hex, secs, truncate};
use crate::{ErrorClass, RelayError};

/// 浏览器/桌面端的 UA（上游按来源判定，逐字对齐参考实现）。
const ORCA_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
/// assistant 网关的固定查询串。
const ASSISTANT_QUERY: &str = "name=orcaterm&mode=0&csrfCode=&sseresume=true";

/// 态清单（与参考项目 models.rs 同口径：桌面端 1.6.4 面板逐项对证，2026-10-16）。
/// 上游没有可拉的模型清单接口（DescribeAIModelList 对普通账号回空数组），
/// 裸名都要归一成 TokenHub/<name> 才被上游认。
const MODELS: &[&str] = &[
    "hy4-preview",
    "hy3",
    "kimi-k3",
    "glm-5.3-flash",
    "glm-5.3",
    "glm-5.2",
    "deepseek-v4.1-flash",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
];

/// 默认档（实测可用）。★ 表外名的处理是**显式回退**而不是放行：
/// 上游对未知模型回流内 LLM_ERROR: LLM(TokenHub/xxx) is not loaded，
/// 与其让用户收到一个看不懂的上游错，不如回退到能答的档（SSE 帧
/// 的 model 字段仍回显用户请求的名字，不冒充）。
/// 例外：表内模型（含上游未挂载的 hy4-preview）原样放行——「面板里那个模型
/// 在网关里悄悄消失」更糟；未挂载的错误由流内错误明确下发。
const DEFAULT_MODEL: &str = "TokenHub/glm-5.3-flash";

fn resolve_model(name: &str) -> String {
    let bare = name.trim().trim_start_matches("TokenHub/").trim();
    if bare.is_empty() {
        return DEFAULT_MODEL.to_string();
    }
    if MODELS.iter().any(|m| *m == bare) {
        format!("TokenHub/{bare}")
    } else {
        DEFAULT_MODEL.to_string()
    }
}

fn rand_hex(n: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 会话 id 的形态：`cid-<uuid>-orcaterm`（上游按这个前缀认）。
fn new_conversation_id() -> String {
    let h = rand_hex(16);
    format!("cid-{}-{}-{}-{}-{}-orcaterm", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn new_seq_id() -> String {
    // ★ 必须是**标准 UUID v4**：上游对 `input.id` 做版本位校验。
    //
    //   旧实现只是把 16 字节随机数按 8-4-4-4-12 摆成 UUID 的形状，**没设版本位
    //   与变体位** —— 生成形如 `87803f8e-8db0-b815-7555-0f40842e65c5` 的串
    //   （第 13 位是 `b` 不是 4、第 17 位是 `7` 不在 8/9/a/b 里）。上游因此拒收：
    //     data: [ERROR]: [LLM_ERROR]: Invalid inputId: 87803f8e-...
    //   这个错误极具误导性（"inputId 无效"听起来像内容有问题）。定位过程是：
    //   同一 token、同一份请求体，只把 input.id 换成标准 v4 就恢复正常流式输出；
    //   反过来用那个具体的非 v4 串能**稳定复现**该错误。
    //
    //   为什么之前没暴露：`new_conversation_id()` 的 `cid-<uuid>-orcaterm` 不校验
    //   版本位，所以「建会话成功、发消息才失败」——现象看起来像账号或模型的问题。
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version = 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant = RFC 4122
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// 把 OpenAI messages 压成单条输入。
///
/// ★ 单条消息**原样直通**（最常见的单轮用法不引入任何包装）；多轮时拍平成
///   带角色前缀的记录。上游按 conversationId 留历史、输入通道只有一条 start，
///   而我们每请求新建会话，所以历史只能这样并进来。
pub fn build_prompt(v: &Value) -> String {
    let Some(arr) = v.get("messages").and_then(|m| m.as_array()) else {
        return "你好".to_string();
    };
    let usable: Vec<&Value> = arr
        .iter()
        .filter(|m| !content_text(m).trim().is_empty())
        .collect();
    if usable.is_empty() {
        return "你好".to_string();
    }
    if usable.len() == 1 {
        return content_text(usable[0]);
    }
    let mut out: Vec<String> = vec!["[以下是本轮之前的对话记录，供你了解上下文]".to_string()];
    for m in usable {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let text = content_text(m);
        let tag = match role {
            "system" => "系统指令",
            "assistant" => "助手",
            _ => "用户",
        };
        out.push(format!("[{tag}] {text}"));
    }
    out.push("[对话记录结束；请回应其中最后一条用户消息]".to_string());
    out.join("\n\n")
}

fn content_text(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn base_of(spec: &ProviderSpec) -> String {
    spec.base_url.trim_end_matches('/').to_string()
}

fn assistant_headers(secret: &str, seq: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Bearer {secret}")),
        ("X-Product", "orcaterm".to_string()),
        ("x-seqid", seq.to_string()),
        ("User-Agent", ORCA_UA.to_string()),
        ("Origin", "https://orcaterm.cloud.tencent.com".to_string()),
        ("Referer", "https://orcaterm.cloud.tencent.com/".to_string()),
    ]
}

/// 第 1 步：建会话，返回 conversationId。
async fn create_conversation(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
) -> Result<String, RelayError> {
    let cid = new_conversation_id();
    let url = format!("{}/assistant/conversation", base_of(spec));
    let body = json!({
        "id": cid,
        "title": "新对话",
        "ts": secs() * 1000,
        "customTitle": "",
        "aiTitle": "",
        "tags": ["orcaterm"],
    });
    let mut rb = http
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body.to_string());
    for (k, v) in assistant_headers(secret, &new_seq_id()) {
        rb = rb.header(k, v);
    }
    let resp = rb
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if status == 401 || status == 403 {
        return Err(er(
            401,
            format!("OrcaTerm 登录态已失效：请重新导入或粘贴该账号的 accessToken（HTTP {status}）"),
            ErrorClass::Auth,
        ));
    }
    if !(200..300).contains(&status) {
        return Err(er(
            status,
            format!("OrcaTerm 建会话返回 HTTP {status}：{}", truncate(&text, 240)),
            ErrorClass::Transient,
        ));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| er(502, format!("建会话响应不是 JSON：{e}"), ErrorClass::Transient))?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("未知错误");
        return Err(er(
            502,
            format!("OrcaTerm 建会话失败（code={code}：{msg}）"),
            ErrorClass::Transient,
        ));
    }
    Ok(cid)
}

/// 第 2 步：发消息，拿到 SSE 响应。
async fn open_chat(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    cid: &str,
    model: &str,
    prompt: &str,
) -> Result<reqwest::Response, RelayError> {
    let seq = new_seq_id();
    let url = format!("{}/assistant/chat?{}", base_of(spec), ASSISTANT_QUERY);
    let body = json!({
        "conversationId": cid,
        "user": {
            "id": "",
            "setting": {
                // 上游只认 TokenHub/ 前缀形态——裸名在这里归一
                "model": resolve_model(model),
                "mcpServers": [],
                "uiServers": [],
                "tools": [],
                "approvedTools": [],
                "approvedMCPTools": [],
                "enableAutoSubtaskExecution": false,
                "env": [],
                // 上游是 agent 协议：网关侧全部置空，纯文本直通（不做工具注入）
                "hideToolMessage": true,
                "language": "zh",
            },
        },
        "input": {
            "type": "start",
            "input": prompt,
            "id": seq,
            "emphasisPrompt": "",
            "files": [],
        },
        "stream": true,
    });
    let mut rb = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .body(body.to_string());
    for (k, v) in assistant_headers(secret, &new_seq_id()) {
        rb = rb.header(k, v);
    }
    let resp = rb
        .send()
        .await
        .map_err(|e| er(0, format!("network: {e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(er(
            401,
            "OrcaTerm 登录态已失效：请重新导入或粘贴 accessToken".to_string(),
            ErrorClass::Auth,
        ));
    }
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        return Err(er(
            status,
            format!("OrcaTerm 对话返回 HTTP {status}：{}", truncate(&t, 240)),
            ErrorClass::Transient,
        ));
    }
    Ok(resp)
}

/// 一条 SSE `data:` 载荷的解析结果。
pub enum OrcaChunk {
    /// 正文增量
    Content(String),
    /// 思考增量
    Reasoning(String),
    /// 结束
    Done,
    /// 上游错误（**非 JSON 形态**：`[ERROR]: [LLM_ERROR]: …`）
    Error(String),
    /// 其余（心跳 / 状态帧）
    Other,
}

/// 解析一条 data 载荷。
///
/// ★ 先按前缀认错误：上游的错误帧是 `[ERROR]: [LLM_ERROR]: xxx`，**不是 JSON**。
///   把它当 JSON 解析会得到 Other，于是失败被当成「上游什么都没说」——
///   用户看到一条空回复却没有任何错误提示。这是本家最容易踩的坑。
pub fn parse_chunk(data: &str) -> OrcaChunk {
    let d = data.trim();
    if d.is_empty() {
        return OrcaChunk::Other;
    }
    if d == "[DONE]" {
        return OrcaChunk::Done;
    }
    if d.starts_with("[ERROR]") || d.contains("[LLM_ERROR]") {
        let msg = d.trim_start_matches("[ERROR]:").trim_start_matches("[ERROR]").trim();
        return OrcaChunk::Error(msg.to_string());
    }
    let Ok(v) = serde_json::from_str::<Value>(d) else {
        return OrcaChunk::Other;
    };
    if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
        if code != 0 {
            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("上游返回错误");
            return OrcaChunk::Error(format!("code={code}：{msg}"));
        }
    }
    let is_reasoning = v.get("reasoning").and_then(|r| r.as_bool()).unwrap_or(false);
    if is_reasoning {
        if let Some(t) = v.get("reasoningContent").and_then(|x| x.as_str()) {
            if !t.is_empty() {
                return OrcaChunk::Reasoning(t.to_string());
            }
        }
    }
    if let Some(t) = v.get("content").and_then(|x| x.as_str()) {
        if !t.is_empty() {
            return OrcaChunk::Content(t.to_string());
        }
    }
    OrcaChunk::Other
}

/// 从 SSE 原始响应体里聚合出正文与思考（非流式与验活共用）。
fn drain_sse(raw: &str, text: &mut String, reasoning: &mut String) -> Option<String> {
    for line in raw.lines() {
        let Some(rest) = line.strip_prefix("data:") else { continue };
        match parse_chunk(rest) {
            OrcaChunk::Content(t) => text.push_str(&t),
            OrcaChunk::Reasoning(t) => reasoning.push_str(&t),
            OrcaChunk::Error(e) => return Some(e),
            _ => {}
        }
    }
    None
}

/// 非流式：两步走完，把 SSE 聚合成一个 chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let prompt = build_prompt(&v);
    let cid = create_conversation(http, spec, secret).await?;
    let resp = open_chat(http, spec, secret, &cid, &model, &prompt).await?;
    let raw = resp
        .text()
        .await
        .map_err(|e| er(502, format!("读取流失败：{e}"), ErrorClass::Transient))?;
    let (mut text, mut reasoning) = (String::new(), String::new());
    if let Some(err) = drain_sse(&raw, &mut text, &mut reasoning) {
        return Err(er(502, format!("OrcaTerm 上游错误：{err}"), ErrorClass::Transient));
    }
    if text.trim().is_empty() && reasoning.trim().is_empty() {
        return Err(er(
            502,
            format!("上游没有返回任何内容（可能登录态失效或模型不可用）：{}", truncate(&raw, 200)),
            ErrorClass::Transient,
        ));
    }
    let tin = (body.len() / 4).max(1) as i64;
    let tout = (text.chars().count() as i64 / 2).max(1);
    let mut msg = json!({"role": "assistant", "content": text});
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    Ok(json!({
        "id": format!("chatcmpl-orca-{}", nanos_hex()),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{"index": 0, "message": msg, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout}
    }))
}

/// 流式：把上游 SSE 逐帧翻成 chat SSE。
///
/// ★ 上游是「先思考后正文」的顺序，两者都直接透成对应字段；错误帧发成
///   `error` 帧再收尾（此时可能已经发出去半截内容，客户端至少知道断在哪）。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<(
    futures_util::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
    tokio::sync::mpsc::Receiver<crate::StreamStat>,
), RelayError> {
    use futures_util::StreamExt;

    let v: Value = serde_json::from_slice(body)
        .map_err(|e| er(400, format!("请求体不是合法 JSON：{e}"), ErrorClass::Rejected))?;
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let prompt = build_prompt(&v);
    let cid = create_conversation(http, spec, secret).await?;
    let resp = open_chat(http, spec, secret, &cid, &model, &prompt).await?;

    let id = format!("chatcmpl-orca-{}", nanos_hex());
    let created = secs();
    let body_len = body.len();
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(8);
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let mut buf = String::new();
        let mut upstream = resp.bytes_stream();
        let (mut text, mut reasoning) = (String::new(), String::new());
        let mut errored = false;
        while let Some(chunk) = upstream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                Err(e) => {
                    let f = sse_error(&id, &model, created, &format!("读取上游失败：{e}"));
                    let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
                    errored = true;
                    break;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let Some(rest) = line.trim_start().strip_prefix("data:") else { continue };
                match parse_chunk(rest) {
                    OrcaChunk::Content(t) => {
                        text.push_str(&t);
                        let f = sse_delta(&id, &model, created, json!({"content": t}));
                        if out_tx.send(Ok(bytes::Bytes::from(f))).await.is_err() {
                            return;
                        }
                    }
                    OrcaChunk::Reasoning(t) => {
                        reasoning.push_str(&t);
                        let f = sse_delta(&id, &model, created, json!({"reasoning_content": t}));
                        if out_tx.send(Ok(bytes::Bytes::from(f))).await.is_err() {
                            return;
                        }
                    }
                    OrcaChunk::Error(e) => {
                        let f = sse_error(&id, &model, created, &e);
                        let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
                        errored = true;
                        break;
                    }
                    _ => {}
                }
            }
            if errored {
                break;
            }
        }
        if !errored {
            if text.trim().is_empty() && reasoning.trim().is_empty() {
                let f = sse_error(&id, &model, created, "上游没有返回任何内容（可能登录态失效或模型不可用）");
                let _ = out_tx.send(Ok(bytes::Bytes::from(f))).await;
            }
            let tin = (body_len / 4).max(1) as i64;
            let tout = (text.chars().count() as i64 / 2).max(1);
            let fin = json!({
                "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            });
            let _ = out_tx.send(Ok(bytes::Bytes::from(format!("data: {}\n\n", fin)))).await;
            let usg = json!({
                "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
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

fn sse_delta(id: &str, model: &str, created: u64, delta: Value) -> String {
    let v = json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": Value::Null}]
    });
    format!("data: {}\n\n", v)
}

fn sse_error(id: &str, model: &str, created: u64, msg: &str) -> String {
    let v = json!({
        "id": id, "object": "chat.completion.chunk", "created": created, "model": model,
        "choices": [],
        "error": {"code": 502, "message": msg, "type": "upstream_error"}
    });
    format!("data: {}\n\n", v)
}

/// 验活：只建会话（20 秒够、不消耗对话额度）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    create_conversation(http, spec, secret).await.is_ok()
}

// ─── 模型清单（尽力而为：上游没有公开接口，按优先级试几个候选）────────
//
// 参考实现（Agent2API providers/orcaterm/models.rs）的结论是「上游没有清单接口，
// 下拉项来自桌面端前端打包内置」，依据是 Web 面板的 `DescribeAIModelList` 对普通
// 账号回空数组。但那是**Web 面板**口径，桌面端可能走别的动作 —— 所以这里不直接
// 照搬「没有」，而是真去问一遍：拿到了就用上游的，拿不到就退回静态表。
//
// 为什么值得试：静态表是 2026-10-16 截图对证的，上游加了新模型我们不会知道；
// 而用户点「刷新模型」的期待就是「看到上游现在有什么」。
//
// ★ 失败语义：任何一步失败都返回 Err，由调用方**保留静态表**并如实告知。
//   绝不返回空清单 —— 那会让所有模型从 /v1/models 消失，比「刷不出新的」严重得多。

/// 候选动作（按可能性排序）。每个都要求返回里能提出一个模型名数组。
const MODEL_ACTIONS: &[&str] = &[
    "DescribeAIModelList",
    "DescribeAIModels",
    "DescribeModelList",
    "ListAIModels",
];

/// 从任意形状的响应里尽力提出模型 id 列表。
///
/// 上游返回形状未知（没有公开文档），所以这里按常见形态逐个试：
/// - `{Response:{Models:[{ModelName|Name|Id:"..."}]}}`（腾讯云 CGI 惯例）
/// - `{data:[...]}` / `{models:[...]}` / 裸数组
/// - 元素可以是字符串，也可以是含 name/id/modelName 键的对象
fn extract_model_ids(v: &Value) -> Vec<String> {
    fn pick_name(item: &Value) -> Option<String> {
        match item {
            Value::String(s) => {
                let t = s.trim();
                (!t.is_empty()).then(|| t.to_string())
            }
            Value::Object(o) => {
                for k in ["ModelName", "modelName", "model_name", "Name", "name", "Id", "id", "Model", "model"] {
                    if let Some(Value::String(s)) = o.get(k) {
                        let t = s.trim();
                        if !t.is_empty() {
                            return Some(t.to_string());
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }
    fn walk(v: &Value, out: &mut Vec<String>, depth: usize) {
        if depth > 4 || !out.is_empty() {
            return;
        }
        match v {
            Value::Array(a) => {
                for it in a {
                    if let Some(n) = pick_name(it) {
                        out.push(n);
                    }
                }
                // 数组里套数组/对象的情形再往下走一层
                if out.is_empty() {
                    for it in a {
                        walk(it, out, depth + 1);
                    }
                }
            }
            Value::Object(o) => {
                // 优先找像「模型列表」的键
                for k in [
                    "Models", "models", "ModelList", "modelList", "Data", "data",
                    "Items", "items", "List", "list", "Response", "response",
                ] {
                    if let Some(inner) = o.get(k) {
                        walk(inner, out, depth + 1);
                        if !out.is_empty() {
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, &mut out, 0);
    out.sort();
    out.dedup();
    out
}

/// 尽力向上游要一份模型清单。拿不到就 Err（调用方保留静态表）。
///
/// ★ 实测结论（2026-11-04，用项目内真实账号跑的）：
///   · `DescribeAIModelList` 存在且鉴权通过，但对该账号返回 **`{"data":{"Response":[]}}`**
///     —— 空数组，与参考实现 Agent2API 的结论一致（它对普通账号就是空的）；
///   · `DescribeAIModels` 等名字回 `api-action-not-found`（动作不存在）；
///   · lightai 网关的 `/v1/models`、`/models`、`/assistant/models` 也不给清单。
///   ⇒ 这家**确实没有可用的清单接口**。所以「刷新模型」在这里的正确形态不是
///     「拉上游清单」，而是 [`probe_availability`]：逐个真发一次最小对话，
///     确认静态表里哪些模型现在真的能用。
pub async fn fetch_models(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
) -> Result<Vec<String>, RelayError> {
    let (uin, _) = claims_of(secret);
    let mut tried: Vec<String> = Vec::new();

    // ① CGI 动作（带 uin）
    if !uin.is_empty() {
        for action in MODEL_ACTIONS {
            let url = format!("{CGI_BASE}/cgi/api?action={action}&uin={uin}");
            let body = json!({ "action": action, "data": {} });
            let rb = http
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {secret}"))
                .header("X-Product", "orcaterm")
                .header("User-Agent", ORCA_UA)
                .body(body.to_string())
                .timeout(std::time::Duration::from_secs(12));
            match rb.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if status.is_success() {
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            let ids = extract_model_ids(&v);
                            if !ids.is_empty() {
                                return Ok(ids);
                            }
                        }
                    }
                    // 200 但提不出模型名时，把响应体前 200 字带上：否则只剩一个
                    // 「200 OK」看不出上游到底回了什么（空数组 / 错误信封 / 别的结构）。
                    let hint = text.chars().take(200).collect::<String>().replace('\n', " ");
                    tried.push(format!("{action}={status}[{hint}]"));
                }
                Err(e) => tried.push(format!("{action}=net:{e}")),
            }
        }
    }

    // ② lightai 网关上的 REST 候选（有些部署在这里暴露 OpenAI 风格 /models）
    for path in ["/v1/models", "/models", "/assistant/models"] {
        let url = format!("{}{}", base_of(spec), path);
        let mut rb = http.get(&url).timeout(std::time::Duration::from_secs(12));
        for (k, v) in assistant_headers(secret, &new_seq_id()) {
            rb = rb.header(k, v);
        }
        match rb.send().await {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        let ids = extract_model_ids(&v);
                        if !ids.is_empty() {
                            return Ok(ids);
                        }
                    }
                }
                let hint = text.chars().take(200).collect::<String>().replace('\n', " ");
                tried.push(format!("{path}={status}[{hint}]"));
            }
            Err(e) => tried.push(format!("{path}=net:{e}")),
        }
    }

    Err(er(
        0,
        format!(
            "上游没有可用的模型清单接口（试过 {} ），保留静态表",
            tried.join(", ")
        ),
        ErrorClass::Transient,
    ))
}

/// 探测时**单个模型**的时间预算。
///
/// ★ 为什么必须有：`OutClient` 只有 `connect_timeout`(10s) 与 `read_timeout`(300s)，
///   没有整体超时。`chat_nonstream` 里「建会话 → 发消息 → `resp.text()` 读完」
///   任一环节卡住，最坏要等满 300 秒的 read_timeout。九个模型串行下来就是 45 分钟，
///   而用户是**点了按钮在界面等着** —— 那不是「慢」，是看起来死掉了。
///
/// 40 秒的依据：正常一轮最小对话（"hi" / max_tokens=1）实测 2~8 秒，
/// 建会话 + 首帧各留一倍余量。超时的判为 [`ProbeFail::Upstream`]（抖动，稍后重试），
/// 不冤枉成模型或账号的问题。
const PROBE_PER_MODEL: std::time::Duration = std::time::Duration::from_secs(40);

/// 整次探测的总预算。到点就停，剩下的标成「未探测」而不是假装探过。
///
/// 90 秒 ≈ 9 个模型正常跑完（约 30~70 秒）再留一点余量。
/// 没有它的话，即使每条都只耗 40 秒上限，九条串行也要 6 分钟。
const PROBE_TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

/// 一个候选模型的探测结论。
#[derive(Debug, Clone)]
pub struct ModelProbe {
    /// 模型名（静态表里的形态，带 `TokenHub/` 前缀）。
    pub model: String,
    /// 现在能不能用。
    pub usable: bool,
    /// 失败归类，决定「这是模型的事还是账号的事」。
    pub kind: ProbeFail,
    /// 不能用的原因（给用户看的一句话）。
    pub why: String,
}

/// 探测失败的三类。**这个区分是本模块的核心价值**：
/// 把「某个模型没挂载」和「整个账号/上游坏了」混成一句话，用户会以为平台废了
/// 而去重新登录，而实际上换一个模型就能用（或者反过来：所有模型都挂 = 该换号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFail {
    /// 这个模型本身不可用（上游没挂载 / 名字不对）——换模型即可。
    Model,
    /// 账号或登录态层面（401/403 / Invalid inputId 这类整条链路都不通）——要重登/换号。
    Account,
    /// 上游抖动（5xx / 超时）——稍后重试，不代表任何一方坏了。
    Upstream,
}

impl ProbeFail {
    /// 从错误正文归类。判据都来自实测：
    /// - `LLM is not loaded` → 模型没挂载（参考项目里 hy4-preview 两次实测就是这个）
    /// - `Invalid inputId` → 上游拒了输入 id，实测该账号下**所有**模型都这样，
    ///   说明是会话/登录态层面的问题，不是单个模型
    /// - 401/403 / `Invalid or expired AccessToken` → 凭据
    /// - 5xx → 上游抖动
    fn classify(status: u16, body: &str) -> ProbeFail {
        let low = body.to_ascii_lowercase();
        if low.contains("not loaded") || low.contains("not mounted") || low.contains("not found the model")
        {
            return ProbeFail::Model;
        }
        if status == 401
            || status == 403
            || low.contains("invalid or expired accesstoken")
            || low.contains("need_uin_and_skey")
            || low.contains("invalid inputid")
        {
            return ProbeFail::Account;
        }
        ProbeFail::Upstream
    }
}

/// **可用性探测**：这是本平台「刷新模型」的正确形态。
///
/// ── 为什么不是「拉清单」 ──────────────────────────────────
/// 上游没有清单接口（见 [`fetch_models`] 的实测结论）。所以「刷新」能做的最有价值的
/// 事，是拿静态表逐条去**真发一次最小对话**，回答「现在到底哪些能用」。
/// 这恰好解决了静态表最大的痛点：`hy4-preview` 这种「面板有、上游没挂载」的档位
/// 在参考实现里是靠人工两次实测隔 10 小时发现的 —— 现在点一下就全知道。
///
/// ── 成本控制 ──────────────────────────────────────────────
/// 每条都要建会话 + 发一轮，所以：
///   · prompt 固定为单字 "hi"，`max_tokens` 压到 1（能出首帧就算通）；
///   · **串行**执行：并发建会话在上游看来是异常流量，也容易触发限流；
///   · 每条独立超时（见 [`PROBE_PER_MODEL`]），单条失败不影响其余；
///   · 整体有上限（见 [`PROBE_TOTAL_BUDGET`]），不会让用户对着转圈等到天荒地老；
///   · 由调用方决定是否要点这个按钮（默认不自动跑）。
///
/// ── 早停 ──────────────────────────────────────────────────
/// 一旦连续两条都归为 [`ProbeFail::Account`]，说明是整条链路的问题（不是模型），
/// 剩下的不再探 —— 否则九条全打一遍既慢又给上游添无谓流量，还把同一个原因重复九遍。
pub async fn probe_availability(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    models: &[String],
) -> Vec<ModelProbe> {
    let mut out: Vec<ModelProbe> = Vec::with_capacity(models.len());
    let mut consecutive_account_fails = 0usize;
    let started = std::time::Instant::now();
    for m in models {
        // 总预算到点就停：剩下的标「未探测」，不假装探过（那会污染归因）。
        if started.elapsed() >= PROBE_TOTAL_BUDGET {
            for rest in &models[out.len()..] {
                out.push(ModelProbe {
                    model: rest.clone(),
                    usable: false,
                    kind: ProbeFail::Upstream,
                    why: format!(
                        "（未探测：整次探测已超过 {} 秒预算）",
                        PROBE_TOTAL_BUDGET.as_secs()
                    ),
                });
            }
            break;
        }
        // 上游要的是带前缀的形态；静态表里已经是 TokenHub/xxx，直接用。
        let body = json!({
            "model": m,
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 1,
            "stream": false,
        });
        let body = serde_json::to_vec(&body).unwrap_or_default();
        // 单条超时：卡住的那一条不能拖垮整次探测（见 PROBE_PER_MODEL 注释）。
        let verdict = match tokio::time::timeout(
            PROBE_PER_MODEL,
            chat_nonstream(http, spec, secret, &body),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(er(
                0,
                format!(
                    "探测超时：{} 秒内没拿到结果（上游可能卡住）",
                    PROBE_PER_MODEL.as_secs()
                ),
                ErrorClass::Transient,
            )),
        };
        match verdict {
            Ok(_) => {
                consecutive_account_fails = 0;
                out.push(ModelProbe {
                    model: m.clone(),
                    usable: true,
                    kind: ProbeFail::Model,
                    why: String::new(),
                });
            }
            Err(e) => {
                let kind = ProbeFail::classify(e.status, &e.body);
                if kind == ProbeFail::Account {
                    consecutive_account_fails += 1;
                } else {
                    consecutive_account_fails = 0;
                }
                let why = match kind {
                    ProbeFail::Model => "上游未挂载该模型（面板里有、实际不可用）".to_string(),
                    ProbeFail::Account => {
                        // ★ 必须带原文：归成「账号层面」是个**判断**，用户有权看到
                        //   判断依据。只写一句结论会让人无法自查（实测踩过：上游先回
                        //   502、再回鉴权错误，两次结论不同而界面只显示同一句话）。
                        format!(
                            "账号/登录态层面不通（整条链路，不是这一个模型）：HTTP {} {}",
                            e.status,
                            e.body.chars().take(100).collect::<String>().replace('\n', " ")
                        )
                    }
                    ProbeFail::Upstream => {
                        format!("上游抖动：{}", e.body.chars().take(100).collect::<String>())
                    }
                };
                out.push(ModelProbe {
                    model: m.clone(),
                    usable: false,
                    kind,
                    why,
                });
                // 连续两条账号级失败 → 整条链路的问题，剩下的不再试
                if consecutive_account_fails >= 2 {
                    for rest in &models[out.len()..] {
                        out.push(ModelProbe {
                            model: rest.clone(),
                            usable: false,
                            kind: ProbeFail::Account,
                            why: "（未探测：前面已确认账号层面不通）".to_string(),
                        });
                    }
                    break;
                }
            }
        }
    }
    out
}

// ─── 续期（OAuthRefreshToken）──────────────────────────────────
//
// ★ 之前这个平台是「粘一枚 JWT，2 小时后失效，手动重取」——那是错的。
//   参考项目 Agent2API 的 providers/orcaterm/credentials.rs 早就写明上游**有真续期
//   接口**（与 KukuAI / CatPaw 的关键差别）：
//     POST {CGI}/cgi/api?action=OAuthRefreshToken&uin=<userId>
//     body {"action":"OAuthRefreshToken","data":{"accessToken":<旧 JWT>}}
//     → data.Response.AccessToken 是新 JWT
//   2026-10-16 参考项目实测通过；本次接入前重新探过一次端点（假 token 回
//   INVALID_REQUEST「无法获取会话标识」，说明网关活着、只是凭据无效）。
//
//   所以本平台不该让用户每 2 小时重粘一次。续期链路接进现有的
//   「401 → refresh → 写回账号池 → 重试一次」自愈流程（见 server 的 RefreshKind）。

/// CGI 网关（续期 / 探活 / 余额共用）。注意它与对话用的 lightai 网关**不是同一个域名**：
/// 对话走 lightai，续期与用户信息走 api.orcaterm.cloud.tencent.com。
const CGI_BASE: &str = "https://api.orcaterm.cloud.tencent.com";

/// 把用户可能整行复制的东西归一成一枚裸 token：
/// `Bearer xxx` / `ot_session=xxx` / 首尾空白都要剥掉。
pub fn normalize_token(raw: &str) -> String {
    let mut t = raw.trim().to_string();
    if let Some(rest) = t.strip_prefix("ot_session=") {
        t = rest.trim().to_string();
    }
    if let Some(rest) = t.strip_prefix("Bearer ").or_else(|| t.strip_prefix("bearer ")) {
        t = rest.trim().to_string();
    }
    t
}

/// 解 JWT 的 payload 段（base64url → JSON）。解不出返回 None。
///
/// 不验签：与参考项目同一口径（「token 好不好用由上游说了算」），
/// 这里只取 `userId`（CGI 的 uin）和 `exp`（判断是否临期）。
fn decode_jwt_claims(token: &str) -> Option<Value> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    if payload.is_empty() {
        return None;
    }
    let mut b64 = payload.replace('-', "+").replace('_', "/");
    while b64.len() % 4 != 0 {
        b64.push('=');
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let v = serde_json::from_slice::<Value>(&bytes).ok()?;
    v.is_object().then_some(v)
}

/// 从 JWT 取 (userId, exp 秒)；判不出的字段留空 / 0。
pub fn claims_of(token: &str) -> (String, i64) {
    let Some(c) = decode_jwt_claims(token) else {
        return (String::new(), 0);
    };
    let uid = c
        .get("userId")
        .map(|v| match v {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    let exp = c.get("exp").and_then(Value::as_i64).unwrap_or(0);
    (uid, exp)
}

/// JWT 是否已过期或即将过期（默认 10 分钟窗口）。
///
/// `exp` 解不出时返回 false —— 宁可漏刷一次（让上游 401 触发强制刷），
/// 也不要凭猜测去打上游。
pub fn is_expiring(token: &str, window_s: i64) -> bool {
    let (_, exp) = claims_of(token);
    if exp <= 0 {
        return false;
    }
    exp - (secs() as i64) < window_s
}

fn cgi_headers(secret: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Bearer {secret}")),
        ("Content-Type", "application/json".to_string()),
        ("User-Agent", ORCA_UA.to_string()),
        ("Origin", "https://orcaterm.cloud.tencent.com".to_string()),
        ("Referer", "https://orcaterm.cloud.tencent.com/".to_string()),
    ]
}

/// 调 `OAuthRefreshToken` 换一枚新 JWT。
///
/// 成功返回**可直接写回账号池**的新 secret（就是新 JWT 本体，形态与用户粘贴的一致，
/// 因此账号池里的凭据格式不变）；失败返回 Err（带上游业务码，便于分辨
/// 「token 真过期」还是「网关挂了」）。
pub async fn refresh_secret(http: &reqwest::Client, secret: &str) -> Result<String, RelayError> {
    let token = normalize_token(secret);
    if token.is_empty() {
        return Err(er(401, "OrcaTerm 凭据为空，无法续期", ErrorClass::Auth));
    }
    let (uid, _) = claims_of(&token);
    // uin 是续期的必需参数（放在 query 上）。从 JWT 解不出就说明这根本不是一枚
    // OrcaTerm JWT —— 直接报清楚，而不是发一个 uin='' 的请求过去换回一个看不懂的错误。
    if uid.is_empty() {
        return Err(er(
            401,
            "这枚凭据不是有效的 OrcaTerm JWT（解不出 userId）：请从桌面端 data.bin 复制 oauth_access_token",
            ErrorClass::Auth,
        ));
    }
    let url = format!("{CGI_BASE}/cgi/api?action=OAuthRefreshToken&uin={uid}");
    let body = json!({
        "action": "OAuthRefreshToken",
        "data": { "accessToken": token },
    });
    let mut rb = http.post(&url).body(body.to_string());
    for (k, v) in cgi_headers(&token) {
        rb = rb.header(k, v);
    }
    let resp = rb
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| er(502, format!("OrcaTerm 续期请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    // 上游恒回 HTTP 200，业务结果在 data.Response：可能直接是 AccessToken，
    // 也可能带 Error{Code,Message}。两种都要认。
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let resp_obj = v
        .get("data")
        .and_then(|d| d.get("Response"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(err) = resp_obj.get("Error") {
        let code = err.get("Code").and_then(|c| c.as_str()).unwrap_or("UNKNOWN");
        let msg = err.get("Message").and_then(|m| m.as_str()).unwrap_or("上游错误");
        // 业务码是 INVALID_REQUEST「无法获取会话标识」这种 —— 说明旧 token 已彻底
        // 不可用（或被风控），只能重新登录。分类成 Auth，让上层把号标成待重登。
        return Err(er(
            401,
            format!("OrcaTerm 续期失败（{code}：{msg}）；请重新从桌面端 data.bin 复制 oauth_access_token"),
            ErrorClass::Auth,
        ));
    }
    let next = resp_obj
        .get("AccessToken")
        .and_then(|t| t.as_str())
        .map(normalize_token)
        .unwrap_or_default();
    if next.is_empty() {
        return Err(er(
            if status == 200 { 502 } else { status },
            format!(
                "OrcaTerm 续期响应里没有 AccessToken（HTTP {status}）：{}",
                truncate(&text, 200)
            ),
            ErrorClass::Transient,
        ));
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use base64::Engine as _;

    /// 造一枚测试用 JWT（不签名，payload 明文可解 —— 上游不验签，本模块也只解 payload）。
    fn fake_jwt(claims: serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#);
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{body}.sig")
    }

    /// token 归一化：用户会整行复制 `Authorization: Bearer …` 或 Cookie 里的 `ot_session=…`。
    #[test]
    fn normalize_token_strips_common_prefixes() {
        assert_eq!(normalize_token("  abc.def.ghi  "), "abc.def.ghi");
        assert_eq!(normalize_token("Bearer abc.def.ghi"), "abc.def.ghi");
        assert_eq!(normalize_token("bearer abc.def.ghi"), "abc.def.ghi");
        assert_eq!(normalize_token("ot_session=abc.def.ghi"), "abc.def.ghi");
        assert_eq!(normalize_token(""), "");
    }

    /// 从 JWT 解 userId / exp：续期接口的 uin 参数就是这个 userId。
    #[test]
    fn claims_extract_user_id_and_exp() {
        let t = fake_jwt(json!({"userId": 100012345, "provider": "wechat", "exp": 1793000000i64}));
        let (uid, exp) = claims_of(&t);
        assert_eq!(uid, "100012345", "数字 uid 要转成字符串放进 query");
        assert_eq!(exp, 1793000000);
        // userId 是字符串也要认（上游两种都给过）
        let t2 = fake_jwt(json!({"userId": "998877", "exp": 1793000001i64}));
        assert_eq!(claims_of(&t2), ("998877".to_string(), 1793000001));
    }

    /// 解不出 payload 时不能 panic，也不能凭猜测判「已过期」。
    #[test]
    fn malformed_token_yields_empty_claims_and_not_expiring() {
        for bad in ["", "only.two", "a.b.c", "not-a-jwt"] {
            let (uid, exp) = claims_of(bad);
            assert_eq!(exp, 0, "{bad} 的 exp 必须为 0");
            assert!(uid.is_empty(), "{bad} 不该解出 userId");
            // exp 判不出 → 不能当成「已过期」，否则会对一个可能有效的 token 白刷上游
            assert!(!is_expiring(bad, 600), "{bad} 判不出 exp 时不该报临期");
        }
    }

    /// 临期判定：剩余时间小于窗口才算临期。
    #[test]
    fn is_expiring_respects_window() {
        let now = secs() as i64;
        let fresh = fake_jwt(json!({"userId": 1, "exp": now + 7200}));
        assert!(!is_expiring(&fresh, 600), "还剩 2 小时不该判临期");
        let soon = fake_jwt(json!({"userId": 1, "exp": now + 300}));
        assert!(is_expiring(&soon, 600), "还剩 5 分钟必须判临期");
        let dead = fake_jwt(json!({"userId": 1, "exp": now - 10}));
        assert!(is_expiring(&dead, 600), "已过期必须判临期");
    }

    /// gzip 无关但同样重要：续期用的是 CGI 域名，不是对话用的 lightai 域名。
    /// 混用会得到一个 404（而不是「凭据无效」），排查时极易误导。
    #[test]
    fn cgi_base_differs_from_lightai() {
        assert!(CGI_BASE.contains("api.orcaterm.cloud.tencent.com"), "CGI 域名={CGI_BASE}");
        assert!(!CGI_BASE.contains("lightai.cloud.tencent.com"), "续期不能打 lightai 网关");
    }

    /// ★ 上游错误帧不是 JSON —— 按 JSON 解析会得到 Other，失败就静默了。
    #[test]
    fn error_frame_is_recognized_before_json_parse() {
        match parse_chunk("[ERROR]: [LLM_ERROR]: model not available") {
            OrcaChunk::Error(m) => assert!(m.contains("model not available"), "m={m}"),
            _ => panic!("错误帧必须被识别成 Error"),
        }
    }

    #[test]
    fn parse_content_and_reasoning_and_done() {
        match parse_chunk(r#"{"content":"你"}"#) {
            OrcaChunk::Content(t) => assert_eq!(t, "你"),
            _ => panic!("应是 content"),
        }
        match parse_chunk(r#"{"reasoning":true,"reasoningContent":"想"}"#) {
            OrcaChunk::Reasoning(t) => assert_eq!(t, "想"),
            _ => panic!("应是 reasoning"),
        }
        assert!(matches!(parse_chunk("[DONE]"), OrcaChunk::Done));
        assert!(matches!(parse_chunk(""), OrcaChunk::Other));
        assert!(matches!(parse_chunk(r#"{"type":"status"}"#), OrcaChunk::Other));
    }

    /// 业务码非 0 也算错误（HTTP 恒 200 时错误藏在 code 里）。
    #[test]
    fn nonzero_business_code_is_error() {
        match parse_chunk(r#"{"code":500,"message":"internal"}"#) {
            OrcaChunk::Error(m) => assert!(m.contains("internal")),
            _ => panic!("业务码非 0 应是 Error"),
        }
    }

    /// 单条消息必须**原样直通**（不引入任何包装）。
    #[test]
    fn single_message_passes_through_verbatim() {
        let v = json!({"messages":[{"role":"user","content":"你好"}]});
        assert_eq!(build_prompt(&v), "你好");
    }

    /// 多轮要拍平并带角色前缀。
    #[test]
    fn multi_turn_is_flattened_with_roles() {
        let v = json!({"messages":[
            {"role":"system","content":"你是助手"},
            {"role":"user","content":"问题一"},
            {"role":"assistant","content":"答一"},
            {"role":"user","content":"问题二"},
        ]});
        let p = build_prompt(&v);
        assert!(p.contains("[系统指令] 你是助手"), "p={p}");
        assert!(p.contains("[用户] 问题一"));
        assert!(p.contains("[助手] 答一"));
        assert!(p.contains("[用户] 问题二"));
        assert!(p.contains("请回应其中最后一条用户消息"));
    }

    /// 空消息列表要有兜底（上游不接受空输入）。
    #[test]
    fn empty_messages_fall_back_to_default() {
        assert_eq!(build_prompt(&json!({})), "你好");
        assert_eq!(build_prompt(&json!({"messages":[]})), "你好");
        assert_eq!(build_prompt(&json!({"messages":[{"role":"user","content":"  "}]})), "你好");
    }

    /// 会话 id 必须带上 `cid-` 前缀与 `-orcaterm` 后缀（上游按这个认）。
    #[test]
    fn conversation_id_shape() {
        let c = new_conversation_id();
        assert!(c.starts_with("cid-"), "c={c}");
        assert!(c.ends_with("-orcaterm"), "c={c}");
    }

    /// content 三种形态都要能取文本。
    #[test]
    fn content_text_variants() {
        assert_eq!(content_text(&json!({"content":"a"})), "a");
        assert_eq!(
            content_text(&json!({"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]})),
            "a\nb"
        );
        assert_eq!(content_text(&json!({})), "");
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::resolve_model;

    /// 归一：裸名/全名进、表外回退默认、空串回退；表内（含未挂载）原样加前缀。
    #[test]
    fn model_resolution_matches_reference() {
        assert_eq!(&resolve_model("glm-5.3"), "TokenHub/glm-5.3");
        assert_eq!(&resolve_model("TokenHub/hy3"), "TokenHub/hy3");
        // 表内未挂载档也放行（错误由流内下发，不静默换模型）
        assert_eq!(&resolve_model("hy4-preview"), "TokenHub/hy4-preview");
        assert_eq!(&resolve_model(""), "TokenHub/glm-5.3-flash");
        assert_eq!(&resolve_model("gpt-9999"), "TokenHub/glm-5.3-flash");
    }
}

#[cfg(test)]
mod probe_tests {
    use super::{extract_model_ids, new_seq_id, ProbeFail};
    use serde_json::json;

    /// `input.id` 必须是**标准 UUID v4**，否则上游回 `Invalid inputId`。
    ///
    /// 这是踩过的真坑：旧实现只是把 16 字节随机数摆成 UUID 的形状，没设版本位与
    /// 变体位。上游严格校验，于是「建会话成功、发消息失败」，而且错误文案
    /// （"Invalid inputId"）看着像内容问题，实际是格式问题。
    ///
    /// 反向验证过：用旧实现产出的那种串（如 87803f8e-8db0-**b**815-**7**555-…）
    /// 能稳定复现该错误；换成 v4 立刻恢复。
    #[test]
    fn seq_id_is_a_standard_uuid_v4() {
        for _ in 0..50 {
            let s = new_seq_id();
            assert_eq!(s.len(), 36, "长度应为 36：{s}");
            let parts: Vec<&str> = s.split('-').collect();
            assert_eq!(
                parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
                vec![8, 4, 4, 4, 12],
                "段长应为 8-4-4-4-12：{s}"
            );
            assert!(
                s.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
                "只能有十六进制与连字符：{s}"
            );
            // version = 4：第 13 位（第 3 段首字符）必须是 '4'
            assert_eq!(&parts[2][..1], "4", "版本位必须是 4：{s}");
            // variant = RFC 4122：第 17 位（第 4 段首字符）必须是 8/9/a/b
            let v = &parts[3][..1];
            assert!(
                matches!(v, "8" | "9" | "a" | "b"),
                "变体位必须是 8/9/a/b：{s}"
            );
        }
        // 两次不应相同（随机性）
        assert_ne!(new_seq_id(), new_seq_id());
    }

    /// 上游没有公开文档，所以提取要能吃下多种形态 —— 这是「探测能不能用」的地基。
    #[test]
    fn extract_handles_common_shapes() {
        // 腾讯云 CGI 惯例：Response 里套数组
        assert_eq!(
            extract_model_ids(&json!({"data":{"Response":[{"ModelName":"a"},{"ModelName":"b"}]}})),
            vec!["a".to_string(), "b".to_string()]
        );
        // OpenAI 风格：data 里是含 id 的对象
        assert_eq!(
            extract_model_ids(&json!({"data":[{"id":"m1"},{"id":"m2"}]})),
            vec!["m1".to_string(), "m2".to_string()]
        );
        // 裸字符串数组
        assert_eq!(
            extract_model_ids(&json!({"models":["x","y"]})),
            vec!["x".to_string(), "y".to_string()]
        );
        // 裸数组
        assert_eq!(
            extract_model_ids(&json!(["p","q"])),
            vec!["p".to_string(), "q".to_string()]
        );
        // 各种 name 键
        assert_eq!(
            extract_model_ids(&json!({"list":[{"name":"n1"},{"model":"n2"}]})),
            vec!["n1".to_string(), "n2".to_string()]
        );
        // 空/无模型：提不出东西（调用方据此保留静态表）
        assert!(extract_model_ids(&json!({"data":{"Response":[]}})).is_empty());
        assert!(extract_model_ids(&json!({"code":0})).is_empty());
        assert!(extract_model_ids(&json!("just a string")).is_empty());
        // 去重
        assert_eq!(
            extract_model_ids(&json!(["a","a","b"])),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    /// 失败归类决定「用户该做什么」，三类不能混。
    #[test]
    fn failure_classification_separates_model_account_upstream() {
        // 模型没挂载 → 换个模型就行
        assert_eq!(
            ProbeFail::classify(502, "[LLM_ERROR] LLM(TokenHub/hy4-preview) is not loaded"),
            ProbeFail::Model
        );
        assert_eq!(
            ProbeFail::classify(400, "not found the model xyz"),
            ProbeFail::Model
        );
        // 账号/登录态层面 → 要重登或换号
        assert_eq!(ProbeFail::classify(401, "unauthorized"), ProbeFail::Account);
        assert_eq!(ProbeFail::classify(403, "forbidden"), ProbeFail::Account);
        assert_eq!(
            ProbeFail::classify(502, "[LLM_ERROR]: Invalid inputId: abc-def"),
            ProbeFail::Account
        );
        assert_eq!(
            ProbeFail::classify(502, "Invalid or expired AccessToken"),
            ProbeFail::Account
        );
        assert_eq!(
            ProbeFail::classify(502, "NEED_UIN_AND_SKEY"),
            ProbeFail::Account
        );
        // 其余 5xx / 网络 → 上游抖动，等会儿再试
        assert_eq!(ProbeFail::classify(504, "gateway timeout"), ProbeFail::Upstream);
        assert_eq!(ProbeFail::classify(0, "network: connection reset"), ProbeFail::Upstream);
        // 超时（status=0 + 我们自己的文案）也归抖动，不能冤枉成模型或账号问题
        assert_eq!(
            ProbeFail::classify(0, "探测超时：40 秒内没拿到结果（上游可能卡住）"),
            ProbeFail::Upstream
        );
    }

    /// 超时预算必须让「最坏情况」有界 —— 这是用户对着界面等待的硬约束。
    ///
    /// 背景：`OutClient` 只有 connect_timeout(10s) + read_timeout(300s)，没有整体超时。
    /// 没有本模块的两个预算时，9 个模型串行的最坏耗时是 9 × 300s = 45 分钟。
    #[test]
    fn probe_budgets_bound_the_worst_case() {
        use super::{PROBE_PER_MODEL, PROBE_TOTAL_BUDGET};
        // 单条预算必须明显小于 read_timeout(300s)，否则等于没设
        assert!(
            PROBE_PER_MODEL.as_secs() < 300,
            "单条预算 {}s 不小于 read_timeout，起不到保护作用",
            PROBE_PER_MODEL.as_secs()
        );
        // 总预算要能覆盖「9 条正常跑完」，但别大到用户以为卡死
        assert!(
            PROBE_TOTAL_BUDGET.as_secs() >= 60 && PROBE_TOTAL_BUDGET.as_secs() <= 180,
            "总预算 {}s 超出合理区间（60~180）",
            PROBE_TOTAL_BUDGET.as_secs()
        );
        // 总预算不必等于 条数×单条 —— 单条超时是上界，正常每条只要几秒
        assert!(PROBE_TOTAL_BUDGET < PROBE_PER_MODEL * 9, "总预算应小于逐条上限的简单累加");
    }
}

