//! Loomy（讯飞桌面 AI）模型网关原生中继。
//!
//! 协议（社区接口参考 xxhhlk/loomy2api + Patrick130306/loomy2api，逐条对齐官方桌面客户端）：
//!   推理 baseURL : https://loomyad.xunfei.cn/api/v1
//!   POST {base}/chat/completions（OpenAI 兼容）
//!   GET  {base}/models（OpenAI 兼容）
//!   鉴权头：Authorization: Bearer <session> + token: <session>（双写）
//!           traceparent: 00-<32hex>-<16hex>-01（★ 必需，缺失会挂起到超时）
//!           loomy-version: 0.9.38
//!   模型标识形态：provider/model，例 imodel/spark-x、imodel/doubao-seed-2.0-mini
//!   响应：非流式 choices[].message.{reasoning_content,content}；流式标准 OpenAI SSE
//!
//! 凭据：账号池 apk-loomy 的 session（14 天有效，无 refreshToken）。
use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde_json::{json, Value};

const MODEL_BASE: &str = "https://loomyad.xunfei.cn/api/v1";
const LOOMY_VERSION: &str = "0.9.38";
const DEFAULT_MODEL: &str = "imodel/spark-x";

/// 默认模型（前端裸名 @apk-loomy 时用）。
pub fn default_model() -> &'static str {
    DEFAULT_MODEL
}

fn nonce_hex(n: usize) -> String {
    // 伪随机 hex：nanos 混合进程 id 的 LCG（traceparent/幂等足够）
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15);
    let mut s = String::with_capacity(n);
    for _ in 0..n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s.push(char::from_digit(((x >> 33) % 16) as u32, 16).unwrap());
    }
    s
}

/// 模型网关请求头（每次调用 traceparent 都要新的）。
fn gateway_headers(session: &str) -> Vec<(String, String)> {
    vec![
        ("Authorization".to_string(), format!("Bearer {}", session)),
        ("token".to_string(), session.to_string()),
        (
            "traceparent".to_string(),
            format!("00-{}-{}-01", nonce_hex(32), nonce_hex(16)),
        ),
        ("loomy-version".to_string(), LOOMY_VERSION.to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ]
}

fn apply_headers(mut req: reqwest::RequestBuilder, hs: &[(String, String)]) -> reqwest::RequestBuilder {
    for (k, v) in hs {
        req = req.header(k.as_str(), v.as_str());
    }
    req
}

/// GET /api/v1/models → 实时模型目录。
pub async fn models(session: &str, timeout: u64) -> (u16, Value) {
    let client = super::out_client(false, timeout, false);
    let req = apply_headers(client.get(format!("{}/models", MODEL_BASE)), &gateway_headers(session));
    match req.send().await {
        Ok(r) => {
            let code = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            let v: Value = serde_json::from_str(&txt)
                .unwrap_or(json!({"_http": code, "_raw": txt.chars().take(300).collect::<String>()}));
            (code, v)
        }
        Err(e) => (0, json!({"_http": 0, "_err": e.to_string()})),
    }
}

/// 把模型目录归一成前端平台抽屉要的行（id/name/desc）。
pub async fn model_rows(session: &str, timeout: u64) -> Vec<Value> {
    let (code, v) = models(session, timeout).await;
    if code != 200 {
        return Vec::new();
    }
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| v.as_array())
        .cloned()
        .unwrap_or_default();
    arr.iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|x| x.as_str())?;
            Some(json!({
                "id": id,
                "name": m.get("name").and_then(|x| x.as_str()).unwrap_or(id),
                "desc": m.get("description").or_else(|| m.get("desc")).and_then(|x| x.as_str()).unwrap_or("Loomy 模型网关"),
                "on": true,
            }))
        })
        .collect()
}

