//! 网易 Lobster AI（有道龙虾）每日签到（Buddy 加油站同款价值：+100 积分/号/天）。
//!
//! 协议一手来源：xinxinshuhao-create/lobsterai2api internal/upstream/client.go
//! `DailyCheckin`（2026-10 实测，三段幂等设计）。
//!
//! 流程：GET slot → GET context → POST actions/check_in → 复核 claimedToday。
//! 关键约束：
//! - **clientVersion 必须真实**——slot 接口按版本号判断活动可见性，
//!   旧版本号（如 0.1.0）会拿到 slotState=empty。真实版本从有道更新 API
//!   拉（1 小时缓存），拉不到回落已知可用版本。
//! - 幂等：已签到 / 活动未投放都算成功（不算失败），不会重复领取。
//! - 复核：check_in 后再拉 context，claimedToday 真的翻转才算成功——
//!   上游可能 200 但静默不发奖，不复核会把「假成功」记进台账。

use reqwest::Client;
use serde_json::{json, Value};

const SERVER_BASE: &str = "https://lobsterai-server.youdao.com";
const UPDATE_API: &str =
    "https://api-overmind.youdao.com/openapi/get/luna/hardware/lobsterai/prod/update";
const FALLBACK_VERSION: &str = "2026.9.4";
const PLACEMENT: &str = "desktop_sidebar";

/// 客户端版本缓存（1 小时），与参考实现一致。
static VERSION_CACHE: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// 取真实客户端版本（更新 API → 回落值，1 小时缓存）。
async fn fetch_client_version(http: &Client) -> String {
    {
        let g = VERSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((v, at)) = g.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(3600) {
                return v.clone();
            }
        }
    }
    let mut v = FALLBACK_VERSION.to_string();
    if let Ok(r) = http
        .get(UPDATE_API)
        .timeout(std::time::Duration::from_secs(10))
        .header("Accept", "application/json")
        .send()
        .await
    {
        if let Ok(j) = r.json::<Value>().await {
            if let Some(ver) = j.pointer("/data/value/version").and_then(|x| x.as_str()) {
                if !ver.is_empty() {
                    v = ver.to_string();
                }
            }
        }
    }
    let mut g = VERSION_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some((v.clone(), std::time::Instant::now()));
    v
}

/// 签到请求公共头（Bearer + LobsterAI/<真实版本>）。
fn checkin_headers(rb: reqwest::RequestBuilder, token: &str, version: &str) -> reqwest::RequestBuilder {
    rb.header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", format!("LobsterAI/{version}"))
}

