//! 签到执行：把 Python autocheckin.run_platform 的四种模式移植到 Rust。
//!
//! 模式映射（与 Python 一致）：
//!   flow   → Trae 先查状态未签再领（必须判业务码 code≠0）
//!   bonus  → 小浣熊 login_bonus 单接口
//!   kuku   → 两段式：userreport 取 bdstoken/uinfo/uk → taskComplete(form)
//!   wps    → 先看 tasks 未签则领（已签 400 v7code:400000007 幂等成功）
//!   direct → wb-gateway 每日签到单接口
//!
//! 每轮结果写 usage_events（platform=pid, upstream="checkin:<action>"），
//! 前端任务中心 /api/checkins 直接读得到。

use crate::server::state::AppState;
use serde_json::{json, Value};

/// 通用 HTTP 请求（对齐 Python gwextra._request：Bearer/Cookie 自动分流、
/// 静态 query + 动态 query + params 合并、form/JSON body）。
async fn request(
    base: &str,
    path: &str,
    method: &str,
    secret: &str,
    query: &[(String, String)],
    form: Option<&[(String, String)]>,
    json_body: Option<Value>,
    timeout_secs: u64,
) -> Result<(u16, Value), (i64, String)> {
    let client = reqwest::Client::new();
    let mut url = format!("{}{}", base.trim_end_matches('/'), path);
    if !query.is_empty() {
        let qs: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding_lite(k), urlencoding_lite(v)))
            .collect();
        url = format!("{}?{}", url, qs.join("&"));
    }
    let mut req = match method {
        "POST" => client.post(&url),
        _ => client.get(&url),
    };
    req = req
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
        .header("Accept", "application/json");

    if !secret.is_empty() {
        let is_json_token = secret.trim_start().starts_with('{') || !secret[..secret.len().min(40)].contains('=');
        if is_json_token {
            req = req.header("Authorization", format!("Bearer {}", secret.trim()));
        } else {
            req = req.header("Cookie", secret);
        }
    }

    if method == "POST" {
        if let Some(fb) = form {
            req = req
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(
                    fb.iter()
                        .map(|(k, v)| format!("{}={}", urlencoding_lite(k), urlencoding_lite(v)))
                        .collect::<Vec<_>>()
                        .join("&"),
                );
        } else if let Some(jb) = json_body {
            req = req
                .header("Content-Type", "application/json")
                .body(jb.to_string());
        }
    }

    match req.send().await {
        Ok(r) => {
            let code = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            let parsed: Value = serde_json::from_str(&txt)
                .unwrap_or(json!({"status": code, "raw": txt.chars().take(2000).collect::<String>()}));
            Ok((code, parsed))
        }
        Err(e) => Err((-1, format!("{}", e))),
    }
}

/// Trae flow：先 GET status，未签则 POST claim。必须判业务码 code（无凭据也返 200）。
async fn trae_checkin(secret: &str) -> (bool, String, bool) {
    let base = "https://api.trae.cn";
    let (code, d) = match request(
        base,
        "/trae/api/v2/ug/checkin_credits/status",
        "GET",
        secret,
        &[],
        None,
        None,
        20,
    )
    .await
    {
        Ok(v) => v,
        Err((_, e)) => return (false, format!("Trae status 请求失败: {}", e), false),
    };
    if code >= 400 {
        return (false, format!("Trae status HTTP {}", code), false);
    }
    let body = d.get("data").and_then(|v| v.as_object()).cloned().map(Value::Object).unwrap_or(d.clone());
    let biz_code = body.get("code").and_then(|v| v.as_i64());
    if let Some(c) = biz_code {
        if c != 0 {
            let msg = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
            return (false, format!("Trae 接口拒绝: code={} {}", c, msg), false);
        }
    }
    if body.get("checked_in").and_then(|v| v.as_bool()).unwrap_or(false)
        || body.get("checked_in_today").and_then(|v| v.as_bool()).unwrap_or(false)
    {
        return (true, "今天已签到".to_string(), false);
    }
    let (code2, d2) = match request(
        base,
        "/trae/api/v2/ug/checkin_credits/claim",
        "POST",
        secret,
        &[],
        None,
        Some(json!({})),
        20,
    )
    .await
    {
        Ok(v) => v,
        Err((_, e)) => return (false, format!("Trae claim 请求失败: {}", e), false),
    };
    if code2 >= 400 {
        return (false, format!("Trae claim HTTP {}", code2), false);
    }
    let b2 = d2.get("data").and_then(|v| v.as_object()).cloned().map(Value::Object).unwrap_or(d2.clone());
    let c2 = b2.get("code").and_then(|v| v.as_i64());
    if let Some(c) = c2 {
        if c != 0 {
            let msg = b2.get("message").and_then(|v| v.as_str()).unwrap_or("");
            return (false, format!("Trae 签到未生效: code={} {}", c, msg), false);
        }
    }
    (true, "Trae 签到成功".to_string(), false)
}

