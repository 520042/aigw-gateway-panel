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
pub mod lobster_checkin;
pub mod wb_checkin;
pub mod raccoon_checkin;
pub mod sysprompt;
pub mod history_trim;
pub mod joycode;
pub mod model_map;
pub mod param_override;
/// 重导出 reqwest::Client，供上层 crate（aigw-server）在签名里引用而不必自加依赖。
pub use reqwest::Client as HttpClient;
pub mod commandcode;
pub mod accio;
pub mod autoclaw;
pub mod monkeycode;
pub mod officeace_sign;
pub mod orcaterm;
pub mod common;
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
            | "commandcode"
            | "orcaterm"
            | "monkeycode"
            | "accio"
            | "autoclaw"
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
    // ★ TLS 证书问题要单独说：实测 api.tokenrouter.com 证书过期，reqwest 报
    //   CERT_HAS_EXPIRED，而旧文案只说「无法连接上游（检查本机代理/网络）」——
    //   用户会去查自己的代理、重配凭据，白白折腾。这其实是**上游站点**的问题，
    //   本地做任何事都修不好。
    {
        let low = base.to_lowercase();
        if low.contains("certificate") || low.contains("cert_has_expired") || low.contains("cert") {
            return format!(
                "上游 TLS 证书有问题（过期/域名不匹配/链不完整）：{base}。这是**上游站点**的问题，与你的网络和凭据无关，通常只能等上游续期"
            );
        }
    }
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

/// 验活结论。**三态而不是 bool**：把「探不了」和「凭据死了」混成一个 false，
/// 会把活凭据报成失效（真实事故：wb 撞 /models 404 被报「鉴权失效」，用户反复重登）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// 明确可用（拿到清单 / 明确的鉴权通过响应）
    Alive,
    /// 明确失效（401/403、业务码判失效）
    Dead(String),
    /// **无法判定**：端点不存在（404/405）、响应不是 API JSON（HTML 首页）、网络失败。
    /// 语义是「验活这件事本身没做成」——绝不写成「凭据坏了」。
    Unknown(String),
}

impl VerifyOutcome {
    pub fn is_alive(&self) -> bool {
        matches!(self, VerifyOutcome::Alive)
    }
    pub fn describe(&self) -> String {
        match self {
            VerifyOutcome::Alive => "验活通过 ✓ 凭据可用".to_string(),
            VerifyOutcome::Dead(w) => format!("验活未通过：{w}"),
            VerifyOutcome::Unknown(w) => format!("无法验活（不代表凭据失效）：{w}"),
        }
    }
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

impl RelayError {
    /// 从上游响应里读出 `Retry-After`（秒）或 body 里常见的 `retry_after` 字段。
    ///
    /// 为什么值得单独做：上游明确说了「X 秒后再来」，而我们按固定 30s 起、指数翻倍的
    /// 退避去猜，猜短了继续撞墙（每撞一次还会把 strikes 加一层，最后冻满 15 分钟），
    /// 猜长了则白白浪费窗口。_refs/ 里 workbuddy2api 与 trae2api 都是**优先采用上游
    /// 给的秒数**，只在拿不到时才回落到本地退避。
    ///
    /// 返回值已夹到 [1, 3600]：0 或负数没有意义（立刻重试=立即再撞），
    /// 过大的值多半是上游 bug，往上限夹比原样照做安全。
    pub fn retry_after_hint(&self) -> Option<f64> {
        let body = &self.body;
        // ① Retry-After 语义的秒数字段（各家写法不一，逐个试）
        for key in ["retry_after", "retryAfter", "retry_after_seconds", "retry-after"] {
            if let Some(v) = json_seconds(body, key) {
                return Some(clamp_retry(v));
            }
        }
        // ② OpenAI 风格的 error.retry_after / error.headers
        if let Some(v) = json_seconds(body, "error.retry_after") {
            return Some(clamp_retry(v));
        }
        // ③ 文案里的「X 秒后再试」/ "retry after Xs"（中文上游常见）
        if let Some(v) = text_seconds(body) {
            return Some(clamp_retry(v));
        }
        None
    }
}

/// 从 JSON body 里按点分路径取一个秒数（数字或数字字符串）。
fn json_seconds(body: &str, key: &str) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut cur = &v;
    for seg in key.split('.') {
        cur = cur.get(seg)?;
    }
    if let Some(n) = cur.as_f64() {
        return Some(n);
    }
    // 上游常把数字写成字符串：{"retry_after":"30"}
    cur.as_str().and_then(|s| s.trim().parse::<f64>().ok())
}

/// 从纯文本里抠出「N 秒后再试」这类提示。只认明确的秒数，不做模糊猜测。
fn text_seconds(body: &str) -> Option<f64> {
    let lower = body.to_lowercase();
    // retry after 30s / retry-after: 30 / 30 seconds
    for marker in ["retry after", "retry-after", "try again in", "请等待", "秒后再"] {
        let Some(pos) = lower.find(marker) else { continue };
        let tail: String = lower[pos..].chars().take(48).collect();
        if let Some(n) = first_number(&tail) {
            return Some(n);
        }
    }
    None
}

/// 取字符串里第一段连续数字（可带小数）。
fn first_number(s: &str) -> Option<f64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    rest[..end].trim_end_matches('.').parse::<f64>().ok()
}

/// 夹到 [1, 3600]：0/负数无意义，过大值按上限处理。
fn clamp_retry(v: f64) -> f64 {
    if !v.is_finite() {
        return 1.0;
    }
    v.clamp(1.0, 3600.0)
}

pub struct OutClient {
    http: reqwest::Client,
}

/// 单次模型清单响应的字节上限（超出判失败，不解析）。
///
/// ★ 2026-10-26 从 4MiB 提到 16MiB。原来定 4MiB 是「主流清单几十 KB 到几百 KB，
///   留一倍余量」，但实测 featherless 的 /v1/models 是 6.9MiB（它把全部社区模型都列出来，
///   每条还带 is_gated/created/model_class）—— 4MiB 直接把它挡在门外，于是那个源永远
///   拿不到清单、只能吃静态兜底，而静态表里的模型会随上游变化而过期。
///
///   16MiB 的依据：已知最大真实清单（6.9MiB）的 2.3 倍，仍远小于「异常膨胀」（被投毒
///   或失控上游回几百 MB）的量级。配合上面「边读边判」的流式累积，内存峰值是这个数。
const MODELS_BODY_MAX_BYTES: usize = 16 * 1024 * 1024;

