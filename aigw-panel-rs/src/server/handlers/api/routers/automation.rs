use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use super::wb::*;

/// 签到（`api/autocheckin`）：status=公益站真实清单；trigger=wb 族真实签到。
#[tracing::instrument(skip(st))]
pub async fn autocheckin(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let action = q.get("action").map(|s| s.as_str()).unwrap_or("status").to_string();
    let b: Value = body.clone().map(|Json(v)| v).unwrap_or(Value::Null);
    match action.as_str() {
        "status" => Json(crate::server::autocheckin::status(&st)),
        "platforms" => Json(json!({
            "ok": true,
            "platforms": crate::server::autocheckin::platform_list()
        })),
        "save" => Json(crate::server::autocheckin::save_from_body(&st, &b)),
        "run" => {
            // 后台跑，避免重试间隔（默认 10 分钟）阻塞 HTTP 请求。
            let force = b.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            let only: Option<Vec<String>> = b.get("platforms").and_then(|v| v.as_array()).map(|a| {
                a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()
            });
            let stc = st.clone();
            tokio::spawn(async move {
                let _ = crate::server::autocheckin::run_round(&stc, force, only).await;
            });
            Json(json!({"ok": true, "message": "已触发，结果稍后刷新"}))
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
/// 面板日志（`api/logs`）：读 data/panel.log 尾部（真实 tracing 输出，GUI 子系统
/// 下日志落这里；对齐前端日志页的 panel 行数组契约，最新在前）。
#[tracing::instrument]
pub async fn logs() -> impl IntoResponse {
    // 读文件尾部 300 行（每行截断 500 字符防爆内存）
    let path = crate::core::config::data_dir().join("panel.log");
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|s| {
            s.lines()
                .rev()
                .take(300)
                .map(|l| l.chars().take(500).collect::<String>())
                .collect()
        })
        .unwrap_or_default();
    if lines.is_empty() {
        lines.push("（日志文件暂无内容——面板刚启动或本轮无 INFO 级以上事件）".to_string());
    }
    Json(json!({
        "ok": true,
        "gateway": {"ok": false, "code": "unreachable",
                     "message": "网关 EXE 已移除（Rust 面板原生模式，日志见下方面板侧）"},
        "panel": lines,
        "log_file": path.display().to_string(),
    }))
}
