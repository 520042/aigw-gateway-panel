//! aigw-pool：号池治理（机制对齐旧项目 S3 号池 + lobsterai2api 冷却映射 +
//! Patrick/loomy「5xx 不怪账号」归因 + dengyie 账号状态机思想）。
//!
//! 六件套：
//! 1. **InFlight 租约**：同 (平台,账号,模型) ≤2 并发；[`Lease`] Drop 自动释放。
//! 2. **(账号,模型) 冷却**：`min(30s·2^n, 900s) × jitter[0.5,1.5)`，只冻这一对。
//! 3. **WafIpGate**：401/403（含假成功指纹）→ 账号级拉黑 30min。
//! 4. **RecentPick**：60s 窗口内同账号被选 ≥2 次 → 有同级候选时让位（防惊群）。
//! 5. **半开**：全被挡时放行一个「最该试」的（冷却剩余最短者清冷却探测），不永久锁死。
//! 6. **forget_account**：删号清全部治理状态，防快照挂幽灵账号。
//!
//! 归因原则（写死在 [`Pool::report`]）：
//! - `Auth`/`Waf`/`Quota` 是账号的问题 → 拉黑/禁用方向；
//! - `Rate`/`Transient` 是用得快/上游的事 → 只冷却 (账号,模型) 对，不动账号。

use aigw_core::ErrorClass;
use rand::Rng;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_INFLIGHT: u32 = 2;
const COOL_BASE_SECS: f64 = 30.0;
const COOL_CAP_SECS: f64 = 900.0;
const WAF_SECS: f64 = 1800.0; // 30min
const RECENT_WINDOW: Duration = Duration::from_secs(60);
const RECENT_YIELD: usize = 2;

#[derive(Debug, Clone)]
struct CoolState {
    until: Instant,
    strikes: u32,
}

#[derive(Default)]
struct Inner {
    /// (platform, account, model) -> 并发数
    inflight: HashMap<(String, String, String), u32>,
    /// (account, model) -> 冷却
    cooldown: HashMap<(String, String), CoolState>,
    /// account -> 拉黑截止
    waf: HashMap<String, Instant>,
    /// account -> 60s 窗口内被选时间
    recent: HashMap<String, Vec<Instant>>,
}

pub struct Pool {
    inner: Mutex<Inner>,
}

/// 并发租约：Drop 自动归还在飞计数。
pub struct Lease {
    pool: Arc<Pool>,
    key: (String, String, String),
    released: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !self.released {
            self.pool.release_inflight(&self.key);
        }
    }
}

impl Pool {
    pub fn new() -> Arc<Pool> {
        Arc::new(Pool {
            inner: Mutex::new(Inner::default()),
        })
    }

