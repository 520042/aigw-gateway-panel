//! 应用状态：在 axum handler 间共享的可克隆句柄。
use crate::catalog::Catalog;
use crate::core::config::Settings;
use crate::platforms::Registry;
use crate::router::Router;
use crate::server::login::LoginManager;
use crate::server::usage::UsageRecorder;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub settings: RwLock<Settings>,
    pub catalog: Catalog,
    pub router: Arc<Router>,
    pub registry: Arc<Registry>,
    pub disabled: RwLock<HashSet<String>>,
    /// 进程启动时刻（/api/overview 的 uptime 用）
    pub started: std::time::Instant,
    /// 登录会话 + 账号池
    pub login: Arc<LoginManager>,
    /// 用量记录
    pub usage: Arc<UsageRecorder>,
}
