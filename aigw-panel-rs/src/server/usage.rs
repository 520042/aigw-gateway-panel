//! 用量记录（对齐 Python `_record_usage`：事件列表 90 天 / 5000 条封顶）。
//!
//! 每次 /v1/chat/completions 记一条事件：{ts, platform, model, in, out, ms, ok, upstream}。
//! 非流式响应能从上游 usage 里取真实 token；流式记请求级（in/out=0，如实）。
use serde_json::{json, Value};
use std::sync::Mutex;

pub struct UsageRecorder {
    inner: Mutex<Vec<Value>>,
    loaded: Mutex<bool>,
}

const CAP_EVENTS: usize = 5000;
const CAP_DAYS: f64 = 90.0 * 86400.0;

impl UsageRecorder {
    pub fn new() -> Self {
        UsageRecorder {
            inner: Mutex::new(Vec::new()),
            loaded: Mutex::new(false),
        }
    }

    fn ensure_loaded(&self) {
        let mut l = self.loaded.lock().unwrap();
        if *l {
            return;
        }
        let v = crate::server::store::load("usage_events");
        let ev = v.get("events").and_then(|e| e.as_array()).cloned().unwrap_or_default();
        *self.inner.lock().unwrap() = ev;
        *l = true;
    }

    fn flush(&self, ev: &[Value]) {
        crate::server::store::save("usage_events", &json!({"events": ev}));
    }

    pub fn record(&self, platform: &str, model: &str, tin: i64, tout: i64, ms: i64, ok: bool, upstream: &str) {
        self.ensure_loaded();
        let mut ev = self.inner.lock().unwrap();
        let now = SystemNanos() as f64 / 1e9;
        ev.push(json!({
            "ts": now,
            "platform": platform,
            "model": model,
            "in": tin,
            "out": tout,
            "ms": ms,
            "ok": ok,
            "upstream": upstream,
        }));
        let cutoff = now - CAP_DAYS;
        ev.retain(|x| x.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0) >= cutoff);
        let len = ev.len();
        if len > CAP_EVENTS {
            ev.drain(..len - CAP_EVENTS);
        }
        self.flush(&ev);
    }

    /// 聚合（对齐 api_usage：totals/days[-14]/today/rows/recent）。
    pub fn aggregate(&self) -> Value {
        self.ensure_loaded();
        let ev = self.inner.lock().unwrap().clone();
        let (mut requests, mut success, mut failed): (i64, i64, i64) = (0, 0, 0);
        let (mut tin, mut tout): (i64, i64) = (0, 0);
        let mut day_map: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
        for x in &ev {
            let okk = x.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let i = x.get("in").and_then(|v| v.as_i64()).unwrap_or(0);
            let o = x.get("out").and_then(|v| v.as_i64()).unwrap_or(0);
            requests += 1;
            if okk { success += 1; } else { failed += 1; }
            tin += i;
            tout += o;
            let ts = x.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let day = day_of(ts);
            let dd = day_map
                .entry(day.clone())
                .or_insert_with(|| json!({"date": day, "requests": 0, "success": 0, "failed": 0, "tokensIn": 0, "tokensOut": 0}));
            dd["requests"] = json!(dd["requests"].as_i64().unwrap_or(0) + 1);
            if okk { dd["success"] = json!(dd["success"].as_i64().unwrap_or(0) + 1); }
            else { dd["failed"] = json!(dd["failed"].as_i64().unwrap_or(0) + 1); }
            dd["tokensIn"] = json!(dd["tokensIn"].as_i64().unwrap_or(0) + i);
            dd["tokensOut"] = json!(dd["tokensOut"].as_i64().unwrap_or(0) + o);
        }
        let days: Vec<Value> = day_map.values().cloned().collect();
        let today_key = day_of(SystemNanos() as f64 / 1e9);
        let today = day_map.get(&today_key).cloned().unwrap_or_else(|| {
            json!({"date": today_key, "requests": 0, "success": 0, "failed": 0, "tokensIn": 0, "tokensOut": 0})
        });
        let mut recent = ev.clone();
        recent.sort_by(|a, b| {
            b.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0)
                .partial_cmp(&a.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        recent.truncate(20);
        json!({
            "totals": {
                "requests": requests,
                "success": success,
                "failed": failed,
                "tokensIn": tin,
                "tokensOut": tout,
                "tokensTotal": tin + tout,
                "credit": 0,
            },
            "days": days.iter().rev().take(14).rev().cloned().collect::<Vec<_>>(),
            "today": today,
            "rows": recent.iter().take(200).cloned().collect::<Vec<_>>(),
            "recent": recent,
        })
    }
}

fn SystemNanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// unix 秒 → 本地日期串（UTC 近似；展示用，不引 chrono）。
fn day_of(ts: f64) -> String {
    let secs = ts as i64;
    let days = secs.div_euclid(86400);
    // civil_from_days（Howard Hinnant 算法）
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}
