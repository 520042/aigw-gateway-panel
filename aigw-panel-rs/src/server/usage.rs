//! 用量记录（对齐 Python `_record_usage`：事件列表 90 天 / 5000 条封顶）。
//!
//! 每次 /v1/chat/completions 记一条事件：{ts, platform, model, in, out, ms, ok, upstream}。
//! 非流式响应能从上游 usage 里取真实 token；流式记请求级（in/out=0，如实）。
use serde_json::{json, Value};
use std::sync::Mutex;

pub struct UsageRecorder {
    inner: Mutex<Vec<Value>>,
    loaded: Mutex<bool>,
    /// S5：SQLite 存储（打不开时 available()==false，自动降级回 JSON 全量写）
    db: crate::server::db::Db,
    /// 自上次裁剪以来的插入数（攒够一批才裁剪，避免每条都跑 DELETE）
    since_prune: std::sync::atomic::AtomicUsize,
}

const CAP_EVENTS: usize = 5000;
const CAP_DAYS: f64 = 90.0 * 86400.0;

impl UsageRecorder {
    pub fn new() -> Self {
        UsageRecorder {
            inner: Mutex::new(Vec::new()),
            loaded: Mutex::new(false),
            db: crate::server::db::Db::open(),
            since_prune: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn ensure_loaded(&self) {
        let mut l = self.loaded.lock().unwrap();
        if *l {
            return;
        }
        // S5：优先读 SQLite（启动时已把 JSON 里更新的事件合并进来）；
        //       DB 不可用才回退读 JSON。
        let ev = if self.db.available() {
            self.db.usage_load()
        } else {
            crate::server::store::load("usage_events")
                .get("events")
                .and_then(|e| e.as_array())
                .cloned()
                .unwrap_or_default()
        };
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
        let row = json!({
            "ts": now,
            "platform": platform,
            "model": model,
            "in": tin,
            "out": tout,
            "ms": ms,
            "ok": ok,
            "upstream": upstream,
        });
        ev.push(row.clone());
        let cutoff = now - CAP_DAYS;
        ev.retain(|x| x.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0) >= cutoff);
        let len = ev.len();
        if len > CAP_EVENTS {
            ev.drain(..len - CAP_EVENTS);
        }
        // ★ S5：落库方式按 DB 可用性二选一
        if self.db.available() {
            // 单条 INSERT —— 不再每次请求全量重写 750KB 的 JSON
            self.db.usage_insert(&row);
            let n = self
                .since_prune
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n >= 500 {
                self.since_prune.store(0, std::sync::atomic::Ordering::Relaxed);
                self.db.usage_prune(now - CAP_DAYS, CAP_EVENTS as i64);
            }
        } else {
            self.flush(&ev);
        }
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

        // ★ 详细看板聚合（2026-10-15 用户要求）：by_model / by_platform /
        //   24h 时段分布 / 延迟分位（P50/P95/平均）/ 最高峰时段。
        use std::collections::BTreeMap as BM;
        let mut by_model: BM<String, (i64, i64, i64, i64)> = BM::new(); // req,ok,tin,tout
        let mut by_platform: BM<String, (i64, i64, i64, i64)> = BM::new();
        let mut hour_map = [0i64; 24]; // 24h 时段请求分布（按本地日历小时）
        let mut lat_all: Vec<f64> = Vec::new(); // 成功请求的延迟(ms)
        for x in &ev {
            let okk = x.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let i = x.get("in").and_then(|v| v.as_i64()).unwrap_or(0);
            let o = x.get("out").and_then(|v| v.as_i64()).unwrap_or(0);
            let ms = x.get("ms").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let ts = x.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let model = x.get("model").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let plat = x.get("platform").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let e = by_model.entry(model).or_insert((0, 0, 0, 0));
            e.0 += 1; if okk { e.1 += 1; } e.2 += i; e.3 += o;
            let e = by_platform.entry(plat).or_insert((0, 0, 0, 0));
            e.0 += 1; if okk { e.1 += 1; } e.2 += i; e.3 += o;
            // 本地小时（近似：UTC+8，与 Python CST 一致）
            let local = ts + 8.0 * 3600.0;
            let hr = ((local % 86400.0) / 3600.0).floor();
            let hr = if hr < 0.0 { 0 } else if hr > 23.0 { 23 } else { hr as usize };
            hour_map[hr] += 1;
            if okk && ms > 0.0 { lat_all.push(ms); }
        }
        lat_all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let lat = |p: f64| -> i64 {
            if lat_all.is_empty() { return 0; }
            let idx = ((lat_all.len() as f64 - 1.0) * p).round() as usize;
            lat_all[idx.min(lat_all.len() - 1)] as i64
        };
        let avg_ms = if lat_all.is_empty() { 0 } else { (lat_all.iter().sum::<f64>() / lat_all.len() as f64).round() as i64 };
        let peak_hour = hour_map.iter().enumerate().max_by_key(|(_, v)| **v)
            .map(|(h, _)| format!("{:02}:00-{:02}:00", h, (h + 1) % 24))
            .unwrap_or_default();
        let to_rows = |m: &BM<String, (i64, i64, i64, i64)>| -> Vec<Value> {
            let mut rows: Vec<Value> = m.iter().map(|(k, (req, ok, i, o))| {
                let (req, ok) = (*req, *ok);
                json!({
                    "key": k, "requests": req, "success": ok, "failed": req - ok,
                    "tokensIn": i, "tokensOut": o,
                    "okRate": if req > 0 { ((ok as f64 / req as f64) * 1000.0).round() / 10.0 } else { 0.0 },
                })
            }).collect();
            rows.sort_by(|a, b| b.get("requests").and_then(|v| v.as_i64()).unwrap_or(0)
                .cmp(&a.get("requests").and_then(|v| v.as_i64()).unwrap_or(0)));
            rows
        };
        let hours: Vec<Value> = hour_map.iter().enumerate()
            .map(|(h, n)| json!({"hour": format!("{:02}", h), "requests": n})).collect();

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
            // ★ 详细看板（2026-10-15）
            "by_model": to_rows(&by_model),
            "by_platform": to_rows(&by_platform),
            "hours": hours,
            "peak_hour": peak_hour,
            "latency": {
                "avg_ms": avg_ms,
                "p50_ms": lat(0.50),
                "p95_ms": lat(0.95),
                "samples": lat_all.len(),
            },
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
