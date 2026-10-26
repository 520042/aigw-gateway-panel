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
/// 车道级封锁/地域封锁的冷却时长：只冻该 (账号,模型) 一小段，不拉黑账号。
/// 短于 WAF_SECS 是因为这类封锁随时可能解除（上游调整准入策略、换出口 IP），
/// 冻太久反而让车道在恢复后还白等半小时。
const LANE_COOL_SECS: f64 = 120.0; // 2min
const RECENT_WINDOW: Duration = Duration::from_secs(60);

/// 额度（402）按日重置的上游：冻到**次日 04:00**（本地时区）再放行。
/// 对齐 workbuddy2api 的「402 硬冷却至次日 04:00」口径——按日额度在零点前后
/// 结算，04:00 是上游实际的额度切换点（抓包实测）。
/// 返回距下一个 04:00 的秒数（最少 1 小时，避免恰好 03:59 时只冻 60 秒）。
fn quota_hold_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // 本地时区偏移（Windows/Unix 都不引 chrono，这里按 UTC+8 口径；
    // 面板面向国内上游，固定 UTC+8 与「北京时间 04:00」一致）
    let local = now + 8 * 3600;
    let secs_into_day = local.rem_euclid(86400);
    let target = 4 * 3600; // 04:00
    let mut delta = target - secs_into_day;
    if delta <= 0 {
        delta += 86400;
    }
    (delta as f64).max(3600.0)
}
const RECENT_YIELD: usize = 2;

#[derive(Debug, Clone)]
struct CoolState {
    until: Instant,
    strikes: u32,
}

/// 账号健康结论：Pool::report 按错误类别写入，面板账号表直接展示。
#[derive(Debug, Clone)]
struct HealthState {
    /// 机器可读：ok / auth / quota / rate。
    status: &'static str,
    /// 人话说明（面板原样显示）。验活失败时由调用方给出具体原因，故用 String。
    label: String,
    at: Instant,
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
    /// account -> 最近一次真实结果（面板「健康」列直接读它）。
    /// 只存内存不落盘：进程重启后旧结论未必成立，重新验活比读历史更准。
    health: HashMap<String, HealthState>,
    /// ★ 2026-10-26（P1）：account -> 积分余额快照（值, 记录时刻）。
    /// 供「快过期积分优先」选号用：积分少且久未刷新的号先消耗掉，
    /// 避免小余额账号永远排在后面、积分到期作废。
    credits: HashMap<String, (f64, Instant)>,
}
/// 一个账号当前的阻断状态（面板账号表「健康」列用）。
///
/// `waf_s` 是整号限速剩余秒数，`cooling_s` 是该号在任一模型上最长的冷却剩余。
/// 两者都为 0 表示这个号此刻没有被动过手脚。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blocked {
    pub waf_s: f64,
    pub cooling_s: f64,
}

impl Blocked {
    /// 完全没有阻断。
    pub fn is_clear(&self) -> bool {
        self.waf_s <= 0.0 && self.cooling_s <= 0.0
    }
}


