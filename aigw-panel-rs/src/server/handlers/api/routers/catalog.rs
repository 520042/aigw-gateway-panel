use crate::server::state::AppState;
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;

/// `/api/catalog`：目录快照页（catalog_page，app/catalog.py 六大清单）。
#[tracing::instrument(skip(st))]
pub async fn catalog(State(st): State<AppState>) -> impl IntoResponse {
    let cp = st.inner.catalog.raw_get("catalog_page").cloned().unwrap_or(json!({}));
    let mut out = if cp.is_object() { cp } else { json!({}) };
    out["ok"] = json!(true);
    Json(out)
}
