//! 平台 / 登录抽象 trait。
//!
//! 全部 42 个接入平台以 `PlatformProvider` 统一抽象：任何平台只需实现
//! `id` / `name` / `login_method` / `relay`。[`crate::platforms::Registry`] 从
//! `catalog_bundled.json` 数据驱动地构造出每个平台的实现（`CatalogPlatform`），
//! 因此「新增平台 = 更新数据，无需改 Rust 代码」（符合大厂「配置即代码」风格）。
use crate::core::config::Settings;
use crate::core::model::{LoginMethod, Plan, RelayRequest};

/// 解析上下文：中继时需要的共享运行时配置。
pub struct ResolverCtx<'a> {
    pub cfg: &'a Settings,
}

/// 平台接入抽象：一个可被路由 / 中继引擎调度的接入源。
pub trait PlatformProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn login_method(&self) -> LoginMethod;
    /// 把当前请求解析成中继计划。
    fn relay(&self, req: &RelayRequest, ctx: &ResolverCtx) -> Plan;
}

/// 登录流程抽象（后续接入原生登录链路时实现）。
pub trait LoginProvider: Send + Sync {
    fn id(&self) -> &str;
    fn start(&self) -> LoginSession;
    fn poll(&self, state: &str) -> LoginStatus;
}

/// 登录会话（start 返回，前端轮询用）。
pub struct LoginSession {
    pub state: String,
    pub url: Option<String>,
}

/// 登录状态（poll 返回）。
pub enum LoginStatus {
    Pending,
    Ready { secret: String },
    Failed(String),
}