/// 号池：并发租约 + 冷却/WAF + 会话粘性的集中治理点（每行一个账号）。
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
        // 锁中毒不能让租约**静默漏还**：漏还的后果是该 (平台,账号,模型) 的在飞计数
        // 永远卡在 MAX_INFLIGHT，此后所有请求都被判「provider busy」429，
        // 而这个账号在面板上看起来完全健康 —— 极难排查。统一 into_inner 继续用数据。
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = g.inflight.get_mut(key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.inflight.remove(key);
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
        // 锁中毒时旧实现 `.lock().ok()?` 返回 None，上层把它读成「没有可用账号」
        // 并回 503「账号当前全部不可用（冷却中或凭据失效隔离）」—— 一句与真实原因
        // 完全无关的误导。中毒不是数据坏了，继续用里面的数据即可。
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
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
        platform: &str,
        accounts: &[String],
        model: &str,
    ) -> Option<String> {
        let now = Instant::now();
        // 锁中毒不能静默返回 None：那会让 pick 表现得像「全部被挡」，进而把整个平台的
        // 请求变成 503（或旧版那样越过隔离硬走）。统一 into_inner 继续用里面的数据。
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // 三种「挡住」的原因必须分开记：冷却中的可以半开探测，纯让位的可以放行，
        // 而在飞已满的账号**不能**被半开选中（否则 pick 说可用、acquire 立刻判 429）。
        let mut cooling: Vec<(String, f64)> = Vec::new(); // (account, 冷却剩余秒)
        let mut yielded: Vec<String> = Vec::new(); // 仅 RecentPick 让位
        let mut busy = 0usize; // 在飞已满
        // ★ 2026-10-26（P1，对齐 workbuddy2api「快过期积分加权」）：
        // 先把**所有通过硬门槛**（非 WAF、非冷却、在飞未满）的候选收集起来，
        // 再按「积分余额低者优先」排序后取第一个。原实现是遍历顺序先到先得——
        // 账号池按添加顺序排列，于是永远只用第一个号，其余号的积分到期作废。
        // 排序口径：有余额快照的按余额升序（少的下次先用掉），没快照的排最后
        // （不知道余额就不该抢在已知小余额的号前面）。
        let mut candidates: Vec<(f64, bool, usize, String)> = Vec::new(); // (credit, 有快照, 原序, account)
        for (idx, a) in accounts.iter().enumerate() {
            if g.waf.get(a).map(|t| *t > now).unwrap_or(false) {
                continue;
            }
            let inflight_key = (platform.to_string(), a.clone(), model.to_string());
            if g.inflight
                .get(&inflight_key)
                .map(|n| *n >= MAX_INFLIGHT)
                .unwrap_or(false)
            {
                busy += 1;
                continue;
            }
            if let Some(cs) = g.cooldown.get(&(a.to_string(), model.to_string())) {
                if cs.until > now {
                    cooling.push((a.clone(), cs.until.duration_since(now).as_secs_f64()));
                    continue;
                }
            }
            let (credit, has) = match g.credits.get(a) {
                Some((c, _)) => (*c, true),
                None => (f64::MAX, false),
            };
            candidates.push((credit, has, idx, a.clone()));
        }
        // 余额升序；同余额/都没快照时保持原有顺序（稳定排序，不打乱既有行为）
        candidates.sort_by(|x, y| {
            x.0.partial_cmp(&y.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(x.2.cmp(&y.2))
        });
        for (_, _, _, a) in &candidates {
            // RecentPick：60s 窗口 ≥2 次 → 有更早候选可用时让位
            let picks = g
                .recent
                .get(a)
                .map(|v| v.iter().filter(|t| now.duration_since(**t) < RECENT_WINDOW).count())
                .unwrap_or(0);
            if picks >= RECENT_YIELD {
                yielded.push(a.clone());
                continue;
            }
            g.recent.entry(a.to_string()).or_default().push(now);
            if let Some(v) = g.recent.get_mut(a) {
                v.retain(|t| now.duration_since(*t) < RECENT_WINDOW);
            }
            return Some(a.clone());
        }
        // 全被挡 → 半开：优先真正在冷却的（剩余最短者，清冷却探测）
        cooling.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((account, remain)) = cooling.first() {
            g.cooldown.remove(&(account.clone(), model.to_string()));
            tracing::warn!(
                "pool 半开：全部候选被挡，清冷却探测 {}（剩余 {:.0}s）",
                account,
                remain
            );
            return Some(account.clone());
        }
        // 其次放行「只是最近被选多了一次」的账号，并清空它的 recent 窗口。
        // 旧实现只 return、不重置 recent，于是下次 pick 又看到它 ≥2 次、又走同一分支，
        // 同一个账号被无限重复选中，防惊群完全失效。
        if let Some(account) = yielded.first() {
            g.recent.remove(account);
            return Some(account.clone());
        }
        // 剩下的全是「在飞已满」：真实原因是并发打满，交由上层回 429，不能假装有账号可用。
        let _ = busy;
        None
    }

    /// 该 (账号,模型) 当前是否可直接使用（未 WAF 拉黑、未冷却）。
    /// 会话粘性路由命中后必须再过这一关，否则粘性会把请求钉死在刚被拉黑的账号上。
    pub fn available(&self, account: &str, model: &str) -> bool {
        let now = Instant::now();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.waf.get(account).map(|t| *t > now).unwrap_or(false) {
            return false;
        }
        match g.cooldown.get(&(account.to_string(), model.to_string())) {
            Some(cs) => cs.until <= now,
            None => true,
        }
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
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if ok {
                g.cooldown.remove(&(account.to_string(), model.to_string()));
                g.waf.remove(account);
                g.recent.remove(account);
                g.health.insert(
                    account.to_string(),
                    HealthState { status: "ok", label: "正常".to_string(), at: now },
                );
                return;
            }
            match class {
                // 账号级问题：401/403 → 账号级拉黑 30min。
                // 之前只在 waf_fingerprint（HTTP 200 + 假成功）时才拉黑，真实 401
                // 只冻 30s 的 (账号,模型) 对，过期后继续拿废凭据反复打上游。
                ErrorClass::Auth | ErrorClass::Waf => {
                    g.waf.insert(account.to_string(), now + Duration::from_secs_f64(WAF_SECS));
                    g.health.insert(
                        account.to_string(),
                        HealthState {
                            status: "auth",
                            label: "鉴权失败：凭据可能已过期，需要重新登录/更换".to_string(),
                            at: now,
                        },
                    );
                    let _ = waf_fingerprint;
                }
                ErrorClass::Quota => {
                    // ★ 额度耗尽不能复用 waf（账号级 30min 拉黑）：很多上游的额度是按小时/
                    //   按天重置的，一记 402 就把凭据隔离半小时，比实际需要久得多；而且 waf
                    //   在 pick 里是 continue（连半开探测都不给）。这里改成「该 (账号,模型)
                    //   一次长冷却」——到期后 pick 的半开会放行一次探测，能自然恢复。
                    // ★ 2026-10-26（P1，对齐 workbuddy2api）：额度按**日**重置的上游
                    //   （WorkBuddy 族）应冻到次日 04:00 而不是固定 30 分钟——30 分钟
                    //   后醒来额度依然为 0，等于每半小时白打一次上游。取两者较晚者。
                    let key = (account.to_string(), model.to_string());
                    // 先读旧 strikes 再 insert（同一句里同时 get/insert 会触发 E0502 借用冲突）
                    let strikes = g.cooldown.get(&key).map(|c| c.strikes).unwrap_or(0);
                    let quota_secs = quota_hold_secs();
                    g.cooldown.insert(
                        key,
                        CoolState {
                            until: now + Duration::from_secs_f64(WAF_SECS.max(quota_secs)),
                            // 不递增 strikes：额度耗尽不是「这个模型不稳」，别把指数退避带偏
                            strikes,
                        },
                    );
                    g.health.insert(
                        account.to_string(),
                        HealthState {
                            status: "quota",
                            label: "额度不足：上游返回 402，等额度重置".to_string(),
                            at: now,
                        },
                    );
                }
                // 请求本身不合法（上游 400/404/410/413/415/422）：既不是账号的问题，也不是
                // 上游抖了。不冷却、不拉黑、不计 strikes —— 否则调用方写错一个参数就能
                // 把这个账号/模型冻住，甚至把整个平台熔断。
                ErrorClass::Rejected => {}
                // 车道级封锁（OpenCode 403 FreeTierError）/地域封锁（403 RegionError）：
                // ★ 绝不能走 Auth 分支。Auth 是「账号级 30 分钟拉黑 + 提示重新登录」，而
                //   免凭据车道根本没有凭据可重登，模型侧的地域限制也不会因为换号而消失。
                //   但也不能完全不记：否则每次请求都白打一次上游。折中——只冻该 (账号,模型)
                //   一小段（LANE_COOL_SECS），strikes 不递增（这不是模型不稳），到期自动恢复，
                //   面板上明确写「车道被封锁」而不是「鉴权失败」。
                ErrorClass::LaneLocked | ErrorClass::Region => {
                    let key = (account.to_string(), model.to_string());
                    let strikes = g.cooldown.get(&key).map(|c| c.strikes).unwrap_or(0);
                    g.cooldown.insert(
                        key,
                        CoolState {
                            until: now + Duration::from_secs_f64(LANE_COOL_SECS),
                            strikes,
                        },
                    );
                    let (status, label) = if class == ErrorClass::Region {
                        ("region", "地域封锁：上游对该地区不提供这个模型，换车道或换网络出口")
                    } else {
                        (
                            "lane",
                            "车道被封锁：上游按客户端指纹拒绝整条免凭据车道（不是你的凭据问题，也无需重新登录）",
                        )
                    };
                    g.health.insert(
                        account.to_string(),
                        HealthState {
                            status,
                            label: label.to_string(),
                            at: now,
                        },
                    );
                }
                // 限流：账号本身没坏，但面板上要能看出「这个号现在被限流」。
                ErrorClass::Rate => {
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
                    g.health.insert(
                        account.to_string(),
                        HealthState {
                            status: "rate",
                            label: "被上游限流：稍后会自动恢复".to_string(),
                            at: now,
                        },
                    );
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
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.waf.remove(account);
            g.recent.remove(account);
            g.health.remove(account);
            g.cooldown
                .retain(|(a, _), _| a != account);
            g.inflight
                .retain(|(_, a, _), _| a != account);
        }
    }

    /// 账号最近一次健康结论：(status, label, 距今秒数)。
    /// 面板账号表用它显示「正常 / 鉴权失败 / 额度不足 / 被限流」。
    pub fn health_of(&self, account: &str) -> Option<(String, String, u64)> {
        let now = Instant::now();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.health.get(account).map(|h| {
            (
                h.status.to_string(),
                h.label.clone(),
                now.saturating_duration_since(h.at).as_secs(),
            )
        })
    }

    /// 显式写入健康结论（面板「立即检查」验活后调用）。
    pub fn mark_health(&self, account: &str, ok: bool, label: &str) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.health.insert(
            account.to_string(),
            HealthState {
                status: if ok { "ok" } else { "auth" },
                label: if label.is_empty() {
                    if ok { "正常".to_string() } else { "验活未通过".to_string() }
                } else {
                    label.to_string()
                },
                at: Instant::now(),
            },
        );
    }

    /// ★ 2026-10-26（P1）：记录某账号的积分余额快照（供「快过期优先」选号）。
    /// 由签到/成长任务/余额查询路径在拿到真实余额时调用。`None` 表示上游没给数字，
    /// 此时不记录（不能用 0 覆盖已有值——0 会被误判成「余额耗尽」）。
    pub fn record_credit(&self, account: &str, credit: Option<f64>) {
        let Some(c) = credit else { return };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.credits.insert(account.to_string(), (c, Instant::now()));
    }

    /// 某账号的积分余额（面板展示用）。`None` = 从未记录过。
    pub fn credit_of(&self, account: &str) -> Option<f64> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.credits.get(account).map(|(c, _)| *c)
    }

    /// 查一个账号此刻是否被冷却 / WAF 整号限速。
    ///
    /// 面板的「健康」列是**账号级**的：一个号在某个模型上冷却 40 秒，用户需要看到
    /// 「这个号现在别用」，而不是自己去翻号池明细。所以冷却按账号取**最长剩余**，
    /// 不按 (账号,模型) 逐条展开。
    pub fn blocked_of(&self, account: &str) -> Blocked {        let now = Instant::now();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let waf_s = g
            .waf
            .get(account)
            .map(|t| t.saturating_duration_since(now).as_secs_f64())
            .unwrap_or(0.0);
        let cooling_s = g
            .cooldown
            .iter()
            .filter(|((a, _), _)| a == account)
            .map(|(_, c)| c.until.saturating_duration_since(now).as_secs_f64())
            .fold(0.0_f64, f64::max);
        Blocked { waf_s, cooling_s }
    }

    /// 手动解冻：清掉该账号的整号限速与全部 (账号,模型) 冷却。
    ///
    /// 为什么需要：冷却时长是**猜的**（WAF 30 分钟、额度 402 也是 30 分钟），
    /// 而上游的实际恢复时间常常短得多。面板既然能把「还要等 109 秒」显示出来，
    /// 就该允许用户在确认上游已经好了之后一键放行，而不是干等或重启服务。
    ///
    /// 只放行**未来的请求**：`health`（最近一次失败结论）是历史，不动它 ——
    /// 解冻不等于「验证过这个号是好的」，那要由一次真实请求或「立即检查」来证明。
    /// 返回 (是否清了整号限速, 清掉的 (账号,模型) 冷却条数)。
    pub fn thaw(&self, account: &str) -> (bool, usize) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let had_waf = g.waf.remove(account).is_some();
        let before = g.cooldown.len();
        g.cooldown.retain(|(a, _), _| a != account);
        let cooled = before - g.cooldown.len();
        // 连击计数也要清：否则解冻后第一次失败就直接跳到更高档退避，
        // 用户手动放行的意义就被吃掉了。
        g.recent.remove(account);
        (had_waf, cooled)
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

