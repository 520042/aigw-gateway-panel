//! 通用 OpenAI 兼容「透明」原生中继（移植自 app/native_relay.py 的 relay_once/relay_stream）。
//!
//! 适用对象：上游本身就是 OpenAI 格式对话接口的平台（小浣熊 llm v2 / Trae api.trae.cn /
//! 未来可扩展的其它 copilot 系）。与豆包 samantha 不同，**请求体直接透传、响应体直接转发**，
//! 面板只负责「凭据注入 + 鉴权头 + Cookie 键过滤 + 流式透明转发」。
//!
//! 鉴权类型（`relay_spec.auth`）：
//! - `cookie`        → 把账号池凭据作为 Cookie 注入；有 `cookie_keys` 时只外发鉴权键
//! - `bearer`        → `Authorization: Bearer <secret>`
//! - `cloudide`      → `Authorization: Cloud-IDE-JWT <secret>`（Trae 系的真实鉴权头）
use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NativeAuth {
    Cookie,
    Bearer,
    CloudIdeJwt,
}

pub struct NativeSpec {
    pub base: String,
    pub path: String,
    pub auth: NativeAuth,
    /// Cookie 鉴权时只外发的键（其余键不外发，避免泄漏/风控）。空 = 原样透传。
    pub cookie_keys: Vec<String>,
}

impl NativeSpec {
    /// 从 catalog 的 relay_spec（JSON）解析；缺 base 视为无效。
    pub fn from_catalog(spec: &Value) -> Option<NativeSpec> {
        let base = spec.get("base").and_then(|v| v.as_str())?;
        let path = spec
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("/chat/completions")
            .to_string();
        let auth = match spec.get("auth").and_then(|v| v.as_str()).unwrap_or("bearer") {
            "cookie" => NativeAuth::Cookie,
            "cloudide" | "cloud-ide-jwt" => NativeAuth::CloudIdeJwt,
            _ => NativeAuth::Bearer,
        };
        let cookie_keys = spec
            .get("cookie_keys")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Some(NativeSpec {
            base: base.to_string(),
            path,
            auth,
            cookie_keys,
        })
    }
}

/// 只保留 Cookie 里鉴权需要的键（移植自 native_relay._filter_cookie）。
/// 一个键都没命中时**不再原样透传**（宁可缺鉴权失败，也不外发无关凭据）。
pub fn filter_cookie(keys: &[String], cookie: &str) -> String {
    if keys.is_empty() || cookie.is_empty() {
        return cookie.to_string();
    }
    let keep: Vec<&str> = cookie
        .split(';')
        .map(|p| p.trim())
        .filter(|p| {
            let k = p.split('=').next().unwrap_or("").trim();
            keys.iter().any(|k2| k2 == k)
        })
        .collect();
    keep.join("; ")
}

fn auth_header(spec: &NativeSpec, secret: &str) -> (String, String) {
    match spec.auth {
        NativeAuth::Cookie => ("cookie".to_string(), filter_cookie(&spec.cookie_keys, secret)),
        NativeAuth::Bearer => ("authorization".to_string(), format!("Bearer {}", secret)),
        NativeAuth::CloudIdeJwt => {
            ("authorization".to_string(), format!("Cloud-IDE-JWT {}", secret))
        }
    }
}

fn url(spec: &NativeSpec) -> String {
    format!("{}{}", spec.base.trim_end_matches('/'), spec.path)
}

