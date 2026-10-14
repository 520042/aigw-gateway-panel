//! 领域模型：路由目标、中继计划、登录方式、中继请求。
//!
//! 这些类型被 [`crate::router`]、[`crate::relay`]、[`crate::platforms`] 共用，
//! 集中定义以避免循环依赖。
use std::collections::HashMap;

/// 面板版本（同步写入 HTTP 头 / /api/overview）。
pub const VERSION: &str = "0.3.0";

/// 自动路由引擎的一个上游目标。
#[derive(Debug, Clone)]
pub struct Target {
    pub model: String,
    pub upstream_id: String,
    pub name: String,
    /// 上游 base（无尾斜杠）。
    pub endpoint: String,
    pub api_key: String,
    pub protocol: String, // openai / anthropic / web
    pub weight: i64,
    pub priority: i64,
    pub free: bool,
    pub headers: HashMap<String, String>,
}

/// 中继计划：把一次 /v1/chat/completions 解析成「去哪打、用什么模型名」。
#[derive(Debug, Clone)]
pub enum Plan {
    /// 公开免费 / 公共车道（如 Pollinations），直接转发。
    Public {
        url: String,
        model_override: Option<String>,
    },
    /// 上游透传（用户自有端点或自定义 API），key 缺省时透传客户端 Bearer / 设置 key。
    Upstream {
        url: String,
        key: Option<String>,
    },
    /// 需要该平台登录凭据，当前未登录。
    NeedLogin { pid: String, name: String },
    /// 平台原生中继（如豆包 samantha）：pid 平台 + 模型名，凭据取账号池。
    Native { pid: String, model: String },
    /// 网页版反代（智谱清言 / Trae 国内 / DeepSeek 网页）：私有协议，凭据取账号池。
    Web { pid: String, model: String },
}

/// 平台登录方式（trait 元数据，用于前端渲染接入卡片）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginMethod {
    None,
    Cookie,
    QrCode,
    OAuth2,
    File,
    Unknown,
}

/// 一次中继请求（handler 解析后传入 trait）。
#[derive(Debug, Clone)]
pub struct RelayRequest {
    /// `@平台` 之后的模型后缀（auto/free 场景为完整 model）。
    pub model: String,
    pub body: Vec<u8>,
    pub headers: http::HeaderMap,
}
