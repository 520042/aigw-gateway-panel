//! 统一错误类型。
//!
//! 整个面板的 fallible 路径统一收敛到 [`AppError`]，并实现 `axum::response::IntoResponse`，
//! 让 handler 可以以 `Result<impl IntoResponse, AppError>` 形式返回，或由 `main` 在启动期
//! 直接 `?` 展开为 HTTP 响应。
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("catalog parse error: {0}")]
    Catalog(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("upstream error: {0}")]
    Upstream(String),

    #[error("not implemented: {0}")]
    NotImplemented(String),

    #[error("bad request: {0}")]
    BadRequest(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            AppError::Catalog(m) => (StatusCode::INTERNAL_SERVER_ERROR, "catalog_error", m.clone()),
            AppError::Io(m) => (StatusCode::INTERNAL_SERVER_ERROR, "io_error", m.to_string()),
            AppError::Json(m) => (StatusCode::BAD_REQUEST, "json_error", m.to_string()),
            AppError::Upstream(m) => (StatusCode::BAD_GATEWAY, "upstream_error", m.clone()),
            AppError::NotImplemented(m) => {
                (StatusCode::NOT_IMPLEMENTED, "not_implemented", m.clone())
            }
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.clone()),
        };
        (
            status,
            Json(json!({ "ok": false, "code": code, "message": message })),
        )
            .into_response()
    }
}
