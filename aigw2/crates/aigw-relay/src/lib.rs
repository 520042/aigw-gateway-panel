//! aigw-relay：上游转发引擎。
//!
//! 架构分派（声明式 spec 驱动）：
//! - `upstream_kind=openai`：**同协议字节透传**（ops120/zcode2api 原则），控制面并行
//!   SSE 解析归一记账；
//! - `upstream_kind=chatglm`：**adapter 转换**（OpenAI→chatglm 私有协议→OpenAI），
//!   签名走 aigw-crypto，parts 全量快照差分为 delta（glm.rs）。
//!
//! 超时四项（旧项目沉淀）：connect 10s / read 90s / 流式不设总时长 / 非流式 180s。

pub mod glm;
pub mod auth_login;
pub mod proxy;

use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::mpsc;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(90);
const NONSTREAM_TOTAL: Duration = Duration::from_secs(180);

#[derive(Debug)]
pub struct RelayError {
    pub status: u16,
    pub body: String,
    pub class: ErrorClass,
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "upstream {}: {} ({:?})", self.status, self.body, self.class)
    }
}

pub struct OutClient {
    http: reqwest::Client,
}

impl OutClient {
    /// 暴露内部 HTTP 客户端（供 aigw-server 拉聚合源等使用）。
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn new() -> Self {
        let mut b = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT);
        // 出站代理：direct/global/bypass_cn（proxy::current 全局配置，热更新生效）
        if let Some(p) = proxy::proxy_for_current() {
            b = b.proxy(p);
        } else {
            b = b.no_proxy();
        }
        let http = b.build().expect("out client build");
        OutClient { http }
    }

    fn chat_url(spec: &ProviderSpec) -> String {
        format!("{}{}", spec.base_url, spec.chat_path())
    }

    fn apply_headers(
        rb: reqwest::RequestBuilder,
        spec: &ProviderSpec,
        secret: &str,
        body: bool,
    ) -> reqwest::RequestBuilder {
        let mut rb = rb;
        for (k, v) in &spec.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        // 账号凭据注入按认证类型：cookie 型放 Cookie 头；其余 spec 静态头无 Authorization 时补 Bearer
        if !secret.is_empty() {
            match spec.auth {
                aigw_registry::AuthKind::Cookie => {
                    rb = rb.header("Cookie", secret);
                }
                _ => {
                    if !spec.headers.contains_key("Authorization") {
                        // Cline WorkOS 账号：secret=`workos:{at}:{rt}` → 只取 `workos:{at}` 作 Bearer
                        // （cline2api pool.go:380 一手：上游 Authorization: Bearer workos:<accessToken>）
                        if spec.id == "cline" && secret.starts_with("workos:") {
                            let at = secret.split(':').take(2).collect::<Vec<_>>().join(":");
                            rb = rb.header("Authorization", format!("Bearer {at}"));
                        } else {
                            rb = rb.header("Authorization", format!("Bearer {secret}"));
                        }
                    }
                }
            }
        }
        if body {
            rb = rb.header("Content-Type", "application/json");
        }
        rb
    }

    /// CodeArts `SDK-HMAC-SHA256` 签名注入（signer="codearts"，账号 secret 存 `AK:SK`）。
    /// 对齐 codearts2api `_hmac_headers`：benefit 免费通道多签
    /// `maas_type`/`model-id`/`model-name` 三头（新模型默认走 benefit，上游零改动可用）。
    fn apply_codearts_sign(
        rb: reqwest::RequestBuilder,
        spec: &ProviderSpec,
        secret: &str,
        body: &[u8],
    ) -> reqwest::RequestBuilder {
        if spec.signer != "codearts" {
            return rb;
        }
        let Some((ak, sk)) = secret.split_once(':') else {
            return rb;
        };
        if ak.is_empty() || sk.is_empty() {
            return rb;
        }
        let model = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        let sdk_date = aigw_crypto::codearts_sdk_date();
        let host = reqwest::Url::parse(&spec.base_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_else(|| "snap-access.cn-north-4.myhuaweicloud.com".to_string());
        let mut hs: Vec<(String, String)> = vec![
            ("host".into(), host),
            ("content-type".into(), "application/json".into()),
            ("x-sdk-date".into(), sdk_date.clone()),
        ];
        if !model.is_empty() {
            hs.push(("maas_type".into(), "benefit".into()));
            hs.push(("model-id".into(), model.clone()));
            hs.push(("model-name".into(), model));
        }
        let canonical_uri = format!("{}/", spec.chat_path()); // 必须带尾斜杠
        let auth = aigw_crypto::codearts_build_sign(
            "POST",
            &canonical_uri,
            "",
            &hs,
            body,
            ak,
            sk,
            &sdk_date,
        );
        rb.header("x-sdk-date", sdk_date)
            .header("maas_type", "benefit")
            .header("Authorization", auth)
    }

    // ================= 顶层分派（声明式 spec 驱动） =================

    /// 非流式转发：按 upstream_kind 分派。
    pub async fn forward_nonstream(
        &self,
        spec: &ProviderSpec,
        secret: &str,
        body: &[u8],
    ) -> Result<Value, RelayError> {
        match spec.upstream_kind.as_str() {
            "chatglm" => glm::chat_nonstream(&self.http, spec, secret, body).await,
            _ => self.post_openai_nonstream(spec, secret, body).await,
        }
    }

    /// 流式转发：按 upstream_kind 分派。返回 `(下游流, 统计接收端)`。
    pub async fn forward_stream(
        &self,
        spec: &ProviderSpec,
        secret: &str,
        body: &[u8],
    ) -> Result<
        (
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
            >,
            mpsc::Receiver<StreamStat>,
        ),
        RelayError,
    > {
        match spec.upstream_kind.as_str() {
            "chatglm" => {
                let (s, rx) = glm::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            _ => {
                let (s, rx) = self.post_openai_stream(spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
        }
    }

    // ================= openai 透传 =================

    /// 非流式透传：POST {base}{chat_path}，总时长 180s，2xx 假成功指纹检查。
    pub async fn post_openai_nonstream(
        &self,
        spec: &ProviderSpec,
        secret: &str,
        body: &[u8],
    ) -> Result<Value, RelayError> {
        let url = Self::chat_url(spec);
        let mut rb = Self::apply_headers(self.http.post(&url), spec, secret, true);
        rb = Self::apply_codearts_sign(rb, spec, secret, body);
        let rb = rb
            .timeout(NONSTREAM_TOTAL)
            .body(body.to_vec());
        let resp = rb.send().await.map_err(|e| RelayError {
            status: 0,
            body: format!("network: {e}"),
            class: ErrorClass::Transient,
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(RelayError {
                status,
                class: ErrorClass::from_status(status),
                body: text.chars().take(800).collect(),
            });
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| RelayError {
            status,
            class: ErrorClass::Transient,
            body: format!("bad json: {e}; head={}", text.chars().take(300).collect::<String>()),
        })?;
        // 假成功指纹（spec.biz_err 关键词命中顶层 code/message/desc）
        if let Some(cls) = biz_fingerprint(spec, &v) {
            return Err(RelayError {
                status: 200,
                class: cls,
                body: text.chars().take(300).collect(),
            });
        }
        Ok(v)
    }

    /// 流式透传：SSE 逐字节原样下发；并行解析归一，结束时经 channel 回传统计。
    pub async fn post_openai_stream(
        &self,
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
        let url = Self::chat_url(spec);
        let mut rb = Self::apply_headers(self.http.post(&url), spec, secret, true);
        rb = Self::apply_codearts_sign(rb, spec, secret, body);
        let rb = rb.body(body.to_vec()); // 流式：不设总时长（read_timeout 兜底包间 idle）
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
                body: text.chars().take(800).collect(),
            });
        }
        Ok(tap_passthrough(resp))
    }

    /// live 模型清单：GET {base}{models_path}，可选 isFree 过滤（kilo：id 带 org/ 前缀保留全名）。
    pub async fn fetch_models(&self, spec: &ProviderSpec) -> Result<Vec<String>, RelayError> {
        let url = format!("{}{}", spec.base_url, spec.models_path());
        let rb = Self::apply_headers(self.http.get(&url), spec, "", false)
            .timeout(Duration::from_secs(20));
        let resp = rb.send().await.map_err(|e| RelayError {
            status: 0,
            body: format!("network: {e}"),
            class: ErrorClass::Transient,
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(RelayError {
                status,
                class: ErrorClass::from_status(status),
                body: text.chars().take(400).collect(),
            });
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| RelayError {
            status,
            class: ErrorClass::Transient,
            body: format!("bad json: {e}"),
        })?;
        let mut ids: Vec<String> = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .filter(|m| {
                        if spec.models_filter == "isFree" {
                            m.get("isFree").and_then(|x| x.as_bool()).unwrap_or(false)
                        } else {
                            true
                        }
                    })
                    .filter_map(|m| m.get("id").and_then(|x| x.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort();
        Ok(ids)
    }
}

impl Default for OutClient {
    fn default() -> Self {
        Self::new()
    }
}

/// 假成功指纹：2xx JSON 顶层 code/message/desc 命中 spec.biz_err 关键词 → Auth。
fn biz_fingerprint(spec: &ProviderSpec, v: &Value) -> Option<ErrorClass> {
    if spec.biz_err.is_empty() {
        return None;
    }
    let hay = [
        v.get("code").and_then(|c| c.as_str()).unwrap_or(""),
        v.get("message").and_then(|m| m.as_str()).unwrap_or(""),
        v.get("desc").and_then(|d| d.as_str()).unwrap_or(""),
    ]
    .join(" ");
    if hay.is_empty() {
        return None;
    }
    spec.biz_err
        .iter()
        .find(|k| hay.contains(k.as_str()))
        .map(|_| ErrorClass::Auth)
}

/// 上游 SSE → 原样透传 + tap 统计（mpsc 回传）。
fn tap_passthrough(
    resp: reqwest::Response,
) -> (
    impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin + Send + 'static,
    mpsc::Receiver<StreamStat>,
) {
    let (tx, rx) = mpsc::channel::<StreamStat>();
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let mut parser = SseParser::new();
        let mut stat = StreamStat::default();
        let mut stream = resp.bytes_stream();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    for ev in parser.feed(&chunk) {
                        stat.absorb(&ev);
                    }
                    if bytes_tx.send(Ok(chunk)).await.is_err() {
                        break; // 下游断开
                    }
                }
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {e}");
                    let _ = bytes_tx
                        .send(Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
                        .await;
                    break;
                }
            }
        }
        stat.finish_reason = parser.finish_reason.clone();
        stat.tokens_out = parser.completion_tokens;
        stat.tokens_in = parser.prompt_tokens;
        let _ = tx.send(stat);
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    (stream, rx)
}

/// 流式统计（SSE tap 聚合，供 usage 记账）。
#[derive(Debug, Default, Clone)]
pub struct StreamStat {
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub finish_reason: Option<String>,
    pub error: Option<ErrorClass>,
    pub error_msg: String,
    pub saw_done: bool,
}

impl StreamStat {
    pub fn absorb(&mut self, ev: &SseEvent) {
        match ev {
            SseEvent::Done => self.saw_done = true,
            SseEvent::Data(v) => {
                if let Some(u) = v.get("usage") {
                    if let Some(n) = u.get("prompt_tokens").and_then(|x| x.as_i64()) {
                        if n > 0 {
                            self.tokens_in = n;
                        }
                    }
                    if let Some(n) = u.get("completion_tokens").and_then(|x| x.as_i64()) {
                        if n > 0 {
                            self.tokens_out = n;
                        }
                    }
                }
                if let Some(err) = v.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    self.error = Some(ErrorClass::Transient);
                    self.error_msg = msg;
                }
            }
            SseEvent::Other => {}
        }
    }
}

