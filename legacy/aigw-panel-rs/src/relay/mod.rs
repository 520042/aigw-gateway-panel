//! 中继执行引擎：把 [`crate::core::model::Plan`] 落为真实 HTTP 转发（流式）。
//!
//! - auto / free                  → Pollinations 免费车道（gpt-oss-20b，2026-10-14 实测可用）
//! - doubao-* / @apk-doubao       → 豆包 samantha 原生中继（凭据：账号池 apk-doubao）
//! - @需要登录的平台              → 返回 need_login 结构
//! - 配置了 upstream_base          → 透传用户自有 OpenAI 兼容上游
//!
//! 解析（Plan 生成）由 [`crate::platforms::Registry::resolve`] 负责，本模块只负责「执行」。
pub mod doubao;
pub mod loomy;
pub mod native;
pub mod web;
pub mod yuanbao;
pub use crate::core::model::Plan;

pub const POLLINATIONS_BASE: &str = "https://text.pollinations.ai";
pub const POLLINATIONS_PATH: &str = "/openai/chat/completions";
const RELAY_TIMEOUT: u64 = 25;

use crate::core::config::Settings;
use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use http::HeaderMap;
use serde_json::{json, Value};

/// 出站客户端（2026-10-16 S3：超时**四项分设**）。
///
/// 原来的实现是一个 `.timeout(N)` 管到底 —— 对流式响应那是**总时长**，
/// 一个超过 N 秒的长回答会被拦腰砍断（客户端只收到半截 SSE）。现在：
///
/// | 项 | 值 | 说明 |
/// |---|---|---|
/// | connect | 10s | 建连（`connect_timeout`） |
/// | 首包 TTFB | 90s | `read_timeout` 在等响应头期间同样生效 |
/// | 包间 idle | 90s | `read_timeout` 每次成功读取后重置 |
/// | 总时长 | 非流式 180s / **流式不设** | 长会话可能持续数分钟 |
///
/// `no_proxy` 只为网页反代（`web`）开启：那类请求带完整登录 Cookie，
/// 不能泄露给系统代理；原生中继走默认代理策略（与改动前一致）。
pub fn out_client(stream: bool, total_s: u64, no_proxy: bool) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(90));
    if no_proxy {
        b = b.no_proxy();
    }
    if !stream {
        b = b.timeout(std::time::Duration::from_secs(total_s));
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 执行中继计划，返回（流式或错误）响应。
pub async fn execute(plan: Plan, mut body: Vec<u8>, headers: &HeaderMap, cfg: &Settings) -> Response {
    // 必要时覆盖 model 字段后重序列化
    if let Plan::Public {
        model_override: Some(m),
        ..
    } = &plan
    {
        if let Ok(mut v) = serde_json::from_slice::<Value>(&body) {
            if let Value::Object(ref mut o) = v {
                o.insert("model".to_string(), Value::String(m.clone()));
            }
            if let Ok(b) = serde_json::to_vec(&v) {
                body = b;
            }
        }
    }

    let (url, auth) = match &plan {
        Plan::Public { url, .. } => (url.clone(), None),
        Plan::Upstream { url, key } => {
            // key 优先级：Plan 内置（自定义 API）> 客户端 Bearer > 设置 upstream_key
            let a = key
                .clone()
                .map(|k| format!("Bearer {}", k))
                .or_else(|| {
                    headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    if cfg.upstream_key.is_empty() {
                        None
                    } else {
                        Some(format!("Bearer {}", cfg.upstream_key))
                    }
                });
            (url.clone(), a)
        }
        Plan::NeedLogin { pid, name } => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({
                    "ok": false,
                    "code": "need_login",
                    "message": format!("平台 {} 需要先在「接入源」登录获取凭据；请先用 auto/free 免费车道或配置自有上游", name),
                    "pid": pid,
                })),
            )
                .into_response();
        }
        // Native 在 handler 层分流（需要账号池凭据）；走到这里如实报未接凭据
        Plan::Native { pid, .. } => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({
                    "ok": false, "code": "need_login",
                    "message": format!("平台 {} 需要先登录获取凭据（原生中继）", pid),
                    "pid": pid,
                })),
            )
                .into_response();
        }
        // Web 反代同样在 handler 层分流；走到这里如实报未接凭据
        Plan::Web { pid, .. } => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({
                    "ok": false, "code": "need_login",
                    "message": format!("平台 {} 需要先登录获取凭据（网页反代）", pid),
                    "pid": pid,
                })),
            )
                .into_response();
        }
    };

    // ★ S3：流式语义 —— connect 10s / idle 90s / 不设总时长
    let mut builder = out_client(true, RELAY_TIMEOUT, false)
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(a) = auth {
        builder = builder.header(header::AUTHORIZATION, a);
    }
    if let Some(v) = headers.get("x-aigw-strategy") {
        builder = builder.header("X-AIGW-Strategy", v.clone());
    }
    let req = builder.body(body);

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let ct = resp.headers().get(header::CONTENT_TYPE).cloned();
            let stream = resp.bytes_stream().map(|r| {
                r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            });
            let mut b = Response::new(Body::from_stream(stream));
            *b.status_mut() = status;
            if let Some(ct) = ct {
                b.headers_mut().insert(header::CONTENT_TYPE, ct);
            }
            b
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(json!({
                "error": {"message": e.to_string(), "type": "aigw_upstream_error"}
            })),
        )
            .into_response(),
    }
}

/// 给 chat handler 用的便捷入口（解析 model 字段）。
pub fn model_of(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| "auto".to_string())
}

/// 把上游 SSE（OpenAI 格式 `data:` 帧）聚合回一个 chat.completion；上游返 JSON 则原样回。
/// 用于上游**只收流式**（copilot 系 11101）时，把客户端要求的非流式请求转流式后还原。
pub async fn aggregate_sse_response(resp: Response) -> Result<Value, (i64, String)> {
    let status = resp.status().as_u16() as i64;
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (parts, body) = resp.into_parts();
    let bytes = match http_body_util::BodyExt::collect(body).await {
        Ok(c) => c.to_bytes(),
        Err(e) => return Err((status, e.to_string())),
    };
    let _ = parts;
    let raw = String::from_utf8_lossy(&bytes).to_string();
    if !(200..300).contains(&status) {
        return Err((status, raw.chars().take(400).collect()));
    }
    if ct.contains("event-stream") || raw.trim_start().starts_with("data:") {
        return crate::relay::native::aggregate_openai_sse(&raw)
            .ok_or((status, "上游 SSE 解析为空".to_string()));
    }
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| (status, format!("上游返回非 JSON 正文：{}", raw.chars().take(200).collect::<String>())))
}
