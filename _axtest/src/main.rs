use axum::{Json, Router, routing::post, extract::State, response::Response, http::StatusCode};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
struct AppState { x: Arc<u32> }

async fn auth_action(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    match b.get("action").and_then(|a| a.as_str()) {
        Some("start") => Json(json!({"ok": true, "x": *st.x})).into_response(),
        _ => (StatusCode::BAD_REQUEST, "bad").into_response(),
    }
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/auth", post(auth_action))
        .with_state(AppState { x: Arc::new(1) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