/// 小浣熊 bonus：单接口 login_bonus。
async fn raccoon_bonus(secret: &str) -> (bool, String, bool) {
    let base = "https://xiaohuanxiong.com";
    let (code, _d) = match request(
        base,
        "/api/web/desktop/v1/login/points/grant",
        "POST",
        secret,
        &[],
        None,
        Some(json!({})),
        20,
    )
    .await
    {
        Ok(v) => v,
        Err((_, e)) => return (false, format!("小浣熊请求失败: {}", e), false),
    };
    if code >= 400 {
        return (false, format!("小浣熊 HTTP {}", code), false);
    }
    (true, "小浣熊登录送积分已领取".to_string(), false)
}

/// 库库两段式：先 userreport 取 bdstoken/uinfo/uk，再 taskComplete(form task_type=LOGIN)。
async fn kuku_checkin(secret: &str) -> (bool, String, bool) {
    let base = "https://kuku.baidu.com";
    let fixed_q: Vec<(String, String)> = vec![
        ("clienttype".into(), "400".into()),
        ("app_id".into(), "123971023".into()),
        ("web".into(), "1".into()),
        ("channel".into(), "chunlei".into()),
        ("version".into(), "1.6.8".into()),
    ];
    let (code, d) = match request(
        base,
        "/api/genflowpro/common/userreport",
        "GET",
        secret,
        &fixed_q,
        None,
        None,
        20,
    )
    .await
    {
        Ok(v) => v,
        Err((_, e)) => return (false, format!("库库 userreport 失败: {}", e), false),
    };
    if code >= 400 {
        return (false, format!("库库 userreport HTTP {}", code), false);
    }
    let sp = d.get("data").and_then(|v| v.get("data")).cloned().unwrap_or(Value::Null);
    let bdstoken = sp.get("bdstoken").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let uinfo = sp.get("uinfo").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let uk = sp.get("uk").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if bdstoken.is_empty() || uinfo.is_empty() || uk.is_empty() {
        return (false, "库库 userreport 未返回完整会话参数".to_string(), false);
    }
    let mut q = fixed_q.clone();
    q.push(("bdstoken".into(), bdstoken));
    q.push(("uinfo".into(), uinfo));
    q.push(("uk".into(), uk));
    let form = vec![("task_type".to_string(), "LOGIN".to_string())];
    let (code2, d2) = match request(
        base,
        "/api/genflowpro/freepoint/taskComplete",
        "POST",
        secret,
        &q,
        Some(&form),
        None,
        20,
    )
    .await
    {
        Ok(v) => v,
        Err((_, e)) => return (false, format!("库库 taskComplete 失败: {}", e), false),
    };
    if code2 >= 400 {
        return (false, format!("库库 taskComplete HTTP {} (参数可能过期)", code2), false);
    }
    let data = d2.get("data").cloned().unwrap_or(Value::Null);
    let status = data.get("complete_status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let reward = data.get("reward_point");
    if status == "SUCCESS" {
        let r = reward.and_then(|v| v.as_i64()).map(|n| format!("+{}积分", n)).unwrap_or_default();
        return (true, format!("库库签到成功 {}", r), false);
    }
    (true, format!("库库签到响应: {}", status), false)
}

/// WPS 幂等签到：先 GET tasks，未签则 POST complete；已签 400 v7code:400000007 视为成功。
async fn wps_checkin(secret: &str) -> (bool, String, bool) {
    let base = "https://lingxi.kdocs.cn";
    let (code, d) = match request(
        base,
        "/api/public/v1/tasks",
        "GET",
        secret,
        &[],
        None,
        None,
        20,
    )
    .await
    {
        Ok(v) => v,
        Err((_, e)) => return (false, format!("WPS tasks 请求失败: {}", e), false),
    };
    if code >= 400 {
        return (false, format!("WPS tasks HTTP {}", code), false);
    }
    let data = d.get("data").cloned().unwrap_or(Value::Null);
    let tasks = data.get("tasks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let dc = tasks.iter().find(|t| {
        t.get("task_key").and_then(|v| v.as_str()) == Some("daily_check_in")
            || t.get("type").and_then(|v| v.as_str()) == Some("daily_check_in")
    });
    if let Some(t) = dc {
        if t.get("status").and_then(|v| v.as_str()) == Some("claimed") {
            return (true, "WPS 今天已签到".to_string(), false);
        }
    }
    let (code2, d2) = match request(
        base,
        "/api/public/v1/tasks/daily_check_in/complete",
        "POST",
        secret,
        &[],
        None,
        Some(json!({})),
        20,
    )
    .await
    {
        Ok(v) => v,
        Err((_, e)) => return (false, format!("WPS complete 请求失败: {}", e), false),
    };
    if code2 < 400 {
        return (true, "WPS 签到成功".to_string(), false);
    }
    let s = d2.to_string();
    if s.contains("400000007") {
        return (true, "WPS 今天已签到（幂等）".to_string(), false);
    }
    (false, format!("WPS complete HTTP {}", code2), false)
}

