//! wb 族（WorkBuddy/CodeBuddy）每日签到 —— Buddy 加油站每日 100 积分。
//!
//! 一手来源：`_refs/workbuddy2api-master/admin/backend.py:338-368`：
//! - 查状态：`POST {base}/v2/billing/meter/checkin-activity-status` `{}` → data{active/today_checked_in/end_time}
//! - 领取：`POST {base}/v2/billing/meter/daily-checkin` `{}` → code==0 成功 {credit, streak_days}
//! - 域：CN=`https://copilot.tencent.com`；intl（workbuddy.ai）无此活动。
//! - 头：`Authorization: Bearer <token>` + `User-Agent: CLI/2.63.2 CodeBuddy/2.63.2`
//!   （backend.py:827 原样；X-User-Id 等辅助头可省——/v2/plugin/accounts 实测 Bearer 即决定账号归属）。

use reqwest::Client;
use serde_json::{json, Value};

const WB_CLI_UA: &str = "CLI/2.63.2 CodeBuddy/2.63.2";

/// 平台 → 签到域（仅 CN；intl 返回 None = 不支持）。
pub fn wb_checkin_base(platform: &str) -> Option<&'static str> {
    match platform {
        "wb-gateway" | "codebuddy" => Some("https://copilot.tencent.com"),
        _ => None,
    }
}

async fn post_json(
    http: &Client,
    url: String,
    token: &str,
    body: &Value,
) -> Result<Value, String> {
    let resp = http
        .post(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("User-Agent", WB_CLI_UA)
        .header("Authorization", format!("Bearer {token}"))
        .json(body)
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(format!(
            "HTTP {status}：{}",
            v.to_string().chars().take(200).collect::<String>()
        ));
    }
    Ok(v)
}

/// 查询签到活动状态 → {active, today_checked_in, end_time, activity_name, ...}
pub async fn checkin_status(
    http: &Client,
    base: &str,
    token: &str,
) -> Result<Value, String> {
    let v = post_json(
        http,
        format!("{base}/v2/billing/meter/checkin-activity-status"),
        token,
        &json!({}),
    )
    .await?;
    Ok(v.get("data").cloned().unwrap_or_else(|| json!({})))
}

/// 执行每日签到领取 → {ok, credit, streak_days} 或 {ok:false, code, status, msg}
pub async fn claim_daily_checkin(
    http: &Client,
    base: &str,
    token: &str,
) -> Result<Value, String> {
    let v = post_json(
        http,
        format!("{base}/v2/billing/meter/daily-checkin"),
        token,
        &json!({}),
    )
    .await?;
    let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 {
        return Ok(json!({
            "ok": false,
            "code": code,
            "status": map_checkin_status(code),
            "msg": v.get("msg").cloned().unwrap_or(Value::Null),
        }));
    }
    let data = v.get("data").cloned().unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "credit": data.get("credit").or_else(|| v.get("credit")).and_then(|x| x.as_i64()).unwrap_or(0),
        "streak_days": data.get("streak_days").or_else(|| v.get("streak_days")).and_then(|x| x.as_i64()).unwrap_or(0),
    }))
}

/// 业务失败码 → 人话（workbuddy2api 的 _map_checkin_status 语义归纳）。
fn map_checkin_status(code: i64) -> &'static str {
    match code {
        0 => "成功",
        // 常见业务码（已领/未开始/已结束/无资格）按 workbuddy2api 语义归并
        1 => "活动不存在",
        2 => "今日已领取",
        3 => "活动未开始",
        4 => "活动已结束",
        5 => "无参与资格",
        _ => "业务拒绝",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_only_cn_platforms() {
        assert_eq!(wb_checkin_base("wb-gateway"), Some("https://copilot.tencent.com"));
        assert_eq!(wb_checkin_base("codebuddy"), Some("https://copilot.tencent.com"));
        assert_eq!(wb_checkin_base("wb-gateway-intl"), None);
        assert_eq!(wb_checkin_base("cline"), None);
    }

    #[test]
    fn claim_status_mapping() {
        assert_eq!(map_checkin_status(0), "成功");
        assert_eq!(map_checkin_status(2), "今日已领取");
        assert_eq!(map_checkin_status(4), "活动已结束");
    }
}
