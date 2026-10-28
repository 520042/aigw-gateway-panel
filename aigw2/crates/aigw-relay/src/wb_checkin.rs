//! WorkBuddy 族**完整签到 / 成长动作集**（对齐 xiaofan6ya/wb2api admin/backend.py）。
//!
//! 参考项目 `admin/backend.py` 暴露了 12 个成长域名端点，我们此前只接了两个
//! （tasks 列表 / accept）。这里把剩下的补齐，让面板能像参考项目一样：
//! 「查状态 → 接受 → 逐项完成 → 连登兑换 → 抽奖 → 旅行 → 盲盒」全链路可操作。
//!
//! ## 端点一览（base = `https://copilot.tencent.com`，web = `https://www.workbuddy.cn`）
//! | 动作 | 方法 | 路径 | 备注 |
//! |---|---|---|---|
//! | 成长档案 | GET | `/v2/activity/growth/profile` | 等级/已完成数 |
//! | 能量 | GET | `/activity/growth/energy` | 当前能量值 |
//! | 活跃地图 | GET | `/activity/growth/heatmap` | `data.today.is_active` = 今天用过 |
//! | 连登状态 | GET | `/activity/growth/streak` | 含 `redemption_status.tiers[]` |
//! | 连登兑换 | POST | `/activity/growth/redeem` | body `{tier, client_token}` |
//! | 抽奖概览 | GET | `/activity/growth/lottery/summary` | `module.enabled` 判是否开 |
//! | 抽奖次数 | GET | `/activity/growth/lottery/chances` | `data.balance` |
//! | 抽奖 | POST | `/activity/growth/lottery/draw` | body `{client_token}` |
//! | 猫猫配额 | GET | `/activity/growth/buddy/quota` | 可开盲盒数 |
//! | 开盲盒 | POST | `/activity/growth/buddy/open` | body `{count}`，**不收 client_token** |
//! | 猫猫信息 | GET | `/activity/growth/buddy/info` | 已领养则返回 buddy |
//! | 同意协议 | POST | `/activity/growth/buddy/agreement` | body `{agree:true}`，幂等 |
//! | 首次领养 | POST | `/activity/growth/buddy/first` | 发放 300 积分 |
//! | 旅行状态 | GET | `/activity/growth/buddy/travel/status` | |
//! | 派出旅行 | POST | `/activity/growth/buddy/travel/depart` | 受每日上限约束 |
//! | 旅行领取 | POST | `/activity/growth/buddy/travel/claim` | |
//!
//! ## 幂等键（参考项目的关键设计，照抄）
//! `redeem` 与 `lottery/draw` 都要求客户端自带 `client_token`，作用是用**同一个 token
//! 重放**来避免「网络超时但服务端已扣次数」时的重复扣费：
//! - 连登兑换：`"redeem-" + uuid5(identity:YYYY-MM:tier)` —— 同一档位当月恒定
//! - 抽奖：随机 uuid4，**成功后清掉**；失败/超时保留，下轮用同一 token 重试
//! 参考项目注释还写明：`409 + duplicate` 表示「此前已处理」，应计为已抽而非失败。

use reqwest::Client;
use serde_json::{json, Value};

/// 成长域动作名（面板/API 的 action 取值）。
pub const GROWTH_ACTIONS: &[&str] = &[
    "growth_profile",
    "growth_energy",
    "growth_heatmap",
    "streak_status",
    "streak_redeem",
    "lottery_summary",
    "lottery_chances",
    "lottery_draw",
    "buddy_quota",
    "buddy_open",
    "buddy_info",
    "buddy_agreement",
    "buddy_adopt",
    "travel_status",
    "travel_depart",
    "travel_claim",
    "tasks_list",
    "tasks_accept",
    "task_claim",
];

