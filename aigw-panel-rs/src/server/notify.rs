//! 通知模块：对齐 Python app/notify.py。
//! 支持 7 种 webhook：wecom/dingtalk/feishu/pushplus/sctp/bark/custom。
//! 事件门控：checkin/quota/error/growth/task 五类，配置存 data/notify.json。

use serde_json::{json, Value};

pub const WEBHOOK_TYPES: &[&str] = &["wecom", "dingtalk", "feishu", "pushplus", "sctp", "bark", "custom"];

/// 发一条 webhook（对齐 Python send_one：按 type 构造不同 payload）。
pub async fn send_one_pub(hook: &Value, title: &str, body: &str) -> (bool, String) {
    send_one(hook, title, body).await
}

/// 内部实现。
async fn send_one(hook: &Value, title: &str, body: &str) -> (bool, String) {
    let t = hook.get("type").and_then(|v| v.as_str()).unwrap_or("custom");
    let url = hook.get("url").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if url.is_empty() {
        return (false, "缺少 webhook 地址".to_string());
    }
    let combined = format!("{}\n{}", title, body);
    let payload = match t {
        "wecom" => json!({"msgtype": "text", "text": {"content": combined}}),
        "dingtalk" => json!({"msgtype": "text", "text": {"content": combined}}),
        "feishu" => json!({"msg_type": "text", "content": {"text": combined}}),
        "pushplus" => json!({"title": title, "content": body, "template": "txt"}),
        "sctp" => json!({"title": title, "desp": body}),
        "bark" => json!({"title": title, "body": body}),
        _ => json!({"title": title, "content": body, "msg": combined}),
    };
    let client = reqwest::Client::new();
    match client
        .post(&url)
        .timeout(std::time::Duration::from_secs(12))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => {
            let code = r.status().as_u16();
            if (200..300).contains(&code) {
                (true, "发送成功".to_string())
            } else {
                let raw = r.text().await.unwrap_or_default();
                let preview: String = raw.chars().take(180).collect();
                (false, format!("HTTP {} {}", code, preview))
            }
        }
        Err(e) => (false, format!("{}", e)),
    }
}

/// 事件是否开启（对齐 Python gate 表）。
fn event_enabled(cfg: &Value, event: &str) -> bool {
    match event {
        "checkin" => cfg.get("notify_checkin").and_then(|v| v.as_bool()).unwrap_or(true),
        "quota" => cfg.get("notify_quota").and_then(|v| v.as_bool()).unwrap_or(true),
        "error" => cfg.get("notify_error").and_then(|v| v.as_bool()).unwrap_or(true),
        "growth" => cfg.get("notify_checkin").and_then(|v| v.as_bool()).unwrap_or(true),
        _ => true,
    }
}

/// 发送事件通知到所有启用的 webhook，写历史。
pub async fn notify(event: &str, title: &str, body: &str) -> Vec<Value> {
    let mut cfg = crate::server::store::load("notify");
    if !cfg.is_object() {
        cfg = json!({});
    }
    if !event_enabled(&cfg, event) {
        return vec![json!({"ok": false, "message": "该类通知已在设置中关闭"})];
    }
    let hooks: Vec<Value> = cfg
        .get("webhooks")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter(|h| h.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true))
                .filter(|h| {
                    let evs = h.get("events").and_then(|v| v.as_array());
                    match evs {
                        Some(arr) if !arr.is_empty() => arr.iter().any(|e| e.as_str() == Some(event)),
                        _ => true,
                    }
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if hooks.is_empty() {
        return vec![json!({"ok": false, "message": "未配置启用的 webhook"})];
    }
    let mut results = Vec::new();
    for h in &hooks {
        let (ok, msg) = send_one(h, title, body).await;
        results.push(json!({
            "type": h.get("type").and_then(|v| v.as_str()).unwrap_or("custom"),
            "target": h.get("name").and_then(|v| v.as_str()).or_else(|| h.get("url").and_then(|v| v.as_str())).unwrap_or(""),
            "ok": ok,
            "message": msg,
        }));
    }
    // 写历史（最多 100 条）
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if cfg.get("history").is_none() {
        cfg["history"] = json!([]);
    }
    if let Some(arr) = cfg.get_mut("history").and_then(|v| v.as_array_mut()) {
        for r in &results {
            arr.insert(
                0,
                json!({
                    "ts": now_ts,
                    "event": event,
                    "ok": r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
                    "target": r.get("target").and_then(|v| v.as_str()).unwrap_or(""),
                    "message": r.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                }),
            );
        }
        arr.truncate(100);
    }
    crate::server::store::save("notify", &cfg);
    results
}
