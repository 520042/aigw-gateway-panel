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
pub mod doubao;
pub mod yuanbao;
pub mod kuku;
pub mod metaso;
pub mod qwen;
pub mod mimo;
pub mod kiro;
pub mod cloudcode;
pub mod deepseek;
pub mod opencode;
pub mod qoder;
pub mod trae;
pub mod coze;
pub mod devin;
pub mod auth_login;
pub mod proxy;
pub mod wb;
pub mod wb_growth;
pub mod sanitize;

/// 该 `upstream_kind` 是否有专门的 adapter 实现（false 表示走 OpenAI 兼容透传）。
///
/// 与 `aigw_registry::KNOWN_UPSTREAM_KINDS` 配对：新增 adapter 必须两边都改；
/// 只改 registry 会让该平台静默走 OpenAI 透传（协议不符 → 上游 400/乱码），
/// 因此 `every_known_upstream_kind_has_an_adapter` 测试会拦住这种漂移。
pub fn has_adapter(kind: &str) -> bool {
    matches!(
        kind,
        "chatglm"
            | "doubao"
            | "yuanbao"
            | "kuku"
            | "metaso"
            | "qwen-web"
            | "mimo"
            | "kiro"
            | "cloudcode"
            | "deepseek-web"
            | "opencode"
            | "qoder"
            | "trae"
            | "coze"
            | "devin"
    )
}

use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 包间 read 超时（SSE 流式靠它兜底）。
///
/// 90s 曾经不够：kilo 免费池流式实测首包 134s（直连上游同样慢），超时后 reqwest 报
/// "error sending request"，客户端只看到 502 + 一串英文，无法判断是上游慢还是自己错了。
/// 放宽到 300s 覆盖这类慢上游，同时用 `network_hint` 把超时语义说清楚。
const READ_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const NONSTREAM_TOTAL: Duration = Duration::from_secs(180);

/// 把 reqwest 的网络错误翻成用户能读懂的话（尤其超时）。
/// reqwest 对超时/连接失败都输出 "error sending request for url (…)"，
/// 直接透出会让用户以为是网关坏了，实际上绝大多数是上游慢或本机代理不通。
/// `budget` 传该请求实际生效的超时（调用点各自的 timeout/read_timeout），别写死一个数。
pub fn network_hint(e: &reqwest::Error, budget: Duration) -> String {
    let base = format!("{e}");
    if e.is_timeout() {
        return format!(
            "上游在 {:.0}s 内没有响应（超时）：{base}。可能是该上游当前很慢或不可用，可换模型/平台重试；若长期如此请在「接入源」里停用该平台。",
            budget.as_secs_f64()
        );
    }
    if e.is_connect() {
        return format!("无法连接上游（检查本机代理/网络）：{base}");
    }
    base
}

