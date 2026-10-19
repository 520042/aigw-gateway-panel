//! aigw-core：领域模型与错误分类。
//!
//! M0 骨架先行两块（后续里程碑在此 crate 内扩展领域模型）：
//! 1. [`ErrorClass`]：上游失败的业务分类，号池据此决定冷却/拉黑/禁用策略。
//!    规则来源：本项目 S3 号池治理 + lobsterai2api 错误分类映射 +
//!    Patrick/loomy2api「5xx 不怪账号」故障归因原则。
//! 2. [`StreamChunk`]：统一中间流块。各线协议（OpenAI/Anthropic/Responses）
//!    按 body 形状归一到这里（dsh-our-free-model 的判流思路），
//!    再按目标方言转出。M1 落地流式状态机。

use serde::{Deserialize, Serialize};

/// 上游/请求失败的业务分类。
///
/// 归因原则（写进代码前先写在这里）：
/// - `Auth`/`Waf`/`Quota` 是**账号的问题** → 冷却、拉黑或禁用该账号；
/// - `Rate` 是**用得快** → 按 Retry-After 冷却，不禁用；
/// - `Transient` 是**上游/网络的问题** → 熔断该目标，不怪账号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// 凭据失效/未授权：401/403。
    Auth,
    /// 限流：429。按 Retry-After 冷却。
    Rate,
    /// 额度耗尽：402 或明确 quota 文案。账号标 EXHAUSTED。
    Quota,
    /// 上游临时故障：5xx/网络错误。只熔断，不怪账号。
    Transient,
    /// WAF/风控拦截：401/403 且命中风控指纹（如 HTTP 200 + 业务错误码假成功）。
    Waf,
}

impl ErrorClass {
    /// 按 HTTP 状态码粗分类。
    ///
    /// body 内业务码的二次纠正（如 loomy HTTP 200 + code=100002 假成功 → Auth/Waf）
    /// 由各平台 adapter 调 [`ErrorClass::refine_biz`] 完成。
    pub fn from_status(status: u16) -> Self {
        match status {
            401 | 403 => ErrorClass::Auth,
            402 => ErrorClass::Quota,
            429 => ErrorClass::Rate,
            s if s >= 500 => ErrorClass::Transient,
            _ => ErrorClass::Transient,
        }
    }

    /// 业务错误码二次纠正（假成功识别）。
    ///
    /// `biz_code` 为上游 body 里的 code/message 摘要；命中已知风控指纹返回 `Some`。
    /// M0 先落地机制与单测；指纹库随 M3 各平台 adapter 逐个补充。
    pub fn refine_biz(&self, biz_code: &str, biz_message: &str) -> Option<ErrorClass> {
        let m = biz_message.to_lowercase();
        // 登录失效/授权类文案（loomy/星火系假成功指纹，v1 biz_err 实测沉淀）
        let auth_like = ["登录", "失效", "授权", "未登录", "token"]
            .iter()
            .any(|k| m.contains(k));
        if auth_like {
            return Some(ErrorClass::Auth);
        }
        let _ = biz_code;
        None
    }
}

/// 统一中间流块：所有上游方言先归一到这里，再按目标协议转出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamChunk {
    /// 正文增量。
    Text { delta: String },
    /// 思考/推理增量（reasoning）。
    Thinking { delta: String },
    /// 工具调用增量（按 index 聚片）。
    ToolCall {
        index: u32,
        id: String,
        name: String,
        arguments_delta: String,
    },
    /// 用量（流内或 FIN 帧携带）。
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    /// 终止帧。finish_reason 不伪造（ops120/zcode2api 原则）。
    Done { finish_reason: Option<String> },
    /// 流中错误（mid-stream 无法改状态码，按块内错误传递）。
    Error { class: ErrorClass, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_status() {
        assert_eq!(ErrorClass::from_status(401), ErrorClass::Auth);
        assert_eq!(ErrorClass::from_status(403), ErrorClass::Auth);
        assert_eq!(ErrorClass::from_status(402), ErrorClass::Quota);
        assert_eq!(ErrorClass::from_status(429), ErrorClass::Rate);
        assert_eq!(ErrorClass::from_status(500), ErrorClass::Transient);
        assert_eq!(ErrorClass::from_status(503), ErrorClass::Transient);
        assert_eq!(ErrorClass::from_status(404), ErrorClass::Transient);
    }

    #[test]
    fn refine_biz_auth_fingerprints() {
        assert_eq!(
            ErrorClass::Transient.refine_biz("100002", "登录已失效"),
            Some(ErrorClass::Auth)
        );
        assert_eq!(
            ErrorClass::Transient.refine_biz("", "token expired"),
            Some(ErrorClass::Auth)
        );
        assert_eq!(ErrorClass::Transient.refine_biz("ok", "一切正常"), None);
    }

    #[test]
    fn stream_chunk_serde_roundtrip() {
        let chunks = vec![
            StreamChunk::Text { delta: "你好".into() },
            StreamChunk::Thinking { delta: "…".into() },
            StreamChunk::ToolCall {
                index: 0,
                id: "call_1".into(),
                name: "get_weather".into(),
                arguments_delta: "{\"city\":".into(),
            },
            StreamChunk::Usage { prompt_tokens: 12, completion_tokens: 34 },
            StreamChunk::Done { finish_reason: Some("stop".into()) },
            StreamChunk::Error { class: ErrorClass::Rate, message: "429".into() },
        ];
        for c in chunks {
            let json = serde_json::to_string(&c).unwrap();
            let back: StreamChunk = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{back:?}"), format!("{c:?}"));
        }
        // tag 必须是 snake_case
        assert!(serde_json::to_string(&StreamChunk::Text { delta: "x".into() })
            .unwrap()
            .contains("\"type\":\"text\""));
    }
}
