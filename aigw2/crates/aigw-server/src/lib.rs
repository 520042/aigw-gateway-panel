//! aigw-server：axum 路由 + rust-embed 内嵌前端 + 中继接线。
//!
//! M1 契约面：
//! - `POST /v1/chat/completions`：真实转发（免密车道，OpenAI 同协议透传）；
//!   pool 选号 → relay 转发 → usage 记账 → SSE 流式/非流式响应。
//! - `GET /v1/models`：静态清单 + live 拉取（60s 缓存，失败回退静态）。
//! - `GET /api/usage`：用量汇总（store）。
//! - `GET /api/health`、`GET /*`（rust-embed 静态前端）。
//!
//! 路由策略 M1 为单平台（唯一启用 spec 接管全部模型）；M2 引入
//! 模型→平台映射与 fallback 链（dsh combo 降级）。

use aigw_core::ErrorClass;
use aigw_pool::Pool;
use aigw_relay::auth_login::LoginEngine;
use aigw_relay::{OutClient, StreamStat};
use aigw_registry::{AuthKind, ProviderSpec};
use aigw_store::{AccountStore, CustomStore, FreeStore, Store, UsageEvent};
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 前端静态资源。`$CARGO_MANIFEST_DIR/../../web` = workspace 根的 `web/`。
/// debug 构建默认动态读盘（改文件刷新即生效），release 构建内嵌进二进制。
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web"]
struct Web;

#[derive(Clone)]
pub struct AppState {
    pub specs: Vec<ProviderSpec>,
    pub store: Arc<Store>,
    pub pool: Arc<Pool>,
    pub client: Arc<OutClient>,
    pub accounts: Arc<AccountStore>,
    /// 自定义 API 存储（接入源页「自定义 API」动态并入 specs）。
    pub custom: Arc<CustomStore>,
    /// 公益站聚合存储（接入源页「公益站」，手动/每日刷新）。
    pub free: Arc<FreeStore>,
    /// 登录引擎（批2：设备授权/扫码 → token 自动落池）
    pub login: Arc<LoginEngine>,
    /// 会话粘性路由（对齐 workbuddy2api StickyRouter + loomy「最后一条 user」修正）
    pub sticky: Arc<aigw_pool::StickyRouter>,
    /// live 模型缓存：(时间, (spec_id, model) 索引, 响应体)
    pub models_cache: Arc<Mutex<Option<(Instant, Vec<(String, String)>, Value)>>>,
}

impl AppState {
    /// 组装运行态（main 用）。
    pub fn new(
        specs: Vec<ProviderSpec>,
        store: Arc<Store>,
        pool: Arc<Pool>,
        client: Arc<OutClient>,
        accounts: Arc<AccountStore>,
        custom: Arc<CustomStore>,
        free: Arc<FreeStore>,
    ) -> AppState {
        AppState {
            specs,
            store,
            pool,
            client,
            accounts,
            custom,
            free,
            login: Arc::new(LoginEngine::new()),
            sticky: Arc::new(aigw_pool::StickyRouter::new()),
            models_cache: Arc::new(Mutex::new(None)),
        }
    }

    /// 全量平台 = 文件 spec + 自定义 API + 公益站（动态并进）。
    pub fn all_specs(&self) -> Vec<ProviderSpec> {
        let mut v = self.specs.clone();
        for c in self.custom.rows() {
            v.push(custom_to_spec(&c));
        }
        for s in self.free.rows() {
            v.push(free_to_spec(&s));
        }
        v
    }

    /// 按模型名选平台：① `模型id@平台` 后缀直指定（旧版语义）② 静态清单声明
    /// ③ live 缓存索引 ④ 兜底首个启用。自定义 API 动态并入。
    fn spec_for_model(&self, model: &str) -> Option<ProviderSpec> {
        let all = self.all_specs();
        if let Some((_, plat)) = model.rsplit_once('@') {
            if let Some(s) = all.iter().find(|s| s.enabled && &s.id == plat) {
                return Some(s.clone());
            }
        }
        if let Some(s) = all
            .iter()
            .find(|s| s.enabled && s.models.iter().any(|m| m == model))
        {
            return Some(s.clone());
        }
        if let Ok(g) = self.models_cache.lock() {
            if let Some((_, index, _)) = g.as_ref() {
                if let Some((sid, _)) = index.iter().find(|(_, m)| m == model) {
                    if let Some(s) = all.iter().find(|s| s.enabled && &s.id == sid) {
                        return Some(s.clone());
                    }
                }
            }
        }
        all.iter().find(|s| s.enabled).cloned()
    }
}

/// 自定义 API → 声明式 ProviderSpec（auth=api_key；Anthropic 协议走 anthropic 透传）。
fn custom_to_spec(c: &aigw_store::CustomApi) -> ProviderSpec {
    ProviderSpec {
        id: c.id.clone(),
        name: c.name.clone(),
        enabled: c.enabled,
        auth: AuthKind::ApiKey,
        base_url: c.base_url.clone(),
        format: c.format.clone(),
        models: c.models.clone(),
        models_policy: aigw_registry::ModelsPolicy::Live,
        stream_dialect: match c.format.as_str() {
            "anthropic" => aigw_registry::StreamDialect::AnthropicSse,
            _ => aigw_registry::StreamDialect::SseDelta,
        },
        headers: Default::default(),
        chat_path: String::new(),
        models_path: String::new(),
        models_filter: String::new(),
        upstream_kind: "openai".into(),
        signer: String::new(),
        biz_err: Vec::new(),
        hosts: Vec::new(),
        login_url: String::new(),
        notes: String::new(),
    }
}

