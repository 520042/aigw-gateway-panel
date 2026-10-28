//! 上游错误 → 客户端能识别的响应。
//!
//! 从 lib.rs 拆出（2026-10-26 第二十轮）。这一簇全是纯函数：入参是上游状态/类别/
//! 响应体，出参是 HTTP 响应或一句人话提示。不碰 AppState，因此可单测。
//!
//! ★ 两条纪律：
//! ① **状态码必须落在客户端能识别的区间**。上游返回 200 但 body 是登录页 HTML
//!   （网页版逆向最常见的失效形态）时，绝不能把 200 透出去——SDK 会当成功解析，
//!   然后抛一个跟真实原因毫无关系的解析异常。`upstream_status` 负责这道闸门。
//! ② **提示必须指向下一步动作**。只说「上游错误」等于没说：额度、鉴权、限流、
//!   地域四类问题的处理办法完全不同（换号 / 重登 / 等一会 / 换出口），
//!   `upstream_hint` 按类别分别给。

use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// 上游失败要落成「客户端能识别的错误码」。
///
/// 踩过的坑：上游返回 HTTP 200 但 body 是 HTML / 非 JSON（网页版逆向最常见的
/// 失效形态），`RelayError.status` 会被填成 200，于是 `openai_error(200, ...)`
/// 回了一个 **HTTP 200 + 错误 JSON**——OpenAI SDK 把它当成功响应解析，直接抛
/// 解析异常，调用方完全看不到"上游返回了登录页"这个真实原因。
pub(crate) fn upstream_status(status: u16) -> u16 {
    match status {
        // 0 = 连接/超时失败；1xx/2xx/3xx 都不是合法错误码
        0..=399 => 502,
        s => s,
    }
}

/// HTTP 状态 → OpenAI 兼容的 `error.type`。
///
/// ★ 旧实现不管什么状态一律写死 `"upstream_error"`：400（客户端 JSON 语法错）、401（还没接
///   账号）、429（本地并发闸）、503（未知模型）全被当成「上游故障」。按 `error.type` 决定
///   「重试 / 降级 / 换 Key」的客户端（LiteLLM、Claude Code、各种自研网关）会把**自己的参数
///   错误**当服务端抖动反复重试，日志里满是我们返回的错却显示上游有问题。
pub(crate) fn error_type(status: u16) -> &'static str {
    match status {
        401 => "authentication_error",
        402 => "insufficient_quota",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        // 501：平台 format 不是 openai（本面板不做协议转换）——是调用方选错了端点，不是上游挂了
        501 => "invalid_request_error",
        // 504：上游 200 之后一直不给首帧（连接打开但空转/半死），是网关侧判定出来的超时，
        // 用 gateway_timeout 比笼统的 upstream_error 更能让运维一眼看出「不是上游返回了错，
        // 是它压根没说话」。
        504 => "gateway_timeout",
        400..=499 => "invalid_request_error",
        _ => "upstream_error",
    }
}

pub(crate) fn openai_error(status: u16, msg: &str) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (
        code,
        Json(json!({
            "error": {"message": msg, "type": error_type(status), "code": status}
        })),
    )
        .into_response()
}

/// 同 openai_error，但允许显式指定 error.type 与字符串 error.code。
///
/// 用于「错误码必须能被客户端当成**永久性**失败」的少数场景：OpenAI 官方对
/// 不存在的模型回 404 + type=invalid_request_error + code=model_not_found，
/// 而按状态码推导出来的 404 -> not_found_error / code:404 与之不同。
pub(crate) fn openai_error_coded(status: u16, typ: &str, code: &str, msg: &str) -> Response {
    let http = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (
        http,
        Json(json!({
            "error": {"message": msg, "type": typ, "code": code}
        })),
    )
        .into_response()
}

/// 上游错误透传：上游本来就是 OpenAI 错误信封时**原样回**，不要把整段 JSON 塞进 message。
///
/// ★ 旧实现把上游的 `{"error":{…}}` 整段当 message 传进 `openai_error`：
///   当纯文本 message，客户端真正要读的 `error.type`（`insufficient_quota` /
///   `context_length_exceeded` / `invalid_api_key`）与 `error.code` 全部丢失，
///   只剩一串转义后的 JSON，排障时完全看不出上游到底说了什么。
/// 在错误 message 尾巴追加「建议」；已带建议的不重复追加。
pub(crate) fn msg_with_hint(msg: &str, hint: Option<&str>) -> String {
    match hint {
        Some(h) if !msg.contains("建议：") => format!("{msg}（建议：{h}）"),
        _ => msg.to_string(),
    }
}

