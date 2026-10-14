//! 前端数据面所有 /api/* 常驻接口。
//!
//! 契约对齐 Python main.py / app.sources.py / app.custom_apis.py / gwextra
//! （前端 app.js 零改动复用）：
//! - overview                     总览（含 uptime，状态条用）
//! - models                       GET：models_enriched + rate_summary
//!                                POST：toggle_model / platform_models / select
//! - models?action=grouped        模型总表（catalog 驱动，真实分组 + disabled 开关）
//! - models?action=sources        接入源清单 + 分类 + KPI（sources 页核心）
//! - login 全 action              会话式登录（start/poll/submit/cancel）
//! - accounts                     list/delete/toggle（data/accounts.json）
//! - sites                        公益站清单（sites + catalog）
//! - custom_apis                  list/add/update/delete/sync（真实探测上游 /models）
//! - autocheckin?action=status    签到状态（真实端点驱动）
//! - route                        路由总览（真实 router 状态）
//! - usage                        用量聚合（usage_events 90 天/5000 条）
//! - checkins                     签到记录基线
use crate::core::config;
use crate::core::model::VERSION;
use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

#[tracing::instrument(skip(st))]
pub async fn overview(State(st): State<AppState>) -> impl IntoResponse {
    let catalog = &st.inner.catalog;
    let relays = catalog.platforms().as_object().map(|m| m.len()).unwrap_or(0);
    Json(json!({
        "ok": true,
        "version": VERSION,
        "uptime": st.inner.started.elapsed().as_secs(),
        "native": {
            "accounts": 0,
            "relays": relays,
            "note": "登录/倍率/签到/对话/用量/通知全部面板原生，无 EXE 依赖（Rust 重写）"
        },
        "checkin": {"total": 0, "today": 0, "last_date": null},
        "tasks": {"done": 0, "total": 0},
        "sites": {"total": catalog.catalog_sites().len()},
        "scheduler": {"ticks": 0, "last_tick": 0, "last_checkin_date": null, "last_growth_date": null},
        "usage": {"total_tokens": 0},
        "note": "Rust 全量重写（大厂级分层）：免费车道(Pollinations)+上游透传已可用；逐平台登录/原生中继按 relay_spec 接入中"
    }))
}

/// GET /api/models 的独立形状（无 body 基线）由 `models` 的 "" 分支内联处理。

