//! 静态资源服务 + 兜底路由 + 健康检查。
use crate::server::state::AppState;
use axum::{
    body::Body,
    extract::{OriginalUri, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::path::{Path, PathBuf};

pub fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "ico" => "image/x-icon",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

async fn serve_file(state: &AppState, rel: &str) -> Response {
    let base = PathBuf::from(&state.inner.settings.read().await.static_dir);
    let clean = rel.trim_start_matches('/').replace("\\", "/");
    if clean.contains("..") {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    }
    let target = base.join(clean);
    if let Ok(bytes) = tokio::fs::read(&target).await {
        return (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&target)),
            )],
            Body::from(bytes),
        )
            .into_response();
    }
    if !rel.starts_with("api/") && !rel.starts_with("v1/") {
        if let Ok(idx) = tokio::fs::read(base.join("index.html")).await {
            return (
                StatusCode::OK,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                )],
                Body::from(idx),
            )
                .into_response();
        }
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

pub async fn index(State(state): State<AppState>) -> Response {
    serve_file(&state, "index.html").await
}

pub async fn catch_all(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let path = uri.path().trim_start_matches('/').to_string();
    if path.starts_with("api/") || path.starts_with("v1/") {
        return (
            StatusCode::NOT_IMPLEMENTED,
            axum::Json(json!({
                "ok": false,
                "code": "not_implemented",
                "message": "该接口将在后续模块用 Rust 重写接入"
            })),
        )
            .into_response();
    }
    serve_file(&state, &path).await
}

pub async fn health() -> impl IntoResponse {
    axum::Json(json!({ "ok": true, "version": crate::core::model::VERSION }))
}
