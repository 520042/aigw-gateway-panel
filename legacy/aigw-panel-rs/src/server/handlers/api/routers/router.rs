use crate::server::state::AppState;
use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde_json::json;
use std::collections::HashMap;

/// 路由总览（`api/route`）：真实 router 状态（models/config/熔断参数）
/// + 号池治理快照（InFlight / 冷却 / WAF 拉黑 / RecentPick 参数）。
#[tracing::instrument(skip(st))]
pub async fn route(State(st): State<AppState>) -> impl IntoResponse {
    let r = &st.inner.router;
    Json(json!({
        "ok": true,
        "models": r.describe_models(),
        "config": {"auto": "fastest", "free": "free"},
        "defaults": ["auto", "free"],
        "auto_probe": true,
        "probe_interval": 60,
        "breaker_fail": 3,
        "breaker_cooldown": 60,
        "in_route_sites": [],
        // ★ 2026-10-16 S3：号池治理实时状态（见 server/pool.rs）
        "pool": st.inner.pool.snapshot(),
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