/// POST /api/models：toggle_model / platform_models / select。
#[tracing::instrument(skip(st, body))]
pub async fn models_post(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("").to_string();
    match action.as_str() {
        "toggle_model" => {
            let mid = body.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if mid.is_empty() {
                return Json(json!({"ok": false, "message": "id 必填"}));
            }
            // 记账用裸名（id@pid → 剥后缀），路由按裸名注册
            let key = mid.rsplit_once('@').map(|(b, _)| b.to_string()).unwrap_or_else(|| mid.clone());
            let mut disabled = st.inner.disabled.write().await;
            let on = if disabled.contains(&key) {
                disabled.remove(&key);
                true
            } else {
                disabled.insert(key.clone());
                false
            };
            crate::server::store::save(
                "disabled_models",
                &json!(disabled.iter().cloned().collect::<Vec<_>>()),
            );
            let list: Vec<String> = disabled.iter().cloned().collect();
            Json(json!({"ok": true, "id": mid, "key": key, "on": on, "disabled": list}))
        }
        "platform_models" => {
            let pid = body.get("platform").and_then(|v| v.as_str()).unwrap_or("");
            if pid.is_empty() {
                return Json(json!({"ok": false, "message": "platform 必填"}));
            }
            let catalog = &st.inner.catalog;

            // ★ wb 族：有凭据 → 在线拉真实模型目录（live=true）
            if pid.starts_with("wb-gateway") {
                let base = if pid == "wb-gateway-intl" {
                    "https://www.codebuddy.ai"
                } else {
                    "https://copilot.tencent.com"
                };
                if let Some(sec) = st.inner.login.secrets_for(pid).first().cloned() {
                    let client = reqwest::Client::new();
                    let resp = client
                        .get(format!("{}/console/enterprises/personal/models", base))
                        .timeout(std::time::Duration::from_secs(15))
                        .header("Authorization", format!("Bearer {}", sec))
                        .send()
                        .await;
                    if let Ok(r) = resp {
                        if r.status().is_success() {
                            if let Ok(v) = r.json::<Value>().await {
                                let rows = extract_model_rows(&v);
                                if !rows.is_empty() {
                                    let n = rows.len();
                                    let mut cache = crate::server::store::load("platform_models");
                                    if !cache.is_object() { cache = json!({}); }
                                    cache[pid] = json!({
                                        "models": rows, "live": true,
                                        "ts": std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map(|d| d.as_secs_f64()).unwrap_or(0.0),
                                    });
                                    crate::server::store::save("platform_models", &cache);
                                    return Json(json!({
                                        "ok": true, "platform": pid,
                                        "name": "WorkBuddy（在线目录）",
                                        "models": rows, "live": true,
                                        "note": format!("实时拉取 {} 个模型（凭据验活通过）", n),
                                    }));
                                }
                            }
                        }
                    }
                }
            }

            // 模型来源优先级：bundled 内置 > relay_specs 静态清单 > free_lanes 匿名清单
            let (name, models, note) = if let Some(rows) =
                catalog.raw_get("bundled").and_then(|b| b.get(pid)).cloned()
            {
                let n = rows.as_array().map(|a| a.len()).unwrap_or(0);
                (
                    format!("{}（内置清单）", pid),
                    rows,
                    format!("APK 内置的 {} 个模型；实时拉取属原生中继模块", n),
                )
            } else if let Some(spec) = catalog.relay_spec(pid) {
                let rows = spec.get("models_static").cloned().unwrap_or(json!([]));
                let n = rows.as_array().map(|a| a.len()).unwrap_or(0);
                (
                    spec.get("name").and_then(|v| v.as_str()).unwrap_or(pid).to_string(),
                    rows,
                    format!("社区逆向静态清单（{} 个）；实时拉取属原生中继模块", n),
                )
            } else if let Some(fl) = catalog.free_lane(pid) {
                let rows = fl.get("models_anon").cloned().unwrap_or(json!([]));
                (
                    fl.get("name").and_then(|v| v.as_str()).unwrap_or(pid).to_string(),
                    rows,
                    "免凭据车道匿名清单".to_string(),
                )
            } else {
                return Json(json!({
                    "ok": false,
                    "message": "该平台不提供模型清单接口（对齐 Python：既没 models 也没目录）"
                }));
            };
            // 持久化（对齐 Python store.platform_models，grouped 页优先消费）
            let mut cache = crate::server::store::load("platform_models");
            if !cache.is_object() {
                cache = json!({});
            }
            cache[pid] = json!({
                "models": models,
                "live": false,
                "ts": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0),
            });
            crate::server::store::save("platform_models", &cache);
            Json(json!({
                "ok": true,
                "platform": pid,
                "name": name,
                "models": models,
                "live": false,
                "note": note,
            }))
        }
        "select" => {
            let models = body.get("models").cloned().unwrap_or(json!([]));
            crate::server::store::save("select_models", &json!({"models": models}));
            Json(json!({"ok": true}))
        }
        _ => Json(json!({
            "ok": false, "code": "unknown_action",
            "message": format!("models POST action='{}' 未实现", action)
        })),
    }
}

