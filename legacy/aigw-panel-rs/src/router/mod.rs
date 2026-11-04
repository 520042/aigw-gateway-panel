//! 自动模型路由引擎（对齐 app/router.py）。
//!
//! 四策略（fastest/weight/priority/free）+ 健康熔断（连续失败进 OPEN，冷却后半开）+ 会话亲和
//! （同会话粘同一上游）。运行期状态与静态定义分离，避免选路时复制运行时指标。
use crate::core::model::Target;
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default)]
struct Runtime {
    ewma: Option<f64>,
    state: String, // closed / open / half-open
    open_until: f64,
    consec_fail: u32,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn rt_key(upstream_id: &str, endpoint: &str) -> String {
    format!("{}|{}", upstream_id, endpoint)
}

pub struct Router {
    targets: RwLock<HashMap<String, Vec<Target>>>,
    runtime: RwLock<HashMap<String, Runtime>>,
    session: Mutex<HashMap<String, (String, f64)>>,
    breaker_fail: u32,
    breaker_cooldown: f64,
    affinity_ttl: f64,
}

impl Router {
    pub fn new() -> Self {
        Router {
            targets: RwLock::new(HashMap::new()),
            runtime: RwLock::new(HashMap::new()),
            session: Mutex::new(HashMap::new()),
            breaker_fail: 3,
            breaker_cooldown: 60.0,
            affinity_ttl: 3600.0,
        }
    }

    pub fn add_model(&self, name: &str, targets: Vec<Target>) {
        let mut m = self.targets.write().unwrap();
        m.insert(name.to_string(), targets);
    }

    pub fn clear(&self) {
        self.targets.write().unwrap().clear();
        self.session.lock().unwrap().clear();
    }

    pub fn list_models(&self) -> Vec<String> {
        self.targets.read().unwrap().keys().cloned().collect()
    }

    /// 路由表完整描述（`/api/route` 给前端画表用）。
    /// ★ 2026-10-16 修：原来直接把 `list_models()`（Vec<String>）塞进 JSON 的
    ///   "models"，前端 `Object.keys()` 拿到的是下标 "0"/"1"，表格整列
    ///   undefined —— 契约对不上。现在按前端期望返回
    ///   { 模型: { healthy, total, targets: [{name,endpoint,free,state,ewma_ms,...}] } }。
    pub fn describe_models(&self) -> serde_json::Value {
        use serde_json::json;
        let t = self.targets.read().unwrap();
        let rt = self.runtime.read().unwrap();
        let now = now_secs();
        let mut out = serde_json::Map::new();
        for (name, list) in t.iter() {
            let mut tg = Vec::new();
            let mut healthy = 0i64;
            for x in list {
                let key = rt_key(&x.upstream_id, &x.endpoint);
                let r = rt.get(&key);
                let st = r.map(|v| v.state.as_str()).unwrap_or("closed");
                let open_stuck = st == "open"
                    && now < r.map(|v| v.open_until).unwrap_or(0.0);
                if !open_stuck {
                    healthy += 1;
                }
                let ewma = r.and_then(|v| v.ewma);
                tg.push(json!({
                    "name": x.name,
                    "endpoint": x.endpoint,
                    "model": x.model,
                    "free": x.free,
                    "weight": x.weight,
                    "priority": x.priority,
                    "state": if open_stuck { "open" } else { st },
                    "ewma_ms": ewma.map(|e| e.round() as i64),
                    "consec_fail": r.map(|v| v.consec_fail).unwrap_or(0),
                    "success_rate": serde_json::Value::Null,
                    "last_error": "",
                }));
            }
            out.insert(
                name.clone(),
                json!({"healthy": healthy, "total": list.len(), "targets": tg}),
            );
        }
        serde_json::Value::Object(out)
    }

    /// auto / free 都指向 Pollinations 免费车道（gpt-oss-20b），free 标记优先。
    pub fn register_auto_free(&self, pollinations_endpoint: &str) {
        let mk = |mid: &str, free: bool| Target {
            model: mid.to_string(),
            upstream_id: "pollinations:free".to_string(),
            name: "Pollinations 免费池".to_string(),
            endpoint: pollinations_endpoint.trim_end_matches('/').to_string(),
            api_key: String::new(),
            protocol: "openai".to_string(),
            weight: 100,
            priority: 0,
            free,
            headers: HashMap::new(),
        };
        self.add_model("auto", vec![mk("gpt-oss-20b", false)]);
        self.add_model("free", vec![mk("gpt-oss-20b", true)]);
    }

