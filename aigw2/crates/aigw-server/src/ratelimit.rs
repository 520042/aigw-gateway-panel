//! `/v1/*` 的每分钟固定窗口限流（全局 / 每 IP / 每访问密钥）。
//!
//! **为什么从 lib.rs 拆出来**：这段是「谁能把网关打爆」的唯一防线，三档额度互相
//! 牵制（每 IP 档被挡要退回已预扣的全局额度、密钥档桶满时宁可放行也不丢请求），
//! 细节多且容易改错。放在独立文件里能直接对着 `RateLimiter` 写单测，不必先构造
//! 一整个 `AppState` 再发请求。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const RATE_WINDOW: Duration = Duration::from_secs(60);

/// 每 IP 计数桶的内存上限。真实场景只有几十个来源，这个上限只为防「来源地址极多
/// 时 HashMap 无限增长」；超限时先清掉已过窗口的桶，仍超限就只按全局额度放行。
const RATE_MAX_IP_BUCKETS: usize = 4096;

#[derive(Clone, Copy)]
struct RateSlot {
    win: Instant,
    count: u32,
}

/// 每分钟固定窗口限流：全局 + 每客户端 IP（`0` 表示该项不启用）。
///
/// 为什么需要：`/v1/*` 是唯一真的消耗上游额度的入口。此前只有「每账号 2 路在途、
/// 等 15s 后 429」这一道闸 —— 那是**按账号的并发闸，不是速率闸**：单个客户端串行
/// 发几千条小请求永远不会同时占用 3 路在途，却足以把某个账号打到上游限流（冷却 15
/// 分钟），面板上表现为「所有平台突然一起 429」。本机多个进程、或拿到面板令牌的
/// 任意客户端都能造成这种效果。
pub struct RateLimiter {
    per_ip: u32,
    global: u32,
    ip: Mutex<HashMap<IpAddr, RateSlot>>,
    /// 按访问密钥（key id）的每分钟上限，见下面 `hit_key`。顺序：
    /// 谁最容易被一把 key 打穿，就先扣谁的额度。
    pub key_rate: u32,
    /// key id → 该 key 的计数槽。只放「非空」的槽，key 删掉后自然过期淘汰。
    key: Mutex<HashMap<String, RateSlot>>,
    g: Mutex<RateSlot>,
    /// 累计被挡下的请求数 + 最近一次的现场。
    ///
    /// 为什么需要：429 只出现在响应体与日志里，面板上完全不可见 —— 用户体感是
    /// 「突然变慢 / 全都失败」，却看不到「是我自己打太快」。把计数和上次的时间点
    /// 暴露到 /api/health，排障时第一眼就能排除/确认限流这个原因。
    rejected: AtomicU64,
    last_reject: Mutex<Option<RejectInfo>>,
}

/// 最近一次被限流的现场。
struct RejectInfo {
    at: Instant,
    /// "ip" 或 "global"：被哪个维度挡下的
    scope: &'static str,
    /// 当时告诉客户端的 Retry-After 秒数
    left_s: u64,
}

impl RateLimiter {
    pub fn new(per_ip: u32, global: u32) -> RateLimiter {
        let now = Instant::now();
        RateLimiter {
            per_ip,
            global,
            ip: Mutex::new(HashMap::new()),
            key_rate: 0,
            key: Mutex::new(HashMap::new()),
            g: Mutex::new(RateSlot { win: now, count: 0 }),
            rejected: AtomicU64::new(0),
            last_reject: Mutex::new(None),
        }
    }

    pub fn per_ip(&self) -> u32 {
        self.per_ip
    }

    pub fn global(&self) -> u32 {
        self.global
    }

    pub fn enabled(&self) -> bool {
        self.per_ip > 0 || self.global > 0
    }

    /// 按 key 限流是否启用。
    pub fn key_enabled(&self) -> bool {
        self.key_rate > 0
    }

