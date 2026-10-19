//! 号池治理（2026-10-16，重构路线 S3；对齐参考框架 `admin/pool.py`）。
//!
//! 解决四个真实故障模式：
//!
//! 1. **同一个账号被打爆** —— InFlight 租约：同一 (平台,账号,模型) 的在途请求
//!    超过 `MAX_INFLIGHT` 时，选号直接跳过它，把流量分给同平台其它账号。
//! 2. **一个账号对某个模型持续失败，却每次请求都还先试它** —— (账号,模型) 粒度
//!    冷却：失败只冻结这一个组合（换模型照样能用），连续失败按**指数退避 + 抖动**
//!    拉长冷却，不会永远每 60s 准时再撞一次墙。
//! 3. **凭据已死 / 被风控** —— WafIpGate：401/403 按**账号级**拉黑
//!    `WAF_TTL`（凭据失效不会自己好，短冷却毫无意义）。
//! 4. **同一个账号被连续选中** —— RecentPick：窗口内被选次数作为同级排序键，
//!    紧迫度相同时让给最近少被选的账号（等价轮询，但尊重到期日优先级）。
//!
//! ★ 与 `router` 的分工：`router` 管「模型 → 上游」的熔断/延迟；本模块管
//!   「平台 → 账号池里哪一个凭据」。请求路径（`v1.rs::relay_pooled`）只读内存，
//!   绝不打上游 —— 和 urgency_rank 一样，任何实时探测都不允许放进对话链路。
//!
//! ★ 半开语义：全部账号都在冷却时，强制放行最紧迫的那一个（对齐 router 的
//!   half-open），避免一次风控把整个平台永久锁死。
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 同一 (平台,账号,模型) 允许的最大在途请求数。
const MAX_INFLIGHT: usize = 2;
/// 冷却基数（秒）：第 1 次失败冻这么久，之后每次翻倍。
const BASE_COOLDOWN: f64 = 30.0;
/// 冷却上限（秒）：指数退避最多冻 15 分钟。
const MAX_COOLDOWN: f64 = 900.0;
/// WAF / 凭据失效拉黑时长（秒）：30 分钟。
const WAF_TTL: f64 = 1800.0;
/// RecentPick 窗口（秒）与窗口内最大选中次数。
const RECENT_WINDOW: f64 = 60.0;
const RECENT_MAX: usize = 2;

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 轻量伪随机（xorshift64*，纳秒播种）—— 只为退避抖动，不引 rand crate。
fn jitter() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0);
    let mut x = SEED.load(Ordering::Relaxed);
    if x == 0 {
        x = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            | 1;
        SEED.store(x, Ordering::Relaxed);
    }
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    SEED.store(x, Ordering::Relaxed);
    let v = x.wrapping_mul(0x2545F4914F6CDD1D) as f64 / u64::MAX as f64;
    0.5 + v // [0.5, 1.5)
}

/// 失败分类：决定冻多久、冻多宽。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// 凭据失效 / 被风控（401/403）→ 账号级拉黑
    Auth,
    /// 限流（429）→ (账号,模型) 指数退避
    Rate,
    /// 网络错误 / 5xx / 超时 → (账号,模型) 指数退避
    Transient,
}

pub fn classify(status: u16) -> FailKind {
    match status {
        401 | 403 => FailKind::Auth,
        429 => FailKind::Rate,
        _ => FailKind::Transient,
    }
}

/// InFlight 租约：Drop 时自动释放，杜绝「提前 return 忘记减计数」。
pub struct Lease<'a> {
    pool: &'a Pool,
    key: String,
}

impl<'a> Drop for Lease<'a> {
    fn drop(&mut self) {
        if let Ok(mut m) = self.pool.inflight.lock() {
            if let Some(n) = m.get_mut(&self.key) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(&self.key);
                }
            }
        }
    }
}

#[derive(Default)]
struct CoolEntry {
    /// 冷却截止秒（0 = 不在冷却）
    until: f64,
    /// 连续失败次数（指数退避的指数）
    fails: u32,
}