/// 公益站 → 声明式 ProviderSpec（auth=api_key；openai 兼容 /v1）。
fn free_to_spec(s: &aigw_store::FreeSite) -> ProviderSpec {
    let base = s.base_url.trim_end_matches('/').to_string();
    ProviderSpec {
        id: s.id.clone(),
        name: s.name.clone(),
        enabled: s.enabled,
        auth: AuthKind::ApiKey,
        base_url: format!("{base}/v1"),
        format: "openai".into(),
        models: Vec::new(),
        models_policy: aigw_registry::ModelsPolicy::Live,
        stream_dialect: aigw_registry::StreamDialect::SseDelta,
        headers: Default::default(),
        chat_path: String::new(),
        models_path: String::new(),
        models_filter: String::new(),
        upstream_kind: "openai".into(),
        signer: String::new(),
        biz_err: Vec::new(),
        hosts: Vec::new(),
        login_url: s.url.clone(),
        notes: s.desc.clone(),
    }
}

pub fn build_app(st: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/usage", get(usage))
        .route("/api/requests", get(requests))
        .route("/api/platforms", get(platforms))
        .route("/api/custom", get(custom_list).post(custom_action))
        .route("/api/free", get(free_list).post(free_action))
        .route("/api/proxy", get(proxy_get).post(proxy_set))
        .route("/api/accounts", get(accounts))
        .route("/api/pool", get(pool_snapshot))
        .route("/api/auth", post(auth_action))
        .route("/api/checkin", get(checkin_status_all).post(checkin_claim_all))
        .route("/api/backup", get(backup_export).post(backup_restore))
        .route("/api/update-check", get(update_check))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .fallback(web_file)
        .with_state(st)
}

