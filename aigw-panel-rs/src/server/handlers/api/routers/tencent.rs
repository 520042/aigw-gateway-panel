use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use super::util::*;

/// `/api/tencent`：腾讯 CodeBuddy / WorkBuddy 直连 7 action（对齐 Python api_tencent）。
///   probe / login / poll / status / refresh / catalog / quota / checkin / trae_local / rates
#[tracing::instrument(skip(st, body))]
pub async fn tencent(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    use crate::server::tencent as TC;
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("probe").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let body_token = b.get("token").and_then(|v| v.as_str()).unwrap_or("");
    let (token, acct_name, base) = tc_resolve(&st, body_token);

    match action.as_str() {
        "probe" => {
            let probes = TC::probe_all(&token, &base, 8).await;
            let endpoints: Value = Value::Object(
                TC::ENDPOINTS
                    .iter()
                    .map(|(k, m, p)| (k.to_string(), json!({"method": m, "path": p})))
                    .collect(),
            );
            Json(json!({"ok": true, "probes": probes, "token": !token.is_empty(),
                        "account": acct_name, "endpoints": endpoints}))
        }
        "login" => {
            // 服务端签发 state（对齐 wb-gateway 登录）；失败回退本地 uuid。
            let mut wb_state = String::new();
            let mut url = String::new();
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());
            if let Ok(r) = client
                .post(format!("{}/v2/plugin/auth/state?platform=CLI", TC::BASE_CN))
                .header("Origin", "https://www.codebuddy.cn")
                .header("Referer", "https://www.codebuddy.cn/")
                .header("User-Agent", "CLI/2.63.2 CodeBuddy/2.63.2")
                .header("Content-Type", "application/json")
                .body("{}")
                .send()
                .await
            {
                if r.status().is_success() {
                    if let Ok(v) = r.json::<Value>().await {
                        let s = v
                            .pointer("/data/state")
                            .or_else(|| v.get("state"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let a = v
                            .pointer("/data/authUrl")
                            .or_else(|| v.get("authUrl"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !s.is_empty() {
                            wb_state = s;
                            url = if !a.is_empty() {
                                a
                            } else {
                                format!("https://www.codebuddy.cn/login?platform=CLI&state={}", wb_state)
                            };
                        }
                    }
                }
            }
            if wb_state.is_empty() {
                wb_state = TC::new_state();
                url = TC::login_url(&wb_state);
            }
            TC::remember_state(&wb_state, TC::BASE_CN);
            Json(json!({
                "ok": true, "state": wb_state, "url": url,
                "note": "已生成登录会话，请在新标签页完成扫码/账密登录；面板会自动拿到 token 并入库。"
            }))
        }
        "poll" => {
            let state = b
                .get("state")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| q.get("state").cloned())
                .unwrap_or_default();
            if state.is_empty() {
                return Json(json!({"ok": false, "code": "no_state", "message": "没有 state，请先发起登录"}));
            }
            let base = TC::base_of_state(&state).unwrap_or_else(|| TC::BASE_CN.to_string());
            let (_ok, d) = TC::call(&base, "", "oauth_token", &state, None, 15).await;
            let tok = d
                .pointer("/data/accessToken")
                .or_else(|| d.get("accessToken"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if tok.is_empty() {
                let code = d.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
                return Json(json!({
                    "ok": true, "state": state, "pending": true,
                    "message": if code == 11217 { "等待你在授权页完成登录（11217 登录中）…" }
                               else { "等待登录完成…" }
                }));
            }
            let refresh = d
                .pointer("/data/refreshToken")
                .or_else(|| d.get("refreshToken"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let user_id = d
                .pointer("/data/userId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let cred = json!({
                "accessToken": tok, "refreshToken": refresh, "userId": user_id,
                "savedAt": now_local(),
            });
            let saved = TC::save_cred(&cred);
            st.inner.login.add_account("wb-gateway", "腾讯直连（自动登录）", "token", &tok);
            if !refresh.is_empty() {
                st.inner.login.update_refresh("wb-gateway", &refresh);
            }
            Json(json!({"ok": true, "state": state, "saved": saved, "userId": user_id, "account": "原生登录"}))
        }
        "status" => {
            if token.is_empty() {
                return Json(json!({"ok": true, "logged": false, "account": ""}));
            }
            let v = TC::verify(&token, &base).await;
            Json(json!({"ok": true, "logged": true, "account": acct_name,
                        "tokenLen": token.len(), "verify": v}))
        }
        "refresh" => {
            let cred = TC::load_cred();
            let rt = cred.get("refreshToken").and_then(|v| v.as_str()).unwrap_or("");
            if rt.is_empty() {
                return Json(json!({"ok": false, "code": "no_refresh", "message": "没有 refreshToken，请先登录"}));
            }
            let (okk, d) = TC::refresh(rt, &base).await;
            if !okk {
                return Json(json!({"ok": false, "code": "refresh_failed",
                                   "message": d.to_string().chars().take(300).collect::<String>()}));
            }
            let mut newc = cred.clone();
            newc["accessToken"] = d.get("accessToken").cloned().unwrap_or(json!(""));
            newc["refreshToken"] = d.get("refreshToken").cloned().unwrap_or(json!(rt));
            newc["refreshed_at"] = json!(now_local());
            TC::save_cred(&newc);
            let at = d.get("accessToken").and_then(|v| v.as_str()).unwrap_or("");
            let v = TC::verify(at, &base).await;
            Json(json!({"ok": true, "refreshed": true, "refreshedAt": now_local(), "verify": v}))
        }
        "catalog" => {
            if token.is_empty() {
                return Json(json!({"ok": false, "code": "no_token", "message": "没有 token，请先登录"}));
            }
            let ms = TC::catalog(&token, &base).await;
            let n = ms.len();
            Json(json!({"ok": true, "models": ms, "count": n, "account": acct_name}))
        }
        "quota" => {
            if token.is_empty() {
                return Json(json!({"ok": false, "code": "no_token", "message": "没有 token，请先登录"}));
            }
            let (okk, qv) = TC::quota(&token, &base).await;
            if !okk {
                return Json(json!({"ok": false, "code": "quota_failed",
                                   "message": qv.to_string().chars().take(300).collect::<String>()}));
            }
            let (ok2, summary) = TC::quota_summary(&token, &base).await;
            let sum_f = |k: &str| -> f64 {
                summary.iter().map(|x| x.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0)).sum()
            };
            let (tr, ts, tu) = if ok2 {
                (sum_f("remain"), sum_f("size"), sum_f("used"))
            } else {
                (0.0, 0.0, 0.0)
            };
            let summary_out = if ok2 { summary } else { vec![] };
            Json(json!({
                "ok": true, "quota": qv,
                "summary": summary_out,
                "totalRemain": tr, "totalSize": ts, "totalUsed": tu,
            }))
        }
        "checkin" => {
            if token.is_empty() {
                return Json(json!({"ok": false, "code": "no_token", "message": "没有 token，请先登录"}));
            }
            let (okk, d) = TC::daily_checkin(&token, &base).await;
            let msg = d.get("msg").and_then(|v| v.as_str()).unwrap_or("");
            let code = d.get("code").cloned().unwrap_or(Value::Null);
            let already = msg.contains("已签到");
            Json(json!({
                "ok": true, "checkedIn": okk || already, "already": already,
                "code": code,
                "msg": if !msg.is_empty() { msg.to_string() } else if okk { "签到成功".to_string() } else { "失败".to_string() },
            }))
        }
        "trae_local" => {
            let mut pf = b.get("platform").and_then(|v| v.as_str()).unwrap_or("apk-trae").to_string();
            if pf != "apk-trae" && pf != "web-trae" {
                pf = "apk-trae".to_string();
            }
            let (okk, info) = TC::read_trae_token();
            if !okk {
                return Json(json!({
                    "ok": true, "found": false,
                    "candidates": info.get("candidates").cloned().unwrap_or(json!([])),
                    "error": info.get("error").and_then(|v| v.as_str()).unwrap_or(""),
                    "hint": info.get("hint").and_then(|v| v.as_str()).unwrap_or(""),
                }));
            }
            let secret = info.get("token").and_then(|v| v.as_str()).unwrap_or("");
            if secret.len() < 16 {
                return Json(json!({"ok": false, "code": "bad_token", "message": "读到的 token 异常（长度不足）"}));
            }
            let name = info.get("account").and_then(|v| v.as_str()).unwrap_or("Trae 本机登录态");
            st.inner.login.add_account(&pf, name, "token", secret);
            Json(json!({"ok": true, "found": true, "account": name,
                        "source": info.get("source").cloned().unwrap_or(Value::Null),
                        "note": format!("已写入 {} 账号，刷新账号页即可见", pf)}))
        }
        "rates" => {
            let (okk, d) = TC::rates(&token, &base).await;
            if okk {
                Json(d)
            } else {
                Json(json!({"ok": false, "code": "rates_failed",
                            "message": d.to_string().chars().take(300).collect::<String>()}))
            }
        }
        _ => {
            let list: Vec<&str> = TC::ENDPOINTS.iter().map(|(n, _, _)| *n).collect();
            Json(json!({"ok": true, "endpoints": list}))
        }
    }
}

/// 解析腾讯直连 token：body.token > data/tencent_cred.json > 账号池 wb 族。
/// 返回 (token, account_name, base)。
fn tc_resolve(st: &AppState, body_token: &str) -> (String, String, String) {
    if !body_token.is_empty() {
        return (body_token.to_string(), "body".to_string(), crate::server::tencent::BASE_CN.to_string());
    }
    let cred = crate::server::tencent::load_cred();
    if let Some(t) = cred.get("accessToken").and_then(|v| v.as_str()) {
        if !t.is_empty() {
            return (t.to_string(), "原生登录".to_string(), crate::server::tencent::BASE_CN.to_string());
        }
    }
    for pid in ["wb-gateway", "wb-gateway-intl", "apk-codebuddy", "apk-codebuddy-cn"] {
        if let Some(s) = st.inner.login.secrets_for(pid).first().cloned() {
            let base = if pid == "wb-gateway-intl" {
                crate::server::tencent::BASE_INTL
            } else {
                crate::server::tencent::BASE_CN
            };
            return (s, pid.to_string(), base.to_string());
        }
    }
    (String::new(), String::new(), crate::server::tencent::BASE_CN.to_string())
}