pub struct Pool {
    /// `平台|账号|模型` → 在途数
    inflight: Mutex<HashMap<String, usize>>,
    /// `账号|模型` → 冷却状态
    cooldown: Mutex<HashMap<String, CoolEntry>>,
    /// `平台|账号` → WAF 拉黑截止秒（账号级，跨模型）
    waf: Mutex<HashMap<String, f64>>,
    /// `平台|账号|模型` → 最近被选中的时间戳列表
    recent: Mutex<HashMap<String, Vec<f64>>>,
}

impl Default for Pool {
    fn default() -> Self {
        Pool {
            inflight: Mutex::new(HashMap::new()),
            cooldown: Mutex::new(HashMap::new()),
            waf: Mutex::new(HashMap::new()),
            recent: Mutex::new(HashMap::new()),
        }
    }
}

impl Pool {
    pub fn new() -> Self {
        Pool::default()
    }

    fn cd_key(account_id: &str, model: &str) -> String {
        format!("{}|{}", account_id, model)
    }

    fn waf_key(platform: &str, account_id: &str) -> String {
        format!("{}|{}", platform, account_id)
    }

    fn in_key(platform: &str, account_id: &str, model: &str) -> String {
        format!("{}|{}|{}", platform, account_id, model)
    }

    // ── InFlight 租约 ────────────────────────────────────────

