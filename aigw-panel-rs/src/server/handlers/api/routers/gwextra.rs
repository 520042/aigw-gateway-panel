use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// `/api/gwextra`：平台动作执行（数据驱动自 catalog.gwextra；对齐 Python api_gwextra）。
pub async fn gwextra(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    match action.as_str() {
        "list" => {
            let platform = b.get("platform").and_then(|v| v.as_str()).unwrap_or("");
            let actions = st
                .inner
                .catalog
                .raw_get("gwextra")
                .and_then(|g| g.get("actions"))
                .and_then(|a| a.get(platform))
                .cloned()
                .unwrap_or(json!([]));
            Json(json!({"ok": true, "platform": platform, "actions": actions}))
        }
        "exec" => {
            let pid = b
                .get("pid")
                .or_else(|| b.get("platform"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let aid = b
                .get("aid")
                .or_else(|| b.get("action"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if pid.is_empty() || aid.is_empty() {
                return Json(json!({"ok": false, "message": "pid 和 aid 必填"}));
            }
            match exec_gwextra(&st, &pid, &aid).await {
                Ok(v) => Json(json!({"ok": true, "data": v})),
                Err(m) => Json(json!({"ok": false, "code": "exec_failed", "message": m})),
            }
        }
        _ => Json(json!({"ok": true, "actions": "list/exec"})),
    }
}

/// 执行一个 gwextra 动作（移植 Python gwextra._request + call 的核心）。
async fn exec_gwextra(st: &AppState, platform: &str, aid: &str) -> Result<Value, String> {
    let gx = st.inner.catalog.raw_get("gwextra").cloned().unwrap_or(json!({}));
    let actions = gx
        .get("actions")
        .and_then(|a| a.get(platform))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let spec = actions
        .iter()
        .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(aid))
        .cloned()
        .ok_or_else(|| format!("平台 {} 不支持动作 {}", platform, aid))?;

    let method = spec.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
    let path = spec.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut base = spec
        .get("base")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            gx.get("base")
                .and_then(|b| b.get(platform))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    if base == "@lobster" {
        let s = st.inner.settings.read().await.clone();
        base = s.lobster_server.trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err("未配置 Lobster 上游地址（设置 →「Lobster 上游地址」）".to_string());
        }
    }
    if base.is_empty() {
        return Err(format!("平台 {} 未配置基址", platform));
    }
    let secret = st.inner.login.secrets_for(platform).first().cloned().unwrap_or_default();
    let no_auth = spec.get("no_auth").and_then(|v| v.as_bool()).unwrap_or(false);
    if secret.is_empty() && !no_auth {
        return Err(format!("账号池里没有 {} 的可用凭据，请先在「接入源」完成登录", platform));
    }
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let client = reqwest::Client::new();
    let mut req = if method == "POST" { client.post(&url) } else { client.get(&url) };
    req = req
        .timeout(std::time::Duration::from_secs(25))
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
        .header("Accept", "application/json");
    // 供应商专属头（Loomy / Antigravity 的 UA 是服务端校验的）
    if let Some(vh) = gx.get("vendor_headers").and_then(|v| v.get(platform)).and_then(|v| v.as_object()) {
        for (k, v) in vh {
            if let Some(sv) = v.as_str() {
                req = req.header(k, sv);
            }
        }
    }
    if !secret.is_empty() {
        let head40: &str = if secret.len() > 40 { &secret[..40] } else { &secret };
        if secret.trim_start().starts_with('{') || !head40.contains('=') {
            req = req.header("Authorization", format!("Bearer {}", secret.trim()));
        } else {
            req = req.header("Cookie", secret.as_str());
        }
    }
    if method != "GET" {
        let body = spec.get("body_form").cloned().unwrap_or(json!({}));
        req = req
            .header("Content-Type", "application/json")
            .body(body.to_string());
    }
    match req.send().await {
        Ok(r) => {
            let code = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            let parsed: Value = serde_json::from_str(&txt)
                .unwrap_or(json!({"status": code, "raw": txt.chars().take(2000).collect::<String>()}));
            if code >= 400 {
                Err(parsed.to_string().chars().take(500).collect::<String>())
            } else {
                Ok(json!({"platform": platform, "action": aid, "http": code, "data": parsed}))
            }
        }
        Err(e) => Err(format!("unreachable: {}", e)),
    }
}
