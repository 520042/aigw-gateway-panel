use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use super::util::*;

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
        "" => {
            // ★ 2026-10-15：裸 GET /api/models 原来固定回空 models_enriched →
            //   接入源页 KPI「收录模型」恒为 0（Python 版这栏是内置模型表计数）。
            //   这里由 catalog.grouped 展平成 models_enriched（对齐 Python 形状）。
            let disabled = st.inner.disabled.read().await.clone();
            let g = st.inner.catalog.grouped(&disabled);
            let mut rows: Vec<Value> = Vec::new();
            if let Some(groups) = g.get("groups").and_then(|v| v.as_array()) {
                for grp in groups {
                    let gname = grp.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if let Some(ms) = grp.get("models").and_then(|v| v.as_array()) {
                        for m in ms {
                            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            if id.is_empty() {
                                continue;
                            }
                            rows.push(json!({
                                "id": id,
                                "name": m.get("name").and_then(|v| v.as_str()).unwrap_or(id),
                                "credits": m.get("credits").cloned().unwrap_or(json!("")),
                                "group": gname,
                                "on": m.get("on").and_then(|v| v.as_bool()).unwrap_or(true),
                                "rate_source": "内置表",
                                "availableAccounts": 0,
                                "cost": "未观测",
                            }));
                        }
                    }
                }
            }
            // credits 可能是字符串也可能是数字 —— 非空即视为「有倍率」
            let has_rate = |r: &Value| -> bool {
                match r.get("credits") {
                    Some(Value::String(s)) => !s.is_empty(),
                    Some(Value::Null) | None => false,
                    Some(_) => true,
                }
            };
            let with_rate = rows.iter().filter(|r| has_rate(r)).count();
            let total = rows.len();
            Json(json!({
                "ok": true,
                "models_enriched": rows,
                "rate_summary": {"total": total, "with_rate": with_rate, "gateway_offline": true},
            }))
        }
        _ => Json(json!({
            "ok": false, "code": "unknown_action",
            "message": format!("models action='{}' 尚未实现", action)
        })),
    }
}

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

            // ★ Loomy：有 session → 在线拉真实模型目录（OpenAI 兼容 /api/v1/models）
            if pid == "apk-loomy" {
                if let Some(sec) = st.inner.login.secrets_for("apk-loomy").first().cloned() {
                    let rows = crate::relay::loomy::model_rows(&sec, 15).await;
                    if !rows.is_empty() {
                        let n = rows.len();
                        let mut cache = crate::server::store::load("platform_models");
                        if !cache.is_object() { cache = json!({}); }
                        cache[pid] = json!({
                            "models": rows, "live": true,
                            "ts": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
                        });
                        crate::server::store::save("platform_models", &cache);
                        return Json(json!({
                            "ok": true, "platform": pid, "name": "Loomy（在线目录）",
                            "models": rows, "live": true,
                            "note": format!("实时拉取 {} 个模型（session 有效）", n),
                        }));
                    }
                }
            }

            // ★ 元宝：有 Cookie → **实时**拉真实模型清单（/api/agent/model/list）
            if pid == "apk-yuanbao" {
                if let Some(sec) = st.inner.login.secrets_for("apk-yuanbao").first().cloned() {
                    let rows = crate::relay::yuanbao::model_rows(&sec, 20).await;
                    if !rows.is_empty() {
                        let n = rows.len();
                        let mut cache = crate::server::store::load("platform_models");
                        if !cache.is_object() {
                            cache = json!({});
                        }
                        cache[pid] = json!({
                            "models": rows, "live": true, "ts": now_ts(),
                        });
                        crate::server::store::save("platform_models", &cache);
                        return Json(json!({
                            "ok": true, "platform": pid, "name": "元宝（账号实时清单）",
                            "models": rows, "live": true,
                            "note": format!("已从账号实时拉取 {} 个模型并缓存", n),
                        }));
                    }
                }
            }

            // ★ 2026-10-15（用户要求「所有平台的模型都要实时从账号拉」）：
            //   数据驱动通用分支 —— 有凭据 + catalog 声明了 models 动作的平台都在这里实时拉。
            if let Some(rows) = live_models_generic(&st, pid).await {
                let n = rows.len();
                let mut cache = crate::server::store::load("platform_models");
                if !cache.is_object() {
                    cache = json!({});
                }
                cache[pid] = json!({"models": rows, "live": true, "ts": now_ts()});
                crate::server::store::save("platform_models", &cache);
                return Json(json!({
                    "ok": true, "platform": pid,
                    "name": format!("{}（账号实时清单）", pid),
                    "models": rows, "live": true,
                    "note": format!("已从账号实时拉取 {} 个模型并缓存", n),
                }));
            }

            // ★ 2026-10-15（用户要求「获取新的模型清单要保存、下次直接调用」）：
            //   实时拉不到时，**先用上次实时拉取的缓存**，而不是一上来就给内置清单。
            if let Some(entry) = crate::server::store::load("platform_models").get(pid) {
                if entry.get("live").and_then(|v| v.as_bool()) == Some(true) {
                    let rows = entry.get("models").cloned().unwrap_or(json!([]));
                    let n = rows.as_array().map(|a| a.len()).unwrap_or(0);
                    if n > 0 {
                        let ts = entry.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let ago = if ts > 0.0 {
                            let mins = ((now_ts() - ts) / 60.0).max(0.0);
                            if mins < 1.0 { "刚刚".to_string() }
                            else if mins < 60.0 { format!("{:.0} 分钟前", mins) }
                            else { format!("{:.0} 小时前", mins / 60.0) }
                        } else { "未知时间".to_string() };
                        return Json(json!({
                            "ok": true, "platform": pid,
                            "name": format!("{}（缓存清单）", pid),
                            "models": rows, "live": true,
                            "note": format!("用上次实时拉取的 {} 个模型（{}）；点「刷新」可重新拉取", n, ago),
                        }));
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
                    format!(
                        "内置的 {} 个模型（未取到账号实时清单：可能未登录或接口失败；登录后点「刷新」可拉真实清单）",
                        n
                    ),
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
            // ★ 2026-10-15：**只在没有实时清单时**才落盘 —— 否则会把上次实时拉取的清单
            //   覆盖成内置静态清单（用户明确要求「实时清单要保存、别被覆盖」）。
            let existing_live = crate::server::store::load("platform_models")
                .get(pid)
                .and_then(|e| e.get("live"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut cache = crate::server::store::load("platform_models");
            if !cache.is_object() {
                cache = json!({});
            }
            if !existing_live {
                cache[pid] = json!({
                    "models": models,
                    "live": false,
                    "ts": now_ts(),
                });
                crate::server::store::save("platform_models", &cache);
            }
            Json(json!({
                "ok": true,
                "platform": pid,
                "name": name,
                "models": models,
                "live": false,
                "note": note,
            }))
        }
        "test_model" => {
            let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            Json(test_model(&st, &model).await)
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

/// ★ 2026-10-15（用户要求「所有平台的模型都要实时从账号拉」）：
/// **数据驱动**的通用实时拉取 —— 只要该平台有凭据、且 catalog.gwextra 里声明了
/// `models` 动作（base + path），就用它的鉴权头去拉，再用通用提取器解析。
/// 拉不到返回 None（调用方回退缓存 / 内置清单）。
async fn live_models_generic(st: &AppState, pid: &str) -> Option<Vec<Value>> {
    let (secret, typ) = st.inner.login.secret_and_type(pid)?;
    let g = st.inner.catalog.raw_get("gwextra")?;
    let top_base = g
        .get("base")
        .and_then(|b| b.as_object())
        .and_then(|b| b.get(pid))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let acts = g.get("actions")?.get(pid)?.as_array()?;
    // 找声明了模型清单的动作用（path 里带 model）
    let act = acts.iter().find(|a| {
        a.get("path")
            .and_then(|p| p.as_str())
            .map(|p| p.to_lowercase().contains("model"))
            .unwrap_or(false)
    })?;
    let base = act
        .get("base")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(top_base)?;
    let path = act.get("path")?.as_str()?;
    if path.is_empty() {
        return None;
    }
    let method = act
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("GET")
        .to_uppercase();
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .ok()?;
    // ★ 动作若声明了 body（或 method=POST），就按 JSON POST 发（有些平台必须带 body 才回全量）
    let body_spec = act.get("body").cloned();
    let mut req = if method == "POST" || body_spec.is_some() {
        client.post(&url).json(&body_spec.unwrap_or_else(|| json!({})))
    } else {
        client.get(&url)
    };
    req = req
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
        )
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", format!("{}/", base));
    // 鉴权：cookie 类走 Cookie 头，其余走 Bearer
    req = if typ.eq_ignore_ascii_case("cookie") {
        req.header("Cookie", secret)
    } else {
        req.header("Authorization", format!("Bearer {}", secret))
    };
    // 平台专属请求头（不覆盖鉴权头）
    if let Some(vh) = g
        .get("vendor_headers")
        .and_then(|v| v.get(pid))
        .and_then(|v| v.as_object())
    {
        for (k, v) in vh {
            let kl = k.to_lowercase();
            if kl == "authorization" || kl == "cookie" {
                continue;
            }
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }
    }
    let r = req.send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: Value = r.json().await.ok()?;
    let rows = crate::server::model_extract::extract_generic(&v);
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// 连接测试（对齐 Python api_models test_model）：对单个模型发一条极小真实请求，
/// 回 {ok, model, latency_ms, via, reply, error}。这是真实调用（消耗极少量额度）。
async fn test_model(st: &AppState, model: &str) -> Value {
    let t0 = std::time::Instant::now();
    let msgs = json!([{"role": "user", "content": "ping"}]);
    let msgs_arr: Vec<Value> = msgs.as_array().cloned().unwrap_or_default();
    let fin = |okk: bool, via: &str, reply: &str, err: &str| -> Value {
        json!({
            "ok": okk, "model": model,
            "latency_ms": t0.elapsed().as_millis() as i64,
            "via": via,
            "reply": reply.chars().take(120).collect::<String>(),
            "error": if okk { Value::Null } else { json!(err.chars().take(300).collect::<String>()) },
        })
    };
    let reply_of = |p: &Value| -> String {
        p.pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    if model.is_empty() {
        return json!({"ok": false, "message": "model 必填"});
    }
    let (bare, pid) = match model.rsplit_once('@') {
        Some((b, p)) => (b.to_string(), p.to_string()),
        None => (model.to_string(), String::new()),
    };
    // 1) 网页反代族
    if !pid.is_empty() && crate::relay::web::is_web_pid(&pid) {
        let secrets = st.inner.login.secrets_for(&pid);
        if secrets.is_empty() {
            return fin(false, &pid, "", "需要先在「接入源」登录获取凭据");
        }
        let dm = if bare.is_empty() {
            crate::relay::web::default_model(&pid).to_string()
        } else {
            bare.clone()
        };
        return match crate::relay::web::chat(&pid, &msgs_arr, &secrets[0], &dm, 45).await {
            Ok(p) => fin(true, &pid, &reply_of(&p), ""),
            Err((s, m)) => fin(false, &pid, "", &format!("HTTP {}：{}", s, m)),
        };
    }
    // 2) 原生 @pid
    if !pid.is_empty() {
        let secrets = st.inner.login.secrets_for(&pid);
        if secrets.is_empty() {
            return fin(false, &pid, "", "需要先在「接入源」登录获取凭据");
        }
        if pid == "apk-doubao" {
            let dm = if bare.is_empty() { "doubao-pro".to_string() } else { bare.clone() };
            return match crate::relay::doubao::chat(&msgs_arr, &secrets[0], &dm, 45).await {
                Ok(p) => fin(true, &pid, &reply_of(&p), ""),
                Err((s, m)) => fin(false, &pid, "", &format!("HTTP {}：{}", s, m)),
            };
        }
        if pid == "apk-yuanbao" {
            let dm = if bare.is_empty() { "deepseek".to_string() } else { bare.clone() };
            return match crate::relay::yuanbao::chat(&msgs_arr, &secrets[0], &dm, 45).await {
                Ok(p) => fin(true, &pid, &reply_of(&p), ""),
                Err((s, m)) => fin(false, &pid, "", &format!("HTTP {}：{}", s, m)),
            };
        }
        if pid == "apk-loomy" {
            let dm = if bare.is_empty() {
                crate::relay::loomy::default_model().to_string()
            } else {
                bare.clone()
            };
            return match crate::relay::loomy::chat(&msgs_arr, &secrets[0], &dm, 45).await {
                Ok(p) => fin(true, &pid, &reply_of(&p), ""),
                Err((s, m)) => fin(false, &pid, "", &format!("HTTP {}：{}", s, m)),
            };
        }
        // wb-gateway 族（copilot 系只收流式）
        if pid.starts_with("wb-gateway") {
            return test_wb(&pid, &bare, model, t0, &secrets[0]).await;
        }
        // 通用 OpenAI 兼容原生中继
        if let Some(spec) = st
            .inner
            .catalog
            .relay_spec(&pid)
            .cloned()
            .and_then(|v| crate::relay::native::NativeSpec::from_catalog(&v))
        {
            let body = json!({
                "model": if bare.is_empty() { "default" } else { bare.as_str() },
                "messages": msgs, "stream": false,
            });
            return match crate::relay::native::chat(&spec, body.to_string().as_bytes(), &secrets[0], 45).await {
                Ok(p) => fin(true, &pid, &reply_of(&p), ""),
                Err((s, m)) => fin(false, &pid, "", &format!("HTTP {}：{}", s, m)),
            };
        }
        return fin(false, &pid, "", "未知中继平台");
    }
    // 3) 裸名豆包模型
    if crate::relay::doubao::is_doubao_model(&bare) {
        let secrets = st.inner.login.secrets_for("apk-doubao");
        if secrets.is_empty() {
            return fin(false, "apk-doubao", "", "需要先在「接入源」登录豆包凭据");
        }
        return match crate::relay::doubao::chat(&msgs_arr, &secrets[0], &bare, 45).await {
            Ok(p) => fin(true, "apk-doubao", &reply_of(&p), ""),
            Err((s, m)) => fin(false, "apk-doubao", "", &format!("HTTP {}：{}", s, m)),
        };
    }
    // 4) auto / free / 裸名兜底 → Pollinations 免费车道
    if model == "auto" || model == "free" || bare == "auto" || bare == "free" {
        let url = format!("{}{}", crate::relay::POLLINATIONS_BASE, crate::relay::POLLINATIONS_PATH);
        let body = json!({"model": "gpt-oss-20b", "messages": msgs, "stream": false});
        let client = reqwest::Client::new();
        return match client
            .post(&url)
            .timeout(std::time::Duration::from_secs(45))
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
        {
            Ok(r) => {
                let status = r.status().as_u16();
                let okk = r.status().is_success();
                let txt = r.text().await.unwrap_or_default();
                let p: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
                if okk {
                    fin(true, "free-lane", &reply_of(&p), "")
                } else {
                    fin(false, "free-lane", "", &format!("HTTP {}：{}", status, txt))
                }
            }
            Err(e) => fin(false, "free-lane", "", &format!("{}", e)),
        };
    }
    json!({"ok": false, "message": format!("未知模型：{}（可用清单见「路由与模型 → 模型与倍率」）", model)})
}

/// wb-gateway 族连接测试：copilot 系上游只收流式，强制 stream=true 后聚合 SSE。
async fn test_wb(pid: &str, bare: &str, model: &str, t0: std::time::Instant, secret: &str) -> Value {
    let base = if pid == "wb-gateway-intl" {
        "https://www.codebuddy.ai"
    } else {
        "https://copilot.tencent.com"
    };
    let body = json!({
        "model": if bare.is_empty() { "auto" } else { bare },
        "messages": [{"role": "user", "content": "ping"}],
        "stream": true,
    });
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v2/chat/completions", base))
        .timeout(std::time::Duration::from_secs(45))
        .header("Authorization", format!("Bearer {}", secret))
        .header("Content-Type", "application/json")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
        .body(body.to_string())
        .send()
        .await;
    let fin = |okk: bool, reply: &str, err: &str| -> Value {
        json!({ "ok": okk, "model": model, "latency_ms": t0.elapsed().as_millis() as i64,
                "via": pid, "reply": reply.chars().take(120).collect::<String>(),
                "error": if okk { Value::Null } else { json!(err.chars().take(300).collect::<String>()) } })
    };
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            let txt = r.text().await.unwrap_or_default();
            let payload = if ct.contains("event-stream") || txt.trim_start().starts_with("data:") {
                crate::relay::native::aggregate_openai_sse(&txt).unwrap_or(Value::Null)
            } else {
                serde_json::from_str::<Value>(&txt).unwrap_or(Value::Null)
            };
            let reply = payload.pointer("/choices/0/message/content").and_then(|v| v.as_str()).unwrap_or("");
            if (200..300).contains(&status) {
                fin(true, reply, "")
            } else {
                fin(false, "", &format!("HTTP {}：{}", status, txt.chars().take(200).collect::<String>()))
            }
        }
        Err(e) => fin(false, "", &format!("{}", e)),
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