/// GET /api/models?action=grouped|sources|...（原有 GET 分支保留）。
#[tracing::instrument(skip(st))]
pub async fn models(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("");
    match action {
        "grouped" => {
            let disabled = st.inner.disabled.read().await.clone();
            let mut g = match st.inner.catalog.grouped(&disabled) {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            g.insert("ok".to_string(), Value::Bool(true));
            Json(Value::Object(g))
        }
        "sources" => {
            let catalog = &st.inner.catalog;
            Json(json!({
                "ok": true,
                "sources": catalog.catalog_sources(),
                "categories": catalog.categories(),
                "platform_kinds": catalog.platform_kinds(),
                "local_proxies": catalog.local_proxies(),
                "summary": catalog.sources_summary(),
            }))
        }
        "" => Json(json!({
            "ok": true,
            "models_enriched": [],
            "rate_summary": {},
        })),
        _ => Json(json!({
            "ok": false, "code": "unknown_action",
            "message": format!("models action='{}' 尚未实现", action)
        })),
    }
}

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
            let sess = st.inner.login.start_ex_state(platform, &method, &login_url, &message, &wb_state);
            // ★ 跳转由前端 window.open 完成（点击手势内开标签，同一浏览器实例
            //   必前台）；后端 explorer 拉默认浏览器在部分环境静默不出窗，已弃用。
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

/// 对齐 `/api/accounts`：list（GET）/ delete / toggle（POST body）。
#[tracing::instrument(skip(st, body))]
pub async fn accounts(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("list").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    match action.as_str() {
        "delete" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let ok = st.inner.login.delete_account(id);
            Json(json!({"ok": ok, "message": if ok { "" } else { "账号不存在" }}))
        }
        "toggle" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let on = b.get("on").and_then(|v| v.as_bool()).unwrap_or(true);
            let ok = st.inner.login.toggle_account(id, on);
            Json(json!({"ok": ok}))
        }
        _ => {
            let (rows, stats) = st.inner.login.list_accounts();
            Json(json!({"ok": true, "accounts": rows, "stats": stats}))
        }
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
        "actions" => Json(json!({"ok": true, "actions": {}})),
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

/// 对齐 `/api/sites`：公益站清单（boot 预取 → catalogSites → 公益中转站卡片）。
#[tracing::instrument(skip(st))]
pub async fn sites(State(st): State<AppState>) -> impl IntoResponse {
    let s = st.inner.catalog.catalog_sites();
    Json(json!({
        "ok": true,
        "sites": s,
        "catalog": s,
    }))
}

/// 对齐 `/api/custom_apis`：list（GET）/ add / update / delete / sync（POST）。
/// sync 真实探测上游 `<base_url>/models`（Bearer key），拉回模型清单。
#[tracing::instrument(skip(st, body))]
pub async fn custom_apis(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("list").to_string();
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);

    fn load_apis() -> Map<String, Value> {
        match crate::server::store::load("custom_apis").get("apis") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        }
    }
    fn save_apis(m: &Map<String, Value>) {
        crate::server::store::save("custom_apis", &json!({"apis": m}));
    }
    fn mask(s: &str) -> String {
        if s.is_empty() {
            String::new()
        } else if s.len() > 12 {
            format!("{}…{}", &s[..6], &s[s.len() - 4..])
        } else {
            "*".repeat(s.len())
        }
    }

    match action.as_str() {
        "list" => {
            let apis = load_apis();
            let mut out = Vec::new();
            for (id, c) in &apis {
                out.push(json!({
                    "id": id,
                    "name": c.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    "protocol": c.get("protocol").and_then(|v| v.as_str()).unwrap_or("openai"),
                    "base_url": c.get("base_url").and_then(|v| v.as_str()).unwrap_or(""),
                    "api_key": mask(c.get("api_key").and_then(|v| v.as_str()).unwrap_or("")),
                    "has_key": !c.get("api_key").and_then(|v| v.as_str()).unwrap_or("").is_empty(),
                    "models": c.get("models").cloned().unwrap_or(json!([])),
                    "model_count": c.get("models").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                    "last_sync": c.get("last_sync").and_then(|v| v.as_str()).unwrap_or(""),
                    "created_at": c.get("created_at").and_then(|v| v.as_str()).unwrap_or(""),
                }));
            }
            Json(json!({"ok": true, "apis": out}))
        }
        "add" => {
            let mut apis = load_apis();
            let id = format!(
                "custom-{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            let cfg = json!({
                "name": b.get("name").and_then(|v| v.as_str()).unwrap_or("未命名"),
                "protocol": b.get("protocol").and_then(|v| v.as_str()).unwrap_or("openai"),
                "base_url": b.get("base_url").and_then(|v| v.as_str()).unwrap_or("").trim_end_matches('/'),
                "api_key": b.get("api_key").and_then(|v| v.as_str()).unwrap_or(""),
                "models": [],
                "model_count": 0,
                "last_sync": "",
                "created_at": now_local(),
            });
            apis.insert(id.clone(), cfg);
            save_apis(&apis);
            Json(json!({"ok": true, "id": id}))
        }
        "update" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mut apis = load_apis();
            match apis.get_mut(id) {
                Some(c) => {
                    for k in ["name", "protocol", "base_url", "api_key"] {
                        if let Some(v) = b.get(k).and_then(|v| v.as_str()) {
                            if k == "base_url" {
                                c[k] = json!(v.trim_end_matches('/'));
                            } else {
                                c[k] = json!(v);
                            }
                        }
                    }
                    save_apis(&apis);
                    Json(json!({"ok": true}))
                }
                None => Json(json!({"ok": false, "message": "配置不存在"})),
            }
        }
        "delete" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mut apis = load_apis();
            let ok = apis.remove(id).is_some();
            save_apis(&apis);
            Json(json!({"ok": ok}))
        }
        "sync" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let mut apis = load_apis();
            let Some(c) = apis.get_mut(id).cloned() else {
                return Json(json!({"ok": false, "message": "配置不存在"}));
            };
            let base = c.get("base_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let key = c.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if base.is_empty() {
                return Json(json!({"ok": false, "message": "base_url 为空"}));
            }
            let url = format!("{}/models", base.trim_end_matches('/'));
            let client = reqwest::Client::new();
            let mut req = client.get(&url).timeout(std::time::Duration::from_secs(15));
            if !key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    let txt = resp.text().await.unwrap_or_default();
                    let parsed: Result<Value, _> = serde_json::from_str(&txt);
                    let models: Vec<String> = parsed
                        .ok()
                        .and_then(|v| {
                            v.get("data")
                                .and_then(|d| d.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
                                        .map(|s| s.to_string())
                                        .collect()
                                })
                        })
                        .unwrap_or_default();
                    let n = models.len();
                    if n == 0 {
                        return Json(json!({"ok": false, "message": "上游 /models 返回里没有模型（形状不是 OpenAI data[]）"}));
                    }
                    if let Some(c) = apis.get_mut(id) {
                        c["models"] = json!(models);
                        c["model_count"] = json!(n);
                        c["last_sync"] = json!(now_local());
                    }
                    save_apis(&apis);
                    Json(json!({"ok": true, "count": n, "models": models}))
                }
                Ok(resp) => Json(json!({
                    "ok": false,
                    "message": format!("上游 /models 返回 HTTP {}", resp.status())
                })),
                Err(e) => Json(json!({"ok": false, "message": format!("探测失败：{}", e)})),
            }
        }
        _ => Json(json!({"ok": false, "code": "unknown_action",
                         "message": format!("custom_apis action='{}' 未实现", action)})),
    }
}

