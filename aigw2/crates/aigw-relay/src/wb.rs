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
/// wb 族的**站点前台**地址（成长任务等走 /api/… 的 web 接口用）。
///
/// ★ 单一来源是 spec.domain（wb-gateway=www.workbuddy.cn / wb-gateway-intl=www.workbuddy.ai）。
///   历史上这里写死过 `if platform == "codebuddy" { .cn } else { workbuddy.cn }` ——
///   于是国际版账号被导向国内站，请求必然失败。
pub fn web_base_for(spec: &aigw_registry::ProviderSpec) -> Option<String> {
    if spec.signer != "wb" {
        return None;
    }
    if spec.domain.is_empty() {
        return None;
    }
    Some(format!("https://{}", spec.domain))
}
pub fn wb_checkin_base(platform: &str) -> Option<&'static str> {
    match platform {
        // ★ 国内站与国际站**共用同一个端点**，靠 X-Domain 头区分站点
        //   （2026-10-28 实测：intl 带 X-Domain 请求返回 401「拒绝凭据」而不是 404
        //   「端点不存在」，说明端点在同一域名下可用）。早期版本把 intl 排除在外，
        //   理由是「计费端点未实测」——现在有了 X-Domain 且端点已探明，纳入。
        "wb-gateway" | "codebuddy" | "wb-gateway-intl" => Some("https://copilot.tencent.com"),
        _ => None,
    }
}

async fn post_json(
    http: &Client,
    url: String,
    token: &str,
    body: &Value,
) -> Result<Value, String> {
    post_json_domain(http, url, token, "", body).await
}

