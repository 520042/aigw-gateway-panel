import io

p = 'v1.rs'
L = io.open(p, encoding='utf-8').read().split('\n')


def find(pred, start=0):
    for i in range(start, len(L)):
        if pred(L[i]):
            return i
    raise SystemExit('anchor not found')


# ---------------------------------------------------------------- step 4
# wb-gateway 段整体替换（号池治理失败转移 + 半开）
i_wb = find(lambda l: l.startswith('    // wb-gateway 族：有凭据'))
i_end = find(lambda l: l.startswith('    record_usage(&st, &model, &upstream, t0, resp).await'), i_wb)
wb_new = r'''    // wb-gateway 族：有凭据 → 升级为 Bearer 直打
    // copilot 系上游**只收流式**（实测非流式 400/11101）——客户端要非流式时强制转流式，
    // 执行后把 SSE 聚合回完整的 chat.completion（对齐 Python native_relay stream_only）。
    let mut body_v = body.to_vec();
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
'''.split('\n')
L = L[:i_wb] + wb_new + L[i_end:]

# ---------------------------------------------------------------- step 5
# messages handler：豆包分支
i_db = find(lambda l: l.startswith('    if crate::relay::doubao::is_doubao_model(&model) {'))
# 该分支以单独一行 "    }" 结束（let plan = ... 之前）
i_plan = find(lambda l: l.startswith('    let plan = st.inner.registry.resolve(&model, &openai_bytes'), i_db)
db_new = r'''    if crate::relay::doubao::is_doubao_model(&model) {
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
            match relay_pooled(&st, "apk-doubao", &model, |sec| {
                relay::doubao::chat_stream(&msgs, &sec, &model, 180)
            })
            .await
            {
                Ok(r) => return anthropic_sse(r).await,
                Err((s, m)) => {
                    return (StatusCode::BAD_GATEWAY, Json(json!({"type":"error",
                        "error":{"type":"api_error","message":format!("豆包中继失败（HTTP {}）：{}", s, m)}})))
                        .into_response();
                }
            }
        }
        return match relay_pooled(&st, "apk-doubao", &model, |sec| {
            relay::doubao::chat(&msgs, &sec, &model, 180)
        })
        .await
        {
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
'''.split('\n')
L = L[:i_db] + db_new + L[i_plan:]

# ---------------------------------------------------------------- step 6
# messages handler：通用原生分支（小浣熊/Trae）
i_nat2 = find(lambda l: l.startswith('    if let relay::Plan::Native { pid, .. } = &plan {'))
i_after = find(lambda l: l.startswith('    let resp = relay::execute(plan, openai_bytes'), i_nat2)
nat2_new = r'''    if let relay::Plan::Native { pid, .. } = &plan {
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
                    match relay_pooled(&st, pid, &model, |sec| {
                        relay::native::chat_stream(&spec, &openai_bytes, &sec, 180)
                    })
                    .await
                    {
                        Ok(r) => return anthropic_sse(r).await,
                        Err((s, m)) => {
                            return (StatusCode::BAD_GATEWAY, Json(json!({"type":"error",
                                "error":{"type":"api_error","message":format!("{} 中继失败（HTTP {}）：{}", pid, s, m)}})))
                                .into_response();
                        }
                    }
                }
                return match relay_pooled(&st, pid, &model, |sec| {
                    relay::native::chat(&spec, &openai_bytes, &sec, 180)
                })
                .await
                {
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
'''.split('\n')
L = L[:i_nat2] + nat2_new + L[i_after:]

io.open(p, 'w', encoding='utf-8', newline='\n').write('\n'.join(L))
print('step4-6 ok, lines now', len(L))