/// 造一个 OpenAI 错误信封（message + type + code [+ hint]）。
pub(crate) fn upstream_error_json(status: u16, message: &str, hint: Option<&str>) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut err = json!({
        "message": msg_with_hint(message, hint),
        "type": error_type(status),
        "code": status,
    });
    if let (Some(h), Some(obj)) = (hint, err.as_object_mut()) {
        obj.insert("hint".to_string(), json!(h));
    }
    (code, Json(json!({ "error": err }))).into_response()
}

pub(crate) fn upstream_error_response(status: u16, body: &str, hint: Option<&str>) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let text = body.trim();
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        // 有的上游把 error 写成裸字符串：直接当 message
        if let Some(s) = v.get("error").and_then(|e| e.as_str()) {
            return upstream_error_json(status, s, hint);
        }
        if v.get("error").map(|e| e.is_object()).unwrap_or(false) {
            let mut v = v;
            if let Some(obj) = v.get_mut("error").and_then(|e| e.as_object_mut()) {
                // 不少上游只回 message 不写 code，客户端普遍读 code 判类型，这里补 HTTP 状态
                if !obj.contains_key("code") {
                    obj.insert("code".to_string(), json!(status));
                }
                // 建议直接写进同一个信封：message 给人看，hint 给前端做强调
                if let Some(h) = hint {
                    let msg = obj
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !msg.contains("建议：") {
                        obj.insert("message".to_string(), json!(format!("{msg}（建议：{h}）")));
                    }
                    obj.insert("hint".to_string(), json!(h));
                }
            }
            return (code, Json(v)).into_response();
        }
        // 非标准形状（{"message":…} / {"errno":…} / 裸数组）：原文留在 error.raw，别丢信息
        let mut err = json!({
            "message": msg_with_hint(&format!("上游返回 {status}"), hint),
            "type": error_type(status),
            "code": status,
            "raw": v,
        });
        if let (Some(h), Some(obj)) = (hint, err.as_object_mut()) {
            obj.insert("hint".to_string(), json!(h));
        }
        return (code, Json(json!({ "error": err }))).into_response();
    }
    if text.is_empty() {
        return upstream_error_json(status, &format!("上游返回 {status}，响应体为空"), hint);
    }
    // 逐字符截断：`&text[..n]` 是**字节**切片，中文/emoji 落在字符中间会 panic
    let clipped: String = text.chars().take(600).collect();
    let more = if text.chars().count() > 600 {
        "…（已截断）"
    } else {
        ""
    };
    upstream_error_json(status, &format!("上游返回 {status}：{clipped}{more}"), hint)
}

// ---------------------------------------------------------------------------
// 上游失败的「可行动建议」
// ---------------------------------------------------------------------------

/// 区域的中文名（给用户看，不把 cn/intl 直接抛给他）。
pub(crate) fn region_label(region: &str) -> &'static str {
    match region {
        "cn" => "国内站",
        "intl" => "国际站",
        _ => "全球站",
    }
}