    /// 尝试取一个在途租约。同 (平台,账号,模型) 已达并发上限 → `None`（选号跳过）。
    pub fn try_acquire<'a>(
        &'a self,
        platform: &str,
        account_id: &str,
        model: &str,
    ) -> Option<Lease<'a>> {
        let key = Self::in_key(platform, account_id, model);
        let mut m = self.inflight.lock().ok()?;
        let n = m.entry(key.clone()).or_insert(0);
        if *n >= MAX_INFLIGHT {
            return None;
        }
        *n += 1;
        Some(Lease { pool: self, key })
    }

    /// 当前在途数（仅 snapshot / 测试用）。
    pub fn inflight_count(&self, platform: &str, account_id: &str, model: &str) -> usize {
        self.inflight
            .lock()
            .map(|m| {
                m.get(&Self::in_key(platform, account_id, model))
                    .copied()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    // ── 冷却 / WAF ───────────────────────────────────────────

    /// 该 (账号,模型) 是否正在冷却，或账号是否被 WAF 拉黑。
    pub fn is_blocked(&self, platform: &str, account_id: &str, model: &str) -> bool {
        let now = now_secs();
        if let Ok(mut w) = self.waf.lock() {
            if let Some(until) = w.get(&Self::waf_key(platform, account_id)).copied() {
                if now < until {
                    return true;
                }
                w.remove(&Self::waf_key(platform, account_id));
            }
        }
        if let Ok(mut c) = self.cooldown.lock() {
            if let Some(e) = c.get_mut(&Self::cd_key(account_id, model)) {
                if now < e.until {
                    return true;
                }
                // 冷却已过：清零失败计数（下次失败从基数重新退避）
                e.until = 0.0;
                e.fails = 0;
            }
        }
        false
    }

    /// 剩余冷却秒数（展示用；不在冷却 → 0）。
    pub fn cooldown_left(&self, account_id: &str, model: &str) -> f64 {
        let now = now_secs();
        self.cooldown
            .lock()
            .ok()
            .and_then(|c| c.get(&Self::cd_key(account_id, model)).map(|e| e.until))
            .map(|until| (until - now).max(0.0))
            .unwrap_or(0.0)
    }

    pub fn waf_left(&self, platform: &str, account_id: &str) -> f64 {
        let now = now_secs();
        self.waf
            .lock()
            .ok()
            .and_then(|w| w.get(&Self::waf_key(platform, account_id)).copied())
            .map(|until| (until - now).max(0.0))
            .unwrap_or(0.0)
    }

    /// 成功：清掉该 (账号,模型) 的冷却与失败计数。
    pub fn mark_ok(&self, account_id: &str, model: &str) {
        if let Ok(mut c) = self.cooldown.lock() {
            c.remove(&Self::cd_key(account_id, model));
        }
    }

    /// 失败：按错误类型决定冻结范围与时长。
    /// - Auth（401/403）→ 账号级拉黑 `WAF_TTL`（凭据失效不会自愈）
    /// - Rate / Transient → (账号,模型) 指数退避 `min(base·2^n, max) × 抖动`
    pub fn mark_fail(&self, platform: &str, account_id: &str, model: &str, status: u16) {
        match classify(status) {
            FailKind::Auth => {
                if let Ok(mut w) = self.waf.lock() {
                    w.insert(Self::waf_key(platform, account_id), now_secs() + WAF_TTL * jitter());
                }
                // 同时清掉模型级冷却，避免两条记录互相遮挡
                if let Ok(mut c) = self.cooldown.lock() {
                    c.remove(&Self::cd_key(account_id, model));
                }
            }
            kind => {
                let mut c = match self.cooldown.lock() {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let e = c.entry(Self::cd_key(account_id, model)).or_default();
                e.fails = e.fails.saturating_add(1);
                let base = if kind == FailKind::Rate {
                    BASE_COOLDOWN * 2.0
                } else {
                    BASE_COOLDOWN
                };
                let raw = (base * 2f64.powi(e.fails as i32 - 1)).min(MAX_COOLDOWN);
                e.until = now_secs() + raw * jitter();
            }
        }
    }

    /// 手动解除某账号的 WAF 拉黑（用户在面板上「重新启用」时用）。
    pub fn clear_waf(&self, platform: &str, account_id: &str) {
        if let Ok(mut w) = self.waf.lock() {
            w.remove(&Self::waf_key(platform, account_id));
        }
    }

    /// 清空全部治理状态（账号池大改后调用）。
    pub fn reset(&self) {
        if let Ok(mut g) = self.inflight.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.cooldown.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.waf.lock() {
            g.clear();
        }
        if let Ok(mut g) = self.recent.lock() {
            g.clear();
        }
    }

    /// 忘掉某个账号的全部治理状态（账号被删除时调用）。
    /// 不清的话 `/api/route` 的快照会显示一个已经不存在的账号在冷却/拉黑，
    /// 用户会以为面板闹鬼。
    pub fn forget_account(&self, account_id: &str) {
        if let Ok(mut m) = self.inflight.lock() {
            m.retain(|k, _| k.splitn(3, '|').nth(1) != Some(account_id));
        }
        if let Ok(mut m) = self.cooldown.lock() {
            m.retain(|k, _| k.splitn(2, '|').next() != Some(account_id));
        }
        if let Ok(mut m) = self.waf.lock() {
            m.retain(|k, _| k.splitn(2, '|').nth(1) != Some(account_id));
        }
        if let Ok(mut m) = self.recent.lock() {
            m.retain(|k, _| k.splitn(3, '|').nth(1) != Some(account_id));
        }
    }

    // ── RecentPick ───────────────────────────────────────────

    /// 记录一次选中（在真正发起请求前调用）。
    pub fn note_pick(&self, platform: &str, account_id: &str, model: &str) {
        let key = Self::in_key(platform, account_id, model);
        let now = now_secs();
        if let Ok(mut r) = self.recent.lock() {
            let v = r.entry(key).or_default();
            v.retain(|t| now - *t < RECENT_WINDOW);
            v.push(now);
        }
    }

    /// 窗口内被选中的次数（排序键：越小越该被选）。
    pub fn recent_count(&self, platform: &str, account_id: &str, model: &str) -> usize {
        let now = now_secs();
        self.recent
            .lock()
            .ok()
            .map(|r| {
                r.get(&Self::in_key(platform, account_id, model))
                    .map(|v| v.iter().filter(|t| now - **t < RECENT_WINDOW).count())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    /// 窗口内选中次数是否已达上限（用于同级排序惩罚，不硬过滤）。
    pub fn recent_saturated(&self, platform: &str, account_id: &str, model: &str) -> bool {
        self.recent_count(platform, account_id, model) >= RECENT_MAX
    }

    // ── 快照（/api/route 的 pool 段） ─────────────────────────

    /// 治理状态摘要（同时惰性清理过期项）。
    pub fn snapshot(&self) -> serde_json::Value {
        use serde_json::{json, Value};
        let now = now_secs();
        let inflight: Vec<Value> = self
            .inflight
            .lock()
            .map(|m| {
                m.iter()
                    .filter(|(_, n)| **n > 0)
                    .map(|(k, n)| {
                        let p: Vec<&str> = k.splitn(3, '|').collect();
                        json!({
                            "platform": p.first().copied().unwrap_or(""),
                            "account": p.get(1).copied().unwrap_or(""),
                            "model": p.get(2).copied().unwrap_or(""),
                            "n": n,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut cooling: Vec<Value> = Vec::new();
        if let Ok(mut c) = self.cooldown.lock() {
            c.retain(|_, e| now < e.until);
            for (k, e) in c.iter() {
                let p: Vec<&str> = k.splitn(2, '|').collect();
                cooling.push(json!({
                    "account": p.first().copied().unwrap_or(""),
                    "model": p.get(1).copied().unwrap_or(""),
                    "left_s": (e.until - now).round() as i64,
                    "fails": e.fails,
                }));
            }
        }
        let mut waf: Vec<Value> = Vec::new();
        if let Ok(mut w) = self.waf.lock() {
            w.retain(|_, until| now < *until);
            for (k, until) in w.iter() {
                let p: Vec<&str> = k.splitn(2, '|').collect();
                waf.push(json!({
                    "platform": p.first().copied().unwrap_or(""),
                    "account": p.get(1).copied().unwrap_or(""),
                    "left_s": (until - now).round() as i64,
                }));
            }
        }
        json!({
            "max_inflight": MAX_INFLIGHT,
            "cooldown_base_s": BASE_COOLDOWN,
            "cooldown_max_s": MAX_COOLDOWN,
            "waf_ttl_s": WAF_TTL,
            "recent_window_s": RECENT_WINDOW,
            "recent_max": RECENT_MAX,
            "inflight": inflight,
            "cooling": cooling,
            "waf": waf,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflight_lease_caps_concurrency() {
        let p = Pool::new();
        let a = p.try_acquire("apk-doubao", "acc1", "m");
        assert!(a.is_some(), "第一个租约应成功");
        let b = p.try_acquire("apk-doubao", "acc1", "m");
        assert!(b.is_some(), "第二个租约应成功（上限 2）");
        let c = p.try_acquire("apk-doubao", "acc1", "m");
        assert!(c.is_none(), "第三个应被拒绝");
        assert_eq!(p.inflight_count("apk-doubao", "acc1", "m"), 2);
        // 不同模型不共享配额
        assert!(p.try_acquire("apk-doubao", "acc1", "other").is_some());
        // 不同账号不共享配额
        assert!(p.try_acquire("apk-doubao", "acc2", "m").is_some());
        drop(a);
        assert_eq!(p.inflight_count("apk-doubao", "acc1", "m"), 1);
        assert!(p.try_acquire("apk-doubao", "acc1", "m").is_some(), "释放后应可再取");
    }

    #[test]
    fn cooldown_grows_exponentially_and_expires() {
        let p = Pool::new();
        assert!(!p.is_blocked("apk-doubao", "acc1", "m"));
        p.mark_fail("apk-doubao", "acc1", "m", 500);
        assert!(p.is_blocked("apk-doubao", "acc1", "m"), "失败后应冷却");
        let first = p.cooldown_left("acc1", "m");
        assert!((15.0..45.0).contains(&first), "首次冷却基数 30s×抖动，实际 {}", first);
        // 只冻 (账号,模型)：换模型立刻可用
        assert!(!p.is_blocked("apk-doubao", "acc1", "other"));
        // 再次失败 → 退避翻倍（名义 60s×抖动，落 30~90s；用区间断言，避免随机抖动的大小比较 flake）
        p.mark_fail("apk-doubao", "acc1", "m", 500);
        let second = p.cooldown_left("acc1", "m");
        assert!((30.0..90.0).contains(&second), "二次冷却名义 60s×抖动，实际 {}", second);
        // 上限 900s（名义）× 抖动 → 区间 450~1350；与首次冷却下界(15)有确定性 gap，可证退避增长
        for _ in 0..10 {
            p.mark_fail("apk-doubao", "acc1", "m", 500);
        }
        let capped = p.cooldown_left("acc1", "m");
        assert!((450.0..1350.0).contains(&capped), "退避封顶应在 450~1350，实际 {}", capped);
        assert!(capped > first + 100.0, "退避应显著长于首次（{} vs {}）", capped, first);
        // 成功清冷却
        p.mark_ok("acc1", "m");
        assert!(!p.is_blocked("apk-doubao", "acc1", "m"));
    }

    #[test]
    fn auth_failure_blocks_whole_account_across_models() {
        let p = Pool::new();
        p.mark_fail("wb-gateway", "acc1", "gpt-5", 401);
        assert!(p.is_blocked("wb-gateway", "acc1", "gpt-5"));
        assert!(
            p.is_blocked("wb-gateway", "acc1", "another-model"),
            "401 是账号级拉黑，应跨模型生效"
        );
        // 403（被风控）同样处理
        p.mark_fail("wb-gateway", "acc2", "m", 403);
        assert!(p.is_blocked("wb-gateway", "acc2", "m"));
        // 别的账号不受影响
        assert!(!p.is_blocked("wb-gateway", "acc3", "m"));
        // 其它平台不受影响
        assert!(!p.is_blocked("apk-doubao", "acc1", "gpt-5"));
        // WAF 时长 = 1800s × 抖动[0.5,1.5) → 断言下限即可（别写死上限，那是 flake）
        let left = p.waf_left("wb-gateway", "acc1");
        assert!(left > 800.0 && left <= 2700.0, "WAF 剩余 {} 不在 [900,2700) 量级", left);
    }

    #[test]
    fn recent_pick_counts_within_window() {
        let p = Pool::new();
        assert_eq!(p.recent_count("apk-doubao", "acc1", "m"), 0);
        for _ in 0..3 {
            p.note_pick("apk-doubao", "acc1", "m");
        }
        assert_eq!(p.recent_count("apk-doubao", "acc1", "m"), 3);
        assert!(p.recent_saturated("apk-doubao", "acc1", "m"));
        // 不同模型分别计数
        assert_eq!(p.recent_count("apk-doubao", "acc1", "other"), 0);
    }

    #[test]
    fn jitter_stays_in_range() {
        for _ in 0..200 {
            let j = jitter();
            assert!((0.5..1.5).contains(&j), "抖动越界: {}", j);
        }
    }

    #[test]
    fn classify_maps_status() {
        assert_eq!(classify(401), FailKind::Auth);
        assert_eq!(classify(403), FailKind::Auth);
        assert_eq!(classify(429), FailKind::Rate);
        assert_eq!(classify(500), FailKind::Transient);
        assert_eq!(classify(0), FailKind::Transient);
        assert_eq!(classify(400), FailKind::Transient);
    }

    #[test]
    fn forget_account_clears_all_its_state() {
        let p = Pool::new();
        let _l = p.try_acquire("apk-loomy", "gone", "m");
        p.mark_fail("apk-loomy", "gone", "m", 429);
        p.mark_fail("apk-loomy", "gone", "m2", 403);
        p.note_pick("apk-loomy", "gone", "m");
        // 另一个账号的状态不能被误删
        p.mark_fail("apk-loomy", "stay", "m", 429);
        p.forget_account("gone");
        let s = p.snapshot();
        assert_eq!(s["cooling"].as_array().unwrap().len(), 1, "只应剩 stay 的冷却");
        assert_eq!(s["waf"].as_array().unwrap().len(), 0, "gone 的 WAF 应被清掉");
        assert!(p.is_blocked("apk-loomy", "stay", "m"), "别人的冷却不能被误删");
    }

    #[test]
    fn snapshot_shape_is_stable() {
        let p = Pool::new();
        let _l = p.try_acquire("apk-doubao", "acc1", "m");
        p.mark_fail("wb-gateway", "acc2", "m", 429);
        p.mark_fail("wb-gateway", "acc3", "m", 403);
        let s = p.snapshot();
        for k in [
            "max_inflight",
            "cooldown_base_s",
            "cooldown_max_s",
            "waf_ttl_s",
            "recent_window_s",
            "recent_max",
            "inflight",
            "cooling",
            "waf",
        ] {
            assert!(s.get(k).is_some(), "snapshot 缺字段 {}", k);
        }
        assert_eq!(s["inflight"].as_array().unwrap().len(), 1);
        assert_eq!(s["cooling"].as_array().unwrap().len(), 1);
        assert_eq!(s["waf"].as_array().unwrap().len(), 1);
    }
}
