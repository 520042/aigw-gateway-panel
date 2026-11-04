//! HTTP 服务层：状态、鉴权中间件、静态资源、handlers、路由装配。
pub mod auth;
pub mod autocheckin;
pub mod balance;
pub mod cdp;
pub mod db;
pub mod checkin;
pub mod credits;
pub mod free_lane_models;
pub mod free_models;
pub mod handlers;
pub mod loomy_auth;
pub mod model_extract;
pub mod localproxy;
pub mod login;
pub mod notify;
pub mod pool;
pub mod state;
pub mod static_files;
pub mod store;
pub mod tencent;
pub mod usage;

use crate::server::state::AppState;
use axum::{middleware, routing::{get, post}, Router};

/// 用系统默认浏览器打开 URL（带前台焦点）。
/// ★ 2026-10-15：改为首选 `cmd /c start`——explorer.exe 从面板进程 spawn 时
///   在部分环境报 0xc0000142（DLL 初始化失败，弹错误框，用户截图实锤）；
///   cmd start 是 Windows 拉默认浏览器的标准方式，CREATE_NO_WINDOW 防 cmd 窗口闪现。
///   登录跳转已全部走前端 window.open，此处仅面板启动开首页，失败无碍。
pub fn open_browser(url: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let cmd = std::path::Path::new(&sysroot).join("System32").join("cmd.exe");
    match std::process::Command::new(cmd)
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(_) => tracing::info!(url, "已打开默认浏览器（cmd start）"),
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
        .route("/api/tencent", get(handlers::api::tencent).post(handlers::api::tencent))
        .route("/api/gwextra", get(handlers::api::gwextra).post(handlers::api::gwextra))
        .route("/api/credentials", get(handlers::api::credentials).post(handlers::api::credentials))
        .route("/api/catalog", get(handlers::api::catalog))
        .route("/api/localproxy", get(handlers::api::localproxy).post(handlers::api::localproxy))
        .route("/api/sites", get(handlers::api::sites))
        .route("/api/custom_apis", get(handlers::api::custom_apis).post(handlers::api::custom_apis))
        .route("/api/autocheckin", get(handlers::api::autocheckin).post(handlers::api::autocheckin))
        .route("/api/route", get(handlers::api::route))
        .route("/api/usage", get(handlers::api::usage))
        .route("/api/credits", get(handlers::api::credits).post(handlers::api::credits))
        .route("/api/growth", get(handlers::api::growth))
        .route("/api/tasks", get(handlers::api::tasks).post(handlers::api::tasks))
        .route("/api/free_models", get(handlers::api::free_models))
        .route("/api/free_lane_models", get(handlers::api::free_lane_models))
        .route("/api/notify", get(handlers::api::notify).post(handlers::api::notify))
        .route("/api/logs", get(handlers::api::logs))
        .route("/api/data/backup", post(handlers::api::data_backup))
        .route("/api/data/backups", get(handlers::api::data_backups))
        .route("/api/data/restore", post(handlers::api::data_restore))
        .route("/auth/callback", get(handlers::callback::auth_callback))
        .route("/v1/models", get(handlers::v1::models))
        .route("/v1/chat/completions", post(handlers::v1::chat_completions))
        .route("/v1/messages", post(handlers::v1::messages))
        .fallback(static_files::catch_all)
        .layer(middleware::from_fn_with_state(state.clone(), auth::auth_mw))
        .with_state(state.clone())
}