/// 同 `post_json`，但带 `X-Domain`（wb 族区分国内站/国际站的依据）。
async fn post_json_domain(
    http: &Client,
    url: String,
    token: &str,
    domain: &str,
    body: &Value,
) -> Result<Value, String> {
    let resp = http
        .post(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("User-Agent", WB_CLI_UA)
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Domain", if domain.is_empty() { "" } else { domain })
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
/// wb 族验活：用**积分查询端点**（get-user-resource）判凭据死活。
///
/// ★ 为什么不能用通用 verify_openai（GET /models）：copilot.tencent.com 没有
///   那个端点，活凭据会被判成「鉴权失效」——用户看到「检查未通过」去重新登录，
///   重登后依然不过（端点不存在，与凭据无关）。实测事故：本轮用户账号
///   积分能查（5961）但「检查」报失效，就是这个假阴性。
///
/// 为什么选这个端点而不是对话链路：它是**已验证可用的轻量鉴权探测**（同一个
///   post_json 就是积分查询本身，用户账号上用通过），不发对话、不烧额度、
///   不带对话头族——验活语义只需要回答「这个 Bearer 还认不认」。
///
/// 判定：HTTP 2xx 且业务码为 0/缺失 → 活；401/403 → 死；网络失败 → 保守判死
///   （面板显示「未通过」会诱导用户重登，而重登对网络故障没用，所以网络错误
///   也按失败报——但 message 里会带上原因，不会让人误以为是凭据问题）。
// ── 登录用户信息（best-effort）───────────────────────────────

/// 在一棵 JSON 子树里按候选键找字符串值（忽略大小写与下划线差异）。
///
/// ★ 为什么用候选键而不是固定结构：`/v2/plugin/accounts` 的响应结构没有公开
///   协议，上游随时可能改字段名。**解析不出就不显示**（返回空对象），比硬编码
///   一个字段名然后整块显示空白更诚实——用户至少不会看到一个假的「—」。
fn pick_str(node: &Value, keys: &[&str], depth: u32) -> Option<String> {
    if depth > 5 {
        return None;
    }
    let norm = |s: &str| s.to_lowercase().replace(['_', '-'], "");
    let want: Vec<String> = keys.iter().map(|k| norm(k)).collect();
    match node {
        Value::Object(m) => {
            // 先看本层键名命中
            for (k, v) in m {
                let nk = norm(k);
                if want.iter().any(|w| *w == nk) {
                    if let Some(s) = v.as_str() {
                        let s = s.trim();
                        if !s.is_empty() {
                            return Some(s.to_string());
                        }
                    }
                }
            }
            for v in m.values() {
                if let Some(hit) = pick_str(v, keys, depth + 1) {
                    return Some(hit);
                }
            }
            None
        }
        Value::Array(a) => a.iter().find_map(|v| pick_str(v, keys, depth + 1)),
        _ => None,
    }
}

/// 拉登录用户信息：`GET /v2/plugin/accounts`（登录态端点，实测无凭据回 401）。
///
/// 返回 {name,email,plan,avatar}（只含解析出来的字段；都没有则空对象）。
/// 失败返回空对象——**信息展示是锦上添花，绝不能因为它挡住登录流程**。
pub async fn fetch_profile(http: &Client, base: &str, secret: &str) -> Value {
    let resp = http
        .get(format!("{base}/v2/plugin/accounts"))
        .timeout(std::time::Duration::from_secs(12))
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {secret}"))
        .header("User-Agent", WB_CLI_UA)
        .send()
        .await;
    let Ok(resp) = resp else { return json!({}) };
    if !resp.status().is_success() {
        return json!({});
    }
    let Ok(v) = resp.json::<Value>().await else { return json!({}) };
    // 候选子树优先（避免把模型名/套餐码当成人名），再退整树高置信键
    let sub = v
        .get("data")
        .or_else(|| v.get("account"))
        .or_else(|| v.get("user"))
        .unwrap_or(&v);
    let name = pick_str(sub, &["nickName", "nickname", "userName", "user_name", "displayName", "display_name", "name"], 0)
        .or_else(|| pick_str(&v, &["nickName", "nickname", "userName", "displayName"], 0));
    let email = pick_str(sub, &["email", "mail", "userEmail", "user_email"], 0)
        .or_else(|| pick_str(&v, &["email", "userEmail"], 0));
    let plan = pick_str(sub, &["planName", "plan_name", "packageName", "package_name", "plan", "package"], 0);
    let avatar = pick_str(sub, &["avatarUrl", "avatar_url", "avatar"], 0);
    let mut out = serde_json::Map::new();
    if let Some(x) = name { out.insert("name".into(), json!(x)); }
    if let Some(x) = email { out.insert("email".into(), json!(x)); }
    if let Some(x) = plan { out.insert("plan".into(), json!(x)); }
    if let Some(x) = avatar { out.insert("avatar".into(), json!(x)); }
    Value::Object(out)
}
pub async fn verify(
    _http: &reqwest::Client,
    spec: &aigw_registry::ProviderSpec,
    secret: &str,
) -> crate::VerifyOutcome {
    use crate::VerifyOutcome;
    let base = wb_checkin_base(&spec.id).unwrap_or("https://copilot.tencent.com");
    let http = crate::auth_login::auth_http();
    // ★ 带 X-Domain：同一个端点上国内站与国际站靠它区分，缺了它国际版账号
    //   会被判成「未授权」（与对话链路的判定一致）。
    match post_json_domain(
        &http,
        format!("{base}/v2/billing/meter/get-user-resource"),
        secret,
        &spec.domain,
        &json!({}),
    )
    .await
    {
        Ok(v) => {
            // 业务码：0/缺失算活；非 0（如 40100 未登录）算死
            match v.get("code").and_then(|c| c.as_i64()) {
                None | Some(0) => VerifyOutcome::Alive,
                Some(c) => VerifyOutcome::Dead(format!(
                    "业务码 {c}：{}",
                    v.get("msg")
                        .or_else(|| v.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("上游拒绝该凭据")
                )),
            }
        }
        // ★ 错误串分两类（post_json 对非 2xx 返回 "HTTP {status}：{body}"）：
        //   · HTTP 401/403 → **明确失效**（上游拒绝了这把凭据）；
        //   · 其余（网络失败/超时/网关 HTML 页）→ **无法判定**，不写号池健康。
        //   把 401 当网络失败会让真失效的账号显示「无法验活」，用户不知道要不要重登；
        //   反过来把网络失败当失效更糟（白重登）。两类必须分开。
        Err(e) => {
            if e.starts_with("HTTP 401") || e.starts_with("HTTP 403") {
                // 只留状态码：body 常是网关的 HTML 页，塞进用户提示里既丑又没信息量。
                let code: String = e.split('：').next().unwrap_or("HTTP 4xx").to_string();
                VerifyOutcome::Dead(format!("{code}（上游拒绝了这把凭据）"))
            } else {
                VerifyOutcome::Unknown(format!("网络请求失败（{e}）"))
            }
        }
    }
}
pub async fn query_balance(
    http: &Client,
    base: &str,
    token: &str,
    domain: &str,
) -> Result<Value, String> {
    let v = post_json_domain(
        http,
        format!("{base}/v2/billing/meter/get-user-resource"),
        token,
        domain,
        &json!({}),
    )
    .await?;
    let data = v.get("data").cloned().unwrap_or_else(|| v.clone());
    // ★ 多资源包优先（总剩余 + 最早到期 + 逐包明细），退回扁平 credit
    let (credit, expire_at, packs) = parse_user_resource(&data);
    let num = |k: &str| -> i64 {
        data.get(k)
            .and_then(|x| x.as_i64())
            .or_else(|| data.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse().ok()))
            .unwrap_or(0)
    };
    Ok(json!({
        "ok": true,
        "credit": credit.unwrap_or(0.0),
        "energy": num("energy"),
        "growth_score": num("growth_score"),
        "expire_at": expire_at,
        "packs": packs,
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


    /// 站点前台地址必须来自 spec.domain：国际版账号被导到国内站会直接失败。
    #[test]
    fn web_base_follows_spec_domain() {
        let mk = |signer: &str, domain: &str| {
            serde_json::from_value::<aigw_registry::ProviderSpec>(serde_json::json!({
                "id": "wb", "name": "wb", "enabled": true, "auth": "o_auth_device",
                "base_url": "https://copilot.tencent.com", "format": "openai",
                "models_policy": "static", "stream_dialect": "sse_delta",
                "signer": signer, "domain": domain
            }))
            .expect("spec")
        };
        assert_eq!(
            web_base_for(&mk("wb", "www.workbuddy.cn")).as_deref(),
            Some("https://www.workbuddy.cn")
        );
        assert_eq!(
            web_base_for(&mk("wb", "www.workbuddy.ai")).as_deref(),
            Some("https://www.workbuddy.ai"),
            "国际版必须指向 .ai 站 —— 写死国内域会让成长任务全失败"
        );
        // 非 wb 族 / 没配 domain：给 None，调用方跳过（不猜一个域去打）
        assert_eq!(web_base_for(&mk("loomy", "www.workbuddy.cn")), None);
        assert_eq!(web_base_for(&mk("wb", "")), None);
    }

    /// wb 三站共用同一个端点（靠 X-Domain 区分站点）；非 wb 族一律 None。
    ///
    /// ★ 2026-10-28 起 intl 从 None 变成 Some：带 X-Domain 请求实测返回 401
    ///   （「拒绝凭据」而非「端点不存在」），说明端点在同一域名下可用；
    ///   早先把它排除是「计费端点未实测」的保守，现在依据更足了。
    #[test]
    fn base_covers_all_wb_editions_only() {
        for p in ["wb-gateway", "codebuddy", "wb-gateway-intl"] {
            assert_eq!(
                wb_checkin_base(p),
                Some("https://copilot.tencent.com"),
                "{p} 应认得"
            );
        }
        assert_eq!(wb_checkin_base("cline"), None);
        assert_eq!(wb_checkin_base("api-openrouter"), None);
        assert_eq!(wb_checkin_base(""), None);
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

// ── 积分余额 / 到期解析 ────────────────────────────────────────

/// 把上游的日期时间串折成 epoch 秒。支持四种实测/常见形态：
///   "2026-10-27T15:00:00Z" / "2026-10-27T15:00:00+08:00" /
///   "2026-10-27 15:00:00"（按北京时间 +8 折算，WB 的中文接口一贯如此）/ 纯数字（秒或毫秒）。
/// 解析不出返回 None —— 到期字段缺失时调用方按「不知道到期」处理，不猜。
pub fn parse_time_epoch(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // 纯数字：秒或毫秒
    if let Ok(n) = s.parse::<i64>() {
        return Some(if n > 10_000_000_000 { n / 1000 } else { n });
    }
    let (date, rest) = s.split_once(['T', ' '])?;
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 {
        return None;
    }
    let y: i64 = dp[0].parse().ok()?;
    let mo: i64 = dp[1].parse().ok()?;
    let d: i64 = dp[2].parse().ok()?;
    let tp = rest.split(['+', 'Z']).next()?;
    let tp = tp.trim();
    let tp = if tp.len() > 8 { &tp[..8] } else { tp };
    let hhmmss: Vec<&str> = tp.split(':').collect();
    let h: i64 = hhmmss.first().and_then(|x| x.parse().ok()).unwrap_or(0);
    let mi: i64 = hhmmss.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    let se: i64 = hhmmss.get(2).and_then(|x| x.parse().ok()).unwrap_or(0);
    // 无时区按北京时间（WB 中文接口的服务端口径）；Z/±offset 的时区差已由上面截断处理
    let offset: i64 = if s.contains('Z') { 0 } else if let Some(p) = s.split_once('+') {
        let tz: Vec<&str> = p.1.split(':').collect();
        let th: i64 = tz.first().and_then(|x| x.parse().ok()).unwrap_or(0);
        let tm: i64 = tz.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
        th * 3600 + tm * 60
    } else { 8 * 3600 };
    // Hinnant days_from_civil：民用历 → 天数（零依赖，无 chrono）
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + se - offset)
}

/// 解析 get-user-resource 的 data：优先取**多资源包**形态（逐包剩余 + 逐包到期），
/// 退回扁平 credit。返回 (总剩余, 最早到期 epoch 秒) —— 两者都可能为 None。
///
/// ★ 为什么逐包：WB 的积分不是一个数，是多个包（套餐基础 / 每日 / 赠送）各有
///   不同到期时间。「快过期加权」要的是**最早到期那个非零包**的时刻 ——
///   光知道总额判断不出「这号快作废了」。
pub fn parse_user_resource(data: &Value) -> (Option<f64>, Option<i64>, Vec<Value>) {
    let accounts = data
        .get("Response")
        .and_then(|r| r.get("Data"))
        .and_then(|d| d.get("Accounts"))
        .and_then(|a| a.as_array());
    if let Some(arr) = accounts {
        if !arr.is_empty() {
            let mut total = 0.0f64;
            let mut any = false;
            let mut earliest: Option<i64> = None;
            // 逐包明细（对齐参考项目「积分明细」：面额/已用/剩余/到期各份单列）。
            let mut packs: Vec<Value> = Vec::new();
            for item in arr {
                let num = |k: &str| -> f64 {
                    item.get(k)
                        .and_then(|x| x.as_f64())
                        .or_else(|| item.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse().ok()))
                        .unwrap_or(0.0)
                };
                let left = num("CycleCapacityRemainPrecise");
                let cap = num("CycleCapacitySizePrecise");
                let expire = item
                    .get("DeductionEndTime")
                    .or_else(|| item.get("CycleEndTime"))
                    .and_then(|x| x.as_str())
                    .and_then(parse_time_epoch);
                // 名字以上游原文为准（参考实现 issue #239 的教训：本地码表会把
                // 上游改过的套餐名覆盖成过时旧名）；没有原文就退回包码。
                let name = item
                    .get("PackageName")
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .or_else(|| item.get("PackageCode").and_then(|x| x.as_str()))
                    .unwrap_or("未名份额")
                    .to_string();
                packs.push(json!({
                    "name": name,
                    "total": cap,
                    "left": left,
                    "expire_at": expire,
                }));
                // 剩 0 的包不算「还在用的额度」，它的到期也别拿来排序
                if left <= 0.0 {
                    continue;
                }
                any = true;
                total += left;
                if let Some(e) = expire {
                    earliest = Some(earliest.map_or(e, |cur| cur.min(e)));
                }
            }
            if any {
                return (Some(total), earliest, packs);
            }
        }
    }
    // 扁平形态（老口径）：credit / total_credit
    let num = |k: &str| -> Option<f64> {
        data.get(k)
            .and_then(|x| x.as_f64())
            .or_else(|| data.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse().ok()))
    };
    let a = num("credit");
    let b = num("total_credit");
    let credit = match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    }
    .filter(|c| *c > 0.0);
    (credit, None, Vec::new())
}

#[cfg(test)]
mod rate_tests {
    use super::*;
    use serde_json::json;

    /// 三种时间形态都要折成 epoch；解析不出必须是 None（不猜）。
    #[test]
    fn parse_time_epoch_forms() {
        assert_eq!(parse_time_epoch("2026-10-27T15:00:00Z"), Some(1_793_113_200));
        // +08 的 15:00 == 07:00Z：与上一行同一时刻
        assert_eq!(
            parse_time_epoch("2026-10-27T15:00:00+08:00"),
            parse_time_epoch("2026-10-27T07:00:00Z")
        );
        // 无时区按 +8（WB 中文接口口径）：比 Z 少 8 小时
        assert_eq!(
            parse_time_epoch("2026-10-27 15:00:00"),
            parse_time_epoch("2026-10-27T07:00:00Z")
        );
        // 纯数字秒/毫秒
        assert_eq!(parse_time_epoch("1793113200"), Some(1_793_113_200));
        assert_eq!(parse_time_epoch("1793113200000"), Some(1_793_113_200));
        assert_eq!(parse_time_epoch(""), None);
        assert_eq!(parse_time_epoch("下周二"), None);
    }

    /// 多资源包：求和、取最早到期、跳过剩余 0 的包。
    #[test]
    fn parse_user_resource_multi_pack() {
        let d = json!({
            "Response": {"Data": {"Accounts": [
                {"CycleCapacityRemainPrecise": 100.0, "DeductionEndTime": "2026-10-27T15:00:00Z"},
                {"CycleCapacityRemainPrecise": 50, "DeductionEndTime": "2026-10-28T15:00:00Z"},
                {"CycleCapacityRemainPrecise": 0.0, "DeductionEndTime": "2026-10-26T15:00:00Z"},
                {"CycleCapacityRemainPrecise": 0.0, "DeductionEndTime": "2026-10-20T00:00:00Z"},
            ]}}
        });
        let (credit, exp, packs) = parse_user_resource(&d);
        assert_eq!(credit, Some(150.0));
        // 明细含**全部四行**（包括剩 0 的）——「已用完」也是要展示的份额
        assert_eq!(packs.len(), 4);
        let p0 = &packs[0];
        assert_eq!(p0["left"], json!(100.0));
        assert!(p0["expire_at"].is_number());
        assert_eq!(exp, parse_time_epoch("2026-10-27T15:00:00Z"));
    }

    /// 扁平老形态：取 credit/total_credit 大者。
    #[test]
    fn parse_user_resource_flat() {
        let d = json!({"credit": 300, "total_credit": 500});
        let (c, e, packs) = parse_user_resource(&d);
        assert_eq!((c, e), (Some(500.0), None));
        assert!(packs.is_empty(), "扁平形态没有份额明细");
        let d2 = json!({"ok": true});
        let (c2, e2, packs2) = parse_user_resource(&d2);
        assert_eq!((c2, e2), (None, None));
        assert!(packs2.is_empty());
    }


    /// 倍率值形态五花八门：字符串带前缀、数字、乱格式；越界数字不算。
    #[test]
    fn extract_rate_shapes() {
        assert_eq!(extract_rate(&json!("x0.79 credits")).as_deref(), Some("0.79"));
        assert_eq!(extract_rate(&json!(0.5)).as_deref(), Some("0.50"));
        assert_eq!(extract_rate(&json!("1.2")).as_deref(), Some("1.20"));
        assert_eq!(extract_rate(&json!("x3")).as_deref(), Some("3.00"));
        // 键名命中但值不是数字：不许把原文当倍率
        assert_eq!(extract_rate(&json!("免费")), None);
        assert_eq!(extract_rate(&json!(123456)), None, "越界数（多半是 token 价）");
    }

    /// 目录结构千层饼：递归找条目 + 直接/嵌套倍率 + id 键位候选。
    #[test]
    fn walk_models_finds_nested_entries() {
        let v = json!({
            "data": {"models": [
                {"id": "glm-5.3", "pricing": {"credits": "x0.53 credits"}},
                {"modelId": "kimi-k3", "credit": 1.62},
                {"name": "无 id 的对象", "inner": [{"model": "deep.x", "rate": 2}]},
                {"id": "no-rate-model"},
            ]}
        });
        let mut out = Vec::new();
        walk_models(&v, &mut out, 0);
        let map: std::collections::HashMap<_, _> = out
            .into_iter()
            .filter(|(_, r)| r.is_some())
            .map(|(i, r)| (i, r.unwrap()))
            .collect();
        assert_eq!(map.get("glm-5.3").map(String::as_str), Some("0.53"));
        assert_eq!(map.get("kimi-k3").map(String::as_str), Some("1.62"));
        assert_eq!(map.get("deep.x").map(String::as_str), Some("2.00"));
        assert!(!map.contains_key("no-rate-model"));
    }

    /// intl 与国内站各自的端点；未知平台 None。
    #[test]
    fn console_endpoints_map() {
        assert!(console_models_url("wb-gateway").unwrap().contains("copilot.tencent.com"));
        assert!(console_models_url("wb-gateway-intl").unwrap().contains("codebuddy.ai"));
        assert_eq!(console_models_url("kilo"), None);
    }
}

// ── 模型倍率在线目录（best-effort） ───────────────────────────

/// console 模型目录端点（倍率的在线权威）。未登录时上游回 401/302，
/// 调用方**必须**按失败处理 —— 解析只在 2xx + JSON 上做。
pub fn console_models_url(platform: &str) -> Option<&'static str> {
    match platform {
        "wb-gateway" | "codebuddy" => {
            Some("https://copilot.tencent.com/console/enterprises/personal/models")
        }
        "wb-gateway-intl" => {
            Some("https://www.codebuddy.ai/console/enterprises/personal/models")
        }
        _ => None,
    }
}

/// 倍率字段候选（旧版 gwextra.py 实测沉淀：不同版本接口字段不一样）。
const RATE_KEYS: &[&str] = &[
    "credits",
    "credit",
    "credits_per_token",
    "creditRate",
    "credit_rate",
    "rate",
    "multiplier",
    "ratio",
    "creditsMultiplier",
    "credits_multiplier",
    "creditCost",
    "price",
    "pricing",
    "cost",
];

/// 从 "x0.79 credits" / 0.79 / "0.79" / "x0.79" 这类形态里抠出倍率数字串。
/// 抠不出返回 None（键名命中但值不是数字时**不许**把原文当倍率）。
pub fn extract_rate(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if (0.0..=1000.0).contains(&f) {
                    return Some(format!("{:.2}", f));
                }
            }
            None
        }
        Value::String(s) => {
            // 抠出第一段数字（x0.79 credits → 0.79）
            let bytes: Vec<char> = s.chars().collect();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i].is_ascii_digit() {
                    break;
                }
                i += 1;
            }
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.') {
                i += 1;
            }
            if start == i {
                return None;
            }
            let num: String = bytes[start..i].iter().collect();
            num.parse::<f64>().ok().and_then(|f| {
                if (0.0..=1000.0).contains(&f) {
                    Some(format!("{f:.2}"))
                } else {
                    None
                }
            })
        }
        _ => None,
    }
}

