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
    /// 请求被上游拒绝：400/404/410/413/415/422。**不怪账号、也不熔断**——归因在调用方
    /// （模型名写错、body 太大、参数非法、Content-Type 不对）。号池对这类不写任何状态。
    Rejected,
    /// **车道级封锁**：上游按客户端指纹拒绝整条免凭据车道（OpenCode 的 403 FreeTierError
    /// "free tier can only be used from within OpenCode"）。
    ///
    /// 关键区别：这既不是凭据失效（这条车道根本没有凭据），也不是模型写错，更不是上游故障。
    /// 归到 Auth 会让整条车道被 30 分钟账号级拉黑，并在面板上提示「去重新登录」——
    /// 而匿名车道没有可重新登录的东西，用户会照着做无用功。
    /// 参考 _refs/Ebony-Vinyl/dsh-our-free-model 为此单独设了 laneLocked 码。
    LaneLocked,
    /// **地域封锁**：上游对该模型/该地区不可用（403 RegionError
    /// "This model is not available in your country"）。与凭据无关，换地区/换车道才行。
    Region,
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
            // 调用方把请求写错了：上游明确拒绝的是**请求本身**，既不是账号也不是上游故障。
            // 旧实现让它们全落到 `_ => Transient`，而 Transient 的语义是「熔断该目标」——
            // 于是任何一个在 body 里写错模型名的客户端（上游回 404）都能熔断掉整个平台。
            // 410 Gone：上游明确废弃了该模型（OpenCode 的 ModelDeprecated）。同属调用方
            // 该换模型名的情况，不能算上游故障去熔断平台。
            400 | 404 | 410 | 413 | 415 | 422 => ErrorClass::Rejected,
            s if s >= 500 => ErrorClass::Transient,
            // 408 请求超时 / 451 地域限制：仍然归到网络/上游侧。
            _ => ErrorClass::Transient,
        }
    }

    /// 业务错误码二次纠正（假成功识别）。
    ///
    /// `biz_code` 为上游 body 里的 code/message 摘要；命中已知风控指纹返回 `Some`。
    /// M0 先落地机制与单测；指纹库随 M3 各平台 adapter 逐个补充。
    pub fn refine_biz(&self, biz_code: &str, biz_message: &str) -> Option<ErrorClass> {
        // 已经归因为「限流/额度」的不要再被文案改判：下面 auth 指纹里有 token，
        // 而 "token limit" / "tokens per minute" 是**正常限流**，误判成 Auth 会让
        // 这个账号进 30 分钟凭据隔离（面板上表现为「刚登录的号突然失效」）。
        if matches!(self, ErrorClass::Rate | ErrorClass::Quota) {
            return Some(*self);
        }
        let m = biz_message.to_lowercase();
        // 先排除限流/额度语义
        let rate_like = ["limit", "quota", "rate", "too many", "配额", "上限", "频繁", "超出"]
            .iter()
            .any(|k| m.contains(k));
        if rate_like {
            return None;
        }
        // 登录失效/授权类文案（loomy/星火系假成功指纹，v1 biz_err 实测沉淀）。
        // ★ 旧实现用裸 "token" 子串匹配，"token limit"/"token 配额" 全被当成登录失效。
        let auth_like = [
            "登录",
            "失效",
            "授权",
            "未登录",
            "token expired",
            "token 过期",
            "token已过期",
            "token 无效",
            "invalid token",
            "invalid_api_key",
            "unauthorized",
            "unauthenticated",
        ]
        .iter()
        .any(|k| m.contains(k));
        if auth_like {
            return Some(ErrorClass::Auth);
        }
        // 业务码指纹（数字码由 adapter 转成十进制字符串传入）
        if matches!(biz_code.trim(), "100002" | "100003" | "40001") {
            return Some(ErrorClass::Auth);
        }
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
        // 客户端错误不再算上游故障：否则写错一个模型名（上游 404）就能熔断整个平台
        for s in [400, 404, 413, 415, 422] {
            assert_eq!(ErrorClass::from_status(s), ErrorClass::Rejected, "status {s}");
        }
        // 408/451 仍按网络/上游侧处理
        assert_eq!(ErrorClass::from_status(408), ErrorClass::Transient);
        assert_eq!(ErrorClass::from_status(451), ErrorClass::Transient);
    }

    #[test]
    fn refine_biz_does_not_turn_rate_limits_into_auth() {
        // "token limit" 含 token：旧实现判成 Auth → 账号被 30 分钟凭据隔离
        assert_eq!(
            ErrorClass::Rate.refine_biz("", "token limit exceeded"),
            Some(ErrorClass::Rate)
        );
        assert_eq!(
            ErrorClass::Quota.refine_biz("", "今日配额不足"),
            Some(ErrorClass::Quota)
        );
        assert_eq!(ErrorClass::Transient.refine_biz("", "tokens per minute limit"), None);
        assert_eq!(
            ErrorClass::Transient.refine_biz("", "token 无效，请重新登录"),
            Some(ErrorClass::Auth)
        );
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
