//! aigw-panel · Rust 重写（大厂级分层架构）
//!
//! 模块分层：
//! - [`core`]    通用基座：错误类型、配置、领域模型、抽象 trait
//! - [`catalog`] 平台目录（单一数据源 `catalog_bundled.json`，由 Python 一手模块导出）
//! - [`router`]  自动模型路由引擎（四策略 + 熔断 + 会话亲和）
//! - [`relay`]   中继执行引擎（免费车道 / 上游透传 / 原生中继）
//! - [`platforms`] 平台注册表（trait 抽象 + 从 catalog 数据驱动构建）
//! - [`server`]  HTTP 服务（状态、鉴权、静态资源、handlers）
//! - [`tray`]    系统托盘（占位，后续接入 Windows 原生 GUI）
//!
//! 设计原则：lib + bin 分离；所有 handler 经统一错误类型 [`core::error::AppError`]
//! 接入 axum；日志走 [`tracing`]，不再使用 `println!`；启动参数走 [`clap`]。
pub mod catalog;
pub mod core;
pub mod install;
pub mod platforms;
pub mod relay;
pub mod router;
pub mod server;
pub mod tray;

pub use core::model::VERSION;
