//! Qoder（QoderWork / Qoder CLI）adapter —— 纯 HTTP，COSY 签名 + SSE 信封流。
//!
//! 一手来源：_refs/qoder2api-hub-main（qoder_proxy.py / qoder_sign.py / qoder_catalog.py）。
//! 该平台没有 OpenAI 兼容端点，链路分三层：
//! 1. 业务面 `GET {openapi}/api/v1/userinfo`（纯 Bearer）拿 uid / user_type / org；
//! 2. 数据面 `POST {gateway}/algo/api/v2/service/pro/sse/agent_chat_generation?...`，
//!    body 是官方 baseprompt.json 模板经 **qoder_encode**（自定义 base64）编码后的原始字节，
//!    头组由 `aigw_crypto::qoder::CosySession` 生成（cosy-* + Bearer COSY.payload.sig）；
//! 3. 响应是**信封 SSE**：`data:{"headers":{…},"body":"<内层 OpenAI chunk>","statusCodeValue":200}`，
//!    `data:{"body":"[DONE]"}` 结束；statusCodeValue != 200 时 body 是错误详情。
//!
//! 登录（设备码）已在 auth_login.rs 的 qoder_start / qoder_poll 里实现，本模块只做推理。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_crypto::qoder::{qoder_encode, CosySession};
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::sync::mpsc;

/// 官方 baseprompt 模板（请求体骨架，59KB）。
const BASE_PROMPT: &str = include_str!("qoder_baseprompt.json");

/// 推理端点（path 会去掉 `/algo` 前缀参与签名）。
const CHAT_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";

const QD_USER_TYPE: &str = "personal_professional_trial";

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

/// 导入的 Qoder 凭据：access token（`dt-` 前缀）+ 可选身份字段。
#[derive(Clone, Debug, Default)]
pub struct QdCred {
    pub token: String,
    pub uid: String,
    pub user_type: String,
    pub org_id: String,
    pub org_name: String,
    pub refresh_token: String,
}

fn pick_one(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.trim().to_string());
            }
        }
    }
    None
}

/// 解析凭据：JSON（accessToken/uid/user_type/…）或裸 token 字符串。
pub fn parse_cred(secret: &str) -> Result<QdCred, String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("Qoder 凭据为空：请粘贴设备授权得到的 access token（dt- 开头）".into());
    }
    if s.starts_with('{') {
        let v: Value = serde_json::from_str(s).map_err(|e| format!("Qoder 凭据 JSON 解析失败：{e}"))?;
        let token = pick_one(&v, &["accessToken", "access_token", "token", "userToken"])
            .ok_or_else(|| "Qoder 凭据 JSON 里缺 accessToken/token".to_string())?;
        return Ok(QdCred {
            token,
            uid: pick_one(&v, &["uid", "id", "userId", "user_id"]).unwrap_or_default(),
            user_type: pick_one(&v, &["userType", "user_type"]).unwrap_or_default(),
            org_id: pick_one(&v, &["organizationId", "organization_id", "orgId"]).unwrap_or_default(),
            org_name: pick_one(&v, &["organizationName", "organization_name", "orgName"]).unwrap_or_default(),
            refresh_token: pick_one(&v, &["refreshToken", "refresh_token"]).unwrap_or_default(),
        });
    }
    Ok(QdCred { token: s.to_string(), ..Default::default() })
}

/// 区域：base_url 含 qoder.com.cn 视为国内版，其余走国际版。
fn is_cn(spec: &ProviderSpec) -> bool {
    spec.base_url.contains("qoder.com.cn") || spec.hosts.iter().any(|h| h.contains("qoder.com.cn"))
}

fn openapi_base(spec: &ProviderSpec) -> String {
    for (k, v) in &spec.headers {
        if k == "x-qoder-openapi" && !v.is_empty() {
            return v.clone();
        }
    }
    if is_cn(spec) {
        "https://openapi.qoder.com.cn".into()
    } else {
        "https://openapi.qoder.sh".into()
    }
}

/// 推理主机候选（官方故障切换域名）；测试靠 spec.base_url 注入 mock。
fn gateways(spec: &ProviderSpec) -> Vec<String> {
    if !spec.base_url.is_empty() && (spec.base_url.starts_with("http://") || spec.base_url.contains("127.0.0.1")) {
        return vec![spec.base_url.clone()];
    }
    if is_cn(spec) {
        vec!["https://gateway.qoder.com.cn".into()]
    } else {
        vec![
            "https://api1.qoder.sh".into(),
            "https://api2.qoder.sh".into(),
            "https://api3.qoder.sh".into(),
        ]
    }
}

/// 模型目录条目：上游 key / 显示名 / 是否推理 / 是否视觉 / 最大输入。
pub struct ModelMeta {
    pub key: &'static str,
    pub display: &'static str,
    pub reasoning: bool,
    pub vl: bool,
    pub max_input: u64,
}

const CN_MODELS: &[ModelMeta] = &[
    ModelMeta { key: "auto", display: "Auto", reasoning: true, vl: true, max_input: 200000 },
    ModelMeta { key: "qmodel_38max", display: "Qwen3.8-Max", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "qfmodel", display: "Qwen3.8-Flash", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "qmodel_latest", display: "Qwen3.7-Max", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "qmodel", display: "Qwen3.7-Plus", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "q37fmodel", display: "Qwen3.7-Flash", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "dmodel", display: "DeepSeek-V4-Pro", reasoning: true, vl: true, max_input: 96000 },
    ModelMeta { key: "dfmodel", display: "DeepSeek-Flash", reasoning: false, vl: true, max_input: 180000 },
    ModelMeta { key: "gmodel", display: "GLM-5.3", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "gfmodel", display: "GLM-5.3-Flash", reasoning: true, vl: true, max_input: 1000000 },
    ModelMeta { key: "gm51model", display: "GLM-5.2", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "kmodel_latest", display: "Kimi-K3", reasoning: false, vl: true, max_input: 180000 },
    ModelMeta { key: "kmodel", display: "Kimi-K2.8-Preview", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "mmodel", display: "MiniMax-M2.7", reasoning: false, vl: false, max_input: 180000 },
];

