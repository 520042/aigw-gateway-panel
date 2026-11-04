use crate::server::state::AppState;
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;
use crate::core::model::VERSION;

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

