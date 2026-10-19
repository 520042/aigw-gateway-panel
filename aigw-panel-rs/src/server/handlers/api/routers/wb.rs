use crate::server::state::AppState;
use serde_json::{json, Value};
use super::util::base64url_nopad;

/// wb 族动作执行：签到 / 积分 / 成长任务全套（真实端点，gwextra verified）。
/// 成长任务端点来自 gwextra.ACTIONS["wb-gateway"]（activity/growth/*），
/// 对齐 dsh-our-free-model 的成长任务闭环：accept → claim → streak/redeem/lottery/travel。
/// 注：growth_task_claim 走 Web 域 www.workbuddy.cn（不是 CLI 域），其余走 CLI 域。
pub(crate) async fn wb_action(st: &AppState, pid: &str, action: &str) -> Value {
    let cli_base = if pid == "wb-gateway-intl" {
        "https://www.codebuddy.ai"
    } else {
        "https://copilot.tencent.com"
    };
    let web_base = if pid == "wb-gateway-intl" {
        "https://www.codebuddy.ai"
    } else {
        "https://www.workbuddy.cn"
    };
    let Some(sec) = st.inner.login.secrets_for(pid).first().cloned() else {
        return json!({"ok": false, "message": format!("{} 未登录（先跳转登录/粘贴 token）", pid)});
    };
    // (method, path, base_domain)：cli 或 web
    let (method, path, domain): (&str, &str, &str) = match action {
        // —— 基础 ——
        "checkin" => ("POST", "/v2/billing/meter/daily-checkin", "cli"),
        "points" => ("POST", "/v2/billing/meter/get-user-resource", "cli"),
        "checkin_status" => ("GET", "/v2/billing/meter/checkin-activity-status", "cli"),
        // —— 成长任务 ——
        "growth_tasks" => ("GET", "/activity/growth/tasks", "cli"),
        "growth_task_accept" => ("POST", "/activity/growth/tasks/accept", "cli"),
        "growth_task_claim" => ("POST", "/activity/growth/tasks/claim", "web"),
        // —— 连登 & 抽奖 ——
        "streak_status" => ("GET", "/activity/growth/streak", "cli"),
        "streak_redeem" => ("POST", "/activity/growth/redeem", "cli"),
        "lottery_summary" => ("GET", "/activity/growth/lottery/summary", "cli"),
        "lottery_draw" => ("POST", "/activity/growth/lottery/draw", "cli"),
        // —— 猫猫旅行 ——
        "buddy_info" => ("GET", "/activity/growth/buddy/info", "cli"),
        "buddy_adopt" => ("POST", "/activity/growth/buddy/first", "cli"),
        "travel_status" => ("GET", "/activity/growth/buddy/travel/status", "cli"),
        "travel_depart" => ("POST", "/activity/growth/buddy/travel/depart", "cli"),
        "travel_claim" => ("POST", "/activity/growth/buddy/travel/claim", "cli"),
        _ => return json!({"ok": false, "message": format!("未知 wb 动作: {}", action)}),
    };
    let base = if domain == "web" { web_base } else { cli_base };
    let client = reqwest::Client::new();
    let mut req = if method == "POST" {
        client.post(format!("{}{}", base, path))
    } else {
        client.get(format!("{}{}", base, path))
    };
    req = req
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {}", sec))
        .header("X-No-Department-Info", "true")
        .header("X-No-Authorization", "true")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
        );
    if method == "POST" {
        req = req.header("Content-Type", "application/json").body("{}");
    }
    match req.send().await {
        Ok(r) => {
            let st_code = r.status().as_u16();
            let txt = r.text().await.unwrap_or_default();
            let parsed: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
            tracing::info!(pid, action, status = st_code, body = %txt.chars().take(300).collect::<String>(), "wb action 响应");
            json!({"ok": (200..300).contains(&st_code), "status": st_code, "data": parsed})
        }
        Err(e) => json!({"ok": false, "message": format!("{}", e)}),
    }
}

/// MiniMax OAuth 设备码第一步：申请 device_code + user_code。
/// 返回 (device_code, user_code, verification_uri, code_verifier)。
/// PKCE S256：verifier 随机 43 字节 base62，challenge=SHA256(verifier) base64url。
pub(crate) async fn minimax_device_start() -> Result<(String, String, String, String), String> {
    use sha2::{Digest, Sha256};
    let verifier: String = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // 伪随机 48 字符 base62（够用；verifier 只需唯一性与长度）
        const CH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut v = String::new();
        let mut x = nanos as u64 ^ 0x9E3779B97F4A7C15;
        for _ in 0..48 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            v.push(CH[(x >> 33) as usize % CH.len()] as char);
        }
        v
    };
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64url_nopad(&digest);
    let client = reqwest::Client::new();
    let resp = client
        .post("https://account.minimax.cn/oauth2/device/code")
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
        )
        .body(format!(
            "client_id=mcode-public&scope=agent.default&audience=agent-backend&code_challenge={}&code_challenge_method=S256",
            challenge
        ))
        .send()
        .await
        .map_err(|e| format!("{}", e))?;
    let code = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {} {}", code, v.to_string().chars().take(150).collect::<String>()));
    }
    let dc = v
        .get("device_code")
        .or_else(|| v.pointer("/data/device_code"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let uc = v
        .get("user_code")
        .or_else(|| v.pointer("/data/user_code"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let vu = v
        .get("verification_uri")
        .or_else(|| v.pointer("/data/verification_uri"))
        .or_else(|| v.get("verification_url"))
        .and_then(|x| x.as_str())
        .unwrap_or("https://account.minimax.cn/activate")
        .to_string();
    if dc.is_empty() || uc.is_empty() {
        return Err(format!("响应缺 device_code/user_code: {}", v.to_string().chars().take(150).collect::<String>()));
    }
    Ok((dc, uc, vu, verifier))
}