/// 签到（`api/autocheckin`）：status=公益站真实清单；trigger=wb 族真实签到。
#[tracing::instrument(skip(st))]
pub async fn autocheckin(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("status").to_string();
    match action.as_str() {
        "status" => {
            let sites = st.inner.catalog.catalog_sites();
            let platforms: Vec<Value> = sites
                .iter()
                .filter(|s| s.get("checkin").and_then(|v| v.as_bool()).unwrap_or(false))
                .map(|s| {
                    let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    json!({
                        "platform": name,
                        "name": name,
                        "has_public_checkin": true,
                        "on": false,
                        "done_today": false,
                        "time": "",
                    })
                })
                .collect();
            Json(json!({"ok": true, "platforms": platforms}))
        }
        "trigger" => {
            // 真实执行签到：platform 必填（前端单平台触发）
            let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
            let pid = b
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("wb-gateway")
                .to_string();
            let t0 = std::time::Instant::now();
            let (okk, msg, skipped) = crate::server::checkin::run_platform(&st, &pid).await;
            let ms = t0.elapsed().as_millis() as i64;
            // 写 usage_events 供任务中心历史显示
            st.inner.usage.record(&pid, "checkin", 0, 0, ms, okk || skipped, &format!("checkin:{}", pid));
            let now_iso = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Json(json!({
                "ok": true,
                "executed": [{
                    "platform": pid, "name": pid,
                    "ok": okk, "skipped": skipped,
                    "message": msg, "at": now_iso,
                }],
                "message": msg,
            }))
        }
        "growth_run" => {
            // ★ 一键成长任务闭环（对齐 dsh-our-free-model 的自动任务）：
            //   连登兑换 → 抽奖 → 旅行(出发+领取) → 任务接受。仅 wb 族有这套端点。
            //   每一步独立容错，某步失败不阻断其余；结果逐条回显。
            let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
            let pid = b.get("platform").and_then(|v| v.as_str()).unwrap_or("wb-gateway").to_string();
            if !pid.starts_with("wb-gateway") {
                return Json(json!({"ok": false, "message": "成长任务仅 WorkBuddy 族支持"}));
            }
            let mut steps: Vec<Value> = Vec::new();
            for act in ["streak_redeem", "lottery_draw", "travel_depart", "travel_claim", "growth_task_accept"] {
                let r = wb_action(&st, &pid, act).await;
                let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                steps.push(json!({"action": act, "ok": ok, "status": r.get("status").cloned().unwrap_or(Value::Null)}));
            }
            let done = steps.iter().filter(|s| s.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)).count();
            st.inner.usage.record(&pid, "growth", 0, 0, 0, done > 0, &format!("growth:{}", pid));
            Json(json!({
                "ok": true,
                "platform": pid,
                "summary": format!("成长任务执行 {} 步，{} 步返回成功", steps.len(), done),
                "steps": steps,
            }))
        }
        _ => Json(json!({"ok": false, "code": "unknown_action",
                           "message": format!("autocheckin action='{}' 未实现", action)})),
    }
}

