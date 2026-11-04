//! 智谱清言（chatglm.cn）adapter——v2 第一个非透传 upstream_kind。
//!
//! 一手来源：aigw-panel-rs/src/relay/web.rs（复刻 glm2api yxc0915 协议）：
//! - `POST {base}/user-api/user/refresh`：refresh_token → access_token（30min 缓存）；
//! - `POST {base}/backend-api/assistant/stream`：X-Sign 头组（aigw-crypto::glm_build_sign）；
//!   body 为 chatglm 私有格式（assistant_id/conversation_id/messages/model/stream）；
//! - 响应 SSE：`parts` 是**全量快照**（非增量），取最后一个非空 content，
//!   差分转 OpenAI delta 流；非流式聚合最后快照。

use crate::{RelayError, SseEvent, SseParser, StreamStat};
use aigw_core::ErrorClass;
use aigw_crypto::glm_build_sign;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const GLM_ASSISTANT_ID: &str = "65940acff94777010aa6b796";
const UA_CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const AT_TTL_SECS: i64 = 1800;

fn tok_cache() -> &'static Mutex<HashMap<String, (String, i64)>> {
    static C: OnceLock<Mutex<HashMap<String, (String, i64)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 解析 glm 模型变体：`-think` / `-search` / `-think-search` → (think, search, 实际模型)。
pub fn glm_variants(model: &str) -> (bool, bool, String) {
    let mut think = false;
    let mut search = false;
    let mut m = model.to_string();
    loop {
        if m.ends_with("-think") {
            think = true;
            m.truncate(m.len() - "-think".len());
        } else if m.ends_with("-search") {
            search = true;
            m.truncate(m.len() - "-search".len());
        } else {
            break;
        }
    }
    (think, search, m)
}