const INTL_MODELS: &[ModelMeta] = &[
    ModelMeta { key: "auto", display: "Auto", reasoning: false, vl: true, max_input: 200000 },
    ModelMeta { key: "ultimate", display: "Ultimate", reasoning: true, vl: true, max_input: 1000000 },
    ModelMeta { key: "performance", display: "Performance", reasoning: false, vl: true, max_input: 1000000 },
    ModelMeta { key: "efficient", display: "Efficient", reasoning: false, vl: true, max_input: 200000 },
    ModelMeta { key: "smodel", display: "Sonus", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "cmodel", display: "Cantus", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "qmodel_38max", display: "Qwen3.8-Max", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "qfmodel", display: "Qwen3.8-Flash", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "qmodel_latest", display: "Qwen3.7-Max", reasoning: false, vl: true, max_input: 1000000 },
    ModelMeta { key: "qmodel", display: "Qwen3.7-Plus", reasoning: false, vl: true, max_input: 1000000 },
    ModelMeta { key: "kmodel_latest", display: "Kimi-K3", reasoning: false, vl: true, max_input: 180000 },
    ModelMeta { key: "kmodel", display: "Kimi-K2.8-Preview", reasoning: false, vl: true, max_input: 180000 },
    ModelMeta { key: "gmodel", display: "GLM-5.3", reasoning: true, vl: true, max_input: 180000 },
    ModelMeta { key: "gfmodel", display: "GLM-5.3-Flash", reasoning: true, vl: true, max_input: 1000000 },
    ModelMeta { key: "dmodel", display: "DeepSeek-V4-Pro", reasoning: true, vl: true, max_input: 1000000 },
    ModelMeta { key: "dfmodel", display: "DeepSeek-Flash", reasoning: true, vl: true, max_input: 1000000 },
    ModelMeta { key: "mmodel", display: "MiniMax-M3", reasoning: false, vl: true, max_input: 180000 },
];

/// 别名表（客户端常用写法 → 上游 key）。
const MODEL_ALIASES: &[(&str, &str)] = &[
    ("auto", "auto"),
    ("qwen3.8-max", "qmodel_38max"),
    ("qwen3.8-flash", "qfmodel"),
    ("qwen3.7-max", "qmodel_latest"),
    ("qwen3.7-plus", "qmodel"),
    ("qwen3.7-flash", "q37fmodel"),
    ("deepseek-v4-pro", "dmodel"),
    ("deepseek-v4-flash", "dfmodel"),
    ("glm-5.3", "gmodel"),
    ("glm-5.3-flash", "gfmodel"),
    ("glm-5.2", "gm51model"),
    ("kimi-k3", "kmodel_latest"),
    ("kimi-k2.8-preview", "kmodel"),
    ("kimi-k2.7", "kmodel"),
    ("minimax-m3", "mmodel"),
    ("minimax-m2.7", "mmodel"),
    ("sonus", "smodel"),
    ("cantus", "cmodel"),
    ("ultimate", "ultimate"),
    ("performance", "performance"),
    ("efficient", "efficient"),
];

fn catalog(spec: &ProviderSpec) -> &'static [ModelMeta] {
    if is_cn(spec) { CN_MODELS } else { INTL_MODELS }
}

/// 把客户端模型名解析为上游 model key。
pub fn resolve_key(spec: &ProviderSpec, model: &str) -> String {
    let m = model.trim();
    if m.is_empty() {
        return "auto".into();
    }
    // 展示 id 形态 "qmodel_38max (Qwen3.8-Max)"
    let head = if let Some(pos) = m.rfind(" (") {
        if m.ends_with(')') { m[..pos].trim() } else { m }
    } else {
        m
    };
    let list = catalog(spec);
    if list.iter().any(|x| x.key == head) {
        return head.to_string();
    }
    let low = head.to_lowercase();
    for (k, v) in MODEL_ALIASES {
        if *k == low {
            return (*v).to_string();
        }
    }
    if let Some(hit) = list.iter().find(|x| x.display.eq_ignore_ascii_case(&low)) {
        return hit.key.to_string();
    }
    // 兜底：原样下发（上游自己会报错，比静默改成 auto 更可诊断）
    head.to_string()
}

fn meta_of(spec: &ProviderSpec, key: &str) -> Option<&'static ModelMeta> {
    catalog(spec).iter().find(|x| x.key == key)
}


// ══════════════════════════════════════════════════════════════
// 会话：业务面 userinfo → COSY 会话
// ══════════════════════════════════════════════════════════════

/// GET {openapi}/api/v1/userinfo（纯 Bearer）。
async fn fetch_userinfo(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    token: &str,
) -> Result<Value, String> {
    let url = format!("{}/api/v1/userinfo", openapi_base(spec));
    let resp = http
        .get(&url)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", "application/json")
        .header("user-agent", format!("Qoder/{QD_VER}"))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Qoder userinfo 请求失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("Qoder userinfo HTTP {status}：{}", trunc(&text, 200)));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|e| format!("Qoder userinfo 非 JSON：{e}；{}", trunc(&text, 200)))
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

/// userinfo 响应兼容：裸对象或 {data:{…}} / {result:{…}} 包裹。
fn unwrap_data(v: &Value) -> &Value {
    for k in ["data", "result"] {
        if let Some(inner) = v.get(k) {
            if inner.is_object() {
                return inner;
            }
        }
    }
    v
}

/// Qoder 目前**没有**可用的刷新链路：设备授权（deviceToken/poll）只发 `dt-`
/// access token，不发 refreshToken，上游也没公开 refresh 端点。
///
/// 这里显式留一个返回 Err 的桩而不是让调用方猜：凭据过期时 server 层会拿到
/// 「Qoder 凭据已过期：请到「接入源 → Qoder」重新走设备授权登录（该平台设备授权
/// 只发 access token，没有 refreshToken 可续期）」这条可直接展示给用户的提示，
/// 而不是含糊的「重新登录」。上游哪天开放续期，只需要改这一个函数。
pub async fn refresh_secret(_http: &reqwest::Client, _secret: &str) -> Result<String, String> {
    Err("Qoder 凭据已过期：请到「接入源 → Qoder」重新走设备授权登录（该平台设备授权只发 access token，没有 refreshToken 可续期）".into())
}

