use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use super::util::*;

/// `/api/localproxy`：本机反代上游（内置 CLIProxyAPI）管理。
pub async fn localproxy(
    State(_st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    use crate::server::localproxy as LP;
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("status").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let port = b.get("port").and_then(|v| v.as_u64()).map(|n| n as u16);
    let key = b.get("key").and_then(|v| v.as_str());
    match action.as_str() {
        "start" => {
            let (okk, msg) = LP::start(port, key).await;
            if okk {
                Json(json!({"ok": true, "message": msg}))
            } else {
                Json(json!({"ok": false, "code": "start_failed", "message": msg}))
            }
        }
        "stop" => {
            let killed = LP::stop();
            Json(json!({"ok": true, "killed": killed,
                        "message": if killed.is_empty() { "本来就没在运行".to_string() }
                                   else { format!("已停止（{} 个进程）", killed.len()) }}))
        }
        "restart" => {
            let (okk, msg) = LP::restart(port, key).await;
            if okk {
                Json(json!({"ok": true, "message": msg}))
            } else {
                Json(json!({"ok": false, "code": "restart_failed", "message": msg}))
            }
        }
        "config" => {
            let host = b.get("host").and_then(|v| v.as_str());
            let (okk, msg) = LP::write_config(port, key, host);
            if !okk {
                return Json(json!({"ok": false, "code": "config_failed", "message": msg}));
            }
            if b.get("apply").and_then(|v| v.as_bool()).unwrap_or(false) {
                let stx = LP::status().await;
                if stx.get("online").and_then(|v| v.as_bool()).unwrap_or(false) {
                    LP::restart(port, key).await;
                }
            }
            Json(json!({"ok": true, "message": msg, "status": LP::status().await}))
        }
        "login" => {
            let provider = b.get("provider").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let (okk, d) = LP::start_login(&provider).await;
            if okk {
                let mut out = d;
                out["ok"] = json!(true);
                Json(out)
            } else {
                Json(json!({"ok": false, "code": "login_failed",
                            "message": d.get("message").and_then(|v| v.as_str()).unwrap_or("登录失败")}))
            }
        }
        "log" => {
            let lines = b.get("lines").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
            Json(json!({"ok": true, "log": LP::tail_log(lines)}))
        }
        _ => Json(LP::status().await),
    }
}

/// 对齐 `/api/sites`：公益站清单（boot 预取 → catalogSites → 公益中转站卡片）。
#[tracing::instrument(skip(st))]
pub async fn sites(State(st): State<AppState>) -> impl IntoResponse {
    let s = st.inner.catalog.catalog_sites();
    Json(json!({
        "ok": true,
        "sites": s,
        "catalog": s,
    }))
}

/// 对齐 `/api/custom_apis`：list（GET）/ add / update / delete / sync（POST）。
/// sync 真实探测上游 `<base_url>/models`（Bearer key），拉回模型清单。
#[tracing::instrument(skip(st, body))]
pub async fn custom_apis(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("list").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);

    fn load_apis() -> Map<String, Value> {
        match crate::server::store::load("custom_apis").get("apis") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        }
    }
    fn save_apis(m: &Map<String, Value>) {
        crate::server::store::save("custom_apis", &json!({"apis": m}));
    }
    fn mask(s: &str) -> String {
        if s.is_empty() {
            String::new()
        } else if s.len() > 12 {
            format!("{}…{}", &s[..6], &s[s.len() - 4..])
        } else {
            "*".repeat(s.len())
        }
    }

    match action.as_str() {
        "list" => {
            let apis = load_apis();
            let mut out = Vec::new();
            for (id, c) in &apis {
                out.push(json!({
                    "id": id,
                    "name": c.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "protocol": c.get("protocol").and_then(|v| v.as_str()).unwrap_or("openai"),
                    "base_url": c.get("base_url").and_then(|v| v.as_str()).unwrap_or(""),
                    "api_key": mask(c.get("api_key").and_then(|v| v.as_str()).unwrap_or("")),
                    "has_key": !c.get("api_key").and_then(|v| v.as_str()).unwrap_or("").is_empty(),
                    "models": c.get("models").cloned().unwrap_or(json!([])),
                    "model_count": c.get("models").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                    "last_sync": c.get("last_sync").and_then(|v| v.as_str()).unwrap_or(""),
                    "created_at": c.get("created_at").and_then(|v| v.as_str()).unwrap_or(""),
                }));
            }
            Json(json!({"ok": true, "apis": out}))
        }
        "add" => {
            let mut apis = load_apis();
            let id = format!(
                "custom-{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let cfg = json!({
                "name": b.get("name").and_then(|v| v.as_str()).unwrap_or("未命名"),
                "protocol": b.get("protocol").and_then(|v| v.as_str()).unwrap_or("openai"),
                "base_url": b.get("base_url").and_then(|v| v.as_str()).unwrap_or("").trim_end_matches('/'),
                "api_key": b.get("api_key").and_then(|v| v.as_str()).unwrap_or(""),
                "models": [],
                "model_count": 0,
                "last_sync": "",
                "created_at": now_local(),
            });
            apis.insert(id.clone(), cfg);
            save_apis(&apis);
            Json(json!({"ok": true, "id": id}))
        }
        "update" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mut apis = load_apis();
            match apis.get_mut(id) {
                Some(c) => {
                    for k in ["name", "protocol", "base_url", "api_key"] {
                        if let Some(v) = b.get(k).and_then(|v| v.as_str()) {
                            if k == "base_url" {
                                c[k] = json!(v.trim_end_matches('/'));
                            } else {
                                c[k] = json!(v);
                            }
                        }
                    }
                    save_apis(&apis);
                    Json(json!({"ok": true}))
                }
                None => Json(json!({"ok": false, "message": "配置不存在"})),
            }
        }
        "delete" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mut apis = load_apis();
            let ok = apis.remove(id).is_some();
            save_apis(&apis);
            Json(json!({"ok": ok}))
        }
        "sync" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mut apis = load_apis();
            let Some(c) = apis.get_mut(id).cloned() else {
                return Json(json!({"ok": false, "message": "配置不存在"}));
            };
            let base = c.get("base_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let key = c.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if base.is_empty() {
                return Json(json!({"ok": false, "message": "base_url 为空"}));
            }
            let url = format!("{}/models", base.trim_end_matches('/'));
            let client = reqwest::Client::new();
            let mut req = client.get(&url).timeout(std::time::Duration::from_secs(15));
            if !key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    let txt = resp.text().await.unwrap_or_default();
                    let parsed: Result<Value, _> = serde_json::from_str(&txt);
                    let models: Vec<String> = parsed
                        .ok()
                        .and_then(|v| {
                            v.get("data")
                                .and_then(|d| d.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
                                        .map(|s| s.to_string())
                                        .collect()
                                })
                        })
                        .unwrap_or_default();
                    let n = models.len();
                    if n == 0 {
                        return Json(json!({"ok": false, "message": "上游 /models 返回里没有模型（形状不是 OpenAI data[]）"}));
                    }
                    if let Some(c) = apis.get_mut(id) {
                        c["models"] = json!(models);
                        c["model_count"] = json!(n);
                        c["last_sync"] = json!(now_local());
                    }
                    save_apis(&apis);
                    Json(json!({"ok": true, "count": n, "models": models}))
                }
                Ok(resp) => Json(json!({
                    "ok": false,
                    "message": format!("上游 /models 返回 HTTP {}", resp.status())
                })),
                Err(e) => Json(json!({"ok": false, "message": format!("探测失败：{}", e)})),
            }
        }
        _ => Json(json!({"ok": false, "code": "unknown_action",
                         "message": format!("custom_apis action='{}' 未实现", action)})),
    }
}