/// OpenAI 请求体 → chatglm 请求体（messages 只留 user/assistant）。
pub fn build_glm_request(openai_body: &Value) -> Result<Value, RelayError> {
    let model = openai_body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("glm-4");
    if model.contains("image") || model.contains("cogview") {
        return Err(RelayError {
            status: 400,
            class: ErrorClass::Transient,
            body: "智谱图片生成不支持经 /v1/chat/completions".into(),
        });
    }
    let (think, search, actual) = glm_variants(model);
    let conv: Vec<Value> = openai_body
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .filter(|m| {
                    matches!(
                        m.get("role").and_then(|r| r.as_str()),
                        Some("user") | Some("assistant")
                    )
                })
                .map(|m| {
                    json!({
                        "role": m.get("role").and_then(|r| r.as_str()).unwrap_or(""),
                        "content": m.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut body = json!({
        "assistant_id": GLM_ASSISTANT_ID,
        "conversation_id": "",
        "messages": conv,
        "model": actual,
        "stream": true,
        "prompt": "",
        "tools": [],
    });
    if think {
        body["thinking"] = json!({"type": "enabled"});
    }
    if search {
        body["search"] = json!({"enable": true});
    }
    Ok(body)
}

/// 从 chatglm SSE 帧提取 parts 全量快照的最后非空 content。
pub fn parts_content(v: &Value) -> Option<String> {
    let parts = v.get("parts")?.as_array()?;
    let mut last = String::new();
    for p in parts {
        if let Some(c) = p.get("content").and_then(|c| c.as_str()) {
            if !c.is_empty() {
                last = c.to_string();
            }
        } else if let Some(c) = p.as_str() {
            if !c.is_empty() {
                last = c.to_string();
            }
        }
    }
    if last.is_empty() {
        None
    } else {
        Some(last)
    }
}

/// 全量快照差分 → 增量。cur 以 prev 为前缀返回增量；相同返回 None；否则全量容错。
pub fn diff_snap(prev: &str, cur: &str) -> Option<String> {
    if cur == prev {
        return None;
    }
    if cur.starts_with(prev) {
        Some(cur[prev.len()..].to_string())
    } else {
        Some(cur.to_string())
    }
}

/// 按**字符**截断（不能按字节切：上游错误体常是中文，
/// `&text[..n]` 会落在 UTF-8 字符中间 panic 掉 tokio worker）。
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

async fn access_token(
    spec: &ProviderSpec,
    http: &reqwest::Client,
    refresh_token: &str,
) -> Result<String, RelayError> {
    {
        let mut cache = tok_cache().lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs();
        if cache.len() > 64 {
            cache.retain(|_, v| now < v.1);
        }
        if let Some((at, exp)) = cache.get(refresh_token) {
            if now < *exp {
                return Ok(at.clone());
            }
        }
    }
    let resp = http
        .post(format!("{}/user-api/user/refresh", spec.base_url))
        .header("Authorization", format!("Bearer {refresh_token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", UA_CHROME)
        .body("{}")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| RelayError {
            status: 0,
            body: format!("刷新 token 请求失败：{e}"),
            class: ErrorClass::Transient,
        })?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(RelayError {
            status: 401,
            class: ErrorClass::Auth,
            body: format!("刷新 token HTTP {status}：{}", head(&text, 200)),
        });
    }
    let d: Value = serde_json::from_str(&text).map_err(|_| RelayError {
        status: 401,
        class: ErrorClass::Auth,
        body: format!("刷新 token 返回非 JSON：{}", head(&text, 200)),
    })?;
    let at = d
        .pointer("/data/access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if at.is_empty() {
        return Err(RelayError {
            status: 401,
            class: ErrorClass::Auth,
            body: format!("刷新 token 未返回 access_token：{}", head(&text, 200)),
        });
    }
    tok_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(refresh_token.to_string(), (at.clone(), now_secs() + AT_TTL_SECS));
    Ok(at)
}

fn signed_headers(at: &str) -> Vec<(String, String)> {
    let (timestamp, nonce, sign) = glm_build_sign();
    vec![
        ("Authorization".into(), format!("Bearer {at}")),
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "text/event-stream".into()),
        ("User-Agent".into(), UA_CHROME.into()),
        ("X-Nonce".into(), nonce),
        ("X-Sign".into(), sign),
        ("X-Timestamp".into(), timestamp),
    ]
}

async fn post_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    at: &str,
    glm_body: &Value,
) -> Result<reqwest::Response, RelayError> {
    let url = format!("{}/backend-api/assistant/stream", spec.base_url);
    let mut rb = http
        .post(&url)
        .timeout(Duration::from_secs(180))
        .body(glm_body.to_string());
    for (k, v) in signed_headers(at) {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.map_err(|e| RelayError {
        status: 0,
        body: format!("network: {e}"),
        class: ErrorClass::Transient,
    })?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let text = resp.text().await.unwrap_or_default();
        return Err(RelayError {
            status,
            class: ErrorClass::from_status(status),
            body: text.chars().take(400).collect(),
        });
    }
    Ok(resp)
}

/// 非流式：SSE 收齐 → 聚合最后快照 → OpenAI chat.completion。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    if secret.is_empty() {
        return Err(RelayError {
            status: 401,
            class: ErrorClass::Auth,
            body: "缺少 chatglm_refresh_token".into(),
        });
    }
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let openai_model = openai
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("glm-4")
        .to_string();
    let glm_body = build_glm_request(&openai)?;
    let at = access_token(spec, http, secret).await?;
    let resp = post_stream(http, spec, &at, &glm_body).await?;

    let mut parser = SseParser::new();
    let mut stream = resp.bytes_stream();
    let mut last = String::new();
    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| RelayError {
            status: 0,
            body: format!("mid-stream: {e}"),
            class: ErrorClass::Transient,
        })?;
        for ev in parser.feed(&chunk) {
            if let SseEvent::Data(v) = ev {
                if let Some(c) = parts_content(&v) {
                    last = c;
                }
            }
        }
    }
    if last.is_empty() {
        return Err(RelayError {
            status: 502,
            class: ErrorClass::Transient,
            body: "智谱上游未返回可解析内容（签名或请求体不符）".into(),
        });
    }
    Ok(json!({
        "id": format!("chatcmpl-glm-{}", std::process::id()),
        "object": "chat.completion",
        "created": now_secs(),
        "model": openai_model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": last},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0},
    }))
}

/// 流式：parts 全量快照 → 差分 delta → OpenAI chunk 流（结尾 [DONE]）。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<
    (
        impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin + Send + 'static,
        mpsc::Receiver<StreamStat>,
    ),
    RelayError,