#[derive(Debug)]
pub struct RelayError {
    pub status: u16,
    pub body: String,
    pub class: ErrorClass,
}
/// 错误类别 → 对外的 HTTP 状态码。
///
/// ★ 上游把「限流」写在 body 业务码里（HTTP 200 + code=429）时，各 adapter 用
///   `er(502, ..., ErrorClass::Rate)` 包装——状态码写死 502 会让 OpenAI SDK 认为
///   「网关坏了」而不是「我被限流」，既不会按 Retry-After 退避，面板统计也失真。
///   这里统一按类别给出语义正确的状态码。
pub fn status_for_class(class: ErrorClass) -> u16 {
    match class {
        ErrorClass::Auth => 401,
        ErrorClass::Quota => 402,
        ErrorClass::Rate => 429,
        ErrorClass::Rejected => 400,
        ErrorClass::Waf => 403,
        // 车道级封锁/地域封锁：403 比 401 准确——凭据没错（本来就没有），是准入被拒。
        // 用 401 会让 OpenAI SDK 触发「重新认证」流程，而这里根本没有可重登的东西。
        ErrorClass::LaneLocked | ErrorClass::Region => 403,
        ErrorClass::Transient => 502,
    }
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
        domain: &str,
        body: &[u8],
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
        if !body.is_empty() {
            rb = rb.header("Content-Type", "application/json");
        }
        // Loomy 模型网关（signer="loomy"）：除 Bearer 外还要 token 双写 +
        // 每次随机的 traceparent（缺失会挂起到超时）+ loomy-version。
        if spec.signer == "loomy" && !secret.is_empty() {
            rb = rb
                .header("token", secret)
                .header(
                    "traceparent",
                    format!("00-{}-{}-01", Self::rand_hex(32), Self::rand_hex(16)),
                )
                .header("loomy-version", "0.9.38")
                .header("Accept", "application/json");
        }
        // WorkBuddy / CodeBuddy 族（signer="wb"）：只带 Bearer 会被上游判 11101
        // 「请求内容不合法，可能缺少必要信息」—— 它要的是整族官方客户端头。
        if spec.signer == "wb" && !secret.is_empty() {
            rb = Self::apply_wb_headers(rb, spec, secret, domain, body);
        }
        rb
    }

    /// 十六进制 SHA-256（WorkBuddy 头族的设备/会话/链路标识都用它派生）。
    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(bytes);
        d.iter().map(|b| format!("{b:02x}")).collect()
    }
    /// WorkBuddy / CodeBuddy 族（signer="wb"）的官方客户端头族注入。
    ///
    /// 上游 copilot.tencent.com 的请求校验不只看 Bearer：它按一组「使用端归属 + 风控闸门 +
    /// 账号级设备标识 + 会话行迹」头判断请求是不是官方客户端发出的。只带 Bearer 裸打会得到
    /// 11101「请求内容不合法，可能缺少必要信息」（实测：token 有效、请求已到业务层，纯粹头族不全）。
    ///
    /// 全套取值一手来自 _refs/workbuddy2api-master：client_profile.py 的 DEFAULTS
    /// （5.5.6 / CLI 2.137.1 / X-Product=SaaS，后者是部署形态不是产品名）、_build_ua 的三段式 UA、
    /// risk_headers() 的 X-IDE-* 闸门头、stable_device_id(uid, salt) = md5("{salt}:{uid}")[..36]
    /// （机器标识必须按账号稳定派生：每次随机=频繁换设备，全网共用常量=多号同设备，两者都是异常特征）、
    /// 以及 _session_headers 的会话/链路头族（X-B3-ParentSpanId 故意不发：官方只在有父 span 时才带）。
    ///
    /// uid 取不到时（我们的登录响应只有 accessToken+domain）用凭据的 sha256 尾巴兜底 —— 它是按账号
    /// 稳定的，能保住「一个账号一台固定设备」这个语义。
    fn apply_wb_headers(
        rb: reqwest::RequestBuilder,
        spec: &ProviderSpec,
        secret: &str,
        domain: &str,
        body: &[u8],
    ) -> reqwest::RequestBuilder {
        const DESKTOP_VERSION: &str = "5.5.6";
        const CLI_VERSION: &str = "2.137.1";
        const BRAND: &str = "WorkBuddy";
        let _ = spec;
        let digest = Self::sha256_hex(secret.as_bytes());
        let uid = digest[..16].to_string();
        let device = |salt: &str| Self::sha256_hex(format!("{salt}:{uid}").as_bytes())[..36].to_string();
        let trace = digest[..32].to_string();
        let mid = Self::sha256_hex(format!("{digest}:msg").as_bytes());
        let mid = mid[..32].to_string();
        let span = mid[..16].to_string();
        let conv = serde_json::from_slice::<Value>(body)
            .ok()
            .map(|v| Self::wb_turn_key(&v))
            .unwrap_or_default();
        let rb = rb
            .header(
                "User-Agent",
                format!("{BRAND}/{DESKTOP_VERSION} {BRAND}/{DESKTOP_VERSION} CLI/{CLI_VERSION}"),
            )
            .header("X-IDE-Name", BRAND)
            .header("X-IDE-Type", BRAND)
            .header("X-Product", "SaaS")
            .header("X-IDE-Version", DESKTOP_VERSION)
            .header("X-CodeBuddy-Request", "1")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Accept-Language", "zh-CN")
            .header("X-Private-Data", "false")
            .header("X-User-Id", &uid)
            .header("X-Machine-ID", device("machine"))
            .header("X-Session-ID", device("session"))
            .header("X-Request-ID", &mid)
            .header("X-Conversation-Message-ID", &mid)
            .header("X-B3-TraceId", &trace)
            .header("X-B3-SpanId", &span)
            .header("X-B3-Sampled", "1")
            .header("X-Trace-ID", &trace)
            .header("b3", format!("{trace}-{span}-1"))
            .header("traceparent", format!("00-{trace}-{span}-01"))
            .header("X-Agent-Intent", "craft");
        let rb = if conv.is_empty() {
            rb
        } else {
            let rk = if conv.len() > 32 { &conv[..32] } else { conv.as_str() };
            rb.header("X-Conversation-Request-ID", rk)
                .header("X-Root-Request-ID", rk)
        };
        if domain.is_empty() {
            rb
        } else {
            rb.header("X-Domain", domain)
        }
    }

    /// WorkBuddy 的「对话轮」键：优先客户端显式的 conversation 标识，其次最后一条 user
    /// 消息的文本摘要（对齐官方语义：同一次用户发送内的多轮调用恒等，用户发下一条就换）。
    fn wb_turn_key(v: &Value) -> String {
        for k in ["conversation_id", "conversationId", "prompt_cache_key"] {
            if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
        let msgs = match v.get("messages").and_then(|m| m.as_array()) {
            Some(m) => m,
            None => return String::new(),
        };
        for m in msgs.iter().rev() {
            if m.get("role").and_then(|r| r.as_str()) != Some("user") {
                continue;
            }
            let text = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            if text.is_empty() {
                return String::new();
            }
            let h = Self::sha256_hex(text.as_bytes());
            return format!("u{}-{}", msgs.len(), &h[..16]);
        }
        String::new()
    }
    /// 非加密安全随机 hex（Loomy traceparent 幂等用，足够）。
    fn rand_hex(n: usize) -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..n).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect()
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
        domain: &str,
        body: &[u8],
    ) -> Result<Value, RelayError> {
        if !spec.chat_supported {
            return Err(RelayError {
                status: 501,
                class: ErrorClass::Transient,
                body: format!(
                    "{} 暂不支持对话反代（仅支持登录/验活）：该平台无公开对话端点，官方逆向需浏览器环境，与本项目纯 HTTP 架构不符",
                    spec.name
                ),
            });
        }
        // WorkBuddy 族上游**只支持流式**（实测非流式一律 400 code=11101，报文里
        // stream:false 会被判「请求内容不合法」）。这里自己改成流式发出去再聚合成一个
        // 完整 JSON，而不是让调用方收到一个看不懂的 11101。
        if spec.signer == "wb" {
            return self.wb_nonstream(spec, secret, domain, body).await;
        }
        // ★ 2026-10-26（P2）：非流式路径同样做指纹脱敏（与流式共用同一套规则）。
        let owned;
        let body = match serde_json::from_slice::<Value>(body) {
            Ok(mut v) => {
                let sc = sanitize::sanitize_request_body(&mut v);
                if sc.is_empty() {
                    body
                } else {
                    tracing::debug!(changed = ?sc, "出站请求体已做指纹脱敏（非流式）");
                    owned = serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec());
                    owned.as_slice()
                }
            }
            Err(_) => body,
        };
        match spec.upstream_kind.as_str() {
            "chatglm" => glm::chat_nonstream(&self.http, spec, secret, body).await,
            "doubao" => doubao::chat_nonstream(&self.http, spec, secret, body).await,
            "yuanbao" => yuanbao::chat_nonstream(&self.http, spec, secret, body).await,
            "kuku" => kuku::chat_nonstream(&self.http, spec, secret, body).await,
            "metaso" => metaso::chat_nonstream(&self.http, spec, secret, body).await,
            "qwen-web" => qwen::chat_nonstream(&self.http, spec, secret, body).await,
            "mimo" => mimo::chat_nonstream(&self.http, spec, secret, body).await,
            "kiro" => kiro::chat_nonstream(&self.http, spec, secret, body).await,
            "cloudcode" => cloudcode::chat_nonstream(&self.http, spec, secret, body).await,
            "deepseek-web" => deepseek::chat_nonstream(&self.http, spec, secret, body).await,
            "opencode" => opencode::chat_nonstream(&self.http, spec, secret, body).await,
            "qoder" => qoder::chat_nonstream(&self.http, spec, secret, body).await,
            "trae" => trae::chat_nonstream(&self.http, spec, secret, body).await,
            "coze" => coze::chat_nonstream(&self.http, spec, secret, body).await,
        "devin" => devin::chat_nonstream(&self.http, spec, secret, body).await,
            _ => self.post_openai_nonstream(spec, secret, domain, body).await,
        }
    }

    /// WorkBuddy 族的非流式：上游不认 stream:false，强制改成流式再聚合成完整响应。
    ///
    /// 直接透传非流式请求会拿到 400 code=11101（"请求内容不合法，可能缺少必要信息"），
    /// 而这个码同时也会被真实参数问题触发，用户无从分辨到底是参数错了还是平台不支持
    /// 非流式。所以在这里把 stream 改成 true 自己收流聚合。
    async fn wb_nonstream(
        &self,
        spec: &ProviderSpec,
        secret: &str,
        domain: &str,
        body: &[u8],
    ) -> Result<Value, RelayError> {
        let mut v: Value = serde_json::from_slice(body).map_err(|e| RelayError {
            status: 400,
            class: ErrorClass::Rejected,
            body: format!("bad json: {e}"),
        })?;
        let changed = prepare_wb_body(&mut v);
        if !changed.is_empty() {
            tracing::debug!(changed = ?changed, "WorkBuddy 请求体已归一");
        }
        let rewritten = serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec());
        let (stream, rx) = self.post_openai_stream(spec, secret, domain, &rewritten).await?;
        drop(rx);
        let text = collect_sse_text(stream).await;
        match aggregate_sse_nonstream(&text) {
            Ok(Some(v)) => Ok(v),
            Err(msg) => Err(RelayError {
                status: 400,
                class: ErrorClass::Rejected,
                body: format!("上游流内错误：{msg}"),
            }),
            Ok(None) => Err(RelayError {
                status: 502,
                class: ErrorClass::Transient,
                body: format!(
                    "上游流式返回里没有可聚合的内容：{}",
                    text.chars().take(300).collect::<String>()
                ),
            }),
        }
    }
    /// 账号验活：按 upstream_kind 分派到各平台 adapter（cookie 平台走真实轻量端点）。
    pub async fn verify_account(&self, spec: &ProviderSpec, secret: &str) -> bool {
        match spec.upstream_kind.as_str() {
            "doubao" => doubao::verify(&self.http, spec, secret).await,
            "yuanbao" => yuanbao::verify(&self.http, spec, secret).await,
            "kuku" => kuku::verify(&self.http, spec, secret).await,
            "metaso" => metaso::verify(&self.http, spec, secret).await,
            "qwen-web" => qwen::verify(&self.http, spec, secret).await,
            "mimo" => mimo::verify(&self.http, spec, secret).await,
            "kiro" => kiro::verify(&self.http, spec, secret).await,
            "cloudcode" => cloudcode::verify(&self.http, spec, secret).await,
            "deepseek-web" => deepseek::verify(&self.http, spec, secret).await,
            "opencode" => opencode::verify(&self.http, spec, secret).await,
            "qoder" => qoder::verify(&self.http, spec, secret).await,
            "trae" => trae::verify(&self.http, spec, secret).await,
            "coze" => coze::verify(&self.http, spec, secret).await,
        "devin" => devin::verify(&self.http, spec, secret).await,
            _ => self.verify_openai(spec, secret).await,
        }
    }

    /// 通用 openai 透传验活：GET models_path，2xx 即有效。
    async fn verify_openai(&self, spec: &ProviderSpec, secret: &str) -> bool {
        let path = if spec.models_path.is_empty() {
            "/models".to_string()
        } else {
            spec.models_path.clone()
        };
        let url = format!("{}{}", spec.base_url, path);
        let rb = Self::apply_headers(self.http.get(&url), spec, secret, "", &[]);
        match rb.timeout(std::time::Duration::from_secs(15)).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }

    /// 流式转发：按 upstream_kind 分派。返回 `(下游流, 统计接收端)`。
    pub async fn forward_stream(
        &self,
        spec: &ProviderSpec,
        secret: &str,
        domain: &str,
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
        if !spec.chat_supported {
            return Err(RelayError {
                status: 501,
                class: ErrorClass::Transient,
                body: format!(
                    "{} 暂不支持对话反代（仅支持登录/验活）：该平台无公开对话端点，官方逆向需浏览器环境，与本项目纯 HTTP 架构不符",
                    spec.name
                ),
            });
        }
        // ★ WorkBuddy 的流式也要先过一遍请求体归一：上游对 tool_choice 对象形态回
        //   400（"cannot unmarshal object into Go struct field Request.tool_choice of
        //   type string"），而这是**流式**路径才暴露的 —— 只归一非流式会留个洞。
        // ★ 2026-10-26（P2）：同一次改写里做客户端指纹脱敏（Claude Code / Codex CLI
        //   的身份自证句会让上游判「非授权客户端」直接 400）。对所有平台生效——
        //   哪些上游会拦是随时变的，出站前统一清掉最省心。
        let owned;
        let body = {
            match serde_json::from_slice::<Value>(body) {
                Ok(mut v) => {
                    let mut dirty = false;
                    if spec.signer == "wb" {
                        let ch = prepare_wb_body(&mut v);
                        if !ch.is_empty() {
                            dirty = true;
                        }
                    }
                    let sc = sanitize::sanitize_request_body(&mut v);
                    if !sc.is_empty() {
                        tracing::debug!(changed = ?sc, "出站请求体已做指纹脱敏");
                        dirty = true;
                    }
                    if dirty {
                        owned = serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec());
                        owned.as_slice()
                    } else {
                        body
                    }
                }
                Err(_) => body,
            }
        };
        match spec.upstream_kind.as_str() {
            "chatglm" => {
                let (s, rx) = glm::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "doubao" => {
                let (s, rx) = doubao::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "yuanbao" => {
                let (s, rx) = yuanbao::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "kuku" => {
                let (s, rx) = kuku::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "metaso" => {
                let (s, rx) = metaso::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "qwen-web" => {
                let (s, rx) = qwen::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "mimo" => {
                let (s, rx) = mimo::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "kiro" => {
                let (s, rx) = kiro::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "cloudcode" => {
                let (s, rx) = cloudcode::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "deepseek-web" => {
                let (s, rx) = deepseek::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "opencode" => {
                let (s, rx) = opencode::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "qoder" => {
                let (s, rx) = qoder::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "trae" => {
                let (s, rx) = trae::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "coze" => {
                let (s, rx) = coze::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "devin" => {
                let (s, rx) = devin::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            _ => {
                let (s, rx) = self.post_openai_stream(spec, secret, domain, body).await?;
                Ok((Box::pin(s), rx))
            }
        }
    }

    /// 把 body 读取结果折成 `Result<String, RelayError>`：超时定性为「上游卡在正文中途」。
    ///
    /// 拆成独立函数是为了能测：`.timeout(NONSTREAM_TOTAL)` 只覆盖到「收到响应头」这一步，
    /// `resp.text()` 自己不受它约束（实测一条失败记录正好等于 NONSTREAM_TOTAL + 收头耗时），
    /// 上游发完 200 头就不吐字节时请求会一直挂着。这里给 body 单独的读预算。
    async fn read_body_within(resp: reqwest::Response, budget: Duration) -> Result<String, RelayError> {
        match tokio::time::timeout(budget, resp.text()).await {
            Ok(Ok(t)) => Ok(t),
            Ok(Err(e)) => Err(RelayError {
                status: 0,
                class: ErrorClass::Transient,
                body: format!("network: {}", network_hint(&e, budget)),
            }),
            Err(_) => Err(RelayError {
                status: 0,
                class: ErrorClass::Transient,
                body: format!(
                    "network: 上游在 {}s 内没有把响应体发完（超时）：已经收到 HTTP 响应头，但正文一直没到齐。可能是该上游卡在中途或正在限速，可换模型/平台重试。",
                    budget.as_secs()
                ),
            }),
        }
    }

    // ================= openai 透传 =================

    /// 非流式透传：POST {base}{chat_path}，总时长 180s，2xx 假成功指纹检查。
    pub async fn post_openai_nonstream(
        &self,
        spec: &ProviderSpec,
        secret: &str,
        domain: &str,
        body: &[u8],
    ) -> Result<Value, RelayError> {
        let url = Self::chat_url(spec);
        let mut rb = Self::apply_headers(self.http.post(&url), spec, secret, domain, body);
        rb = Self::apply_codearts_sign(rb, spec, secret, body);
        let rb = rb
            .timeout(NONSTREAM_TOTAL)
            .body(body.to_vec());
        let resp = rb.send().await.map_err(|e| RelayError {
            status: 0,
            body: format!("network: {}", network_hint(&e, NONSTREAM_TOTAL)),
            class: ErrorClass::Transient,
        })?;
        let status = resp.status().as_u16();
        // ★ 部分上游（如 LobsterAI 网关）无视 stream:false 仍回 text/event-stream；
        // 直接当 JSON 解析只会得到 "bad json: expected value at line 1 column 1"，
        // 把上游真实原因（例如 40300 不支持的模型）彻底掩盖掉。
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
            .unwrap_or(false);
        // ★ 读 body 本身没有超时：`.timeout(NONSTREAM_TOTAL)` 只覆盖到「收到响应头」。
        //   实测有一条桥上游的失败记录是 180014ms（正好 = NONSTREAM_TOTAL + 14ms，
        //   14ms 是收头耗时）：上游发完 200 头就不再吐字节，resp.text() 会一直挂着，
        //   这条请求就永久占着一个连接和一个并发位，最终只能等它自己断开。
        //   所以 body 另给一个与总预算同量级的读超时（见 read_body_within）。
        let text = Self::read_body_within(resp, NONSTREAM_TOTAL).await?;
        if !(200..300).contains(&status) {
            return Err(RelayError {
                status,
                class: ErrorClass::from_status(status),
                body: text.chars().take(800).collect(),
            });
        }
        let trimmed = text.trim_start();
        if is_sse || trimmed.starts_with("data:") || trimmed.starts_with("event:") {
            match aggregate_sse_nonstream(&text) {
                Ok(Some(v)) => return Ok(v),
                Ok(None) => {}
                Err(msg) => {
                    return Err(RelayError {
                        status: 400,
                        class: ErrorClass::Rejected,
                        body: format!("上游流内错误：{msg}"),
                    })
                }
            }
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| RelayError {
            // ★ 不能沿用上游的 2xx：HTTP 200 + HTML（网页版逆向失效时返回登录页）
            // 会被上层当成"成功响应里的错误"，SDK 侧只看到解析失败。
            // 用 502 表达"上游给了不可用内容"，客户端才会走错误分支。
            status: if (200..300).contains(&status) { 502 } else { status },
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
        domain: &str,
        body: &[u8],
    ) -> Result<
        (
            impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin + Send + 'static,
            mpsc::Receiver<StreamStat>,
        ),
        RelayError,
    > {
        let url = Self::chat_url(spec);
        let mut rb = Self::apply_headers(self.http.post(&url), spec, secret, domain, body);
        rb = Self::apply_codearts_sign(rb, spec, secret, body);
        let rb = rb.body(body.to_vec()); // 流式：不设总时长（read_timeout 兜底包间 idle）
        let resp = rb.send().await.map_err(|e| RelayError {
            status: 0,
            body: format!("network: {}", network_hint(&e, READ_TIMEOUT)),
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
        Ok(tap_passthrough(resp, body.len()))
    }

    /// 带凭据的 live 模型清单（api_key/cookie 平台必须走这条）。
    pub async fn fetch_models_with_secret(
        &self,
        spec: &ProviderSpec,
        secret: &str,
    ) -> Result<Vec<String>, RelayError> {
        let url = format!("{}{}", spec.base_url, spec.models_path());
        let rb = Self::apply_headers(self.http.get(&url), spec, secret, "", &[])
            .timeout(Duration::from_secs(12));
        let resp = rb.send().await.map_err(|e| RelayError {
            status: 0,
            body: format!("network: {}", network_hint(&e, Duration::from_secs(12))),
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

/// 把「客户端请求体」改写成 WorkBuddy 上游可接受的样子（就地改写，返回改写摘要）。
///
/// 顺序即语义，与参考实现 workbuddy2api 的 prepare_upstream_body 一致：
///   1. 强制 stream=true（上游只认流式，非流式由我们聚合）
///   2. role 归一（developer → system）
///   3. tool_choice 归一（对象形态上游直接 400 code=11101）
///   4. 孤儿 tool_calls 清理（配不成对的整条删，否则整段会话每条都 400）
///   5. reasoning_effort 关闭意图翻译成 thinking:{type:disabled}（上游对 off 会 400）
///   6. deepseek 族 reasoning_content 回填（多轮里缺一条就 400）
///
/// 每一步都只动该动的字段，客户端没表达的意图一律不代它开启 —— 否则用户的账单会莫名其妙变高。
fn prepare_wb_body(v: &mut Value) -> Vec<&'static str> {
    let mut changed: Vec<&'static str> = Vec::new();
    let Some(o) = v.as_object_mut() else {
        return changed;
    };
    if o.get("stream").and_then(|x| x.as_bool()) != Some(true) {
        o.insert("stream".into(), Value::Bool(true));
        changed.push("stream");
    }
    if !o.contains_key("stream_options") {
        o.insert("stream_options".into(), serde_json::json!({"include_usage": true}));
    }
    if let Some(msgs) = o.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for m in msgs.iter_mut() {
            if let Some(mo) = m.as_object_mut() {
                let is_dev = mo
                    .get("role")
                    .and_then(|r| r.as_str())
                    .map(|r| r.trim().eq_ignore_ascii_case("developer"))
                    .unwrap_or(false);
                if is_dev {
                    mo.insert("role".into(), Value::String("system".into()));
                    if !changed.contains(&"role") {
                        changed.push("role");
                    }
                }
            }
        }
    }
    if normalize_wb_tool_choice(o) {
        changed.push("tool_choice");
    }
    if cleanup_orphan_tool_calls(o) > 0 {
        changed.push("orphan_tool_calls");
    }
    if normalize_wb_thinking(o) {
        changed.push("thinking");
    }
    if backfill_reasoning_content(o) > 0 {
        changed.push("reasoning_content");
    }
    changed
}

/// tool_choice 对象形态 → 上游认的字符串形态。返回是否改写。
fn normalize_wb_tool_choice(o: &mut serde_json::Map<String, Value>) -> bool {
    let Some(tc) = o.get("tool_choice") else {
        return false;
    };
    let suppress = |o: &mut serde_json::Map<String, Value>| {
        o.remove("tools");
        o.remove("functions");
    };
    if let Some(s) = tc.as_str() {
        if s.trim().eq_ignore_ascii_case("none") {
            o.remove("tool_choice");
            suppress(o);
            return true;
        }
        return false;
    }
    let typ = tc
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match typ.as_str() {
        "none" => {
            o.remove("tool_choice");
            suppress(o);
        }
        "auto" | "required" => {
            o.insert("tool_choice".into(), Value::String(typ));
        }
        "function" => {
            let name = tc
                .pointer("/function/name")
                .or_else(|| tc.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let name = if name.is_empty() { "auto".to_string() } else { name };
            o.insert("tool_choice".into(), Value::String(name));
        }
        _ => {
            o.remove("tool_choice");
        }
    }
    true
}

/// 剔除配不成对的 tool_calls / tool 结果，返回删除条目数。
fn cleanup_orphan_tool_calls(o: &mut serde_json::Map<String, Value>) -> usize {
    let Some(msgs) = o.get("messages").and_then(|m| m.as_array()) else {
        return 0;
    };
    let mut call_ids: Vec<String> = Vec::new();
    let mut result_ids: Vec<String> = Vec::new();
    for m in msgs {
        let Some(mo) = m.as_object() else { continue };
        match mo.get("role").and_then(|r| r.as_str()) {
            Some("tool") => {
                if let Some(t) = mo.get("tool_call_id").and_then(|x| x.as_str()) {
                    if !t.is_empty() {
                        result_ids.push(t.to_string());
                    }
                }
            }
            Some("assistant") => {
                if let Some(tcs) = mo.get("tool_calls").and_then(|x| x.as_array()) {
                    for tc in tcs {
                        if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                            if !id.is_empty() {
                                call_ids.push(id.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if call_ids.is_empty() && result_ids.is_empty() {
        return 0;
    }
    let keep: Vec<String> = call_ids
        .iter()
        .filter(|c| result_ids.contains(c))
        .cloned()
        .collect();
    let mut changed = 0usize;
    if let Some(msgs) = o.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for m in msgs.iter_mut() {
            let Some(mo) = m.as_object_mut() else { continue };
            if mo.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                continue;
            }
            let Some(tcs) = mo.get("tool_calls").and_then(|x| x.as_array()).cloned() else {
                continue;
            };
            if tcs.is_empty() {
                continue;
            }
            let all_kept = tcs.iter().all(|tc| {
                tc.get("id")
                    .and_then(|x| x.as_str())
                    .map(|id| keep.iter().any(|k| k == id))
                    .unwrap_or(false)
            });
            if !all_kept {
                mo.remove("tool_calls");
                changed += 1;
            }
        }
        // 重算存活集合：上一步被删掉的 tool_calls 里的 id 不能再算配对成功。
        let mut surviving: Vec<String> = Vec::new();
        let mut idx = 0usize;
        while idx < msgs.len() {
            if let Some(mo) = msgs[idx].as_object() {
                if mo.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    if let Some(tcs) = mo.get("tool_calls").and_then(|x| x.as_array()) {
                        for tc in tcs {
                            if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                                surviving.push(id.to_string());
                            }
                        }
                    }
                }
            }
            idx += 1;
        }
        let before = msgs.len();
        msgs.retain(|m| {
            let Some(mo) = m.as_object() else { return true };
            if mo.get("role").and_then(|r| r.as_str()) != Some("tool") {
                return true;
            }
            match mo.get("tool_call_id").and_then(|x| x.as_str()) {
                Some(t) => surviving.iter().any(|k| k == t),
                None => true,
            }
        });
        changed += before - msgs.len();
    }
    changed
}

/// 把「关闭思考」的意图翻译成上游认的 thinking:{type:"disabled"}。
///
/// 上游对 reasoning_effort="off" 直接 400，而 "none" 反而**开启**思考 —— 两个坑都得躲开。
/// 只有客户端明确表达了关闭才动；没表态保持原样，绝不代它开启（否则账单会莫名变高）。
fn normalize_wb_thinking(o: &mut serde_json::Map<String, Value>) -> bool {
    let mut wants_off = false;
    for key in ["reasoning_effort", "reasoningEffort"] {
        if let Some(s) = o.get(key).and_then(|x| x.as_str()) {
            let t = s.trim().to_ascii_lowercase();
            if t == "off" || t == "none" || t == "disabled" {
                wants_off = true;
            }
        }
    }
    if let Some(ty) = o
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|t| t.as_str())
    {
        let t = ty.trim().to_ascii_lowercase();
        if t == "disabled" || t == "off" || t == "none" {
            wants_off = true;
        }
    }
    if o.get("enable_thinking").and_then(|x| x.as_bool()) == Some(false) {
        wants_off = true;
    }
    if !wants_off {
        return false;
    }
    o.remove("reasoning_effort");
    o.remove("reasoningEffort");
    o.remove("reasoning");
    o.remove("include_reasoning");
    o.remove("enable_thinking");
    o.remove("enableThinking");
    o.insert(
        "thinking".into(),
        serde_json::json!({"type": "disabled"}),
    );
    true
}

/// deepseek 族多轮一致性：有人带 reasoning 痕迹，就要求所有 assistant 都带 reasoning_content。
fn backfill_reasoning_content(o: &mut serde_json::Map<String, Value>) -> usize {
    let model = o
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !(model.contains("deepseek") || model.contains("ds-")) {
        return 0;
    }
    let Some(msgs) = o.get("messages").and_then(|m| m.as_array()) else {
        return 0;
    };
    let has_trace = msgs.iter().any(|m| {
        let Some(mo) = m.as_object() else { return false };
        mo.get("reasoning")
            .and_then(|r| r.as_str())
            .map(|r| !r.is_empty())
            .unwrap_or(false)
            || mo.contains_key("reasoning_content")
    });
    if !has_trace {
        return 0;
    }
    let mut n = 0usize;
    if let Some(msgs) = o.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for m in msgs.iter_mut() {
            let Some(mo) = m.as_object_mut() else { continue };
            if mo.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                continue;
            }
            if mo.contains_key("reasoning_content") {
                continue;
            }
            let r = mo
                .get("reasoning")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            mo.insert("reasoning_content".into(), Value::String(r));
            n += 1;
        }
    }
    n
}
/// 把一个字节流整段收成 UTF-8 文本（WorkBuddy 非流式聚合用）。
async fn collect_sse_text(
    mut stream: impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(item) = futures_util::StreamExt::next(&mut stream).await {
        match item {
            Ok(b) => buf.extend_from_slice(&b),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}
/// 上游无视 `stream:false` 仍回 SSE 时，把增量帧聚合成一份非流式响应。
///
/// 返回 `Ok(None)` 表示这根本不是 SSE，调用方继续按 JSON 解析；
/// 返回 `Err(msg)` 表示上游在流里显式报了错（`event: error` /
/// `{"error":{...}}` 等），把原因原文交回上层，不再伪装成「bad json」。
fn aggregate_sse_nonstream(text: &str) -> Result<Option<Value>, String> {
    // ★ 跳过前导的空行与 `:` 注释行（SSE 心跳）。WorkBuddy 每条流的开头都是
    //   `: heartbeat`，旧实现用 trim_start() 只去空白，整段就被判成「不是 SSE」→
    //   Ok(None) → 非流式聚合路径报 502「没有可聚合的内容」，而流里明明有完整正文。
    let trimmed = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .skip_while(|l| l.trim().is_empty() || l.trim_start().starts_with(':'))
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = trimmed.trim_start();
    if !(trimmed.starts_with("data:") || trimmed.starts_with("event:")) {
        return Ok(None);
    }
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish: Option<String> = None;
    let mut id = String::new();
    let mut model = String::new();
    let mut created: i64 = 0;
    let mut usage: Option<Value> = None;
    // 工具调用是增量的：每个帧只带来 index + 片段（id/name 只在第一帧出现，
    // arguments 被切成许多段）。丢掉它们就等于把「模型要调工具」这件事整个吞掉，
    // 客户端只看到一个 finish_reason=tool_calls 的空消息，永远拿不到参数。
    let mut tools_acc: Vec<Value> = Vec::new();
    let mut frames = 0usize;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let payload = match line.strip_prefix("data:") {
            Some(p) => p.trim(),
            None => continue,
        };
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let v: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        frames += 1;
        if let Some(e) = v.get("error") {
            let msg = e
                .get("message")
                .and_then(|m| m.as_str())
                .or_else(|| e.as_str())
                .or_else(|| v.get("message").and_then(|m| m.as_str()))
                .unwrap_or("上游流内错误");
            return Err(msg.to_string());
        }
        if id.is_empty() {
            if let Some(s) = v.get("id").and_then(|x| x.as_str()) {
                id = s.to_string();
            }
        }
        if model.is_empty() {
            if let Some(s) = v.get("model").and_then(|x| x.as_str()) {
                model = s.to_string();
            }
        }
        if created == 0 {
            created = v.get("created").and_then(|x| x.as_i64()).unwrap_or(0);
        }
        if let Some(u) = v.get("usage") {
            if !u.is_null() {
                usage = Some(u.clone());
            }
        }
        if let Some(ch) = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
        {
            if let Some(d) = ch.get("delta") {
                if let Some(s) = d.get("content").and_then(|x| x.as_str()) {
                    content.push_str(s);
                }
                if let Some(s) = d.get("reasoning_content").and_then(|x| x.as_str()) {
                    reasoning.push_str(s);
                }
                if let Some(tcs) = d.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tcs {
                        let idx = tc
                            .get("index")
                            .and_then(|i| i.as_u64())
                            .unwrap_or(0) as usize;
                        while tools_acc.len() <= idx {
                            tools_acc.push(json!({
                                "id": "",
                                "type": "function",
                                "function": {"name": "", "arguments": ""}
                            }));
                        }
                        let slot = &mut tools_acc[idx];
                        if let Some(s) = tc.get("id").and_then(|x| x.as_str()) {
                            slot["id"] = json!(s);
                        }
                        if let Some(s) = tc.get("type").and_then(|x| x.as_str()) {
                            slot["type"] = json!(s);
                        }
                        if let Some(f) = tc.get("function") {
                            if let Some(s) = f.get("name").and_then(|x| x.as_str()) {
                                let cur = slot["function"]["name"].as_str().unwrap_or("");
                                if !s.is_empty() && !cur.contains(s) {
                                    slot["function"]["name"] = json!(format!("{cur}{s}"));
                                }
                            }
                            if let Some(s) = f.get("arguments").and_then(|x| x.as_str()) {
                                let cur =
                                    slot["function"]["arguments"].as_str().unwrap_or("").to_string();
                                slot["function"]["arguments"] = json!(format!("{cur}{s}"));
                            }
                        }
                    }
                }
            }
            if let Some(s) = ch.get("finish_reason").and_then(|x| x.as_str()) {
                finish = Some(s.to_string());
            }
        }
    }
    if frames == 0 {
        return Ok(None);
    }
    let mut message = json!({"role": "assistant", "content": content});
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !tools_acc.is_empty() {
        // arguments 必须是合法 JSON 字符串（客户端会自己 parse），空串会让一批 SDK 直接崩。
        for t in tools_acc.iter_mut() {
            if t["function"]["arguments"].as_str().unwrap_or("").is_empty() {
                t["function"]["arguments"] = json!("{}");
            }
        }
        message["tool_calls"] = Value::Array(tools_acc);
    }
    let out_id = if id.is_empty() {
        "chatcmpl-aggregated".to_string()
    } else {
        id
    };
    let out_finish = finish.unwrap_or_else(|| "stop".to_string());
    let mut out = json!({
        "id": out_id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": out_finish,
        }],
    });
    if let Some(u) = usage {
        out["usage"] = u;
    }
    Ok(Some(out))
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
    in_bytes: usize,
) -> (
    impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin + Send + 'static,
    mpsc::Receiver<StreamStat>,
) {
    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let t0 = std::time::Instant::now();
        let mut parser = SseParser::new();
        let mut stat = StreamStat::default();
        let mut stream = resp.bytes_stream();
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    // ★ 2026-10-26（P3）：首块数据到达 → 记 TTFB（只记一次）。
                    // 这是「上游多久开始出字」的实测值，与整轮耗时是两个指标。
                    if stat.ttfb_ms == 0 && !chunk.is_empty() {
                        stat.ttfb_ms = t0.elapsed().as_millis() as i64;
                    }
                    for ev in parser.feed(&chunk) {
                        stat.absorb(&ev);
                    }
                    if bytes_tx.send(Ok(chunk)).await.is_err() {
                        // 下游断开：客户端主动取消。旧实现直接 break 且不置 error，
                        // 于是这次请求在用量台账里被记成"成功"，成功率虚高。
                        stat.error = Some(ErrorClass::Transient);
                        stat.error_msg = "下游断开（客户端取消）".to_string();
                        stat.client_gone = true;
                        break;
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
        // 上游自己给了 usage 就用它（parser 与 absorb 同源，值一致）；
        // 没给的话这里兜底估算——**旧实现直接赋 0**，于是「流式但上游不发 usage」
        // 的 openai 兼容平台在台账里恒记 0 token，看板严重偏低。
        // 口径与 estimate_stream_tokens / 各 adapter 非流式分支一致：
        // 入参字节 / 4，出参字符 / 2。
        stat.tokens_out = parser.completion_tokens;
        stat.tokens_in = parser.prompt_tokens;
        stat.fill_missing_tokens(in_bytes);
        // ★ 握手已经是 200，中途出错就没有第二次改状态码的机会了。
        // 客户端（OpenAI SDK）只会看到"200 + 流突然结束"，报
        // "stream ended without [DONE]"，看不出到底错在哪。
        // 所以这里补一个标准 SSE error 帧再收 [DONE]，让 SDK 走错误分支。
        let err_for_client = stat.error.clone();
        let err_msg = stat.error_msg.clone();
        let saw_done = stat.saw_done;
        let _ = tx.send(stat).await;
        if err_for_client.is_some() && !saw_done {
            let frame = json!({
                "error": {
                    "message": if err_msg.is_empty() {
                        format!("上游流式响应中断（{:?}）", err_for_client.unwrap())
                    } else {
                        err_msg
                    },
                    "type": "upstream_error",
                    "code": 502,
                }
            });
            let _ = bytes_tx
                .send(Ok(Bytes::from(format!("data: {frame}\n\ndata: [DONE]\n\n"))))
                .await;
        }
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    (stream, rx)
}

/// 流式 usage 估算。
///
/// cookie 逆向类平台（豆包/元宝/文库/秘塔/GLM）的上游 SSE 普遍不带 usage 字段，
/// 过去这些平台的**流式**请求在台账里恒记 0 token（非流式分支有估算，流式漏了），
/// 导致 /api/usage 与看板严重偏低。这里按「请求体字节 / 4」「出参字符 / 2」粗估，
/// 口径与各 adapter 非流式分支的 chars/2 保持一致。
pub fn estimate_stream_tokens(in_bytes: usize, out_chars: usize) -> (i64, i64) {
    let tin = (in_bytes / 4).max(1) as i64;
    let tout = (out_chars / 2).max(1) as i64;
    (tin, tout)
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
    /// ★ 2026-10-26（P3）：首字节耗时（毫秒，从发起上游请求算起）。
    /// 0 = 一个字节都没收到（整轮失败）。这是流式体验的核心指标——
    /// 推理 30 秒但 0.8 秒出字，与 8 秒才出字，用户体验差一个数量级，
    /// 而只看总耗时（ms）两者完全一样。
    pub ttfb_ms: i64,
    /// 流内累计的正文/思考字符数（上游不给 usage 时按 chars/2 估算出参用）。
    pub out_chars: usize,
    /// 这次「失败」其实是**客户端自己取消**（下游断开），不是上游出问题。
    ///
    /// 台账仍按既有口径记一次失败（上游确实没跑完，记成功会让成功率虚高），
    /// 但不该拿去惩罚账号，也不该在面板上写「这个平台坏了」——
    /// 用户点一下停止就把健康账号冻一轮，属于自己打自己。
    pub client_gone: bool,
}

/// 流内 `error` 帧的分类：上游用 SSE 传错误时没有 HTTP 状态码，只能读 `type`/`code`/`message`。
/// 旧实现一律记为 `Transient`，于是「凭据失效」的账号永远不会被拉黑，会一直空转。
pub fn classify_error_frame(err: &Value) -> ErrorClass {
    let pick = |k: &str| {
        err.get(k)
            .and_then(|x| x.as_str().map(|s| s.to_string()).or_else(|| x.as_i64().map(|n| n.to_string())))
            .unwrap_or_default()
    };
    let ty = pick("type").to_ascii_lowercase();
    let code = pick("code").to_ascii_lowercase();
    let msg = pick("message").to_ascii_lowercase();
    let hay = format!("{ty} {code} {msg}");
    let has = |keys: &[&str]| keys.iter().any(|k| hay.contains(k));
    if has(&[
        "authentication",
        "unauthorized",
        "invalid_api_key",
        "invalid_token",
        "permission",
        "forbidden",
        "登录",
        "失效",
        "授权",
        "未登录",
    ]) {
        return ErrorClass::Auth;
    }
    if has(&["quota", "insufficient", "balance", "额度", "余额"]) {
        return ErrorClass::Quota;
    }
    if has(&["rate_limit", "rate limit", "too many requests", "429", "限流"]) {
        return ErrorClass::Rate;
    }
    ErrorClass::Transient
}

impl StreamStat {
    /// 上游没给 usage 时按「入参字节 / 4、出参字符 / 2」兜底估算。
    ///
    /// ★ 旧实现把 tokens_in/tokens_out 直接赋成 parser 的 0：openai 兼容透传
    /// 分支（tap_passthrough）**从不请求 include_usage**，只要上游不主动给
    /// usage（很多免费站都不给），流式请求在台账里就恒记 0 token，看板严重偏低。
    /// 口径与 estimate_stream_tokens 及各 adapter 非流式分支保持一致。
    pub fn fill_missing_tokens(&mut self, in_bytes: usize) {
        if self.tokens_in <= 0 {
            self.tokens_in = (in_bytes / 4).max(1) as i64;
        }
        if self.tokens_out <= 0 && self.out_chars > 0 {
            self.tokens_out = (self.out_chars / 2).max(1) as i64;
        }
    }

    pub fn absorb(&mut self, ev: &SseEvent) {
        match ev {
            SseEvent::Done => self.saw_done = true,
            SseEvent::Data(v) => {
                // 累计出参字符数：上游不发 usage 时用它估算 tokens_out。
                if let Some(c) = v
                    .pointer("/choices/0/delta/content")
                    .and_then(|x| x.as_str())
                {
                    self.out_chars += c.chars().count();
                }
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
                    self.error = Some(classify_error_frame(err));
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

/// 增量 UTF-8 解码器：把「被 chunk 边界切开的多字节字符」留到下一个 chunk。
///
/// ★ 为什么不能用 `String::from_utf8_lossy` 逐 chunk 解码：它是**逐 chunk** 替换，
///   只要网络 chunk 边界落在一个汉字的 3 个字节中间，这个汉字就当场变成 U+FFFD 写进
///   缓冲，下一 chunk 的开头字节同样无效——损坏**不可恢复**。glm 是「全文快照 + 差分」，
///   快照一旦带 U+FFFD，差分出来的 delta 会把它永久吐给客户端；其它 adapter 则表现为
///   「回答无故少字 / 整帧 JSON 非法被丢弃」。reqwest 的 chunk 边界来自 TCP/TLS 解密边界，
///   落在任意字节位置上，所以长中文回答几乎必现。
#[derive(Default)]
pub struct Utf8Decoder {
    /// 上一次 push 末尾不完整的多字节字符残字节。
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入原始字节，返回**本次能确定**的完整 UTF-8 文本。
    ///
    /// 末尾不完整的字符残字节留在内部等下一次 push；真正非法的字节序列按 U+FFFD 替换。
    /// 注意区分这两种情况：前者必须等（否则汉字会被切坏），后者必须丢弃（否则缓冲永久堵塞）。
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    break;
                }
                Err(e) => {
                    let n = e.valid_up_to();
                    if n > 0 {
                        // 前缀已校验为合法 UTF-8，lossy 只是类型转换（不会产生 U+FFFD）
                        out.push_str(String::from_utf8_lossy(&self.pending[..n]).as_ref());
                        self.pending.drain(..n);
                        continue;
                    }
                    match e.error_len() {
                        // 真正的非法字节序列：替换掉并继续，避免缓冲永久堵塞
                        Some(l) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..l);
                        }
                        // 只是被截断（3 字节汉字只到了 1~2 字节）：保留残字节等下一个 chunk
                        None => break,
                    }
                }
            }
        }
        out
    }
}

/// 在 SSE 文本缓冲里找**帧边界**，同时兼容 `\n\n` 与 `\r\n\r\n`。
///
/// 返回 `(帧结束偏移, 分隔符长度)`。只认 `\n\n` 的实现遇到 CRLF 上游时永远切不出帧：
/// 缓冲一路涨到流结束、一帧都解析不出，最终表现为「未返回内容」并被误判成凭据失效。
pub fn find_frame_end(buf: &str) -> Option<(usize, usize)> {
    let lf = buf.find("\n\n").map(|i| (i, 2));
    let crlf = buf.find("\r\n\r\n").map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if b.0 < a.0 { b } else { a }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// SSE 解析器：跨 chunk 缓冲、按行提取 `data:`（多字节安全）。
pub struct SseParser {
    buf: String,
    /// 跨 chunk 的 UTF-8 残字节（见 Utf8Decoder）。旧实现用 from_utf8_lossy 逐 chunk
    /// 解码，被 chunk 边界切开的汉字会永久变成 U+FFFD。
    dec: Utf8Decoder,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub finish_reason: Option<String>,
    pub error: Option<ErrorClass>,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser {
            buf: String::new(),
            dec: Utf8Decoder::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            finish_reason: None,
            error: None,
        }
    }

    /// 喂入原始字节，返回本次解析出的完整事件。
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        // 只解码完整前缀，残字节留到下一个 chunk（否则跨边界的汉字会被永久损坏）
        let text = self.dec.push(bytes);
        self.buf.push_str(&text);
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
                if let Some(err) = v.get("error") {
                    self.error = Some(classify_error_frame(err));
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
            // 业务码可能是字符串（"100002"）也可能是数字（loomy/kuku 用数字）：
            // 旧实现把数字码一律转成空串传给 refine_biz，等于业务码指纹表永远用不上。
            let code = v
                .get("code")
                .and_then(|c| {
                    c.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| c.as_i64().map(|n| n.to_string()))
                })
                .unwrap_or_default();
            let msg = v
                .get("message")
                .or_else(|| v.get("desc"))
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if let Some(refined) = base.refine_biz(&code, msg) {
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

    /// 反向校验：registry 白名单里除 `openai`（透传）外，每个 kind 都必须有 adapter。
    #[test]
    fn every_known_upstream_kind_has_an_adapter() {
        for k in aigw_registry::KNOWN_UPSTREAM_KINDS {
            assert!(
                *k == "openai" || has_adapter(k),
                "registry 白名单有 upstream_kind={k}，但 aigw-relay 没有 adapter（会静默走 OpenAI 透传）"
            );
        }
    }

    /// ★ 上游发完 200 响应头就不再吐字节时，body 必须按预算超时收场，
    /// 而不是一直挂着（实测有 180014ms 的记录：正好 NONSTREAM_TOTAL + 收头耗时）。
    #[tokio::test]
    async fn read_body_times_out_when_upstream_stalls_after_headers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            // 200 + 一份不完整的 JSON，然后既不补齐也不关连接
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 50\r\n\r\n{\"choices\":")
                .await;
            // 挂住：只有客户端放弃后这个任务才结束
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/x"))
            .send()
            .await
            .expect("收头应成功");
        let started = std::time::Instant::now();
        let err = OutClient::read_body_within(resp, Duration::from_millis(300))
            .await
            .expect_err("正文读不完必须报错，不能永远挂着");
        let took = started.elapsed();
        assert!(
            took < Duration::from_secs(5),
            "必须在预算附近收场，实际 {took:?}"
        );
        assert!(err.body.contains("没有把响应体发完"), "got {}", err.body);
        assert_eq!(err.status, 0);
        assert!(matches!(err.class, ErrorClass::Transient), "超时应当可换号重试");
    }

    /// 正常的完整正文不受读预算影响（别把闸门装成常态失败）。
    #[tokio::test]
    async fn read_body_returns_the_full_text_when_it_arrives_in_time() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":true}")
                .await;
        });
        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/x"))
            .send()
            .await
            .unwrap();
        let text = OutClient::read_body_within(resp, Duration::from_secs(5))
            .await
            .expect("完整正文不该报错");
        assert_eq!(text, "{\"ok\":true}");
    }

    /// 上游无视 stream:false 仍回 SSE（LobsterAI 网关实测如此）时，非流式路径必须
    /// 聚合成一份 chat.completion，而不是抛「bad json: expected value at line 1」。
    #[test]
    fn aggregate_sse_nonstream_folds_chunks() {
        let sse = "data:{\"id\":\"c1\",\"created\":7,\"model\":\"glm-5.3\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"先想\"}}]}\n\n\
                   data:{\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你\"}}]}\n\n\
                   data:{\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"好\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n\
                   data:[DONE]\n\n";
        let v = aggregate_sse_nonstream(sse)
            .expect("不应报错")
            .expect("应识别为 SSE");
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["id"], "c1");
        assert_eq!(v["created"], 7);
        assert_eq!(v["model"], "glm-5.3");
        assert_eq!(v["choices"][0]["message"]["content"], "你好");
        assert_eq!(v["choices"][0]["message"]["reasoning_content"], "先想");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert_eq!(v["usage"]["completion_tokens"], 2);
    }

    /// ★ 回归：工具调用是**增量**的（arguments 被切成很多段），聚合时必须拼回去。
    /// 旧实现只拼 content/reasoning，客户端拿到的是 finish_reason=tool_calls 但
    /// message 里没有 tool_calls 的空回复 —— 智能体框架会以为模型什么都没做。
    #[test]
    fn aggregate_sse_nonstream_rebuilds_tool_call_arguments() {
        let sse = concat!(
            ": heartbeat", "\n", "\n",
            r#"data:{"id":"c1","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"calc","arguments":""}}]}}]}"#, "\n", "\n",
            r#"data:{"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"expr\":\""}}]}}]}"#, "\n", "\n",
            r#"data:{"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"2+2\"}"}}]}}]}"#, "\n", "\n",
            r#"data:{"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#, "\n", "\n",
            "data:[DONE]", "\n", "\n",
        );
        let v = aggregate_sse_nonstream(sse).expect("不应报错").expect("是 SSE");
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        let tc = &v["choices"][0]["message"]["tool_calls"];
        assert_eq!(tc[0]["id"], "call_1", "{v}");
        assert_eq!(tc[0]["function"]["name"], "calc", "{v}");
        assert_eq!(tc[0]["function"]["arguments"], r#"{"expr":"2+2"}"#, "{v}");
    }
    /// 流内错误帧要把上游原因原文交回上层，而不是伪装成「bad json」。
    #[test]
    fn aggregate_sse_nonstream_surfaces_error_frame() {
        let sse = "event:error\ndata:{\"type\":\"error\",\"error\":{\"type\":\"proxy_error\",\"message\":\"不支持的模型: x\",\"code\":40300}}\n\n";
        let err = aggregate_sse_nonstream(sse).expect_err("错误帧应转成 Err");
        assert!(err.contains("不支持的模型"), "got {err}");
    }

    /// tool_choice 对象形态必须归一：上游收到 {"type":"auto"} 直接 400 code=11101。
    #[test]
    fn prepare_wb_body_normalizes_tool_choice_and_roles() {
        let mut v = serde_json::json!({
            "model": "auto",
            "messages": [{"role": "developer", "content": "be nice"}],
            "tool_choice": {"type": "function", "function": {"name": "read_file"}},
            "tools": [{"type": "function", "function": {"name": "read_file"}}]
        });
        let changed = prepare_wb_body(&mut v);
        assert_eq!(v["stream"], serde_json::json!(true));
        assert_eq!(v["stream_options"]["include_usage"], serde_json::json!(true));
        assert_eq!(v["messages"][0]["role"], serde_json::json!("system"));
        assert_eq!(v["tool_choice"], serde_json::json!("read_file"), "对象形态必须压成名字：{v}");
        assert!(changed.contains(&"tool_choice"), "{changed:?}");
        assert!(changed.contains(&"role"), "{changed:?}");
        // 关掉工具：tool_choice=none 时 tools/functions 必须一起清，否则上游仍会尝试调用。
        let mut v2 = serde_json::json!({
            "model": "auto",
            "messages": [],
            "tool_choice": "none",
            "tools": [{"type": "function"}]
        });
        prepare_wb_body(&mut v2);
        assert!(v2.get("tool_choice").is_none());
        assert!(v2.get("tools").is_none(), "{v2}");
        // 对象形态的 auto / required 要压成同名字符串（不能一律压成 auto）。
        for want in ["auto", "required"] {
            let mut v3 = serde_json::json!({
                "model": "auto",
                "messages": [],
                "tool_choice": {"type": want},
            });
            prepare_wb_body(&mut v3);
            assert_eq!(v3["tool_choice"], serde_json::json!(want), "{v3}");
        }
        // 认不出的对象一律删掉（留着上游只会 11101）。
        let mut v4 = serde_json::json!({
            "model": "auto",
            "messages": [],
            "tool_choice": {"type": "weird"},
        });
        prepare_wb_body(&mut v4);
        assert!(v4.get("tool_choice").is_none(), "{v4}");
    }

    /// 配不成对的 tool_calls 会让**整段会话**的每条请求都 400，必须整条删。
    #[test]
    fn prepare_wb_body_drops_orphan_tool_calls_both_ways() {
        let mut v = serde_json::json!({
            "model": "auto",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call-a", "type": "function", "function": {"name": "read", "arguments": "{}"}},
                    {"id": "call-b", "type": "function", "function": {"name": "read", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "call-a", "content": "ok"},
                {"role": "tool", "tool_call_id": "call-zzz", "content": "orphan"}
            ]
        });
        let changed = prepare_wb_body(&mut v);
        assert!(changed.contains(&"orphan_tool_calls"), "{changed:?}");
        let msgs = v["messages"].as_array().unwrap();
        // call-b 没结果 → 整条 tool_calls 被删（all-or-nothing），连带 call-a 的结果也没了配对。
        assert!(msgs[1].get("tool_calls").is_none(), "{} ", msgs[1]);
        assert_eq!(msgs.len(), 2, "两个 tool 结果都成了孤儿：{msgs:?}");
    }

    /// 关掉思考必须翻成 thinking:{type:disabled}：上游对 off 直接 400，而 none 反而开启。
    #[test]
    fn prepare_wb_body_translates_thinking_off_without_ever_enabling_it() {
        let mut v = serde_json::json!({
            "model": "auto",
            "messages": [],
            "reasoning_effort": "off"
        });
        prepare_wb_body(&mut v);
        assert_eq!(v["thinking"], serde_json::json!({"type": "disabled"}));
        assert!(v.get("reasoning_effort").is_none(), "{v}");
        // 没表态 → 一个思考相关字段都不许自己加（否则账单莫名变高）。
        let mut v2 = serde_json::json!({"model": "auto", "messages": []});
        let changed = prepare_wb_body(&mut v2);
        assert!(v2.get("thinking").is_none());
        assert!(v2.get("reasoning_effort").is_none());
        assert!(!changed.contains(&"thinking"), "{changed:?}");
    }

    /// deepseek 族多轮：只要有一条 assistant 带过 reasoning，其余都得补 reasoning_content。
    #[test]
    fn prepare_wb_body_backfills_reasoning_for_deepseek_only() {
        let mk = |model: &str| {
            serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "assistant", "content": "a1", "reasoning_content": "想过了"},
                    {"role": "assistant", "content": "a2"}
                ]
            })
        };
        let mut ds = mk("deepseek-v4");
        prepare_wb_body(&mut ds);
        assert_eq!(ds["messages"][1]["reasoning_content"], serde_json::json!(""), "{ds}");
        let mut other = mk("hunyuan-4");
        prepare_wb_body(&mut other);
        assert!(
            other["messages"][1].get("reasoning_content").is_none(),
            "非 deepseek 不该被加字段：{other}"
        );
    }
    /// ★ 回归：WorkBuddy 每条流都以 `: heartbeat` 注释行开头，前面还有空行。
    /// 旧实现 trim_start() 只去空白，整段被判成「不是 SSE」→ Ok(None) → 非流式
    /// 聚合路径报 502「上游流式返回里没有可聚合的内容」，而流里明明有完整正文。
    #[test]
    fn aggregate_sse_nonstream_skips_leading_heartbeat_lines() {
        let sse = ": heartbeat\n\ndata:{\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你\"}}]}\n\n\
                   : heartbeat\n\
                   data:{\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"好\"},\"finish_reason\":\"stop\"}]}\n\n\
                   data:[DONE]\n\n";
        let v = aggregate_sse_nonstream(sse)
            .expect("不应报错")
            .expect("心跳注释行不该让它被当成非 SSE");
        assert_eq!(v["choices"][0]["message"]["content"], "你好");
    }

    /// 普通 JSON / HTML 不该被当成 SSE 吞掉（保持原有 502 语义）。
    #[test]
    fn aggregate_sse_nonstream_ignores_non_sse_body() {
        assert!(aggregate_sse_nonstream("{\"ok\":true}").unwrap().is_none());
        assert!(aggregate_sse_nonstream("<!doctype html><html></html>")
            .unwrap()
            .is_none());
        assert!(aggregate_sse_nonstream("").unwrap().is_none());
    }

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

    /// 上游不给 usage 时，流式记账不能恒为 0（旧实现直接赋 parser 的 0）。
    #[test]
    fn stream_stat_estimates_tokens_when_upstream_omits_usage() {
        let mut stat = StreamStat::default();
        // 上游只发正文，不发 usage —— 很多免费 OpenAI 兼容站如此
        stat.absorb(&SseEvent::Data(json!({
            "choices":[{"delta":{"content":"你好世界"}}]
        })));
        assert_eq!(stat.tokens_in, 0);
        assert_eq!(stat.tokens_out, 0);
        assert_eq!(stat.out_chars, 4, "出参字符应被累计");
        // 1000 字节入参 / 4 个字符出参
        stat.fill_missing_tokens(1000);
        assert_eq!(stat.tokens_in, 250);
        assert_eq!(stat.tokens_out, 2);
        // 上游真给了 usage 时不得被估算覆盖
        let mut real = StreamStat::default();
        real.absorb(&SseEvent::Data(json!({
            "choices":[{"delta":{"content":"hi"}}],
            "usage":{"prompt_tokens":11,"completion_tokens":22}
        })));
        real.fill_missing_tokens(1000);
        assert_eq!(real.tokens_in, 11);
        assert_eq!(real.tokens_out, 22);
    }

    #[test]
    fn classify_with_biz_fakesuccess() {
        assert_eq!(
            classify_with_biz(200, r#"{"code":"100002","desc":"登录已失效"}"#),
            ErrorClass::Auth
        );
        assert_eq!(classify_with_biz(429, "rate limited"), ErrorClass::Rate);
    }

    #[test]
    fn utf8_decoder_keeps_split_multibyte_intact() {
        // 逐字节喂入 = 最坏情况：每一个 chunk 边界都切在字符内部。
        // 旧实现用 String::from_utf8_lossy 逐 chunk 解码，这里会得到一串 U+FFFD。
        let s = "你好，世界🌍ABC";
        let mut d = Utf8Decoder::new();
        let mut out = String::new();
        for b in s.as_bytes() {
            out.push_str(&d.push(&[*b]));
        }
        assert_eq!(out, s);
        assert!(!out.contains('\u{FFFD}'), "出现替换字符说明多字节被切坏: {out}");
    }

    #[test]
    fn utf8_decoder_replaces_truly_invalid_bytes_without_wedging() {
        let mut d = Utf8Decoder::new();
        // 0xFF 不可能出现在合法 UTF-8 里：必须立刻替换掉，不能永久留在 pending 里堵住后续解码。
        assert_eq!(d.push(&[b'a', 0xFF, b'b']), "a\u{FFFD}b");
        assert_eq!(d.push("好".as_bytes()), "好");
    }

    #[test]
    fn sse_parser_multibyte_split_across_every_byte() {
        // 复现 P0：流式中文回答在 chunk 边界被切开后永久损坏。
        let mut p = SseParser::new();
        // 用 raw string 避免一层层转义；"\n\n" 单独拼，帧结束符与 JSON 分开看更清楚。
        let payload = String::from(
            r#"data: {"choices":[{"delta":{"content":"中文会坏掉"},"finish_reason":"stop"}]}"#,
        ) + "\n\n";
        let mut events = Vec::new();
        for b in payload.as_bytes() {
            events.extend(p.feed(&[*b]));
        }
        let joined: String = events
            .iter()
            .filter_map(|e| match e {
                SseEvent::Data(v) => v
                    .pointer("/choices/0/delta/content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(joined, "中文会坏掉");
        assert_eq!(p.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn find_frame_end_accepts_crlf() {
        assert_eq!(find_frame_end("a\n\nb"), Some((1, 2)));
        assert_eq!(find_frame_end("a\r\n\r\nb"), Some((1, 4)));
        // 两种都有时取更早的
        assert_eq!(find_frame_end("a\n\nb\r\n\r\n"), Some((1, 2)));
        assert_eq!(find_frame_end("no separator"), None);
        assert_eq!(find_frame_end("only one\nnewline"), None);
    }

    #[test]
    fn status_for_class_is_client_actionable() {
        // 限流必须是 429（SDK 会退避/重试），不能一律 502「网关坏了」。
        assert_eq!(status_for_class(ErrorClass::Rate), 429);
        assert_eq!(status_for_class(ErrorClass::Auth), 401);
        assert_eq!(status_for_class(ErrorClass::Quota), 402);
        assert_eq!(status_for_class(ErrorClass::Rejected), 400);
        assert_eq!(status_for_class(ErrorClass::Waf), 403);
        assert_eq!(status_for_class(ErrorClass::Transient), 502);
        // 车道级封锁/地域封锁是 403 而不是 401：没有凭据可重新认证。
        assert_eq!(status_for_class(ErrorClass::LaneLocked), 403);
        assert_eq!(status_for_class(ErrorClass::Region), 403);
    }
}