/// 从 userinfo 里补全身份（uid / user_type / 组织）。
fn fill_identity(cred: &mut QdCred, info: &Value) {
    let d = unwrap_data(info);
    if cred.uid.is_empty() {
        cred.uid = pick_one(d, &["id", "uid", "user_id", "userId"]).unwrap_or_default();
    }
    if cred.user_type.is_empty() {
        cred.user_type = pick_one(d, &["user_type", "userType"]).unwrap_or_default();
    }
    if cred.org_id.is_empty() {
        cred.org_id = pick_one(d, &["organization_id", "organizationId"]).unwrap_or_default();
    }
    if cred.org_name.is_empty() {
        cred.org_name =
            pick_one(d, &["organization_name", "organizationName"]).unwrap_or_default();
    }
}

/// 准备一次推理所需的全部上下文（凭据 → 身份 → COSY 会话 → 上游 body）。
struct QdPrepared {
    cosy: CosySession,
    body: String,
    model_key: String,
    in_bytes: usize,
}

async fn prepare(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    raw: &[u8],
) -> Result<QdPrepared, RelayError> {
    let mut cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(raw).unwrap_or_else(|_| json!({}));
    if cred.uid.is_empty() || cred.user_type.is_empty() {
        match fetch_userinfo(http, spec, &cred.token).await {
            Ok(info) => fill_identity(&mut cred, &info),
            Err(e) => {
                return Err(er(
                    401,
                    format!("Qoder 凭据校验失败：{e}（请重新走设备授权登录）"),
                    ErrorClass::Auth,
                ))
            }
        }
    }
    if cred.user_type.is_empty() {
        cred.user_type = QD_USER_TYPE.to_string();
    }
    let cosy = CosySession::new(&cred.uid, &cred.token)
        .map_err(|e| er(500, format!("Qoder COSY 会话构建失败：{e}"), ErrorClass::Transient))?;
    let model_key = resolve_key(spec, v.get("model").and_then(|m| m.as_str()).unwrap_or(""));
    let body_obj = build_body(spec, &cred, &v, &model_key);
    let plain = serde_json::to_vec(&body_obj).unwrap_or_default();
    let body = qoder_encode(&plain);
    let in_bytes = plain.len();
    Ok(QdPrepared { cosy, body, model_key, in_bytes })
}

/// 基座模板（进程内只解析一次）。
fn base_prompt() -> &'static Value {
    static BASE: OnceLock<Value> = OnceLock::new();
    BASE.get_or_init(|| serde_json::from_str(BASE_PROMPT).unwrap_or_else(|_| json!({})))
}

const LEAK_MARKER: &str = "[assistant 请求调用工具]";
const TOOL_RESULT_MARKER: &str = "[工具结果";

/// 结构化工具历史白名单（上游转换层认可 tool 角色的模型族）。
fn structured_ok(spec: &ProviderSpec, key: &str) -> bool {
    let k = key.to_lowercase();
    is_cn(spec) && ["qwen", "glm", "gm4", "gm5"].iter().any(|p| k.starts_with(p))
}

fn is_deepseek_key(key: &str) -> bool {
    let k = key.to_lowercase();
    k.starts_with("dmodel") || k.starts_with("dfmodel")
}

/// content 压平：返回 (文本, 图片 URL 列表)。
fn content_text(c: Option<&Value>) -> (String, Vec<String>) {
    let mut images = Vec::new();
    let Some(c) = c else {
        return (String::new(), images);
    };
    if let Some(s) = c.as_str() {
        return (s.to_string(), images);
    }
    let parts: Vec<&Value> = match c {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut out = String::new();
    for p in parts {
        if let Some(s) = p.as_str() {
            out.push_str(s);
            continue;
        }
        let ty = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match ty {
            "text" | "input_text" | "output_text" | "summary_text" => {
                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                    out.push_str(t);
                }
            }
            "image_url" | "input_image" | "image" => {
                let u = p
                    .get("image_url")
                    .and_then(|x| x.get("url").or_else(|| x.as_str().map(|_| x)))
                    .and_then(|x| x.as_str())
                    .or_else(|| p.get("url").and_then(|x| x.as_str()))
                    .or_else(|| p.get("image_url").and_then(|x| x.as_str()));
                if let Some(u) = u {
                    images.push(u.to_string());
                }
            }
            _ => {
                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                    out.push_str(t);
                }
            }
        }
    }
    (out, images)
}