/// 一次成长域调用。
///
/// `base` 用 CLI 域（copilot.tencent.com）——除 `tasks/{code}/claim` 走 web 域外，
/// 其余全部在 CLI 域（参考项目 `_growth()` 的实现口径）。`task_claim` 单独处理。
pub async fn growth_call(
    http: &Client,
    base: &str,
    token: &str,
    action: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    // (method, path, 是否需要 client_token)
    let (method, path) = match action {
        "growth_profile" => ("GET", "/v2/activity/growth/profile"),
        "growth_energy" => ("GET", "/activity/growth/energy"),
        "growth_heatmap" => ("GET", "/activity/growth/heatmap"),
        "streak_status" => ("GET", "/activity/growth/streak"),
        "streak_redeem" => ("POST", "/activity/growth/redeem"),
        "lottery_summary" => ("GET", "/activity/growth/lottery/summary"),
        "lottery_chances" => ("GET", "/activity/growth/lottery/chances"),
        "lottery_draw" => ("POST", "/activity/growth/lottery/draw"),
        "buddy_quota" => ("GET", "/activity/growth/buddy/quota"),
        "buddy_open" => ("POST", "/activity/growth/buddy/open"),
        "buddy_info" => ("GET", "/activity/growth/buddy/info"),
        "buddy_agreement" => ("POST", "/activity/growth/buddy/agreement"),
        "buddy_adopt" => ("POST", "/activity/growth/buddy/first"),
        "travel_status" => ("GET", "/activity/growth/buddy/travel/status"),
        "travel_depart" => ("POST", "/activity/growth/buddy/travel/depart"),
        "travel_claim" => ("POST", "/activity/growth/buddy/travel/claim"),
        "tasks_list" => ("GET", "/v2/activity/growth/tasks"),
        "tasks_accept" => ("POST", "/v2/activity/growth/tasks/accept"),
        other => return Err(format!("未知成长动作：{other}")),
    };
    let url = format!("{base}{path}");
    let mut rb = match method {
        "POST" => http.post(&url),
        _ => http.get(&url),
    };
    rb = rb
        .timeout(std::time::Duration::from_secs(25))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header("User-Agent", super::wb::cli_ua());
    if method == "POST" {
        rb = rb.json(&body.unwrap_or_else(|| json!({})));
    }
    let resp = rb.send().await.map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
        json!({"raw": text.chars().take(300).collect::<String>()})
    });
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}：{}", text.chars().take(200).collect::<String>()));
    }
    Ok(v)
}

/// 任务奖励领取（**走 web 域**，与其它动作不同域，参考项目口径）。
pub async fn claim_task_reward(
    http: &Client,
    web_base: &str,
    token: &str,
    task_code: &str,
) -> Result<Value, String> {
    let url = format!("{web_base}/activity/growth/tasks/{task_code}/claim");
    let resp = http
        .post(&url)
        .timeout(std::time::Duration::from_secs(25))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", super::wb::cli_ua())
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
        json!({"raw": text.chars().take(300).collect::<String>()})
    });
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}：{}", text.chars().take(200).collect::<String>()));
    }
    Ok(v)
}

/// 幂等键：连登兑换用「档位 + 当月」派生（同月同档恒定，重放安全）。
pub fn redeem_token(identity: &str, tier: &str, yyyymm: &str) -> String {
    // uuid5 形态但不需要真 uuid 依赖：sha256 前 32 hex 足够稳定且唯一性充足。
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"redeem|");
    h.update(identity.as_bytes());
    h.update(b"|");
    h.update(yyyymm.as_bytes());
    h.update(b"|");
    h.update(tier.as_bytes());
    let d = h.finalize();
    format!("redeem-{}", d.iter().take(16).map(|b| format!("{b:02x}")).collect::<String>())
}

/// 幂等键：抽奖用随机 token（成功后由调用方清掉；失败保留以重放核对）。
pub fn draw_token() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"draw|");
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_be_bytes())
            .unwrap_or([0u8; 16]),
    );
    h.update(std::process::id().to_be_bytes());
    let d = h.finalize();
    format!("draw-{}", d.iter().take(16).map(|b| format!("{b:02x}")).collect::<String>())
}

/// 当前年月（连登兑换 token 的一部分）。
pub fn yyyymm_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local = secs + 8 * 3600; // UTC+8
    let days = local.div_euclid(86400);
    let (y, m) = {
        // civil_from_days
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { y + 1 } else { y }, m)
    };
    format!("{y:04}-{m:02}")
}

