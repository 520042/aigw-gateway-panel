//! 前端数据面所有 /api/* 常驻接口。
//!
//! 契约对齐 Python main.py / app.sources.py / app.custom_apis.py / gwextra
//! （前端 app.js 零改动复用）。2026-10-16 起实现按领域拆到 `routers/`，
//! 本文件只做转发，保持 `handlers::api::xxx` 的老引用路径不变。
//!
//! - overview                     总览（含 uptime，状态条用）
//! - models                       GET：models_enriched + rate_summary
//!                                POST：toggle_model / platform_models / select
//! - models?action=grouped        模型总表（catalog 驱动，真实分组 + disabled 开关）
//! - models?action=sources        接入源清单 + 分类 + KPI（sources 页核心）
//! - login 全 action              会话式登录（start/poll/submit/cancel）
//! - accounts                     list/delete/toggle（data/accounts.json）
//! - credits                      积分到期快照（账号池 → 各平台余额接口）
//! - sites                        公益站清单（sites + catalog）
//! - custom_apis                  list/add/update/delete/sync（真实探测上游 /models）
//! - autocheckin?action=status    签到状态（真实端点驱动）
//! - route                        路由总览（真实 router 状态）
//! - usage                        用量聚合（usage_events 90 天/5000 条）
//! - checkins                     签到记录基线
pub mod routers;

pub use routers::*;