fn norm_args(a: Option<&Value>) -> String {
    match a {
        Some(Value::String(s)) => s.clone(),
        Some(v @ Value::Object(_)) | Some(v @ Value::Array(_)) => {
            serde_json::to_string(v).unwrap_or_default()
        }
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

/// 客户端会话压平为上游可接受的 {role, content} 序列。
fn flatten_messages(
    v: &Value,
    keep_reasoning: bool,
    structured: bool,
) -> (Option<String>, Vec<Value>, Vec<String>) {
    let mut system_text: Option<String> = None;
    let mut flat: Vec<Value> = Vec::new();
    let mut all_images: Vec<String> = Vec::new();
    let msgs = v.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
    for m in msgs {
        if !m.is_object() {
            continue;
        }
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let (text, imgs) = content_text(m.get("content"));
        all_images.extend(imgs);
        let role = if role == "developer" { "system" } else { role };
        if role == "system" {
            if system_text.is_none() && !text.trim().is_empty() {
                system_text = Some(text);
            }
            continue;
        }
        if role == "tool" {
            let name = m.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let name_part = if name.is_empty() { String::new() } else { format!(" ({name})") };
            if structured {
                flat.push(json!({
                    "role": "tool",
                    "tool_call_id": m.get("tool_call_id").and_then(|x| x.as_str()).unwrap_or(""),
                    "content": text,
                }));
            } else {
                flat.push(json!({
                    "role": "user",
                    "content": format!("{TOOL_RESULT_MARKER}{name_part}]\n{text}"),
                }));
            }
            continue;
        }
        if !matches!(role, "user" | "assistant") {
            if !role.is_empty() {
                flat.push(json!({"role": "user", "content": text}));
            }
            continue;
        }
        let mut item = json!({"role": role, "content": text});
        if role == "assistant" {
            if let Some(calls) = m.get("tool_calls").and_then(|c| c.as_array()) {
                if !calls.is_empty() {
                    if structured {
                        let mapped: Vec<Value> = calls
                            .iter()
                            .map(|tc| {
                                let f = tc.get("function").cloned().unwrap_or_else(|| json!({}));
                                json!({
                                    "id": tc.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                                    "type": tc.get("type").and_then(|x| x.as_str()).unwrap_or("function"),
                                    "function": {
                                        "name": f.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                                        "arguments": norm_args(f.get("arguments")),
                                    },
                                })
                            })
                            .collect();
                        item["tool_calls"] = json!(mapped);
                    } else {
                        let slim: Vec<Value> = calls
                            .iter()
                            .map(|tc| {
                                let f = tc.get("function").cloned().unwrap_or_else(|| json!({}));
                                json!({
                                    "name": f.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                                    "arguments": norm_args(f.get("arguments")),
                                })
                            })
                            .collect();
                        let base = item["content"].as_str().unwrap_or("").to_string();
                        item["content"] = json!(format!(
                            "{base}\n\n{LEAK_MARKER}\n{}",
                            serde_json::to_string(&slim).unwrap_or_default()
                        ));
                    }
                }
            }
            if keep_reasoning {
                if let Some(rc) = m.get("reasoning_content").and_then(|x| x.as_str()) {
                    item["reasoning_content"] = json!(rc);
                }
            }
        }
        flat.push(item);
    }
    (system_text, flat, all_images)
}

/// 工具定义归一化：顶层 name 形态包成 {type:function,function:{…}}。
fn normalize_tools(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(tools) = v.get("tools").and_then(|t| t.as_array()) else {
        return out;
    };
    for t in tools {
        if let Some(f) = t.get("function") {
            out.push(json!({"type": "function", "function": f}));
            continue;
        }
        let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let mut f = json!({"name": name});
        for k in ["description", "parameters", "strict"] {
            if let Some(x) = t.get(k) {
                f[k] = x.clone();
            }
        }
        out.push(json!({"type": "function", "function": f}));
    }
    out
}

/// 深拷贝 baseprompt 并覆写为本次请求（对照 build_qoder_body）。
pub fn build_body(spec: &ProviderSpec, cred: &QdCred, v: &Value, model_key: &str) -> Value {
    let mut body = base_prompt().clone();
    let key_meta = meta_of(spec, model_key);
    let structured = structured_ok(spec, model_key);
    let keep_reasoning = is_deepseek_key(model_key);
    let (system_text, flat, images) = flatten_messages(v, keep_reasoning, structured);

    // 模板只保留 system 基座，其自带示例 user 轮次丢弃
    let tmpl_system: Vec<Value> = body
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut msgs: Vec<Value> = match &system_text {
        Some(s) => vec![json!({"role": "system", "content": s})],
        None => tmpl_system,
    };
    msgs.extend(flat.iter().cloned());
    body["messages"] = json!(msgs);

    // 最新用户提示词（chat_context 高亮 + business 名）
    let mut prompt = String::new();
    for m in flat.iter().rev() {
        if m.get("role").and_then(|r| r.as_str()) == Some("user") {
            let t = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if !t.is_empty() {
                prompt = t.to_string();
                break;
            }
        }
    }

    let nid = uuid_like();
    body["request_id"] = json!(nid);
    body["chat_record_id"] = json!(nid);
    body["request_set_id"] = json!(uuid_like());
    body["session_id"] = json!(uuid_like());
    body["stream"] = json!(true);
    body["agent_id"] = json!("agent_common");
    body["aliyun_user_type"] = json!(if cred.user_type.is_empty() {
        QD_USER_TYPE
    } else {
        cred.user_type.as_str()
    });

    // model_config
    let mut mc = body.get("model_config").cloned().unwrap_or_else(|| json!({}));
    mc["key"] = json!(model_key);
    mc["display_name"] = json!(key_meta.map(|m| m.display).unwrap_or(model_key));
    mc["model"] = json!("");
    mc["format"] = json!("openai");
    mc["is_vl"] = json!(key_meta.map(|m| m.vl).unwrap_or(false));
    mc["is_reasoning"] = json!(key_meta.map(|m| m.reasoning).unwrap_or(false));
    mc["api_key"] = json!("");
    mc["url"] = json!("");
    mc["source"] = json!("system");
    mc["max_input_tokens"] = json!(key_meta.map(|m| m.max_input).unwrap_or(180_000));
    body["model_config"] = mc.clone();

    // chat_context
    let mut cc = body.get("chat_context").cloned().unwrap_or_else(|| json!({}));
    let mut txt = cc.get("text").cloned().unwrap_or_else(|| json!({}));
    if !txt.is_object() {
        txt = json!({"type": "text", "text": ""});
    }
    txt["text"] = json!(prompt);
    cc["text"] = txt;
    let mut extra = cc.get("extra").cloned().unwrap_or_else(|| json!({}));
    if !extra.is_object() {
        extra = json!({});
    }
    let mut oc = extra.get("originalContent").cloned().unwrap_or_else(|| json!({}));
    if !oc.is_object() {
        oc = json!({"type": "text", "text": ""});
    }
    oc["text"] = json!(prompt);
    extra["originalContent"] = oc;
    extra["modelConfig"] = mc;
    cc["extra"] = extra;
    if !images.is_empty() {
        cc["imageUrls"] = json!(images);
        body["image_urls"] = json!(images);
    }
    body["chat_context"] = cc;

    // parameters
    let mut params = body.get("parameters").cloned().unwrap_or_else(|| json!({}));
    if let Some(mt) = v
        .get("max_tokens")
        .or_else(|| v.get("max_completion_tokens"))
        .and_then(|x| x.as_i64())
    {
        params["max_tokens"] = json!(mt);
    }
    if let Some(e) = v
        .get("reasoning_effort")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
        params["reasoning_effort"] = json!(e);
    }
    body["parameters"] = params;

    // tools：只用客户端的，避免把 Qoder 桌面工具泄漏出去
    body["tools"] = json!(normalize_tools(v));

    // business
    let mut biz = body.get("business").cloned().unwrap_or_else(|| json!({}));
    biz["id"] = json!(uuid_like());
    biz["begin_at"] = json!(now_ms());
    biz["name"] = json!(if prompt.is_empty() {
        "chat".to_string()
    } else {
        prompt.chars().take(30).collect::<String>()
    });
    for (k, val) in [("product", "cli"), ("type", "agent"), ("stage", "start")] {
        if biz.get(k).is_none() {
            biz[k] = json!(val);
        }
    }
    body["business"] = biz;
    body
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

const QD_VER: &str = "1.1.64";

// ══════════════════════════════════════════════════════════════
// 数据面：信封 SSE 解包
// ══════════════════════════════════════════════════════════════

/// 一帧信封的解析结果。
#[derive(Debug)]
enum Env {
    Chunk(Value),
    Done,
}

fn env_status_class(code: u16) -> ErrorClass {
    match code {
        401 | 403 => ErrorClass::Auth,
        402 => ErrorClass::Quota,
        429 => ErrorClass::Rate,
        400..=499 => ErrorClass::Rejected,
        _ => ErrorClass::Transient,
    }
}

/// 去掉 `data:` 前缀；跳过空行与 `:` 注释行。
fn data_of(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || t.starts_with(':') {
        return None;
    }
    if let Some(rest) = t.strip_prefix("data:") {
        let r = rest.trim();
        if r.is_empty() {
            return None;
        }
        return Some(r);
    }
    // 少数情况下上游直接吐内层 JSON，没有 data: 前缀
    if t.starts_with('{') {
        return Some(t);
    }
    None
}

/// 清理内层 chunk：删空 function_call / 空 tool_calls / 假值噪声键。
fn clean_chunk(mut v: Value) -> Value {
    let Some(choices) = v.get("choices").and_then(|c| c.as_array()).cloned() else {
        return v;
    };
    if choices.is_empty() {
        return v;
    }
    let mut out_choices: Vec<Value> = Vec::new();
    for mut c in choices {
        if let Some(d) = c.get("delta").and_then(|d| d.as_object()).cloned() {
            let mut d = d;
            if let Some(fc) = d.get("function_call") {
                let has_name = fc.get("name").and_then(|x| x.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                if !has_name {
                    d.remove("function_call");
                }
            }
            if let Some(tc) = d.get("tool_calls").and_then(|x| x.as_array()) {
                if tc.is_empty() {
                    d.remove("tool_calls");
                }
            }
            for k in ["extra_fields", "refusal", "reasoning_content"] {
                let drop = match d.get(k) {
                    Some(Value::Null) => true,
                    Some(Value::Bool(b)) => !*b,
                    Some(Value::String(s)) => s.is_empty(),
                    Some(Value::Array(a)) => a.is_empty(),
                    Some(Value::Object(o)) => o.is_empty(),
                    _ => false,
                };
                if drop {
                    d.remove(k);
                }
            }
            let has_finish = c
                .get("finish_reason")
                .and_then(|x| x.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if d.is_empty() && !has_finish {
                continue; // 整帧无内容，丢弃
            }
            c["delta"] = Value::Object(d);
        }
        out_choices.push(c);
    }
    v["choices"] = json!(out_choices);
    v
}

/// 解析一行信封 → 内层 chunk / DONE。
fn envelope(data: &str) -> Result<Env, RelayError> {
    let outer: Value = serde_json::from_str(data).map_err(|e| {
        er(502, format!("Qoder 信封帧非 JSON：{e}；{}", trunc(data, 200)), ErrorClass::Transient)
    })?;
    if outer.get("body").is_none() {
        // 直通形态：本来就是内层 chunk
        if outer.get("choices").is_some() {
            return Ok(Env::Chunk(clean_chunk(outer)));
        }
        return Ok(Env::Done);
    }
    let code = outer
        .get("statusCodeValue")
        .and_then(|x| {
            x.as_i64()
                .or_else(|| x.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        })
        .unwrap_or(200);
    let body = outer.get("body").and_then(|b| b.as_str()).unwrap_or("").trim().to_string();
    if code != 200 {
        let cls = env_status_class(code as u16);
        return Err(er(code as u16, format!("Qoder 上游 {code}：{}", trunc(&body, 400)), cls));
    }
    if body.is_empty() {
        return Ok(Env::Done);
    }
    if body == "[DONE]" {
        return Ok(Env::Done);
    }
    let inner: Value = serde_json::from_str(&body).map_err(|e| {
        er(502, format!("Qoder 内层帧非 JSON：{e}；{}", trunc(&body, 200)), ErrorClass::Transient)
    })?;
    Ok(Env::Chunk(clean_chunk(inner)))
}

/// 取 choices[0] 的 (delta, finish_reason)。
fn chunk_parts(v: &Value) -> (Option<Value>, Option<String>) {
    let Some(c) = v.get("choices").and_then(|x| x.as_array()).and_then(|a| a.first()) else {
        return (None, None);
    };
    let delta = c.get("delta").cloned();
    let fr = c
        .get("finish_reason")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    (delta, fr)
}

fn delta_text(delta: &Value, key: &str) -> String {
    delta.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn chunk_model(v: &Value) -> String {
    v.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string()
}

// ══════════════════════════════════════════════════════════════
// 出站请求
// ══════════════════════════════════════════════════════════════

/// 向候选推理主机发一次请求（第一个成功即返回；传输层失败切下一个）。
async fn post_chat(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    cosy: &CosySession,
    body: &str,
    model_key: &str,
    timeout: std::time::Duration,
) -> Result<reqwest::Response, RelayError> {
    let hosts = gateways(spec);
    let mut last: Option<RelayError> = None;
    for host in hosts {
        let url = format!("{host}{CHAT_PATH}");
        let hs = cosy.headers(body, &url, model_key);
        let mut rb = http
            .post(&url)
            .timeout(timeout)
            .body(body.to_string());
        for (k, v) in hs {
            rb = rb.header(k, v);
        }
        match rb.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if (200..300).contains(&status) {
                    return Ok(resp);
                }
                let text = resp.text().await.unwrap_or_default();
                let cls = match status {
                    401 | 403 => ErrorClass::Auth,
                    402 => ErrorClass::Quota,
                    429 => ErrorClass::Rate,
                    400..=499 => ErrorClass::Rejected,
                    _ => ErrorClass::Transient,
                };
                let e = er(status, format!("Qoder 上游 {status}：{}", trunc(&text, 400)), cls);
                if cls != ErrorClass::Transient {
                    return Err(e);
                }
                last = Some(e);
            }
            Err(e) => {
                last = Some(er(
                    502,
                    format!("Qoder 传输失败：{e}"),
                    ErrorClass::Transient,
                ));
            }
        }
    }
    Err(last.unwrap_or_else(|| er(502, "Qoder 无可用推理主机", ErrorClass::Transient)))
}

// ══════════════════════════════════════════════════════════════
// 入口
// ══════════════════════════════════════════════════════════════

/// 非流式：把信封流折叠成一条 chat.completion。
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
    let p = prepare(http, spec, secret, raw).await?;
    let resp = post_chat(
        http,
        spec,
        &p.cosy,
        &p.body,
        &p.model_key,
        std::time::Duration::from_secs(180),
    )
    .await?;
    let text = resp.text().await.unwrap_or_default();

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut finish: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut out_model = req_model.clone();
    let mut frames = 0usize;
    for line in text.lines() {
        let Some(data) = data_of(line) else { continue };
        match envelope(data)? {
            Env::Done => {
                if frames > 0 {
                    break;
                }
            }
            Env::Chunk(c) => {
                frames += 1;
                let m = chunk_model(&c);
                if !m.is_empty() {
                    out_model = m;
                }
                if let Some(u) = c.get("usage") {
                    if !u.is_null() {
                        usage = Some(u.clone());
                    }
                }
                let (delta, fr) = chunk_parts(&c);
                if let Some(d) = &delta {
                    content.push_str(&delta_text(d, "content"));
                    reasoning.push_str(&delta_text(d, "reasoning_content"));
                    if let Some(tcs) = d.get("tool_calls").and_then(|x| x.as_array()) {
                        merge_tool_calls(&mut calls, tcs);
                    }
                }
                if fr.is_some() {
                    finish = fr;
                }
            }
        }
    }
    if frames == 0 {
        return Err(er(
            401,
            format!(
                "Qoder 未返回任何内容（凭据可能已失效）：{}",
                trunc(&text, 300)
            ),
            ErrorClass::Auth,
        ));
    }
    let has_call = !calls.is_empty();
    if content.is_empty() && reasoning.is_empty() && !has_call {
        return Err(er(
            401,
            format!("Qoder 未返回正文（凭据可能已失效）：{}", trunc(&text, 300)),
            ErrorClass::Auth,
        ));
    }
    let mut msg = json!({"role": "", "content": content});
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    if has_call {
        msg["tool_calls"] = json!(calls);
    }
    let out_finish = finish.unwrap_or_else(|| {
        if has_call { "tool_calls".into() } else { "stop".into() }
    });
    let mut out = json!({
        "id": format!("chatcmpl-qd-{}", nanos()),
        "object": "chat.completion",
        "created": secs(),
        "model": if out_model.is_empty() { p.model_key.clone() } else { out_model },
        "choices": [{"index": 0, "message": msg, "finish_reason": out_finish}],
    });
    out["usage"] = match usage {
        Some(u) => u,
        None => json!({
            "prompt_tokens": (p.in_bytes / 4).max(1) as i64,
            "completion_tokens": ((content.chars().count() + reasoning.chars().count()) / 2).max(1) as i64,
        }),
    };
    Ok(out)
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 按 index 合并增量 tool_calls（id/name 取首次，arguments 拼接）。
fn merge_tool_calls(acc: &mut Vec<Value>, tcs: &[Value]) {
    for tc in tcs {
        let idx = tc.get("index").and_then(|x| x.as_i64()).unwrap_or(acc.len() as i64).max(0) as usize;
        while acc.len() <= idx {
            acc.push(json!({"id": "", "type": "function", "function": {"name": "", "arguments": ""}}));
        }
        let slot = &mut acc[idx];
        if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
            if !id.is_empty() && slot["id"].as_str().unwrap_or("").is_empty() {
                slot["id"] = json!(id);
            }
        }
        if let Some(ty) = tc.get("type").and_then(|x| x.as_str()) {
            if !ty.is_empty() {
                slot["type"] = json!(ty);
            }
        }
        if let Some(f) = tc.get("function") {
            if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                if !n.is_empty() && slot["function"]["name"].as_str().unwrap_or("").is_empty() {
                    slot["function"]["name"] = json!(n);
                }
            }
            if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                let cur = slot["function"]["arguments"].as_str().unwrap_or("").to_string();
                slot["function"]["arguments"] = json!(format!("{cur}{a}"));
            }
        }
    }
}
/// 流式：Qoder 信封 SSE → OpenAI chunk 帧。
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
    let p = prepare(http, spec, secret, raw).await?;
    let resp = post_chat(
        http,
        spec,
        &p.cosy,
        &p.body,
        &p.model_key,
        std::time::Duration::from_secs(300),
    )
    .await?;

    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let in_bytes = p.in_bytes;
    let fallback_model = if req_model.is_empty() { p.model_key.clone() } else { req_model };
    tokio::spawn(async move {
        let rid = format!("chatcmpl-qd-{}", nanos());
        let created = secs();
        let model_out = std::sync::Arc::new(std::sync::Mutex::new(fallback_model));
        let mo = model_out.clone();
        let mk = move |delta: Value, finish: Option<&str>| -> String {
            // 中毒时旧实现退回空串：整条流的每个 chunk 的 model 字段都变成 ""，
            // 客户端按 model 分组的逻辑全部错乱。
            let m = mo.lock().unwrap_or_else(|e| e.into_inner()).clone();
            format!(
                "data: {}\n\n",
                json!({
                    "id": rid,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": m,
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
                })
            )
        };
        let mut stat = StreamStat::default();
        let mut out_chars: usize = 0;
        let mut buf = String::new();
        let mut dec = crate::Utf8Decoder::new();
        let mut sent_role = false;
        let mut done_sent = false;
        let mut stream = resp.bytes_stream();
        'outer: while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(b) => b,
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {e}");
                    break 'outer;
                }
            };
            buf.push_str(&dec.push(&chunk));
            let mut out = String::new();
            while let Some((pos, sep)) = crate::find_frame_end(&buf) {
                let block: String = buf.drain(..pos + sep).collect();
                for line in block.lines() {
                    let Some(data) = data_of(line) else { continue };
                    let env = match envelope(data) {
                        Ok(e) => e,
                        Err(e) => {
                            stat.error = Some(e.class);
                            stat.error_msg = e.body.clone();
                            break 'outer;
                        }
                    };
                    let c = match env {
                        Env::Done => {
                            if !done_sent {
                                out.push_str(&mk(json!({}), Some("stop")));
                                stat.finish_reason = Some("stop".into());
                                done_sent = true;
                            }
                            continue;
                        }
                        Env::Chunk(c) => c,
                    };
                    let m = chunk_model(&c);
                    if !m.is_empty() {
                        // 漏记会让用量/日志里的 model 一直是空或旧值（锁中毒时静默）
                        let mut g = model_out.lock().unwrap_or_else(|e| e.into_inner());
                        *g = m;
                    }
                    if let Some(u) = c.get("usage") {
                        if !u.is_null() {
                            if let Some(x) = u.get("prompt_tokens").and_then(|x| x.as_i64()) {
                                stat.tokens_in = x;
                            }
                            if let Some(x) = u.get("completion_tokens").and_then(|x| x.as_i64()) {
                                stat.tokens_out = x;
                            }
                        }
                    }
                    let (delta, fr) = chunk_parts(&c);
                    let Some(delta) = delta else {
                        if let Some(f) = fr {
                            if !done_sent {
                                out.push_str(&mk(json!({}), Some(&f)));
                                stat.finish_reason = Some(f);
                                done_sent = true;
                            }
                        }
                        continue;
                    };
                    let text = delta_text(&delta, "content");
                    let th = delta_text(&delta, "reasoning_content");
                    let has_call = delta
                        .get("tool_calls")
                        .and_then(|x| x.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    if (!text.is_empty() || !th.is_empty() || has_call) && !sent_role {
                        out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
                        sent_role = true;
                    }
                    if !th.is_empty() {
                        out_chars += th.chars().count();
                        out.push_str(&mk(json!({"reasoning_content": th}), None));
                    }
                    if !text.is_empty() {
                        out_chars += text.chars().count();
                        out.push_str(&mk(json!({"content": text}), None));
                    }
                    if has_call {
                        out.push_str(&mk(json!({"tool_calls": delta["tool_calls"].clone()}), None));
                    }
                    if let Some(f) = fr {
                        if !done_sent {
                            out.push_str(&mk(json!({}), Some(&f)));
                            stat.finish_reason = Some(f);
                            done_sent = true;
                        }
                    }
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break 'outer;
            }
        }
        let mut tail = String::new();
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg = "Qoder 未返回正文（凭据可能已失效，请重新走设备授权登录）".into();
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
            stat.finish_reason = Some("stop".into());
        }
        tail.push_str("data: [DONE]\n\n");
        stat.saw_done = true;
        if stat.tokens_in == 0 || stat.tokens_out == 0 {
            let (ti, to) = crate::estimate_stream_tokens(in_bytes, out_chars);
            if stat.tokens_in == 0 {
                stat.tokens_in = ti;
            }
            if stat.tokens_out == 0 {
                stat.tokens_out = to;
            }
        }
        let _ = bytes_tx.send(Ok(Bytes::from(tail))).await;
        let _ = tx.send(stat).await;
    });
    Ok((
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(bytes_rx))
            as std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
        rx,
    ))
}