    /// 记一次「带着访问密钥」的 /v1 请求。
    ///
    /// 为什么单独一层：每 IP 限流在本机部署下等同于**全局**限流（面板、脚本、客户端
    /// 全在 127.0.0.1），所以「给同事一把 key、他挂了死循环」会把这个人的面板和所有
    /// 别的客户端一起卡住 —— 而 key 的意义正是「把不同用途分开」。按 key 计数才能
    /// 让限额落在「谁在刷」上。这个 map 有界：key 删了就自然淘汰，上限同 IP 桶。
    pub fn hit_key(&self, key_id: &str) -> Result<(), u64> {
        if !self.key_enabled() {
            return Ok(());
        }
        let now = Instant::now();
        let mut m = self.key.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= RATE_MAX_IP_BUCKETS {
            m.retain(|_, s| now.duration_since(s.win) < RATE_WINDOW);
        }
        if m.len() >= RATE_MAX_IP_BUCKETS {
            // 桶满且全在窗口内：宁可放行也不丢请求（有界内存优先），代价是新 key 暂不计。
            return Ok(());
        }
        let slot = m.entry(key_id.to_string()).or_insert(RateSlot { win: now, count: 0 });
        roll(slot, now);
        if slot.count >= self.key_rate {
            let wait = retry_after(slot, now);
            drop(m);
            self.note_reject("key", wait);
            return Err(wait);
        }
        slot.count += 1;
        Ok(())
    }

    /// 记一次 `/v1` 请求；超限返回需要等待的秒数（用于 `Retry-After`）。
    pub fn hit(&self, ip: Option<IpAddr>) -> Result<(), u64> {
        if !self.enabled() {
            return Ok(());
        }
        let now = Instant::now();
        {
            let mut g = self.g.lock().unwrap_or_else(|e| e.into_inner());
            roll(&mut g, now);
            if self.global > 0 && g.count >= self.global {
                let wait = retry_after(&g, now);
                drop(g);
                self.note_reject("global", wait);
                return Err(wait);
            }
            if self.global > 0 {
                g.count += 1;
            }
        }
        if self.per_ip > 0 {
            if let Some(ip) = ip {
                let mut m = self.ip.lock().unwrap_or_else(|e| e.into_inner());
                if m.len() >= RATE_MAX_IP_BUCKETS {
                    m.retain(|_, s| now.duration_since(s.win) < RATE_WINDOW);
                }
                // 清完仍满：只按全局额度放行（有界内存优先于逐 IP 精确）
                if m.len() < RATE_MAX_IP_BUCKETS {
                    let slot = m.entry(ip).or_insert(RateSlot { win: now, count: 0 });
                    roll(slot, now);
                    if slot.count >= self.per_ip {
                        let wait = retry_after(slot, now);
                        drop(m);
                        // 这条没被放行：退回刚才预扣的全局额度
                        if self.global > 0 {
                            let mut g = self.g.lock().unwrap_or_else(|e| e.into_inner());
                            g.count = g.count.saturating_sub(1);
                        }
                        self.note_reject("ip", wait);
                        return Err(wait);
                    }
                    slot.count += 1;
                }
            }
        }
        Ok(())
    }

    /// 记一次「被挡下」：累计数 + 最近现场。
    fn note_reject(&self, scope: &'static str, left_s: u64) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        let mut l = self.last_reject.lock().unwrap_or_else(|e| e.into_inner());
        *l = Some(RejectInfo {
            at: Instant::now(),
            scope,
            left_s,
        });
    }

    /// 给面板/health 的快照：限流是否启用、上限多少、被挡过几次、上次多久前。
    pub fn snapshot(&self) -> Value {
        let last = self.last_reject.lock().unwrap_or_else(|e| e.into_inner());
        let (ago, scope, left) = match last.as_ref() {
            Some(r) => (
                Some(r.at.elapsed().as_secs()),
                json!(r.scope),
                Some(r.left_s),
            ),
            None => (None, Value::Null, None),
        };
        json!({
            "enabled": self.enabled(),
            "per_ip_rpm": self.per_ip,
            "global_rpm": self.global,
            "key_rpm": self.key_rate,
            "rejected_total": self.rejected.load(Ordering::Relaxed),
            "last_reject_s_ago": ago,
            "last_reject_scope": scope,
            "last_retry_after_s": left,
        })
    }
}

fn roll(slot: &mut RateSlot, now: Instant) {
    if now.duration_since(slot.win) >= RATE_WINDOW {
        slot.win = now;
        slot.count = 0;
    }
}