/// 平台清单（声明式 spec 快照；含自定义 API 动态并入；base_url 含占位符时标注）。
async fn platforms(State(st): State<AppState>) -> Json<Value> {
    let specs = st.all_specs();
    let custom_rows = st.custom.rows();
    let rows: Vec<Value> = specs
        .iter()
        .map(|s| {
            let acct_n = st.accounts.secrets_for(&s.id).len();
            let is_custom = s.id.starts_with("custom-");
            let key_mask = if is_custom {
                custom_rows
                    .iter()
                    .find(|c| c.id == s.id)
                    .map(|c| {
                        if c.api_key.len() >= 8 {
                            format!("{}…{}", &c.api_key[..4], &c.api_key[c.api_key.len() - 4..])
                        } else {
                            "••••".to_string()
                        }
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            json!({
                "id": s.id, "name": s.name, "auth": s.auth,
                "enabled": s.enabled,
                "upstream_kind": s.upstream_kind,
                "models_n": s.models.len(),
                "models": s.models,
                "models_policy": s.models_policy,
                "placeholder": s.base_url.starts_with('@'),
                "accounts_n": acct_n,
                "login_url": s.login_url,
                "notes": s.notes,
                "chat_path": s.chat_path(),
                "custom": is_custom,
                "key_mask": key_mask,
            })
        })
        .collect();
    Json(json!({"total": rows.len(), "rows": rows}))
}

/// 自定义 API 列表（掩码 key）。
async fn custom_list(State(st): State<AppState>) -> Json<Value> {
    Json(st.custom.snapshot())
}

/// 自定义 API 写操作：
///   {"action":"add","name","format","base_url","api_key"}
///   {"action":"update","id","name?","format?","base_url?","api_key?"}
///   {"action":"remove","id"}
///   {"action":"sync_models","id","models":[..]}
async fn custom_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "add" => {
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let format = v.get("format").and_then(|x| x.as_str()).unwrap_or("openai");
            let base_url = v.get("base_url").and_then(|x| x.as_str()).unwrap_or("");
            let api_key = v.get("api_key").and_then(|x| x.as_str()).unwrap_or("");
            match st.custom.add(name, format, base_url, api_key) {
                Ok(c) => Json(json!({"ok": true, "api": c})).into_response(),
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        "update" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let format = v.get("format").and_then(|x| x.as_str()).unwrap_or("");
            let base_url = v.get("base_url").and_then(|x| x.as_str()).unwrap_or("");
            let api_key = v.get("api_key").and_then(|x| x.as_str()).unwrap_or("");
            match st.custom.update(id, name, format, base_url, api_key) {
                Ok(c) => Json(json!({"ok": true, "api": c})).into_response(),
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        "remove" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let removed = st.custom.remove(id);
            Json(json!({"ok": removed, "message": if removed { "已删除" } else { "条目不存在" }})).into_response()
        }
        "sync_models" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let models: Vec<String> = v
                .get("models")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            match st.custom.set_models(id, models) {
                Ok(()) => Json(json!({"ok": true, "message": "模型清单已同步"})).into_response(),
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{action}")}))).into_response(),
    }
}

// ---- 公益站聚合（手动刷新 + 每日自动） ----

/// 聚合源（GitHub Pages 静态导航，页面内嵌 JSON-LD itemListElement：name+url）。
const GONGYI_SOURCES: &[&str] = &["https://bubblevv.github.io/ai-api-gongyi-nav/"];

/// JSON-LD ListItem（聚合页内嵌）。
#[derive(serde::Deserialize)]
struct GongyiItem {
    name: String,
    #[serde(default)]
    url: String,
}

/// 从 host 提取 id 用 slug（去 www.，非法字符转 -）。
fn slug_of(host: &str) -> String {
    let h = host.trim_start_matches("www.");
    let mut s = String::new();
    for ch in h.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            s.push(ch);
        } else {
            s.push('-');
        }
    }
    s.trim_matches('-').to_string()
}

/// 从 URL 提取 host（https://host/path?a=b → host）。
fn host_of(u: &str) -> Option<String> {
    let rest = u.split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.to_string())
}

/// 抓取聚合源并解析出站点列表（按 host 去重，首个保留）。
pub async fn fetch_gongyi_sites(client: &aigw_relay::OutClient) -> Result<Vec<aigw_store::FreeSite>, String> {
    let url = GONGYI_SOURCES[0];
    let resp = client.http()
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("抓取聚合源失败：{e}"))?;
    let html = resp
        .text()
        .await
        .map_err(|e| format!("读取聚合源响应失败：{e}"))?;

    // 提取 "itemListElement": [ ... ]（括号配对，兼容内部 JSON 结构）
    let start = html
        .find("\"itemListElement\"")
        .ok_or("聚合页未找到 itemListElement")?;
    let rest = &html[start + "\"itemListElement\"".len()..];
    let colon = rest.find(':').ok_or("itemListElement 缺冒号")?;
    let arr = &rest[colon + 1..];
    let arr = arr.trim_start();
    if !arr.starts_with('[') {
        return Err("itemListElement 不是数组".into());
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = 0usize;
    for (i, ch) in arr.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match ch {
            '\\' if in_str => esc = true,
            '"' => in_str = !in_str,
            '[' => {
                if !in_str {
                    depth += 1
                }
            }
            ']' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    if end == 0 {
        return Err("itemListElement 数组未闭合".into());
    }
    let items: Vec<GongyiItem> = serde_json::from_str(&arr[..end])
        .map_err(|e| format!("解析站点列表失败：{e}"))?;

    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<aigw_store::FreeSite> = Vec::new();
    for it in items {
        let u = it.url.trim().to_string();
        if u.is_empty() {
            continue;
        }
        let Some(host) = host_of(&u) else { continue };
        if seen.contains(&host) {
            continue;
        }
        seen.push(host.clone());
        let slug = slug_of(&host);
        if slug.is_empty() {
            continue;
        }
        out.push(aigw_store::FreeSite {
            id: format!("free-{slug}"),
            name: it.name.trim().to_string(),
            base_url: format!("https://{host}"),
            url: u,
            desc: String::new(),
            enabled: true,
        });
    }
    Ok(out)
}

/// 刷新公益站聚合：抓取 → 解析 → 合并（已存在保留）。
/// 返回 (聚合总数, 新增数, 保留数)。
pub async fn refresh_free_sites(
    client: &aigw_relay::OutClient,
    free: &aigw_store::FreeStore,
) -> Result<(usize, usize, usize), String> {
    let sites = fetch_gongyi_sites(client).await?;
    let (added, kept) = free.upsert_batch(sites.clone());
    Ok((sites.len(), added, kept))
}

/// GET /api/free → 公益站条目列表。
async fn free_list(State(st): State<AppState>) -> Json<Value> {
    let rows = st.free.rows();
    let list: Vec<Value> = rows
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "name": s.name, "base_url": s.base_url,
                "url": s.url, "desc": s.desc, "enabled": s.enabled,
            })
        })
        .collect();
    Json(json!({"total": list.len(), "rows": list}))
}

/// POST /api/free：
///   {"action":"refresh"}            手动刷新聚合源
///   {"action":"remove","id":"free-…"} 删除站点
async fn free_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "refresh" => {
            match refresh_free_sites(&st.client, &st.free).await {
                Ok((total, added, kept)) => Json(json!({
                    "ok": true, "total": total, "added": added, "kept": kept,
                    "message": format!("刷新完成：共 {total} 站，新增 {added}，保留 {kept}（已添加的不会丢失）"),
                }))
                .into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        "remove" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let removed = st.free.remove(id);
            Json(json!({"ok": removed, "message": if removed { "已删除" } else { "条目不存在" }})).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{action}")}))).into_response(),
    }
}

/// 出站代理配置：GET /api/proxy → 当前配置（mode/url）。
///   POST /api/proxy {"mode":"direct"|"global"|"bypass_cn","url":"http://127.0.0.1:7890"}
///   保存 data/proxy.json 并热更新全局（后续所有上游/登录请求立即生效，无需重启）。
async fn proxy_get() -> Json<Value> {
    let cfg = aigw_relay::proxy::current();
    Json(json!({
        "ok": true,
        "mode": cfg.mode,
        "url": cfg.url,
        "modes": ["direct", "global", "bypass_cn"],
    }))
}

