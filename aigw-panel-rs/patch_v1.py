import io

p = 'v1.rs'
L = io.open(p, encoding='utf-8').read().split('\n')


def find(pred, start=0):
    for i in range(start, len(L)):
        if pred(L[i]):
            return i
    raise SystemExit('anchor not found')


# ---------------------------------------------------------------- step 1
# 在 chat_completions 前插入 relay_pooled helper
i_cc = find(lambda l: l.startswith('pub async fn chat_completions'))
helper = r'''/// 号池治理中继执行器（2026-10-16，S3；治理规则见 `server/pool.rs`）。
///
/// 依次尝试按「紧迫度 → RecentPick → last_used」排好序的账号：
/// - 跳过正在冷却 / 被 WAF 拉黑 / 在途已满的账号；
/// - 成功 → `mark_ok` 立即返回；失败 → `mark_fail`（401/403 账号级拉黑）后换下一个；
/// - 全部被治理状态挡住 → **半开**最紧迫的那一个（对齐 router 的 half-open），
///   避免一次风控把整个平台永久锁死。
///
/// ★ 关键约束：本函数只读内存（账号池 + pool），绝不打上游余额/探测接口 ——
///   对话链路上不允许任何额外网络往返。
async fn relay_pooled<T, F, Fut>(
    st: &AppState,
    platform: &str,
    model: &str,
    mut attempt: F,
) -> Result<T, (i64, String)>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, (i64, String)>>,
{
    /// 单次请求最多换几个号（再多说明是上游整体故障，不是号的问题）
    const MAX_TRY: usize = 3;

    let mut ranked = st.inner.login.accounts_ranked(platform);
    if ranked.is_empty() {
        return Err((503, format!("{} 需要先在「接入源」登录获取凭据", platform)));
    }
    // RecentPick：紧迫度相同（同为 i64::MAX = 都没到期数据）时让给窗口内
    // 被选次数少的账号；紧迫度不同的仍以积分到期日为硬优先。
    ranked.sort_by(|a, b| {
        a.urgency
            .cmp(&b.urgency)
            .then_with(|| {
                st.inner
                    .pool
                    .recent_count(platform, &a.id, model)
                    .cmp(&st.inner.pool.recent_count(platform, &b.id, model))
            })
            .then_with(|| a.last_used.cmp(&b.last_used))
    });

    let mut last_err: Option<(i64, String)> = None;
    let mut tried = 0usize;
    for a in &ranked {
        if tried >= MAX_TRY {
            break;
        }
        if st.inner.pool.is_blocked(platform, &a.id, model) {
            continue;
        }
        let Some(lease) = st.inner.pool.try_acquire(platform, &a.id, model) else {
            continue; // 在途已满，换号
        };
        tried += 1;
        st.inner.pool.note_pick(platform, &a.id, model);
        let r = attempt(a.secret.clone()).await;
        drop(lease);
        match r {
            Ok(v) => {
                st.inner.pool.mark_ok(&a.id, model);
                return Ok(v);
            }
            Err((status, msg)) => {
                st.inner.pool.mark_fail(platform, &a.id, model, status as u16);
                last_err = Some((status, msg));
            }
        }
    }
    // 半开：全部被治理状态挡住时，放行最紧迫的一个（不清冷却，靠本次结果说话）
    if tried == 0 {
        let a = &ranked[0];
        if let Some(lease) = st.inner.pool.try_acquire(platform, &a.id, model) {
            st.inner.pool.note_pick(platform, &a.id, model);
            let r = attempt(a.secret.clone()).await;
            drop(lease);
            match r {
                Ok(v) => {
                    st.inner.pool.mark_ok(&a.id, model);
                    return Ok(v);
                }
                Err((status, msg)) => {
                    st.inner.pool.mark_fail(platform, &a.id, model, status as u16);
                    last_err = Some((status, msg));
                }
            }
        }
    }
    Err(last_err.unwrap_or((
        503,
        format!("{} 的可用账号都在冷却或在途，请稍后重试", platform),
    )))
}

'''
L = L[:i_cc] + helper.split('\n') + L[i_cc:]

# ---------------------------------------------------------------- step 2
# Web 分支整体替换
i_web = find(lambda l: l.startswith('    // 网页版反代'))
i_nat = find(lambda l: l.startswith('    // 原生中继'), i_web)
web_new = r'''    // 网页版反代（智谱清言 / Trae 国内 / DeepSeek 网页）：私有协议，凭据取账号池
    if let relay::Plan::Web { pid, model: dm } = &plan {
        // ★ 号池治理：紧迫度优先 + 冷却/在途过滤 + 失败自动换号（见 relay_pooled）
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
            match relay_pooled(&st, pid, dm, |sec| relay::web::chat(pid, &msgs, &sec, dm, 180)).await {
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
        match relay_pooled(&st, pid, dm, |sec| relay::web::chat(pid, &msgs, &sec, dm, 180)).await {
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
'''.split('\n')
L = L[:i_web] + web_new + L[i_nat:]

# ---------------------------------------------------------------- step 3
# Native 分支整体替换
i_nat = find(lambda l: l.startswith('    // 原生中继'))
i_wb = find(lambda l: l.startswith('    // wb-gateway 族'), i_nat)
nat_new = r'''    // 原生中继（号池治理选号）：豆包 samantha + 元宝 + Loomy + 通用 OpenAI 透明（小浣熊/Trae）
    if let relay::Plan::Native { pid, model: dm } = &plan {
        // ★ 号池治理：紧迫度优先 + 冷却/在途过滤 + 失败自动换号（见 relay_pooled）
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
                match relay_pooled(&st, pid, dm, |sec| relay::doubao::chat_stream(&msgs, &sec, dm, 180)).await {
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
            match relay_pooled(&st, pid, dm, |sec| relay::doubao::chat(&msgs, &sec, dm, 180)).await {
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
                match relay_pooled(&st, pid, dm, |sec| relay::yuanbao::chat_stream(&msgs, &sec, dm, 180)).await {
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
            match relay_pooled(&st, pid, dm, |sec| relay::yuanbao::chat(&msgs, &sec, dm, 180)).await {
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
                match relay_pooled(&st, pid, dm, |sec| relay::loomy::chat_stream(&msgs, &sec, dm, 180)).await {
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
            match relay_pooled(&st, pid, dm, |sec| relay::loomy::chat(&msgs, &sec, dm, 180)).await {
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
            match relay_pooled(&st, pid, dm, |sec| relay::native::chat_stream(&spec, &body, &sec, 180)).await {
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
        match relay_pooled(&st, pid, dm, |sec| relay::native::chat(&spec, &body, &sec, 180)).await {
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
'''.split('\n')
L = L[:i_nat] + nat_new + L[i_wb:]

io.open(p, 'w', encoding='utf-8', newline='\n').write('\n'.join(L))
print('step1-3 ok, lines now', len(L))