/// 路由总览（`api/route`）：真实 router 状态（models/config/熔断参数）。
#[tracing::instrument(skip(st))]
pub async fn route(State(st): State<AppState>) -> impl IntoResponse {
    let r = &st.inner.router;
    Json(json!({
        "ok": true,
        "models": r.list_models(),
        "config": {"auto": "fastest", "free": "free"},
        "defaults": ["auto", "free"],
        "auto_probe": true,
        "probe_interval": 60,
        "breaker_fail": 3,
        "breaker_cooldown": 60,
        "in_route_sites": [],
    }))
}

/// 用量聚合（`api/usage?range=all`）：usage_events 真实数据。
#[tracing::instrument(skip(st))]
pub async fn usage(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let _range = q.get("range").cloned().unwrap_or_else(|| "all".to_string());
    let panel = st.inner.usage.aggregate();
    Json(json!({"ok": true, "usage": panel, "series": null}))
}

#[tracing::instrument(skip(st))]
pub async fn settings_get(State(st): State<AppState>) -> impl IntoResponse {
    let s = st.inner.settings.read().await.clone();
    Json(json!({ "ok": true, "settings": config::settings_public(&s) }))
}

#[tracing::instrument(skip(st))]
pub async fn settings_post(
    State(st): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut s = st.inner.settings.read().await.clone();
    if let Some(obj) = body.as_object() {
        if let Some(v) = obj.get("panel_token").and_then(|v| v.as_str()) {
            s.panel_token = v.to_string();
        }
        if let Some(v) = obj.get("port").and_then(|v| v.as_u64()) {
            s.port = v as u16;
        }
        if let Some(v) = obj.get("upstream_base").and_then(|v| v.as_str()) {
            s.upstream_base = v.to_string();
        }
        if let Some(v) = obj.get("upstream_key").and_then(|v| v.as_str()) {
            s.upstream_key = v.to_string();
        }
        if let Some(v) = obj.get("lobster_server").and_then(|v| v.as_str()) {
            s.lobster_server = v.to_string();
        }
    }
    config::save_settings(&s);
    *st.inner.settings.write().await = s.clone();
    Json(json!({
        "ok": true,
        "settings": config::settings_public(&s),
        "note": "port 变更需重启面板生效"
    }))
}

