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
const CREDIT_TTL: Duration = Duration::from_secs(25 * 3600);
/// 积分快照的有效期。
///
/// ★ 快照的刷新时机是**签到成功后**（每日一次），所以有效期要比签到周期长一点。
///   设 25 小时而不是 24：签到只要偶尔晚几分钟，24 小时就会让快照在「今天还没签」
///   的那段空窗里过期，选号忽然失去依据。多留 1 小时缓冲。
///
/// ★ 过期快照必须当作「没有快照」：不这样的话，一个号的积分早已用尽（或早已充值）
///   而快照还是旧值，排序会一直照旧 —— 旧值偏低则反复挑中一个没额度的号去撞 402，
///   旧值偏高则有额度的号永远排在后面。两种情况都没有任何日志，只有「怎么也跑不通」
///   或「某个号一直不用」。

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

/// 退避多久：(冷却秒数, 新 strikes)。抖动/限流两类冷却共用同一条曲线。
///
/// ★ 上游给了 Retry-After 就照上游，不递增 strikes —— 它有准确的恢复时间，
///   下一次理应从轻档重新开始，而不是带着历史惩罚继续翻倍。
/// 拿不到提示才走本地指数退避：30s × 2^strikes（封顶 15min）× 抖动[0.5,1.5)。
/// 抖动是必要的：同一时刻被限流的一批号如果同时解冻，会一起撞回上游。
fn backoff_secs(strikes: u32, hint: Option<f64>) -> (f64, u32) {
    match hint {
        Some(h) => (h, strikes),
        None => {
            let exp = COOL_BASE_SECS * 2f64.powi(strikes.min(5) as i32);
            let jitter = rand::thread_rng().gen_range(0.5..1.5);
            (exp.min(COOL_CAP_SECS) * jitter, strikes + 1)
        }
    }
}
const RECENT_YIELD: usize = 2;

#[derive(Debug, Clone)]
struct CoolState {
    until: Instant,
    strikes: u32,
    /// 硬冷却：到期前**不允许半开探测放行**。
    ///
    /// 只有「上游明确说了额度没了」这类结论才置位（402 额度耗尽 / 地域封锁）。
    /// 半开的假设是「冷却也许已经过期，探一次就知道」；但额度类冷却的到期时间
    /// 是照上游的额度重置点算出来的，探一次只会再拿一个 402，还拖慢真实恢复
    /// （每个请求都先白打一次上游）。软冷却（抖动/限流）保持原行为。
    hard: bool,
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
    /// account -> Token 周期限制规则（面板配置，随账号表载）
    token_rules: HashMap<String, TokenRule>,
    /// account -> (当前窗口起点, 窗口内已用 tokens)。只给配了规则的号建条目。
    token_used: HashMap<String, (i64, u64)>,
    /// account -> 60s 窗口内被选时间
    recent: HashMap<String, Vec<Instant>>,
    /// account -> 最近一次真实结果（面板「健康」列直接读它）。
    /// 只存内存不落盘：进程重启后旧结论未必成立，重新验活比读历史更准。
    health: HashMap<String, HealthState>,
    /// account -> 积分余额快照（值, 记录时刻, **最早到期的资源包时刻** epoch 秒）。
    /// 供「快过期积分优先」选号用：积分少且久未刷新的号先消耗掉，
    /// 避免小余额账号永远排在后面、积分到期作废。
    credits: HashMap<String, CreditSnap>,
}
/// 积分快照：余额、记录时刻，以及**最早到期的资源包时刻**（epoch 秒）。
///
/// ★ 为什么要存到期时刻：WB 的积分不是一个数，是**多个资源包**
///   （套餐基础包 / 每日包 / 赠送包各有不同到期时间）。「快过期加权」要的是
///   「先消耗快到期那个号」，光有余额高低不够 —— 余额 5000 但明天到期的号，
///   应当排在余额 100 但下月才到期的号前面。缺失（None）= 上游没给到期字段，
///   只按余额排序，不惩罚也不加分。
#[derive(Clone, Copy)]
pub struct CreditSnap {
    pub value: f64,
    pub at: Instant,
    pub expire_at: Option<i64>,
}