/// SSE 事件（归一层）。
#[derive(Debug, Clone)]
pub enum SseEvent {
    /// data 行（已解析为 JSON）。
    Data(Value),
    /// `data: [DONE]` 终止帧。
    Done,
    /// 注释/空行/无法解析的行。
    Other,
}

/// SSE 解析器：跨 chunk 缓冲、按行提取 `data:`（多字节安全）。
pub struct SseParser {
    buf: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub finish_reason: Option<String>,
    pub error: Option<ErrorClass>,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser {
            buf: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            finish_reason: None,
            error: None,
        }
    }

    /// 喂入原始字节，返回本次解析出的完整事件。
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(ev) = self.parse_line(line) {
                out.push(ev);
            }
        }
        out
    }

    fn parse_line(&mut self, line: &str) -> Option<SseEvent> {
        let data = line.strip_prefix("data:")?;
        let data = data.trim_start_matches(' ');
        if data == "[DONE]" {
            return Some(SseEvent::Done);
        }
        if data.is_empty() {
            return Some(SseEvent::Other);
        }
        match serde_json::from_str::<Value>(data) {
            Ok(v) => {
                if let Some(u) = v.get("usage") {
                    if let Some(n) = u.get("prompt_tokens").and_then(|x| x.as_i64()) {
                        if n > 0 {
                            self.prompt_tokens = n;
                        }
                    }
                    if let Some(n) = u.get("completion_tokens").and_then(|x| x.as_i64()) {
                        if n > 0 {
                            self.completion_tokens = n;
                        }
                    }
                }
                if let Some(fr) = v
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|c| c.first())
                    .and_then(|c| c.get("finish_reason"))
                {
                    if fr.is_string() {
                        self.finish_reason = fr.as_str().map(String::from);
                    }
                }
                if v.get("error").is_some() {
                    self.error = Some(ErrorClass::Transient);
                }
                Some(SseEvent::Data(v))
            }
            Err(_) => Some(SseEvent::Other),
        }
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

