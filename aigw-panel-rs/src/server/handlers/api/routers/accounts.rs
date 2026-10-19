use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use super::util::*;

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
            // ★ S3：账号没了，它的在途/冷却/拉黑记录也要跟着消失，
            //   否则 /api/route 快照里会挂着个不存在的账号。
            if ok {
                st.inner.pool.forget_account(id);
            }
            Json(json!({"ok": ok, "message": if ok { "" } else { "账号不存在" }}))
        }
        "toggle" => {
            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let on = b.get("on").and_then(|v| v.as_bool()).unwrap_or(true);
            let ok = st.inner.login.toggle_account(id, on);
            Json(json!({"ok": ok}))
        }
        "list" | _ => {
            let refresh = q.get("refresh").map(|s| s == "1").unwrap_or(false)
                || b.get("refresh").and_then(|v| v.as_i64()).map(|n| n == 1).unwrap_or(false);
            if refresh {
                if let Some(gwextra) = st.inner.catalog.raw_get("gwextra") {
                    st.inner.login.refresh_balances(&gwextra).await;
                }
            }
            let (rows, stats) = st.inner.login.list_accounts();
            Json(json!({"ok": true, "accounts": rows, "stats": stats}))
        }
    }
}

/// 积分到期快照（`/api/credits`）。
/// `refresh=1` 强制打上游重拉；否则返回内存缓存（60s 内复用），
/// 避免每次切页面都对 47 个资源包 + 各平台余额接口发起一轮请求。
///
/// ★ S5 响应体积治理：`rows`（资源包明细，随账号数线性增长）默认只返前
///   `CREDITS_PAGE` 条 —— 前端默认视图本来也只看「有余额」的少数几行；
///   用户点「展开全部」时带 `all=1` 再拉全量。KPI / dist / by_platform
///   这些汇总字段**不受分页影响**（它们本来就是聚合值）。
#[tracing::instrument(skip(st))]
pub async fn credits(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    /// 明细默认页大小（积分到期页默认视图只显示有余额的包，通常个位数）
    const CREDITS_PAGE: usize = 30;
    let b: Value = body.map(|Json(v)| v).unwrap_or(Value::Null);
    let force = q.get("refresh").map(|s| s == "1").unwrap_or(false)
        || b.get("refresh").and_then(|v| v.as_bool()).unwrap_or(false)
        || b.get("refresh").and_then(|v| v.as_i64()).map(|n| n == 1).unwrap_or(false);
    // all=1 → 不分页（用户主动展开全部时）；limit=N 自定义页大小
    let all = q.get("all").map(|s| s == "1").unwrap_or(false)
        || b.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(CREDITS_PAGE)
        .max(1);

    {
        let c = st.inner.credits_cache.read().await;
        if !force {
            if let Some(v) = c.as_ref() {
                let fresh = v
                    .get("ts")
                    .and_then(|x| x.as_i64())
                    .map(|ts| now_ms_i64() - ts < 60_000)
                    .unwrap_or(false);
                if fresh {
                    return Json(page_rows(v, all, limit));
                }
            }
        }
    }

    let gwextra = st
        .inner
        .catalog
        .raw_get("gwextra")
        .cloned()
        .unwrap_or(Value::Null);
    // ★ 必须用 raw_rows()：list_accounts() 会把 secret 脱敏成 "eyJhbG…fCGg"，
    //   拿掩码打上游一律 401（2026-10-16 踩坑）。
    let rows = st.inner.login.raw_rows();
    let snap = crate::server::credits::snapshot(&rows, &gwextra).await;
    // ★ urgency_rank：把每个账号的紧迫度（到期日越近越该先用）写回账号池，
    //   之后请求路径的 secrets_ranked() 只读这一个整数，不再打上游。
    if let Some(rs) = snap.get("rows").and_then(|v| v.as_array()) {
        st.inner.login.set_urgency(&crate::server::credits::account_urgency(rs));
    }
    {
        let mut c = st.inner.credits_cache.write().await;
        *c = Some(snap.clone());
    }
    Json(page_rows(&snap, all, limit))
}

/// 按分页参数裁剪 `rows`，并补上 `rows_total` / `rows_shown` 让前端能显示
/// 「还有 N 条未显示」。汇总字段（packages/total_remain/dist/by_platform）不动 ——
/// 它们必须是全量口径，否则 KPI 会和明细对不上。
fn page_rows(snap: &Value, all: bool, limit: usize) -> Value {
    let mut v = snap.clone();
    let Some(arr) = v.get("rows").and_then(|r| r.as_array()).cloned() else {
        return v;
    };
    let total = arr.len();
    let shown = if all { total } else { total.min(limit) };
    if let Value::Object(ref mut o) = v {
        o.insert("rows".to_string(), Value::Array(arr.into_iter().take(shown).collect()));
        o.insert("rows_total".to_string(), json!(total));
        o.insert("rows_shown".to_string(), json!(shown));
        o.insert("rows_truncated".to_string(), json!(shown < total));
    }
    v
}