/// 会话粘性路由（对齐 workbuddy2api `StickyRouter` + loomy2api「粘性键取最后一条
/// user」修正）：会话键 → 账号映射，TTL 滚动续期。
///
/// 为什么取「最后一条 user」而不是首条：通用客户端（Cline/Roo/Codex）重发全历史时
/// 首条 user 消息恒定不变，若以首条做键会令整个会话永远钉在同一个账号上
/// （单号霸占、其余号饿死）；最后一条 user 随对话推进变化，粘性与负载均衡兼顾。
const STICKY_TTL: Duration = Duration::from_secs(1800);
/// 粘性表条目上限。
///
/// 会话键来自请求体（`metadata.conversation_id` 等客户端可控字段），TTL 只负责「过期」，
/// 不负责「数量」——一个每分钟换一个会话 id 的客户端，30 分钟就能塞进几万条；
/// 而 `route()` 每次请求都要 `retain` 全表，条目数直接决定每个请求的固定开销。
/// 超过上限时丢最久未用的（它们本来也快到期了）。
pub const STICKY_MAX_ENTRIES: usize = 8192;

/// 过期清理 + 数量封顶（见 [`STICKY_MAX_ENTRIES`]）。
fn prune_sticky(map: &mut HashMap<String, (String, Instant)>, now: Instant) {
    map.retain(|_, (_, t)| now.duration_since(*t) < STICKY_TTL);
    if map.len() <= STICKY_MAX_ENTRIES {
        return;
    }
    // 按最后使用时间排序，丢掉最久未用的那部分（刚刚才 retain 过，所以都是在 TTL 内
    // 的活跃条目，丢掉的只是「相对更旧」，不影响正在进行的会话：它们的 key 下次
    // 请求会重新 bind，只是可能换一个账号）。
    let mut ages: Vec<(String, Instant)> = map.iter().map(|(k, (_, t))| (k.clone(), *t)).collect();
    ages.sort_by_key(|(_, t)| *t);
    let excess = map.len() - STICKY_MAX_ENTRIES;
    for (k, _) in ages.into_iter().take(excess) {
        map.remove(&k);
    }
}