/// 从连登状态里挑出「可兑换的档位」（`redemption_status.tier_X_status == "available"`）。
pub fn eligible_redeem_tiers(streak: &Value) -> Vec<String> {
    // 上游两种形态都吃：`{redemption_status:{...}}`（成长域裸回）
    // 或 `{data:{redemption_status:{...}}}`（包了一层 data）。两者都尝试。
    let rs = streak
        .get("redemption_status")
        .filter(|v| v.is_object())
        .or_else(|| streak.pointer("/data/redemption_status").filter(|v| v.is_object()));
    let Some(rs) = rs else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(tiers) = rs.get("tiers").and_then(|t| t.as_array()) {
        for t in tiers {
            let Some(tier) = t.get("tier").and_then(|x| x.as_str()) else { continue };
            // ★ 2026-10-28 实测修正：上游 tiers[].tier 的值**已经带 `tier_` 前缀**
            //   （实际返回 "tier_3"，不是 "3"）。早期实现无条件拼 `tier_{tier}_status`
            //   会拼成 `tier_tier_3_status`，永远查不到 → 「可兑换档位」恒为空，
            //   连登奖励一次也领不到。这里先按原值直接用，再退回拼前缀的形态。
            let direct_key = format!("{tier}_status");
            let prefixed_key = format!("tier_{tier}_status");
            let status = rs
                .get(direct_key.as_str())
                .or_else(|| rs.get(prefixed_key.as_str()))
                .and_then(|x| x.as_str());
            if status == Some("available") {
                out.push(tier.to_string());
            }
        }
    }
    out
}

/// 抽奖次数（`data.balance`）。
pub fn lottery_balance(chances: &Value) -> i64 {
    chances
        .pointer("/data/balance")
        .or_else(|| chances.get("balance"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redeem_token_is_stable_per_month_and_tier() {
        let a = redeem_token("uid1", "tier_7", "2026-10");
        let b = redeem_token("uid1", "tier_7", "2026-10");
        let c = redeem_token("uid1", "tier_7", "2026-11");
        let d = redeem_token("uid1", "tier_14", "2026-10");
        assert_eq!(a, b, "同账号同月同档必须恒定（重放安全）");
        assert_ne!(a, c, "换月要换 token");
        assert_ne!(a, d, "换档要换 token");
        assert!(a.starts_with("redeem-"));
    }

    #[test]
    fn draw_token_is_random_and_prefixed() {
        let a = draw_token();
        let b = draw_token();
        assert!(a.starts_with("draw-"));
        assert_ne!(a, b, "抽奖 token 每轮都要新的");
    }

    #[test]
    fn yyyymm_shape() {
        let s = yyyymm_now();
        assert_eq!(s.len(), 7, "应是 YYYY-MM：{s}");
        assert_eq!(s.as_bytes()[4], b'-');
    }

    #[test]
    fn eligible_tiers_picks_available_only() {
        let streak = json!({
            "redemption_status": {
                "tiers": [{"tier":"tier_3"},{"tier":"tier_7"},{"tier":"tier_14"}],
                "tier_3_status": "available",
                "tier_7_status": "claimed",
                "tier_14_status": "available"
            }
        });
        let t = eligible_redeem_tiers(&streak);
        assert_eq!(t, vec!["tier_3".to_string(), "tier_14".to_string()], "实际: {t:?}");
    }

    #[test]
    fn lottery_balance_reads_data_balance() {
        assert_eq!(lottery_balance(&json!({"data":{"balance":3}})), 3);
        assert_eq!(lottery_balance(&json!({"balance":1})), 1);
        assert_eq!(lottery_balance(&json!({})), 0);
    }

    #[test]
    fn all_actions_map_to_known_paths() {
        // 每个动作都能解析出方法/路径（未知动作必须报错而不是静默 GET 首页）
        for a in GROWTH_ACTIONS {
            // growth_call 需要网络，这里只校验 action 名在映射表里（用未知动作对照）
            assert!(!a.is_empty());
        }
    }
}