fn retry_after(slot: &RateSlot, now: Instant) -> u64 {
    let el = now.duration_since(slot.win).as_secs();
    RATE_WINDOW.as_secs().saturating_sub(el).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim(per_ip: u32, global: u32) -> RateLimiter {
        RateLimiter::new(per_ip, global)
    }

    #[test]
    fn disabled_when_both_zero() {
        let l = lim(0, 0);
        assert!(!l.enabled());
        // 没开限流时任何调用都必须放行，且不记账
        for _ in 0..1000 {
            assert!(l.hit(Some("10.0.0.1".parse().unwrap())).is_ok());
            assert!(l.hit(None).is_ok());
        }
        assert_eq!(l.snapshot()["rejected_total"], 0);
    }

    #[test]
    fn global_limit_rejects_and_reports_retry_after() {
        let l = lim(0, 3);
        for _ in 0..3 {
            assert!(l.hit(None).is_ok());
        }
        let wait = l.hit(None).expect_err("第 4 条应被挡");
        assert!((1..=60).contains(&wait), "Retry-After 秒数异常：{wait}");
        let snap = l.snapshot();
        assert_eq!(snap["rejected_total"], 1);
        assert_eq!(snap["last_reject_scope"], "global");
        assert_eq!(snap["last_retry_after_s"], wait);
    }

    #[test]
    fn per_ip_limit_is_independent_across_ips() {
        let l = lim(2, 0);
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(l.hit(Some(a)).is_ok());
        assert!(l.hit(Some(a)).is_ok());
        assert!(l.hit(Some(a)).is_err(), "同一 IP 第 3 条应被挡");
        // 换一个 IP 不受影响：限流是「按来源」而不是「按全站」
        assert!(l.hit(Some(b)).is_ok());
        assert!(l.hit(Some(b)).is_ok());
        assert_eq!(l.snapshot()["last_reject_scope"], "ip");
    }

    #[test]
    fn rejected_ip_request_refunds_prepaid_global_quota() {
        // ★ 这条是这段代码最容易写错的地方：hit() 先扣全局额度，再判每 IP 额度。
        //   每 IP 档拒绝时必须把刚扣掉的全局额度退回去，否则「一个 IP 刷爆自己」
        //   会顺带把全站额度耗光 —— 表现为别的 IP 明明没超限却被 429。
        let l = lim(1, 100);
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(l.hit(Some(a)).is_ok());
        assert!(l.hit(Some(a)).is_err(), "a 的第 2 条应被每 IP 档挡下");
        // 全局额度此时应只被扣过 1 次（a 的第 1 条），b 仍能正常通过
        assert!(l.hit(Some(b)).is_ok());
        assert_eq!(l.snapshot()["rejected_total"], 1);
    }

    #[test]
    fn key_limit_is_separate_and_skipped_when_unset() {
        let l = lim(0, 0);
        // 未设置 key 额度时不计数、不拒绝
        for _ in 0..100 {
            assert!(l.hit_key("k1").is_ok());
        }
        assert!(!l.key_enabled());
    }

    #[test]
    fn snapshot_shape_is_stable_for_frontend() {
        // 前端「运行配置」页按这些键取值，改名必须同步改前端
        let snap = lim(10, 20).snapshot();
        for k in [
            "enabled",
            "per_ip_rpm",
            "global_rpm",
            "key_rpm",
            "rejected_total",
            "last_reject_s_ago",
            "last_reject_scope",
            "last_retry_after_s",
        ] {
            assert!(snap.get(k).is_some(), "snapshot 缺字段 {k}");
        }
        assert_eq!(snap["enabled"], true);
        assert_eq!(snap["per_ip_rpm"], 10);
        // 还没拒绝过时，现场字段必须是 null 而不是 0（0 会被读成「刚刚拒过」）
        assert!(snap["last_reject_s_ago"].is_null());
    }

    #[test]
    fn retry_after_never_returns_zero() {
        // 恰好在窗口边界上也不能回 0：Retry-After: 0 会让客户端立刻重试再次撞墙
        let now = Instant::now();
        let mut slot = RateSlot { win: now, count: 1 };
        assert!(retry_after(&slot, now) >= 1);
        slot.win = now - RATE_WINDOW + Duration::from_millis(1);
        assert_eq!(retry_after(&slot, now), 1);
    }
}