/// 按假成功指纹修正错误分类（无 spec 指纹时的旧版通用规则：登录/失效/授权/token 文案）。
pub fn classify_with_biz(status: u16, body: &str) -> ErrorClass {
    let base = ErrorClass::from_status(status);
    if (200..300).contains(&status) {
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            let code = v
                .get("code")
                .and_then(|c| c.as_str().or_else(|| c.as_i64().map(|_| "")))
                .unwrap_or("");
            let msg = v
                .get("message")
                .or_else(|| v.get("desc"))
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if let Some(refined) = base.refine_biz(code, msg) {
                return refined;
            }
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_parser_handles_split_chunks() {
        let mut p = SseParser::new();
        let e1 = p.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"He\"}");
        assert!(e1.is_empty(), "行未结束不应产出事件");
        let e2 = p.feed(b"}]}\n\ndata: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":9}}\n");
        assert_eq!(e2.len(), 2);
        let e3 = p.feed(b"\ndata: [DONE]\n\n");
        assert!(matches!(e3.last(), Some(SseEvent::Done)));
        assert_eq!(p.prompt_tokens, 7);
        assert_eq!(p.completion_tokens, 9);
    }

    #[test]
    fn sse_parser_finish_reason_and_multibyte() {
        let mut p = SseParser::new();
        let payload = "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\n\n";
        let bytes = payload.as_bytes();
        let mid = 12; // 切在多字节字符中间
        let _ = p.feed(&bytes[..mid]);
        let rest = p.feed(&bytes[mid..]);
        assert!(rest.iter().any(|e| matches!(e, SseEvent::Data(_))));
        assert_eq!(p.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn stream_stat_absorbs_usage_and_error() {
        let mut stat = StreamStat::default();
        stat.absorb(&SseEvent::Data(json!({"usage":{"prompt_tokens":3,"completion_tokens":0}})));
        stat.absorb(&SseEvent::Data(json!({"usage":{"completion_tokens":8}})));
        stat.absorb(&SseEvent::Data(json!({"error":{"message":"boom"}})));
        stat.absorb(&SseEvent::Done);
        assert_eq!(stat.tokens_in, 3);
        assert_eq!(stat.tokens_out, 8);
        assert!(stat.error.is_some());
        assert!(stat.saw_done);
    }

    #[test]
    fn classify_with_biz_fakesuccess() {
        assert_eq!(
            classify_with_biz(200, r#"{"code":"100002","desc":"登录已失效"}"#),
            ErrorClass::Auth
        );
        assert_eq!(classify_with_biz(429, "rate limited"), ErrorClass::Rate);
    }
}