async fn proxy_set(body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("");
    if !["direct", "global", "bypass_cn"].contains(&mode) {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "mode 必须是 direct | global | bypass_cn"}))).into_response();
    }
    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if mode != "direct" && url.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "代理模式需要 url（如 http://127.0.0.1:7890）"}))).into_response();
    }
    let cfg = aigw_relay::proxy::ProxyConfig { mode: mode.into(), url };
    match aigw_relay::proxy::save_and_apply(&aigw_relay::proxy::default_data_dir(), &cfg) {
        Ok(()) => Json(json!({"ok": true, "message": format!("代理已切换为 {}，所有流程即时生效", mode), "cfg": cfg})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "message": format!("保存失败：{e}")}))).into_response(),
    }
}

async fn accounts(State(st): State<AppState>) -> Json<Value> {
    Json(st.accounts.rows_snapshot())
}

async fn pool_snapshot(State(st): State<AppState>) -> Json<Value> {
    Json(st.pool.snapshot())
}

/// wb 族每日签到：GET /api/checkin（查全部 wb 账号状态）/ POST /api/checkin（全部领取）。
/// 一手：workbuddy2api-master admin/backend.py:338-368（Buddy 加油站每日 100 积分）。
async fn checkin_status_all(State(st): State<AppState>) -> Json<Value> {
    Json(checkin_all(&st, false).await)
}

async fn checkin_claim_all(State(st): State<AppState>) -> Json<Value> {
    Json(checkin_all(&st, true).await)
}

async fn checkin_all(st: &AppState, claim: bool) -> Value {
    let http = aigw_relay::auth_login::auth_http();
    let mut results: Vec<Value> = Vec::new();
    for platform in ["wb-gateway", "codebuddy"] {
        let Some(base) = aigw_relay::wb::wb_checkin_base(platform) else { continue };
        for (acc_id, secret) in st.accounts.secrets_for(platform) {
            let r = if claim {
                aigw_relay::wb::claim_daily_checkin(&http, base, &secret).await
            } else {
                aigw_relay::wb::checkin_status(&http, base, &secret).await
            };
            results.push(json!({
                "platform": platform,
                "account": acc_id,
                "action": if claim { "claim" } else { "status" },
                "ok": r.is_ok(),
                "data": r.unwrap_or_else(|e| json!({"error": e})),
            }));
        }
    }
    json!({"ok": true, "claim": claim, "results": results, "count": results.len()})
}

/// 备份导出（M5 交付链，对齐 cline2api tmp+rename 数据纪律）：GET /api/backup
/// 扫描 data 目录全部运行态文件（*.json + panel.db），打包为 JSON 数组供前端下载/保存。
async fn backup_export() -> Json<Value> {
    let dd = aigw_relay::proxy::default_data_dir();
    let mut files: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dd) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let ok_ext = name.ends_with(".json") || name == "panel.db";
            if !ok_ext {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&p) {
                files.push(json!({"name": name, "bytes": base64_encode(&bytes), "size": bytes.len()}));
            }
        }
    }
    files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Json(json!({
        "ok": true,
        "ts": now_ms(),
        "dir": dd.to_string_lossy(),
        "files": files,
        "count": files.len(),
    }))
}

/// 备份恢复（先写 .bak-<ts> 旧文件再覆盖，损坏不静默）：POST /api/backup
///   {"files":[{"name":"accounts.json","bytes":"<b64>"}]}
async fn backup_restore(_st: State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let Some(files) = v.get("files").and_then(|x| x.as_array()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 files 数组"}))).into_response();
    };
    let dd = aigw_relay::proxy::default_data_dir();
    let ts = now_ms();
    let mut restored = 0usize;
    let mut skipped = Vec::new();
    for f in files {
        let Some(name) = f.get("name").and_then(|x| x.as_str()) else { continue };
        // 只允许受控文件名，防路径穿越
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            skipped.push(format!("{name}（非法名跳过）"));
            continue;
        }
        let Some(b64) = f.get("bytes").and_then(|x| x.as_str()) else { continue };
        let bytes = match base64_decode(b64) {
            Some(b) => b,
            None => {
                skipped.push(format!("{name}（b64 解码失败跳过）"));
                continue;
            }
        };
        let target = dd.join(name);
        if target.exists() {
            let _ = std::fs::rename(&target, dd.join(format!(".bak-{ts}-{name}")));
        }
        if std::fs::write(&target, &bytes).is_ok() {
            restored += 1;
        }
    }
    // 恢复后重启服务即全量生效（custom/free/accounts 均以 data 目录文件为源）
    Json(json!({
        "ok": true,
        "restored": restored,
        "skipped": skipped,
        "dir": dd.to_string_lossy(),
        "note": "已恢复文件；重启服务后全量生效。旧文件保留为 .bak-<ts>-* 以便回滚。",
    }))
    .into_response()
}