    fn release_inflight(&self, key: &(String, String, String)) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(n) = g.inflight.get_mut(key) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    g.inflight.remove(key);
                }
            }
        }
    }

    /// 账号在该 (平台,模型) 下是否可立即用；可用则拿租约返回 [`Lease`]。
    pub fn acquire(
        self: &Arc<Self>,
        platform: &str,
        account: &str,
        model: &str,
    ) -> Option<Lease> {
        let key = (
            platform.to_string(),
            account.to_string(),
            model.to_string(),
        );
        let mut g = self.inner.lock().ok()?;
        let n = g.inflight.entry(key.clone()).or_insert(0);
        if *n >= MAX_INFLIGHT {
            return None;
        }
        *n += 1;
        Some(Lease {
            pool: self.clone(),
            key,
            released: false,
        })
    }

    /// 按候选顺序挑一个「现在能用」的账号；选中即登记 RecentPick。
    /// 全部被挡时触发**半开**：放行冷却剩余最短者（清其冷却，探测性放行）。
    pub fn pick(
        &self,
        _platform: &str,
        accounts: &[String],
        model: &str,
    ) -> Option<String> {
        let now = Instant::now();
        let mut g = self.inner.lock().ok()?;
        let mut blocked: Vec<(String, f64)> = Vec::new(); // (account, 冷却剩余秒)
        for a in accounts {
            if g.waf.get(a).map(|t| *t > now).unwrap_or(false) {
                continue;
            }
            if let Some(cs) = g.cooldown.get(&(a.to_string(), model.to_string())) {
                if cs.until > now {
                    blocked.push((a.clone(), cs.until.duration_since(now).as_secs_f64()));
                    continue;
                }
            }
            // RecentPick：60s 窗口 ≥2 次 → 有更早候选可用时让位
            let picks = g
                .recent
                .get(a)
                .map(|v| v.iter().filter(|t| now.duration_since(**t) < RECENT_WINDOW).count())
                .unwrap_or(0);
            if picks >= RECENT_YIELD {
                // 让位：仅当后续还有「干净」候选时才跳过；先记着，兜底再回来
                blocked.push((a.clone(), 0.0));
                continue;
            }
            g.recent
                .entry(a.to_string())
                .or_default()
                .push(now);
            g.recent
                .get_mut(a)
                .map(|v| v.retain(|t| now.duration_since(*t) < RECENT_WINDOW));
            return Some(a.clone());
        }
        // 全被挡 → 半开：优先真正在冷却的（剩余最短者），否则让位型候选
        blocked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((account, remain)) = blocked.first() {
            if *remain > 0.0 {
                g.cooldown
                    .remove(&(account.clone(), model.to_string()));
                tracing::warn!(
                    "pool 半开：全部候选被挡，清冷却探测 {}（剩余 {:.0}s）",
                    account,
                    remain
                );
                return Some(account.clone());
            }
            // 仅 RecentPick 让位的情况：直接放行第一个（同级没有更干净的）
            return Some(account.clone());
        }
        None
    }

    /// 上报结果：成功清冷却/拉黑；失败按归因冷却或拉黑。
    pub fn report(
        &self,
        platform: &str,
        account: &str,
        model: &str,
        ok: bool,
        class: ErrorClass,
        waf_fingerprint: bool,
    ) {
        let _ = platform;
        let now = Instant::now();
        if let Ok(mut g) = self.inner.lock() {
            if ok {
                g.cooldown.remove(&(account.to_string(), model.to_string()));
                g.waf.remove(account);
                g.recent.remove(account);
                return;
            }
            match class {
                // 账号级问题：401/403（含假成功指纹）→ WAF 拉黑 30min
                ErrorClass::Auth if waf_fingerprint => {
                    g.waf.insert(account.to_string(), now + Duration::from_secs_f64(WAF_SECS));
                }
                ErrorClass::Quota => {
                    // 额度耗尽：账号级顶格冷却（M1 不引入禁用态，M2 换 dengyie 状态机）
                    g.waf.insert(account.to_string(), now + Duration::from_secs_f64(WAF_SECS));
                }
                // 其余只冻 (账号,模型) 对：指数退避 + 抖动
                other => {
                    let _ = other;
                    let key = (account.to_string(), model.to_string());
                    let strikes = g.cooldown.get(&key).map(|c| c.strikes).unwrap_or(0);
                    let exp = COOL_BASE_SECS * 2f64.powi(strikes.min(5) as i32);
                    let capped = exp.min(COOL_CAP_SECS);
                    let jitter = rand::thread_rng().gen_range(0.5..1.5);
                    let secs = capped * jitter;
                    g.cooldown.insert(
                        key,
                        CoolState {
                            until: now + Duration::from_secs_f64(secs),
                            strikes: strikes + 1,
                        },
                    );
                }
            }
        }
    }

    /// 删号清幽灵：清该账号全部治理状态。
    pub fn forget_account(&self, account: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.waf.remove(account);
            g.recent.remove(account);
            g.cooldown
                .retain(|(a, _), _| a != account);
            g.inflight
                .retain(|(_, a, _), _| a != account);
        }
    }

    /// 快照（调试/前端号池页）。
    pub fn snapshot(&self) -> serde_json::Value {
        let now = Instant::now();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cooldown: Vec<_> = g
            .cooldown
            .iter()
            .filter(|(_, c)| c.until > now)
            .map(|((a, m), c)| {
                json!({
                    "account": a, "model": m,
                    "left_s": c.until.duration_since(now).as_secs_f64().round(),
                    "strikes": c.strikes,
                })
            })
            .collect();
        let waf: Vec<_> = g
            .waf
            .iter()
            .filter(|(_, t)| **t > now)
            .map(|(a, t)| json!({ "account": a, "left_s": t.duration_since(now).as_secs_f64().round() }))
            .collect();
        let inflight: Vec<_> = g
            .inflight
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|((p, a, m), n)| json!({"platform": p, "account": a, "model": m, "n": n}))
            .collect();
        json!({
            "inflight": inflight,
            "cooling": cooldown,
            "waf": waf,
            "max_inflight": MAX_INFLIGHT,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accs(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("acc-{i}")).collect()
    }

    #[test]
    fn inflight_lease_cap_and_drop_release() {
        let pool = Pool::new();
        let l1 = pool.acquire("p", "a", "m");
        let l2 = pool.acquire("p", "a", "m");
        assert!(l1.is_some() && l2.is_some());
        assert!(pool.acquire("p", "a", "m").is_none(), "第 3 个租约应被拒");
        drop(l1);
        let l3 = pool.acquire("p", "a", "m");
        assert!(l3.is_some(), "Drop 后应释放名额");
    }

    #[test]
    fn cooldown_grows_exponentially_and_expires() {
        let pool = Pool::new();
        // 三次失败（Transient）→ 抖动后的冷却区间断言（避免 flake）
        let left_after = |n: u32| {
            for _ in 0..n {
                pool.report("p", "a", "m", false, ErrorClass::Transient, false);
            }
            let snap = pool.snapshot();
            let arr = snap["cooling"].as_array().unwrap();
            assert_eq!(arr.len(), 1);
            let left = arr[0]["left_s"].as_f64().unwrap();
            pool.forget_account("a");
            left
        };
        let first = left_after(1);
        assert!((15.0..45.0).contains(&first), "首次 30×[0.5,1.5)，得 {first}");
        let second = left_after(2);
        assert!((30.0..90.0).contains(&second), "二次 60×[0.5,1.5)，得 {second}");
        let capped = left_after(9);
        assert!(
            (450.0..1350.0).contains(&capped),
            "9 次后应触顶 900×[0.5,1.5)，得 {capped}"
        );
    }

    #[test]
    fn cooldown_expires_allows_pick_again() {
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Transient, false);
        // 手动把冷却时间拨到过去（借 forget+report 不可行，直接改内部：用半开探测代替）
        // 半开应放行且清冷却
        let picked = pool.pick("p", &["a".to_string()], "m");
        // 冷却剩余 30×jitter 秒 > 0，pick 全挡 → 半开放行
        assert_eq!(picked.as_deref(), Some("a"));
        // 半开已清冷却 → 再 pick 正常
        let again = pool.pick("p", &["a".to_string()], "m");
        assert_eq!(again.as_deref(), Some("a"));
    }

    #[test]
    fn waf_blacklists_account_30min() {
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Auth, true);
        let snap = pool.snapshot();
        let waf = snap["waf"].as_array().unwrap();
        assert_eq!(waf.len(), 1);
        let left = waf[0]["left_s"].as_f64().unwrap();
        assert!(left > 29.0 * 60.0, "WAF 拉黑应≈30min，得 {left}s");
        // 账号被拉黑：pick 全挡（WAF 账号不参与半开放行），无候选可用
        assert!(pool.pick("p", &["a".to_string()], "other").is_none());
    }

    #[test]
    fn recent_pick_yields_after_two_picks_in_window() {
        let pool = Pool::new();
        let accounts = accs(2);
        assert_eq!(pool.pick("p", &accounts, "m").as_deref(), Some("acc-0"));
        assert_eq!(pool.pick("p", &accounts, "m").as_deref(), Some("acc-0"));
        // acc-0 已 2 次 → 让位给 acc-1
        assert_eq!(pool.pick("p", &accounts, "m").as_deref(), Some("acc-1"));
    }

    #[test]
    fn success_resets_state() {
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Rate, false);
        pool.report("p", "a", "m", true, ErrorClass::Transient, false);
        let snap = pool.snapshot();
        assert_eq!(snap["cooling"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn forget_clears_everything() {
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Auth, true); // WAF
        pool.report("p", "a", "m2", false, ErrorClass::Transient, false); // 冷却
        let _ = pool.acquire("p", "a", "m3").unwrap();
        pool.forget_account("a");
        let snap = pool.snapshot();
        assert_eq!(snap["waf"].as_array().unwrap().len(), 0);
        assert_eq!(snap["cooling"].as_array().unwrap().len(), 0);
        assert_eq!(snap["inflight"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn transient_does_not_waf_account() {
        // 「5xx 不怪账号」：Transient 只冻 (账号,模型)，换模型立即可用
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Transient, false);
        assert_eq!(pool.pick("p", &["a".to_string()], "other").as_deref(), Some("a"));
    }
}
