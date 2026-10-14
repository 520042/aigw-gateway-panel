//! OpenAI 兼容 /v1 端点：models / chat/completions（流式，经 relay 引擎 + 用量记录）
//! / messages（Anthropic 兼容：请求/响应双向转换，含流式 SSE 事件变换）。
use crate::relay;
use crate::server::state::AppState;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{StreamExt, TryStreamExt};
use http::HeaderMap;
use serde_json::{json, Value};

#[tracing::instrument]
pub async fn models() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [
            {"id": "auto", "object": "model", "owned_by": "aigw"},
            {"id": "free", "object": "model", "owned_by": "aigw"},
            {"id": "gpt-oss-20b", "object": "model", "owned_by": "pollinations"},
            {"id": "openai", "object": "model", "owned_by": "pollinations"}
        ]
    }))
}

/// wb-gateway 族原生中继：Plan::NeedLogin 且有凭据时，改写为 Upstream 直打
/// `{base}/v2/chat/completions`（同族同契约：国内 copilot.tencent.com /
/// 国际 www.codebuddy.ai，Bearer JWT；对齐 Python main.py `_WB_BALANCE_HOST`）。
/// 同时把 body 里的 model 剥掉 `@wb-*` 后缀。
fn wb_upgrade(plan: &relay::Plan, secrets: &[String], body: &mut Vec<u8>) -> Option<relay::Plan> {
    let pid = match plan {
        relay::Plan::NeedLogin { pid, .. } if pid.starts_with("wb-gateway") => pid.clone(),
        _ => return None,
    };
    let secret = secrets.first()?;
    let base = match pid.as_str() {
        "wb-gateway" => "https://copilot.tencent.com",
        "wb-gateway-intl" => "https://www.codebuddy.ai",
        _ => return None,
    };
    // body: model 剥 @wb-* 后缀
    if let Ok(mut v) = serde_json::from_slice::<Value>(body) {
        if let Value::Object(ref mut o) = v {
            if let Some(m) = o.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()) {
                let bare = m.rsplit_once('@').map(|(b, _)| b.to_string()).unwrap_or(m);
                o.insert("model".to_string(), Value::String(bare));
            }
        }
        if let Ok(b) = serde_json::to_vec(&v) {
            *body = b;
        }
    }
    Some(relay::Plan::Upstream {
        url: format!("{}/v2/chat/completions", base),
        key: Some(secret.clone()),
    })
}