fn find_rate_in(item: &Value) -> Option<String> {
    // 直接字段 → 一层嵌套（旧版同款顺序）
    if let Some(o) = item.as_object() {
        for k in RATE_KEYS {
            if let Some(v) = o.get(*k) {
                if let Some(r) = extract_rate(v) {
                    return Some(r);
                }
            }
        }
        for v in o.values() {
            if v.is_object() {
                if let Some(r) = find_rate_in(v) {
                    return Some(r);
                }
            }
        }
    }
    None
}

/// 递归找模型条目（接口可能包在 data.models / models / list 里）。
/// 条目判定：含 id/modelId/model/model_id 任一键的对象。
fn walk_models(node: &Value, out: &mut Vec<(String, Option<String>)>, depth: u32) {
    if depth > 5 {
        return;
    }
    match node {
        Value::Array(arr) => {
            for x in arr {
                let id = x
                    .as_object()
                    .and_then(|o| {
                        ["id", "modelId", "model", "model_id"]
                            .iter()
                            .find_map(|k| o.get(*k).and_then(|v| v.as_str().map(|s| s.to_string())))
                    });
                if let Some(id) = id {
                    out.push((id, find_rate_in(x)));
                } else {
                    walk_models(x, out, depth + 1);
                }
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                if matches!(
                    k.as_str(),
                    "models" | "model_list" | "modelList" | "list" | "items" | "entries" | "data"
                ) {
                    if v.is_array() || v.is_object() {
                        walk_models(v, out, depth + 1);
                    }
                } else {
                    walk_models(v, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

/// 拉在线倍率目录。返回 模型裸名 → 倍率串；调用方拿到**非空**才覆盖缓存。
pub async fn fetch_model_rates(
    http: &Client,
    platform: &str,
    secret: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let url = console_models_url(platform).ok_or("平台没有在线目录端点")?;
    let mut rb = http
        .get(url)
        .timeout(std::time::Duration::from_secs(15))
        .header("Accept", "application/json")
        .header("User-Agent", WB_CLI_UA);
    // 凭据形态自适应（旧版实测）：含 `=` 的当 Cookie，否则 Bearer
    if secret.contains('=') && !secret.contains(' ') {
        rb = rb.header("Cookie", secret);
    } else {
        rb = rb.header("Authorization", format!("Bearer {secret}"));
    }
    let resp = rb.send().await.map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        // 401/302 是常态（APISIX 网关层拦未登录）——如实失败，不猜
        return Err(format!("HTTP {status}"));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("非 JSON 响应（可能被重定向到登录页）：{e}"))?;
    let mut found = Vec::new();
    walk_models(&v, &mut found, 0);
    let mut map = std::collections::HashMap::new();
    for (id, rate) in found {
        if let Some(r) = rate {
            map.entry(id).or_insert(r);
        }
    }
    if map.is_empty() {
        return Err("接口可达但未解析出任何倍率（结构可能变化）".to_string());
    }
    Ok(map)
}
