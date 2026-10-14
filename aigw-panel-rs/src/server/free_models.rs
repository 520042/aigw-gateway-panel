//! 每日免费模型榜单（数据源 ClawLabsAI/free-ai-models）。
//! 对齐 Python app/free_models_feed.py：拉 data/models.json，过滤 chat 类，
//! 缓存到 data/free_models_feed.json，非阻塞读取 + refresh 强制拉取。

use serde_json::{json, Value};
use std::path::PathBuf;

const FEED_URL: &str = "https://raw.githubusercontent.com/ClawLabsAI/free-ai-models/main/data/models.json";
const CACHE_FILE: &str = "free_models_feed.json";
const TTL_SECS: u64 = 3600 * 20; // 20h：源每日只更新一次

fn cache_path() -> PathBuf {
    crate::core::config::data_dir().join(CACHE_FILE)
}

/// 拉取并解析榜单；返回 payload 或 None（失败）。
pub async fn fetch(force: bool) -> Option<Value> {
    if !force {
        if let Some(cached) = read_cache() {
            let fetched_at = cached.get("fetched_at").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let now = now_secs_f64();
            if now - fetched_at < TTL_SECS as f64 {
                return Some(cached);
            }
        }
    }
    let client = reqwest::Client::new();
    match client
        .get(FEED_URL)
        .timeout(std::time::Duration::from_secs(30))
        .header("User-Agent", "aigw-panel/1.0")
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let txt = r.text().await.unwrap_or_default();
            let d: Value = match serde_json::from_str(&txt) {
                Ok(v) => v,
                Err(_) => return None,
            };
            let mut out: Vec<Value> = Vec::new();
            for m in d.get("models").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                let kind = m.get("kind").and_then(|v| v.as_str());
                if let Some(k) = kind {
                    if k != "chat" {
                        continue;
                    }
                }
                let mid = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if mid.is_empty() {
                    continue;
                }
                out.push(json!({
                    "rank": m.get("rank").cloned().unwrap_or(Value::Null),
                    "id": mid,
                    "name": m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    "provider": m.get("provider").cloned().unwrap_or(Value::Null),
                    "data_use": m.get("data_use").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
                    "served_by": m.get("served_by").cloned().unwrap_or(json!([])),
                    "context_window": m.get("context_window").cloned().unwrap_or(Value::Null),
                    "supports_tools": m.get("supports_tools").and_then(|v| v.as_bool()).unwrap_or(false),
                    "rate_limit": m.get("rate_limit").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    "health": m.get("health").cloned().unwrap_or(Value::Null),
                    "expires": m.get("expires").cloned().unwrap_or(Value::Null),
                    "source": m.get("source").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    "integrated": integrated_by_key(&m),
                }));
            }
            // 按 rank 排序（None 排最后）
            out.sort_by_key(|m| {
                let r = m.get("rank").and_then(|v| v.as_i64());
                (r.is_none(), r.unwrap_or(999))
            });
            let payload = json!({
                "updated_at": d.get("updated_at").cloned().unwrap_or(Value::Null),
                "total": out.len(),
                "models": out,
                "fetched_at": now_secs_f64(),
            });
            let _ = std::fs::write(cache_path(), serde_json::to_string_pretty(&payload).unwrap_or_default());
            Some(payload)
        }
        _ => None,
    }
}

/// 非阻塞读取：先磁盘缓存。
pub fn get_cached() -> Option<Value> {
    read_cache()
}

fn read_cache() -> Option<Value> {
    match std::fs::read_to_string(cache_path()) {
        Ok(s) => serde_json::from_str(&s).ok(),
        Err(_) => None,
    }
}

fn now_secs_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 标记是否已被面板集成（对齐 Python _feed_integrated_by_key）。
fn integrated_by_key(m: &Value) -> Value {
    let key = format!(
        "{} {} {}",
        m.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        m.get("source").and_then(|v| v.as_str()).unwrap_or(""),
        m.get("provider").and_then(|v| v.as_str()).unwrap_or("")
    )
    .to_lowercase();
    if key.contains("openrouter") {
        json!("api-openrouter")
    } else if key.contains("pollinations") {
        json!("pollinations")
    } else {
        Value::Null
    }
}