#[tracing::instrument(skip(st, headers, body))]
pub async fn chat_completions(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let t0 = std::time::Instant::now();
    let cfg = st.inner.settings.read().await.clone();
    let model = relay::model_of(&body);
    let plan = st.inner.registry.resolve(&model, &body, &headers, &cfg);
    let upstream = match &plan {
        relay::Plan::Public { .. } => "public",
        relay::Plan::Upstream { .. } => "upstream",
        relay::Plan::NeedLogin { pid, .. } => pid.as_str(),
        relay::Plan::Native { pid, .. } => pid.as_str(),
        relay::Plan::Web { pid, .. } => pid.as_str(),
    }
    .to_string();

    // 网页版反代（智谱清言 / Trae 国内 / DeepSeek 网页）：私有协议，凭据取账号池
    if let relay::Plan::Web { pid, model: dm } = &plan {
        let secrets = st.inner.login.secrets_for(pid);
        if secrets.is_empty() {
            st.inner.usage.record(pid, &model, 0, 0, 0, false, pid);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "ok": false, "code": "need_login",
                    "message": format!("{} 网页反代需要先在「接入源」登录获取凭据；或先用 auto/free 免费车道", pid),
                    "pid": pid,
                })),
            )
                .into_response();
        }
        let msgs: Vec<Value> = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("messages").and_then(|m| m.as_array()).cloned())
            .unwrap_or_default();
        let stream = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
            .unwrap_or(false);
        if stream {
            match relay::web::chat(pid, &msgs, &secrets[0], dm, 180).await {
                Ok(payload) => {
                    st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, true, pid);
                    return relay::web::envelope_to_sse(&payload);
                }
                Err((status, msg)) => {
                    st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "error": {"message": format!("{} 网页反代失败（HTTP {}）：{}", pid, status, msg),
                                      "type": "aigw_web_relay_error"},
                            "pid": pid,
                        })),
                    )
                        .into_response();
                }
            }
        }
        match relay::web::chat(pid, &msgs, &secrets[0], dm, 180).await {
            Ok(payload) => {
                let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                st.inner.usage.record(pid, &model, tin, tout, t0.elapsed().as_millis() as i64, true, pid);
                return (StatusCode::OK, Json(payload)).into_response();
            }
            Err((status, msg)) => {
                st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "error": {"message": format!("{} 网页反代失败（HTTP {}）：{}", pid, status, msg),
                                  "type": "aigw_web_relay_error"},
                        "pid": pid,
                    })),
                )
                    .into_response();
            }
        }
    }

    // 原生中继（凭据轮询账号池）：豆包 samantha + 通用 OpenAI 透明（小浣熊/Trae）
    if let relay::Plan::Native { pid, model: dm } = &plan {
        let secrets = st.inner.login.secrets_for(pid);
        if secrets.is_empty() {
            st.inner.usage.record(pid, &model, 0, 0, 0, false, pid);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "ok": false, "code": "need_login",
                    "message": format!("{} 原生中继需要先在「接入源」登录获取凭据；或先用 auto/free 免费车道", pid),
                    "pid": pid,
                })),
            )
                .into_response();
        }
        // 共享：请求是否流式（豆包 / 元宝 / 通用透明中继都用到）
        let stream = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
            .unwrap_or(false);
        // 豆包走专用 samantha 协议
        if pid == "apk-doubao" {
            let msgs: Vec<Value> = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v.get("messages").and_then(|m| m.as_array()).cloned())
                .unwrap_or_default();
            if stream {
                match relay::doubao::chat_stream(&msgs, &secrets[0], dm, 180).await {
                    Ok(resp) => {
                        st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, true, pid);
                        return resp;
                    }
                    Err((status, msg)) => {
                        st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "error": {"message": format!("豆包原生中继失败（HTTP {}）：{}", status, msg),
                                          "type": "aigw_native_relay_error"},
                                "pid": pid,
                            })),
                        )
                            .into_response();
                    }
                }
            }
            match relay::doubao::chat(&msgs, &secrets[0], dm, 180).await {
                Ok(payload) => {
                    let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                    let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                    st.inner.usage.record(pid, &model, tin, tout, t0.elapsed().as_millis() as i64, true, pid);
                    return (StatusCode::OK, Json(payload)).into_response();
                }
                Err((status, msg)) => {
                    st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "error": {"message": format!("豆包原生中继失败（HTTP {}）：{}", status, msg),
                                      "type": "aigw_native_relay_error"},
                            "pid": pid,
                        })),
                    )
                        .into_response();
                }
            }
        }
        // 元宝多步私有协议原生中继（建会话 + 流式对话）
        if pid == "apk-yuanbao" {
            let msgs: Vec<Value> = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v.get("messages").and_then(|m| m.as_array()).cloned())
                .unwrap_or_default();
            if stream {
                match relay::yuanbao::chat_stream(&msgs, &secrets[0], dm, 180).await {
                    Ok(resp) => {
                        st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, true, pid);
                        return resp;
                    }
                    Err((status, msg)) => {
                        st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "error": {"message": format!("元宝原生中继失败（HTTP {}）：{}", status, msg),
                                          "type": "aigw_native_relay_error"},
                                "pid": pid,
                            })),
                        )
                            .into_response();
                    }
                }
            }
            match relay::yuanbao::chat(&msgs, &secrets[0], dm, 180).await {
                Ok(payload) => {
                    let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                    let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                    st.inner.usage.record(pid, &model, tin, tout, t0.elapsed().as_millis() as i64, true, pid);
                    return (StatusCode::OK, Json(payload)).into_response();
                }
                Err((status, msg)) => {
                    st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "error": {"message": format!("元宝原生中继失败（HTTP {}）：{}", status, msg),
                                      "type": "aigw_native_relay_error"},
                            "pid": pid,
                        })),
                    )
                        .into_response();
                }
            }
        }
        // 通用 OpenAI 兼容原生中继（小浣熊 cookie / Trae Cloud-IDE-JWT）
        let spec = st
            .inner
            .catalog
            .relay_spec(pid)
            .cloned()
            .and_then(|v| relay::native::NativeSpec::from_catalog(&v));
        let spec = match spec {
            Some(s) => s,
            None => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "ok": false, "code": "no_spec",
                        "message": format!("平台 {} 没有原生中继规格（relay_spec 缺失）", pid),
                        "pid": pid,
                    })),
                )
                    .into_response();
            }
        };
        let stream = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
            .unwrap_or(false);
        if stream {
            match relay::native::chat_stream(&spec, &body, &secrets[0], 180).await {
                Ok(resp) => {
                    st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, true, pid);
                    return resp;
                }
                Err((status, msg)) => {
                    st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "error": {"message": format!("{} 原生中继失败（HTTP {}）：{}", pid, status, msg),
                                      "type": "aigw_native_relay_error"},
                            "pid": pid,
                        })),
                    )
                        .into_response();
                }
            }
        }
        match relay::native::chat(&spec, &body, &secrets[0], 180).await {
            Ok(payload) => {
                let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                st.inner.usage.record(pid, &model, tin, tout, t0.elapsed().as_millis() as i64, true, pid);
                return (StatusCode::OK, Json(payload)).into_response();
            }
            Err((status, msg)) => {
                st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "error": {"message": format!("{} 原生中继失败（HTTP {}）：{}", pid, status, msg),
                                  "type": "aigw_native_relay_error"},
                        "pid": pid,
                    })),
                )
                    .into_response();
            }
        }
    }

    // wb-gateway 族：有凭据 → 升级为 Bearer 直打
    let mut body_v = body.to_vec();
    let orig_stream = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    let plan = if let Some(up) = wb_upgrade(&plan, &st.inner.login.secrets_for(upstream.trim()), &mut body_v) {
        up
    } else {
        plan
    };

    // copilot 系上游**只收流式**（实测非流式 400/11101）——客户端要非流式时强制转流式，
    // 执行后把 SSE 聚合回完整的 chat.completion（对齐 Python native_relay stream_only）。
    let wb_force_stream = matches!(plan, relay::Plan::Upstream { .. }) && !orig_stream;
    if wb_force_stream {
        if let Ok(mut v) = serde_json::from_slice::<Value>(&body_v) {
            if let Value::Object(ref mut o) = v {
                o.insert("stream".to_string(), Value::Bool(true));
            }
            if let Ok(b) = serde_json::to_vec(&v) {
                body_v = b;
            }
        }
    }

    let resp = relay::execute(plan, body_v, &headers, &cfg).await;
    if wb_force_stream {
        match relay::aggregate_sse_response(resp).await {
            Ok(payload) => {
                let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                st.inner
                    .usage
                    .record(&model, &model, tin, tout, t0.elapsed().as_millis() as i64, true, &model);
                return (StatusCode::OK, Json(payload)).into_response();
            }
            Err((status, msg)) => {
                st.inner
                    .usage
                    .record(&model, &model, 0, 0, t0.elapsed().as_millis() as i64, false, &model);
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "error": {"message": format!("wb 原生中继失败（HTTP {}）：{}", status, msg),
                                  "type": "aigw_native_relay_error"}
                    })),
                )
                    .into_response();
            }
        }
    }
    record_usage(&st, &model, &upstream, t0, resp).await
}