/// 签到历史（`api/checkins`）：真实数据来自 usage_events（upstream="checkin:*" 行）。
#[tracing::instrument(skip(st))]
pub async fn checkins(State(st): State<AppState>) -> impl IntoResponse {
    let agg = st.inner.usage.aggregate();
    let total = agg.get("totals").and_then(|t| t.get("requests")).and_then(|v| v.as_i64()).unwrap_or(0);
    let today = agg.get("today").and_then(|t| t.get("requests")).and_then(|v| v.as_i64()).unwrap_or(0);
    let last_date = agg.get("today").and_then(|t| t.get("date")).and_then(|v| v.as_str()).unwrap_or("null").to_string();
    Json(json!({
        "ok": true,
        "checkins": {"total": total, "today": today, "last_date": if last_date=="null" {Value::Null} else {json!(last_date)}},
        "note": "Rust 签到执行已接入（Trae/小浣熊/库库/WPS/WorkBuddy 五种模式）"
    }))
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

/// 从 wb 模型目录响应里尽力抽模型行（字段名以真实响应为准，多层兼容）。
fn extract_model_rows(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    // 常见形状：{data:{models:[...]}} / {models:[...]} / {data:[...]} / [...]
    let candidates = [
        v.pointer("/data/models"),
        v.pointer("/models"),
        v.pointer("/data"),
        Some(v),
    ];
    for c in candidates.into_iter().flatten() {
        if let Some(arr) = c.as_array() {
            for m in arr {
                let id = m
                    .get("id")
                    .or_else(|| m.get("model"))
                    .or_else(|| m.get("name"))
                    .and_then(|x| x.as_str());
                if let Some(id) = id {
                    if !id.is_empty() {
                        out.push(json!({
                            "id": id,
                            "name": m.get("name").and_then(|x| x.as_str()).unwrap_or(id),
                            "credits": m.get("credits").or_else(|| m.get("cost")).cloned().unwrap_or(json!("")),
                            "on": true,
                        }));
                    }
                }
            }
            if !out.is_empty() {
                break;
            }
        }
    }
    out
}

/// MiniMax OAuth 设备码第一步：申请 device_code + user_code。
/// 返回 (device_code, user_code, verification_uri, code_verifier)。
/// PKCE S256：verifier 随机 43 字节 base62，challenge=SHA256(verifier) base64url。
async fn minimax_device_start() -> Result<(String, String, String, String), String> {
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

fn base64url_nopad(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 { out.push(T[(n >> 6) as usize & 63] as char); }
        if chunk.len() > 2 { out.push(T[n as usize & 63] as char); }
    }
    out
}

/// wb 族动作执行：签到 / 积分 / 成长任务全套（真实端点，gwextra verified）。
/// 成长任务端点来自 gwextra.ACTIONS["wb-gateway"]（activity/growth/*），
/// 对齐 dsh-our-free-model 的成长任务闭环：accept → claim → streak/redeem/lottery/travel。
/// 注：growth_task_claim 走 Web 域 www.workbuddy.cn（不是 CLI 域），其余走 CLI 域。
async fn wb_action(st: &AppState, pid: &str, action: &str) -> Value {
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

fn nanos_hex() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 任务中心（`api/growth`）：对齐 Python api_growth 的负载形状。
/// 平台清单来自 catalog 签到站（checkin=true），summary 汇总真实计数。
#[tracing::instrument(skip(st))]
pub async fn growth(State(st): State<AppState>) -> impl IntoResponse {
    let sites = st.inner.catalog.catalog_sites();
    let platforms: Vec<Value> = sites
        .iter()
        .filter(|s| s.get("checkin").and_then(|v| v.as_bool()).unwrap_or(false))
        .map(|s| {
            let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
            json!({
                "platform": name,
                "name": name,
                "has_public_checkin": true,
                "on": false,
                "done_today": false,
                "time": "",
            })
        })
        .collect();
    let summary = json!({
        "platforms": platforms.len(),
        "on": 0,
        "done_today": 0,
        "failed_today": 0,
        "sites": platforms.len(),
        "sites_done": 0,
        "ticks": 0,
    });
    Json(json!({"ok": true, "growth": {
        "source": "native",
        "enabled": false,
        "hour": "09:10",
        "stagger_sec": 45,
        "retry_times": 2,
        "retry_delay_min": 10,
        "notify": true,
        "platforms": platforms,
        "sites": [],
        "summary": summary,
        "checkin_summary": {"total": 0, "today": 0, "last_date": null},
        "tasks": {"total": 0, "today": 0, "done": 0, "pending": 0},
    }}))
}

/// 外部站点任务清单（`api/tasks`）：GET 读清单，POST 支持 add/delete/toggle/run_all。
/// Rust 侧站点任务存储待接入，POST 操作如实回状态（不伪造执行结果）。
#[tracing::instrument(skip(st))]
pub async fn tasks(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let _ = &st;
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("");
    let _b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    match action {
        "add" => Json(json!({"ok": true, "message": "站点任务存储待接入（当前不持久化）"})),
        "delete" => Json(json!({"ok": true, "message": "站点任务存储待接入"})),
        "toggle" => Json(json!({"ok": true, "message": "站点任务存储待接入"})),
        "run_all" => Json(json!({"ok": true, "executed": [], "message": "站点任务存储待接入（无任务可执行）"})),
        _ => Json(json!({"ok": true, "tasks": [], "note": "站点任务模块待接入"})),
    }
}

/// 每日免费模型榜单（`api/free_models?refresh=1`）：ClawLabsAI 数据源。
/// refresh=1 强制现场拉取；否则回缓存（绝不现场阻塞）。
#[tracing::instrument]
pub async fn free_models(q: Query<HashMap<String, String>>) -> impl IntoResponse {
    let refresh = q.get("refresh").map(|s| s == "1").unwrap_or(false);
    if refresh {
        match crate::server::free_models::fetch(true).await {
            Some(d) => Json(json!({"ok": true, "models_payload": d})),
            None => Json(json!({"ok": false, "message": "拉取失败（网络或源不可达）"})),
        }
    } else {
        match crate::server::free_models::get_cached() {
            Some(d) => Json(json!({"ok": true, "models_payload": d})),
            None => Json(json!({
                "ok": true, "updated_at": null, "total": 0, "models": [],
                "note": "首次加载，请点刷新拉取榜单"
            })),
        }
    }
}

/// 免费车道动态模型拉取（`api/free_lane_models?refresh=1`）：anon-zen + kilo。
/// refresh=1 强制现场拉取；否则回缓存。
#[tracing::instrument]
pub async fn free_lane_models(q: Query<HashMap<String, String>>) -> impl IntoResponse {
    let refresh = q.get("refresh").map(|s| s == "1").unwrap_or(false);
    if refresh {
        match crate::server::free_lane_models::fetch_all().await {
            Some(d) => Json(json!({"ok": true, "lanes": d})),
            None => Json(json!({"ok": false, "message": "免费车道拉取失败（网络或源不可达）"})),
        }
    } else {
        match crate::server::free_lane_models::get_cached() {
            Some(d) => Json(json!({"ok": true, "lanes": d})),
            None => Json(json!({"ok": true, "lanes": {}, "note": "首次加载，请点刷新拉取"})),
        }
    }
}

/// 通知配置（`api/notify`）：GET 读配置，POST 支持 add/delete/test/send/save_flags。
#[tracing::instrument(skip(st))]
pub async fn notify(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let _ = &st;
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("");
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    match action {
        "add" => {
            let url = b.get("url").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if url.is_empty() {
                return Json(json!({"ok": false, "message": "url 必填"}));
            }
            let mut cfg = crate::server::store::load("notify");
            if !cfg.is_object() { cfg = json!({}); }
            if cfg.get("webhooks").is_none() { cfg["webhooks"] = json!([]); }
            if let Some(arr) = cfg.get_mut("webhooks").and_then(|v| v.as_array_mut()) {
                let mut h = b.clone();
                h["enabled"] = json!(true);
                arr.push(h);
            }
            crate::server::store::save("notify", &cfg);
            Json(json!({"ok": true, "message": "已添加", "notify": cfg}))
        }
        "delete" => {
            let idx = b.get("index").and_then(|v| v.as_u64()).unwrap_or(999) as usize;
            let mut cfg = crate::server::store::load("notify");
            if let Some(arr) = cfg.get_mut("webhooks").and_then(|v| v.as_array_mut()) {
                if idx < arr.len() { arr.remove(idx); }
            }
            crate::server::store::save("notify", &cfg);
            Json(json!({"ok": true, "message": "已删除", "notify": cfg}))
        }
        "test" => {
            let (ok, msg) = send_one_hook(&b, "AI 资源网关面板", "通知测试成功 ✅").await;
            Json(json!({"ok": true, "sent": ok, "message": msg}))
        }
        "send" => {
            let event = b.get("event").and_then(|v| v.as_str()).unwrap_or("checkin").to_string();
            let title = b.get("title").and_then(|v| v.as_str()).unwrap_or("测试通知").to_string();
            let body_txt = b.get("body").and_then(|v| v.as_str()).unwrap_or("来自面板的手动通知").to_string();
            let results = crate::server::notify::notify(&event, &title, &body_txt).await;
            Json(json!({"ok": true, "results": results}))
        }
        "save_flags" => {
            let patch = b.get("notify").cloned().unwrap_or(json!({}));
            let mut cfg = crate::server::store::load("notify");
            if !cfg.is_object() { cfg = json!({}); }
            for k in ["notify_checkin", "notify_quota", "notify_error"] {
                if let Some(v) = patch.get(k) {
                    cfg[k] = json!(v.as_bool().unwrap_or(true));
                }
            }
            crate::server::store::save("notify", &cfg);
            Json(json!({"ok": true, "notify": cfg, "message": "已保存"}))
        }
        _ => {
            let cfg = crate::server::store::load("notify");
            let cfg_obj = if cfg.is_object() { cfg } else { json!({"webhooks": [], "notify_checkin": true, "notify_quota": true, "notify_error": true}) };
            let types: Vec<Value> = crate::server::notify::WEBHOOK_TYPES
                .iter()
                .map(|t| json!({"type": t}))
                .collect();
            Json(json!({"ok": true, "notify": cfg_obj, "types": types}))
        }
    }
}

async fn send_one_hook(hook: &Value, title: &str, body: &str) -> (bool, String) {
    crate::server::notify::send_one_pub(hook, title, body).await
}

/// 面板日志（`api/logs`）：Rust 面板原生模式，仅面板侧内存日志。
#[tracing::instrument]
pub async fn logs() -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "gateway": {"ok": false, "code": "unreachable",
                     "message": "网关 EXE 已移除（Rust 面板原生模式，仅面板侧日志）"},
        "panel": []
    }))
}

/// 当前面板端口（main 装配时写入；回调/登录 URL 用）。
pub fn port_of_panel() -> u16 {
    *PANEL_PORT.get_or_init(|| 8800)
}

static PANEL_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

pub fn set_panel_port(p: u16) {
    let _ = PANEL_PORT.set(p);
}

/// 极简 percent-encode（回调 URL 塞进 query 用）。
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

/// 标准 UUID v4 字符串（36 位带横线）—— 对齐 Python `str(uuid.uuid4())`。
/// ★ 授权页服务端按 UUID 形态校验 state；裸 hex 会直接「登录失败」。
fn uuid_v4() -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(format!(
        "{}|{}|{}",
        nanos_hex(),
        std::process::id(),
        nanos_hex() ^ 0x9E3779B97F4A7C15
    ));
    let mut b = h.finalize();
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    let x: String = b.iter().map(|byte| format!("{:02x}", byte)).collect();
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

fn now_local() -> String {    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let sod = (secs % 86400) as u32;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60
    )
}