/// 幂等键（uuid v4 形态；不引 uuid crate，用系统熵拼）。
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut x = n as u64 ^ 0x9E3779B97F4A7C15;
    for slot in b.iter_mut() {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut mix = x >> 33;
        mix ^= std::process::id() as u64;
        *slot = (mix & 0xff) as u8;
    }
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// 每日签到 → {ok, claimed, credit, message}。
///
/// ok 语义：签到成功 / 今天已签 / 活动未投放 都算 ok=true（幂等，不算失败）；
/// 只有网络失败 / 上游拒绝 / 复核未翻转才 ok=false。
pub async fn daily_checkin(http: &Client, token: &str) -> Value {
    let version = fetch_client_version(http).await;
    let headers = |rb: reqwest::RequestBuilder| checkin_headers(rb, token, &version);

    // ① 查活动槽：服务端按 clientVersion 决定活动可见性
    let slot_url = format!(
        "{SERVER_BASE}/api/client-activities/slot?placement={PLACEMENT}&clientVersion={version}&containerApiVersion=2&platform=win32"
    );
    let Ok(r) = headers(http.get(&slot_url)).timeout(std::time::Duration::from_secs(15)).send().await else {
        return json!({"ok": false, "message": "slot 请求失败（网络）"});
    };
    let status = r.status().as_u16();
    let Ok(slot) = r.json::<Value>().await else {
        return json!({"ok": false, "message": format!("slot 响应非 JSON（HTTP {status}）")});
    };
    if !(200..300).contains(&status) {
        return json!({"ok": false, "message": format!("slot HTTP {status}")});
    }
    let slot_state = slot.get("slotState").and_then(|x| x.as_str()).unwrap_or("");
    let Some(activity) = slot.get("activity") else {
        return json!({"ok": true, "claimed": false, "credit": 0, "message": format!("当前无签到活动（slotState={slot_state}）")});
    };
    let Some(code) = activity.get("activityCode").and_then(|x| x.as_str()) else {
        return json!({"ok": true, "claimed": false, "credit": 0, "message": "活动无 code（未投放）"});
    };
    let rev = activity.get("configRevision").and_then(|x| x.as_i64()).unwrap_or(0);

    // ② 查活动状态：已签到直接跳过（幂等）
    let ctx_url = format!(
        "{SERVER_BASE}/api/client-activities/{}/context?configRevision={rev}",
        url_lite_escape(code)
    );
    let read_ctx = || async {
        let r = headers(http.get(&ctx_url))
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .ok()?;
        r.json::<Value>().await.ok()
    };
    let Some(ctx) = read_ctx().await else {
        return json!({"ok": false, "message": "context 请求失败（网络）"});
    };
    let claimed_today = ctx.pointer("/state/claimedToday").and_then(|x| x.as_bool()).unwrap_or(false);
    let actions: Vec<String> = ctx
        .get("actions")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if claimed_today || !actions.iter().any(|a| a == "check_in") {
        return json!({"ok": true, "claimed": false, "credit": 0,
                      "message": if claimed_today { "今天已签到（幂等跳过）" } else { "活动状态不含 check_in 动作" }});
    }

    // ③ 提交签到（幂等键防止重复发放）
    let action_url = format!(
        "{SERVER_BASE}/api/client-activities/{}/actions/check_in",
        url_lite_escape(code)
    );
    let body = json!({"configRevision": rev, "idempotencyKey": uuid_v4(), "payload": {}});
    let Ok(r) = headers(http.post(&action_url))
        .timeout(std::time::Duration::from_secs(15))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await else {
        return json!({"ok": false, "message": "check_in 请求失败（网络）"});
    };
    let status = r.status().as_u16();
    let Ok(res) = r.json::<Value>().await else {
        return json!({"ok": false, "message": format!("check_in 响应非 JSON（HTTP {status}）")});
    };
    let reward = res.pointer("/result/rewardCredits").and_then(|x| x.as_i64()).unwrap_or(0);

    // ④ 复核：claimedToday 真的翻转才算成功（上游可能 200 但静默不发奖）
    let claimed_ok = read_ctx()
        .await
        .and_then(|after| after.pointer("/state/claimedToday").and_then(|x| x.as_bool()).map(|b| b))
        .unwrap_or(false);
    if !claimed_ok {
        return json!({"ok": false, "message": "check_in 已提交但 claimedToday 未翻转（假成功，未记成功）"});
    }
    json!({"ok": true, "claimed": true, "credit": reward,
           "message": format!("签到成功 +{reward} 积分（活动 {code}）")})
}

/// 路径段的最小转义（activityCode 是服务端词表值，通常安全；这里只挡 `/`）。
fn url_lite_escape(s: &str) -> String {
    s.replace('/', "%2F")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v4_shape() {
        let u = uuid_v4();
        // 8-4-4-4-12，第三段以 4 开头（version 4），第四段以 8/9/a/b 开头（variant）
        assert_eq!(u.len(), 36);
        assert_eq!(u.chars().filter(|c| *c == '-').count(), 4);
        let segs: Vec<&str> = u.split('-').collect();
        assert_eq!(segs.len(), 5);
        assert!(segs[2].starts_with('4'));
        assert!("89ab".contains(&segs[3][..1].to_ascii_lowercase()[..1]));
    }

    #[test]
    fn url_lite_escape_blocks_slash() {
        assert_eq!(url_lite_escape("a/b"), "a%2Fb");
        assert_eq!(url_lite_escape("plain"), "plain");
    }
}