/// 在线更新检查（对齐 21 号 dsh /v1/status 版本自证纪律）：GET /api/update-check
/// 本地 = 编译版本 + 部署目录时间；远端 = GitHub 仓库最新 main commit。
async fn update_check() -> Json<Value> {
    let dd = aigw_relay::proxy::default_data_dir();
    let exe_ts = std::env::current_exe()
        .ok()
        .and_then(|p| p.metadata().ok())
        .map(|m| {
            m.modified()
                .ok()
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis())
                .unwrap_or(0)
        })
        .unwrap_or(0);
    // 远端最新 commit（GitHub API，8s 超时；失败返回 offline 标记不阻断）
    let remote: Value = std::thread::spawn(|| {
        let url = "https://api.github.com/repos/520042/aigw-gateway-panel/commits/main";
        let resp = std::process::Command::new("curl")
            .args(["-s", "--max-time", "8", "-H", "User-Agent: aigw-panel", url])
            .output();
        match resp {
            Ok(o) if o.status.success() => {
                serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    })
    .join()
    .unwrap_or(Value::Null);
    let remote_sha = remote.get("sha").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let remote_date = remote
        .get("commit")
        .and_then(|c| c.get("committer"))
        .and_then(|c| c.get("date"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Json(json!({
        "ok": true,
        "local": {
            "version": env!("CARGO_PKG_VERSION"),
            "milestone": "v2-final",
            "exe_built_ts": exe_ts,
            "data_dir": dd.to_string_lossy(),
        },
        "remote": {
            "sha": remote_sha,
            "date": remote_date,
            "reachable": !remote_sha.is_empty(),
        },
        "note": "本地 Rust 工程更新：git pull 后 cargo build + 拷贝 aigw.exe 与 providers/ 覆盖部署目录。",
    }))
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// 登录会话（批2）：POST /api/auth
///   {"action":"start","platform":"wb-gateway"|"minimax"|"apk-yuanbao"|...}
///   {"action":"poll","id":"ls-..."}
///   {"action":"save","platform":"apk-yuanbao","secret":"Cookie..."}  ← 跳转登录手动粘贴闭环
/// 成功 token/cookie 自动落账号池。
async fn auth_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "start" => {
            let platform = v.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            if platform.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 platform"}))).into_response();
            }
            Json(st.login.start(&st.client, platform, &st.specs).await).into_response()
        }
        "poll" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            Json(st.login.poll(&st.client, id, &st.accounts).await).into_response()
        }
        "save" => {
            let platform = v.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            let secret = v.get("secret").and_then(|x| x.as_str()).unwrap_or("");
            if platform.is_empty() || secret.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 platform 或 secret"}))).into_response();
            }
            Json(st.login.save(platform, secret, &st.accounts)).into_response()
        }
        "callback" => {
            // 用户粘贴回调 URL（服务器场景）→ 提取 code/state 入队
            let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
            match aigw_relay::auth_login::submit_callback_url(url) {
                Ok(()) => Json(json!({"ok": true, "message": "回调已捕获，点「查询授权状态」完成入库"})).into_response(),
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{action}")}))).into_response(),
    }
}

async fn health() -> Json<Value> {
    Json(json!({
        "ok": true,
        "service": "aigw",
        "version": env!("CARGO_PKG_VERSION"),
        "milestone": "v2-final",
    }))
}

async fn usage(State(st): State<AppState>) -> Json<Value> {
    Json(st.store.usage_totals())
}

/// FNV-1a 64 位（std 实现，零新增依赖；仅作稳定压缩，不需加密强度）。
fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 会话粘性键（对齐 workbuddy2api 键优先级 + loomy2api「最后一条 user」修正）：
/// ① `metadata.conversation_id` / `conversationId` / `conversation_id` /
///    `prompt_cache_key`（02 号五键优先级）；
/// ② 都没有 → **最后一条** `role=user` 消息内容 FNV-1a。
///    为什么取最后一条：通用客户端重发全历史时首条 user 恒定，以首条为键会让
///    整个会话永久钉在同一账号（单号霸占、其余饿死）；最后一条随对话推进变化。
fn session_key(v: &Value) -> String {
    if let Some(md) = v.get("metadata").and_then(|m| m.as_object()) {
        for k in ["conversation_id", "conversationId", "conversation_id", "prompt_cache_key"] {
            if let Some(s) = md.get(k).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    for k in ["conversation_id", "conversationId"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    if let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) {
        for m in msgs.iter().rev() {
            if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                let text = m
                    .get("content")
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                return format!("u{:016x}", fnv1a(text.as_bytes()));
            }
        }
    }
    "anon".to_string()
}

/// M6 请求历史：最近 60 条 + 按平台聚合（成本归因雏形）。
async fn requests(State(st): State<AppState>) -> Json<Value> {
    let recent = st
        .store
        .usage_recent(60)
        .iter()
        .map(|e| {
            json!({
                "ts": e.ts, "platform": e.platform, "model": e.model,
                "tin": e.tin, "tout": e.tout, "ms": e.ms, "ok": e.ok,
                "upstream": e.upstream,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({"recent": recent, "by_platform": st.store.usage_by_platform()}))
}

async fn models(State(st): State<AppState>) -> Response {
    // 60s 缓存直接命中
    if let Some((at, _, body)) = st.models_cache.lock().ok().and_then(|g| g.clone()) {
        if at.elapsed() < Duration::from_secs(60) {
            return ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response();
        }
    }
    let mut data: Vec<Value> = Vec::new();
    let mut index: Vec<(String, String)> = Vec::new();
    for s in st.all_specs().iter().filter(|s| s.enabled) {
        let mut ids: Vec<String> = s.models.clone();
        if s.models_policy == aigw_registry::ModelsPolicy::Live {
            match st.client.fetch_models(s).await {
                Ok(live) if !live.is_empty() => ids = live,
                Ok(_) => {} // 上游空清单 → 保留静态
                Err(e) => {
                    tracing::warn!("models live 拉取失败（{}），回退静态：{}", s.id, e);
                }
            }
        }
        for m in ids {
            index.push((s.id.clone(), m.clone()));
            data.push(json!({"id": m, "object": "model", "owned_by": s.id}));
        }
    }
    let body = json!({"object": "list", "data": data});
    if let Ok(mut g) = st.models_cache.lock() {
        *g = Some((Instant::now(), index, body.clone()));
    }
    ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

fn openai_error(status: u16, msg: &str) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (
        code,
        Json(json!({
            "error": {"message": msg, "type": "upstream_error", "code": status}
        })),
    )
        .into_response()
}

#[allow(clippy::too_many_arguments)]
fn record(
    store: &Store,
    pool: &Pool,
    spec_id: &str,
    account: &str,
    model: &str,
    started: Instant,
    ok: bool,
    tin: i64,
    tout: i64,
    class: Option<ErrorClass>,
    waf_fp: bool,
    upstream: &str,
) {
    store.usage_record(UsageEvent {
        ts: aigw_store::now_unix(),
        platform: spec_id.to_string(),
        model: model.to_string(),
        tin,
        tout,
        ms: started.elapsed().as_millis() as i64,
        ok,
        upstream: upstream.to_string(),
    });
    match class {
        Some(c) => pool.report(spec_id, account, model, false, c, waf_fp),
        None => pool.report(spec_id, account, model, true, ErrorClass::Transient, false),
    }
}

async fn chat_completions(State(st): State<AppState>, body: Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return openai_error(400, "invalid JSON body");
    };
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    // `模型id@平台` 后缀路由（旧版语义）；转发给上游时剥掉
    let (upstream_model, _) = match model.rsplit_once('@') {
        Some((m, p)) => (m.to_string(), Some(p.to_string())),
        None => (model.clone(), None),
    };
    let Some(spec) = st.spec_for_model(&model) else {
        return openai_error(503, "no enabled provider in registry");
    };
    let is_stream = v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    // body 的 model 改写为剥后缀版本
    let mut vb = v.clone();
    vb["model"] = json!(upstream_model);
    let body: Bytes = Bytes::from(vb.to_string());

    // 账号 candidates：anon 平台走免密单账号；其余从 AccountStore 取启用账号（明文 secret）
    let anon = spec.auth == AuthKind::Anon;
    let pool_accounts: Vec<(String, String)> = if anon {
        vec![("anon-public".to_string(), String::new())]
    } else {
        st.accounts.secrets_for(&spec.id)
    };
    if pool_accounts.is_empty() {
        return openai_error(401, &format!("平台 {} 无可用账号，请先接入", spec.id));
    }
    let ids: Vec<String> = pool_accounts.iter().map(|(id, _)| id.clone()).collect();
    // 会话粘性优先：命中且账号在候选 → 复用；否则正常选号并绑定。
    // 键 = conversation_id 系列 → 兜底最后一条 user 哈希（loomy 修正，防单号霸占）。
    let skey = session_key(&v);
    let account = st.sticky.route(&skey, &ids).unwrap_or_else(|| {
        let a = st.pool.pick(&spec.id, &ids, &model).unwrap_or_else(|| {
            tracing::warn!("pool 全挡，半开已放行；仍取首个候选硬走");
            ids[0].clone()
        });
        st.sticky.bind(&skey, &a);
        a
    });
    let secret = pool_accounts
        .iter()
        .find(|(id, _)| *id == account)
        .map(|(_, s)| s.clone())
        .unwrap_or_default();
    let Some(_lease) = st.pool.acquire(&spec.id, &account, &model) else {
        return openai_error(429, "provider busy: inflight cap reached");
    };
    let started = Instant::now();

    if is_stream {
        match st.client.forward_stream(&spec, &secret, &body).await {
            Ok((stream, stat_rx)) => {
                let store = st.store.clone();
                let pool = st.pool.clone();
                let spec_id = spec.id.clone();
                let spec_url = spec.base_url.clone();
                let model2 = model.clone();
                let account2 = account.clone();
                tokio::spawn(async move {
                    // 等流结束的统计 → 记账 + pool 上报
                    if let Ok(stat) = stat_rx.recv() {
                        let stat: StreamStat = stat;
                        let ok = stat.error.is_none();
                        record(
                            &store,
                            &pool,
                            &spec_id,
                            &account2,
                            &model2,
                            started,
                            ok,
                            stat.tokens_in,
                            stat.tokens_out,
                            stat.error,
                            false,
                            &spec_url,
                        );
                    }
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
            Err(e) => {
                record(
                    &st.store,
                    &st.pool,
                    &spec.id,
                    &account,
                    &model,
                    started,
                    false,
                    0,
                    0,
                    Some(e.class),
                    false,
                    &spec.base_url,
                );
                // Cline WorkOS 401 → 刷新双通道自愈 → 重试一次（22 cline2api 自愈链）
                if spec.id == "cline" && e.status == 401 && secret.matches(':').count() >= 2 {
                    if let Some(resp) = cline_retry_after_refresh(
                        &st, &spec, &account, &secret, &body, is_stream, started, &model,
                    )
                    .await
                    {
                        return resp;
                    }
                }
                openai_error(if e.status == 0 { 502 } else { e.status }, &e.body)
            }
        }
    } else {
        match st.client.forward_nonstream(&spec, &secret, &body).await {
            Ok(v) => {
                let (tin, tout) = extract_usage(&v);
                record(
                    &st.store,
                    &st.pool,
                    &spec.id,
                    &account,
                    &model,
                    started,
                    true,
                    tin,
                    tout,
                    None,
                    false,
                    &spec.base_url,
                );
                Json(v).into_response()
            }
            Err(e) => {
                let waf_fp = e.status == 200; // 200 + 假成功指纹已在 relay 内分类
                record(
                    &st.store,
                    &st.pool,
                    &spec.id,
                    &account,
                    &model,
                    started,
                    false,
                    0,
                    0,
                    Some(e.class),
                    waf_fp,
                    &spec.base_url,
                );
                // Cline WorkOS 401 → 刷新双通道自愈 → 重试一次（22 cline2api 自愈链）
                if spec.id == "cline" && e.status == 401 && secret.matches(':').count() >= 2 {
                    if let Some(resp) = cline_retry_after_refresh(
                        &st, &spec, &account, &secret, &body, is_stream, started, &model,
                    )
                    .await
                    {
                        return resp;
                    }
                }
                openai_error(if e.status == 0 { 502 } else { e.status }, &e.body)
            }
        }
    }
}

/// Cline WorkOS 401 自愈重试：刷新双通道（/auth/refresh → WorkOS 直刷 → register 换皮）→
/// 更新账号 secret → 重试一次转发。刷新失败/仍 401 返回 None（上层保留原错误）。
async fn cline_retry_after_refresh(
    st: &AppState,
    spec: &ProviderSpec,
    account: &str,
    secret: &str,
    body: &Bytes,
    is_stream: bool,
    started: Instant,
    model: &str,
) -> Option<Response> {
    let http = aigw_relay::auth_login::auth_http();
    let new_secret = aigw_relay::auth_login::cline_refresh(&http, secret).await.ok()?;
    st.accounts.update_secret(account, &new_secret);
    tracing::info!("cline 账号 {account} 401 自愈刷新成功，重试一次转发");
    let spec2 = spec.clone();
    let body2 = body.clone();
    if is_stream {
        match st.client.forward_stream(&spec2, &new_secret, &body2).await {
            Ok((stream, stat_rx)) => {
                let store = st.store.clone();
                let pool = st.pool.clone();
                let spec_id = spec.id.clone();
                let spec_url = spec.base_url.clone();
                let model2 = model.to_string();
                let account2 = account.to_string();
                tokio::spawn(async move {
                    if let Ok(stat) = stat_rx.recv() {
                        let ok = stat.error.is_none();
                        record(
                            &store,
                            &pool,
                            &spec_id,
                            &account2,
                            &model2,
                            started,
                            ok,
                            stat.tokens_in,
                            stat.tokens_out,
                            stat.error,
                            false,
                            &spec_url,
                        );
                    }
                });
                Some(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .header(header::CACHE_CONTROL, "no-cache")
                        .body(Body::from_stream(stream))
                        .unwrap(),
                )
            }
            Err(e2) => {
                record(
                    &st.store,
                    &st.pool,
                    &spec.id,
                    &account,
                    &model,
                    started,
                    false,
                    0,
                    0,
                    Some(e2.class),
                    false,
                    &spec.base_url,
                );
                Some(openai_error(if e2.status == 0 { 502 } else { e2.status }, &e2.body))
            }
        }
    } else {
        match st.client.forward_nonstream(&spec2, &new_secret, &body2).await {
            Ok(v) => {
                let (tin, tout) = extract_usage(&v);
                record(
                    &st.store,
                    &st.pool,
                    &spec.id,
                    &account,
                    &model,
                    started,
                    true,
                    tin,
                    tout,
                    None,
                    false,
                    &spec.base_url,
                );
                Some(Json(v).into_response())
            }
            Err(e2) => {
                record(
                    &st.store,
                    &st.pool,
                    &spec.id,
                    &account,
                    &model,
                    started,
                    false,
                    0,
                    0,
                    Some(e2.class),
                    false,
                    &spec.base_url,
                );
                Some(openai_error(if e2.status == 0 { 502 } else { e2.status }, &e2.body))
            }
        }
    }
}

fn extract_usage(v: &Value) -> (i64, i64) {
    let u = v.get("usage");
    (
        u.and_then(|u| u.get("prompt_tokens"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        u.and_then(|u| u.get("completion_tokens"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
    )
}

async fn web_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Web::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    // 前端迭代期禁强缓存：浏览器每次向服务端校验，避免旧 JS 卡死交互
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                f.data.to_vec(),
            )
                .into_response()
        }
        // SPA 回退：未知路径一律回首页
        None => match Web::get("index.html") {
            Some(f) => (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                f.data.to_vec(),
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_string(resp: Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).to_string()
    }

    fn req(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    fn spec_toml(raw: &str) -> ProviderSpec {
        toml::from_str(raw).unwrap()
    }

    fn state(specs: Vec<ProviderSpec>) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(
            specs,
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        )
    }

    fn state_with_accounts(specs: Vec<ProviderSpec>, accounts: AccountStore) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(
            specs,
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(accounts),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        )
    }

    #[tokio::test]
    async fn health_ok() {
        let app = build_app(state(vec![]));
        let res = app.oneshot(req("/api/health")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"ok\":true"), "body={body}");
    }

    #[tokio::test]
    async fn models_static_contract() {
        let spec = spec_toml(
            r#"
id = "demo"
name = "Demo"
enabled = true
auth = "api_key"
base_url = "https://example.invalid/v1"
format = "openai"
models = ["m-1", "m-2"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let app = build_app(state(vec![spec]));
        let res = app.oneshot(req("/v1/models")).await.unwrap();
        let body = body_string(res).await;
        assert!(body.contains("\"m-1\""), "body={body}");
        assert!(body.contains("\"owned_by\":\"demo\""), "body={body}");
    }

    #[tokio::test]
    async fn models_live_falls_back_to_static() {
        // base_url 指向不可达端口 → live 拉取失败 → 回退静态清单
        let spec = spec_toml(
            r#"
id = "down"
name = "Down"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["static-fallback"]
models_policy = "live"
stream_dialect = "sse_delta"
"#,
        );
        let app = build_app(state(vec![spec]));
        let res = app.oneshot(req("/v1/models")).await.unwrap();
        let body = body_string(res).await;
        assert!(body.contains("static-fallback"), "body={body}");
    }

    #[tokio::test]
    async fn chat_without_provider_is_503() {
        let app = build_app(state(vec![]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .body(Body::from(r#"{"model":"x","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn usage_endpoint_reflects_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        store.usage_record(UsageEvent {
            ts: aigw_store::now_unix(),
            platform: "t".into(),
            model: "m".into(),
            tin: 3,
            tout: 5,
            ms: 9,
            ok: true,
            upstream: "u".into(),
        });
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let res = app.oneshot(req("/api/usage")).await.unwrap();
        let body = body_string(res).await;
        assert!(body.contains("\"requests\":1"), "body={body}");
    }

    // ---------- fake upstream 密闭测试（Patrick/loomy 纪律：不碰真网） ----------

    /// 起 mock 上游，返回其 base（http://127.0.0.1:port）。
    async fn spawn_mock(body: &'static str, content_type: &'static str) -> String {
        use axum::routing::post as post_route;
        let app = Router::new().route(
            "/chat/completions",
            post_route(move || async move {
                ([(header::CONTENT_TYPE, content_type)], body.to_string())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    fn spec_for(base: &str) -> ProviderSpec {
        let raw = format!(
            r#"
id = "mock"
name = "Mock"
enabled = true
auth = "anon"
base_url = "{base}"
format = "openai"
models = []
models_policy = "live"
stream_dialect = "sse_delta"
"#
        );
        toml::from_str(&raw).unwrap()
    }

    #[tokio::test]
    async fn chat_nonstream_via_mock_upstream() {
        let mock_resp = r#"{"id":"cmpl-1","choices":[{"message":{"role":"assistant","content":"OK"}}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let app = build_app(state(vec![spec_for(&base)]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .body(Body::from(
                        r#"{"model":"mock-1","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"OK\""), "body={body}");
    }

    #[tokio::test]
    async fn chat_stream_via_mock_upstream() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
        let base = spawn_mock(Box::leak(sse.to_string().into_boxed_str()), "text/event-stream").await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let app = build_app(AppState::new(
            vec![spec_for(&base)],
            store.clone(),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        ));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .body(Body::from(r#"{"model":"mock-1","stream":true,"messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"), "ct={ct}");
        let body = body_string(res).await;
        assert!(body.contains("Hi"), "透传应保留原始 SSE：{body}");
        assert!(body.contains("[DONE]"), "body={body}");
        // 记账是 spawn 的，稍等一拍
        for _ in 0..20 {
            if store.usage_totals()["requests"].as_u64().unwrap_or(0) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let totals = store.usage_totals();
        assert_eq!(totals["requests"], 1);
        assert_eq!(totals["tokensIn"], 4);
        assert_eq!(totals["tokensOut"], 2);
    }

    // ---------- glm adapter 密闭测试（refresh + stream 全 mock） ----------

    fn glm_spec_for(base: &str) -> ProviderSpec {
        let raw = format!(
            r#"
id = "web-glm"
name = "GLM Mock"
enabled = true
auth = "cookie"
base_url = "{base}"
format = "openai"
models = ["glm-4-flash"]
models_policy = "static"
stream_dialect = "event_machine"
upstream_kind = "chatglm"
signer = "glm"
biz_err = ["登录", "失效"]
"#
        );
        toml::from_str(&raw).unwrap()
    }

    #[tokio::test]
    async fn glm_nonstream_via_mock_upstream() {
        use axum::routing::post as post_route;
        let app = Router::new()
            .route(
                "/user-api/user/refresh",
                post_route(|| async { Json(json!({"data": {"access_token": "at-test"}})) }),
            )
            .route(
                "/backend-api/assistant/stream",
                post_route(|| async {
                    ([(header::CONTENT_TYPE, "text/event-stream")],
                     "data: {\"parts\":[{\"content\":\"你好\"}]}\n\ndata: {\"parts\":[{\"content\":\"你好世界\"}]}\n\n")
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let accounts = AccountStore::from_rows(vec![aigw_store::Account {
            id: "g1".into(),
            platform: "web-glm".into(),
            name: "t".into(),
            secret: "rt-test".into(),
            kind: "cookie".into(),
            enabled: true,
            status: String::new(),
        }]);
        let app = build_app(state_with_accounts(vec![glm_spec_for(&format!("http://{addr}"))], accounts));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .body(Body::from(
                        r#"{"model":"glm-4-flash","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("你好世界"), "聚合最后快照：{body}");
        assert!(body.contains("chat.completion"), "OpenAI 形态：{body}");
    }
}
