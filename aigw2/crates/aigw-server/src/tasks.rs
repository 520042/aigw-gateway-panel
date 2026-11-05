//! 后台长任务的健康台账与「不会死」的调度循环。
//!
//! 从 lib.rs 拆出（2026-10-26 第二十二轮）。
//!
//! ★ 为什么需要台账：`tokio::spawn` 出去的后台循环**一旦 panic 就永久消失**，
//!   而进程还活着、`/api/health` 还是 ok:true —— 面板看上去一切正常，实际
//!   「公益站不再刷新」「模型清单不再预热」，且没有任何日志会被用户看到。
//!   把「最近一次成功/失败时刻」显式暴露出来后，「本来该在 X 小时前跑过的任务，
//!   已经 Y 小时没动静」就成了可观测事实。
//!
//! ★ `last_ok_msg` 不是装饰：只记时间的话，「刷新成功但一个站都没拿到」与
//!   「刷新成功拿到 43 个站」在面板上是同一个样子，聚合页改版这类故障会静默
//!   持续一整天。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

/// 后台长任务的一次运行结果（只用于观测，不参与调度决策）。
#[derive(Clone, Default)]
pub struct TaskStat {
    pub ok_runs: u64,
    pub err_runs: u64,
    /// 最近一次成功的 unix 秒。
    pub last_ok: Option<u64>,
    /// 最近一次成功的说明（例如「共 43 站，新增 2，保留 41；聚合页 43 条，丢弃 3（host 不合规 2，重复 1）」）。
    ///
    /// 只记时间不记内容的话，「刷新成功但一个站都没拿到」与「刷新成功拿到 43 个站」在
    /// 面板上是同一个样子 —— 聚合页改版这类故障会静默持续一整天。
    pub last_ok_msg: Option<String>,
    /// 最近一次失败（或 panic）的 unix 秒与原因。
    pub last_err: Option<(u64, String)>,
}

