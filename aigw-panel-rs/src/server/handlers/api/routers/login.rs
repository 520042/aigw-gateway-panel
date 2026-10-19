use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use super::util::*;
use super::wb::*;

/// 对齐 `api/login` 全 action（GET+POST；action 走 query，body 走 JSON）：
///   platforms / browser / start / poll / submit / cancel / qrscan
/// start 生成会话（waiting + auth_url）；submit 保存凭据后会话转 success，
/// 前端轮询到 success 自动收尾 ——「跳转登录 + 手动粘贴」全流程真实闭环。
#[tracing::instrument(skip(st, body))]
pub async fn login(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    match action.as_str() {
        "platforms" => Json(json!({
            "ok": true,
            "platforms": st.inner.catalog.platform_list()
        })),
        "browser" => Json(json!({
            "ok": true,
            "browser": {"profiles": [], "usable": false, "auto_supported": false,
                        "reason": "Rust 版暂不支持本机浏览器直读，请用跳转登录 + 手动粘贴凭据"}
        })),
        "start" => {
            let platform = b.get("platform").and_then(|v| v.as_str()).unwrap_or("");
            if platform.is_empty() {
                return Json(json!({"ok": false, "message": "platform 必填"}));
            }
            let (method, login_url) = st
                .inner
                .catalog
                .platform_list()
                .as_array()
                .and_then(|arr| {
                    arr.iter().find(|p| p.get("id").and_then(|v| v.as_str()) == Some(platform))
                })
                .map(|p| {
                    (
                        p.get("method").and_then(|v| v.as_str()).unwrap_or("file").to_string(),
                        p.get("login_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    )
                })
                .unwrap_or(("file".to_string(), String::new()));
            let method = if method.is_empty() { "file".to_string() } else { method };
            // copilot 系（WorkBuddy 国内/国际）：对齐 Python tlogin 原生链路 ——
            // 本地生成 state → 授权页 https://copilot.tencent.com/login?platform=CLI&state=
            // → 用户在页面登录后 F12 抓 /console/login/enterprise 的 accessToken 手动粘贴
            // （2026-10-13 用户拍板的「跳转制」，Python 侧同样不再轮询自动捕捉）。
            let mut message = String::new();
            let mut login_url = login_url;
            let mut wb_state = String::new();
            if platform.starts_with("wb-gateway") {
                // ★ 2026-10-14 实拆 workbuddy2api-panel（活跃维护的同族实现）：
                //   POST /v2/plugin/auth/state?platform=CLI 服务端签发 state
                //   （必须带 Origin: codebuddy.cn + CLI UA，否则 APISIX 拦 →
                //   Python 侧当年记的"404 已下线"实为缺头被拦）。
                //   之后轮询 GET /v2/plugin/auth/token?state= 拿 accessToken —— 全自动。
                let intl = platform == "wb-gateway-intl";
                let (base, origin) = if intl {
                    ("https://www.workbuddy.ai", "https://www.workbuddy.ai")
                } else {
                    ("https://copilot.tencent.com", "https://www.codebuddy.cn")
                };
                let client = reqwest::Client::new();
                let resp = client
                    .post(format!("{}/v2/plugin/auth/state?platform=CLI", base))
                    .timeout(std::time::Duration::from_secs(20))
                    .header("Origin", origin)
                    .header("Referer", format!("{}/", origin))
                    .header("User-Agent", "CLI/2.63.2 CodeBuddy/2.63.2")
                    .header("Content-Type", "application/json")
                    .body("{}")
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let v: Value = r.json().await.unwrap_or(Value::Null);
                        let st_ = v
                            .pointer("/data/state")
                            .or_else(|| v.get("state"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let auth = v
                            .pointer("/data/authUrl")
                            .or_else(|| v.get("authUrl"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !st_.is_empty() {
                            wb_state = st_.clone();
                            login_url = if !auth.is_empty() {
                                auth
                            } else {
                                let host = if intl { "www.workbuddy.cn" } else { "www.codebuddy.cn" };
                                format!("https://{}/login?platform=CLI&state={}", host, st_)
                            };
                            message = "已生成设备授权会话（服务端 state）。新标签页打开授权页完成登录后\
                                       面板自动拿到 token 并入库——无需手动粘贴。"
                                .to_string();
                        } else {
                            message = format!("auth/state 响应异常：{}", v.to_string().chars().take(200).collect::<String>());
                        }
                    }
                    Ok(r) => {
                        message = format!("auth/state HTTP {}（{}）——回退手动粘贴", r.status(), base);
                    }
                    Err(e) => message = format!("auth/state 请求失败：{} —— 回退手动粘贴", e),
                }
                if wb_state.is_empty() {
                    // 回退：网页登录 + 手动粘贴
                    let host = if intl { "www.workbuddy.cn" } else { "www.workbuddy.cn" };
                    login_url = format!(
                        "https://{}/login/?platform=agents&state=0&redirect_uri=https%3A%2F%2F{}%2Fapp",
                        host, host
                    );
                    if message.is_empty() {
                        message = "已打开网页登录页。登录后 F12 抓 /v2/ 请求的 Bearer 粘贴。".to_string();
                    }
                }
            } else if platform == "minimax" {
                // ★ MiniMax Code 设备码登录（对齐 dsh-our-free-model 渠道实现）：
                //   POST https://account.minimax.cn/oauth2/device/code
                //   (client_id=mcode-public, scope=agent.default, audience=agent-backend,
                //    PKCE S256) → user_code + verification_uri
                //   轮询 POST /oauth2/token (grant_type=device_code + code_verifier)
                //   → access_token 入库 → 签到/模型可用。
                match minimax_device_start().await {
                    Ok((device_code, user_code, verify_url, verifier)) => {
                        login_url = verify_url;
                        message = format!(
                            "MiniMax 设备码：打开验证页输入码 {} 完成授权，面板自动拿到 token 并入库。",
                            user_code
                        );
                        wb_state = format!("minimax:{}:{}:{}", device_code, verifier, user_code);
                    }
                    Err(e) => {
                        message = format!("MiniMax 设备码申请失败：{} —— 回退手动粘贴 access_token", e);
                    }
                }
            } else if !login_url.is_empty() {
                message = "已在你的浏览器打开登录页（新标签页）。完成登录后把 \
                           Cookie / Token 粘贴到下方输入框保存。"
                    .to_string();
            }
            let mut sess = st.inner.login.start_ex_state(platform, &method, &login_url, &message, &wb_state);
            // MiniMax 设备码会话：method 对齐前端 oauth_device 分支（显示设备码框），
            // user_code 从 state 里取出单列（前端 viewLoginSession 读 sess.user_code）。
            if platform == "minimax" && wb_state.starts_with("minimax:") {
                sess["method"] = json!("oauth_device");
                let parts: Vec<&str> = wb_state.splitn(4, ':').collect();
                if parts.len() >= 4 {
                    sess["user_code"] = json!(parts[3]);
                }
            }
            // ★ 2026-10-15（用户实拍「无法跳转登录」）：改回**双通道**——
            //   后端也用 cmd/c 打开默认浏览器（服务端 open_browser，实测面板启动时可用），
            //   彻底绕开「前端内嵌预览/弹窗拦截 window.open」导致的一点动静都没有。
            //   前端拿到 browser_opened=true 时不再自己开标签，避免开两个。
            // ★ 2026-10-15（用户要求「真正一键获取 Cookie」）：**Cookie 类平台**改用
            //   CDP 受控浏览器 —— 面板自己拉起一个带调试端口的 Edge/Chrome（独立 profile、
            //   登录态持久），用户在它里面登录后，经 CDP 直接读**明文 cookie**，
            //   完全绕开 app-bound(v20) 加密 / Cookies 文件被独占锁定 / 浏览器不在支持列表 这三大坑。
            if method == "cookie" && !login_url.is_empty() {
                match crate::server::cdp::ensure(&login_url).await {
                    Ok(_) => {
                        sess["cdp_port"] = json!(crate::server::cdp::CDP_PORT);
                        sess["browser_opened"] = json!(true);
                        sess["message"] = json!(
                            "已在**面板专用浏览器**里打开登录页（独立窗口，登录态会记住）。\
                             在里面完成登录后点「我已登录，立即获取」——面板会直接读取 Cookie，无需粘贴。"
                        );
                    }
                    Err(e) => {
                        // 退化：系统默认浏览器 + 手动粘贴
                        crate::server::open_browser(&login_url);
                        sess["browser_opened"] = json!(true);
                        sess["message"] = json!(format!(
                            "受控浏览器启动失败（{}）。已用你的默认浏览器打开登录页，\
                             请登录后把 Cookie 粘贴到下方输入框保存。",
                            e
                        ));
                    }
                }
                st.inner.login.update_session(&sess);
            } else if !login_url.is_empty() {
                crate::server::open_browser(&login_url);
                sess["browser_opened"] = json!(true);
            }
            Json(json!({"ok": true, "session": sess}))
        }
        "poll" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let Some(mut sess) = st.inner.login.poll(id) else {
                return Json(json!({"ok": false, "message": "会话不存在或已过期"}));
            };
            // wb 族设备授权轮询：GET /v2/plugin/auth/token?state= → accessToken 即成功
            let platform = sess.get("platform").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let state = sess.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string();

            // ★ 2026-10-15：CDP 受控浏览器会话 —— 「我已登录，立即获取」走到这里：
            //   经 CDP 读**明文 cookie**，按平台 hosts 过滤后入库（不涉及任何解密）。
            if sess.get("cdp_port").is_some() {
                let hosts = platform_hosts(&st, &platform);
                let host_list = if hosts.is_empty() {
                    vec![platform.clone()]
                } else {
                    hosts
                };
                match crate::server::cdp::all_cookies().await {
                    Ok(cookies) => {
                        let hdr = crate::server::cdp::cookie_header_for(&cookies, &host_list);
                        if hdr.is_empty() {
                            sess["message"] = json!(format!(
                                "受控浏览器里还没读到 {} 的 Cookie —— 请确认已在那个窗口里登录完成，再点一次。",
                                host_list.join(" / ")
                            ));
                        } else {
                            // ★ 校验闸门（2026-10-15）：只抓到 BAIDUID 这类追踪 cookie 时
                            //   **不能**当登录态入库 —— 先用平台 verify 规则在线确认一次。
                            let verify = st
                                .inner
                                .catalog
                                .platforms()
                                .get(&platform)
                                .and_then(|p| p.get("verify"))
                                .cloned();
                            let verdict = match &verify {
                                Some(v) => crate::server::cdp::verify_cookie(v, &hdr).await,
                                None => None,
                            };
                            if verdict == Some(false) {
                                sess["message"] = json!(
                                    "在受控浏览器里读到了 Cookie，但**在线校验未通过**（多半还没真正登录）。\
                                     请在那个窗口里完成登录后再点一次。"
                                );
                            } else {
                                // 固定 name → 反复点不会产生重复账号（add_account 按 platform+name 覆盖）
                                st.inner.login.add_account(
                                    &platform,
                                    &format!("{}（CDP 受控浏览器）", platform),
                                    "cookie",
                                    &hdr,
                                );
                                sess["status"] = json!("success");
                                sess["message"] = json!(format!(
                                    "已从受控浏览器读到 Cookie{}并保存（{} 字节）",
                                    if verdict.is_some() { "、校验通过" } else { "（该平台无校验规则，按未校验处理）" },
                                    hdr.len()
                                ));
                                st.inner.login.update_session(&sess);
                                tracing::info!(
                                    platform = %platform, bytes = hdr.len(),
                                    verified = verdict.is_some(), "CDP 抓取 Cookie 成功"
                                );
                                return Json(json!({"ok": true, "session": sess}));
                            }
                        }
                    }
                    Err(e) => {
                        sess["message"] = json!(format!("CDP 读取失败：{}（可改用手动粘贴 Cookie）", e));
                    }
                }
                st.inner.login.update_session(&sess);
                return Json(json!({"ok": true, "session": sess}));
            }

            if platform.starts_with("wb-gateway") && !state.is_empty() {
                let (base, origin) = if platform == "wb-gateway-intl" {
                    ("https://www.workbuddy.ai", "https://www.workbuddy.ai")
                } else {
                    ("https://copilot.tencent.com", "https://www.codebuddy.cn")
                };
                let client = reqwest::Client::new();
                let resp = client
                    .get(format!("{}/v2/plugin/auth/token?state={}", base, state))
                    .timeout(std::time::Duration::from_secs(15))
                    .header("Origin", origin)
                    .header("Referer", format!("{}/", origin))
                    .header("User-Agent", "CLI/2.63.2 CodeBuddy/2.63.2")
                    .send()
                    .await;
                if let Ok(r) = resp {
                    if let Ok(v) = r.json::<Value>().await {
                        let tok = v
                            .pointer("/data/accessToken")
                            .or_else(|| v.get("accessToken"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !tok.is_empty() {
                            // 顺手拿账号昵称
                            let acct = match client
                                .get(format!("{}/v2/plugin/login/account?state={}", base, state))
                                .timeout(std::time::Duration::from_secs(15))
                                .header("Origin", origin)
                                .header("User-Agent", "CLI/2.63.2 CodeBuddy/2.63.2")
                                .send()
                                .await
                            {
                                Ok(r2) => r2.json::<Value>().await.unwrap_or(Value::Null),
                                Err(_) => Value::Null,
                            };
                            let uid = acct
                                .pointer("/data/userId")
                                .or_else(|| acct.pointer("/data/uid"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("wb 自动登录");
                            let refresh = v
                                .pointer("/data/refreshToken")
                                .or_else(|| v.get("refreshToken"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            st.inner.login.add_account(&platform, &format!("{}（自动登录）", uid), "token", &tok);
                            st.inner.login.update_refresh(&platform, &refresh);
                            if let Some(done) = st.inner.login.complete(id, "自动登录成功，token 已入库") {
                                sess = done;
                            }
                        } else {
                            let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
                            sess["message"] = json!(if code == 11217 {
                                "等待你在授权页完成登录（11217 登录中）…".to_string()
                            } else {
                                format!("轮询中：{}", v.to_string().chars().take(120).collect::<String>())
                            });
                        }
                    }
                }
                st.inner.login.update_session(&sess);
            }
            // MiniMax 设备码轮询：state="minimax:{device_code}:{verifier}:{user_code}"
            //   POST /oauth2/token (grant_type=device_code + code_verifier) → access_token 入库
            else if platform == "minimax" && state.starts_with("minimax:") {
                let parts: Vec<&str> = state.splitn(4, ':').collect();
                if parts.len() >= 3 {
                    let device_code = parts[1];
                    let verifier = parts[2];
                    let client = reqwest::Client::new();
                    let resp = client
                        .post("https://account.minimax.cn/oauth2/token")
                        .timeout(std::time::Duration::from_secs(15))
                        .header("Accept", "application/json")
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .body(format!(
                            "grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={}&client_id=mcode-public&code_verifier={}",
                            device_code, verifier
                        ))
                        .send()
                        .await;
                    if let Ok(r) = resp {
                        if let Ok(v) = r.json::<Value>().await {
                            let tok = v
                                .get("access_token")
                                .or_else(|| v.pointer("/data/access_token"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !tok.is_empty() {
                                let refresh = v
                                    .get("refresh_token")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                st.inner.login.add_account("minimax", "MiniMax（设备码自动登录）", "token", &tok);
                                st.inner.login.update_refresh("minimax", &refresh);
                                if let Some(done) = st.inner.login.complete(id, "MiniMax 自动登录成功，token 已入库") {
                                    sess = done;
                                }
                            } else {
                                let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
                                // authorization_pending = 用户还没完成授权，继续等
                                if err == "authorization_pending" || err == "slow_down" {
                                    sess["message"] = json!(format!("等待你在验证页输入码完成授权（{}）…", parts.get(3).map(|s| s.to_string()).unwrap_or_default()));
                                } else if err == "expired_token" || err == "access_denied" {
                                    sess["status"] = json!("error");
                                    sess["message"] = json!(format!("设备码已过期或被拒绝（{}），请重新发起登录", err));
                                } else {
                                    sess["message"] = json!(format!("轮询中：{}", v.to_string().chars().take(120).collect::<String>()));
                                }
                            }
                        }
                    }
                    st.inner.login.update_session(&sess);
                }
            }
            Json(json!({"ok": true, "session": sess}))
        }
        "loomy_sms_send" => {
            // Loomy 短信登录第一步（签名协议复刻自 Python loomy_client.send_sms）
            let phone = b.get("phone").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if phone.is_empty() {
                return Json(json!({"ok": false, "message": "手机号必填"}));
            }
            match crate::server::loomy_auth::send_sms(&phone).await {
                Ok(msgid) => Json(json!({"ok": true, "msgid": msgid, "message": "验证码已发送"})),
                Err(e) => Json(json!({"ok": false, "message": e})),
            }
        }
        "loomy_sms_verify" => {
            let phone = b.get("phone").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            let code = b.get("code").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            let msgid = b.get("msgid").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if phone.is_empty() || code.is_empty() || msgid.is_empty() {
                return Json(json!({"ok": false, "message": "手机号/验证码/msgid 必填"}));
            }
            match crate::server::loomy_auth::verify_sms(&phone, &code, &msgid).await {
                Ok((session, uid)) => {
                    st.inner.login.add_account("apk-loomy", &format!("Loomy（{}）", phone), "token", &session);
                    Json(json!({"ok": true, "message": "Loomy 登录成功，session 已入账号池（14 天有效）", "uid": uid}))
                }
                Err(e) => Json(json!({"ok": false, "message": e})),
            }
        }
        "loomy_pwd" => {
            let phone = b.get("phone").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            let password = b.get("password").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if phone.is_empty() || password.is_empty() {
                return Json(json!({"ok": false, "message": "手机号/密码必填"}));
            }
            match crate::server::loomy_auth::pwd_login(&phone, &password).await {
                Ok((session, uid)) => {
                    st.inner.login.add_account("apk-loomy", &format!("Loomy（{}）", phone), "token", &session);
                    Json(json!({"ok": true, "message": "Loomy 登录成功，session 已入账号池（14 天有效）", "uid": uid}))
                }
                Err(e) => Json(json!({"ok": false, "message": e})),
            }
        }
        "submit" => {
            let platform = b.get("platform").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if platform.is_empty() {
                return Json(json!({"ok": false, "message": "platform 必填"}));
            }
            let secret = b
                .get("secret")
                .and_then(|v| v.as_str())
                .or_else(|| b.get("cookie").and_then(|v| v.as_str()))
                .unwrap_or("")
                .trim()
                .to_string();
            if secret.is_empty() {
                return Json(json!({"ok": false, "message": "凭据内容为空"}));
            }
            let typ = b
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if b.get("cookie").is_some() { "cookie".to_string() } else { "token".to_string() }
                });
            let name = b
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}（手动粘贴）", platform));
            st.inner.login.add_account(&platform, &name, &typ, &secret);
            if let Some(rt) = b.get("refresh_token").and_then(|v| v.as_str()) {
                st.inner.login.update_refresh(&platform, rt);
            }
            tracing::info!(platform, %typ, "凭据已入库（手动粘贴）");
            if let Some(id) = b.get("id").and_then(|v| v.as_str()) {
                if let Some(sess) = st.inner.login.complete(id, "凭据已保存到账号池") {
                    return Json(json!({"ok": true, "session": sess}));
                }
            }
            Json(json!({"ok": true}))
        }
        "verify_all" => {
            // ★ 逐账号真实验活：wb 族打 /console/enterprises/personal/models（Bearer），
            //   豆包打 passport/account/info/v2（user_id）；其余平台如实标未接入验活器。
            let rows = st.inner.login.list_accounts().0;
            let mut results: Vec<Value> = Vec::new();
            for a in &rows {
                let platform = a.get("platform").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let secret = st
                    .inner
                    .login
                    .secrets_for(&platform)
                    .first()
                    .cloned()
                    .unwrap_or_default();
                let (valid, msg) = verify_account(&platform, &secret).await;
                results.push(json!({
                    "platform": platform,
                    "name": a.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "valid": valid,
                    "message": msg,
                }));
            }
            Json(json!({"ok": true, "results": results}))
        }
        "cancel" => {
            if let Some(id) = b.get("id").and_then(|v| v.as_str()) {
                st.inner.login.cancel(id);
            }
            Json(json!({"ok": true}))
        }
        "qrscan" => Json(json!({
            "ok": false, "code": "not_ready",
            "message": "远程扫码（无头浏览器）模块待 Rust 接入；请用「跳转登录 + 手动粘贴」"
        })),
        _ => Json(json!({
            "ok": false, "code": "unknown_action",
            "message": format!("login action='{}' 未实现", action)
        })),
    }
}

/// 对齐 `/api/platform?action=...`：wb 族动作执行（checkin/points）真实端点。
#[tracing::instrument(skip(st, body))]
pub async fn platform_actions(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    match action.as_str() {
        "actions" => {
            // 平台内部能力表（数据驱动自 catalog.gwextra.actions）。
            // 前端 viewPlatformCaps 直接迭代 {pid: [actions...]}，runAction 再走 /api/gwextra?action=exec。
            let acts = st
                .inner
                .catalog
                .raw_get("gwextra")
                .and_then(|g| g.get("actions"))
                .cloned()
                .unwrap_or(json!({}));
            Json(json!({"ok": true, "actions": acts}))
        }
        // wb 族全套动作：签到/积分/成长任务（growth/streak/lottery/travel/buddy）
        "checkin" | "points" | "checkin_status"
        | "growth_tasks" | "growth_task_accept" | "growth_task_claim"
        | "streak_status" | "streak_redeem" | "lottery_summary" | "lottery_draw"
        | "buddy_info" | "buddy_adopt" | "travel_status" | "travel_depart" | "travel_claim" => {
            // platform：body 优先，query 兜底（GET 请求无 body）
            let pid = b
                .get("platform")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| q.get("platform").cloned())
                .unwrap_or_default();
            if pid.is_empty() {
                return Json(json!({"ok": false, "message": "platform 必填"}));
            }
            if !pid.starts_with("wb-gateway") {
                return Json(json!({"ok": false, "message": "该平台的签到/积分/成长任务动作仅 WorkBuddy 族支持"}));
            }
            Json(wb_action(&st, &pid, &action).await)
        }
        "trae_refresh" => {
            // Trae token 刷新（Trae2api-cn 实拆）：POST {host}/cloudide/api/v3/trae/oauth/ExchangeToken
            // {ClientID:"ono9krqynydwx5", RefreshToken, ClientSecret:"-", UserID}
            let pid = b.get("platform").and_then(|v| v.as_str()).unwrap_or("apk-trae").to_string();
            let Some(sec) = st.inner.login.secrets_for("apk-trae").first().cloned() else {
                return Json(json!({"ok": false, "message": "apk-trae 未登录（粘贴 RefreshToken）"}));
            };
            let client = reqwest::Client::new();
            let result = match client
                .post("https://api.trae.cn/cloudide/api/v3/trae/oauth/ExchangeToken")
                .timeout(std::time::Duration::from_secs(30))
                .json(&json!({
                    "ClientID": "ono9krqynydwx5",
                    "RefreshToken": sec,
                    "ClientSecret": "-",
                    "UserID": "",
                }))
                .send()
                .await
            {
                Ok(r) => {
                    let sc = r.status().as_u16();
                    let v: Value = r.json().await.unwrap_or(Value::Null);
                    let tok = v.pointer("/Result/Token").or_else(|| v.pointer("/Result/token")).and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let rt = v.pointer("/Result/RefreshToken").or_else(|| v.pointer("/Result/refreshToken")).and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if !tok.is_empty() {
                        st.inner.login.add_account("apk-trae", "apk-trae（refresh 续期）", "refresh_token", &tok);
                        st.inner.login.update_refresh("apk-trae", &rt);
                    }
                    json!({"ok": !tok.is_empty(), "status": sc, "got_token": !tok.is_empty(), "data": v})
                }
                Err(e) => json!({"ok": false, "message": format!("{}", e)}),
            };
            Json(result)
        }
        _ => Json(json!({"ok": false, "code": "unknown_action",
                    "message": format!("platform action='{}' 未实现", action)})),
    }
}

/// `/api/credentials`：凭据文件（账号池）列表 + 删除（对齐 Python api_creds 契约）。
pub async fn credentials(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let (rows, _stats) = st.inner.login.list_accounts();
    match action.as_str() {
        "delete" => {
            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let mut ok = false;
            for a in &rows {
                if a.get("name").and_then(|v| v.as_str()) == Some(name) {
                    if let Some(id) = a.get("id").and_then(|v| v.as_str()) {
                        ok = st.inner.login.delete_account(id);
                    }
                    break;
                }
            }
            Json(json!({"ok": ok, "message": if ok { "" } else { "凭据不存在" }}))
        }
        _ => {
            let files: Vec<Value> = rows
                .iter()
                .map(|a| {
                    json!({
                        "name": a.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "site": a.get("platform").and_then(|v| v.as_str()).unwrap_or(""),
                    })
                })
                .collect();
            Json(json!({"ok": true, "files": files}))
        }
    }
}

/// 单账号验活（真实探活，不伪造）。
async fn verify_account(platform: &str, secret: &str) -> (bool, String) {
    if secret.is_empty() {
        return (false, "凭据为空".to_string());
    }
    match platform {
        "apk-doubao" => {
            let ok = crate::relay::doubao::verify(secret).await;
            (
                ok,
                if ok {
                    "豆包 passport 验活通过（user_id 存在）".to_string()
                } else {
                    "豆包 passport 未返回 user_id（Cookie 可能失效）".to_string()
                },
            )
        }
        p if p.starts_with("wb-gateway") => {
            let base = if p == "wb-gateway-intl" {
                "https://www.codebuddy.ai"
            } else {
                "https://copilot.tencent.com"
            };
            let url = format!("{}/console/enterprises/personal/models", base);
            let client = reqwest::Client::new();
            let resp = client
                .get(&url)
                .timeout(std::time::Duration::from_secs(15))
                .header("Authorization", format!("Bearer {}", secret))
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
                )
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    (true, format!("WB 模型目录可达（HTTP {}）", r.status()))
                }
                Ok(r) => (
                    false,
                    format!("WB 模型目录 HTTP {}（token 失效或权限不足）", r.status()),
                ),
                Err(e) => (false, format!("验活请求失败：{}", e)),
            }
        }
        _ => (false, "该平台验活器属原生中继模块，尚未接入".to_string()),
    }
}