/// 非流式对话：返回 OpenAI chat.completion（上游返 SSE 时聚合）。
pub async fn chat(
    messages: &[Value],
    session: &str,
    model: &str,
    timeout: u64,
) -> Result<Value, (i64, String)> {
    let body = json!({
        "model": if model.is_empty() { DEFAULT_MODEL } else { model },
        "messages": messages,
        "stream": false,
        "enable_thinking": true,
        "chat_template_kwargs": {"enable_thinking": true},
    });
    // 非流式：总时长 timeout（+ connect 10s / read 90s，见 out_client）
    let client = super::out_client(false, timeout, false);
    let req = apply_headers(
        client.post(format!("{}/chat/completions", MODEL_BASE)),
        &gateway_headers(session),
    )
    .body(body.to_string());
    let resp = req.send().await.map_err(|e| (0, e.to_string()))?;
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
        return crate::relay::native::aggregate_openai_sse(&raw)
            .ok_or((status, "Loomy SSE 解析为空".to_string()));
    }
    let v: Value = serde_json::from_str(&raw)
        .map_err(|_| (status, format!("Loomy 返回非 JSON：{}", raw.chars().take(200).collect::<String>())))?;
    // ★ 200 + 业务码 = 失败（失效 session 实测 {"code":"100002","desc":"登录已失效"}）。
    //   不识别就会被号池治理当成成功，失效凭据永远拉黑不掉。
    if let Some(e) = biz_err(&v) {
        return Err(e);
    }
    Ok(v)
}

/// Loomy 业务错误码识别（2026-10-16 S3）。
/// 成功响应是标准 OpenAI chat.completion（无 `code` 字段）；
/// 失败是 `{"code":"100002","data":{},"desc":"登录已失效，请重新登录"}`。
/// 返回的 status 决定号池治理怎么冻：401 → 账号级拉黑，400 → 模型级冷却。
fn biz_err(v: &Value) -> Option<(i64, String)> {
    let code = v.get("code")?;
    let s = match code {
        Value::String(x) => x.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if s.is_empty() || s == "0" || s == "200" {
        return None;
    }
    let desc = v
        .get("desc")
        .or_else(|| v.get("message"))
        .and_then(|x| x.as_str())
        .unwrap_or("未知错误");
    let auth = desc.contains("登录")
        || desc.contains("失效")
        || desc.contains("授权")
        || desc.contains("token")
        || desc.contains("Token");
    Some((
        if auth { 401 } else { 400 },
        format!("Loomy 业务错误 {}：{}", s, desc),
    ))
}

/// 流式对话：透明转发上游 OpenAI SSE（上游返 JSON 时包成单帧 + [DONE]）。
pub async fn chat_stream(
    messages: &[Value],
    session: &str,
    model: &str,
    timeout: u64,
) -> Result<Response, (i64, String)> {
    let body = json!({
        "model": if model.is_empty() { DEFAULT_MODEL } else { model },
        "messages": messages,
        "stream": true,
        "enable_thinking": true,
        "chat_template_kwargs": {"enable_thinking": true},
    });
    let _ = timeout; // 流式：不设总时长（S3 四项分设）
    let client = super::out_client(true, 0, false);
    let req = apply_headers(
        client.post(format!("{}/chat/completions", MODEL_BASE)),
        &gateway_headers(session),
    )
    .body(body.to_string());
    let resp = req.send().await.map_err(|e| (0, e.to_string()))?;
    let status = resp.status().as_u16() as i64;
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !(200..300).contains(&status) {
        let raw = resp.text().await.unwrap_or_default();
        return Err((status, raw.chars().take(400).collect()));
    }
    if ct.contains("event-stream") {
        let stream = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>));
        let mut b = Response::new(Body::from_stream(stream));
        *b.status_mut() = StatusCode::OK;
        b.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        return Ok(b);
    }
    // 上游没遵守 stream：聚合成单帧 + [DONE]
    let raw = resp.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
    // ★ 200 + 业务码 = 失败：不能包成假 SSE 帧骗客户端（也骗不过号池治理）
    if let Some(e) = biz_err(&payload) {
        return Err(e);
    }
    let mut sse = String::new();
    let chunk = json!({
        "id": payload.get("id").cloned().unwrap_or(json!("chatcmpl-loomy")),
        "object": "chat.completion.chunk",
        "created": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "model": if model.is_empty() { DEFAULT_MODEL } else { model },
        "choices": [{"index": 0, "delta": {"role": "assistant",
            "content": payload.pointer("/choices/0/message/content").and_then(|v| v.as_str()).unwrap_or("")},
            "finish_reason": "stop"}],
    });
    sse.push_str(&format!("data: {}\n\n", chunk));
    sse.push_str("data: [DONE]\n\n");
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")],
        sse,
    )
        .into_response())
}
