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

/// 号池治理中继宏（2026-10-16，S3；治理规则见 `server/pool.rs`）。
///
/// **为什么是宏而不是 `async fn` + 闭包**：闭包返回的 future 借用入参 `&str`，
/// 而 `FnMut(&str) -> Fut` 要求 `Fut` 与入参生命周期无关（Rust 没有 lending
/// closure）—— 写成函数必然撞 E0515。宏把循环体原地展开，借用检查自然通过。
///
/// 行为：按「紧迫度 → RecentPick → last_used」排序，跳过冷却 / WAF 拉黑 /
/// 在途已满的账号；成功即返回并 `mark_ok`；失败 `mark_fail`（401/403 账号级
/// 拉黑）后换下一个（单次请求最多 3 个）；全部被挡住 → 半开最紧迫的一个。
/// ★ 只读内存，绝不打上游余额/探测接口。
///
/// 用法（`$call` = 用 `$sec` 打一次上游的 async 表达式）：
/// ```ignore
/// match pooled_attempt!(&st, pid, dm, |sec| relay::web::chat(pid, &msgs, sec, dm, 180)) {
///     Ok(payload) => { /* 成功 */ }
///     Err((status, msg)) => { /* 失败 */ }
/// }
/// ```
macro_rules! pooled_attempt {
    ($st:expr, $pid:expr, $model:expr, |$sec:ident| $call:expr) => {{
        let __st: &crate::server::state::AppState = $st;
        let __pid: &str = $pid;
        let __model: &str = $model;
        let mut __ranked = __st.inner.login.accounts_ranked(__pid);
        if __ranked.is_empty() {
            Err((503i64, format!("{} 需要先在「接入源」登录获取凭据", __pid)))
        } else {
            // RecentPick：紧迫度相同（都没到期数据）时让给窗口内被选次数少的
            __ranked.sort_by(|a, b| {
                a.urgency
                    .cmp(&b.urgency)
                    .then_with(|| {
                        __st
                            .inner
                            .pool
                            .recent_count(__pid, &a.id, __model)
                            .cmp(&__st.inner.pool.recent_count(__pid, &b.id, __model))
                    })
                    .then_with(|| a.last_used.cmp(&b.last_used))
            });
            let mut __last: Option<(i64, String)> = None;
            let mut __tried = 0usize;
            let mut __out: Result<_, (i64, String)> = Err((503i64, String::new()));
            for __a in &__ranked {
                if __tried >= 3 {
                    break;
                }
                if __st.inner.pool.is_blocked(__pid, &__a.id, __model) {
                    continue;
                }
                let Some(__lease) = __st.inner.pool.try_acquire(__pid, &__a.id, __model) else {
                    continue; // 在途已满，换号
                };
                __tried += 1;
                __st.inner.pool.note_pick(__pid, &__a.id, __model);
                let $sec: &str = __a.secret.as_str();
                let __r = $call.await;
                drop(__lease);
                match __r {
                    Ok(v) => {
                        __st.inner.pool.mark_ok(&__a.id, __model);
                        __out = Ok(v);
                        break;
                    }
                    Err((status, msg)) => {
                        __st.inner.pool.mark_fail(__pid, &__a.id, __model, status as u16);
                        __last = Some((status, msg));
                    }
                }
            }
            // 半开：全部被治理状态挡住时，放行最紧迫的一个（不清冷却，靠本次结果说话）
            if __tried == 0 {
                let __a = &__ranked[0];
                if let Some(__lease) = __st.inner.pool.try_acquire(__pid, &__a.id, __model) {
                    __st.inner.pool.note_pick(__pid, &__a.id, __model);
                    let $sec: &str = __a.secret.as_str();
                    let __r = $call.await;
                    drop(__lease);
                    match __r {
                        Ok(v) => {
                            __st.inner.pool.mark_ok(&__a.id, __model);
                            __out = Ok(v);
                        }
                        Err((status, msg)) => {
                            __st.inner.pool.mark_fail(__pid, &__a.id, __model, status as u16);
                            __last = Some((status, msg));
                        }
                    }
                }
            }
            if __out.is_ok() {
                __out
            } else {
                Err(__last.unwrap_or((
                    503i64,
                    format!("{} 的可用账号都在冷却或在途，请稍后重试", __pid),
                )))
            }
        }
    }};
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
        // ★ 号池治理：紧迫度优先 + 冷却/在途过滤 + 失败自动换号（见 pooled_attempt 宏）
        if st.inner.login.accounts_ranked(pid).is_empty() {
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
            match pooled_attempt!(&st, pid, dm, |sec| relay::web::chat(pid, &msgs, sec, dm, 180)) {
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
        match pooled_attempt!(&st, pid, dm, |sec| relay::web::chat(pid, &msgs, sec, dm, 180)) {
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

    // 原生中继（号池治理选号）：豆包 samantha + 元宝 + Loomy + 通用 OpenAI 透明（小浣熊/Trae）
    if let relay::Plan::Native { pid, model: dm } = &plan {
        // ★ 号池治理：紧迫度优先 + 冷却/在途过滤 + 失败自动换号（见 pooled_attempt 宏）
        if st.inner.login.accounts_ranked(pid).is_empty() {
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
        // 共享：请求是否流式 + messages（豆包 / 元宝 / Loomy / 通用都用到）
        let stream = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
            .unwrap_or(false);
        let msgs: Vec<Value> = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("messages").and_then(|m| m.as_array()).cloned())
            .unwrap_or_default();
        // 豆包走专用 samantha 协议
        if pid == "apk-doubao" {
            if stream {
                match pooled_attempt!(&st, pid, dm, |sec| relay::doubao::chat_stream(&msgs, sec, dm, 180)) {
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
            match pooled_attempt!(&st, pid, dm, |sec| relay::doubao::chat(&msgs, sec, dm, 180)) {
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
            if stream {
                match pooled_attempt!(&st, pid, dm, |sec| relay::yuanbao::chat_stream(&msgs, sec, dm, 180)) {
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
            match pooled_attempt!(&st, pid, dm, |sec| relay::yuanbao::chat(&msgs, sec, dm, 180)) {
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
        // Loomy（讯飞）模型网关：OpenAI 兼容 + 每请求 traceparent 头
        if pid == "apk-loomy" {
            if stream {
                match pooled_attempt!(&st, pid, dm, |sec| relay::loomy::chat_stream(&msgs, sec, dm, 180)) {
                    Ok(resp) => {
                        st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, true, pid);
                        return resp;
                    }
                    Err((status, msg)) => {
                        st.inner.usage.record(pid, &model, 0, 0, t0.elapsed().as_millis() as i64, false, pid);
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(json!({
                                "error": {"message": format!("Loomy 原生中继失败（HTTP {}）：{}", status, msg),
                                          "type": "aigw_native_relay_error"},
                                "pid": pid,
                            })),
                        )
                            .into_response();
                    }
                }
            }
            match pooled_attempt!(&st, pid, dm, |sec| relay::loomy::chat(&msgs, sec, dm, 180)) {
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
                            "error": {"message": format!("Loomy 原生中继失败（HTTP {}）：{}", status, msg),
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
        if stream {
            match pooled_attempt!(&st, pid, dm, |sec| relay::native::chat_stream(&spec, &body, sec, 180)) {
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
        match pooled_attempt!(&st, pid, dm, |sec| relay::native::chat(&spec, &body, sec, 180)) {
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
    // copilot 系上游**只收流式**（实测非流式 400/11101）——客户端要非流式时强制转流式，
    // 执行后把 SSE 聚合回完整的 chat.completion（对齐 Python native_relay stream_only）。
    let body_v = body.to_vec();
    let orig_stream = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    let wb_pid = match &plan {
        relay::Plan::NeedLogin { pid, .. } if pid.starts_with("wb-gateway") => Some(pid.clone()),
        _ => None,
    };
    // ★ 号池治理失败转移：最多试 3 个账号；冷却/在途的跳过；
    //   全在冷却 → 半开最紧迫的那个（对齐 router half-open，避免一次风控永久锁死）。
    let mut wb_ok: Option<Response> = None;
    let mut wb_err: Option<Response> = None;
    if let Some(pid) = wb_pid {
        let mut cands = st.inner.login.accounts_ranked(&pid);
        cands.truncate(3);
        if cands.iter().any(|a| !st.inner.pool.is_blocked(&pid, &a.id, &model)) {
            cands.retain(|a| !st.inner.pool.is_blocked(&pid, &a.id, &model));
        } else {
            cands.truncate(1);
        }
        for a in cands {
            if st.inner.pool.try_acquire(&pid, &a.id, &model).is_none() {
                continue; // 在途已满，换号
            }
            st.inner.pool.note_pick(&pid, &a.id, &model);
            let mut b2 = body_v.clone();
            if !orig_stream {
                if let Ok(mut v) = serde_json::from_slice::<Value>(&b2) {
                    if let Value::Object(ref mut o) = v {
                        o.insert("stream".to_string(), Value::Bool(true));
                    }
                    if let Ok(b) = serde_json::to_vec(&v) {
                        b2 = b;
                    }
                }
            }
            let Some(up) = wb_upgrade(&plan, std::slice::from_ref(&a.secret), &mut b2) else {
                continue;
            };
            let r = relay::execute(up, b2, &headers, &cfg).await;
            let code = r.status().as_u16();
            if (200..300).contains(&code) {
                st.inner.pool.mark_ok(&a.id, &model);
                wb_ok = Some(r);
                break;
            }
            st.inner.pool.mark_fail(&pid, &a.id, &model, code);
            wb_err = Some(r);
        }
    }
    // 全失败 → 原样回最后一次失败（状态码非 2xx，record_usage 会如实记账）
    if wb_ok.is_none() {
        if let Some(r) = wb_err {
            return record_usage(&st, &model, &upstream, t0, r).await;
        }
    }
    let (resp, wb_streamed) = match wb_ok {
        // 成功：非流式请求是被强制转成流式的，需要聚合回完整 completion
        Some(r) => (r, !orig_stream),
        // 没有 wb 凭据 / 非 wb 计划 → 原样执行（NeedLogin 会返 503 引导登录）
        None => (relay::execute(plan, body_v, &headers, &cfg).await, false),
    };
    if wb_streamed {
        match relay::aggregate_sse_response(resp).await {
            Ok(payload) => {
                let tin = payload.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                let tout = payload.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
                st.inner
                    .usage
                    .record(&upstream, &model, tin, tout, t0.elapsed().as_millis() as i64, true, &upstream);
                return (StatusCode::OK, Json(payload)).into_response();
            }
            Err((status, msg)) => {
                st.inner
                    .usage
                    .record(&upstream, &model, 0, 0, t0.elapsed().as_millis() as i64, false, &upstream);
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
        // ★ 号池治理：紧迫度优先 + 冷却/在途过滤 + 失败自动换号
        if st.inner.login.accounts_ranked("apk-doubao").is_empty() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"type":"error","error":{"type":"invalid_request_error",
                    "message":"豆包原生中继需要先在「接入源」登录（粘贴 Cookie）"}})),
            )
                .into_response();
        }
        if stream {
            match pooled_attempt!(&st, "apk-doubao", &model, |sec| {
                relay::doubao::chat_stream(&msgs, sec, &model, 180)
            }) {
                Ok(r) => return anthropic_sse(r).await,
                Err((s, m)) => {
                    return (StatusCode::BAD_GATEWAY, Json(json!({"type":"error",
                        "error":{"type":"api_error","message":format!("豆包中继失败（HTTP {}）：{}", s, m)}})))
                        .into_response();
                }
            }
        }
        return match pooled_attempt!(&st, "apk-doubao", &model, |sec| {
            relay::doubao::chat(&msgs, sec, &model, 180)
        }) {
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
            // ★ 号池治理：紧迫度优先 + 冷却/在途过滤 + 失败自动换号
            if st.inner.login.accounts_ranked(pid).is_empty() {
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
                    match pooled_attempt!(&st, pid, &model, |sec| {
                        relay::native::chat_stream(&spec, &openai_bytes, sec, 180)
                    }) {
                        Ok(r) => return anthropic_sse(r).await,
                        Err((s, m)) => {
                            return (StatusCode::BAD_GATEWAY, Json(json!({"type":"error",
                                "error":{"type":"api_error","message":format!("{} 中继失败（HTTP {}）：{}", pid, s, m)}})))
                                .into_response();
                        }
                    }
                }
                return match pooled_attempt!(&st, pid, &model, |sec| {
                    relay::native::chat(&spec, &openai_bytes, sec, 180)
                }) {
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
