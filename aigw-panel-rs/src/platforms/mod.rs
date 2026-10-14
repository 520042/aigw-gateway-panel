//! 平台注册表：把 catalog 数据驱动地构造成 [`PlatformProvider`] 实现。
pub mod registry;

pub use registry::{CatalogPlatform, Registry};
