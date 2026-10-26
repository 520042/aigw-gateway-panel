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

/// CLI UA（成长任务等模块复用；与签到域保持一致）。
pub fn cli_ua() -> &'static str {
    WB_CLI_UA
}

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
    // ★ 不能 `resp.json().await.unwrap_or(Value::Null)`：签到域在登录态失效 / 被风控 /
    //   网关维护时会回 **HTTP 200 + HTML 登录页**，旧实现把它静默变成 Null 后继续当成功处理
    //   （claim 分支 v.get("code") 取不到 → unwrap_or(0) → 报「✅ 成功 +0 积分」）。
    //   这里改成先取文本再解析：非 JSON 一律当失败，并把开头片段带回给用户。
    let text = resp.text().await.map_err(|e| format!("读取响应失败：{e}"))?;
    if !(200..300).contains(&status) {
        // ★ 实测（2026-10，copilot.tencent.com）：「今天已签到」回的是
        //   HTTP 400 + {"code":10001,"msg":"今天已签到，请明天再来"}，
        //   而不是参考实现假设的 200+code=2。非 2xx 一律 Err("HTTP 400：…") 会把
        //   业务码埋进字符串，上层只能把幂等的「已签过」当「失败」计数 ——
        //   每日自动签到的台账天天报「失败 1」，用户以为凭据坏了。
        //   非 2xx 但响应体是带数字 code 的正规 JSON 对象时放行给业务层解释；
        //   其余（HTML / 风控页 / 网关错误文本）照旧报错并带回片段。
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if v.is_object() && v.get("code").and_then(|x| x.as_i64()).is_some() {
                return Ok(v);
            }
        }
        let head: String = text.chars().take(200).collect();
        return Err(format!("HTTP {status}：{head}"));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "上游返回的不是 JSON（多半是登录页或风控页，凭据可能已失效）：{}",
            text.chars().take(120).collect::<String>()
        )
    })?;
    Ok(v)
}

/// 查询签到活动状态 → {ok, active, today_checked_in, end_time, activity_name}
///
/// ★ 必须带 `ok`：前端 `fmtCheckin` 靠 `d.ok === true/false` 分支渲染，
/// 旧版直接回上游 `data`（只有 active/today_checked_in/end_time），
/// 前端认不出形状就退化成 raw JSON 串，用户看到一堆字段名。
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
    // 上游对「登录态失效」不回非 2xx，而是 200 + {"code":401,"msg":"..."}（无 data 字段）。
    // 旧实现 `v.get("data").unwrap_or({})` 把这种响应当成「活动存在但没签到」，
    // 前端于是渲染「🕘 今日未签」——用户以为还能签，点了领取才报错。
    let Some(data) = v.get("data").filter(|d| d.is_object()) else {
        return Ok(json!({
            "ok": false,
            "error": biz_failure_text(&v),
        }));
    };
    let pick = |k: &str| data.get(k).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "ok": true,
        "active": pick("active"),
        "today_checked_in": pick("today_checked_in"),
        "end_time": pick("end_time"),
        "activity_name": pick("activity_name"),
    }))
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
    // ★ code 缺失绝不能当 0（成功）。参考实现 _refs/workbuddy2api-master/admin/backend.py:342-368
    //   用 `code = data.get("code")` + `if code and code != 0` —— 没有 code 字段就不算成功。
    //   旧实现 `unwrap_or(0)` 让「200 + 风控页/空对象」变成「✅ 成功 +0 积分」的假成功。
    let Some(code) = v.get("code").and_then(|x| x.as_i64()) else {
        return Ok(json!({
            "ok": false,
            "code": Value::Null,
            "status": "上游未返回业务码",
            "msg": biz_failure_text(&v),
        }));
    };
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

/// ★ 2026-10-26（用户排队消息④）：wb 族积分余额查询。
/// `POST {base}/v2/billing/meter/get-user-resource` `{}` →
/// `data.credit`（总积分）/ `data.energy`（能量）/ `data.growth_score`（成长值）。
/// 字段缺失时按 0 计，不判失败——上游不同站点（wb 国内/国际、codebuddy）返回的
/// 资源字段名不完全一致，取到哪个显示哪个。
pub async fn query_balance(http: &Client, base: &str, token: &str) -> Result<Value, String> {
    let v = post_json(
        http,
        format!("{base}/v2/billing/meter/get-user-resource"),
        token,
        &json!({}),
    )
    .await?;
    let data = v.get("data").cloned().unwrap_or_else(|| v.clone());
    let num = |k: &str| -> i64 {
        data.get(k)
            .and_then(|x| x.as_i64())
            .or_else(|| data.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse().ok()))
            .unwrap_or(0)
    };
    Ok(json!({
        "ok": true,
        "credit": num("credit").max(num("total_credit")),
        "energy": num("energy"),
        "growth_score": num("growth_score"),
    }))
}