/// 上游错误码/文案 → 用户下一步该做什么。
///
/// 动机：401/402/429/连不上 在面板里原本都是一条「上游返回 N：原文」，用户看不出
/// 该**重新登录**、该**加账号**、还是该**开代理**——尤其 Kimi/MiniMax/智谱/Qoder
/// 这类国内国际两套账号体系的平台，最容易把「代理没开」误判成「凭据失效」。
pub(crate) fn upstream_hint(spec: &ProviderSpec, status: u16, class: Option<ErrorClass>, body: &str) -> Option<String> {
    let low = body.to_ascii_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| low.contains(k));
    let proxy_mode = aigw_relay::proxy::current().mode;
    // ① 网络层失败（连不上/超时/DNS）：先怀疑代理模式，别让人去重登
    if has(&[
        "error sending request",
        "dns error",
        "failed to lookup address",
        "connection refused",
        "connection reset",
        "tcp connect error",
        "operation timed out",
        "timed out",
    ]) {
        return Some(match spec.region.as_str() {
            "intl" => format!(
                "{}（{}）在国内一般连不上，当前代理模式是 {}：去「设置 → 网络代理」改成 global 或 bypass_cn，并确认本地代理在跑",
                spec.name,
                region_label(&spec.region),
                proxy_mode
            ),
            _ => format!(
                "连不上上游（当前代理模式 {}）：{} 建议直连，把代理改成 direct 或 bypass_cn，别用 global（那会把国内请求也绕出国）",
                proxy_mode,
                region_label(&spec.region)
            ),
        });
    }
    // ①.5 车道级封锁/地域封锁：必须在鉴权分支**之前**判，否则 403 会掉进下面的
    // 「去重新登录」——匿名车道没有凭据可重登，用户会照着做无用功。
    if matches!(class, Some(ErrorClass::LaneLocked))
        || has(&["freetiererror", "free tier can only be used from within"])
    {
        return Some(
            "上游按客户端指纹封锁了整条免 Key 车道（不是你的凭据问题，也无需重新登录）：这是上游的准入策略，过一段时间或换一条免费车道（kilo / pollinations）再试"
                .to_string(),
        );
    }
    if matches!(class, Some(ErrorClass::Region))
        || has(&["regionerror", "not available in your country"])
    {
        return Some(format!(
            "{} 不对该地区提供这个模型（地域封锁，与凭据无关）：换一条车道，或把「设置 → 网络代理」改成 global/bypass_cn 换出口再试",
            spec.name
        ));
    }
    // ② 鉴权/登录态
    if matches!(class, Some(ErrorClass::Auth))
        || status == 401
        || status == 403
        || has(&[
            "unauthor",
            "invalid token",
            "token expire",
            "token is not active",
            "not logged in",
            "login required",
            "session expired",
            "authentication is invalid",
            "jwt",
            "forbidden",
        ])
    {
        let mut s = format!("到「接入源 → {}」点重新登录/重新粘贴凭据", spec.name);
        if spec.region != "global" {
            s.push_str(&format!(
                "；注意{}与另一区的账号、Key 互不通用（两边是独立账号体系），别拿另一个区的凭据来试",
                region_label(&spec.region)
            ));
        }
        s.push_str("；登录成功后把失效的旧账号删掉，否则账号池还会轮换到它");
        return Some(s);
    }
    // ③ 额度/积分
    if matches!(class, Some(ErrorClass::Quota))
        || status == 402
        || has(&["quota", "insufficient", "balance", "credit", "billing", "额度", "积分", "欠费"])
    {
        return Some(
            "额度/积分用尽：给这个平台再加一个账号（面板会在账号间轮换），或换免费车道（kilo / pollinations / anon-zen 免凭据直接可用）"
                .to_string(),
        );
    }
    // ④ 限流
    if matches!(class, Some(ErrorClass::Rate))
        || status == 429
        || has(&["rate limit", "too many request", "requests per", "too frequent", "限流", "频率"])
    {
        // ★ 2026-10-26（审计优化）：OpenCode Zen 免费池的 429 是 **IP 级日额度**
        //（FreeUsageLimitError，实测 2026-10-26），不是每分钟限速——
        // "等 30–60 秒"对它无效，正确动作是换出口 IP（面板「⇅ 代理设置」切代理）
        // 或等额度日重置。普通 429 才走"短等重试"那套。
        if spec.id == "anon-zen"
            || spec.upstream_kind == "opencode"
            || has(&["free usage limit", "opencode"])
        {
            return Some(
                "OpenCode 免费池的本机 IP 今日额度已用完（指纹协议已通过，仅配额问题）：\
                 到「⇅ 代理设置」切换代理/节点换个出口 IP 立即可用，或等额度重置；\
                 期间可先用车道 kilo / pollinations（免凭据）顶上"
                    .to_string(),
            );
        }
        return Some(
            "触发限流：等 30–60 秒再试；同一平台多存几个账号能明显减少撞限流（面板自动轮换）".to_string(),
        );
    }
    // ⑤ 模型名不被接受
    if status == 404
        || has(&[
            "model not found",
            "model_not_found",
            "unknown model",
            "invalid model",
            "no such model",
            "does not exist",
        ])
    {
        return Some(format!(
            "该平台不认这个模型名：先看 /v1/models 里有没有它；要走指定平台就写 模型id@{}；官方带日期全名（…-20250929）与点号版本面板会自动归一，但仍以平台声明的为准",
            spec.id
        ));
    }
    // ⑥ 上游自己故障/网络抖动
    if status >= 500 || matches!(class, Some(ErrorClass::Transient)) {
        return Some(
            "上游自己故障或网络抖动（不是面板的问题）：稍后重试；连续失败会让该账号进冷却，冷却结束自动恢复"
                .to_string(),
        );
    }
    // ⑦ 请求被上游拒绝（参数/内容审查）
    if matches!(class, Some(ErrorClass::Rejected)) || status == 400 {
        return Some(
            "上游拒了这次请求：多为该平台不支持这个参数（tools / 图片 / 超长上下文）或触发内容审查，换个模型或缩短上下文再试"
                .to_string(),
        );
    }
    None
}