/// 「快到期」判定窗口：48 小时内到期的号优先消耗。
///
/// ★ 为什么是 48 小时：WB 的每日包按自然日重置、套餐包按 30 天周期。
///   24 小时太紧（签到每天才刷一次快照，很容易全天没有一个号命中，等于没做）；
///   72 小时太松（几乎全部号都命中，又退回纯余额排序）。48h 是「今天+明天」
///   两天的量，签到周期内必然反映出来。
pub const CREDIT_EXPIRING_WINDOW_SECS: i64 = 48 * 3600;

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
    /// ★ 2026-10-26（P3）：单账号在单模型上的最大并发（可热更新，面板 /api/runtime 改）。
    /// 0 = 未注入，用编译期默认 MAX_INFLIGHT。
    max_inflight: std::sync::atomic::AtomicU32,
    /// ★ 2026-10-26（P3）：状态落盘路径（None = 不持久化，测试用）。
    state_path: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// ★ 2026-10-28（对齐 magpie reserve）：选号策略。
    /// 0 = 默认（快过期优先 → 余额升序）；>0 = reserve/ceiling 的百分比阈值。
    ceiling: std::sync::atomic::AtomicU32,
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
            max_inflight: std::sync::atomic::AtomicU32::new(0),
            state_path: std::sync::Mutex::new(None),
            ceiling: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// ★ 2026-10-26（P3）：带持久化的构造。冷却/健康/积分快照落盘到
    /// `{data_dir}/pool-state.json`，重启后恢复。
    ///
    /// **为什么重要**：原本这些状态纯内存，重启即清零——所有已冷却的账号立刻被
    /// 重新尝试（一上来就再吃一轮 429/402），会话粘性全断（同一会话换号，
    /// 上游 prompt cache 白丢）。运维重启一次面板，等于把账号治理状态归零。
    ///
    /// 单调 `Instant` 存不了墙钟，所以落盘换算成「剩余秒数」，加载时再换算回
    /// `Instant`（已过期的直接丢弃）。跨重启、跨机器都可读。
    pub fn open(data_dir: &std::path::Path) -> Arc<Pool> {
        let pool = Pool {
            inner: Mutex::new(Inner::default()),
            max_inflight: std::sync::atomic::AtomicU32::new(0),
            state_path: std::sync::Mutex::new(Some(data_dir.join("pool-state.json"))),
            ceiling: std::sync::atomic::AtomicU32::new(0),
        };
        let arc = Arc::new(pool);
        arc.restore();
        arc
    }

    fn restore(&self) {
        let path = match self.state_path.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(p) => p,
            None => return,
        };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            tracing::warn!("pool：状态文件损坏，忽略（{}）", path.display());
            return;
        };
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut n_cool = 0;
        let mut n_health = 0;
        if let Some(arr) = v.get("cooldown").and_then(|x| x.as_array()) {
            for item in arr {
                let (Some(acc), Some(model), Some(remain), Some(strikes)) = (
                    item.get("account").and_then(|x| x.as_str()),
                    item.get("model").and_then(|x| x.as_str()),
                    item.get("remain_s").and_then(|x| x.as_f64()),
                    item.get("strikes").and_then(|x| x.as_u64()),
                ) else {
                    continue;
                };
                if remain <= 0.0 {
                    continue;
                }
                g.cooldown.insert(
                    (acc.to_string(), model.to_string()),
                    CoolState {
                        until: now + Duration::from_secs_f64(remain),
                        strikes: strikes as u32,
                        // 落盘格式没记 hard：按硬冷却恢复。软冷却跨重启被当硬冷却的代价
                        // 只是「少探一次」，反过来则会让额度冷却在重启后被反复探测。
                        hard: true,
                    },
                );
                n_cool += 1;
            }
        }
        if let Some(arr) = v.get("health").and_then(|x| x.as_array()) {
            for item in arr {
                let (Some(acc), Some(status), Some(age)) = (
                    item.get("account").and_then(|x| x.as_str()),
                    item.get("status").and_then(|x| x.as_str()),
                    item.get("age_s").and_then(|x| x.as_f64()),
                ) else {
                    continue;
                };
                // 超过 6 小时的结论不恢复：那是上一班岗的判断，展示出来会误导
                //（面板写着「鉴权失败」，其实早已重登过）。
                if age > 6.0 * 3600.0 {
                    continue;
                }
                g.health.insert(
                    acc.to_string(),
                    HealthState {
                        status: match status {
                            "ok" => "ok",
                            "quota" => "quota",
                            "rate" => "rate",
                            _ => "auth",
                        },
                        label: item
                            .get("label")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        at: now - Duration::from_secs_f64(age),
                    },
                );
                n_health += 1;
            }
        }
        if let Some(arr) = v.get("credits").and_then(|x| x.as_array()) {
            for item in arr {
                if let (Some(acc), Some(c)) = (
                    item.get("account").and_then(|x| x.as_str()),
                    item.get("credit").and_then(|x| x.as_f64()),
                ) {
                    let expire_at = item.get("expire_at").and_then(|x| x.as_i64());
                    g.credits.insert(
                        acc.to_string(),
                        CreditSnap {
                            value: c,
                            at: now,
                            expire_at,
                        },
                    );
                }
            }
        }
        if n_cool > 0 || n_health > 0 {
            tracing::info!(cooldown = n_cool, health = n_health, "pool：已恢复重启前状态");
        }
    }

    /// 把冷却 / 健康 / 积分快照落盘（原子写：.tmp + rename）。
    pub fn persist(&self) {
        let path = match self.state_path.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(p) => p,
            None => return,
        };
        let now = Instant::now();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cooldown: Vec<serde_json::Value> = g
            .cooldown
            .iter()
            .filter_map(|((acc, model), cs)| {
                let remain = cs.until.saturating_duration_since(now).as_secs_f64();
                if remain <= 0.0 {
                    return None;
                }
                Some(serde_json::json!({
                    "account": acc, "model": model,
                    "remain_s": remain.round(), "strikes": cs.strikes
                }))
            })
            .collect();
        let health: Vec<serde_json::Value> = g
            .health
            .iter()
            .map(|(acc, h)| {
                serde_json::json!({
                    "account": acc, "status": h.status, "label": h.label,
                    "age_s": now.duration_since(h.at).as_secs_f64().round()
                })
            })
            .collect();
        let credits: Vec<serde_json::Value> = g
            .credits
            .iter()
            .map(|(acc, s)| {
                serde_json::json!({"account": acc, "credit": s.value, "expire_at": s.expire_at})
            })
            .collect();
        let v = serde_json::json!({
            "saved_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "cooldown": cooldown, "health": health, "credits": credits,
        });
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string(&v).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// 设置单账号单模型最大并发（0 = 回到编译期默认）。面板 /api/runtime 调用。
    /// 当前在途上限（只读，供日志/面板说明「几路已满」）。
    pub fn max_inflight_public(&self) -> u32 {
        self.max_inflight()
    }

    pub fn set_max_inflight(&self, n: u32) {
        self.max_inflight
            .store(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// ★ 2026-10-28（对齐 magpie reserve）：设置选号策略。
    ///
    /// `ceiling` = 0 关闭（走默认：快到期优先 → 余额升序）；
    /// `ceiling` = 1..100 启用 reserve/ceiling 语义 ——
    /// **按「额度已用百分比」分层**：未达阈值的（tier 0）优先，
    /// 达阈值的（tier 1）让位，冷却中的（tier 2）最后。
    /// 所有号都超阈值时，按使用率**从低到高**（用得少的先用）。
    ///
    /// 这与默认策略互补：默认看「积分余额绝对值」，reserve 看「相对用掉多少」。
    /// 典型用法：给主号留 20%（ceiling=80），网关只用掉八成。
    pub fn set_ceiling(&self, ceiling: u32) {
        self.ceiling
            .store(ceiling.min(100), std::sync::atomic::Ordering::Relaxed);
    }

    fn ceiling(&self) -> u32 {
        self.ceiling.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 某账号当前周期的额度使用百分比（0-100）。无规则/无用量 → None（视为「未知」，算未超阈值）。
    fn usage_pct(g: &Inner, account: &str, now_epoch: i64) -> Option<f64> {
        let rule = g.token_rules.get(account)?;
        let win = now_epoch.div_euclid(rule.period_s) * rule.period_s;
        let (w, used) = *g.token_used.get(account)?;
        if w != win {
            return Some(0.0); // 本期还没用过
        }
        if rule.limit == 0 {
            return None;
        }
        Some((used as f64 / rule.limit as f64) * 100.0)
    }

    /// 当前生效的最大并发。
    fn max_inflight(&self) -> u32 {
        let v = self.max_inflight.load(std::sync::atomic::Ordering::Relaxed);
        if v == 0 { MAX_INFLIGHT } else { v }
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
        if *n >= self.max_inflight() {
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
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // 锁中毒不能静默返回 None：那会让 pick 表现得像「全部被挡」，进而把整个平台的
        // 请求变成 503（或旧版那样越过隔离硬走）。统一 into_inner 继续用里面的数据。
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // 三种「挡住」的原因必须分开记：冷却中的可以半开探测，纯让位的可以放行，
        // 而在飞已满的账号**不能**被半开选中（否则 pick 说可用、acquire 立刻判 429）。
        // (account, 冷却剩余秒, 是否硬冷却)：硬冷却不参与半开探测
        let mut cooling: Vec<(String, f64, bool)> = Vec::new();
        let mut yielded: Vec<String> = Vec::new(); // 仅 RecentPick 让位
        let mut busy = 0usize; // 在飞已满
        // ★ 2026-10-26（P1，对齐 workbuddy2api「快过期积分加权」）：
        // 先把**所有通过硬门槛**（非 WAF、非冷却、在飞未满）的候选收集起来，
        // 再按「积分余额低者优先」排序后取第一个。原实现是遍历顺序先到先得——
        // 账号池按添加顺序排列，于是永远只用第一个号，其余号的积分到期作废。
        // 排序口径：有余额快照的按余额升序（少的下次先用掉），没快照的排最后
        // （不知道余额就不该抢在已知小余额的号前面）。
        // (快到期键, credit, 原序, account) —— 键序见下方 sort
        let mut candidates: Vec<(bool, f64, usize, String)> = Vec::new();
        for (idx, a) in accounts.iter().enumerate() {
            // ★ 限制器：本周期用量触顶的号直接不参与候选（不是冷却——冷却会
            //   自动衰减回来，触顶要到**窗口翻页**才回来，判据不同层）。
            if token_block_at(&g, a, now_epoch).is_some() {
                continue;
            }
            if g.waf.get(a).map(|t| *t > now).unwrap_or(false) {
                continue;
            }
            let inflight_key = (platform.to_string(), a.clone(), model.to_string());
            if g.inflight
                .get(&inflight_key)
                .map(|n| *n >= self.max_inflight())
                .unwrap_or(false)
            {
                busy += 1;
                continue;
            }
            if let Some(cs) = g.cooldown.get(&(a.to_string(), model.to_string())) {
                if cs.until > now {
                    cooling.push((
                        a.clone(),
                        cs.until.duration_since(now).as_secs_f64(),
                        cs.hard,
                    ));
                    continue;
                }
            }
            // 过期快照视同「没有快照」（排最后），理由见 CREDIT_TTL。
            // ★ 余额 < 1 的快照**同样**降级为无快照：低余额优先的本意是
            //   「小而易废的额度先花掉」，不是「把流量砸向已知没余额的号」。
            //   x0.79 一组的倍率下 1 分连一次请求都不够付，把它排最前只会
            //   让每个冷却窗都白撞一次 402 才轮到可用号。
            //   ★ 只降级**路由**——面板仍如实显示 0（那是真实余额，隐藏它
            //   才是说谎）。两个口径不同是有意的。
            let (credit, exp_soon) = match g.credits.get(a) {
                Some(s) if now.duration_since(s.at) <= CREDIT_TTL && s.value >= 1.0 => {
                    let soon = s
                        .expire_at
                        .map(|e| e <= now_epoch + CREDIT_EXPIRING_WINDOW_SECS)
                        .unwrap_or(false);
                    (s.value, soon)
                }
                _ => (f64::MAX, false),
            };
            // 主键取反（!exp_soon）：sort 升序时 false 在前 → 「快到期」的号排最前
            candidates.push((!exp_soon, credit, idx, a.clone()));
        }
        // 排序：快到期优先 → 余额升序 → 原序（稳定，不打乱既有行为）。
        // 「快到期优先」正是目标里那条加权：余额再多，明天到期就该先花掉，
        // 否则整包作废——比「余额低」更硬的损失。
        //
        // ★ 2026-10-28（对齐 magpie reserve）：ceiling > 0 时改用
        //   reserve/ceiling 语义 —— 按「额度已用百分比」分层，而不是积分绝对值：
        //   tier 0 = 未达阈值（优先）、tier 1 = 已达阈值（让位）、tier 2 = 冷却中（最后）。
        //   全部超阈值时按使用率从低到高（用得少的先用）。
        let ceil_pct = self.ceiling();
        if ceil_pct > 0 {
            let now_epoch = epoch_secs();
            // (tier, usage_pct, 原序, account)
            let mut ranked: Vec<(u8, f64, usize, String)> = candidates
                .iter()
                .map(|(_, _, idx, a)| {
                    let pct = Self::usage_pct(&g, a, now_epoch);
                    let tier = match pct {
                        // 未知额度（无 token 规则 / API Key 类）→ 视为未达阈值
                        None => 0u8,
                        Some(p) if p >= ceil_pct as f64 => 1,
                        _ => 0,
                    };
                    (tier, pct.unwrap_or(0.0), *idx, a.clone())
                })
                .collect();
            ranked.sort_by(|x, y| {
                x.0.cmp(&y.0)
                    .then(x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
                    .then(x.2.cmp(&y.2))
            });
            for (_, _, _, a) in &ranked {
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
            // 全部被挡 → 走与默认策略相同的半开兜底（下面继续执行）
        } else {
            candidates.sort_by(|x, y| {
                x.0.cmp(&y.0)
                    .then(x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
                    .then(x.2.cmp(&y.2))
            });
        }
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
        // 全被挡 → 半开：优先真正在冷却的（剩余最短者，清冷却探测）。
        //
        // ★ 硬冷却（402 额度耗尽 / 地域封锁）不参与：它的到期时间是照上游的额度
        //   重置点算出来的，探一次只是再拿一个同样的错误。放行它等于在重置之前
        //   每个请求都先白打一次上游，既拖慢面板响应，也让「额度不足」的报错
        //   和真实请求交替出现、看起来像时好时坏。
        //   全池只剩硬冷却时 pick 返回 None，上层按「账号当前全部不可用」回 503，
        //   这才是这段时间里诚实的答案。
        cooling.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((account, remain, _)) = cooling.iter().find(|c| !c.2) {
            let account = account.clone();
            g.cooldown.remove(&(account.clone(), model.to_string()));
            tracing::warn!(
                "pool 半开：全部候选被挡，清冷却探测 {}（剩余 {:.0}s）",
                account,
                remain
            );
            return Some(account);
        }
        if cooling.iter().any(|c| c.2) {
            tracing::warn!(
                "pool：{} 的候选全部处于硬冷却（额度/地域），本轮不放行探测",
                platform
            );
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

    /// 该 (账号,模型) 当前是否可直接使用：未 WAF 拉黑、未冷却、**未触顶限额**。
    /// 会话粘性路由命中后必须再过这一关，否则粘性会把请求钉死在刚被拉黑
    /// 或刚触顶的账号上——「限额防用量」的安全属性会被粘性整体绕过：
    /// 绑定过的号每请求都烧，限了等于没限。判据与 pick 共用 token_block_at。
    pub fn available(&self, account: &str, model: &str) -> bool {
        let now = Instant::now();
        let now_epoch = epoch_secs();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.waf.get(account).map(|t| *t > now).unwrap_or(false) {
            return false;
        }
        if token_block_at(&g, account, now_epoch).is_some() {
            return false;
        }
        match g.cooldown.get(&(account.to_string(), model.to_string())) {
            Some(cs) => cs.until <= now,
            None => true,
        }
    }

    /// 上报结果：成功清冷却/拉黑；失败按归因冷却或拉黑。
    /// 判定只依赖 `class`。早期版本还有一个 waf_fingerprint 形参（HTTP 200 + 业务码
    /// 失败 = 上游假成功），但拉黑改由 class 统一决定后它只剩一条消警语句 —— 留着会
    /// 让调用方以为「传 true 才拉黑」，于是流式路径长期传 false、假成功不给号池任何
    /// 信号。已删除。
    pub fn report(
        &self,
        platform: &str,
        account: &str,
        model: &str,
        ok: bool,
        class: ErrorClass,
    ) {
        self.report_with_hint(platform, account, model, ok, class, None)
    }

    /// 带「上游给的 Retry-After」的 report。
    ///
    /// 限流（429）的上游几乎都会明确说「N 秒后再来」。原本一律按本地曲线猜
    /// （30s×2^n × 抖动[0.5,1.5)），猜短了继续撞墙、还把 strikes 越加越高，最后冻满
    /// 15 分钟；猜长了则白白错过窗口。_refs/ 里 workbuddy2api / trae2api 都是
    /// **优先采用上游秒数，拿不到才退避**，这里对齐同一口径。
    ///
    /// 安全边界：提示值再过一次 clamp（[1, 900]）。上限取 COOL_CAP_SECS——上游偶尔
    /// 回 "retry after 86400"，照做等于把这个号冻一整天；下限 1s 防止 0/负数导致
    /// 「立刻重试」的死循环。提示值不递增 strikes：既然上游给了准确恢复时间，
    /// 下一次就应该从轻档重新开始，而不是带着历史惩罚。
    #[allow(clippy::too_many_arguments)]
    pub fn report_with_hint(
        &self,
        platform: &str,
        account: &str,
        model: &str,
        ok: bool,
        class: ErrorClass,
        retry_after: Option<f64>,
    ) {
        let _ = platform;
        let hint = retry_after.filter(|v| v.is_finite()).map(|v| v.clamp(1.0, COOL_CAP_SECS));
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
                            // 额度重置点是算出来的，探一次也不会提前 —— 不许半开放行
                            hard: true,
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
                            // 地域/车道封锁与账号无关：探测换不回正常响应
                            hard: true,
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
                    // 上游说了就照上游：不递增 strikes（它有准确恢复时间，下次该从轻档重新开始）
                    let (secs, next_strikes) = backoff_secs(strikes, hint);
                    g.cooldown.insert(
                        key,
                        CoolState {
                            until: now + Duration::from_secs_f64(secs),
                            strikes: next_strikes,
                            hard: false,
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
                    let (secs, next_strikes) = backoff_secs(strikes, hint);
                    g.cooldown.insert(
                        key,
                        CoolState {
                            until: now + Duration::from_secs_f64(secs),
                            strikes: next_strikes,
                            hard: false,
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
        self.record_credit_exp(account, credit, None);
    }

    /// 同 `record_credit`，但带上「最早到期的资源包时刻」（epoch 秒）。
    /// `expire_at=None` 表示上游没给到期字段 —— 只按余额加权，不假装知道到期。
    pub fn record_credit_exp(
        &self,
        account: &str,
        credit: Option<f64>,
        expire_at: Option<i64>,
    ) {
        let Some(c) = credit else { return };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.credits.insert(
            account.to_string(),
            CreditSnap {
                value: c,
                at: Instant::now(),
                expire_at,
            },
        );
    }

    /// 清除某账号的积分快照。
    ///
    /// ★ 为什么需要：余额查询**明确判出凭据失效**（HTTP 401/403）时，
    ///   旧快照留着有害——死号的余额永不清零，「快到期优先」会把它排到最前，
    ///   每次路由都先撞死号。网络抖动不清（旧值可能仍然成立）。
    pub fn clear_credit(&self, account: &str) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.credits.remove(account);
    }
    /// 某账号的积分余额快照（含**快照年龄**，秒）。`None` = 从未记录或已过期。
    ///
    /// ★ 与 `pick` 的选号判据**同一口径**：过期快照视同没有。否则面板会显示
    ///   「余额 100」，而选号逻辑早就把它当没数据了 —— 同一个数在两处有两种解释，
    ///   用户看到的排名依据与实际排名对不上。
    /// 返回（余额, 快照年龄秒, 最早到期 epoch 秒）。过期视同没有。
    pub fn credit_snapshot(&self, account: &str) -> Option<(f64, u64, Option<i64>)> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let s = g.credits.get(account)?;
        let age = s.at.elapsed();
        if age > CREDIT_TTL {
            return None;
        }
        Some((s.value, age.as_secs(), s.expire_at))
    }

    /// 全池积分聚合（概览页一行读懂资产用）。
    ///
    /// ★ 口径与 pick **完全一致**：过 TTL 的快照不算、<1 的降级不算、
    ///   「快到期」= 48h 窗口。否则概览页说的和选号逻辑做的又是两套账——
    ///   界面显示 9000 分而实际只有 3 个号能用，比不显示更糟。
    /// 返回 (有效快照数, 余额总和, 快到期数, 最早到期 epoch 秒)。
    pub fn credit_summary(&self) -> (usize, f64, usize, Option<i64>) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut n = 0usize;
        let mut total = 0.0f64;
        let mut soon = 0usize;
        let mut earliest: Option<i64> = None;
        for s in g.credits.values() {
            if now.duration_since(s.at) > CREDIT_TTL || s.value < 1.0 {
                continue;
            }
            n += 1;
            total += s.value;
            if let Some(e) = s.expire_at {
                if e <= now_epoch + CREDIT_EXPIRING_WINDOW_SECS {
                    soon += 1;
                }
                earliest = Some(earliest.map_or(e, |c| c.min(e)));
            }
        }
        (n, total, soon, earliest)
    }
    /// 某账号的积分余额（面板展示用）。`None` = 从未记录过或已过期。
    pub fn credit_of(&self, account: &str) -> Option<f64> {
        self.credit_snapshot(account).map(|(c, _, _)| c)
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
            "max_inflight": self.max_inflight(),
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


// ── Token 周期限制器（对齐 Agent2API core/limiter.rs 的 v2 形态）──────────
//
// ★ 解决什么问题：号池的冷却/拉黑都是**事后反应**（撞了 429 才冷却）。
//   而免费额度账号被上游盯上往往是因为**用量太猛**——等 429 出现时已经在
//   风险名单上了。限制器是事前主动控制：「这个账号每周期最多用 N 个 Token」，
//   到量就在选号时跳过，窗口翻页自动恢复，无需人工。
//
// ── 与参考实现同款的三个要点 ────────────────────────────────
//   1. 规则按**账号**配置（不是全局一刀切）；
//   2. 窗口按自然时间对齐（floor(now/period)*period）——「每小时 50 万」
//      指整点翻页，不是滑动窗口，读数可预期；
//   3. **拿得出证据才拦**：窗口里没有累计记录 = 0 消耗 = 不拦。重启后
//      事实表是空的，宁可用满也不误拦（误拦的账号会永远饿死在池里）。

/// 账号的 Token 限制规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenRule {
    /// 每周期允许的 token 上限（in+out 合计）
    pub limit: u64,
    /// 周期秒数（30min ~ 24h，set 处夹住）
    pub period_s: i64,
}

/// 允许范围：30 分钟 ~ 24 小时（参考实现评审结论：窗口太短会「刚恢复又
/// 被拦」反复出现，太长失去保护意义）。
pub const MIN_TOKEN_PERIOD_SECS: i64 = 1_800;
pub const MAX_TOKEN_PERIOD_SECS: i64 = 86_400;

/// 限制器现场：触顶时返回 (已用, 上限, 距窗口翻页的秒数)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenLimited {
    pub used: u64,
    pub limit: u64,
    pub reset_in_s: i64,
}

fn epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 持锁版触顶判定（pick 循环内用；公开方法 `token_block` 自己拿锁）。
fn token_block_at(g: &Inner, account: &str, now_epoch: i64) -> Option<TokenLimited> {
    let rule = g.token_rules.get(account)?;
    let win = now_epoch.div_euclid(rule.period_s) * rule.period_s;
    let (w, used) = *g.token_used.get(account)?;
    if w != win {
        return None;
    }
    if used >= rule.limit {
        Some(TokenLimited {
            used,
            limit: rule.limit,
            reset_in_s: (win + rule.period_s - now_epoch).max(0),
        })
    } else {
        None
    }
}
impl Pool {
    /// 设置/清除账号的 Token 限制（limit=None 清除）。周期越界夹进
    /// [30min, 24h]，不报错——面板传错值也要有确定行为。
    pub fn set_token_rule(&self, account: &str, limit: Option<u64>, period_s: i64) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match limit {
            Some(l) if l > 0 => {
                let p = period_s.clamp(MIN_TOKEN_PERIOD_SECS, MAX_TOKEN_PERIOD_SECS);
                g.token_rules
                    .insert(account.to_string(), TokenRule { limit: l, period_s: p });
            }
            _ => {
                g.token_rules.remove(account);
            }
        }
    }

    /// 测试辅助：直接写本窗口用量（模拟已用 N token）。
    #[cfg(test)]
    pub fn mark_token_usage_for_test(&self, account: &str, used: u64) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let win = epoch_secs().div_euclid(3600) * 3600;
        g.token_used.insert(account.to_string(), (win, used));
    }

    /// 请求收尾记账（tin+tout 合计）。**只对配了规则的账号累计**——
    /// 没配规则的号不占事实表内存。记账即建窗：第一次用量就落进当前窗口。
    pub fn add_tokens_used(&self, account: &str, n: u64) {        if n == 0 {
            return;
        }
        let now_epoch = epoch_secs();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rule) = g.token_rules.get(account).copied() else { return };
        let win = now_epoch.div_euclid(rule.period_s) * rule.period_s;
        let e = g.token_used.entry(account.to_string()).or_insert((win, 0));
        // 窗口翻页：重置起点重新累计（旧窗口读数直接作废，不结转）
        if e.0 != win {
            *e = (win, 0);
        }
        e.1 = e.1.saturating_add(n);
    }

    /// 该账号此刻是否触顶。None = 没配规则 / 未触顶 / 读数缺失（证据不足不拦）。
    pub fn token_block(&self, account: &str) -> Option<TokenLimited> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        token_block_at(&g, account, epoch_secs())
    }

    /// 全池触顶概况：(触顶账号数, 最早的窗口翻页秒数)。给 503 文案用——
    /// 用户得知道「是限制器在挡，多久自动恢复」，而不是被误导去重新登录。
    pub fn token_hit_info(&self, accounts: &[String]) -> Option<(usize, i64)> {
        let now_epoch = epoch_secs();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0usize;
        let mut soonest: Option<i64> = None;
        for a in accounts {
            if let Some(b) = token_block_at(&g, a, now_epoch) {
                n += 1;
                soonest = Some(soonest.map_or(b.reset_in_s, |s| s.min(b.reset_in_s)));
            }
        }
        (n > 0).then_some((n, soonest.unwrap_or(0)))
    }
    /// 面板行内展示：账号的限制配置与当前窗口用量。
    pub fn token_view(&self, account: &str) -> Option<(TokenRule, u64)> {
        let now_epoch = epoch_secs();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let rule = *g.token_rules.get(account)?;
        let win = now_epoch.div_euclid(rule.period_s) * rule.period_s;
        let used = match g.token_used.get(account) {
            Some((w, u)) if *w == win => *u,
            _ => 0,
        };
        Some((rule, used))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 过期的积分快照必须当作「没有快照」，不参与排序。
    ///
    /// ★ 快照只在**签到成功后**刷新（每日一次）。不过期的话，一个号的积分早已
    ///   用尽而快照还是旧值，排序会一直照旧 —— 旧值偏低则反复挑中没额度的号去
    ///   撞 402，旧值偏高则有额度的号永远排在后面。两种情况都没有任何日志。
    #[test]
    fn stale_credit_snapshot_is_ignored() {
        let pool = Pool::new();
        // 两个号：a 的快照很旧且余额低，b 的快照新鲜且余额高。
        // 若旧快照生效，a 会被优先选中；正确行为是 a 视同无快照（排最后）。
        {
            let mut g = pool.inner.lock().unwrap();
            let stale = Instant::now() - CREDIT_TTL - Duration::from_secs(60);
            g.credits.insert("a".to_string(), CreditSnap { value: 1.0, at: stale, expire_at: None });
            g.credits.insert("b".to_string(), CreditSnap { value: 999.0, at: Instant::now(), expire_at: None });
        }
        let accs = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            pool.pick("p", &accs, "m").as_deref(),
            Some("b"),
            "过期快照不该让 a 排在前面"
        );
    }

    /// 聚合口径必须与 pick 一致：TTL 过期、<1、48h 窗口三者都要验。
    #[test]
    fn credit_summary_matches_pick_criteria() {
        let pool = Pool::new();
        let now_e = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        {
            let mut g = pool.inner.lock().unwrap();
            g.credits.insert("a".to_string(), CreditSnap { value: 500.0, at: Instant::now(), expire_at: Some(now_e + 24*3600) });
            g.credits.insert("b".to_string(), CreditSnap { value: 300.0, at: Instant::now(), expire_at: Some(now_e + 30*86400) });
            // 以下三个都不该计入
            g.credits.insert("dead".to_string(), CreditSnap { value: 0.0, at: Instant::now(), expire_at: None });
            g.credits.insert("stale".to_string(), CreditSnap { value: 999.0, at: Instant::now() - CREDIT_TTL - Duration::from_secs(60), expire_at: None });
        }
        let (n, total, soon, earliest) = pool.credit_summary();
        assert_eq!(n, 2);
        assert_eq!(total, 800.0);
        assert_eq!(soon, 1);
        assert_eq!(earliest, Some(now_e + 24*3600));
    }
    /// 余额 0 的号不许占「低余额优先」的第一名：它会把每个冷却窗都变成
    /// 一次白撞的 402。可用号（哪怕余额更高）必须排在它前面。
    #[test]
    fn zero_balance_account_yields_to_usable_ones() {
        let pool = Pool::new();
        {
            let mut g = pool.inner.lock().unwrap();
            g.credits.insert("dead".to_string(), CreditSnap { value: 0.0, at: Instant::now(), expire_at: None });
            g.credits.insert("alive".to_string(), CreditSnap { value: 400.0, at: Instant::now(), expire_at: None });
        }
        let accs = vec!["dead".to_string(), "alive".to_string()];
        assert_eq!(pool.pick("p", &accs, "m").as_deref(), Some("alive"));
        // 全是死余额时照常给出一个（总得有人试，冷却语义会接手）——不许返回 None
        {
            let mut g = pool.inner.lock().unwrap();
            g.credits.insert("alive".to_string(), CreditSnap { value: 0.5, at: Instant::now(), expire_at: None });
        }
        assert!(pool.pick("p", &accs, "m").is_some());
    }
    /// 限制器三态：未触顶正常用 → 触顶被跳过（别的号接手）→ 证据不足不拦。
    #[test]
    fn token_limiter_blocks_only_on_evidence() {
        let pool = Pool::new();
        pool.set_token_rule("a", Some(1000), 3600);
        let accs = vec!["a".to_string(), "b".to_string()];
        // 没记过账 = 没证据 = 不拦（重启后事实表为空的场景）
        assert!(pool.token_block("a").is_none());
        assert_eq!(pool.pick("p", &accs, "m").as_deref(), Some("a"));
        // 用量堆到触顶：a 被跳过，b 接手
        pool.add_tokens_used("a", 1000);
        assert!(pool.token_block("a").is_some());
        assert_eq!(pool.pick("p", &accs, "m2").as_deref(), Some("b"));
        // 没配规则的账号记账是 no-op（不占事实表）
        pool.add_tokens_used("b", 999999);
        assert!(pool.token_block("b").is_none());
        // 清除规则后 a 恢复可用
        pool.set_token_rule("a", None, 3600);
        assert!(pool.token_block("a").is_none());
    }

    /// available() 是粘性命中的最后一道闸——触顶号必须被拒（否则限额被绕过）。
    #[test]
    fn available_rejects_token_capped_account() {
        let pool = Pool::new();
        pool.set_token_rule("a", Some(100), 3600);
        assert!(pool.available("a", "m"), "零用量无证据，不拦");
        pool.add_tokens_used("a", 120);
        assert!(!pool.available("a", "m"), "触顶必须被粘性关卡拒绝");
        assert!(pool.available("b", "m"), "没规则的号不受影响");
    }
    /// 503 文案数据源：token_hit_info 数出触顶账号数、给出最早翻页秒数。
    #[test]
    fn token_hit_info_counts_blocked_accounts() {
        let pool = Pool::new();
        pool.set_token_rule("a", Some(100), 3600);
        pool.set_token_rule("b", Some(100), 3600);
        let accs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        // 有规则但零量（证据不足不拦）；没规则的 c 也不拦
        assert_eq!(pool.token_hit_info(&accs), None);
        pool.add_tokens_used("a", 150);
        pool.add_tokens_used("b", 200);
        let (n, soonest) = pool.token_hit_info(&accs).unwrap();
        assert_eq!(n, 2);
        assert!(soonest > 0 && soonest <= 3600);
    }
    /// 周期越界夹进 [30min, 24h]：错配置要有确定行为而不是报错。
    #[test]
    fn token_period_clamped() {
        let pool = Pool::new();
        pool.set_token_rule("a", Some(500), 5);
        let (rule, _) = pool.token_view("a").unwrap();
        assert_eq!(rule.period_s, 1800);
        pool.set_token_rule("b", Some(500), 999999);
        let (rule2, _) = pool.token_view("b").unwrap();
        assert_eq!(rule2.period_s, 86400);
        assert_eq!(rule2.limit, 500);
        // token_view 的用量读数对没记过的号是 0（不是 None 缺失）
        assert_eq!(pool.token_view("b").unwrap().1, 0);
    }
    /// 401 判出的死号必须能清掉旧快照：死号的余额永不清零，
    /// 「快到期优先」会把每次路由都先撞向死号。
    #[test]
    fn clear_credit_removes_the_snapshot() {
        let pool = Pool::new();
        pool.record_credit_exp("a", Some(900.0), None);
        assert!(pool.credit_snapshot("a").is_some());
        pool.clear_credit("a");
        assert!(pool.credit_snapshot("a").is_none());
        // 清不存在的号不炸
        pool.clear_credit("ghost");
    }
    /// ★ 快到期必须赢过低余额：余额 5000 但 24h 后到期的号先消耗，
    /// 否则整包作废——这是比「余额低」更硬的损失（目标里的「快过期加权」）。
    #[test]
    fn expiring_soon_beats_low_balance() {
        let pool = Pool::new();
        let soon = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            + 24 * 3600;
        let far = soon + 29 * 86400;
        {
            let mut g = pool.inner.lock().unwrap();
            g.credits.insert("a".to_string(), CreditSnap { value: 5000.0, at: Instant::now(), expire_at: Some(soon) });
            g.credits.insert("b".to_string(), CreditSnap { value: 10.0, at: Instant::now(), expire_at: Some(far) });
        }
        let accs = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            pool.pick("p", &accs, "m").as_deref(),
            Some("a"),
            "24h 内到期的 a 必须排在低余额的 b 前面"
        );
    }
    /// 到期时刻必须随快照一起落盘：只记进程内，重启后「快到期优先」就失明。
    #[test]
    fn credit_expire_survives_persist_roundtrip() {
        let dir = std::env::temp_dir().join(format!("aigw-pool-expire-{:?}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let soon = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            + 24 * 3600;
        {
            let pool = Pool::open(&dir);
            pool.record_credit_exp("a", Some(500.0), Some(soon));
            pool.persist();
        }
        let pool2 = Pool::open(&dir);
        let snap = pool2.credit_snapshot("a");
        assert!(snap.is_some(), "重启后快照必须恢复");
        let (c, _age, exp) = snap.unwrap();
        assert_eq!(c, 500.0);
        assert_eq!(exp, Some(soon), "到期时刻必须随快照落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// 未过期的快照仍要按余额升序（少的下次先用掉）。
    #[test]
    fn fresh_credit_snapshot_still_orders_by_balance() {
        let pool = Pool::new();
        pool.record_credit("a", Some(500.0));
        pool.record_credit("b", Some(10.0));
        let accs = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            pool.pick("p", &accs, "m").as_deref(),
            Some("b"),
            "余额少的应先被消耗"
        );
    }

    #[test]
    fn quota_cooldown_is_not_reopened_by_half_open() {
        // 半开的本意是「冷却可能已经过期了，探测一次」。但 402 额度耗尽的冷却
        // 是按「次日 04:00」算出来的硬窗口：上游明确说了额度没了，探测不可能成功。
        // 旧实现里 pick 的半开分支会清掉冷却放行，于是每个请求都白打一次上游
        // （面板上表现为「额度不足」和真实请求交替出现，额度重置前一直空转）。
        let pool = Pool::new();
        let one = vec!["solo".to_string()];
        pool.report("p", "solo", "m", false, ErrorClass::Quota);
        assert!(pool.blocked_of("solo").cooling_s > 0.0, "402 应进入冷却");
        assert_eq!(pool.pick("p", &one, "m"), None, "额度冷却期间不该被半开放行");
    }

    #[test]
    fn transient_cooldown_is_still_reopened_by_half_open() {
        // 对照：普通抖动类冷却必须继续享受半开探测（否则全池冻死时永远没人探路）。
        let pool = Pool::new();
        let one = vec!["solo".to_string()];
        pool.report("p", "solo", "m", false, ErrorClass::Transient);
        assert_eq!(pool.pick("p", &one, "m").as_deref(), Some("solo"), "抖动类冷却仍应半开");
    }

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
                pool.report("p", "a", "m", false, ErrorClass::Transient);
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
        // ★ 上界必须是**闭**区间：jitter 取值域 [0.5,1.5) 的上界是开的，但
        //   `30 × 1.4999…` 经 round() 后**正好等于 45.0**（同理 90.0 / 1350.0）。
        //   旧写法 `(14.0..45.0)` 把 45 排除在外 —— 约 1.7% 的运行会偶发失败
        //   （实测：同一份代码连跑三次红一次，报「首次 30×[0.5,1.5)，得 45」）。
        //   「没触顶」的语义由 `..=45.0` 照样咬得住（46 及以上才算越界）。
        assert!((14.0..=45.0).contains(&first), "首次 30×[0.5,1.5)，得 {first}");
        let second = left_after(2);
        assert!((28.0..=90.0).contains(&second), "二次 60×[0.5,1.5)，得 {second}");
        let capped = left_after(9);
        // 下界取 0.5×900 的 0.99 倍再留 1 秒余量：抖动下限是标准值，但 `left_s` 是取快照
        // 时重新算 remaining 的，9 次 report 之间流逝的毫秒会把它压到 450 以下一点点。
        // 上界 1350（1.5×900）同理改闭区间，见上面的 round() 说明。
        assert!(
            (444.0..=1350.0).contains(&capped),
            "9 次后应触顶 900×[0.5,1.5)，得 {capped}"
        );
    }

    #[test]
    fn cooldown_expires_allows_pick_again() {
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Transient);
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
        pool.report("p", "a", "m", false, ErrorClass::Auth);
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
        pool.report("p", "a", "m", false, ErrorClass::LaneLocked);
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
        pool.report("p", "a", "m", false, ErrorClass::Region);
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
        pool.report("p", "a", "m", false, ErrorClass::Rate);
        pool.report("p", "a", "m", true, ErrorClass::Transient);
        let snap = pool.snapshot();
        assert_eq!(snap["cooling"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn forget_clears_everything() {
        let pool = Pool::new();
        pool.report("p", "a", "m", false, ErrorClass::Auth); // WAF
        pool.report("p", "a", "m2", false, ErrorClass::Transient); // 冷却
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
        pool.report("p", "a", "m", false, ErrorClass::Transient);
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
        p.report("plat", "acc-1", "m1", false, ErrorClass::Quota);
        p.report("plat", "acc-1", "m2", false, ErrorClass::Quota);
        p.report("plat", "acc-1", "m1", false, ErrorClass::Auth);
        p.report("plat", "acc-2", "m1", false, ErrorClass::Quota);
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
        pool.report("p", "a", "m1", false, ErrorClass::Quota);
        let b = pool.blocked_of("a");
        assert!(b.cooling_s > 0.0, "模型级冷却必须体现：{b:?}");
        assert_eq!(b.waf_s, 0.0, "402 不该算整号限速：{b:?}");
        assert!(!b.is_clear(), "{b:?}");

        // 另一个模型也冷却：取最长，不是累加
        pool.report("p", "a", "m2", false, ErrorClass::Auth);
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
        pool.report("p", "a", "m1", true, ErrorClass::Transient);
        assert!(pool.blocked_of("a").is_clear(), "成功后必须清干净");
    }

    #[test]
    fn quota_does_not_blacklist_account_globally() {
        let pool = Pool::new();
        pool.report("p", "a", "m1", false, ErrorClass::Quota);
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
        pool.report("p", "a", "m1", false, ErrorClass::Auth);
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
        p.report("plat", "acc", "m", false, ErrorClass::Auth);
        let (status, label, _) = p.health_of("acc").expect("auth 后应有健康结论");
        assert_eq!(status, "auth");
        assert!(label.contains("鉴权失败"), "label={label}");
        // 成功 → 立刻恢复「正常」
        p.report("plat", "acc", "m", true, ErrorClass::Transient);
        assert_eq!(p.health_of("acc").unwrap().0, "ok");
        // 网络抖动不该把号标成坏号
        p.report("plat", "acc", "m", false, ErrorClass::Transient);
        assert_eq!(p.health_of("acc").unwrap().0, "ok", "Transient 不应覆盖健康结论");
        // 402 / 429 有各自的状态
        p.report("plat", "acc", "m", false, ErrorClass::Quota);
        assert_eq!(p.health_of("acc").unwrap().0, "quota");
        p.report("plat", "acc", "m", false, ErrorClass::Rate);
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

#[cfg(test)]
mod persist_tests {
    use super::*;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aigw-pool-persist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn cooldown_survives_restart() {
        let d = tmp_dir("cool");
        {
            let p = Pool::open(&d);
            // 制造一条 402 冷却
            p.report("test-plat", "acc-1", "m1", false, ErrorClass::Quota);
            p.persist();
            assert!(p.blocked_of("acc-1").cooling_s > 0.0, "冷却应生效");
        }
        // 「重启」：新 Pool 从同一目录加载
        let p2 = Pool::open(&d);
        let b = p2.blocked_of("acc-1");
        assert!(b.cooling_s > 0.0, "冷却应跨重启恢复，实际 cooling_s={}", b.cooling_s);
    }

    #[test]
    fn credit_survives_restart() {
        let d = tmp_dir("credit");
        {
            let p = Pool::open(&d);
            p.record_credit("acc-1", Some(42.0));
            p.persist();
        }
        let p2 = Pool::open(&d);
        assert_eq!(p2.credit_of("acc-1"), Some(42.0));
    }

    #[test]
    fn expired_cooldown_is_dropped() {
        let d = tmp_dir("expired");
        // 手写一份「剩余 0 秒」的状态文件
        std::fs::write(
            d.join("pool-state.json"),
            r#"{"cooldown":[{"account":"a","model":"m","remain_s":0,"strikes":1}],"health":[],"credits":[]}"#,
        )
        .unwrap();
        let p = Pool::open(&d);
        assert_eq!(p.blocked_of("a").cooling_s, 0.0, "过期冷却不该恢复");
    }

    #[test]
    fn without_path_persist_is_noop() {
        let p = Pool::new(); // 无 data_dir
        p.record_credit("x", Some(1.0));
        p.persist(); // 不应 panic
        assert_eq!(p.credit_of("x"), Some(1.0));
    }
    #[test]
    fn retry_after_hint_is_adopted_verbatim() {
        // 上游说「60 秒后再来」就冻 60s，而不是按本地曲线（30s×抖动）猜
        let p = Pool::new();
        p.report_with_hint("pl", "a1", "m", false, ErrorClass::Rate, Some(60.0));
        assert!(!p.available("a1", "m"), "应按上游给的秒数进入冷却");
        let left = p.blocked_of("a1").cooling_s;
        assert!(left > 55.0 && left <= 60.5, "冷却时长应≈60s，实际 {left}");
    }

    #[test]
    fn retry_after_hint_is_clamped_to_cap() {
        // 上游偶尔回 "retry after 86400"：照做等于把这个号冻一整天
        let p = Pool::new();
        p.report_with_hint("pl", "a1", "m", false, ErrorClass::Rate, Some(86_400.0));
        let left = p.blocked_of("a1").cooling_s;
        assert!(left <= 901.0, "应夹到 900s 上限，实际 {left}");
        // 0 / 负数同样不能变成「立刻重试」的死循环
        let p2 = Pool::new();
        p2.report_with_hint("pl", "a2", "m", false, ErrorClass::Rate, Some(0.0));
        let left2 = p2.blocked_of("a2").cooling_s;
        // blocked_of 用的是「到现在还剩多久」，读取时刻已过了几微秒 → 允许亚秒余量
        assert!(left2 >= 0.9, "下限 1s（允许读取误差），实际 {left2}");
    }

    #[test]
    fn retry_after_hint_does_not_raise_strikes() {
        // 上游给了准确恢复时间 → 下次从轻档重新开始，不该带着历史惩罚
        let p = Pool::new();
        for _ in 0..3 {
            p.report_with_hint("pl", "a1", "m", false, ErrorClass::Rate, Some(5.0));
        }
        let left = p.blocked_of("a1").cooling_s;
        assert!(left <= 5.5, "连续有提示时仍是 5s 档而不是指数退避，实际 {left}");
    }

    #[test]
    fn report_without_hint_still_uses_local_backoff() {
        // 老路径（不带提示）必须完全不变：首次约 30s×抖动
        let p = Pool::new();
        p.report("pl", "a1", "m", false, ErrorClass::Rate);
        let left = p.blocked_of("a1").cooling_s;
        assert!(left >= 15.0 && left <= 45.0, "本地退避应约 30s，实际 {left}");
    }


    /// 401 后清快照：死号的旧余额不能再参与加权。
    #[test]
    fn clear_credit_removes_the_snapshot() {
        let pool = Pool::new();
        pool.record_credit_exp("a", Some(900.0), None);
        assert!(pool.credit_snapshot("a").is_some());
        pool.clear_credit("a");
        assert!(pool.credit_snapshot("a").is_none());
        // 不存在的账号清除是无操作（不许 panic）
        pool.clear_credit("ghost");
    }
}

#[cfg(test)]
mod reserve_tests {
    use super::*;

    /// ★ 2026-10-28（对齐 magpie reserve/ceiling）：
    /// 未达阈值的账号应优先被选中；达阈值的让位；全部超阈值时按使用率取最低。
    #[test]
    fn ceiling_lets_under_quota_accounts_go_first() {
        let p = Pool::new();
        p.set_ceiling(80);
        // acc-a 已用 90%（超阈值），acc-b 已用 10%
        p.set_token_rule("acc-a", Some(1000), 3600);
        p.set_token_rule("acc-b", Some(1000), 3600);
        // 直接写用量（测试用：模拟本窗口已用）
        p.mark_token_usage_for_test("acc-a", 900);
        p.mark_token_usage_for_test("acc-b", 100);

        let pick = p.pick("plat", &["acc-a".into(), "acc-b".into()], "m");
        assert_eq!(
            pick,
            Some("acc-b".into()),
            "acc-a 已用 90% 超阈值，应让 acc-b 先上"
        );
    }

    #[test]
    fn ceiling_off_keeps_default_order() {
        let p = Pool::new();
        // 不设 ceiling → 默认策略（无 credit 快照时保持原序）
        p.set_token_rule("acc-a", Some(1000), 3600);
        p.set_token_rule("acc-b", Some(1000), 3600);
        p.mark_token_usage_for_test("acc-a", 900);
        p.mark_token_usage_for_test("acc-b", 100);
        let pick = p.pick("plat", &["acc-a".into(), "acc-b".into()], "m");
        assert_eq!(pick, Some("acc-a".into()), "ceiling 关闭时应保持原序");
    }

    #[test]
    fn all_over_ceiling_picks_least_used() {
        let p = Pool::new();
        p.set_ceiling(50);
        p.set_token_rule("acc-a", Some(1000), 3600);
        p.set_token_rule("acc-b", Some(1000), 3600);
        p.mark_token_usage_for_test("acc-a", 900); // 90%
        p.mark_token_usage_for_test("acc-b", 600); // 60%
        let pick = p.pick("plat", &["acc-a".into(), "acc-b".into()], "m");
        assert_eq!(
            pick,
            Some("acc-b".into()),
            "两个都超 50% 阈值时，取使用率较低的 acc-b（60% < 90%）"
        );
    }
}