/// 把上游 OpenAI SSE 聚合回一个 chat.completion（上游非流式/只返 SSE 时兜底）。
pub fn aggregate_openai_sse(raw: &str) -> Option<Value> {
    let mut txt_parts: Vec<String> = Vec::new();
    let mut model: Option<String> = None;
    let mut cid: Option<String> = None;
    let mut finish: Option<String> = None;
    let mut usage: Option<Value> = None;
    for line in raw.lines() {
        let line = line.trim();
        let Some(chunk) = line.strip_prefix("data:") else { continue };
        let chunk = chunk.trim();
        if chunk == "[DONE]" {
            break;
        }
        let Ok(d) = serde_json::from_str::<Value>(chunk) else {
            continue;
        };
        model = d.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()).or(model);
        cid = d.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()).or(cid);
        if let Some(u) = d.get("usage") {
            usage = Some(u.clone());
        }
        if let Some(choices) = d.get("choices").and_then(|v| v.as_array()) {
            for ch in choices {
                let delta = ch
                    .get("delta")
                    .or_else(|| ch.get("message"))
                    .and_then(|v| v.as_object());
                if let Some(d) = delta {
                    if let Some(c) = d.get("content").and_then(|v| v.as_str()) {
                        txt_parts.push(c.to_string());
                    }
                }
                if let Some(f) = ch.get("finish_reason").and_then(|v| v.as_str()) {
                    finish = Some(f.to_string());
                }
            }
        }
    }
    if model.is_none() && cid.is_none() && txt_parts.is_empty() {
        return None;
    }
    let msg = json!({"role": "assistant", "content": txt_parts.concat()});
    let mut out = json!({
        "id": cid.unwrap_or_else(|| format!("chatcmpl-{:024x}", nanos())),
        "object": "chat.completion",
        "created": nanos() / 1_000_000_000,
        "model": model.unwrap_or_default(),
        "choices": [{"index": 0, "message": msg, "finish_reason": finish.unwrap_or("stop".to_string())}],
    });
    if let Some(u) = usage {
        out["usage"] = u;
    }
    Some(out)
}

/// 非流式聚合（或原样转发 JSON）：返回 OpenAI chat.completion 形状。
/// 上游返回 SSE（即使我们发了 stream=false）时自动聚合；返回 JSON 时原样回。
pub async fn chat(
    spec: &NativeSpec,
    body: &[u8],
    secret: &str,
    timeout: u64,
) -> Result<Value, (i64, String)> {
    let url = url(spec);
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout));
    let (ah_k, ah_v) = auth_header(spec, secret);
    req = req
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("user-agent", "aigw-panel-native-relay/1.0")
        .header(ah_k, ah_v);
    let resp = req.body(body.to_vec()).send().await.map_err(|e| (0, e.to_string()))?;
    let status = resp.status().as_u16() as i64;
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let raw = resp.text().await.map_err(|e| (0, e.to_string()))?;
    if !(200..300).contains(&status) {
        return Err((status, raw.chars().take(400).collect()));
    }
    if ct.contains("event-stream") || raw.trim_start().starts_with("data:") {
        return aggregate_openai_sse(&raw).ok_or((status, "上游 SSE 解析为空".to_string()));
    }
    // 期望 JSON；解析失败说明上游返了非 JSON（如实报错，不算成功）
    serde_json::from_str::<Value>(&raw)
        .map_err(|_| (status, format!("上游返回非 JSON 正文：{}", raw.chars().take(200).collect::<String>())))
}

/// 流式：透明转发上游 OpenAI SSE 帧；上游若返 JSON（未遵守 stream）则包成单帧 + [DONE]。
pub async fn chat_stream(
    spec: &NativeSpec,
    body: &[u8],
    secret: &str,
    timeout: u64,
) -> Result<Response, (i64, String)> {
    let url = url(spec);
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout));
    let (ah_k, ah_v) = auth_header(spec, secret);
    req = req
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("user-agent", "aigw-panel-native-relay/1.0")
        .header(ah_k, ah_v);
    let resp = req.body(body.to_vec()).send().await.map_err(|e| (0, e.to_string()))?;
    let status = resp.status();
    if !(200..300).contains(&status.as_u16()) {
        let t = resp.text().await.unwrap_or_default();
        return Err((status.as_u16() as i64, t.chars().take(400).collect()));
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // 上游返 JSON（没按 stream 来）：包成单帧
    if !ct.contains("event-stream") {
        let raw = resp.text().await.unwrap_or_default();
        let frame = if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            format!("data: {}\n\n", v)
        } else {
            format!(
                "data: {}\n\n",
                json!({"aigw_error": true, "message": "上游返回非 SSE 正文"})
            )
        };
        let mut r = Response::new(Body::from(format!("{}data: [DONE]\n\n", frame)));
        *r.status_mut() = StatusCode::OK;
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        return Ok(r);
    }
    // 上游 SSE：逐帧原样转发
    let stream = resp.bytes_stream().map(|r| {
        r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    });
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    Ok(r)
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