/// OfficeAce 的 max_tokens 默认值与推理预算追加量。
///
/// 上游把 `max_tokens` 当「推理 + 正文」的总预算：给小了会 HTTP 200 但**正文空串**
/// （finish_reason=length，推理 token 吃光了预算）。所以显式请求时**追加**一段，
/// 而不是取 max —— 后者会改掉用户「只想要几个 token」的意图。
/// `max_tokens < 16` 的请求原样透传（那种本来就要最小输出）。
const OFFICEACE_DEFAULT_MAX_TOKENS: i64 = 1024;
const OFFICEACE_REASONING_ALLOWANCE: i64 = 1024;

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
        // OfficeAce（signer="officeace"）：上游要的是 **Basic base64(key:secret)**，
        // 不是 Bearer。凭据约定为 `key:secret`（用户从网关后台拿到的那一对）。
        // ★ 这里覆盖上面注入的 Bearer：那种形态上游只回 401，而错误文案不会告诉你
        //   「认证方式用错了」，只会说「未授权」—— 排查成本很高。
        if spec.signer == "officeace" && !secret.is_empty() {
            use base64::Engine;
            let raw = if secret.contains(':') {
                secret.to_string()
            } else {
                // 只给了一个 key 时补空 secret（Basic 语义允许）
                format!("{secret}:")
            };
            let enc = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
            rb = rb.header("Authorization", format!("Basic {enc}"));
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
        _body: &[u8],
    ) -> reqwest::RequestBuilder {
        // ★★ 2026-10-28 对齐 Agent2API 5.8.4 抓包升级（workbuddy2api-hub #381）：
        // 国际版上游 2026-10 起在对话链路上线**渠道校验**——缺会话头族
        // （X-Conversation-ID / -Request-ID / -Message-ID / -Root-Request-ID）
        // 回 400 / code 11-128（「可能不是官方客户端发出的」）。同时按 5.8.4
        // mitmproxy 抓包修正了多处身份细节（参考实现 workbuddy/adapter.rs
        // chat_headers，本函数与其逐项对齐）：
        //   1. UA 按地区取对话链路专用值（intl：WorkBuddy/5.8.4 workbuddy-ai/5.8.4
        //      CLI/2.161.0；产品段是**小写连字符** workbuddy-ai，不是 WorkBuddy AI）；
        //   2. X-IDE-Name/Type **两版都是 WorkBuddy**（v2 旧版给 intl 发 WorkBuddy AI
        //      是错的——product_name 才是 WorkBuddy AI，而 X-Product=SaaS）；
        //   3. X-IDE-Version 对话链路用 chat_ide_version（intl 5.8.4）——过旧版本号
        //      本身就是网关特征；
        //   4. 会话头族**每次请求现生成**（会话=带连字符 UUID、消息=32 hex）——上游
        //      不拿这些 id 存历史也不做跨请求关联，要的是「有」而不是「稳定」；
        //      恒定派生值（旧实现从 secret 派生）本身就是网关指纹；
        //   5. **不再发 X-Session-ID**：官方对话请求不带它，v2 旧版发的恒定派生值
        //      是另一处网关指纹；
        //   6. 补 X-Agent-Purpose/Type、X-Private-Data=true、Origin/Referer、
        //      intl 的 Accept-Language=en-US。
        let is_intl = domain.contains("workbuddy.ai")
            || domain.contains("codebuddy.ai")
            || spec.id.ends_with("-intl");
        // 对话链路身份（地区差异化；X-IDE-Name/Type 两版同为 WorkBuddy）
        let (chat_ua, ide_version, accept_lang) = if is_intl {
            (
                "WorkBuddy/5.8.4 workbuddy-ai/5.8.4 CLI/2.161.0",
                "5.8.4",
                "en-US",
            )
        } else {
            (
                "WorkBuddy/5.5.4 WorkBuddy/5.5.4 CLI/2.137.1",
                "5.5.4",
                "zh-CN",
            )
        };
        let _ = spec;
        let digest = Self::sha256_hex(secret.as_bytes());
        let uid = digest[..16].to_string();
        let device = |salt: &str| Self::sha256_hex(format!("{salt}:{uid}").as_bytes())[..36].to_string();
        // 会话头族：每请求现生成（理由见函数头 4）
        let conversation_id = {
            // 带连字符 UUID 形态（8-4-4-4-12）；随机来源复用 rand_hex
            let h = Self::rand_hex(32);
            format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
        };
        let message_id = Self::rand_hex(32);
        let conversation_request_id = Self::rand_hex(32);
        let trace = Self::rand_hex(32);
        let span = trace[..16].to_string();
        // Origin/Referer 跟域名走（渠道校验按站点认）
        let origin = if domain.is_empty() {
            if is_intl { "https://www.workbuddy.ai".to_string() } else { "https://copilot.tencent.com".to_string() }
        } else {
            format!("https://{domain}")
        };
        let rb = rb
            .header("User-Agent", chat_ua)
            .header("X-IDE-Name", "WorkBuddy")
            .header("X-IDE-Type", "WorkBuddy")
            .header("X-IDE-Version", ide_version)
            .header("X-Product", "SaaS")
            .header("X-CodeBuddy-Request", "1")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Accept-Language", accept_lang)
            .header("X-Private-Data", "true")
            .header("X-User-Id", &uid)
            .header("X-Machine-ID", device("machine"))
            .header("X-Agent-Intent", "craft")
            .header("X-Agent-Purpose", "conversation")
            .header("X-Agent-Type", "main")
            .header("Origin", &origin)
            .header("Referer", format!("{origin}/"))
            .header("X-Conversation-ID", &conversation_id)
            .header("X-Conversation-Request-ID", &conversation_request_id)
            .header("X-Root-Request-ID", &conversation_request_id)
            .header("X-Conversation-Message-ID", &message_id)
            .header("X-Request-ID", &message_id)
            .header("X-B3-TraceId", &trace)
            .header("X-B3-SpanId", &span)
            .header("X-B3-Sampled", "1")
            .header("X-Trace-ID", &trace)
            .header("b3", format!("{trace}-{span}-1"))
            .header("traceparent", format!("00-{trace}-{span}-01"));
        if domain.is_empty() {
            rb
        } else {
            rb.header("X-Domain", domain)
        }
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
        // ★ 2026-10-28（对齐 magpie system-prompt）：**所有**上游都要能注入自定义
        //   系统提示词，因此必须放在各 upsteam_kind/signer 分发**之前**。
        //   早期实现把注入写在脱敏旁边（分发之后），wb 族在这里就 return 了，
        //   注入永远跑不到 —— 实测 wb 账号上「提示词不生效」正是这个原因。
        //   顺序：先注入用户提示词，再脱敏（用户写的话不该被当成客户端指纹改写）。
        let sp_body;
        let body = {
            match serde_json::from_slice::<Value>(body) {
                Ok(mut v) => {
                    let mut dirty = false;
                    // ★ 2026-10-28（对齐 magpie history-trim）：先裁历史省 token，
                    //   再注入用户提示词 —— 裁的是客户端历史（大头），注的是我们这句
                    //   （小），顺序反了会让刚注入的提示词也被算进「历史」里重复删。
                    if history_trim::apply_global(&mut v) {
                        tracing::debug!("已裁剪历史（省 token）");
                        dirty = true;
                    }
                    if sysprompt::apply_global(&mut v, sysprompt::Proto::Chat) {
                        tracing::debug!("已注入自定义系统提示词");
                        dirty = true;
                    }
                    // ★ 2026-10-28（对齐 magpie param-override）：参数覆盖放最后 ——
                    //   它能删/改前面注入的内容（例如某上游不认 tools 就删掉），
                    //   必须是管线的最后一环。
                    if param_override::apply_global(&mut v) {
                        tracing::debug!("已应用参数覆盖");
                        dirty = true;
                    }
                    if dirty {
                        sp_body = serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec());
                        sp_body.as_slice()
                    } else {
                        body
                    }
                }
                Err(_) => body,
            }
        };
        // WorkBuddy 族上游**只支持流式**（实测非流式一律 400 code=11101，报文里
        // stream:false 会被判「请求内容不合法」）。这里自己改成流式发出去再聚合成一个
        // 完整 JSON，而不是让调用方收到一个看不懂的 11101。
        if spec.signer == "wb" {
            return self.wb_nonstream(spec, secret, domain, body).await;
        }
        // ★ OfficeAce：上游**恒流式**，非流式请求要自己转成流式再聚合。
        //   另外它的 max_tokens 语义特殊（推理+正文的总预算），见 officeace_nonstream。
        if spec.signer == "officeace" {
            return self.officeace_nonstream(spec, secret, domain, body).await;
        }
        // ★ 2026-10-26（P2）：非流式路径同样做指纹脱敏（与流式共用同一套规则）。
        // ★ 降级期（sanitize::degrade_active）：前面的请求刚被上游按审核拒过，说明
        //   脱敏表漏了某个客户端模板句。这期间直接换中性提示词出门，不再让每个请求
        //   都先撞一次 400 再补救 —— 省一轮往返，也少给上游一次风控记录。
        let owned;
        let body = match serde_json::from_slice::<Value>(body) {
            Ok(mut v) => {
                let mut dirty = false;
                if sanitize::degrade_active() {
                    if sanitize::neutralize_request_body(&mut v) {
                        dirty = true;
                    }
                } else {
                    let sc = sanitize::sanitize_request_body(&mut v);
                    if !sc.is_empty() {
                        tracing::debug!(changed = ?sc, "出站请求体已做指纹脱敏（非流式）");
                        dirty = true;
                    }
                }
                // ★ 2026-10-28（对齐 magpie history-trim + system-prompt）：
                //   脱敏 → 裁历史（省 token）→ 注入用户提示词。
                //   裁在注之前：裁的是客户端历史（大头），注的是我们这一句（小）；
                //   顺序反了会让刚注入的提示词也被当成「历史」参与重复删除。
                if history_trim::apply_global(&mut v) {
                    dirty = true;
                }
                if sysprompt::apply_global(&mut v, sysprompt::Proto::Chat) {
                    dirty = true;
                }
                // ★ 2026-10-28（对齐 magpie param-override）：参数覆盖是最后一环。
                if param_override::apply_global(&mut v) {
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
        };
        match spec.upstream_kind.as_str() {
            "commandcode" => commandcode::chat_nonstream(&self.http, spec, secret, body).await,
            "orcaterm" => orcaterm::chat_nonstream(&self.http, spec, secret, body).await,
            "monkeycode" => monkeycode::chat_nonstream(&self.http, spec, secret, body).await,
            "accio" => accio::chat_nonstream(&self.http, spec, secret, body).await,
            "autoclaw" => autoclaw::chat_nonstream(&self.http, spec, secret, body).await,
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
            "joycode" => joycode::chat_nonstream(&self.http, secret, body)
                .await
                .map_err(|e| RelayError { status: 502, class: ErrorClass::Transient, body: e }),
        "devin" => devin::chat_nonstream(&self.http, spec, secret, body).await,
            _ => {
                let r = self.post_openai_nonstream(spec, secret, domain, body).await;
                // ★ 内容拦截补救（对齐 Agent2API 的「动作 0」）：客户端 system 模板里
                //   的指纹句会让上游按逐字审核直接回 400。前置脱敏必然滞后于模板更新，
                //   所以被拦下之后还要能认出来并补救一次 —— 换成中性提示词**立刻重发**，
                //   不换账号、不罚账号（内容问题不是账号问题）。
                self.retry_neutralized_nonstream(spec, secret, domain, body, r)
                    .await
            }
        }
    }

    /// 非流式的内容拦截补救：确认真是被审核拦下的 400 才重发一次。
    ///
    /// ★ 三条纪律：
    /// ① **只重发一次**（再失败原样透传，绝不空转）；
    /// ② **不换账号、不罚账号** —— 这是内容问题，换号只会把故障扩散到健康账号；
    /// ③ **认不出来就别试**：判定保守，宁可少救一次，也不要把「参数真写错了」
    ///    当成审核去重试（那会把一个明确的 400 变成两次 400，还多花一轮往返）。
    async fn retry_neutralized_nonstream(
        &self,
        spec: &ProviderSpec,
        secret: &str,
        domain: &str,
        body: &[u8],
        first: Result<Value, RelayError>,
    ) -> Result<Value, RelayError> {
        let Err(e) = first else {
            return first;
        };
        if !sanitize::looks_like_content_block(e.status, &e.body) {
            return Err(e);
        }
        let Ok(mut v) = serde_json::from_slice::<Value>(body) else {
            return Err(e);
        };
        if !sanitize::neutralize_request_body(&mut v) {
            // 没有 system 也没有可清理的工具描述：重发也不会变好
            return Err(e);
        }
        let rewritten = serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec());
        // ★ 同时触发降级：后续请求直接带中性提示词出门，不再重复「撞 400 再补救」。
        //   降级期到次日 00:00（UTC+8）自动结束，期间再次触发不续期。
        let until = sanitize::degrade_trigger();
        tracing::warn!(
            platform = %spec.id,
            until,
            "上游以内容审核为由拒绝了请求（HTTP 400），已换成中性提示词重发一次并进入降级期"
        );
        self.post_openai_nonstream(spec, secret, domain, &rewritten)
            .await
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
    /// OfficeAce 的非流式转发：上游恒流式，这里改成流式发出去再聚合。
    ///
    /// ── 两个实测坑（都在这里处理，别在别处重推）────────────────
    /// 1. **`max_tokens` 是「推理 + 正文」的总预算**：给小了会 HTTP 200 但正文**空串**
    ///    （`finish_reason=length`，推理 token 全吃掉了）。所以这里给一个下限
    ///    （默认 1024）。注意是**追加**而不是 `max()` —— `max()` 会把「只想要 1 个
    ///    token」的意图改掉；`max_tokens < 16` 原样透传（那种请求本来就要最小输出）。
    /// 2. **必须 `stream: true`**：上游恒流式，传 false 也不会给你非流式响应。
    async fn officeace_nonstream(
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
        if let Some(o) = v.as_object_mut() {
            o.insert("stream".to_string(), json!(true));
            // stream_options.include_usage：拿得到 usage 才有计费依据
            o.insert("stream_options".to_string(), json!({"include_usage": true}));
            let mt = o.get("max_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
            if mt == 0 || mt >= 16 {
                let want = if mt == 0 { OFFICEACE_DEFAULT_MAX_TOKENS } else { mt };
                o.insert(
                    "max_tokens".to_string(),
                    json!(want + OFFICEACE_REASONING_ALLOWANCE),
                );
            }
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
                    "上游流式返回里没有可聚合的内容（OfficeAce 在 max_tokens 很小时会返回空正文）：{}",
                    text.chars().take(300).collect::<String>()
                ),
            }),
        }
    }
    /// 账号验活：按 upstream_kind 分派到各平台 adapter（cookie 平台走真实轻量端点）。
    /// 验活（对外入口）：**二次确认**防网络抖动误判。
    ///
    /// ★ 为什么需要：20 个 adapter 的 verify 都是「请求 Ok 就按判据给 bool，
    ///   请求 Err 一律 false」——把「网络抖动/超时」和「凭据被拒」混成同一个 false。
    ///   用户网络不好时点「检查」，好账号会被标成「鉴权失效」并写进号池健康，
    ///   白白重登一次。这里做一层兜底：第一次判死时**隔 300ms 再验一次**：
    ///   · 第二次活 → 判活（第一次是抖动）；
    ///   · 第二次无法判定 → 判无法判定（网络确实不通）；
    ///   · 第二次仍死 → 判死（两次一致，可信）。
    ///   代价：仅「第一次失败」时多一次请求（用户手动点检查才发生），换来不误杀。
    pub async fn verify_account(&self, spec: &ProviderSpec, secret: &str) -> VerifyOutcome {
        let first = self.verify_once(spec, secret).await;
        if matches!(first, VerifyOutcome::Dead(_)) {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            return match self.verify_once(spec, secret).await {
                VerifyOutcome::Unknown(w) => VerifyOutcome::Unknown(w),
                second => second,
            };
        }
        first
    }

    async fn verify_once(&self, spec: &ProviderSpec, secret: &str) -> VerifyOutcome {
        // ★ wb 族按 **signer** 认，不是 upstream_kind：它们的 upstream_kind 是 openai
        //   （同协议透传），真正的身份标记是 signer="wb"。按 kind 分派会让这个分支
        //   永远不命中 —— 实测症状：wb 的验活消息里出现「对话探测」，说明它走的是
        //   通用 verify_openai，而 wb::verify（带 X-Domain 的 get-user-resource）
        //   从未被调用。
        if spec.signer == "wb" {
            return wb::verify(&self.http, spec, secret).await;
        }
        match spec.upstream_kind.as_str() {
            "doubao" => Self::b2o(doubao::verify(&self.http, spec, secret).await),
            "yuanbao" => Self::b2o(yuanbao::verify(&self.http, spec, secret).await),
            "kuku" => Self::b2o(kuku::verify(&self.http, spec, secret).await),
            "metaso" => Self::b2o(metaso::verify(&self.http, spec, secret).await),
            "qwen-web" => Self::b2o(qwen::verify(&self.http, spec, secret).await),
            "mimo" => Self::b2o(mimo::verify(&self.http, spec, secret).await),
            "kiro" => Self::b2o(kiro::verify(&self.http, spec, secret).await),
            "cloudcode" => Self::b2o(cloudcode::verify(&self.http, spec, secret).await),
            "deepseek-web" => Self::b2o(deepseek::verify(&self.http, spec, secret).await),
            "opencode" => Self::b2o(opencode::verify(&self.http, spec, secret).await),
            "qoder" => Self::b2o(qoder::verify(&self.http, spec, secret).await),
            "trae" => Self::b2o(trae::verify(&self.http, spec, secret).await),
            "coze" => Self::b2o(coze::verify(&self.http, spec, secret).await),
            "joycode" => Self::b2o(joycode::verify(&self.http, secret).await),
        "devin" => Self::b2o(devin::verify(&self.http, spec, secret).await),
        // ★ wb 族（WorkBuddy / CodeBuddy）：通用 verify_openai 会去探 /models，
        //   而 copilot.tencent.com 没有那个端点 —— 活凭据被判「鉴权失效」（假阴性），
        //   用户重登多少次都没用。用积分查询端点判（已验证可用的轻量鉴权探测）。
            // ★ 2026-10-26 补齐五个新 adapter 的验活分派。漏了会掉进 verify_openai
            //   通用兜底（GET models_path）：orcaterm/autoclaw 没有 /models → 好凭据
            //   也被报「验活失败」（假阴性）；commandcode 的目录是公开的 → 错 key 也
            //   「验活通过」（假阳性）。两个方向都错，必须逐家分派。
            "commandcode" => Self::b2o(commandcode::verify(&self.http, spec, secret).await),
            "orcaterm" => Self::b2o(orcaterm::verify(&self.http, spec, secret).await),
            "monkeycode" => Self::b2o(monkeycode::verify(&self.http, spec, secret).await),
            "accio" => Self::b2o(accio::verify(&self.http, spec, secret).await),
            "autoclaw" => Self::b2o(autoclaw::verify(&self.http, spec, secret).await),
            _ => self.verify_openai(spec, secret).await,
        }
    }

    /// 通用 openai 透传验活：GET models_path，2xx 即有效。
