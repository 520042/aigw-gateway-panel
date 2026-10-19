//! /api/* 数据面 —— 按领域拆分的子模块。
//!
//! 历史包袱：这里曾经是一个 2700+ 行的单文件 `api.rs`，所有接口挤在一个
//! match 里，改一处要全文翻。2026-10-16 按参考框架 `admin/routers/` 的
//! 思路按领域拆开：每个子模块一个职责，跨域复用的东西收敛到 `util` / `wb`。
//!
//! | 模块 | 职责 |
//! |---|---|
//! | `util` | 时间戳 / UUID / URL 编解码 / 平台 host / 端口记忆 |
//! | `overview` | 总览 |
//! | `models` | 模型清单（enriched / grouped / sources / toggle / 连接测试） |
//! | `login` | 登录会话 + 接入平台清单 + 凭据 CRUD |
//! | `tencent` | 腾讯系原生直连 |
//! | `gwextra` | 平台动作注册表查询/执行 |
//! | `accounts` | 账号池 CRUD + 积分到期快照 |
//! | `router` | 路由总览 + 用量聚合 |
//! | `catalog` | 平台目录 |
//! | `settings` | 面板设置 |
//! | `proxies` | 本地反代 / 公益站 / 自定义 API |
//! | `automation` | 签到 / 任务 / 通知 / 日志 / 免费模型榜 |
//! | `wb` | wb / minimax 上游动作 |
pub mod accounts;
pub mod automation;
pub mod catalog;
pub mod data;
pub mod gwextra;
pub mod login;
pub mod models;
pub mod overview;
pub mod proxies;
pub mod router;
pub mod settings;
pub mod tencent;
pub mod util;
pub mod wb;

// 各域接口平铺再导出，让 `handlers::api::xxx` 的老路径继续可用
// （server/mod.rs 的路由表、callback.rs 的端口查询都不用改）。
// 部分再导出在本 crate 内可能暂时无人走，属兼容层，勿当死代码删。
#[allow(unused_imports)]
pub use accounts::*;
#[allow(unused_imports)]
pub use automation::*;
#[allow(unused_imports)]
pub use catalog::*;
#[allow(unused_imports)]
pub use data::*;
#[allow(unused_imports)]
pub use gwextra::*;
#[allow(unused_imports)]
pub use login::*;
#[allow(unused_imports)]
pub use models::*;
#[allow(unused_imports)]
pub use overview::*;
#[allow(unused_imports)]
pub use proxies::*;
#[allow(unused_imports)]
pub use router::*;
#[allow(unused_imports)]
pub use settings::*;
#[allow(unused_imports)]
pub use tencent::*;
#[allow(unused_imports)]
pub use util::*;
#[allow(unused_imports)]
pub use wb::*;