pub struct StickyRouter {
    map: Mutex<HashMap<String, (String, Instant)>>,
}

impl StickyRouter {
    pub fn new() -> StickyRouter {
        StickyRouter {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// 命中且账号仍在候选列表 → 复用该账号（滚动 TTL）；否则 None，调用方走正常
    /// 选号后 [`Self::bind`]。
    pub fn route(&self, key: &str, accounts: &[String]) -> Option<String> {
        let now = Instant::now();
        // 锁中毒时旧实现返回 None（「没命中粘性」），随后 bind() 也因 Ok() 分支跳过，
        // 于是粘性路由整个静默失效：同一会话的请求开始在多账号间乱跳。
        let mut g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        prune_sticky(&mut g, now);
        let hit: Option<(String, Instant)> = g.get(key).cloned();
        if let Some((acc, t)) = hit {
            if now.duration_since(t) < STICKY_TTL && accounts.iter().any(|a| *a == acc) {
                g.insert(key.to_string(), (acc.clone(), now));
                return Some(acc);
            }
        }
        None
    }

    /// 绑定/重绑（滚动 TTL）。选号成功后调用。
    pub fn bind(&self, key: &str, account: &str) {
        let now = Instant::now();
        let mut g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(key.to_string(), (account.to_string(), now));
        // 插完再清理：先清后插会稳定地多出 1 条（清理完是 MAX，插入变 MAX+1），
        // 封顶就永远差一条。
        prune_sticky(&mut g, now);
    }

    /// 会话结束 / 账号被禁用时解绑。
    pub fn unbind(&self, key: &str) {
        let mut g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        g.remove(key);
    }

    /// 账号被删/停用时清掉所有指向它的粘性绑定。
    ///
    /// 这里**必须**生效：漏清会让后续请求继续钉在已被删除的账号上，
    /// 表现为删号之后仍然 401。
    pub fn unbind_account(&self, account: &str) {
        let mut g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|_, (acc, _)| acc != account);
    }

    /// 快照（调试/前端）。
    ///
    /// 注意条目数上限是 `STICKY_MAX_ENTRIES`（8192），每条还带完整会话键，
    /// 全量返回能到几百 KB。给面板/接口用请走 [`snapshot_limited`]。
    pub fn snapshot(&self) -> serde_json::Value {
        self.snapshot_limited(usize::MAX)
    }

    /// 有界快照：只回前 `max_entries` 条（按会话键排序，输出稳定），并给出总数。
    ///
    /// 为什么要限：`/api/pool` 是给人看的诊断接口，把 8192 条会话键全塞过去，
    /// 响应几百 KB，前端也只能渲染前几行。真要全量排查用 `?sticky=full`。
    pub fn snapshot_limited(&self, max_entries: usize) -> serde_json::Value {
        let now = Instant::now();
        let g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let mut all: Vec<(String, String, f64)> = g
            .iter()
            .filter(|(_, (_, t))| now.duration_since(*t) < STICKY_TTL)
            .map(|(k, (acc, t))| {
                (
                    k.clone(),
                    acc.clone(),
                    STICKY_TTL.as_secs_f64() - now.duration_since(*t).as_secs_f64(),
                )
            })
            .collect();
        let total = all.len();
        // 按会话键排序：HashMap 迭代顺序每次不同，不排序会让「同一份数据两次拉取」
        // 的截断结果不一样，排查时没法对比。
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all.truncate(max_entries);
        let truncated = total > all.len();
        let entries: Vec<_> = all
            .into_iter()
            .map(|(key, account, left_s)| {
                json!({ "key": key, "account": account, "left_s": left_s })
            })
            .collect();
        json!({
            "ttl_s": STICKY_TTL.as_secs(),
            "total": total,
            "truncated": truncated,
            "entries": entries,
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

    /// 锁中毒不能让号池「假装没有账号」或「漏还在飞计数」。
    ///
    /// 旧实现 release_inflight / acquire 用 `lock().ok()`：中毒后 acquire 返回 None，
    /// 上层读成「账号全部不可用」→ 503，而真实原因（一次 panic 毒化了锁）完全不可见；
    /// release_inflight 更糟——静默漏还，该 (平台,账号,模型) 的在飞计数永远卡在 2，
    /// 此后所有请求都 429「provider busy」，账号在面板上却显示健康。
    #[test]
    fn poisoned_lock_does_not_break_lease_accounting() {
        let pool = Pool::new();
        let p2 = pool.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = p2.inner.lock().unwrap();
            panic!("人为毒化");
        }));
        assert!(pool.inner.lock().is_err(), "前提：锁应已中毒");

        let l = pool.acquire("p", "a", "m");
        assert!(l.is_some(), "锁中毒不应被读成「无可用账号」");
        drop(l);
        // 归还必须真的生效：还能连拿 2 个（上限 2），第 3 个才被拒
        let a = pool.acquire("p", "a", "m");
        let b = pool.acquire("p", "a", "m");
        assert!(
            a.is_some() && b.is_some(),
            "Drop 应在中毒后仍把在飞计数归零"
        );
        assert!(pool.acquire("p", "a", "m").is_none(), "上限仍是 2");
    }

    /// 粘性路由的锁中毒不能让会话在多账号间乱跳。
    #[test]
    fn poisoned_sticky_lock_keeps_routing() {
        let r = StickyRouter::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = r.map.lock().unwrap();
            panic!("人为毒化");
        }));
        assert!(r.map.lock().is_err(), "前提：锁应已中毒");
        r.bind("sess-1", "acc-7");
        assert_eq!(
            r.route("sess-1", &["acc-7".to_string()]).as_deref(),
            Some("acc-7"),
            "中毒后 bind/route 仍须生效"
        );
        r.unbind_account("acc-7");
        assert!(r.route("sess-1", &["acc-7".to_string()]).is_none(), "解绑须生效");
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
        // ★ 下界必须比 30×0.5 略低：left_s 是「报错之后」再取快照算出来的，
        //   任何已经流逝的时间都会让它略小于采样值 —— 用 15.0 作闭下界时，
        //   jitter 恰好落在 0.5 附近就会偶发失败（实测在一次全量测试里红过一次）。
        // ★ 下界给到 14.0 / 28.0：jitter 取值域是 [0.5,1.5) 的**开**区间端点，
        //   rand 的 gen_range 在浮点模式下确实可能返回非常接近 0.5 的值，再叠加
        //   「report 里 now 到 snapshot 里 now 之间的流逝 + round()」，14.5 的下界
        //   在一次全量并行跑里红过（实测）。上界不动：「没触顶」这条照样咬得住。
        assert!((14.0..45.0).contains(&first), "首次 30×[0.5,1.5)，得 {first}");
        let second = left_after(2);
        assert!((28.0..90.0).contains(&second), "二次 60×[0.5,1.5)，得 {second}");
        let capped = left_after(9);
        // 下界取 0.5×900 的 0.99 倍再留 1 秒余量：抖动下限是标准值，但 `left_s` 是取快照
        // 时重新算 remaining 的，9 次 report 之间流逝的毫秒会把它压到 450 以下一点点。
        // 上界仍是 1350（1.5×900），所以「没触顶 / 提前触顶」照样咬得住。
        assert!(
            (444.0..1350.0).contains(&capped),
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
    fn lane_locked_does_not_blacklist_account() {
        // 车道级封锁（OpenCode 403 FreeTierError）绝不能走 Auth 分支：
        // 匿名车道没有凭据可重登，把账号拉黑 30min 只会让整条车道白白停摆。
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::LaneLocked, false);
        let snap = pool.snapshot();
        assert_eq!(
            snap["waf"].as_array().unwrap().len(),
            0,
            "车道封锁不应进账号级 waf 拉黑"
        );
        // 只冻 (账号,模型) 一小段，且远短于 WAF 的 30min
        let cooling = snap["cooling"].as_array().unwrap();
        assert_eq!(cooling.len(), 1);
        let left = cooling[0]["left_s"].as_f64().unwrap();
        assert!(left <= LANE_COOL_SECS, "冷却应≈{LANE_COOL_SECS}s，得 {left}s");
        // 同一账号换个模型仍可用（封锁是针对这条车道的这个模型）
        assert!(pool.available("a", "other"));
        // 健康态是 lane 而不是 auth，面板据此显示「车道被封锁」而不是「去重新登录」
        let (status, _label, _age) = pool.health_of("a").unwrap();
        assert_eq!(status, "lane");
    }

    #[test]
    fn region_lock_is_labelled_region_and_never_waf() {
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Region, false);
        let snap = pool.snapshot();
        assert_eq!(snap["waf"].as_array().unwrap().len(), 0);
        assert_eq!(pool.health_of("a").unwrap().0, "region");
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

    // ── StickyRouter ──────────────────────────────────────────

    #[test]
    fn sticky_map_is_bounded_and_drops_oldest() {
        // 会话键是客户端可控的：`bind` 若不封顶，一个每请求换一个 conversation_id 的
        // 客户端就能在 TTL 窗口内把表塞满任意多条，而 route() 每次请求都要全表 retain。
        let r = StickyRouter::new();
        for i in 0..(STICKY_MAX_ENTRIES + 500) {
            r.bind(&format!("k{i}"), "acc");
        }
        let snap = r.snapshot();
        let n = snap["entries"].as_array().unwrap().len();
        assert!(n <= STICKY_MAX_ENTRIES, "粘性表条目必须封顶，实际 {n}");
        // 最新的仍在，最早的已被淘汰
        let last = format!("k{}", STICKY_MAX_ENTRIES + 499);
        assert!(r.route(&last, &["acc".to_string()]).is_some());
        assert!(r.route("k0", &["acc".to_string()]).is_none());
    }

    #[test]
    fn sticky_route_hits_in_candidates() {
        let r = StickyRouter::new();
        r.bind("sess-1", "acc-1");
        let accs = vec!["acc-0".to_string(), "acc-1".to_string()];
        assert_eq!(r.route("sess-1", &accs).as_deref(), Some("acc-1"));
    }

    #[test]
    fn sticky_route_misses_when_account_gone() {
        let r = StickyRouter::new();
        r.bind("sess-1", "acc-9");
        let accs = vec!["acc-0".to_string(), "acc-1".to_string()];
        assert_eq!(r.route("sess-1", &accs), None, "绑定的号不在候选应让位");
        // 未命中不自动绑定
        assert_eq!(r.snapshot()["entries"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn sticky_unbind_clears() {
        let r = StickyRouter::new();
        r.bind("s", "a");
        r.unbind("s");
        assert_eq!(r.route("s", &["a".to_string()]), None);
    }

    #[test]
    fn sticky_rebind_moves_to_new_account() {
        let r = StickyRouter::new();
        r.bind("s", "a0");
        r.bind("s", "a1"); // 成功重绑到实际成功的号
        assert_eq!(r.route("s", &["a0".to_string(), "a1".to_string()]).as_deref(), Some("a1"));
    }

    #[test]
    fn pick_skips_accounts_at_inflight_cap() {
        let pool = Pool::new();
        let _l1 = pool.acquire("p", "a", "m").unwrap();
        let _l2 = pool.acquire("p", "a", "m").unwrap();
        // a 已满 2 并发：pick 不能再选它（旧实现会选中 → acquire 判 429）
        assert_eq!(pool.pick("p", &["a".to_string()], "m"), None);
        // 换成两个账号时仍能选到 b
        assert_eq!(pool.pick("p", &["a".into(), "b".into()], "m").as_deref(), Some("b"));
    }

    /// 面板账号表的「健康」列要靠 blocked_of 说出「现在还能不能用、还要等多久」。
    ///
    /// 语义约定：waf_s 只反映整号限速，cooling_s 取该号所有模型里最长的冷却；
    /// 两者互不覆盖；成功一次后必须都归零（否则面板会一直显示早已恢复的冷却）。
    #[test]
    fn thaw_clears_blocking_but_keeps_the_failure_record() {
        let p = Pool::new();
        p.report("plat", "acc-1", "m1", false, ErrorClass::Quota, false);
        p.report("plat", "acc-1", "m2", false, ErrorClass::Quota, false);
        p.report("plat", "acc-1", "m1", false, ErrorClass::Auth, false);
        p.report("plat", "acc-2", "m1", false, ErrorClass::Quota, false);
        let b = p.blocked_of("acc-1");
        assert!(b.waf_s > 0.0 && b.cooling_s > 0.0, "种子没生效");

        let (had_waf, cooled) = p.thaw("acc-1");
        assert!(had_waf, "整号限速应被清掉");
        assert_eq!(cooled, 2, "acc-1 的两条 (账号,模型) 冷却都要清");
        assert!(p.blocked_of("acc-1").is_clear(), "解冻后必须立刻放行");

        // 别的账号不受牵连：解冻是账号级操作，不能顺手把全池都放开。
        assert!(!p.blocked_of("acc-2").is_clear(), "acc-2 不该被一起解冻");

        // health 是「最近一次失败结论」，属于历史；解冻不代表验证过它好了。
        assert_eq!(p.health_of("acc-1").map(|h| h.0), Some("auth".to_string()));

        // 幂等：再解冻一次什么都不动，也不 panic。
        assert_eq!(p.thaw("acc-1"), (false, 0));
        assert_eq!(p.thaw("nobody"), (false, 0));
    }
    #[test]
    fn blocked_of_reports_waf_and_longest_cooldown() {
        let pool = Pool::new();
        // 全新账号：无阻断
        let b = pool.blocked_of("a");
        assert!(b.is_clear(), "新账号不该被标阻断：{b:?}");

        // 402 只冻 (账号, 模型) → cooling_s > 0，waf_s 仍为 0
        pool.report("p", "a", "m1", false, ErrorClass::Quota, false);
        let b = pool.blocked_of("a");
        assert!(b.cooling_s > 0.0, "模型级冷却必须体现：{b:?}");
        assert_eq!(b.waf_s, 0.0, "402 不该算整号限速：{b:?}");
        assert!(!b.is_clear(), "{b:?}");

        // 另一个模型也冷却：取最长，不是累加
        pool.report("p", "a", "m2", false, ErrorClass::Auth, false);
        let b2 = pool.blocked_of("a");
        assert!(b2.waf_s > 0.0, "401 必须算整号限速：{b2:?}");
        // 只比大小会因两次调用间的微秒流逝而误判，给 2 秒容差。
        assert!(
            b2.cooling_s > 0.0 && (b2.cooling_s - b.cooling_s).abs() < 2.0,
            "401 不应覆盖/清零已有的模型冷却：{b:?} → {b2:?}"
        );

        // 换一个未受影响的账号：互不串味
        assert!(pool.blocked_of("b").is_clear(), "别的账号不该被牵连");

        // 成功一次 → 该号冷却与拉黑都清掉
        pool.report("p", "a", "m1", true, ErrorClass::Transient, false);
        assert!(pool.blocked_of("a").is_clear(), "成功后必须清干净");
    }

    #[test]
    fn quota_does_not_blacklist_account_globally() {
        let pool = Pool::new();
        pool.report("p", "a", "m1", false, ErrorClass::Quota, false);
        // 402 只冻 (账号, 模型)：同一账号的别的模型照常可用
        assert!(pool.available("a", "m2"), "402 不应账号级拉黑");
        assert!(!pool.available("a", "m1"), "该模型应进冷却");
        // waf 快照里不应出现 a
        let snap = pool.snapshot();
        assert_eq!(snap["waf"].as_array().map(|v| v.len()), Some(0));
    }

    #[test]
    fn auth_blacklists_account_globally() {
        let pool = Pool::new();
        pool.report("p", "a", "m1", false, ErrorClass::Auth, false);
        assert!(!pool.available("a", "m2"), "401 应账号级拉黑，其余模型也不能用");
        assert_eq!(pool.pick("p", &["a".to_string()], "m2"), None, "被拉黑的账号不该被 pick");
    }

    #[test]
    fn recent_pick_yield_is_reset_after_half_open() {
        let pool = Pool::new();
        let one = vec!["solo".to_string()];
        // 连选 2 次后进入「让位」状态，全部候选被挡 → 半开放行并清空 recent
        for _ in 0..RECENT_YIELD {
            assert_eq!(pool.pick("p", &one, "m").as_deref(), Some("solo"));
        }
        assert_eq!(pool.pick("p", &one, "m").as_deref(), Some("solo"), "半开应放行");
        // 清空后下一次仍能正常选中（旧实现会一直走半开分支）
        assert_eq!(pool.pick("p", &one, "m").as_deref(), Some("solo"));
    }

    #[test]
    fn health_reflects_outcome_per_class() {
        let p = Pool::new();
        // 401 → 面板要能直接看出「这个号过期了」
        p.report("plat", "acc", "m", false, ErrorClass::Auth, false);
        let (status, label, _) = p.health_of("acc").expect("auth 后应有健康结论");
        assert_eq!(status, "auth");
        assert!(label.contains("鉴权失败"), "label={label}");
        // 成功 → 立刻恢复「正常」
        p.report("plat", "acc", "m", true, ErrorClass::Transient, false);
        assert_eq!(p.health_of("acc").unwrap().0, "ok");
        // 网络抖动不该把号标成坏号
        p.report("plat", "acc", "m", false, ErrorClass::Transient, false);
        assert_eq!(p.health_of("acc").unwrap().0, "ok", "Transient 不应覆盖健康结论");
        // 402 / 429 有各自的状态
        p.report("plat", "acc", "m", false, ErrorClass::Quota, false);
        assert_eq!(p.health_of("acc").unwrap().0, "quota");
        p.report("plat", "acc", "m", false, ErrorClass::Rate, false);
        assert_eq!(p.health_of("acc").unwrap().0, "rate");
    }

    #[test]
    fn mark_health_and_forget() {
        let p = Pool::new();
        p.mark_health("acc2", true, "");
        let (status, label, age) = p.health_of("acc2").unwrap();
        assert_eq!(status, "ok");
        assert_eq!(label, "正常");
        assert_eq!(age, 0);
        p.mark_health("acc2", false, "Cookie 已过期");
        let (status, label, _) = p.health_of("acc2").unwrap();
        assert_eq!(status, "auth");
        assert_eq!(label, "Cookie 已过期");
        p.forget_account("acc2");
        assert!(p.health_of("acc2").is_none(), "删号后不应残留健康结论");
    }
}