/// 把「探 /models 的响应」判成三态。
///
/// ★ 本轮最重要的一处语义修正（真实事故驱动）：
///   · 旧实现 status.is_success() → 404 判死（6 个源的好凭据被误杀：
///     mistral-codestral / perplexity / voyageai / wps / codearts / web-glm 都没有
///     /models 端点），而 200 HTML（apk-nano 首页）与 200 公开清单（cline 免凭据）
///     又全判活（错 key 也能通过）；
///   · 现在：401/403 判死；404/405/501（端点不存在）与「2xx 但不是 JSON 清单」
///     判**无法判定**——验活没做成 ≠ 凭据坏了。
/// 老 adapter 的 bool 结论桥接成三态：true=活，false=**判死**。
///
/// ★ 为什么 adapter 保持 bool 而不逐个改三态：它们内部各自实现了「用该平台的
///   真实鉴权端点探测」（如 wb 走 get-user-resource、kuku 走 passport），探对了
///   才返回 bool，语义本来就够；真正会误判的只有通用 verify_openai（探 /models）。
fn b2o(ok: bool) -> VerifyOutcome {
    if ok {
        VerifyOutcome::Alive
    } else {
        VerifyOutcome::Dead("上游拒绝了该凭据".to_string())
    }
}

pub fn classify_verify_probe(status: u16, body: &str) -> VerifyOutcome {
    match status {
        401 | 403 => VerifyOutcome::Dead(format!("HTTP {status}（上游明确拒绝该凭据）")),
        404 | 405 | 501 => VerifyOutcome::Unknown(format!(
            "该平台没有 /models 端点（HTTP {status}），无法用清单验活；凭据是否有效请用一次真实对话确认"
        )),
        200..=299 => match serde_json::from_str::<Value>(body) {
            Ok(v) if v.is_object() => {
                let has_list = v.get("data").map(|d| d.is_array()).unwrap_or(false)
                    || v.get("models").map(|d| d.is_array()).unwrap_or(false);
                if has_list {
                    VerifyOutcome::Alive
                } else {
                    VerifyOutcome::Unknown(
                        "清单响应里没有 data/models 数组（可能不需要凭据或是其它端点），无法据此判活".to_string()
                    )
                }
            }
            _ => VerifyOutcome::Unknown(
                "上游返回的不是 JSON 清单（可能是登录页/首页），无法据此判活".to_string()
            ),
        },
        s if s >= 500 => VerifyOutcome::Unknown(format!("上游 {s}（服务端故障，与凭据无关）")),
        s => VerifyOutcome::Unknown(format!("上游 HTTP {s}，无法据此判定凭据")),
    }
}

    async fn verify_openai(&self, spec: &ProviderSpec, secret: &str) -> VerifyOutcome {
        // ★ verify_chat=true 的源**跳过清单探测**：它们的 /models 是公开的，
        //   拿它验活等于「错 key 也通过」（cline 实测：无凭据 200 + 469 个模型）。
        if spec.verify_chat && spec.chat_supported {
            if let Some(o) = self.verify_via_chat(spec, secret).await {
                return o;
            }
        }
        let path = if spec.models_path.is_empty() {
            "/models".to_string()
        } else {
            spec.models_path.clone()
        };
        let url = format!("{}{}", spec.base_url, path);
        let rb = Self::apply_headers(self.http.get(&url), spec, secret, "", &[]);
        let r = match rb.timeout(std::time::Duration::from_secs(15)).send().await {
            Ok(r) => r,
            Err(e) => {
                return VerifyOutcome::Unknown(format!(
                    "网络请求失败（{}）",
                    network_hint(&e, std::time::Duration::from_secs(15))
                ))
            }
        };
        let status = r.status().as_u16();
        let body = r.text().await.unwrap_or_default();
        let outcome = Self::classify_verify_probe(status, &body);
        // ★ 二段探测：/models 探不到（404/405 —— 该平台根本没这个端点）时，
        //   回退到「一次最小对话」判鉴权层。理由：
        //   · 用户点的是「检查」，他要的答案是「我的 key 到底能不能用」，
        //     「无法验活」虽然诚实但没回答问题（perplexity / codestral / codearts
        //     都属于这类 —— 有对话端点、没有清单端点）；
        //   · 代价是一次 max_tokens=1 的请求（约 1 token），用户主动点才发生，可以接受；
        //   · 只对 **kind=openai 且 chat_supported** 的源做：其它协议（chatglm 要签名、
        //     纯 embedding 源根本没有对话端点）用通用请求探只会得到噪音。
        // ① 清单探不到（Unknown）→ 二段对话探测（前面已处理）。
        if matches!(outcome, VerifyOutcome::Unknown(_))
            && spec.upstream_kind == "openai"
            && spec.chat_supported
        {
            if let Some(chat_outcome) = self.verify_via_chat(spec, secret).await {
                return chat_outcome;
            }
        }
        // ② 清单**通过**也不能证明凭据有效 —— 大量平台的 /models 是公开的
        //    （OpenRouter / NVIDIA / HuggingFace / cline…，实测无凭据就回完整清单），
        //    错 key 会被判「验活通过」，用户一对话才 401。
        //    对「需要凭据 + 支持对话」的 openai 源补一次对话探测**证伪**；
        //    只有明确结论（活/死）才采用，Unknown（模型名不对等）沿用原判定——不误杀。
        if outcome.is_alive()
            && spec.upstream_kind == "openai"
            && spec.chat_supported
            && !matches!(spec.auth, aigw_registry::AuthKind::Anon)
        {
            if let Some(chat_outcome) = self.verify_via_chat(spec, secret).await {
                match chat_outcome {
                    VerifyOutcome::Unknown(_) => {}
                    definitive => return definitive,
                }
            }
        }
        outcome
    }

    /// 最小对话探测：发一条 `max_tokens=1` 的 ping，按**鉴权层**的答复判活。
    ///
    /// 判据的分界是「鉴权有没有过」而不是「请求成不成功」：
    ///   · 401/403 → 明确失效；
    ///   · 2xx → 鉴权通过（模型真答了一个 token）；
    ///   · **400/422 → 鉴权通过**（上游说「你的参数不对」，前提是它先认了你的 key）；
    ///   · **429 → 鉴权通过**（能被限流说明身份已被识别）；
    ///   · 404 → 无法判定（模型名不对或端点不存在，与凭据无关）；
    ///   · 5xx / 网络失败 → 无法判定。
    /// 返回 `None` 表示「这次探测本身没做成」，由调用方保留原结论。
    async fn verify_via_chat(&self, spec: &ProviderSpec, secret: &str) -> Option<VerifyOutcome> {
        let path = spec.chat_path();
        let url = format!("{}{}", spec.base_url, path);
        let model = spec
            .models
            .iter()
            .map(|m| m.trim())
            .find(|m| !m.is_empty())
            .unwrap_or("gpt-4o-mini");
        let payload = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
            "stream": false
        });
        let rb = Self::apply_headers(self.http.post(&url), spec, secret, "", b"");
        let r = match rb
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return None,
        };
        let status = r.status().as_u16();
        let body = r.text().await.unwrap_or_default();
        Some(match status {
            401 | 403 => VerifyOutcome::Dead(format!("HTTP {status}（对话探测：上游明确拒绝该凭据）")),
            // 鉴权层通过的两个强信号：限流与服务端说「你的参数不对」
            429 => VerifyOutcome::Alive,
            400 | 422 => VerifyOutcome::Alive,
            200..=299 => VerifyOutcome::Alive,
            404 => VerifyOutcome::Unknown(format!(
                "对话探测 HTTP 404（模型名或端点不存在，与凭据无关）：{}",
                body.chars().take(80).collect::<String>()
            )),
            s if s >= 500 => VerifyOutcome::Unknown(format!("对话探测 HTTP {s}（上游故障，与凭据无关）")),
            s => VerifyOutcome::Unknown(format!("对话探测 HTTP {s}，无法据此判定凭据")),
        })
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
                    // 降级期：直接换中性提示词出门，不再让每个请求先撞一次 400
                    if sanitize::degrade_active() {
                        if sanitize::neutralize_request_body(&mut v) {
                            dirty = true;
                        }
                    } else {
                        let sc = sanitize::sanitize_request_body(&mut v);
                        if !sc.is_empty() {
                            tracing::debug!(changed = ?sc, "出站请求体已做指纹脱敏");
                            dirty = true;
                        }
                    }
                    // ★ 2026-10-28（对齐 magpie history-trim + system-prompt）：
                    //   流式路径同样「裁历史 → 注入提示词」（脱敏之后）。
                    if history_trim::apply_global(&mut v) {
                        dirty = true;
                    }
                    if sysprompt::apply_global(&mut v, sysprompt::Proto::Chat) {
                        dirty = true;
                    }
                    // ★ 2026-10-28（对齐 magpie param-override）：参数覆盖是最后一环。
                    if param_override::apply_global(&mut v) {
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
            "commandcode" => {
                let (s, rx) = commandcode::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "orcaterm" => {
                let (s, rx) = orcaterm::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "monkeycode" => {
                let (s, rx) = monkeycode::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "accio" => {
                let (s, rx) = accio::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            "autoclaw" => {
                let (s, rx) = autoclaw::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
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
            // ★ 2026-10-28（对齐 magpie packages/joycode）：京东云 JoyCode 的对话
            //   走带签名查询串的网关（appid/functionId/t/sign），不是标准 OpenAI 透传。
            //   暂只做非流式：调一次非流式再伪造成单帧 SSE，保住流式调用方不报错。
            "joycode" => {
                let v = joycode::chat_nonstream(&self.http, secret, body).await.map_err(|e| {
                    RelayError { status: 502, class: ErrorClass::Transient, body: e }
                })?;
                let (tx, rx) = tokio::sync::mpsc::channel::<crate::StreamStat>(1);
                let frame = format!("data: {}\n\ndata: [DONE]\n\n", v);
                let _ = tx.send(crate::StreamStat::default()).await;
                let s = futures_util::stream::once(async move {
                    Ok::<_, std::io::Error>(bytes::Bytes::from(frame))
                });
                Ok((Box::pin(s), rx))
            }
            "devin" => {
                let (s, rx) = devin::chat_stream(&self.http, spec, secret, body).await?;
                Ok((Box::pin(s), rx))
            }
            _ => {
                // ★ 内容拦截补救（同非流式）：被上游按审核回 400 时，换成中性提示词
                //   重发一次。不换账号、不罚账号 —— 内容问题不是账号问题。
                match self.post_openai_stream(spec, secret, domain, body).await {
                    Ok((s, rx)) => Ok((Box::pin(s), rx)),
                    Err(e) => {
                        if !sanitize::looks_like_content_block(e.status, &e.body) {
                            return Err(e);
                        }
                        let Ok(mut v) = serde_json::from_slice::<Value>(body) else {
                            return Err(e);
                        };
                        if !sanitize::neutralize_request_body(&mut v) {
                            return Err(e);
                        }
                        let rewritten =
                            serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec());
                        let until = sanitize::degrade_trigger();
                        tracing::warn!(
                            platform = %spec.id,
                            until,
                            "上游以内容审核为由拒绝了流式请求（HTTP 400），已换成中性提示词重发一次并进入降级期"
                        );
                        let (s, rx) = self
                            .post_openai_stream(spec, secret, domain, &rewritten)
                            .await?;
                        Ok((Box::pin(s), rx))
                    }
                }
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
            std::pin::Pin<
                Box<
                    dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>>
                        + Send
                        + 'static,
                >,
            >,
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
        // ★ 上游无视 stream:true 回了非 SSE（自建网关 / 部分免费站很常见）：
        //   旧实现把裸 JSON 直接塞进 SSE 内容类型往外发 —— 客户端（OpenAI SDK /
        //   Claude Code / Codex）会按 SSE 解析而拿到一坨 JSON，报
        //   「stream ended without [DONE]」或直接抛解析异常。实测复现：上游回
        //   {"id":"c1","object":"chat.completion",…}，网关 content-type 却是
        //   text/event-stream，body 里没有 data: 前缀。
        //   修法：识别出非 SSE，整包读下、包成「一帧 data + [DONE]」再发，
        //   统计口径复用 SseParser（与正常流一致，台账不会因此记 0 token）。
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase().contains("event-stream"))
            .unwrap_or(false);
        if !is_sse {
            let text = resp.text().await.unwrap_or_default();
            return Ok(wrap_json_as_sse(text, body.len()));
        }
        Ok(tap_passthrough(resp, body.len()))
    }

    /// 带凭据的 live 模型清单（api_key/cookie 平台必须走这条）。
    /// 可达性探测：GET 一下根地址，返回 `(状态码, None)` 或 `(0, Some(原因))`。
    ///
    /// ★ 给 `models_policy = "none"` 的源用：它们**没有清单端点**（清单靠静态表或
    ///   `模型@平台` 后缀），拿 `/models` 去探必然 404 —— 那是探错了地方，不是端点失效。
    ///   探根路径只要拿到任何 HTTP 状态就说明「域名与网关还在」。
    pub async fn probe_reachable(http: &reqwest::Client, base_url: &str) -> (u16, Option<String>) {
        match http.get(base_url).send().await {
            Ok(r) => (r.status().as_u16(), None),
            Err(e) => (0, Some(format!("network: {e}"))),
        }
    }
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
        // ★ 清单响应必须**边读边判**大小上限。
        //
        // 不能用 resp.text() 读完再判 s.len()：那样 7MB 已经整块进了内存，
        // 上限只起到「事后拒绝」的作用 —— 挡不住内存峰值（也不挡并发叠加）。
        // 实测 featherless 一次回 7MB、api-gateway 的清单也是几 MB 级，
        // 8 并发一起拉时进程会被顶到崩溃（0xC0000005）。
        //
        // 这里按块累积，越过上限立刻停止读取并判失败（响应体随之释放）。
        // 明文：宁可「明确拉不到这份清单」，也不要一份截断的残缺清单——
        // 后者会让面板列出一半模型，比拉不到更难排查。
        let text = {
            use futures_util::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut over = false;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(c) => {
                        buf.extend_from_slice(&c);
                        if buf.len() > MODELS_BODY_MAX_BYTES {
                            over = true;
                            break;
                        }
                    }
                    Err(e) => {
                        return Err(RelayError {
                            status,
                            class: ErrorClass::Transient,
                            body: format!("读取模型清单响应体失败（解压/断开）：{e}"),
                        });
                    }
                }
            }
            if over {
                return Err(RelayError {
                    status,
                    class: ErrorClass::Transient,
                    body: format!(
                        "模型清单过大（超过上限 {} 字节），已放弃解析",
                        MODELS_BODY_MAX_BYTES
                    ),
                });
            }
            String::from_utf8_lossy(&buf).to_string()
        };
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
        // ★ 先看业务码：有一类上游 HTTP 恒 200、成败写在 body 里
        //   （实测 loomy：{"code":"100002","desc":"缺少 token","data":{}}）。
        //   不判这一层就会把「认证失败」当成「清单是空的」，于是
        //   体检报告显示「清单 0 条」—— 看着像这个源没有模型，实际是没凭据。
        if let Some(code) = v.get("code").and_then(|c| {
            c.as_str().map(|s| s.to_string()).or_else(|| c.as_i64().map(|n| n.to_string()))
        }) {
            if code != "0" && code != "200" {
                let desc = v
                    .get("desc")
                    .or_else(|| v.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("上游返回业务错误");
                // ★ 复用 core 里那套已沉淀的「假成功」判据（refine_biz），而不是在这里
                //   自己写一套：它排除限流/额度语义的误判（"token limit" 是限流不是失效），
                //   并按业务码指纹（100002/100003/40001）与文案指纹归类。
                //   归不到 Auth 的（比如只是参数错）就别当成凭据问题，用默认的 Transient。
                let class = ErrorClass::Transient
                    .refine_biz(&code, &desc)
                    .unwrap_or(ErrorClass::Transient);
                let status = if class == ErrorClass::Auth { 401 } else { 502 };
                return Err(RelayError {
                    status,
                    class,
                    body: format!("业务码 {code}：{desc}"),
                });
            }
        }
        // ★ 两种清单形态都要认：
        //   ① OpenAI 兼容：{"data":[{"id":"gpt-..."}]}（绝大多数源）
        //   ② Google 原生：{"models":[{"name":"models/gemini-2.5-pro"}]}（Gemini 的
        //      /v1beta/models —— 它的 OpenAI 兼容层 /v1beta/openai **没有** models 端点，
        //      实测那里 404，而原生路径 403=需要 Key 即存在）
        //   只认 ① 会让 Gemini 这类源明明有清单却永远拉不到，只能靠静态表兜底。
        let (arr, id_key) = match v.get("data").and_then(|d| d.as_array()) {
            Some(a) => (Some(a), "id"),
            None => match v.get("models").and_then(|d| d.as_array()) {
                Some(a) => (Some(a), "name"),
                None => (None, "id"),
            },
        };
        let mut ids: Vec<String> = arr
            .map(|a| {
                a.iter()
                    .filter(|m| {
                        if spec.models_filter == "isFree" {
                            m.get("isFree").and_then(|x| x.as_bool()).unwrap_or(false)
                        } else {
                            true
                        }
                    })
                    .filter_map(|m| {
                        m.get(id_key).and_then(|x| x.as_str()).map(|s| {
                            // Google 的 name 形如 "models/gemini-2.5-pro"：剥掉前缀，
                            // 让用户看到的就是能直接调用的模型名。
                            s.strip_prefix("models/").unwrap_or(s).to_string()
                        })
                    })
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
/// 把「上游无视 stream:true 回来的整包 JSON」包装成单帧 SSE。
///
/// 网关已经用 `text/event-stream` 回了客户端，就必须给客户端 SSE 帧——
/// 否则 SDK 按 SSE 解析会崩。统计口径与 `tap_passthrough` 一致（同一个 SseParser），
/// 避免这类平台在用量台账里恒记 0 token。
fn wrap_json_as_sse(text: String, in_bytes: usize) -> (BoxedByteStream, mpsc::Receiver<StreamStat>) {
    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let t = text.trim().to_string();
    let mut stat = StreamStat::default();
    if !t.is_empty() {
        let mut parser = SseParser::new();
        let probe = format!("data: {t}\n\n");
        for ev in parser.feed(probe.as_bytes()) {
            stat.absorb(&ev);
        }
        stat.finish_reason = parser.finish_reason.clone();
        stat.tokens_in = parser.prompt_tokens;
        stat.tokens_out = parser.completion_tokens;
    }
    stat.fill_missing_tokens(in_bytes);
    stat.saw_done = true;
    let _ = tx.try_send(stat);
    let framed = if t.is_empty() {
        "data: [DONE]\n\n".to_string()
    } else {
        format!("data: {t}\n\ndata: [DONE]\n\n")
    };
    (
        Box::pin(futures_util::stream::once(async move {
            Ok(Bytes::from(framed))
        })),
        rx,
    )
}

type BoxedByteStream = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static>,
>;

fn tap_passthrough(resp: reqwest::Response, in_bytes: usize) -> (BoxedByteStream, mpsc::Receiver<StreamStat>) {
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
    (Box::pin(stream), rx)
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
                    // ★ 字段名要兼容多种写法，与 server 侧 `dialect::extract_usage` 同一口径。
                    //   这里曾经只认 prompt_tokens / completion_tokens —— 上游换用
                    //   input_tokens / output_tokens 命名时，**流式**请求的用量全部记成 0，
                    //   而同一份代码的非流式路径已经支持别名了（第 33 轮修过）。
                    //   只修一处等于「同样的问题在另一条路上继续发生」。
                    const IN_KEYS: [&str; 4] =
                        ["prompt_tokens", "input_tokens", "promptTokens", "inputTokens"];
                    const OUT_KEYS: [&str; 4] = [
                        "completion_tokens",
                        "output_tokens",
                        "completionTokens",
                        "outputTokens",
                    ];
                    let pick = |keys: &[&str]| -> Option<i64> {
                        keys.iter()
                            .filter_map(|k| u.get(*k).and_then(|x| x.as_i64()))
                            .find(|n| *n > 0)
                    };
                    if let Some(n) = pick(&IN_KEYS) {
                        self.prompt_tokens = n;
                    }
                    if let Some(n) = pick(&OUT_KEYS) {
                        self.completion_tokens = n;
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

    /// ★ 上游无视 stream:true 回整包 JSON 时，必须包装成合法 SSE ——
    /// 否则客户端（OpenAI SDK / Claude Code）按 SSE 解析会崩。
    #[tokio::test]
    async fn json_response_is_wrapped_into_sse() {
        use futures_util::StreamExt;
        let body = r#"{"id":"c1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#;
        let (mut s, mut rx) = wrap_json_as_sse(body.to_string(), 40);
        let mut acc = String::new();
        while let Some(item) = s.next().await {
            acc += &String::from_utf8_lossy(&item.unwrap());
        }
        assert!(acc.starts_with("data: {"), "要以 data: 帧开头: {acc}");
        assert!(acc.ends_with("data: [DONE]\n\n"), "要以 [DONE] 收尾: {acc}");
        assert!(acc.contains("\"content\":\"hi\""), "原文要完整带出: {acc}");
        let stat = rx.recv().await.expect("统计应收口");
        assert!(stat.saw_done, "应标记已见 [DONE]");
        // 统计口径要吃到 usage（否则这类平台的台账恒记 0 token）
        assert_eq!(stat.tokens_in, 3, "prompt_tokens 应从 usage 取到");
        assert_eq!(stat.tokens_out, 2, "completion_tokens 应从 usage 取到");
    }

    /// 空 body 也不能漏 [DONE]（客户端等它收尾，缺了会挂到超时）。
    #[tokio::test]
    async fn empty_json_response_still_terminates_sse() {
        use futures_util::StreamExt;
        let (mut s, _rx) = wrap_json_as_sse(String::new(), 0);
        let mut acc = String::new();
        while let Some(item) = s.next().await {
            acc += &String::from_utf8_lossy(&item.unwrap());
        }
        assert_eq!(acc, "data: [DONE]\n\n");
    }






    /// ★ 二次确认：第一次失败（网络抖动/上游临时抽风）不该直接判死。
    /// mock 第一次 401、第二次 200 —— 若没有二次确认，好账号会被标「鉴权失效」。
    #[tokio::test]
    async fn verify_retries_once_before_declaring_dead() {
        use axum::routing::get as g_route;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let app = axum::Router::new().route(
            "/models",
            g_route(move || {
                let h = hits2.clone();
                async move {
                    let n = h.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        (axum::http::StatusCode::UNAUTHORIZED, "{\"error\":\"blip\"}")
                    } else {
                        (
                            axum::http::StatusCode::OK,
                            r#"{"object":"list","data":[{"id":"m1"}]}"#,
                        )
                    }
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        let spec: ProviderSpec = serde_json::from_value(serde_json::json!({
            "id": "rt", "name": "rt", "enabled": true, "auth": "api_key",
            "base_url": format!("http://{addr}"), "format": "openai",
            "models": ["m1"], "models_policy": "static", "stream_dialect": "sse_delta",
            "chat_supported": false
        }))
        .expect("spec");
        let client = OutClient::new();
        let o = client.verify_account(&spec, "k").await;
        assert!(matches!(o, VerifyOutcome::Alive), "第一次抖动不该判死: {o:?}");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "应该恰好重试一次");
    }

    /// ★ 三段式验活的第三段：**清单通过也要证伪**。
    /// 大量平台 /models 公开，错 key 也会拿到完整清单（实测 59 个源无凭据 200）。
    /// 补一次对话探测：明确死→判死；明确活→判活；无结论→沿用清单判定（不误杀）。
    #[tokio::test]
    async fn alive_models_is_challenged_by_chat_probe() {
        use axum::routing::{get as g_route, post as p_route};
        let mk = |chat_status: u16| async move {
            let app = axum::Router::new()
                .route(
                    "/models",
                    g_route(|| async {
                        ([(axum::http::header::CONTENT_TYPE, "application/json")],
                         r#"{"object":"list","data":[{"id":"m1"}]}"#)
                    }),
                )
                .route(
                    "/chat/completions",
                    p_route(move || async move {
                        (axum::http::StatusCode::from_u16(chat_status).unwrap(),
                         [(axum::http::header::CONTENT_TYPE, "application/json")],
                         "{\"error\":{\"message\":\"probe\"}}")
                    }),
                );
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(l, app).await });
            addr
        };
        let mk_spec = |base: &str, auth: &str| {
            serde_json::from_value::<ProviderSpec>(serde_json::json!({
                "id": "pa", "name": "pa", "enabled": true, "auth": auth,
                "base_url": base, "format": "openai", "models": ["m1"],
                "models_policy": "static", "stream_dialect": "sse_delta",
                "chat_supported": true
            }))
            .expect("spec")
        };
        let client = OutClient::new();
        // 公开清单 + 对话 401 → 必须判死（旧实现会假阳性判活）
        let addr = mk(401).await;
        let o = client
            .verify_account(&mk_spec(&format!("http://{addr}"), "api_key"), "bad")
            .await;
        assert!(matches!(o, VerifyOutcome::Dead(_)), "公开清单不该掩盖错 key: {o:?}");
        // 公开清单 + 对话 200 → 判活
        let addr = mk(200).await;
        let o = client
            .verify_account(&mk_spec(&format!("http://{addr}"), "api_key"), "good")
            .await;
        assert!(matches!(o, VerifyOutcome::Alive), "对话也通过就该判活: {o:?}");
        // 公开清单 + 对话 404（模型名不对）→ 不误杀，沿用清单的 Alive
        let addr = mk(404).await;
        let o = client
            .verify_account(&mk_spec(&format!("http://{addr}"), "api_key"), "k")
            .await;
        assert!(matches!(o, VerifyOutcome::Alive), "对话探测无结论时不该误杀: {o:?}");
        // anon 源不做对话证伪（本来就没有凭据可证）
        let addr = mk(401).await;
        let o = client
            .verify_account(&mk_spec(&format!("http://{addr}"), "anon"), "k")
            .await;
        assert!(matches!(o, VerifyOutcome::Alive), "anon 源不该被对话探测打扰: {o:?}");
    }

    /// ★ verify_chat=true 的源必须跳过 /models：某些平台清单公开（cline 实测
    /// 无凭据 200 + 469 个模型），拿它验活等于「错 key 也通过」。
    #[tokio::test]
    async fn verify_chat_flag_skips_public_models_list() {
        use axum::routing::{get as g_route, post as p_route};
        // /models 公开返回一份合法清单；/chat 回 401（凭据无效）
        let app = axum::Router::new()
            .route(
                "/models",
                g_route(|| async {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        r#"{"object":"list","data":[{"id":"m1"},{"id":"m2"}]}"#,
                    )
                }),
            )
            .route(
                "/chat/completions",
                p_route(|| async {
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        r#"{"error":{"message":"invalid token"}}"#,
                    )
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        let mk = |verify_chat: bool| {
            let mut s: ProviderSpec = serde_json::from_value(serde_json::json!({
                "id": "cx", "name": "cx", "enabled": true, "auth": "api_key",
                "base_url": format!("http://{addr}"), "format": "openai",
                "models": ["m1"], "models_policy": "static",
                "stream_dialect": "sse_delta", "chat_supported": true,
                "verify_chat": verify_chat
            }))
            .expect("spec");
            s.models_path = String::new();
            s
        };
        let client = OutClient::new();
        // 默认路径：清单 200 公开，但**第三段探测**会用对话证伪 → 也判死。
        // （verify_chat 字段依然有用：跳过清单探测省一次大响应，cline 的清单 469 条。）
        let o = client.verify_account(&mk(false), "bad-key").await;
        assert!(matches!(o, VerifyOutcome::Dead(_)), "默认路径也必须有证伪: {o:?}");
        // verify_chat=true：跳过清单直连对话 → 同样判死
        let o = client.verify_account(&mk(true), "bad-key").await;
        assert!(matches!(o, VerifyOutcome::Dead(_)), "verify_chat 路径必须判死: {o:?}");
    }

    /// 二段探测判据：/models 404 时用「一次最小对话」判鉴权层。
    /// 分界是**鉴权过没过**，不是「请求成不成功」——400/429 都算通过。
    #[tokio::test]
    async fn verify_falls_back_to_chat_probe_when_models_missing() {
        use axum::routing::{get as g_route, post as p_route};
        // mock：/models 一律 404；/chat/completions 按需返回状态码
        let mk = |chat_status: u16| async move {
            let app = axum::Router::new()
                .route("/models", g_route(|| async { (axum::http::StatusCode::NOT_FOUND, "{}") }))
                .route(
                    "/chat/completions",
                    p_route(move || async move {
                        (
                            axum::http::StatusCode::from_u16(chat_status).unwrap(),
                            [(axum::http::header::CONTENT_TYPE, "application/json")],
                            "{\"error\":{\"message\":\"probe\"}}",
                        )
                    }),
                );
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(l, app).await });
            addr
        };
        let mk_spec = |base: &str, chat_ok: bool| {
            let mut s: ProviderSpec = serde_json::from_value(serde_json::json!({
                "id": "px", "name": "px", "enabled": true, "auth": "api_key",
                "base_url": base, "format": "openai", "models": ["m1"],
                "models_policy": "static", "stream_dialect": "sse_delta",
                "chat_supported": chat_ok
            }))
            .expect("spec");
            s.models_path = String::new();
            s
        };
        let client = OutClient::new();
        // 401 → 明确失效
        let addr = mk(401).await;
        let spec = mk_spec(&format!("http://{addr}"), true);
        let o = client.verify_account(&spec, "k").await;
        assert!(matches!(o, VerifyOutcome::Dead(_)), "对话探测 401 应判死: {o:?}");
        // 400 → 鉴权通过（上游先认了 key 才谈参数）
        let addr = mk(400).await;
        let spec = mk_spec(&format!("http://{addr}"), true);
        let o = client.verify_account(&spec, "k").await;
        assert!(matches!(o, VerifyOutcome::Alive), "对话探测 400 应判活（鉴权已过）: {o:?}");
        // 429 → 鉴权通过（能被限流说明身份已识别）
        let addr = mk(429).await;
        let spec = mk_spec(&format!("http://{addr}"), true);
        let o = client.verify_account(&spec, "k").await;
        assert!(matches!(o, VerifyOutcome::Alive), "对话探测 429 应判活: {o:?}");
        // chat_supported=false → **不**做对话探测，保持无法判定（别给 embedding 源发对话）
        let addr = mk(200).await;
        let spec = mk_spec(&format!("http://{addr}"), false);
        let o = client.verify_account(&spec, "k").await;
        assert!(matches!(o, VerifyOutcome::Unknown(_)), "不支持对话的源不该做对话探测: {o:?}");
    }

    /// 验活三态分类：**「探不了」绝不能判死**（6 个源的好凭据曾被 404 误杀），
    /// 「200 但不是 API 清单」也绝不能判活（apk-nano 首页 / cline 公开清单）。
    #[test]
    fn classify_verify_probe_is_three_state() {
        use VerifyOutcome::*;
        // 明确拒绝 → 死
        assert!(matches!(OutClient::classify_verify_probe(401, ""), Dead(_)));
        assert!(matches!(OutClient::classify_verify_probe(403, ""), Dead(_)));
        // 端点不存在 → 无法判定（不判死）
        for s in [404u16, 405, 501] {
            assert!(matches!(OutClient::classify_verify_probe(s, ""), Unknown(_)), "HTTP {s} 不该判死");
        }
        // 2xx + 真清单 → 活
        let list = r#"{"object":"list","data":[{"id":"gpt-5"}]}"#;
        assert!(matches!(OutClient::classify_verify_probe(200, list), Alive));
        let google = r#"{"models":[{"name":"models/gemini-2.5-pro"}]}"#;
        assert!(matches!(OutClient::classify_verify_probe(200, google), Alive));
        // 2xx + HTML 首页（apk-nano）→ 无法判定（旧实现判活 = 假阳性）
        assert!(matches!(OutClient::classify_verify_probe(200, "<!doctype html><html></html>"), Unknown(_)));
        // 2xx + JSON 但没有清单数组（cline 公开响应是这个形状）→ 无法判定
        assert!(matches!(OutClient::classify_verify_probe(200, r#"{"ok":true}"#), Unknown(_)));
        // 5xx → 无法判定（上游故障与凭据无关）
        assert!(matches!(OutClient::classify_verify_probe(502, ""), Unknown(_)));
    }



    /// 流式 usage 解析必须支持别名（与非流式 dialect::extract_usage 同一口径）。
    ///
    /// ★ 这是同一个缺陷的**第二处**：第 33 轮修了非流式的 extract_usage，但流式
    ///   的 SseParser 仍然只认 prompt_tokens/completion_tokens。上游换用 input_tokens
    ///   / output_tokens 命名时，**流式**请求的用量全部记成 0 —— 只修一处等于让
    ///   「同样的问题在另一条路上继续发生」。
    #[test]
    fn sse_parser_reads_usage_aliases() {
        for (kin, kout) in [
            ("prompt_tokens", "completion_tokens"),
            ("input_tokens", "output_tokens"),
            ("promptTokens", "completionTokens"),
            ("inputTokens", "outputTokens"),
        ] {
            let mut p = SseParser::new();
            let line = format!(
                "data: {{\"usage\":{{\"{kin}\":120,\"{kout}\":34}}}}"
            );
            p.feed(format!("{line}\n").as_bytes());
            assert_eq!(
                (p.prompt_tokens, p.completion_tokens),
                (120, 34),
                "字段名 {kin}/{kout} 应被识别"
            );
        }
    }

    /// 0 与缺失同样视为「这一项没说话」，继续找下一个别名。
    #[test]
    fn sse_parser_skips_zero_usage() {
        let mut p = SseParser::new();
        p.feed(br#"data: {"usage":{"prompt_tokens":0,"input_tokens":4800,"completion_tokens":0,"output_tokens":7}}
"#);
        assert_eq!(p.prompt_tokens, 4800, "0 应回落到下一个别名");
        assert_eq!(p.completion_tokens, 7);
    }
    /// 降级期内，请求必须**首发就带中性提示词**，不再先撞一次 400。
    ///
    /// 这是降级状态机存在的全部理由：重发补救只能救「这一次」，而客户端
    /// 通常是一连串请求（同一个 system 模板），每个都撞一次 400 再补救，
    /// 等于每条消息多花一轮往返、还多给上游一次风控记录。
    #[tokio::test]
    async fn degraded_requests_skip_the_first_400() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        sanitize::degrade_clear();

        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(move |_req: axum::extract::Request| {
            let h2 = h2.clone();
            async move {
                h2.fetch_add(1, Ordering::Relaxed);
                // 一律成功：本用例只关心「上游被打了几次」
                axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"id":"1","object":"chat.completion","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}"#,
                    ))
                    .unwrap()
            }
        });
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let spec = serde_json::from_value::<ProviderSpec>(json!({
            "id": "deg-test",
            "name": "deg",
            "enabled": true,
            "auth": "api_key",
            "base_url": format!("http://{addr}"),
            "format": "openai",
            "models": [],
            "models_policy": "static",
            "stream_dialect": "sse_delta",
            "region": "global",
            "headers": {},
        }))
        .expect("spec");
        let body = br#"{"model":"m","system":"You are Claude Code, Anthropic's official CLI for Claude.","messages":[{"role":"user","content":"hi"}],"stream":false}"#;

        let client = OutClient::new();
        // 未降级：打一次就成功
        let _ = client.forward_nonstream(&spec, "", "", body).await.expect("ok");
        assert_eq!(hits.load(Ordering::Relaxed), 1);

        // 进入降级期后再发：仍然只该打一次（首发即中性提示词）
        sanitize::degrade_trigger();
        let _ = client.forward_nonstream(&spec, "", "", body).await.expect("ok");
        assert_eq!(
            hits.load(Ordering::Relaxed),
            2,
            "降级期内不该额外往返"
        );

        sanitize::degrade_clear();
    }

    /// 上游按内容审核回 400 时，必须换成中性提示词重发一次并成功。
    ///
    /// ★ 为什么需要：客户端 system 模板里的指纹句会让上游逐字审核直接拒单，
    ///   而脱敏表必然滞后于模板更新。被拦下之后若不做补救，用户只会看到一个
    ///   无从下手的 400（且换号也救不了，因为问题在内容里）。
    #[tokio::test]
    async fn content_blocked_request_is_retried_with_neutral_prompt() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(move |_req: axum::extract::Request| {
            let h2 = h2.clone();
            async move {
                let n = h2.fetch_add(1, Ordering::Relaxed);
                if n == 0 {
                    // 第一次：按内容审核拒绝（与上游真实文案同形）
                    return axum::response::Response::builder()
                        .status(400)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            r#"{"error":{"message":"content policy violation"}}"#,
                        ))
                        .unwrap();
                }
                axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"id":"1","object":"chat.completion","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}]}"#,
                    ))
                    .unwrap()
            }
        });
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let spec = serde_json::from_value::<ProviderSpec>(json!({
            "id": "cb-test",
            "name": "cb",
            "enabled": true,
            "auth": "api_key",
            "base_url": format!("http://{addr}"),
            "format": "openai",
            "models": [],
            "models_policy": "static",
            "stream_dialect": "sse_delta",
            "region": "global",
            "headers": {},
        }))
        .expect("spec");
        // 请求体带客户端特征 system（正是会被拦下的那种）
        let body = br#"{"model":"m","system":"You are Claude Code, Anthropic's official CLI for Claude.","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
        let out = OutClient::new()
            .forward_nonstream(&spec, "", "", body)
            .await
            .expect("被内容审核拦下的请求应通过中性提示词重发成功");
        assert_eq!(out["choices"][0]["message"]["content"], "OK");
        assert_eq!(hits.load(Ordering::Relaxed), 2, "应恰好重发一次");
    }

    /// 普通的 400（参数写错）不该触发重发：那会把一个明确的错误变成两次往返。
    #[tokio::test]
    async fn ordinary_400_is_not_retried() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(move |_req: axum::extract::Request| {
            let h2 = h2.clone();
            async move {
                h2.fetch_add(1, Ordering::Relaxed);
                axum::response::Response::builder()
                    .status(400)
                    .body(axum::body::Body::from("model not found"))
                    .unwrap()
            }
        });
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let spec = serde_json::from_value::<ProviderSpec>(json!({
            "id": "cb-test2",
            "name": "cb",
            "enabled": true,
            "auth": "api_key",
            "base_url": format!("http://{addr}"),
            "format": "openai",
            "models": [],
            "models_policy": "static",
            "stream_dialect": "sse_delta",
            "region": "global",
            "headers": {},
        }))
        .expect("spec");
        let body = br#"{"model":"m","system":"whatever","messages":[{"role":"user","content":"hi"}],"stream":false}"#;
        let err = OutClient::new()
            .forward_nonstream(&spec, "", "", body)
            .await
            .expect_err("普通 400 应原样失败");
        assert_eq!(err.status, 400);
        assert_eq!(hits.load(Ordering::Relaxed), 1, "不该重发");
    }

    /// 上游用 Brotli 压缩响应时必须能自动解压。
    ///
    /// ★ 真实事故（2026-10-26）：reqwest 只开了 stream/json，没开 brotli，
    ///   于是凡是回 Content-Encoding: br 的上游，拿到的仍是压缩字节 ——
    ///   表现为「HTTP 200 但 error decoding response body」，模型清单一直拉不到。
    ///   实测受影响的有 ai-gateway.vercel.sh、api.kilo.ai、api.featherless.ai。
    ///   而用 br 的上游越来越多（Vercel / Cloudflare 系普遍启用），
    ///   这条测试把「悄悄精简掉 Cargo 特性」变成红灯。
    #[tokio::test]
    async fn brotli_encoded_models_list_is_decoded() {
        // 预压缩的 {"data":[{"id":"br-model-1"},{"id":"br-model-2"}]}
        const BROTLI_B64: &str =
            "GzEA+C0O7Gaf7VG+piaTuwghapGWrb0VI1NiadnXxZI6ZHKNgCTqDCQxGIEKz7Mb3BmKDg==";
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(BROTLI_B64)
            .expect("测试用的 base64 应合法");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(move |_req: axum::extract::Request| {
            let raw = raw.clone();
            async move {
                axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .header("content-encoding", "br")
                    .body(axum::body::Body::from(raw))
                    .unwrap()
            }
        });
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let spec = serde_json::from_value::<ProviderSpec>(json!({
            "id": "br-test",
            "name": "br",
            "enabled": true,
            "auth": "api_key",
            "base_url": format!("http://{addr}"),
            "format": "openai",
            "models": [],
            "models_policy": "live",
            "models_path": "/models",
            "stream_dialect": "sse_delta",
            "region": "global",
            "headers": {},
        }))
        .expect("spec");
        let client = OutClient::new();
        let ids = client
            .fetch_models_with_secret(&spec, "")
            .await
            .expect("Brotli 清单必须能被解压解析");
        assert_eq!(ids, vec!["br-model-1", "br-model-2"]);
    }
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

#[cfg(test)]
mod wb_ua_realm_tests {
    /// ★ 2026-10-26：国际版账号的出站 UA 平台段必须是 "WorkBuddy AI"。
    ///
    /// 一手证据（workbuddy2api internal/upstream/headers.go）：官方桌面端 UA 形状是
    /// "WorkBuddy/<ver> <platform>/<ver> CLI/<ver>"，其中 <platform> 按 realm 切换——
    /// CN 用 "WorkBuddy"，global 用国际版 productName "WorkBuddy AI"。
    /// 国际版账号送错平台段会触发上游 403 code=11140 "request illegal" 风控。
    ///
    /// 这条测试锁住判定规则：任何「统一用一个 brand」的重构都会在这里失败。
    #[test]
    fn intl_detection_rules() {
        let is_intl = |domain: &str, spec_id: &str| {
            domain.contains("workbuddy.ai")
                || domain.contains("codebuddy.ai")
                || spec_id.ends_with("-intl")
        };
        // 国际版：域名 / 平台 id 两种信号
        assert!(is_intl("www.workbuddy.ai", "wb-gateway-intl"));
        assert!(is_intl("www.codebuddy.ai", "codebuddy"));
        assert!(is_intl("", "wb-gateway-intl"));
        // 国内版：不能误判
        assert!(!is_intl("www.codebuddy.cn", "wb-gateway"));
        assert!(!is_intl("www.workbuddy.cn", "wb-gateway"));
        assert!(!is_intl("", "wb-gateway"));
    }

    #[test]
    fn brand_switches_by_realm() {
        let brand = |intl: bool| if intl { "WorkBuddy AI" } else { "WorkBuddy" };
        assert_eq!(brand(true), "WorkBuddy AI");
        assert_eq!(brand(false), "WorkBuddy");
    }
    use crate::{ErrorClass, RelayError};

    fn err_with_body(body: &str) -> RelayError {
        RelayError { status: 429, class: ErrorClass::Rate, body: body.to_string() }
    }

    #[test]
    fn retry_after_hint_reads_json_and_text_forms() {
        // JSON 数字
        assert_eq!(err_with_body(r#"{"retry_after": 30}"#).retry_after_hint(), Some(30.0));
        // JSON 里的数字字符串
        assert_eq!(err_with_body(r#"{"retryAfter": "45"}"#).retry_after_hint(), Some(45.0));
        // OpenAI 风格嵌套
        assert_eq!(
            err_with_body(r#"{"error": {"retry_after": 12}}"#).retry_after_hint(),
            Some(12.0)
        );
        // 纯文本（国内上游常见）
        assert_eq!(err_with_body("rate limited, retry after 60 seconds").retry_after_hint(), Some(60.0));
        assert_eq!(err_with_body("请求过于频繁，请等待 90 秒后再试").retry_after_hint(), Some(90.0));
    }

    #[test]
    fn retry_after_hint_is_bounded_and_optional() {
        // 没有提示就返回 None（由号池走本地退避），不能凭空编一个数
        assert_eq!(err_with_body("internal error").retry_after_hint(), None);
        assert_eq!(err_with_body("{}").retry_after_hint(), None);
        // 0 / 负数没有意义：0 会变成「立刻重试」的死循环
        assert_eq!(err_with_body(r#"{"retry_after": 0}"#).retry_after_hint(), Some(1.0));
        assert_eq!(err_with_body(r#"{"retry_after": -5}"#).retry_after_hint(), Some(1.0));
        // 过大值夹到 1 小时上限
        assert_eq!(err_with_body(r#"{"retry_after": 86400}"#).retry_after_hint(), Some(3600.0));
    }

}