/// 从「没有业务码」的响应里抠一句人话给用户看（风控页/登录失效页/空对象）。
fn biz_failure_text(v: &Value) -> String {
    if let Some(m) = v.get("msg").and_then(|x| x.as_str()) {
        if !m.trim().is_empty() {
            return m.chars().take(160).collect();
        }
    }
    if let Some(m) = v.get("message").and_then(|x| x.as_str()) {
        if !m.trim().is_empty() {
            return m.chars().take(160).collect();
        }
    }
    let raw = v.to_string();
    if raw.trim().is_empty() || raw == "null" {
        return "上游返回空响应".to_string();
    }
    format!("上游响应缺少业务字段：{}", raw.chars().take(120).collect::<String>())
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
        // 2026-10 起实测：登录失效与「已签过」都走 HTTP 400 + 业务码（见 post_json 注释）。
        401 => "登录已失效",
        10001 => "今日已领取",
        _ => "业务拒绝",
    }
}

// ─────────────────────────────────────────────────────────────
// ★ 2026-10-26（P2）：联网搜索（对齐 workbuddy2api internal/upstream/search.go）
// ─────────────────────────────────────────────────────────────

/// 默认返回条数（官方硬编码值）。
pub const SEARCH_DEFAULT_MAX: i64 = 5;
/// 服务端实际返回上限（实测约 10，超过只是被截断不报错）。
pub const SEARCH_MAX_CAP: i64 = 10;
/// query 最短长度（官方 zod schema 的 query.min(2)）。
pub const SEARCH_MIN_QUERY: usize = 2;

