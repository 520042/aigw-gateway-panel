//! HTTP 服务层：状态、鉴权中间件、静态资源、handlers、路由装配。
pub mod auth;
pub mod checkin;
pub mod free_models;
pub mod handlers;
pub mod login;
pub mod notify;
pub mod state;
pub mod static_files;
pub mod store;
pub mod usage;

use crate::server::state::AppState;
use axum::{middleware, routing::{get, post}, Router};

/// 用系统默认浏览器打开 URL（带前台焦点）。
/// ★ 首选 `explorer.exe <url>`——explorer 必然运行在用户交互桌面会话，
///   由它拉起默认浏览器并聚焦（`cmd start` 从服务型进程 spawn 时可能
///   静默不显窗）。explorer 失败再退 cmd start。
pub fn open_browser(url: &str) {
    let opened = (|| -> bool {
        std::process::Command::new("explorer.exe")
            .arg(url)
            .spawn()
            .is_ok()
    })();
    if opened {
        tracing::info!(url, "已通过 explorer 打开默认浏览器");
        return;
    }
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let cmd = std::path::Path::new(&sysroot).join("System32").join("cmd.exe");
    match std::process::Command::new(cmd)
        .args(["/C", "start", "", url])
        .spawn()
    {
        Ok(_) => tracing::info!(url, "已打开默认浏览器（cmd start 兜底）"),
        Err(e) => tracing::warn!(error = %e, url, "打开浏览器失败（可手动访问该地址）"),
    }
}

/// 装配 axum 应用（含鉴权中间件与状态注入）。返回已 `with_state` 的 `Router`。
pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/", get(static_files::index))
        .route("/api/health", get(static_files::health))
        .route("/api/overview", get(handlers::api::overview))
        .route("/api/models", get(handlers::api::models).post(handlers::api::models_post))
        .route(
            "/api/settings",
            get(handlers::api::settings_get).post(handlers::api::settings_post),
        )
        .route("/api/checkins", get(handlers::api::checkins))
        .route("/api/accounts", get(handlers::api::accounts).post(handlers::api::accounts))
        .route("/api/login", get(handlers::api::login).post(handlers::api::login))
        .route("/api/platform", get(handlers::api::platform_actions))
        .route("/api/sites", get(handlers::api::sites))
        .route("/api/custom_apis", get(handlers::api::custom_apis).post(handlers::api::custom_apis))
        .route("/api/autocheckin", get(handlers::api::autocheckin).post(handlers::api::autocheckin))
        .route("/api/route", get(handlers::api::route))
        .route("/api/usage", get(handlers::api::usage))
        .route("/api/growth", get(handlers::api::growth))
        .route("/api/tasks", get(handlers::api::tasks))
        .route("/api/free_models", get(handlers::api::free_models))
        .route("/api/notify", get(handlers::api::notify).post(handlers::api::notify))
        .route("/api/logs", get(handlers::api::logs))
        .route("/auth/callback", get(handlers::callback::auth_callback))
        .route("/v1/models", get(handlers::v1::models))
        .route("/v1/chat/completions", post(handlers::v1::chat_completions))
        .route("/v1/messages", post(handlers::v1::messages))
        .fallback(static_files::catch_all)
        .layer(middleware::from_fn_with_state(state.clone(), auth::auth_mw))
        .with_state(state.clone())
}