> {
    if secret.is_empty() {
        return Err(RelayError {
            status: 401,
            class: ErrorClass::Auth,
            body: "缺少 chatglm_refresh_token".into(),
        });
    }
    let openai: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let openai_model = openai
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("glm-4")
        .to_string();
    let glm_body = build_glm_request(&openai)?;
    let at = access_token(spec, http, secret).await?;
    let resp = post_stream(http, spec, &at, &glm_body).await?;

    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let in_bytes = body.len();
    tokio::spawn(async move {
        let mut parser = SseParser::new();
        let mut prev = String::new();
        let mut stat = StreamStat::default();
        let mut out_chars: usize = 0;
        let chunk_id = format!("chatcmpl-glm-{}", std::process::id());
        let mut stream = resp.bytes_stream();
        let emit = |bytes_tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
                    delta: &str,
                    finish: Option<&str>| {
            let payload = json!({
                "id": chunk_id,
                "object": "chat.completion.chunk",
                "created": now_secs(),
                "model": openai_model,
                "choices": [{"index": 0, "delta": {"content": delta}, "finish_reason": finish}],
            });
            let frame = format!("data: {payload}\n\n");
            bytes_tx.try_send(Ok(Bytes::from(frame))).is_ok()
        };
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    for ev in parser.feed(&chunk) {
                        if let SseEvent::Data(v) = ev {
                            if let Some(cur) = parts_content(&v) {
                                if let Some(delta) = diff_snap(&prev, &cur) {
                                    out_chars += delta.chars().count();
                                    if !emit(&bytes_tx, &delta, None) {
                                        stat.error = Some(ErrorClass::Transient);
                                        stat.error_msg = "downstream closed".into();
                                        break;
                                    }
                                }
                                prev = cur;
                            }
                        }
                    }
                    if stat.error.is_some() {
                        break;
                    }
                }
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {e}");
                    break;
                }
            }
        }
        if stat.error.is_none() {
            let _ = emit(&bytes_tx, "", Some("stop"));
            let _ = bytes_tx.send(Ok(Bytes::from("data: [DONE]\n\n"))).await;
            stat.finish_reason = Some("stop".into());
            stat.saw_done = true;
        }
        let (tin, tout) = crate::estimate_stream_tokens(in_bytes, out_chars);
        stat.tokens_in = tin;
        stat.tokens_out = tout;
        let _ = tx.send(stat).await;
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    Ok((stream, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_parse() {
        assert_eq!(glm_variants("glm-4.5"), (false, false, "glm-4.5".into()));
        assert_eq!(glm_variants("glm-4.5-think"), (true, false, "glm-4.5".into()));
        assert_eq!(
            glm_variants("glm-4.5-think-search"),
            (true, true, "glm-4.5".into())
        );
        assert_eq!(glm_variants("glm-4.5-search"), (false, true, "glm-4.5".into()));
    }

    #[test]
    fn build_request_maps_messages_and_flags() {
        let openai = json!({
            "model": "glm-4.5-think",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "yo"},
                {"role": "tool", "content": "skip"},
            ]
        });
        let b = build_glm_request(&openai).unwrap();
        assert_eq!(b["assistant_id"], GLM_ASSISTANT_ID);
        assert_eq!(b["model"], "glm-4.5");
        assert_eq!(b["thinking"]["type"], "enabled");
        assert!(b["search"]["enable"].is_null());
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "system/tool 应被过滤");
    }

    #[test]
    fn parts_snapshot_last_nonempty() {
        let v = json!({"parts": [
            {"content": "第一段"},
            {"content": "第一段第二段"},
            {"content": ""},
        ]});
        assert_eq!(parts_content(&v).as_deref(), Some("第一段第二段"));
        assert_eq!(parts_content(&json!({"parts": []})), None);
    }

    #[test]
    fn head_never_splits_multibyte_chars() {
        // 中文错误体：按字节切第 200 位会落在字符中间 panic，head() 必须安全。
        let zh = "登录已过期".repeat(100);
        let h = head(&zh, 200);
        assert_eq!(h.chars().count(), 200);
        assert!(zh.starts_with(&h));
        assert_eq!(head("abc", 10), "abc");
        assert_eq!(head("", 10), "");
    }

    #[test]
    fn diff_snap_scenarios() {
        assert_eq!(diff_snap("abc", "abcd").as_deref(), Some("d"));
        assert_eq!(diff_snap("abc", "abc"), None);
        // 前缀不匹配 → 全量容错
        assert_eq!(diff_snap("abc", "xyz").as_deref(), Some("xyz"));
    }
}
