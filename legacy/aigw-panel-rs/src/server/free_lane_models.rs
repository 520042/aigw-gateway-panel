//! 免费车道动态模型清单拉取 + 缓存。
//!
//! 数据源（整合自 dsh-our-free-model，已实测）：
//! - anon-zen（Our Free Model / OpenCode Zen 匿名车道）：GET /zen/v1/models
//!   Bearer public + UA opencode/1.18.31 → 86 个模型（含 *-free 免费款 + 付费款）
//! - kilo（Kilo Gateway 免凭据免费池）：GET /api/gateway/models 免凭据 → 394 个，
//!   取 isFree=true 切片 → 15 个免费模型（id 带 org/ 前缀须保留全名）
//!
//! 缓存到 data/free_lane_models.json，TTL 20h。前端模型总表读缓存，不阻塞。

use serde_json::{json, Value};
use std::path::PathBuf;

const CACHE_FILE: &str = "free_lane_models.json";
const TTL_SECS: u64 = 3600 * 20;

fn cache_path() -> PathBuf {
    crate::core::config::data_dir().join(CACHE_FILE)
}

/// 拉取所有免费车道的动态模型，合并返回 { pid: [models...] }。
/// 失败的车道跳过（不影响其他车道），整体返回 None 仅当全部失败。
pub async fn fetch_all() -> Option<Value> {
    let mut out = json!({});
    let mut any_ok = false;

    // ---- anon-zen：Bearer public + Zen UA ----
    if let Some(models) = fetch_anon_zen().await {
        out["anon-zen"] = Value::Array(models);
        any_ok = true;
    }
    // ---- kilo：免凭据，取 isFree 切片 ----
    if let Some(models) = fetch_kilo().await {
        out["kilo"] = Value::Array(models);
        any_ok = true;
    }

    if any_ok {
        let now = now_secs_f64();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("_fetched_at".to_string(), json!(now));
        }
        let _ = std::fs::write(cache_path(), serde_json::to_string_pretty(&out).unwrap_or_default());
        Some(out)
    } else {
        None
    }
}

async fn fetch_anon_zen() -> Option<Vec<Value>> {
    let client = reqwest::Client::new();
    match client
        .get("https://opencode.ai/zen/v1/models")
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", "Bearer public")
        .header("User-Agent", "opencode/1.18.31")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.ok()?;
            let models: Vec<Value> = v
                .get("data")
                .and_then(|d| d.as_array())
                .map(|a| {
                    a.iter()
                        .map(|m| {
                            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let is_free = id.ends_with("-free") || id.ends_with(":free");
                            json!({
                                "id": id,
                                "name": m.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                                "free": is_free,
                                "owned_by": m.get("owned_by").and_then(|v| v.as_str()).unwrap_or("opencode"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if models.is_empty() { None } else { Some(models) }
        }
        _ => None,
    }
}

async fn fetch_kilo() -> Option<Vec<Value>> {
    let client = reqwest::Client::new();
    match client
        .get("https://api.kilo.ai/api/gateway/models")
        .timeout(std::time::Duration::from_secs(20))
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let v: Value = r.json().await.ok()?;
            let models: Vec<Value> = v
                .get("data")
                .and_then(|d| d.as_array())
                .map(|a| {
                    a.iter()
                        .filter(|m| m.get("isFree").and_then(|v| v.as_bool()).unwrap_or(false))
                        .map(|m| {
                            // ★ id 带 org/ 前缀必须保留全名（上游要求），不能截
                            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            json!({
                                "id": id,
                                "name": m.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                "free": true,
                                "desc": m.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if models.is_empty() { None } else { Some(models) }
        }
        _ => None,
    }
}

/// 非阻塞读取缓存。
pub fn get_cached() -> Option<Value> {
    match std::fs::read_to_string(cache_path()) {
        Ok(s) => {
            let v: Value = serde_json::from_str(&s).ok()?;
            let fetched_at = v.get("_fetched_at").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if now_secs_f64() - fetched_at < TTL_SECS as f64 {
                Some(v)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

fn now_secs_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