/// 联网搜索：`POST {base}/agenttool/v1/search`。
///
/// 请求体固定 `{query, type:"text2text", max_results}`——`type` 是官方硬编码，
/// 服务端只接受这个小写值。返回上游原始 JSON（`{query, results[], total_results}`）。
pub async fn search(
    http: &Client,
    base: &str,
    token: &str,
    query: &str,
    max_results: i64,
) -> Result<Value, String> {
    let q = query.trim();
    if q.chars().count() < SEARCH_MIN_QUERY {
        return Err(format!("search query must be at least {SEARCH_MIN_QUERY} characters"));
    }
    let n = if max_results <= 0 {
        SEARCH_DEFAULT_MAX
    } else {
        max_results.min(SEARCH_MAX_CAP)
    };
    let resp = http
        .post(format!("{base}/agenttool/v1/search"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json;charset=UTF-8")
        .header("User-Agent", WB_CLI_UA)
        .header("X-Agent-Intent", "craft")
        .header("X-Agent-Runtime", "other")
        .json(&serde_json::json!({
            "query": q,
            "type": "text2text",
            "max_results": n,
        }))
        .send()
        .await
        .map_err(|e| format!("搜索请求失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "搜索返回非 JSON（可能登录态失效）：{}",
            text.chars().take(120).collect::<String>()
        )
    })?;
    if !(200..300).contains(&status) {
        return Err(format!("搜索 HTTP {status}：{}", text.chars().take(160).collect::<String>()));
    }
    // 业务码校验：2xx 但 code != 0 同样算失败
    if let Some(code) = v.get("code").and_then(|x| x.as_i64()) {
        if code != 0 {
            return Err(format!("搜索业务错误 code={code}"));
        }
    }
    // 扁平结构（官方响应就在顶层）或包一层 data 两种形态都兼容
    Ok(v.get("data").cloned().unwrap_or(v))
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

    /// 起一个只回一次固定响应的上游（裸 TCP，避免为两条测试引入 axum 依赖）。
    async fn serve_json(body: &'static str) -> String {
        serve_status(200, body).await
    }

    /// 同 serve_json，但可以指定状态码（回归「HTTP 400 + 业务码」需要）。
    async fn serve_status(status: u16, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = l.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    /// ★ 回归（实测 2026-10）：已签过回 HTTP 400 + code=10001。业务失败必须走 Ok，
    /// 上层才能认出「今日已领取」；当传输错误就把幂等的已签过算成「失败」。
    #[tokio::test]
    async fn already_claimed_400_is_business_not_transport() {
        let base = serve_status(400, r#"{"code":10001,"msg":"今天已签到，请明天再来"}"#).await;
        let v = claim_daily_checkin(&reqwest::Client::new(), &base, "t")
            .await
            .expect("带业务码的 JSON 必须走 Ok，不走传输层 Err");
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["code"], json!(10001));
        assert_eq!(v["status"], json!("今日已领取"));
        assert_eq!(v["msg"], json!("今天已签到，请明天再来"));
    }

    /// 非 JSON 的 4xx（网关维护页）仍按传输错误报，带状态码与片段。
    #[tokio::test]
    async fn non_json_400_still_transport_error() {
        let base = serve_status(400, "<html>maintenance</html>").await;
        let e = claim_daily_checkin(&reqwest::Client::new(), &base, "t")
            .await
            .expect_err("非 JSON 的 4xx 必须仍报错");
        assert!(e.contains("HTTP 400"), "{e}");
        assert!(e.contains("maintenance"), "{e}");
    }

    /// ★ 回归：签到域在登录态失效时会回 **HTTP 200 + HTML 登录页**。
    /// 旧实现 `resp.json().await.unwrap_or(Value::Null)` 把它静默变成 Null，
    /// 然后 claim 分支用 `code.unwrap_or(0)` 判成功 → 面板显示「✅ 成功 +0 积分」。
    #[tokio::test]
    async fn non_json_body_is_an_error_not_success() {
        let base = serve_json("<html><body>请重新登录</body></html>").await;
        let r = claim_daily_checkin(&reqwest::Client::new(), &base, "t").await;
        let e = r.expect_err("HTML 响应必须报错，不能当成功");
        assert!(e.contains("不是 JSON"), "错误信息应说明不是 JSON：{e}");
        assert!(e.contains("请重新登录"), "应带回上游片段便于判断：{e}");
    }

    /// ★ 回归：上游 200 但没有 code 字段（风控页 / 空对象）不能被当成「成功 +0 积分」。
    /// 参考实现 workbuddy2api 用 `if code and code != 0` —— code 缺失就不算成功。
    #[tokio::test]
    async fn missing_code_is_not_success() {
        let base = serve_json("{}").await;
        let v = claim_daily_checkin(&reqwest::Client::new(), &base, "t")
            .await
            .expect("HTTP 层成功");
        assert_eq!(v["ok"], json!(false), "缺 code 必须判失败：{v}");
        assert_eq!(v["status"], json!("上游未返回业务码"));
    }

    /// ★ 回归：查状态时上游 200 但没有 data（登录失效），不能被渲染成「今日未签」。
    #[tokio::test]
    async fn status_without_data_is_failure() {
        let base = serve_json(r#"{"code":401,"msg":"登录已失效"}"#).await;
        let v = checkin_status(&reqwest::Client::new(), &base, "t")
            .await
            .expect("HTTP 层成功");
        assert_eq!(v["ok"], json!(false), "缺 data 必须判失败：{v}");
        assert_eq!(v["error"], json!("登录已失效"));
    }

    /// 正常响应仍要照旧解析成功（防止上面的加固把正常路径也拦了）。
    #[tokio::test]
    async fn happy_path_still_parses() {
        let base = serve_json(r#"{"code":0,"data":{"credit":100,"streak_days":3}}"#).await;
        let v = claim_daily_checkin(&reqwest::Client::new(), &base, "t")
            .await
            .expect("HTTP 层成功");
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["credit"], json!(100));
        assert_eq!(v["streak_days"], json!(3));
    }

    #[test]
    fn claim_status_mapping() {
        assert_eq!(map_checkin_status(0), "成功");
        assert_eq!(map_checkin_status(2), "今日已领取");
        assert_eq!(map_checkin_status(4), "活动已结束");
        assert_eq!(map_checkin_status(401), "登录已失效");
        assert_eq!(map_checkin_status(10001), "今日已领取");
    }
}