/// 用量记录：非流式取上游真实 usage token 后原样重建响应；流式/错误记请求级。
async fn record_usage(
    st: &AppState,
    model: &str,
    upstream: &str,
    t0: std::time::Instant,
    resp: Response,
) -> Response {
    let ms = t0.elapsed().as_millis() as i64;
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let ok = (200..300).contains(&status);
    if !ok || ct.contains("event-stream") {
        st.inner
            .usage
            .record(upstream, model, 0, 0, ms, ok, upstream);
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let bytes = match http_body_util::BodyExt::collect(body).await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            st.inner.usage.record(upstream, model, 0, 0, ms, false, upstream);
            return Response::from_parts(parts, Body::empty());
        }
    };
    let (tin, tout) = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => (
            v.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
            v.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
        ),
        Err(_) => (0, 0),
    };
    st.inner.usage.record(upstream, model, tin, tout, ms, ok, upstream);
    Response::from_parts(parts, Body::from(bytes))
}

// ────────────────────────────────────────────────────────────
// Anthropic /v1/messages 兼容（请求 OpenAI 化 → relay → 响应 Anthropic 化）
// ────────────────────────────────────────────────────────────

#[tracing::instrument(skip(st, headers, body))]
pub async fn messages(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let t0 = std::time::Instant::now();
    let cfg = st.inner.settings.read().await.clone();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("auto").to_string();
    let stream = v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    // 1) Anthropic → OpenAI 请求体
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(sys) = v.get("system") {
        let sys_text = match sys {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !sys_text.is_empty() {
            msgs.push(json!({"role": "system", "content": sys_text}));
        }
    }
    if let Some(arr) = v.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            msgs.push(json!({"role": role, "content": content}));
        }
    }
    let max_tokens = v.get("max_tokens").and_then(|m| m.as_u64()).unwrap_or(4096);
    let openai_body = json!({
        "model": model,
        "messages": msgs,
        "max_tokens": max_tokens,
        "stream": stream,
    });
    let openai_bytes = serde_json::to_vec(&openai_body).unwrap_or_default();

    // 2) 同一 relay 引擎（豆包原生模型走原生中继）
    if crate::relay::doubao::is_doubao_model(&model) {
        let secrets = st.inner.login.secrets_for("apk-doubao");
        if secrets.is_empty() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"type":"error","error":{"type":"invalid_request_error",
                    "message":"豆包原生中继需要先在「接入源」登录（粘贴 Cookie）"}})),
            )
                .into_response();
        }
        if stream {
            match relay::doubao::chat_stream(&msgs, &secrets[0], &model, 180).await {
                Ok(r) => return anthropic_sse(r).await,
                Err((s, m)) => {
                    return (StatusCode::BAD_GATEWAY, Json(json!({"type":"error",
                        "error":{"type":"api_error","message":format!("豆包中继失败（HTTP {}）：{}", s, m)}})))
                        .into_response();
                }
            }
        }
        return match relay::doubao::chat(&msgs, &secrets[0], &model, 180).await {
            Ok(payload) => {
                let text = payload.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                Json(json!({
                    "id": format!("msg_{:x}", nanos()),
                    "type": "message", "role": "assistant", "model": model,
                    "content": [{"type": "text", "text": text}],
                    "stop_reason": "end_turn", "stop_sequence": null,
                    "usage": {"input_tokens": tin, "output_tokens": tout},
                }))
                .into_response()
            }
            Err((s, m)) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({"type":"error","error":{"type":"api_error",
                    "message":format!("豆包中继失败（HTTP {}）：{}", s, m)}})),
            )
                .into_response(),
        };
    }
    let plan = st.inner.registry.resolve(&model, &openai_bytes, &headers, &cfg);
    // 通用 OpenAI 原生中继（小浣熊/Trae）也支持 Anthropic 入站：openai_bytes 已是
    // 标准 OpenAI 形状，直接走 native::chat / chat_stream。
    if let relay::Plan::Native { pid, .. } = &plan {
        if matches!(pid.as_str(), "apk-raccoon" | "apk-trae") {
            let secrets = st.inner.login.secrets_for(pid);
            if secrets.is_empty() {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"type":"error","error":{"type":"invalid_request_error",
                        "message": format!("{} 原生中继需要先在「接入源」登录获取凭据", pid)}})),
                ).into_response();
            }
            let spec = st
                .inner
                .catalog
                .relay_spec(pid)
                .cloned()
                .and_then(|v| relay::native::NativeSpec::from_catalog(&v));
            if let Some(spec) = spec {
                if stream {
                    match relay::native::chat_stream(&spec, &openai_bytes, &secrets[0], 180).await {
                        Ok(r) => return anthropic_sse(r).await,
                        Err((s, m)) => {
                            return (StatusCode::BAD_GATEWAY, Json(json!({"type":"error",
                                "error":{"type":"api_error","message":format!("{} 中继失败（HTTP {}）：{}", pid, s, m)}})))
                                .into_response();
                        }
                    }
                }
                return match relay::native::chat(&spec, &openai_bytes, &secrets[0], 180).await {
                    Ok(payload) => {
                        let text = payload.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                        let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                        let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                        Json(json!({
                            "id": format!("msg_{:x}", nanos()),
                            "type": "message", "role": "assistant", "model": model,
                            "content": [{"type": "text", "text": text}],
                            "stop_reason": "end_turn", "stop_sequence": null,
                            "usage": {"input_tokens": tin, "output_tokens": tout},
                        })).into_response()
                    }
                    Err((s, m)) => (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({"type":"error","error":{"type":"api_error",
                            "message":format!("{} 中继失败（HTTP {}）：{}", pid, s, m)}})),
                    ).into_response(),
                };
            }
        }
    }
    let resp = relay::execute(plan, openai_bytes, &headers, &cfg).await;
    let ms = t0.elapsed().as_millis() as i64;
    let status = resp.status();

    if !(200..300).contains(&status.as_u16()) {
        st.inner.usage.record("anthropic", &model, 0, 0, ms, false, "messages");
        return resp;
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if stream && ct.contains("event-stream") {
        // 3a) 流式：OpenAI SSE → Anthropic SSE
        st.inner.usage.record("anthropic", &model, 0, 0, ms, true, "messages");
        return anthropic_sse(resp).await;
    }
    // 3b) 非流式：收集 JSON → Anthropic message
    let (parts, resp_body) = resp.into_parts();
    let bytes = match http_body_util::BodyExt::collect(resp_body).await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"type":"error","error":{"type":"api_error","message": e.to_string()}})),
            )
                .into_response();
        }
    };
    let upstream: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let text = upstream
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let tin = upstream.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
    let tout = upstream.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
    st.inner.usage.record("anthropic", &model, tin, tout, ms, true, "messages");
    Json(json!({
        "id": format!("msg_{:x}", nanos()),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": tin, "output_tokens": tout},
    }))
    .into_response()
}