// ---- 另两种协议的错误形态 ----
//
// Anthropic Messages 用 {"type":"error","error":{...}}，Responses 用
// {"error":{...,"code":<数字状态>}}。两者都不能拿 OpenAI 的 {"error":{"message"}}
// 糊过去：Claude Code / DSH 这类客户端是按 type 字段决定要不要重试的，
// 类型不对就会把「额度耗尽」当成「参数写错」一直重试，或反过来直接放弃。

pub(crate) fn anthropic_error(status: u16, msg: &str) -> Response {
    let etype = match status {
        400 => "invalid_request_error",
        // 413（请求体超上限）也是请求本身的问题，归 invalid_request_error。
        // 漏了它会落到 api_error —— 客户端按「上游故障」重试一个永远超限的请求。
        413 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        503 => "overloaded_error",
        _ => "api_error",
    };
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(json!({"type": "error", "error": {"type": etype, "message": msg}})),
    )
        .into_response()
}

pub(crate) fn responses_error(status: u16, msg: &str) -> Response {
    let kind = match status {
        400 => "invalid_request_error",
        413 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        _ => "api_error",
    };
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
    (
        code,
        Json(json!({"error": {"message": msg, "type": kind, "code": status}})),
    )
        .into_response()
}

pub(crate) fn extract_upstream_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(m) = v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
            return m.to_string();
        }
        if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
            return m.to_string();
        }
        if let Some(m) = v.get("error").and_then(|e| e.as_str()) {
            return m.to_string();
        }
    }
    let clipped: String = body.chars().take(300).collect();
    if clipped.is_empty() {
        "上游返回错误（无响应体）".to_string()
    } else {
        clipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_error_statuses_become_502() {
        // 上游 200 + 登录页 HTML（网页版逆向最常见的失效形态）：透出去会让 SDK
        // 当成功解析、抛一个跟真实原因无关的解析异常。0=连接失败同理。
        assert_eq!(upstream_status(200), 502);
        assert_eq!(upstream_status(0), 502);
        assert_eq!(upstream_status(204), 502);
        assert_eq!(upstream_status(301), 502);
        // 真错误码原样透
        assert_eq!(upstream_status(401), 401);
        assert_eq!(upstream_status(429), 429);
        assert_eq!(upstream_status(503), 503);
    }

    #[test]
    fn extract_upstream_message_reads_nested_and_flat_shapes() {
        // 三种上游常见摆放方式，都要拿到人话而不是整坨 JSON
        assert_eq!(
            extract_upstream_message(r#"{"error":{"message":"额度不足"}}"#),
            "额度不足"
        );
        assert_eq!(extract_upstream_message(r#"{"message":"boom"}"#), "boom");
        assert_eq!(extract_upstream_message(r#"{"error":"gone"}"#), "gone");
        // 非 JSON：原文截断，不空手而归
        assert!(extract_upstream_message("<html>登录页</html>").contains("登录页"));
        // 空 body 也要有说法（「无响应体」比空串好排查）
        assert!(extract_upstream_message("").contains("无响应体"));
    }

    #[test]
    fn protocol_errors_use_their_own_envelope() {
        // Anthropic：type=error 包裹；Responses：error.code 是数字状态。
        // 拿 OpenAI 的形状糊过去会让客户端按错误的 type 决定重试策略。
        let r = anthropic_error(429, "限流");
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        let r2 = responses_error(401, "未授权");
        assert_eq!(r2.status(), StatusCode::UNAUTHORIZED);
        // 非法状态码不能 panic（from_u16 失败要回落，不能让请求线程炸掉）
        let _ = anthropic_error(999, "weird");
        let _ = responses_error(999, "weird");
    }

    #[test]
    fn msg_with_hint_appends_only_when_present() {
        assert_eq!(msg_with_hint("基础错误", None), "基础错误");
        let m = msg_with_hint("基础错误", Some("处理办法：重登"));
        assert!(m.starts_with("基础错误"), "{m}");
        assert!(m.contains("处理办法：重登"), "{m}");
    }
}
