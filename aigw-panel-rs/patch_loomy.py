import io

p = 'loomy.rs'
s = io.open(p, encoding='utf-8').read()

# ── 1) chat()：补 out_client（之前补丁误打在 models() 上）+ 业务码识别 ──
old_chat = '''    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
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
    serde_json::from_str::<Value>(&raw)
        .map_err(|_| (status, format!("Loomy 返回非 JSON：{}", raw.chars().take(200).collect::<String>())))
}'''
new_chat = '''    // 非流式：总时长 timeout（+ connect 10s / read 90s，见 out_client）
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
}'''
assert old_chat in s, 'loomy chat pattern'
s = s.replace(old_chat, new_chat, 1)

# ── 2) chat_stream：上游没遵守 stream 时也要识别业务码 ──
old_st = '''    // 上游没遵守 stream：聚合成单帧 + [DONE]
    let raw = resp.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
    let mut sse = String::new();'''
new_st = '''    // 上游没遵守 stream：聚合成单帧 + [DONE]
    let raw = resp.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
    // ★ 200 + 业务码 = 失败：不能包成假 SSE 帧骗客户端（也骗不过号池治理）
    if let Some(e) = biz_err(&payload) {
        return Err(e);
    }
    let mut sse = String::new();'''
assert old_st in s, 'loomy chat_stream pattern'
s = s.replace(old_st, new_st, 1)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('loomy.rs patched')
