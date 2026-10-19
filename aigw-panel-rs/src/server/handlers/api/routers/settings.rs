use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use crate::core::config;

#[tracing::instrument(skip(st))]
pub async fn settings_get(State(st): State<AppState>) -> impl IntoResponse {
    let s = st.inner.settings.read().await.clone();
    Json(json!({
        "ok": true,
        "settings": config::settings_public(&s),
        "autostart": crate::tray::is_autostart_enabled(),
        "tray_supported": true,
    }))
}

#[tracing::instrument(skip(st, body))]
pub async fn settings_post(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    // action 走 query（前端 post('api/settings?action=xxx', body)）
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("save").to_string();
    match action.as_str() {
        "shutdown" => {
            // 关闭面板：延迟 300ms 让 HTTP 响应先发出去，再退出进程（托盘随之移除）。
            tracing::warn!("收到退出指令，面板即将关闭");
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                std::process::exit(0);
            });
            return Json(json!({"ok": true, "message": "面板正在退出"}));
        }
        "autostart" => {
            let enable = body.get("enable").and_then(|v| v.as_bool()).unwrap_or(true);
            let (ok, msg) = crate::tray::set_autostart(enable);
            return Json(json!({"ok": ok, "message": msg, "autostart": crate::tray::is_autostart_enabled()}));
        }
        _ => {}
    }
    let mut s = st.inner.settings.read().await.clone();
    if let Some(obj) = body.as_object() {
        if let Some(v) = obj.get("panel_token").and_then(|v| v.as_str()) {
            s.panel_token = v.to_string();
        }
        if let Some(v) = obj.get("port").and_then(|v| v.as_u64()) {
            s.port = v as u16;
        }
        if let Some(v) = obj.get("upstream_base").and_then(|v| v.as_str()) {
            s.upstream_base = v.to_string();
        }
        if let Some(v) = obj.get("upstream_key").and_then(|v| v.as_str()) {
            s.upstream_key = v.to_string();
        }
        if let Some(v) = obj.get("lobster_server").and_then(|v| v.as_str()) {
            s.lobster_server = v.to_string();
        }
    }
    config::save_settings(&s);
    *st.inner.settings.write().await = s.clone();
    Json(json!({
        "ok": true,
        "settings": config::settings_public(&s),
        "note": "port 变更需重启面板生效"
    }))
}