/// wb-gateway direct：POST /v2/billing/meter/daily-checkin。
async fn wb_checkin(secret: &str, pid: &str) -> (bool, String, bool) {
    let base = if pid == "wb-gateway-intl" {
        "https://www.codebuddy.ai"
    } else {
        "https://copilot.tencent.com"
    };
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v2/billing/meter/daily-checkin", base))
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {}", secret))
        .header("Content-Type", "application/json")
        .header("X-No-Department-Info", "true")
        .header("X-No-Authorization", "true")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
        .body("{}")
        .send()
        .await;
    match resp {
        Ok(r) => {
            let code = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            if code < 400 {
                (true, format!("wb 签到成功 (HTTP {})", code), false)
            } else {
                (false, format!("wb 签到 HTTP {}: {}", code, txt.chars().take(200).collect::<String>()), false)
            }
        }
        Err(e) => (false, format!("wb 签到请求失败: {}", e), false),
    }
}

/// 执行单平台签到（对齐 Python run_platform）。
/// 返回 (ok, message, skipped)。
pub async fn run_platform(st: &AppState, pid: &str) -> (bool, String, bool) {
    let secrets = st.inner.login.secrets_for(pid);
    if secrets.is_empty() {
        return (false, format!("未登录（账号池里没有 {} 的凭据）", pid), true);
    }
    let secret = &secrets[0];
    match pid {
        "apk-trae" => trae_checkin(secret).await,
        "apk-raccoon" => raccoon_bonus(secret).await,
        "apk-kuku" => kuku_checkin(secret).await,
        "apk-wps" => wps_checkin(secret).await,
        "wb-gateway" | "wb-gateway-intl" => wb_checkin(secret, pid).await,
        _ => (false, format!("平台 {} 没有签到执行器", pid), true),
    }
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