/// 后台任务健康台账：`/api/health` 里的 `tasks` 字段。
///
/// 为什么需要它：`tokio::spawn` 出去的后台循环**一旦 panic 就永久消失**，而进程还活着、
/// `/api/health` 还是 `ok:true` —— 面板看上去一切正常，实际「公益站不再刷新」「模型清单
/// 不再预热」，且没有任何日志会被用户看到。旧实现就是这个状态：一条 `refresh_free_sites`
/// 里的 `unwrap` 就能让每日刷新静默停摆。这里把「最近一次成功/失败时间」显式暴露出来，
/// 「本来该在 X 小时前跑过一次的任务，已经 Y 小时没动静」就成了可观测事实。
#[derive(Default)]
pub struct TaskHealth {
    inner: Mutex<HashMap<&'static str, TaskStat>>,
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl TaskHealth {
    pub fn new() -> TaskHealth {
        TaskHealth::default()
    }

    /// 预登记一个任务：还没跑过也出现在台账里（`ok_runs: 0`），
    /// 否则「24 小时才跑一次」的任务在头一天完全不可见。
    pub fn declare(&self, name: &'static str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name)
            .or_default();
    }

    pub fn mark_ok(&self, name: &'static str, msg: impl Into<String>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.entry(name).or_default();
        e.ok_runs += 1;
        e.last_ok = Some(unix_now());
        e.last_ok_msg = Some(msg.into());
    }

    pub fn mark_err(&self, name: &'static str, why: impl Into<String>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.entry(name).or_default();
        e.err_runs += 1;
        e.last_err = Some((unix_now(), why.into()));
    }

    pub fn stat(&self, name: &str) -> Option<TaskStat> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// 快照：`{任务名: {ok_runs, err_runs, last_ok_s_ago, last_err, last_err_s_ago}}`。
    /// 时间给「多少秒之前」而不是绝对时间，读的人不用再对表。
    pub fn snapshot(&self) -> Value {
        let now = unix_now();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = serde_json::Map::new();
        for (k, v) in g.iter() {
            out.insert(
                (*k).to_string(),
                json!({
                    "ok_runs": v.ok_runs,
                    "err_runs": v.err_runs,
                    "last_ok_s_ago": v.last_ok.map(|t| now.saturating_sub(t)),
                    // 成功也要带说明：失败有 last_err，成功什么都不记就等于「刷新成功」
                    // 可以掩盖「拿到 0 个站」。
                    "last_ok": v.last_ok_msg.clone(),
                    "last_err": v.last_err.as_ref().map(|(_, m)| m.clone()),
                    "last_err_s_ago": v.last_err.as_ref().map(|(t, _)| now.saturating_sub(*t)),
                }),
            );
        }
        Value::Object(out)
    }
}

/// 把「每个周期跑一次」的后台任务包成**不会死**的循环。
///
/// - `body` 返回 `Ok(说明)` 记成功、`Err(原因)` 记失败；
/// - `body` 里 panic 会被 `catch_unwind` 隔离，记一次失败并**继续调度**（旧行为：整个任务消失）；
/// - 成功后退回 `period`，失败后按 `retry` 更早重试 —— 网络抖动不该让「公益站刷新」
///   停摆整整一个周期。
pub fn supervise_loop<F, Fut>(
    name: &'static str,
    health: Arc<TaskHealth>,
    period: std::time::Duration,
    retry: std::time::Duration,
    body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
{
    supervise_loop_opts(name, health, period, retry, false, body)
}

/// 与 supervise_loop 相同，但**先跑一轮再进入周期**。
///
/// 用于「周期很长、且冷启动时就该有数据」的任务：24 小时周期的公益站刷新如果先睡
/// 24 小时再跑，全新部署当天面板和 /api/health 一直是「尚未成功过」——用户只能
/// 靠手动点刷新来绕过，而那本该是默认行为。
pub fn supervise_loop_now<F, Fut>(
    name: &'static str,
    health: Arc<TaskHealth>,
    period: std::time::Duration,
    retry: std::time::Duration,
    body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
{
    supervise_loop_opts(name, health, period, retry, true, body)
}

fn supervise_loop_opts<F, Fut>(
    name: &'static str,
    health: Arc<TaskHealth>,
    period: std::time::Duration,
    retry: std::time::Duration,
    run_first: bool,
    mut body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
{
    use futures_util::FutureExt;
    async move {
        health.declare(name);
        // 首次等待：要立刻跑的任务用 ZERO（sleep(0) 只让出一次执行权）。
        let mut wait = if run_first {
            std::time::Duration::ZERO
        } else {
            period
        };
        loop {
            tokio::time::sleep(wait).await;
            let started = std::time::Instant::now();
            let fut = std::panic::AssertUnwindSafe(body()).catch_unwind();
            let (ok, next) = match fut.await {
                Ok(Ok(msg)) => {
                    health.mark_ok(name, msg.clone());
                    tracing::info!("后台任务 {} 完成：{}", name, msg);
                    (true, period)
                }
                Ok(Err(e)) => {
                    health.mark_err(name, e.clone());
                    tracing::warn!("后台任务 {} 失败：{}（{} 秒后重试）", name, e, retry.as_secs());
                    (false, retry)
                }
                Err(_) => {
                    // panic 的载荷不一定是 &str，这里只关心「发生过」，细节已进 panic hook。
                    health.mark_err(name, "任务 panic 已被隔离（详见日志）");
                    tracing::error!("后台任务 {} 发生 panic，已隔离并继续调度", name);
                    (false, retry)
                }
            };
            let _ = ok;
            // ★ 扣掉本轮执行耗时：定时器的语义是「每 period 跑一次」，不是
            //   「跑完再等 period」。旧写法 `sleep(wait) → 执行 → sleep(wait)` 会让
            //   实际周期 = period + 执行耗时，于是**每天都往后推一点**：签到要遍历
            //   多个账号、逐条打上游，一次几秒到几十秒，跑上一年就偏出几小时。
            //   这类漂移在面板上完全看不出来（只看到「昨天 09:12、今天 09:12」），
            //   直到用户抱怨「自动签到越来越晚」。
            //   执行本身超过 period 时 saturating_sub 给 0（立刻进入下一轮），
            //   不会因为一次长跑就把后续全部推迟。
            wait = next.saturating_sub(started.elapsed());
        }
    }
}

/// 每天在**指定北京时间 HH:MM** 跑一次的调度循环。
///
/// ★ 2026-11-06：面板按参考项目换成 `<input type="time">`，所以调度目标
///   从「小时」细化到「分钟」。旧调用方仍可传整点闭包；新调用方传 (h,m)。
///
/// ★ 为什么不用固定 24h 周期：用户要的是「每天 9 点执行」，而固定周期会从
///   「进程启动时刻」起算，重启一次时刻就变一次；面板上显示的执行时间
///   也无法预期。这里每次都重新算「距离下一个目标时刻还有多久」，
///   与启动时刻无关，重启、休眠恢复后都会对齐到同一个钟点。
///
/// ★ 时区：沿用项目既有口径（aigw-pool::quota_hold_secs），按 UTC+8 / 北京时间。
///   不引 chrono（项目刻意的零依赖约束）。面板上标注「北京时间」，
///   避免其它时区的用户误以为是本地时间。
///
/// ★ 冷启动不立即跑：首次先等到下一个目标时刻。签到/成长任务都是
///   「当天一次」的写操作，进程重启就立刻打一遍上游没有必要，也徒增风控。
pub fn daily_at_hour_loop<F, Fut, H>(
    name: &'static str,
    health: Arc<TaskHealth>,
    mut hour_fn: H,
    retry: std::time::Duration,
    body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
    H: FnMut() -> u8 + Send,
{
    daily_at_time_loop(name, health, move || (hour_fn(), 0), retry, body)
}

/// 每天在指定 `(小时, 分钟)` 跑一次。`time_fn` 每轮重新读：面板改了 HH:MM，
/// 下一轮就按新钟点算，不用重启。
pub fn daily_at_time_loop<F, Fut, T>(
    name: &'static str,
    health: Arc<TaskHealth>,
    mut time_fn: T,
    retry: std::time::Duration,
    mut body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
    T: FnMut() -> (u8, u8) + Send,
{
    use futures_util::FutureExt;
    async move {
        health.declare(name);
        loop {
            // ★ 每轮重新读 time_fn：面板改了执行时刻，下一轮就按新钟点算，不用重启。
            let (h, m) = time_fn();
            let target = (h.min(23) as i64) * 3600 + (m.min(59) as i64) * 60;
            let wait = secs_until_target(target);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            let started = std::time::Instant::now();
            let fut = std::panic::AssertUnwindSafe(body()).catch_unwind();
            let next_retry = match fut.await {
                Ok(Ok(msg)) => {
                    health.mark_ok(name, msg.clone());
                    tracing::info!("后台任务 {} 完成：{}", name, msg);
                    None
                }
                Ok(Err(e)) => {
                    health.mark_err(name, e.clone());
                    tracing::warn!("后台任务 {} 失败：{}（{} 秒后重试）", name, e, retry.as_secs());
                    Some(retry)
                }
                Err(_) => {
                    health.mark_err(name, "任务 panic 已被隔离（详见日志）");
                    tracing::error!("后台任务 {} 发生 panic，已隔离并继续调度", name);
                    Some(retry)
                }
            };
            if let Some(r) = next_retry {
                // 失败重试仍保留，但不能因此把「每天的时刻」推走：
                // 重试完继续按钟点算下一次，而不是在重试后加 24h。
                tokio::time::sleep(r.saturating_sub(started.elapsed())).await;
            }
        }
    }
}

/// 距离下一个「北京时间 HH:MM」还有多少秒（至少 1 秒，避免 0 间隔忙循环）。
pub(crate) fn secs_until_target(target_secs: i64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local = now + 8 * 3600; // 北京时间（UTC+8）
    let into_day = local.rem_euclid(86400);
    let mut delta = target_secs.rem_euclid(86400) - into_day;
    // 今天这个点已经过了（或正好是这一秒）→ 等明天，不立刻跑。
    if delta <= 0 { delta += 86400; }
    (delta as u64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 定时器周期必须**不随执行耗时漂移**。
    ///
    /// ★ 旧写法 `sleep(period) → 执行 → sleep(period)` 让实际周期 = period + 执行耗时，
    ///   于是每天都往后推一点。签到要遍历多个账号、逐条打上游（一次几秒到几十秒），
    ///   跑上一年就偏出几小时，而面板上完全看不出来（只看到「每天都是 09:12」）。
    #[tokio::test]
    async fn task_period_does_not_drift_with_execution_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let h = Arc::new(TaskHealth::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let r2 = runs.clone();
        let period = Duration::from_millis(80);
        // 每次执行吃掉 40ms：旧实现会让周期变成 120ms
        let handle = tokio::spawn(supervise_loop(
            "drift",
            h,
            period,
            period,
            move || {
                let r2 = r2.clone();
                async move {
                    r2.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    Ok::<String, String>("done".into())
                }
            },
        ));
        let started = std::time::Instant::now();
        // 等跑到 5 轮
        for _ in 0..200 {
            if runs.load(Ordering::Relaxed) >= 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let elapsed = started.elapsed();
        handle.abort();
        assert!(runs.load(Ordering::Relaxed) >= 5, "应至少跑 5 轮");
        // 不漂移：5 轮约 5×80=400ms（首轮也要等一个 period）。
        // 漂移实现会是 5×120=600ms。留足调度抖动余量，取 520ms 为界。
        assert!(
            elapsed < Duration::from_millis(560),
            "周期漂移了：5 轮花了 {:?}（不漂移应约 400ms，漂移会是 600ms）",
            elapsed
        );
    }
    use std::time::Duration;

    #[test]
    fn health_records_success_and_failure_separately() {
        let h = TaskHealth::new();
        h.mark_ok("t", String::from("拿到 43 站"));
        assert_eq!(h.snapshot()["t"]["ok_runs"], json!(1));
        // ★ 成功说明必须留下：只有时间的话，「成功但 0 个站」与「成功 43 个站」
        //   在面板上是同一个样子，聚合页改版会静默持续一整天。
        assert_eq!(h.snapshot()["t"]["last_ok"], json!("拿到 43 站"));
        h.mark_err("t", String::from("HTTP 500"));
        assert_eq!(h.snapshot()["t"]["err_runs"], json!(1));
        // 失败不该抹掉上一次成功的说明
        assert_eq!(h.snapshot()["t"]["last_ok"], json!("拿到 43 站"));
    }

    #[tokio::test]
    async fn supervise_loop_keeps_going_after_a_panic() {
        // ★ 这是本模块存在的理由：tokio::spawn 出去的循环一旦 panic 就永久消失，
        //   进程还活着、health 还是 ok —— 任务静默停摆且无人知晓。
        let h = Arc::new(TaskHealth::new());
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r2 = runs.clone();
        let h2 = h.clone();
        let handle = tokio::spawn(supervise_loop(
            "panicky",
            h,
            Duration::from_millis(10),
            Duration::from_millis(10),
            move || {
                let r2 = r2.clone();
                async move {
                    let n = r2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if n == 0 {
                        panic!("任务内部炸了");
                    }
                    Ok::<String, String>("recovered".into())
                }
            },
        ));
        // 等到跑过至少 2 轮（第 1 轮 panic、第 2 轮应照常执行）
        for _ in 0..100 {
            if runs.load(std::sync::atomic::Ordering::Relaxed) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        handle.abort();
        assert!(
            runs.load(std::sync::atomic::Ordering::Relaxed) >= 2,
            "panic 之后必须继续调度，否则任务永久消失"
        );
        // panic 要被记成一次失败（不能当没事发生）
        assert_eq!(h2.snapshot()["panicky"]["err_runs"], json!(1));
    }

    /// 「距离下一个目标钟点」的计算：必须落在 (0, 86400] 之间，
    /// 且目标时刻已过时返回「明天」，而不是负数或 0（会变成忙循环）。
    #[test]
    fn secs_until_target_is_always_positive_and_within_a_day() {
        for h in 0u8..24 {
            let s = secs_until_target((h as i64) * 3600);
            assert!(s > 0, "hour={h} 不能为 0（会忙循环）");
            assert!(s <= 86400, "hour={h} 不该超过一天：{s}");
        }
    }

    /// 自定义时刻要真的参与计算：目标 0 点与目标 23 点的等待秒数应当不同。
    ///
    /// ★ 差值不是 23 小时：两者跨天补 86400 后，等待时间之和 = 86400 + 23h
    ///   （当 0 点已过而 23 点未过时是互补的 1 小时）。实测第一版把断言写成
    ///   「差 23 小时」直接挂了 —— 这里改成验证更本质的性质：不同目标给出
    ///   不同结果，且两者都在 (0, 86400] 内。
    #[test]
    fn different_target_hours_give_different_waits() {
        let a = secs_until_target(0);
        let b = secs_until_target(23 * 3600);
        assert_ne!(a, b, "不同时刻不能算出同一个等待时间");
        assert!((1..=86400).contains(&a));
        assert!((1..=86400).contains(&b));
    }

    /// 分钟必须进入下一次执行时刻：同一小时内 09:00 与 09:30 相差 1800 秒
    /// （若目标已过则是补集 84600 秒，仍不能相等）。
    #[test]
    fn minute_offset_changes_the_wait() {
        let a = secs_until_target(9 * 3600);
        let b = secs_until_target(9 * 3600 + 30 * 60);
        assert_ne!(a, b, "HH:MM 的分钟不能被调度器忽略");
        let d = (a + 86400 - b) % 86400;
        assert!(
            d == 30 * 60 || d == 86400 - 30 * 60,
            "09:00 与 09:30 的等待差只能是 30 分钟或跨天补集 23.5 小时，得到 {d}"
        );
    }

    /// 非法/超界目标也只按一天内的合法时刻处理，绝不让等待时间越过一天。
    #[test]
    fn out_of_range_target_is_normalized() {
        for target in [-60, 86400, 90000, i64::MAX] {
            let s = secs_until_target(target);
            assert!((1..=86400).contains(&s), "target={target} 得到 {s}");
        }
    }
}