    /// 选路：返回最优 Target 克隆。
    pub fn pick(&self, model: &str, strategy: &str, session_id: Option<&str>) -> Option<Target> {
        let cands = {
            let m = self.targets.read().unwrap();
            m.get(model).cloned()?
        };
        if cands.is_empty() {
            return None;
        }
        let rt = self.runtime.read().unwrap();
        let now = now_secs();
        let mut alive: Vec<(Target, Option<f64>, bool)> = Vec::new();
        for t in &cands {
            let r = rt
                .get(&rt_key(&t.upstream_id, &t.endpoint))
                .cloned()
                .unwrap_or_default();
            let broken = r.state == "open" && now < r.open_until;
            if broken {
                continue;
            }
            alive.push((t.clone(), r.ewma, r.state == "open"));
        }
        if alive.is_empty() {
            // 全熔断 → 半开第一个
            let mut w = self.runtime.write().unwrap();
            if let Some(t) = cands.first() {
                let k = rt_key(&t.upstream_id, &t.endpoint);
                if let Some(r) = w.get_mut(&k) {
                    r.state = "half-open".to_string();
                    r.open_until = 0.0;
                }
            }
            drop(w);
            let t = cands.first().unwrap().clone();
            let r = rt
                .get(&rt_key(&t.upstream_id, &t.endpoint))
                .cloned()
                .unwrap_or_default();
            alive.push((t, r.ewma, false));
        }

        let sid = session_id.and_then(|s| self.affinity(s));

        let ordered = match strategy {
            "priority" => {
                let mut v: Vec<_> = alive.clone();
                v.sort_by(|a, b| {
                    a.0.priority
                        .cmp(&b.0.priority)
                        .then(a.0.weight.cmp(&b.0.weight))
                        .then(
                            a.1.map(|x| (x * 1000.0) as i64)
                                .unwrap_or(i64::MAX)
                                .cmp(&b.1.map(|x| (x * 1000.0) as i64).unwrap_or(i64::MAX)),
                        )
                });
                v
            }
            "weight" => {
                let mut v: Vec<_> = alive.clone();
                v.sort_by(|a, b| {
                    b.0.weight
                        .cmp(&a.0.weight)
                        .then(
                            a.1.map(|x| (x * 1000.0) as i64)
                                .unwrap_or(i64::MAX)
                                .cmp(&b.1.map(|x| (x * 1000.0) as i64).unwrap_or(i64::MAX)),
                        )
                });
                v
            }
            "free" => {
                let mut v: Vec<_> = alive.clone();
                v.sort_by(|a, b| {
                    (a.0.free as u8)
                        .cmp(&(b.0.free as u8))
                        .then(
                            a.1.map(|x| (x * 1000.0) as i64)
                                .unwrap_or(i64::MAX)
                                .cmp(&b.1.map(|x| (x * 1000.0) as i64).unwrap_or(i64::MAX)),
                        )
                });
                v
            }
            _ => {
                // fastest：有延迟优先，无延迟排最后
                let mut with_lat: Vec<_> =
                    alive.iter().filter(|x| x.1.is_some()).cloned().collect();
                let without: Vec<_> = alive.iter().filter(|x| x.1.is_none()).cloned().collect();
                with_lat.sort_by(|a, b| {
                    a.1.unwrap_or(f64::MAX)
                        .partial_cmp(&b.1.unwrap_or(f64::MAX))
                        .unwrap()
                });
                with_lat.extend(without);
                with_lat
            }
        };

        let mut ordered = ordered;
        // 会话亲和：同会话优先
        if let Some(sid) = sid {
            if let Some(pos) = ordered.iter().position(|x| x.0.upstream_id == sid) {
                if ordered.len() > 1 {
                    let item = ordered.remove(pos);
                    ordered.insert(0, item);
                }
            }
        }
        ordered.into_iter().next().map(|x| x.0)
    }

    pub fn bind_session(&self, session_id: &str, upstream_id: &str) {
        let mut s = self.session.lock().unwrap();
        s.insert(session_id.to_string(), (upstream_id.to_string(), now_secs()));
    }

    fn affinity(&self, session_id: &str) -> Option<String> {
        let s = self.session.lock().unwrap();
        let (uid, ts) = s.get(session_id)?;
        if now_secs() - ts > self.affinity_ttl {
            return None;
        }
        Some(uid.clone())
    }

    pub fn mark_ok(&self, t: &Target, latency_ms: Option<f64>) {
        let mut rt = self.runtime.write().unwrap();
        let k = rt_key(&t.upstream_id, &t.endpoint);
        let r = rt.entry(k).or_default();
        r.state = "closed".to_string();
        r.consec_fail = 0;
        if let Some(dt) = latency_ms {
            r.ewma = Some(match r.ewma {
                Some(prev) => 0.7 * prev + 0.3 * dt,
                None => dt,
            });
        }
    }

    pub fn mark_fail(&self, t: &Target) {
        let mut rt = self.runtime.write().unwrap();
        let k = rt_key(&t.upstream_id, &t.endpoint);
        let r = rt.entry(k).or_default();
        r.consec_fail += 1;
        if r.consec_fail >= self.breaker_fail {
            r.state = "open".to_string();
            r.open_until = now_secs() + self.breaker_cooldown;
        }
    }

    /// 探针：非 native 目标 GET /models 测延迟；native 免费车道视为活。
    pub async fn probe_all(&self, client: &reqwest::Client) {
        let targets = self
            .targets
            .read()
            .unwrap()
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let mut seen = std::collections::HashSet::new();
        for t in targets {
            let k = rt_key(&t.upstream_id, &t.endpoint);
            if !seen.insert(k.clone()) {
                continue;
            }
            if t.upstream_id.starts_with("native:") {
                self.mark_ok(&t, Some(1.0));
                continue;
            }
            let url = format!("{}/models", t.endpoint);
            let t0 = now_secs();
            let mut req = client.get(&url);
            if !t.api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", t.api_key));
            }
            match req.timeout(std::time::Duration::from_secs(6)).send().await {
                Ok(_) => self.mark_ok(&t, Some((now_secs() - t0) * 1000.0)),
                Err(_) => self.mark_fail(&t),
            }
        }
    }
}
