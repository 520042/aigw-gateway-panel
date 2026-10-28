//! 商汤小浣熊（Raccoon）签到与积分查询（对齐 magpie-community/plugins packages/raccoon）。
//!
//! ## 端点（web 域，与我们 chat 用的 /wenchain/... 不同域）
//! | 用途 | 方法 | 路径 |
//! |---|---|---|
//! | 每日登录积分发放 | **POST** | `/api/web/desktop/v1/login/points/grant` |
//! | 积分余额 | GET | `/api/web/points/v1/balance` |
//! | 权益信息 | GET | `/api/web/auth/v1/entitlement_info` |
//! | 积分流水 | GET | `/api/web/points/v1/bills?paging.limit=N&paging.offset=0` |
//!
//! ## 关键细节（参考项目实测口径，照抄）
//! 1. **grant 是 POST 而不是 GET** —— 参考项目 `apiGetJson(path, token, headers, "POST")`
//!    第四个参数显式传 "POST"。用 GET 会 405。
//! 2. **必须带桌面客户端头** `X-Client-Platform` + `X-Client-Version: v1.0.0`，
//!    否则服务端按 web 端处理、不给桌面端专属的登录奖励。
//! 3. **幂等**：`granted === true` 才是本次真的发了分；`false` 表示今天已领过，
//!    应视为成功（不算失败），奖励点数在 `popup.points`。

use crate::HttpClient;
use serde_json::{json, Value};

/// web 域基址（与 chat 的 /wenchain 域不同）。
const WEB_BASE: &str = "https://xiaohuanxiong.com";

/// 桌面客户端头（grant 必需）。
fn desktop_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("X-Client-Platform", "desktop"),
        ("X-Client-Version", "v1.0.0"),
    ]
}

/// 带 Bearer 的 GET。
async fn get_json(http: &HttpClient, url: &str, token: &str) -> Result<Value, String> {
    let resp = http
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text)
        .unwrap_or_else(|_| json!({"raw": text.chars().take(300).collect::<String>()}));
    if !(200..300).contains(&status) {
        // 参考项目口径：200001/200003 表示登录态失效
        let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
        if matches!(code, 200001 | 200003) || status == 401 {
            return Err("登录已失效（凭据过期，请重新登录小浣熊）".into());
        }
        return Err(format!("HTTP {status}：{}", text.chars().take(200).collect::<String>()));
    }
    Ok(v.get("data").cloned().unwrap_or(v))
}

/// 查询积分余额 → `{available_points, ...}`。
pub async fn points_balance(http: &HttpClient, token: &str) -> Result<Value, String> {
    get_json(http, &format!("{WEB_BASE}/api/web/points/v1/balance"), token).await
}

/// 每日登录积分发放（**POST** + 桌面头）。
///
/// 返回 `{ok, granted, points, message}`：
/// - `granted=true` → 本次真的发了 `popup.points` 分
/// - `granted=false` → 今天已领过（幂等成功，不算失败）
pub async fn claim_daily_grant(http: &HttpClient, token: &str) -> Value {
    let url = format!("{WEB_BASE}/api/web/desktop/v1/login/points/grant");
    let mut rb = http
        .post(&url)
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json");
    for (k, v) in desktop_headers() {
        rb = rb.header(k, v);
    }
    let resp = match rb.body("{}").send().await {
        Ok(r) => r,
        Err(e) => return json!({"ok": false, "message": format!("网络失败：{e}")}),
    };
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text)
        .unwrap_or_else(|_| json!({"raw": text.chars().take(300).collect::<String>()}));
    if !(200..300).contains(&status) {
        let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
        let msg = if matches!(code, 200001 | 200003) || status == 401 {
            "登录已失效（凭据过期，请重新登录小浣熊）".to_string()
        } else {
            format!("HTTP {status}：{}", text.chars().take(200).collect::<String>())
        };
        return json!({"ok": false, "message": msg});
    }
    let d = v.get("data").cloned().unwrap_or(v);
    let granted = d.get("granted").and_then(|x| x.as_bool()).unwrap_or(false);
    let points = d
        .pointer("/popup/points")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    json!({
        "ok": true,
        "granted": granted,
        "points": if granted { points } else { 0 },
        "message": if granted {
            format!("签到成功 +{points} 积分")
        } else {
            "今天已领取（幂等跳过）".to_string()
        },
    })
}

/// 签到 + 回读余额的组合动作（面板「签到」按钮用它，一次拿到前后状态）。
pub async fn checkin_with_balance(http: &HttpClient, token: &str) -> Value {
    let claim = claim_daily_grant(http, token).await;
    let balance = points_balance(http, token).await.ok();
    let avail = balance
        .as_ref()
        .and_then(|b| b.get("available_points"))
        .and_then(|x| x.as_f64());
    let mut out = claim;
    if let Some(a) = avail {
        out["available_points"] = json!(a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_headers_are_present() {
        let h = desktop_headers();
        let names: Vec<&str> = h.iter().map(|(k, _)| *k).collect();
        assert!(names.contains(&"X-Client-Platform"));
        assert!(names.contains(&"X-Client-Version"));
        // 版本必须是 v1.0.0（参考项目实测值）
        assert_eq!(
            h.iter().find(|(k, _)| *k == "X-Client-Version").map(|(_, v)| *v),
            Some("v1.0.0")
        );
    }

    #[test]
    fn web_base_is_raccoon_domain() {
        assert!(WEB_BASE.contains("xiaohuanxiong.com"));
    }
}