/// OpenAI SSE → Anthropic SSE（message_start / content_block_delta / message_stop）。
async fn anthropic_sse(resp: Response) -> Response {
    let status = resp.status();
    let stream = resp
        .into_body()
        .into_data_stream()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
    let st = AnthropicSseState::default();
    let transformed = futures_util::stream::unfold(
        (stream, st, String::new(), false),
        |(mut up, mut st, mut buf, mut done)| async move {
            if done {
                return None;
            }
            let mut out = String::new();
            loop {
                // 先消费缓冲里已完整的 SSE 事件（\n\n 分隔）
                while let Some(pos) = buf.find("\n\n") {
                    let event: String = buf.drain(..pos + 2).collect();
                    out.push_str(&st.convert(&event));
                    if st.finished {
                        return Some((
                            Ok::<Bytes, Box<dyn std::error::Error + Send + Sync>>(Bytes::from(out)),
                            (up, st, buf, true),
                        ));
                    }
                }
                match up.next().await {
                    Some(Ok(chunk)) => {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    Some(Err(e)) => return Some((Err(e), (up, st, buf, true))),
                    None => {
                        out.push_str(&st.finish());
                        return Some((Ok(Bytes::from(out)), (up, st, buf, true)));
                    }
                }
                if !out.is_empty() {
                    return Some((Ok(Bytes::from(out)), (up, st, buf, false)));
                }
            }
        },
    );
    let mut resp = Response::new(Body::from_stream(transformed));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    resp
}

#[derive(Default)]
struct AnthropicSseState {
    started: bool,
    finished: bool,
}

impl AnthropicSseState {
    fn message_start(&self) -> String {
        format!(
            "event: message_start\ndata: {}\n\n",
            json!({"type":"message_start","message":{
                "id": format!("msg_{:x}", nanos()),
                "type":"message","role":"assistant","model":"",
                "content":[],"stop_reason":null,
                "usage":{"input_tokens":0,"output_tokens":0}}})
        )
    }
    fn block_start(&self) -> String {
        format!(
            "event: content_block_start\ndata: {}\n\n",
            json!({"type":"content_block_start","index":0,
                   "content_block":{"type":"text","text":""}})
        )
    }

    /// 转换单个 OpenAI SSE 事件为 Anthropic 事件串。
    fn convert(&mut self, event: &str) -> String {
        let mut out = String::new();
        for line in event.lines() {
            let Some(data) = line.strip_prefix("data: ") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                out.push_str(&self.finish());
                return out;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            let text = v
                .pointer("/choices/0/delta/content")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if !self.started {
                self.started = true;
                out.push_str(&self.message_start());
                out.push_str(&self.block_start());
            }
            if !text.is_empty() {
                out.push_str(&format!(
                    "event: content_block_delta\ndata: {}\n\n",
                    json!({"type":"content_block_delta","index":0,
                           "delta":{"type":"text_delta","text":text}})
                ));
            }
        }
        out
    }

    fn finish(&mut self) -> String {
        if self.finished {
            return String::new();
        }
        self.finished = true;
        let mut out = String::new();
        if !self.started {
            self.started = true;
            out.push_str(&self.message_start());
            out.push_str(&self.block_start());
        }
        out.push_str(&format!(
            "event: content_block_stop\ndata: {}\n\n",
            json!({"type":"content_block_stop","index":0})
        ));
        out.push_str(&format!(
            "event: message_delta\ndata: {}\n\n",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},
                   "usage":{"output_tokens":0}})
        ));
        out.push_str(&format!(
            "event: message_stop\ndata: {}\n\n",
            json!({"type":"message_stop"})
        ));
        out
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