/// 验活：GET {openapi}/api/v1/userinfo 2xx 即有效。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok(cred) = parse_cred(secret) else {
        return false;
    };
    match fetch_userinfo(http, spec, &cred.token).await {
        Ok(info) => {
            let d = unwrap_data(&info);
            d.get("id").is_some() || d.get("uid").is_some() || d.get("user_type").is_some()
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_toml(base: &str, openapi: &str) -> ProviderSpec {
        let raw = format!(
            r#"
id = "web-qoder"
name = "Qoder"
auth = "api_key"
base_url = "{base}"
format = "openai"
models = ["qwen3.8-max", "dmodel"]
models_policy = "static"
stream_dialect = "sse_delta"
upstream_kind = "qoder"

[headers]
x-qoder-openapi = "{openapi}"
"#
        );
        aigw_registry::parse_spec(&raw).expect("spec")
    }


    /// 起一个 mock：userinfo + 信封 SSE 推理端点。
    async fn spawn_mock(stream_body: &'static str) -> String {
        use axum::routing::{any, post};
        let app = axum::Router::new()
            .route(
                "/api/v1/userinfo",
                any(|| async {
                    axum::Json(json!({
                        "id": "u-1",
                        "user_type": "personal_professional_trial",
                        "organization_id": "org-1",
                        "organization_name": "Org"
                    }))
                }),
            )
            .route(
                "/algo/api/v2/service/pro/sse/agent_chat_generation",
                post(move || async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        stream_body,
                    )
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
    fn parse_cred_accepts_token_and_json() {
        assert_eq!(parse_cred("dt-abc").unwrap().token, "dt-abc");
        let c = parse_cred(r#"{"accessToken":"dt-x","uid":"u-9","userType":"pro"}"#).unwrap();
        assert_eq!(c.token, "dt-x");
        assert_eq!(c.uid, "u-9");
        assert_eq!(c.user_type, "pro");
        assert!(parse_cred("  ").is_err());
    }

    #[test]
    fn resolve_key_handles_display_ids_and_aliases() {
        let spec = spec_toml("http://127.0.0.1:1", "http://127.0.0.1:1");
        assert_eq!(resolve_key(&spec, "qmodel_38max (Qwen3.8-Max)"), "qmodel_38max");
        assert_eq!(resolve_key(&spec, "qwen3.7-max"), "qmodel_latest");
        assert_eq!(resolve_key(&spec, "deepseek-v4-pro"), "dmodel");
        assert_eq!(resolve_key(&spec, "Qwen3.7-Plus"), "qmodel");
        assert_eq!(resolve_key(&spec, ""), "auto");
        // 未知模型原样下发（可诊断）
        assert_eq!(resolve_key(&spec, "my-custom"), "my-custom");
    }

    #[test]
    fn flatten_keeps_system_and_degrades_tools() {
        let v = json!({
            "messages": [
                {"role": "system", "content": "be nice"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "Bash", "arguments": {"cmd": "ls"}}}]},
                {"role": "tool", "tool_call_id": "c1", "name": "Bash", "content": "a.txt"},
            ]
        });
        let (system, flat, _) = flatten_messages(&v, false, false);
        assert_eq!(system.as_deref(), Some("be nice"));
        assert_eq!(flat.len(), 3);
        let asst = &flat[1];
        assert!(asst["content"].as_str().unwrap().contains(LEAK_MARKER));
        let tool = &flat[2];
        assert_eq!(tool["role"], "user");
        assert!(tool["content"].as_str().unwrap().starts_with(TOOL_RESULT_MARKER));
    }

    #[test]
    fn flatten_structured_keeps_tool_role() {
        let v = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "Read", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "ok"},
            ]
        });
        let (_, flat, _) = flatten_messages(&v, false, true);
        assert_eq!(flat[1]["tool_calls"][0]["function"]["name"], "Read");
        assert_eq!(flat[2]["role"], "tool");
        assert_eq!(flat[2]["tool_call_id"], "c1");
    }

    #[test]
    fn build_body_overrides_template_fields() {
        let spec = spec_toml("http://127.0.0.1:1", "http://127.0.0.1:1");
        let cred = QdCred {
            token: "dt-1".into(),
            uid: "u-1".into(),
            user_type: "personal_standard".into(),
            ..Default::default()
        };
        let v = json!({"model": "qwen3.8-max", "messages": [{"role": "user", "content": "你好"}], "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}]});
        let body = build_body(&spec, &cred, &v, "qmodel_38max");
        assert_eq!(body["agent_id"], "agent_common");
        assert_eq!(body["aliyun_user_type"], "personal_standard");
        assert_eq!(body["model_config"]["key"], "qmodel_38max");
        assert_eq!(body["model_config"]["display_name"], "Qwen3.8-Max");
        assert_eq!(body["chat_context"]["text"]["text"], "你好");
        assert_eq!(body["business"]["name"], "你好");
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        // 模板自带的 14 个 Qoder 桌面工具不能泄漏
        assert_eq!(body["tools"][0]["function"]["name"], "f");
        // 模板示例 user 轮次必须丢弃
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn envelope_unwraps_and_cleans() {
        let inner = r#"{"choices":[{"index":0,"delta":{"content":"你好","refusal":false}}],"model":"m-1"}"#;
        let data = serde_json::to_string(&json!({"body": inner, "statusCodeValue": 200})).unwrap();
        match envelope(&data).unwrap() {
            Env::Chunk(c) => {
                assert_eq!(c["choices"][0]["delta"]["content"], "你好");
                assert!(c["choices"][0]["delta"].get("refusal").is_none());
                assert_eq!(chunk_model(&c), "m-1");
            }
            _ => panic!("应当解出 chunk"),
        }
        let done = serde_json::to_string(&json!({"body": "[DONE]", "statusCodeValue": 200})).unwrap();
        assert!(matches!(envelope(&done).unwrap(), Env::Done));
    }

    #[test]
    fn envelope_non_200_becomes_error() {
        let data = serde_json::to_string(&json!({"body": "quota exhausted", "statusCodeValue": 402})).unwrap();
        let e = envelope(&data).unwrap_err();
        assert_eq!(e.status, 402);
        assert!(matches!(e.class, ErrorClass::Quota));
        assert!(e.body.contains("quota exhausted"));
    }

    #[tokio::test]
    async fn qoder_nonstream_via_mock() {
        let sse = concat!(
            "data: {\"body\":\"{\\\"choices\\\":[{\\\"index\\\":0,\\\"delta\\\":{\\\"content\\\":\\\"你好\\\"},\\\"finish_reason\\\":null}]}\",\"statusCodeValue\":200}\n\n",
            "data: {\"body\":\"{\\\"choices\\\":[{\\\"index\\\":0,\\\"delta\\\":{\\\"content\\\":\\\"！\\\"},\\\"finish_reason\\\":\\\"stop\\\"}],\\\"usage\\\":{\\\"prompt_tokens\\\":5,\\\"completion_tokens\\\":2}}\",\"statusCodeValue\":200}\n\n",
            "data: {\"body\":\"[DONE]\",\"statusCodeValue\":200}\n\n"
        );
        let base = spawn_mock(sse).await;
        let spec = spec_toml(&base, &base);
        let http = reqwest::Client::new();
        let body = json!({"model": "qwen3.8-max", "messages": [{"role": "user", "content": "hi"}]});
        let out = chat_nonstream(&http, &spec, "dt-abc", body.to_string().as_bytes())
            .await
            .expect("nonstream");
        assert_eq!(out["choices"][0]["message"]["content"], "你好！");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["prompt_tokens"], 5);
        assert_eq!(out["usage"]["completion_tokens"], 2);
    }

    #[tokio::test]
    async fn qoder_stream_via_mock() {
        let sse = concat!(
            "data: {\"body\":\"{\\\"choices\\\":[{\\\"index\\\":0,\\\"delta\\\":{\\\"content\\\":\\\"Hi\\\"},\\\"finish_reason\\\":null}]}\",\"statusCodeValue\":200}\n\n",
            "data: {\"body\":\"{\\\"choices\\\":[{\\\"index\\\":0,\\\"delta\\\":{\\\"content\\\":\\\"!\\\"},\\\"finish_reason\\\":\\\"stop\\\"}]}\",\"statusCodeValue\":200}\n\n",
            "data: {\"body\":\"[DONE]\",\"statusCodeValue\":200}\n\n"
        );
        let base = spawn_mock(sse).await;
        let spec = spec_toml(&base, &base);
        let http = reqwest::Client::new();
        let body = json!({"model": "dmodel", "messages": [{"role": "user", "content": "hi"}]});
        let (mut s, mut rx) = chat_stream(&http, &spec, "dt-abc", body.to_string().as_bytes())
            .await
            .expect("stream");
        let mut text = String::new();
        while let Some(Ok(b)) = s.next().await {
            text.push_str(&String::from_utf8_lossy(&b));
        }
        assert!(text.contains("Hi"), "{text}");
        assert!(text.contains("data: [DONE]"), "{text}");
        let stat = rx.recv().await.expect("stat");
        assert_eq!(stat.finish_reason.as_deref(), Some("stop"));
        assert!(stat.error.is_none());
    }

    #[tokio::test]
    async fn qoder_verify_hits_userinfo() {
        let base = spawn_mock("").await;
        let spec = spec_toml(&base, &base);
        let http = reqwest::Client::new();
        assert!(verify(&http, &spec, "dt-abc").await);
        assert!(!verify(&http, &spec, "  ").await);
    }
}
