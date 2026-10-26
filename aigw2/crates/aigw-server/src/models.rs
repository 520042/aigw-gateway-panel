//! 模型清单相关的纯逻辑：自定义 API / 公益站 → `ProviderSpec` 的映射，
//! 以及每平台 live 清单的缓存形状与 TTL 策略。
//!
//! 这些是 `state.rs`（`AppState::all_specs`）与 `lib.rs`（`/v1/models` 装配）
//! 共同依赖的一层，单独成文件避免两个模块互相引用。

use aigw_registry::{AuthKind, ProviderSpec};

use crate::net::scheme_ok;
use std::time::{Duration, Instant};

/// 自定义 API → 声明式 ProviderSpec（auth=api_key；Anthropic 协议走 anthropic 透传）。
pub(crate) fn custom_to_spec(c: &aigw_store::CustomApi) -> ProviderSpec {
    ProviderSpec {
        id: c.id.clone(),
        name: c.name.clone(),
        // Key 解不开（换机器/换用户）时不能算「可用」：以前这种条目会以「已接入」出现在
        // /v1/models 里，真发请求才 401，用户完全看不出是凭据的问题。
        enabled: c.enabled && !c.key_broken,
        auth: AuthKind::ApiKey,
        login_methods: Vec::new(),
        base_url: c.base_url.clone(),
        format: c.format.clone(),
        models: c.models.clone(),
        // 固定过清单的条目按 Static 发布：assemble_models 只对 Live 条目用 live
        // 缓存覆盖静态清单，模型预热/整表拉取也只挑 Live —— 固定后不再打上游拉清单，
        // 上游那份「过度承诺」的列表不会再冲掉用户收敛好的白名单。
        models_policy: if c.models_pinned && !c.models.is_empty() {
            aigw_registry::ModelsPolicy::Static
        } else {
            aigw_registry::ModelsPolicy::Live
        },
        stream_dialect: match c.format.as_str() {
            "anthropic" => aigw_registry::StreamDialect::AnthropicSse,
            _ => aigw_registry::StreamDialect::SseDelta,
        },
        // ★ 2026-10-26（用户排队消息①）：anthropic 自定义 API 的对话端点是
        //   /v1/messages（Anthropic 官方路径），不能沿用 openai 的 /chat/completions。
        chat_path: if c.format == "anthropic" { "/v1/messages".into() } else { String::new() },
        headers: Default::default(),
        models_path: "/models".into(),
        models_filter: String::new(),
        upstream_kind: "openai".into(),
        signer: String::new(),
        biz_err: Vec::new(),
        hosts: Vec::new(),
        login_url: String::new(),
        notes: if c.key_broken {
            "API Key 在这台机器上解不开（凭据是在别的 Windows 用户/机器上加密的），重新填一次 Key 即可恢复".into()
        } else {
            String::new()
        },
        chat_supported: true,
        region: "global".into(),
    }
}

/// 公益站 → 声明式 ProviderSpec（auth=api_key；openai 兼容 /v1）。
pub(crate) fn free_to_spec(s: &aigw_store::FreeSite) -> ProviderSpec {
    let mut base = s.base_url.trim_end_matches('/').to_string();
    // 聚合站给的地址有两种写法：`https://x.dev` 和 `https://x.dev/v1`。
    // 无条件拼 /v1 会让后者变成 /v1/v1（上游 404），所以先削掉已有的版本段。
    // 注意顺序：先长后短，否则 "/api/v1" 会先被 "/v1" 命中，削成 "/api" 再拼回 "/api/v1"。
    for suf in ["/api/v1", "/v1beta", "/v1"] {
        if base.ends_with(suf) {
            base.truncate(base.len() - suf.len());
            break;
        }
    }
    let base = base.trim_end_matches('/').to_string();
    ProviderSpec {
        id: s.id.clone(),
        name: s.name.clone(),
        enabled: s.enabled,
        login_methods: Vec::new(),
        auth: AuthKind::ApiKey,
        base_url: format!("{base}/v1"),
        format: "openai".into(),
        models: Vec::new(),
        models_policy: aigw_registry::ModelsPolicy::Live,
        stream_dialect: aigw_registry::StreamDialect::SseDelta,
        headers: Default::default(),
        chat_path: String::new(),
        models_path: String::new(),
        models_filter: String::new(),
        upstream_kind: "openai".into(),
        signer: String::new(),
        biz_err: Vec::new(),
        hosts: Vec::new(),
        // login_url 会直接进前端 <a href>：只下发 http(s)。旧版入库时没查 scheme，
        // free_sites.json 里可能还躺着历史毒条目，这里兜底不下发。
        login_url: if scheme_ok(&s.url) { s.url.clone() } else { String::new() },
        notes: s.desc.clone(),
        chat_supported: true,
        region: "global".into(),
    }
}

/// 单平台 live 模型缓存项：(拉取时刻, 模型清单, 这次拉取是否成功)。
/// 失败的平台也缓存（短 TTL），避免每次请求都去撞不可达的公益站。
/// ★ 第三项 ok 是 stale-on-error 的关键：拉取失败时**保留上一次的成功清单**继续
///   发布/路由，只把 ok 标成 false 让 TTL 走 120s 短档以便较快重试。旧实现失败
///   即塞空清单——慢公益站一次 12s 超时或一轮 429 限流，就把这个平台的全部模型
///   从 /v1/models 和 spec_for_model 的 live 兜底里清空最长 120 秒，chat 直接
///   503「未知模型」，而平台本身明明能对话。
pub(crate) type ModelsEntry = (Instant, Vec<String>, bool);

/// 单平台清单的缓存时长：失败（ok=false）或空清单走短档，让上游恢复后能较快
/// 重新探测；成功且非空走长档。失败但带着旧清单的条目在短档窗口内照常可用。
pub(crate) fn models_ttl(ids: &[String], ok: bool) -> Duration {
    if ids.is_empty() || !ok {
        MODELS_TTL_ERR
    } else {
        MODELS_TTL_OK
    }
}

/// live 拉取并发上限。74 个启用平台里 56 个是 live 策略，串行跑最坏要 20 分钟；
/// 并发 12 路可把冷启动压到十几秒，同时不至于把本机网络/代理打满。
pub(crate) const MODELS_FETCH_CONCURRENCY: usize = 12;
/// 成功结果的单平台缓存 TTL。
pub(crate) const MODELS_TTL_OK: Duration = Duration::from_secs(600);
/// 失败/空结果的单平台缓存 TTL（短一些，让上游恢复后能较快重新探测）。
pub(crate) const MODELS_TTL_ERR: Duration = Duration::from_secs(120);
/// 整表缓存 TTL。
pub(crate) const MODELS_TTL_ALL: Duration = Duration::from_secs(60);

/// 公益站刷新最多等模型预热多久（`warm_done` 信号的上限兜底）。
///
/// 预热实测 3–12 秒；卡在死上游时最坏 80 个平台 × 12s 超时 ÷ 12 并发 ≈ 80 秒。
/// 等满 60 秒仍没结束就照常起跑：公益站刷新比「避免两批并发」更重要。
pub const WARM_WAIT_CAP: Duration = Duration::from_secs(60);
