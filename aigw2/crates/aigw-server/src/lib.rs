//! aigw-server：axum 路由 + rust-embed 内嵌前端 + 中继接线。
//!
//! M1 契约面：
//! - `POST /v1/chat/completions`：真实转发（免密车道，OpenAI 同协议透传）；
//!   pool 选号 → relay 转发 → usage 记账 → SSE 流式/非流式响应。
//! - `GET /v1/models`：静态清单 + live 拉取（60s 缓存，失败回退静态）。
//! - `GET /api/usage`：用量汇总（store）。
//! - `GET /api/health`、`GET /*`（rust-embed 静态前端）。
//!
//! 路由策略 M1 为单平台（唯一启用 spec 接管全部模型）；M2 引入
//! 模型→平台映射与 fallback 链（dsh combo 降级）。

use aigw_core::ErrorClass;
use aigw_pool::{Lease, Pool};
use aigw_relay::auth_login::LoginEngine;
use aigw_relay::{OutClient, StreamStat};
use aigw_registry::{AuthKind, ProviderSpec};
use aigw_store::{AccountStore, CustomStore, FlagStore, FreeStore, Store, UsageEvent};
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Extension, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 前端静态资源。`$CARGO_MANIFEST_DIR/../../web` = workspace 根的 `web/`。
/// debug 构建默认动态读盘（改文件刷新即生效），release 构建内嵌进二进制。
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web"]
struct Web;

#[derive(Clone)]
pub struct AppState {
    pub specs: Vec<ProviderSpec>,
    pub store: Arc<Store>,
    pub pool: Arc<Pool>,
    pub client: Arc<OutClient>,
    pub accounts: Arc<AccountStore>,
    /// 自定义 API 存储（接入源页「自定义 API」动态并入 specs）。
    pub custom: Arc<CustomStore>,
    /// 公益站聚合存储（接入源页「公益站」，手动/每日刷新）。
    pub free: Arc<FreeStore>,
    /// 登录引擎（批2：设备授权/扫码 → token 自动落池）
    pub login: Arc<LoginEngine>,
    /// 面板开关（逐模型启用/停用）
    pub flags: Arc<FlagStore>,
    /// 访问密钥表（多把 key + 每把的模型白名单）。
    ///
    /// 与上面的面板令牌是两件事：面板令牌是「能不能进网关」的一把通票，
    /// 这里是「这把 key 能用哪些模型」，可以有多把、各带白名单。
    /// 表为空时（默认）不做任何模型限制，行为与旧版完全一致。
    pub keys: Arc<aigw_store::KeyStore>,
    /// 模型链（combo）：一个模型名 = 一条有序候选链，跨平台/跨模型回退。
    ///
    /// 与 failover_worthy 的同平台换账号是两个层次：那个在一次转发内部换凭据，
    /// 这里是**换模型/换平台**，所以要放在 chat_core 解析模型之后、resolve_spec 之前。
    /// 表为空时（默认）combo: 前缀不生效，行为与旧版一致。
    pub combos: Arc<aigw_store::ComboStore>,
    /// 会话粘性路由（对齐 workbuddy2api StickyRouter + loomy「最后一条 user」修正）
    pub sticky: Arc<aigw_pool::StickyRouter>,
    /// live 模型缓存：(时间, (spec_id, model) 索引, 响应体)
    pub models_cache: Arc<Mutex<Option<(Instant, Vec<(String, String)>, Value)>>>,
    /// 单平台 live 模型缓存：spec_id → (拉取时刻, 模型清单)。失败也缓存（空清单）。
    pub models_cache_per: Arc<Mutex<std::collections::HashMap<String, ModelsEntry>>>,
    /// 整表刷新单飞锁：并发请求只让一个去拉上游，其余等结果。
    pub models_lock: Arc<tokio::sync::Mutex<()>>,
    /// 面板访问令牌：`Some` 时 /api/* 必须携带（X-AIGW-Token / Bearer / Cookie）。
    /// `None` = 本机免令牌（默认；仅回环监听时允许）。
    ///
    /// ★ 用 `Mutex` 而不是只读的 `Arc<Option<_>>`：面板里要能轮换令牌
    /// （`/api/token`）。只读字段意味着「换一次令牌就重启一次服务」，而对外监听
    /// 自动生成的令牌只在启动日志里出现过一次 —— 泄漏后没法在运行中换掉。
    pub token: Arc<Mutex<Option<String>>>,
    /// 令牌来源（可变：面板里轮换后变成 "panel"）。
    pub token_source: Arc<Mutex<&'static str>>,
    /// Host / Origin 白名单（DNS 重绑定与跨站 CSRF 防线）。
    pub allow_hosts: Arc<Vec<String>>,
    /// 监听通配地址（--host 0.0.0.0 / ::）时置 true：此时任何 **IP 字面量** Host
    /// 都放行（局域网里用 192.168.x.x 访问面板是正常用法），但**域名一律拒绝**
    /// —— DNS 重绑定攻击的载荷必然是域名（evil.com → 127.0.0.1）。
    pub allow_any_ip_host: Arc<bool>,
    /// `/v1/*` 速率闸（main 按启动参数注入；测试默认不限流）
    pub limits: Arc<RateLimiter>,
    /// 后台任务健康台账（`/api/health` 的 `tasks` 字段）
    pub tasks: Arc<TaskHealth>,
    /// 加载时被跳过的非法 spec：(文件名, 原因)。
    ///
    /// 为什么要在 /api/health 里暴露：load_dir 是 lenient 的（坏文件只 warn 后
    /// 跳过），所以一个漏写必填字段的 toml 会让整个平台**静默消失** —— 服务照常
    /// 启动、面板只是少一张卡片。真实事故：anon-zen.toml 少了 models_policy，
    /// 该平台从 80 个掉到 79 个且没人发现，直到某个模型 404。日志会随重启滚掉，
    /// 健康检查是唯一长期可见的地方。
    pub skipped_specs: Arc<Vec<(String, String)>>,
    /// 启动预热（`warm_models`）是否已结束。
    ///
    /// 公益站刷新用它决定何时起跑。旧实现是 main 里固定 `sleep(10s)`：预热只用 3 秒时
    /// 白等 7 秒，预热要 12 秒时又照样和它撞车（两批出站连接仍会重叠）。改成「预热结束
    /// 就起跑，最多等 `WARM_WAIT_CAP`」，快机器上更早、慢机器上不再重叠。
    pub warm_done: Arc<tokio::sync::watch::Sender<bool>>,
    /// 每个接入源最近一次失败的原因（面板卡片用；成功一次即清除）。
    pub last_err: Arc<LastErrors>,
    /// 进程启动时刻（`AppState::new` 记录）。
    ///
    /// ★ 排查时最先要回答的两个问题：「这个服务跑了多久」（是不是刚重启过、日志里那次
    ///   故障已经不在当前进程里了）、「我部署的新二进制真的生效了吗」。只看 `/api/health`
    ///   的 version 回答不了后者 —— 版本号一整天都不变，而 exe 的 mtime 只有登到那台机器
    ///   上才看得见。所以这里连 `started_at` 一起报。
    pub started_at: Arc<Instant>,
    /// 数据目录（main 注入）：`/api/token` 用它把轮换后的令牌落盘。
    ///
    /// ★ 不现场调 `default_data_dir()` 解析环境变量：那样测试进程会往真实的
    ///   %LOCALAPPDATA%\aigw2\data 写文件（历史事故：`cargo test -p aigw-server`
    ///   把线上 proxy.json 改成了外部代理），而且「服务用哪个目录」应当只由启动时
    ///   那一个值决定，运行中环境变量变了也不该换地方。
    pub data_dir: Arc<Option<std::path::PathBuf>>,
}

/// 回环主机名白名单（Host / Origin 校验用）。
fn loopback_hosts() -> Vec<String> {
    vec![
        "127.0.0.1".to_string(),
        "localhost".to_string(),
        "::1".to_string(),
        "[::1]".to_string(),
    ]
}

/// 模型名归一化：把「同一模型的多种写法」折叠成同一个 key。
///
/// 现实里同一个模型有至少四种写法，客户端（Claude Code / Codex / 各种 SDK）
/// 偏偏都爱用带日期的官方全名：
///   - `claude-sonnet-4-5-20250929`（官方带日期）
///   - `claude-sonnet-4-5`（去日期）
///   - `claude-sonnet-4.5`（上游点号版本，sub-kiro 声明的就是这个）
///   - `gpt-4o-2024-08-06` / `gpt-4o`
///
/// 旧实现只做**字符串相等**比较，于是客户端写官方全名时一律 404
/// model_not_found —— 而同一个模型明明就在清单里。这里只做「同一模型的
/// 别名折叠」，绝不做模糊/子串匹配（`gpt-4` 不会命中 `gpt-4o`）。
fn norm_model_id(raw: &str) -> String {
    let mut s = raw.trim().to_ascii_lowercase();
    // ① 去掉 -latest 尾巴（OpenRouter / 部分中转站的惯用写法）
    if let Some(rest) = s.strip_suffix("-latest") {
        s = rest.to_string();
    }
    // ② 去掉日期尾巴：-20250929 与 -2024-08-06 两种形态
    let parts: Vec<&str> = s.split('-').collect();
    let is_num = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    let mut cut = parts.len();
    if parts.len() >= 2 && is_num(parts[parts.len() - 1]) && parts[parts.len() - 1].len() == 8 {
        cut -= 1;
    } else if parts.len() >= 4
        && is_num(parts[parts.len() - 1])
        && parts[parts.len() - 1].len() == 2
        && is_num(parts[parts.len() - 2])
        && parts[parts.len() - 2].len() == 2
        && is_num(parts[parts.len() - 3])
        && parts[parts.len() - 3].len() == 4
    {
        cut -= 3;
    }
    let s = parts[..cut].join("-");
    // ③ 点号版本 → 连字符版本（claude-sonnet-4.5 → claude-sonnet-4-5）
    s.replace('.', "-")
}

/// 两个模型名是否指同一个模型。
fn same_model(a: &str, b: &str) -> bool {
    a == b || norm_model_id(a) == norm_model_id(b)
}

impl AppState {
    /// 组装运行态（main 用）。
    pub fn new(
        specs: Vec<ProviderSpec>,
        store: Arc<Store>,
        pool: Arc<Pool>,
        client: Arc<OutClient>,
        accounts: Arc<AccountStore>,
        custom: Arc<CustomStore>,
        free: Arc<FreeStore>,
    ) -> AppState {
        AppState {
            specs,
            store,
            pool,
            client,
            accounts,
            custom,
            free,
            login: Arc::new(LoginEngine::new()),
            flags: Arc::new(FlagStore::empty()),
            // 默认空表 = 不做模型限制（main 会换成 KeyStore::open(data_dir)）
            keys: Arc::new(aigw_store::KeyStore::empty()),
            combos: Arc::new(aigw_store::ComboStore::empty()),
            sticky: Arc::new(aigw_pool::StickyRouter::new()),
            models_cache: Arc::new(Mutex::new(None)),
            models_cache_per: Arc::new(Mutex::new(std::collections::HashMap::new())),
            models_lock: Arc::new(tokio::sync::Mutex::new(())),
            token: Arc::new(Mutex::new(None)),
            allow_hosts: Arc::new(loopback_hosts()),
            allow_any_ip_host: Arc::new(false),
            token_source: Arc::new(Mutex::new("")),
            limits: Arc::new(RateLimiter::new(0, 0)),
            tasks: Arc::new(TaskHealth::new()),
            skipped_specs: Arc::new(Vec::new()),
            warm_done: Arc::new(tokio::sync::watch::channel(false).0),
            last_err: Arc::new(LastErrors::new()),
            started_at: Arc::new(Instant::now()),
            data_dir: Arc::new(None),
        }
    }

    /// 注入「进程启动时刻」（仅测试用）：让健康检查的 started_at / uptime_s 换算
    /// 能被真正验证。
    ///
    /// ★ 为什么必须可注入：`AppState::new` 到读取 /api/health 只隔几毫秒，
    ///   uptime_s 恒为 0，于是「started_at 直接写 now_ms()」和正确实现**无法区分**
    ///   —— 变异咬不住，测试形同虚设。假装 90 秒前启动，换算错了立刻现形。
    pub fn with_started_at(mut self, at: Instant) -> AppState {
        self.started_at = Arc::new(at);
        self
    }

    /// 注入访问密钥表（main 用）：从 data/keys.json 加载。
    pub fn with_keys(mut self, keys: aigw_store::KeyStore) -> AppState {
        self.keys = Arc::new(keys);
        self
    }

    /// 注入模型链表（main 用）：从 data/combos.json 加载。
    pub fn with_combos(mut self, combos: aigw_store::ComboStore) -> AppState {
        self.combos = Arc::new(combos);
        self
    }

    /// 注入数据目录（main 用）：`/api/token` 把轮换后的令牌写到这里。
    pub fn with_data_dir(mut self, dir: std::path::PathBuf) -> AppState {
        self.data_dir = Arc::new(Some(dir));
        self
    }

    /// 注入加载时被跳过的非法 spec（main 用；测试保持空）。
    pub fn with_skipped_specs(mut self, skipped: Vec<(String, String)>) -> AppState {
        self.skipped_specs = Arc::new(skipped);
        self
    }

    /// 注入持久化开关存储（main 用；测试保持 `FlagStore::empty()`）。
    pub fn with_flags(mut self, flags: Arc<FlagStore>) -> AppState {
        self.flags = flags;
        self
    }

    /// 安全策略注入（main 用）：`token=None` 表示本机免令牌；`hosts` 覆盖 Host/Origin
    /// 白名单（对外监听时把绑定 IP 也加进去）。
    pub fn with_security(mut self, token: Option<String>, hosts: Vec<String>) -> AppState {
        self.token = Arc::new(Mutex::new(token));
        self.allow_hosts = Arc::new(hosts);
        self
    }

    /// 记录令牌来源（main 用；测试可省略，省略即空串）。
    /// `/v1/*` 速率闸（main 用；`0` 表示该档关闭）。
    /// 访问密钥的每分钟上限（0 = 不按 key 限）。刻意与 with_limits 同源控制：
    /// 两者都是「启动时定死」的闸门，不该混进带数据目录的 AppState::new。
    pub fn with_key_rate(mut self, key_rpm: u32) -> AppState {
        let per_ip = self.limits.per_ip();
        let global = self.limits.global();
        let mut lim = RateLimiter::new(per_ip, global);
        lim.key_rate = key_rpm;
        self.limits = Arc::new(lim);
        self
    }

    pub fn with_limits(mut self, per_ip: u32, global: u32) -> AppState {
        self.limits = Arc::new(RateLimiter::new(per_ip, global));
        self
    }

    pub fn with_token_source(mut self, src: &'static str) -> AppState {
        self.token_source = Arc::new(Mutex::new(src));
        self
    }

    /// 订阅「预热已结束」信号（main 用来给公益站刷新定起跑时刻）。
    pub fn warm_done_rx(&self) -> tokio::sync::watch::Receiver<bool> {
        self.warm_done.subscribe()
    }

    /// 当前面板令牌（`None` = 本机免令牌）。
    ///
    /// ★ 每次都要重新读锁，不要把结果缓存进长生命周期的局部变量：`/api/token`
    /// 能在请求之间换掉令牌，缓存住的值会让「刚轮换完只在部分路径生效」。
    pub fn current_token(&self) -> Option<String> {
        self.token.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 当前令牌来源标签（"cli" / "env" / "auto" / "panel" / ""）。
    pub fn token_source_now(&self) -> &'static str {
        *self.token_source.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 轮换令牌：内存里的令牌与来源标签一起换（落盘由调用方负责）。
    pub fn set_panel_token(&self, t: Option<String>, src: &'static str) {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = t;
        *self.token_source.lock().unwrap_or_else(|e| e.into_inner()) = src;
    }

    /// 通配监听（0.0.0.0 / ::）时放开「任意 IP 字面量 Host」。
    ///
    /// 旧实现把 `--host 0.0.0.0` 原样塞进白名单，而浏览器发来的 Host 是真实
    /// 局域网 IP（192.168.1.5:8800）→ 一律 403，对外监听等于完全不可用，
    /// 自动生成的面板令牌也就成了摆设。
    pub fn with_wildcard_host(mut self, wildcard: bool) -> AppState {
        self.allow_any_ip_host = Arc::new(wildcard);
        self
    }

    /// 全量平台 = 文件 spec + 自定义 API + 公益站（动态并进）。
    /// 面板上停用的平台在这里统一置 `enabled=false`（文件 spec 不就地改写）。
    pub fn all_specs(&self) -> Vec<ProviderSpec> {
        let mut v = self.specs.clone();
        for c in self.custom.rows() {
            let mut sp = custom_to_spec(&c);
            // 自定义 API 的启停存在 custom.json 的 enabled 字段（不在 flags.json）。
            // 旧实现无视它：前端点「停用」写的是 flags.disabled_platforms，
            // 卡片依旧显示启用、模型也照旧出现在 /v1/models。
            if !c.enabled {
                sp.enabled = false;
            }
            v.push(sp);
        }
        for s in self.free.rows() {
            v.push(free_to_spec(&s));
        }
        for s in v.iter_mut() {
            if s.enabled && self.flags.is_platform_disabled(&s.id) {
                s.enabled = false;
            }
        }
        v
    }

    /// 平台可用凭据（`None` = 没有可用凭据，转发必然 401）。
    ///
    /// - `auth=anon`：免密车道，恒可用（secret 为空串）；
    /// - `upstream_kind=metaso`：允许匿名指纹，无账号时回退空串；
    /// - `custom-*`：Key 存在 CustomStore（不在账号池），必须从这里取，
    ///   否则自定义 API 的模型永远「无可用账号」（旧实现就是这样全废的）；
    /// - 其余：取账号池第一个启用账号的明文 secret。
    pub fn platform_secret(&self, spec: &ProviderSpec) -> Option<String> {
        if spec.auth == AuthKind::Anon {
            return Some(String::new());
        }
        if spec.upstream_kind == "metaso" {
            return Some(
                self.accounts
                    .secrets_for(&spec.id)
                    .into_iter()
                    .next()
                    .map(|(_, s)| s)
                    .unwrap_or_default(),
            );
        }
        if let Some(c) = self.custom.rows().into_iter().find(|c| c.id == spec.id) {
            // 密文解不开时 api_key 里还是 dpapi: 原文：绝不能当凭据发出去。
            if c.key_broken {
                return None;
            }
            let k = c.api_key.trim().to_string();
            return if k.is_empty() { None } else { Some(k) };
        }
        self.accounts
            .secrets_for(&spec.id)
            .into_iter()
            .next()
            .map(|(_, s)| s)
    }

    /// 该平台声明的协议是否真的被转发层支持。
    ///
    /// `format` 字段在 registry 里允许 openai|anthropic|responses，但 relay 只有
    /// OpenAI 透传（chat_path 恒为 /chat/completions，不做协议转换）。
    /// 一个 anthropic 自定义 API 会带着 /chat/completions 打过去 → 404/400，
    /// 用户看到的是「上游报错」而不是「本面板不支持该协议」。这里显式挡掉。
    /// ★ 2026-10-26（用户排队消息①）：放行 format=anthropic —— custom_to_spec
    ///   已把 anthropic 条目的 chat_path 设为 /v1/messages、流方言设为
    ///   AnthropicSse；/v1/chat/completions 端做 OpenAI→Anthropic 请求体转换，
    ///   /v1/messages 端原生透传（见 messages_completions 的 native_anthropic 分支）。
    pub fn platform_supported(&self, spec: &ProviderSpec) -> bool {
        spec.format == "openai" || spec.format == "anthropic"
    }

    /// 平台当前是否「真的能转发」：已启用 + 有可用凭据 + 上游地址已解析。
    ///
    /// 另外两个条件来自实测：
    /// - `chat_supported = false`（apk-wps / apk-nano 只做登录验活）：转发层必然 501，
    ///   但旧实现仍把它们算「可用」，卡片显示可用、点开就报错。
    /// - `base_url` 是占位符：`web-lobster` 的 `base_url = "@lobster"` 是占位符，
    /// 只有旧版 settings.json 里配了 `lobster_server` 才会被替换。没配时
    /// reqwest 直接报 `network: builder error`（连 URL 都构造不出来），
    /// 但平台仍「有账号」，于是模型进清单、点开就一个看不懂的 502。
    pub fn platform_usable(&self, spec: &ProviderSpec) -> bool {
        self.platform_routable(spec)
            && self.platform_supported(spec)
            && spec.chat_supported
            && !spec.base_url.starts_with('@')
    }

    /// 后缀 `xxx@平台` 里的「平台」是否真的是一个平台 id。
    ///
    /// 用于区分「路由后缀」和「上游模型名里本来就有的 @」（如 `qwen3@2025-09`）。
    /// 只认已加载的 spec id（含 custom-*/公益站），不做模糊匹配。
    pub fn is_platform_id(&self, candidate: &str) -> bool {
        if candidate.is_empty() {
            return false;
        }
        self.all_specs().iter().any(|s| s.id == candidate)
    }

    /// 路由候选：已启用 + 有凭据。
    ///
    /// 这里故意不检查协议与占位符：这两类平台也要能被模型名命中，好在 chat 里
    /// 回一条「协议不支持 501」「上游地址未配置 503」，而不是让用户对着
    /// 「未知模型 503」怀疑自己模型名写错了。
    /// （聚合清单 /v1/models 用的是更严格的 platform_usable。）
    pub fn platform_routable(&self, spec: &ProviderSpec) -> bool {
        spec.enabled && self.platform_secret(spec).is_some()
    }

    /// 按模型名选平台：① `模型id@平台` 后缀直指定（旧版语义）② 静态清单声明
    /// ③ live 缓存索引 ④ **唯一可用平台**时直接接管（单平台部署的便利），
    /// 多平台下不再兜底 —— 未知模型必须明确 503，否则「以为在用 A 模型、
    /// 实际打到第一个平台」会静默串号。
    ///
    /// ②③ 只认**可用**平台（有凭据）：同一个模型名常被多个平台声明
    /// （kilo 的 18 个 free 模型里有 15 个也被 api-openrouter 声明），
    /// 旧实现取「第一个 enabled」，会稳定选中没有 Key 的 openrouter，
    /// 用户拿到的是「平台 api-openrouter 无可用账号」。
    fn spec_for_model(&self, model: &str) -> Option<ProviderSpec> {
        // 面板上被停用的模型：连 `模型@平台` 显式指定也拒绝（与聚合清单保持一致）
        if self.flags.is_model_disabled(model) {
            return None;
        }
        let all = self.all_specs();
        let enabled: Vec<&ProviderSpec> = all.iter().filter(|s| s.enabled).collect();
        // 只认「凭据可达」的平台：没有 Key 的平台抢到模型名只会返回 401
        let usable: Vec<&ProviderSpec> = enabled
            .iter()
            .copied()
            .filter(|s| self.platform_routable(s))
            .collect();
        // 再上一层：真正能聊天的平台（凭据 + 协议被支持 + chat_supported + 非占位地址）。
        // 同名模型常被多个平台声明，其中 web-lobster 这类「有账号但 base_url 是占位符
        // @lobster」会抢到名字，用户拿到的是 503「上游地址未配置」——而同一模型在
        // 别的可用平台上本来能跑。优先从这里选，选不到才回落到 usable（错误信息更具体）。
        let preferred: Vec<&ProviderSpec> = usable
            .iter()
            .copied()
            .filter(|s| self.platform_usable(s))
            .collect();
        // 显式 模型@平台：按 enabled 认（错误信息比 503「未知模型」更准确）。
        // ★ 命中就返回；命中不到就**只拿裸名去做后面的 ②③④ 匹配**。后缀已由
        //   chat_core 认定为合法平台 id 并在发上游前剥掉，这里再拿带后缀的整串去比
        //   平台清单只会一路落空，把「平台 X 不认这个模型」降级成「未知模型 whole@X」，
        //   看上去像模型名写错。
        let mut model = model;
        if let Some((bare, plat)) = model.rsplit_once('@') {
            if self.is_platform_id(plat) {
                if let Some(s) = enabled.iter().find(|s| s.id == plat) {
                    return Some((*s).clone());
                }
                model = bare;
            }
        }
        if let Some(s) = preferred
            .iter()
            .find(|s| s.models.iter().any(|m| m == model))
        {
            return Some((*s).clone());
        }
        if let Some(s) = usable.iter().find(|s| s.models.iter().any(|m| m == model)) {
            return Some((*s).clone());
        }
        {
            let g = self.models_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, index, _)) = g.as_ref() {
                for (sid, m) in index.iter() {
                    if m != model {
                        continue;
                    }
                    if let Some(s) = preferred.iter().find(|s| &s.id == sid) {
                        return Some((*s).clone());
                    }
                }
                for (sid, m) in index.iter() {
                    if m != model {
                        continue;
                    }
                    if let Some(s) = usable.iter().find(|s| &s.id == sid) {
                        return Some((*s).clone());
                    }
                }
            }
        }
        // ③' 每平台 live 缓存兜底。
        //
        // 整表索引只在 /v1/models 被调用过之后才存在：冷启动直接发 chat 时
        // 索引是空的，live 平台（kilo / api-* 这类 models_policy=live）
        // 的模型全部落到「未知模型 503」——而它们明明在 providers 里声明得好好的。
        {
            let g = self.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
            for (sid, (_, ids, _)) in g.iter() {
                if !ids.iter().any(|m| m == model) {
                    continue;
                }
                if let Some(s) = preferred.iter().find(|s| &s.id == sid) {
                    return Some((*s).clone());
                }
            }
            for (sid, (_, ids, _)) in g.iter() {
                if !ids.iter().any(|m| m == model) {
                    continue;
                }
                if let Some(s) = usable.iter().find(|s| &s.id == sid) {
                    return Some((*s).clone());
                }
            }
        }
        // ④ 别名归一化匹配：客户端写官方全名（`claude-sonnet-4-5-20250929`）
        //    或点号版本（`claude-sonnet-4.5`），而上游声明的是另一种写法。
        //    只折叠「同一模型的别名」，不做子串/模糊匹配（见 norm_model_id）。
        //    注意：被面板停用的**声明模型**依然拒绝，避免用别名绕过开关。
        let want = norm_model_id(model);
        let alias_hit = |s: &ProviderSpec| {
            s.models
                .iter()
                .any(|m| norm_model_id(m) == want && !self.flags.is_model_disabled(m))
        };
        if let Some(s) = preferred.iter().find(|s| alias_hit(s)) {
            return Some((*s).clone());
        }
        if let Some(s) = usable.iter().find(|s| alias_hit(s)) {
            return Some((*s).clone());
        }
        {
            let g = self.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
            let live_hit = |ids: &Vec<String>| {
                ids.iter()
                    .any(|m| norm_model_id(m) == want && !self.flags.is_model_disabled(m))
            };
            for (sid, (_, ids, _)) in g.iter() {
                if !live_hit(ids) {
                    continue;
                }
                if let Some(s) = preferred.iter().find(|s| &s.id == sid) {
                    return Some((*s).clone());
                }
            }
            for (sid, (_, ids, _)) in g.iter() {
                if !live_hit(ids) {
                    continue;
                }
                if let Some(s) = usable.iter().find(|s| &s.id == sid) {
                    return Some((*s).clone());
                }
            }
        }

        // 单平台兜底：只剩这一个窄口子，且**必须是「我们无法枚举它的清单」**的情形。
        //
        // 旧实现是 `if usable.len() == 1 { return usable[0] }`——只要全站只有一个
        // 可用平台就接受任意模型名，等于静默兜底：拼错模型名的请求被转发出去，
        // 上游回 404「model does not exist」，而面板自己的错误码是 200/503，
        // 调用方（Claude Code / LiteLLM 这类按状态码重试的客户端）会一直重试一个
        // 永远不可能成功的请求。与已确立的「未知模型 404 model_not_found」语义冲突。
        //
        // 保留的例外：唯一平台是 live 清单策略、且它的清单**拉取失败/为空**——
        // 此时我们客观上没有它的模型名册（自定义网关不通 /models 很常见），
        // 「未知模型」这个判断本身无法做出，放行由上游去裁决比本地误判更诚实。
        // ★ 显式平台后缀绝不走兜底：`chat_core` 已按 is_platform_id（含停用平台）
        //   认定后缀合法并把它剥掉了，兜底再把它转给「唯一的那个平台」等于
        //   用户说「走 A」、系统悄悄走了 B —— 上游回 404「model does not exist」，
        //   看起来像模型名写错，实际是路由改了主意。
        if usable.len() == 1 {
            if let Some((_, plat)) = model.rsplit_once('@') {
                if self.is_platform_id(plat) {
                    return None;
                }
            }
            let only = usable[0];
            let live = only.models_policy == aigw_registry::ModelsPolicy::Live;
            let listing_empty = self
                .models_cache_per
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&only.id)
                .map(|(_, ids, _)| ids.is_empty())
                .unwrap_or(true);
            if live && listing_empty && self.platform_usable(only) {
                tracing::warn!(
                    "模型 {} 未在平台 {} 的清单里，但该平台 live 清单为空（拉取失败？），放行由上游裁决",
                    model,
                    only.id
                );
                return Some(only.clone());
            }
        }
        None
    }

    /// 真正发给上游的模型 id：客户端可能用别名（带日期 / 点号写法），
    /// 上游只认自己声明的那个字符串，所以必须按平台清单回填。
    ///
    /// 顺序：精确命中 → 别名归一化命中 → live 缓存精确 → live 缓存归一化 → 原样。
    pub fn canonical_model_for(&self, spec: &ProviderSpec, requested: &str) -> String {
        if spec.models.iter().any(|m| m == requested) {
            return requested.to_string();
        }
        if let Some(m) = spec.models.iter().find(|m| same_model(m, requested)) {
            return m.clone();
        }
        {
            let g = self.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, ids, _)) = g.get(&spec.id) {
                if let Some(m) = ids.iter().find(|m| same_model(m, requested)) {
                    return m.clone();
                }
            }
        }
        requested.to_string()
    }

    /// `spec_for_model` 的异步版：静态清单和缓存都命中不了时，**按需去上游拉一次**
    /// 可用平台的模型清单，再重试匹配。
    ///
    /// 为什么必须这样做：kilo / pollinations 这类 `models_policy=live` 的平台
    /// 静态 `models` 是空的，模型名只存在于 live 清单里。而 live 清单过去只在
    /// `GET /v1/models` 被调用时才填充——服务刚起来、客户端直接发 chat 时
    /// 缓存全空，于是「kilo-auto/free」被判成未知模型 503（实测复现）。
    /// 让路由自己补齐清单，冷启动第一条消息才不会莫名其妙地失败。
    async fn resolve_spec(&self, model: &str) -> Option<ProviderSpec> {
        if let Some(s) = self.spec_for_model(model) {
            return Some(s);
        }
        if self.flags.is_model_disabled(model) {
            return None;
        }
        // 只对「可用 + live + 还没有缓存」的平台补拉，避免每次未命中都打一轮上游
        let targets: Vec<ProviderSpec> = self
            .all_specs()
            .into_iter()
            .filter(|s| self.platform_routable(s))
            .filter(|s| s.models_policy == aigw_registry::ModelsPolicy::Live)
            .filter(|s| {
                // 中毒时旧实现 `.map(..).unwrap_or(false)` 会把**所有** live 平台当成
                // 「已有缓存」，于是一个都不补拉，冷启动的未知模型直接 503。
                !self
                    .models_cache_per
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&s.id)
            })
            .collect();
        if targets.is_empty() {
            return None;
        }
        self.fetch_live_models(targets).await;
        self.spec_for_model(model)
    }

    /// 并发拉取一批平台的 live 模型清单并写回缓存（有界并发，单个失败不影响整体）。
    async fn fetch_live_models(&self, specs: Vec<ProviderSpec>) {
        if specs.is_empty() {
            return;
        }
        let sem = Arc::new(tokio::sync::Semaphore::new(MODELS_FETCH_CONCURRENCY));
        let mut handles = Vec::new();
        for spec in specs {
            let client = self.client.clone();
            let secret = self.platform_secret(&spec).unwrap_or_default();
            let sem2 = sem.clone();
            handles.push(tokio::spawn(async move {
                // Some(ids)=拉取成功（含合法的空清单）；None=失败（超时/4xx/5xx/没额度）。
                let Ok(_permit) = sem2.acquire_owned().await else {
                    return (spec.id.clone(), None);
                };
                let out = match client.fetch_models_with_secret(&spec, &secret).await {
                    Ok(ids) => Some(ids),
                    Err(e) => {
                        tracing::debug!(
                            platform = %spec.id,
                            status = e.status,
                            err = %e.body,
                            "模型清单拉取失败，保留旧清单按短 TTL 重试"
                        );
                        None
                    }
                };
                (spec.id.clone(), out)
            }));
        }
        let results = futures_util::future::join_all(handles).await;
        // 锁中毒不能让 live 清单**永远写不进去**：那会让每个 /v1/models 与每次
        // 冷启动 chat 都重新打一轮上游（实测 26s+），且没有任何日志说明原因。
        let mut g = self.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
        for r in results {
            if let Ok((id, fetched)) = r {
                // ★ stale-on-error：失败（None）不清空旧清单（见 ModelsEntry 注释），
                //   只把 ok 标成 false 让 TTL 走 120s 短档以便较快重试。
                let ok = fetched.is_some();
                let ids = match fetched {
                    Some(ids) => ids,
                    None => g
                        .get(&id)
                        .map(|(_, prev, _)| prev.clone())
                        .unwrap_or_default(),
                };
                g.insert(id, (Instant::now(), ids, ok));
            }
        }
    }

    /// 启动时预热 live 模型清单（后台执行，不阻塞监听）。
    ///
    /// 冷启动后第一条 chat 若撞上空的 live 缓存，路由就得现场去拉上游（实测
    /// 26s 才返回第一条消息）。把这一步挪到启动后台跑，用户第一次提问就是热路径。
    pub fn warm_models(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let st = self.clone();
        async move {
            // 过滤条件必须与 models() 一致（platform_usable），否则「usable 但不可
            // routable」的平台不在预热范围内，冷启动第一条 /v1/models 仍要现场拉。
            let targets: Vec<ProviderSpec> = st
                .all_specs()
                .into_iter()
                .filter(|s| st.platform_usable(s))
                .filter(|s| s.models_policy == aigw_registry::ModelsPolicy::Live)
                .collect();
            st.fetch_live_models(targets).await;
            // 顺手把整表缓存也填好：否则首个 /v1/models 请求虽然 per 缓存已热、不必
            // 访问上游，但仍要现场组装并抢 models_lock。
            let (body, index) = assemble_models(&st);
            let mut g = st.models_cache.lock().unwrap_or_else(|e| e.into_inner());
            *g = Some((Instant::now(), index, body));
            tracing::info!("模型清单预热完成");
        }
    }

    /// 带监管的预热：panic 只记一次失败（`/api/health` 的 `tasks.warm_models` 可见），
    /// 不会让进程或其它任务受影响。
    pub async fn warm_models_supervised(self) {
        use futures_util::FutureExt;
        let tasks = self.tasks.clone();
        let done = self.warm_done.clone();
        match std::panic::AssertUnwindSafe(self.warm_models()).catch_unwind().await {
            Ok(()) => tasks.mark_ok("warm_models", "模型清单已预热"),
            Err(_) => {
                tasks.mark_err("warm_models", "预热 panic 已被隔离（详见日志）");
                tracing::error!("模型清单预热 panic，已隔离");
            }
        }
        // ★ 成功、失败、panic 三条路都要放行：公益站刷新在等这个信号，漏发会让它
        //   一直等到 `WARM_WAIT_CAP` 上限（那一刻才「放弃等待」起跑）。
        //
        // 用 `send_replace` 而不是 `send`：`send` 在没有订阅者时返回 Err **且不改值**
        // —— 启动早期 main 还没订阅（或订阅者被 drop）时信号就丢了，公益站刷新只能
        // 干等到 60 秒上限。这一点被单测 `warm_models_signals_done_so_free_refresh_can_start`
        // 抓出来过（当时一直是 false）。
        let _ = done.send_replace(true);
    }

    /// 刷新全部自定义 API 的模型清单（后台定时任务用）。
    ///
    /// 为什么要定时刷：清单只在「新建 / 编辑 / 手动点同步」时更新，之后就一直冻着。
    /// 本机聚合桥重启后会换一批模型，而面板这边仍按旧清单路由 —— 用户看到的是
    /// 「模型在列表里但一发就 404」。实测过一次：桥刷新后 /v1/models 从 148 掉到 67，
    /// 只有手动点「同步模型」才恢复。
    ///
    /// 单条失败不影响其它条目（各自记账），但有失败就整体返回 Err —— 让 supervise_loop
    /// 用更短的 retry 周期重来，而不是等到下一个整点。
    pub async fn refresh_custom_models_now(&self) -> Result<String, String> {
        let rows = self.custom.rows();
        if rows.is_empty() {
            return Ok("没有自定义 API，跳过".to_string());
        }
        let mut ok = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for c in rows {
            if !c.enabled {
                continue;
            }
            match sync_custom_models(self, &c).await {
                Ok(_) => ok += 1,
                Err(e) => {
                    // 单条失败只记名字与原因，不把整条链路带崩。
                    failed.push(format!("{}（{e}）", c.name));
                    tracing::warn!("自定义 API {} 定时刷新模型清单失败：{e}", c.id);
                }
            }
        }
        if failed.is_empty() {
            Ok(format!("已刷新 {ok} 个自定义 API 的模型清单"))
        } else {
            Err(format!("{} 个成功，{} 个失败：{}", ok, failed.len(), failed.join("；")))
        }
    }
}

/// 自定义 API → 声明式 ProviderSpec（auth=api_key；Anthropic 协议走 anthropic 透传）。
fn custom_to_spec(c: &aigw_store::CustomApi) -> ProviderSpec {
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
fn free_to_spec(s: &aigw_store::FreeSite) -> ProviderSpec {
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

/// Host 头取主机名：`127.0.0.1:8800` → `127.0.0.1`；`[::1]:8800` → `[::1]`。
fn host_only(raw: &str) -> String {
    let s = raw.trim();
    if s.starts_with('[') {
        return match s.find(']') {
            Some(i) => s[..=i].to_string(),
            None => s.to_string(),
        };
    }
    match s.rfind(':') {
        Some(i) if s[i + 1..].chars().all(|c| c.is_ascii_digit()) && !s[i + 1..].is_empty() => {
            s[..i].to_string()
        }
        _ => s.to_string(),
    }
}

/// `https://evil.example:8443/x` → `evil.example`；无法解析返回空串（按不通过处理）。
fn origin_host(raw: &str) -> String {
    let s = raw.trim();
    let rest = match s.find("://") {
        Some(i) => &s[i + 3..],
        None => s,
    };
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    host_only(hostport)
}

/// IP 字面量判定（IPv4 / IPv6 / 带方括号的 IPv6）。域名一律返回 false。
fn is_ip_literal(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.is_empty() {
        return false;
    }
    if h.parse::<std::net::Ipv4Addr>().is_ok() || h.parse::<std::net::Ipv6Addr>().is_ok() {
        return true;
    }
    // 允许局域网常见写法之外不做任何模糊匹配：主机名一律视为域名
    false
}

/// Host / Origin 主机名是否放行。
///
/// - 命中白名单（回环名 + 绑定地址）→ 放行；
/// - 通配监听（0.0.0.0 / ::）时，**任意 IP 字面量**放行：局域网用
///   `192.168.1.5:8800` 访问面板是正常用法；
/// - 域名永不因通配而放行：DNS 重绑定的载荷一定是域名。
fn host_allowed(st: &AppState, host: &str) -> bool {
    if st.allow_hosts.iter().any(|h| ct_eq(h, host)) {
        return true;
    }
    *st.allow_any_ip_host && is_ip_literal(host)
}

//// 出站目标是否属于「不该被本面板当上游」的地址。
///
/// 面板是本机单用户工具，但它有能力带着用户填的 Key 去请求任意地址。真正值得挡的不是
/// 「用户自己填内网地址」——那往往是有意的（本机另一个网关、局域网内的 vLLM）；而是两类：
/// 1. **云元数据端点**（169.254.169.254 / fd00:ec2::254 / metadata.google.internal 等）：
///   一旦被填进来，上游响应里就是云主机的临时凭据，属典型 SSRF 提权。
/// 2. **回环地址上的非预期端口**：本机 8800 是面板自己，用它当上游会形成自指回环。
///
/// 因此这里只做「元数据 + 面板自指」的硬拦截，回环与其它内网地址一律放行并在文档里
/// 讲清楚取舍——一刀切封内网会把「接本机另一个 OpenAI 兼容网关」这种正常用法打死。
pub fn blocked_upstream_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    // 云元数据：各家 IaaS 的链路本地地址与常见内网域名
    const META: &[&str] = &[
        "169.254.169.254",
        "169.254.170.2",
        "100.100.100.200",
        "metadata.google.internal",
        "metadata.goog",
        "metadata.azure.com",
        "fd00:ec2::254",
    ];
    if META.iter().any(|m| h == *m) {
        return true;
    }
    false
}

/// 从 base_url / 代理 url 里取出主机名（去掉 scheme、端口、路径）。取不到返回空串。
pub fn url_host(raw: &str) -> String {
    origin_host(raw)
}

/// 「同一个站」判定用的 host 规范化：小写 + 去 `www.`（端口与路径 `url_host` 已去掉）。
fn site_host(raw: &str) -> String {
    let h = url_host(raw).trim().to_ascii_lowercase();
    match h.strip_prefix("www.") {
        Some(rest) => rest.to_string(),
        None => h,
    }
}

/// 内置接入源里公益站那一批的 host 集合（`providers/free-*.toml`）。
///
/// 为什么要它：公益站有两条来路 —— 编译进二进制的 `providers/free-*.toml`，与聚合页
/// 动态刷新出来的条目（id 也叫 `free-*`，但它只是 host 的 slug）。同一个站同时出现在
/// 两边时 id 并不相同（`free-agentrouter` vs `free-agentrouter-org`），
/// `FreeStore::upsert_batch` 的**按 id 去重**根本拦不住 —— 面板上就是两张一模一样的
/// 卡片，「共 N 站」也把同一个站算了两次。真实数据里 agentrouter.org 就这么重了一次。
pub fn static_free_hosts(specs: &[ProviderSpec]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in specs {
        if !s.id.starts_with("free-") {
            continue;
        }
        // base_url 与 login_url 都算：有的站 base 在 api.x.com、注册页在 x.com。
        for u in [s.base_url.as_str(), s.login_url.as_str()] {
            let h = site_host(u);
            if !h.is_empty() && !out.contains(&h) {
                out.push(h);
            }
        }
    }
    out
}

/// 代理主机是否落在本机回环（localhost / ::1 / 127.0.0.0-8）。
///
/// 与前端 `isLocalProxy()` 的判定保持一致：只要不是回环，就必须由调用方显式确认
/// （`confirm_nonlocal: true`）才允许保存 —— 出站代理能看到每一次转发的完整凭据，
/// 它的危险程度高于任何单个上游。
pub fn is_loopback_host(host: &str) -> bool {
    let h = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    if h == "localhost" || h == "::1" {
        return true;
    }
    match h.parse::<std::net::Ipv4Addr>() {
        Ok(v4) => v4.is_loopback(),
        Err(_) => false,
    }
}
// 常量时间比较（令牌校验用，避免逐字节短路泄露长度/前缀）。
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn forbidden(msg: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"ok": false, "message": msg})),
    )
        .into_response()
}

fn unauthorized(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"ok": false, "message": msg})),
    )
        .into_response()
}

/// 从请求里取面板令牌：`X-AIGW-Token` / `Authorization: Bearer` / `Cookie: aigw_token=`。
/// 传进来的令牌是不是「本服务签发的访问密钥」的形状（24 字节 → 48 位小写 hex）。
/// 用途只有一处：密钥表已经被删空时，仍要认出「这是把作废的旧 key」而不是匿名调用。
fn looks_like_access_key(s: &str) -> bool {
    s.len() == 48 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn presented_token(req: &Request) -> Option<String> {
    if let Some(v) = req.headers().get("x-aigw-token").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    if let Some(v) = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        let t = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")).unwrap_or("");
        if !t.is_empty() {
            return Some(t.trim().to_string());
        }
    }
    if let Some(v) = req.headers().get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for part in v.split(';') {
            let part = part.trim();
            if let Some(t) = part.strip_prefix("aigw_token=") {
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}


/// 限流窗口：固定 60s。窗口内被拒的请求不计数（标准固定窗口语义）——
/// 否则持续刷会让计数只增不减，客户端等到窗口滚动也解不开。
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// 每 IP 计数桶的内存上限。真实场景只有几十个来源，这个上限只为防「来源地址极多
/// 时 HashMap 无限增长」；超限时先清掉已过窗口的桶，仍超限就只按全局额度放行。
const RATE_MAX_IP_BUCKETS: usize = 4096;

#[derive(Clone, Copy)]
struct RateSlot {
    win: Instant,
    count: u32,
}

/// 每分钟固定窗口限流：全局 + 每客户端 IP（`0` 表示该项不启用）。
///
/// 为什么需要：`/v1/*` 是唯一真的消耗上游额度的入口。此前只有「每账号 2 路在途、
/// 等 15s 后 429」这一道闸 —— 那是**按账号的并发闸，不是速率闸**：单个客户端串行
/// 发几千条小请求永远不会同时占用 3 路在途，却足以把某个账号打到上游限流（冷却 15
/// 分钟），面板上表现为「所有平台突然一起 429」。本机多个进程、或拿到面板令牌的
/// 任意客户端都能造成这种效果。
pub struct RateLimiter {
    per_ip: u32,
    global: u32,
    ip: Mutex<HashMap<IpAddr, RateSlot>>,
    /// 按访问密钥（key id）的每分钟上限，见下面 `hit_key`。顺序：
    /// 谁最容易被一把 key 打穿，就先扣谁的额度。
    pub key_rate: u32,
    /// key id → 该 key 的计数槽。只放「非空」的槽，key 删掉后自然过期淘汰。
    key: Mutex<HashMap<String, RateSlot>>,
    g: Mutex<RateSlot>,
    /// 累计被挡下的请求数 + 最近一次的现场。
    ///
    /// 为什么需要：429 只出现在响应体与日志里，面板上完全不可见 —— 用户体感是
    /// 「突然变慢 / 全都失败」，却看不到「是我自己打太快」。把计数和上次的时间点
    /// 暴露到 /api/health，排障时第一眼就能排除/确认限流这个原因。
    rejected: AtomicU64,
    last_reject: Mutex<Option<RejectInfo>>,
}

/// 最近一次被限流的现场。
struct RejectInfo {
    at: Instant,
    /// "ip" 或 "global"：被哪个维度挡下的
    scope: &'static str,
    /// 当时告诉客户端的 Retry-After 秒数
    left_s: u64,
}

impl RateLimiter {
    pub fn new(per_ip: u32, global: u32) -> RateLimiter {
        let now = Instant::now();
        RateLimiter {
            per_ip,
            global,
            ip: Mutex::new(HashMap::new()),
            key_rate: 0,
            key: Mutex::new(HashMap::new()),
            g: Mutex::new(RateSlot { win: now, count: 0 }),
            rejected: AtomicU64::new(0),
            last_reject: Mutex::new(None),
        }
    }

    pub fn per_ip(&self) -> u32 {
        self.per_ip
    }

    pub fn global(&self) -> u32 {
        self.global
    }

    pub fn enabled(&self) -> bool {
        self.per_ip > 0 || self.global > 0
    }

    /// 按 key 限流是否启用。
    pub fn key_enabled(&self) -> bool {
        self.key_rate > 0
    }

    /// 记一次「带着访问密钥」的 /v1 请求。
    ///
    /// 为什么单独一层：每 IP 限流在本机部署下等同于**全局**限流（面板、脚本、客户端
    /// 全在 127.0.0.1），所以「给同事一把 key、他挂了死循环」会把这个人的面板和所有
    /// 别的客户端一起卡住 —— 而 key 的意义正是「把不同用途分开」。按 key 计数才能
    /// 让限额落在「谁在刷」上。这个 map 有界：key 删了就自然淘汰，上限同 IP 桶。
    pub fn hit_key(&self, key_id: &str) -> Result<(), u64> {
        if !self.key_enabled() {
            return Ok(());
        }
        let now = Instant::now();
        let mut m = self.key.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= RATE_MAX_IP_BUCKETS {
            m.retain(|_, s| now.duration_since(s.win) < RATE_WINDOW);
        }
        if m.len() >= RATE_MAX_IP_BUCKETS {
            // 桶满且全在窗口内：宁可放行也不丢请求（有界内存优先），代价是新 key 暂不计。
            return Ok(());
        }
        let slot = m.entry(key_id.to_string()).or_insert(RateSlot { win: now, count: 0 });
        roll(slot, now);
        if slot.count >= self.key_rate {
            let wait = retry_after(slot, now);
            drop(m);
            self.note_reject("key", wait);
            return Err(wait);
        }
        slot.count += 1;
        Ok(())
    }

    /// 记一次 `/v1` 请求；超限返回需要等待的秒数（用于 `Retry-After`）。
    pub fn hit(&self, ip: Option<IpAddr>) -> Result<(), u64> {
        if !self.enabled() {
            return Ok(());
        }
        let now = Instant::now();
        {
            let mut g = self.g.lock().unwrap_or_else(|e| e.into_inner());
            roll(&mut g, now);
            if self.global > 0 && g.count >= self.global {
                let wait = retry_after(&g, now);
                drop(g);
                self.note_reject("global", wait);
                return Err(wait);
            }
            if self.global > 0 {
                g.count += 1;
            }
        }
        if self.per_ip > 0 {
            if let Some(ip) = ip {
                let mut m = self.ip.lock().unwrap_or_else(|e| e.into_inner());
                if m.len() >= RATE_MAX_IP_BUCKETS {
                    m.retain(|_, s| now.duration_since(s.win) < RATE_WINDOW);
                }
                // 清完仍满：只按全局额度放行（有界内存优先于逐 IP 精确）
                if m.len() < RATE_MAX_IP_BUCKETS {
                    let slot = m.entry(ip).or_insert(RateSlot { win: now, count: 0 });
                    roll(slot, now);
                    if slot.count >= self.per_ip {
                        let wait = retry_after(slot, now);
                        drop(m);
                        // 这条没被放行：退回刚才预扣的全局额度
                        if self.global > 0 {
                            let mut g = self.g.lock().unwrap_or_else(|e| e.into_inner());
                            g.count = g.count.saturating_sub(1);
                        }
                        self.note_reject("ip", wait);
                        return Err(wait);
                    }
                    slot.count += 1;
                }
            }
        }
        Ok(())
    }

    /// 记一次「被挡下」：累计数 + 最近现场。
    fn note_reject(&self, scope: &'static str, left_s: u64) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
        let mut l = self.last_reject.lock().unwrap_or_else(|e| e.into_inner());
        *l = Some(RejectInfo {
            at: Instant::now(),
            scope,
            left_s,
        });
    }

    /// 给面板/health 的快照：限流是否启用、上限多少、被挡过几次、上次多久前。
    pub fn snapshot(&self) -> Value {
        let last = self.last_reject.lock().unwrap_or_else(|e| e.into_inner());
        let (ago, scope, left) = match last.as_ref() {
            Some(r) => (
                Some(r.at.elapsed().as_secs()),
                json!(r.scope),
                Some(r.left_s),
            ),
            None => (None, Value::Null, None),
        };
        json!({
            "enabled": self.enabled(),
            "per_ip_rpm": self.per_ip,
            "global_rpm": self.global,
            "key_rpm": self.key_rate,
            "rejected_total": self.rejected.load(Ordering::Relaxed),
            "last_reject_s_ago": ago,
            "last_reject_scope": scope,
            "last_retry_after_s": left,
        })
    }
}

fn roll(slot: &mut RateSlot, now: Instant) {
    if now.duration_since(slot.win) >= RATE_WINDOW {
        slot.win = now;
        slot.count = 0;
    }
}

fn retry_after(slot: &RateSlot, now: Instant) -> u64 {
    let el = now.duration_since(slot.win).as_secs();
    RATE_WINDOW.as_secs().saturating_sub(el).max(1)
}

/// 429：与 `/v1` 其它错误保持 OpenAI 错误信封，并带 `Retry-After`。
fn rate_limited(wait: u64, lim: &RateLimiter) -> Response {
    let mut msg = String::from("请求过于频繁，已达限流上限");
    if lim.per_ip() > 0 && lim.global() > 0 {
        msg.push_str(&format!(
            "（每客户端 IP {}/分钟，全局 {}/分钟）",
            lim.per_ip(),
            lim.global()
        ));
    } else if lim.per_ip() > 0 {
        msg.push_str(&format!("（每客户端 IP {}/分钟）", lim.per_ip()));
    } else {
        msg.push_str(&format!("（全局 {}/分钟）", lim.global()));
    }
    msg.push_str("。可用启动参数 --ip-rpm / --rpm 调整，0 表示关闭该档。");
    let body = json!({
        "error": {
            "message": msg,
            "type": "rate_limited",
            "code": "rate_limited",
        }
    });
    let mut r = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

/// 后台长任务的一次运行结果（只用于观测，不参与调度决策）。
#[derive(Clone, Default)]
pub struct TaskStat {
    pub ok_runs: u64,
    pub err_runs: u64,
    /// 最近一次成功的 unix 秒。
    pub last_ok: Option<u64>,
    /// 最近一次成功的说明（例如「共 43 站，新增 2，保留 41；聚合页 43 条，丢弃 3（host 不合规 2，重复 1）」）。
    ///
    /// 只记时间不记内容的话，「刷新成功但一个站都没拿到」与「刷新成功拿到 43 个站」在
    /// 面板上是同一个样子 —— 聚合页改版这类故障会静默持续一整天。
    pub last_ok_msg: Option<String>,
    /// 最近一次失败（或 panic）的 unix 秒与原因。
    pub last_err: Option<(u64, String)>,
}

/// 后台任务健康台账：`/api/health` 里的 `tasks` 字段。
///
/// 为什么需要它：`tokio::spawn` 出去的后台循环**一旦 panic 就永久消失**，而进程还活着、
/// `/api/health` 还是 `ok:true` —— 面板看上去一切正常，实际「公益站不再刷新」「模型清单
/// 不再预热」，且没有任何日志会被用户看到。旧实现就是这个状态：一条 `refresh_free_sites`
/// 里的 `unwrap` 就能让每日刷新静默停摆。这里把「最近一次成功/失败时间」显式暴露出来，
/// 「本来该在 X 小时前跑过一次的任务，已经 Y 小时没动静」就成了可观测事实。
#[derive(Default)]
pub struct TaskHealth {
    inner: Mutex<HashMap<&'static str, TaskStat>>,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl TaskHealth {
    pub fn new() -> TaskHealth {
        TaskHealth::default()
    }

    /// 预登记一个任务：还没跑过也出现在台账里（`ok_runs: 0`），
    /// 否则「24 小时才跑一次」的任务在头一天完全不可见。
    pub fn declare(&self, name: &'static str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(name)
            .or_default();
    }

    pub fn mark_ok(&self, name: &'static str, msg: impl Into<String>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.entry(name).or_default();
        e.ok_runs += 1;
        e.last_ok = Some(unix_now());
        e.last_ok_msg = Some(msg.into());
    }

    pub fn mark_err(&self, name: &'static str, why: impl Into<String>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.entry(name).or_default();
        e.err_runs += 1;
        e.last_err = Some((unix_now(), why.into()));
    }

    pub fn stat(&self, name: &str) -> Option<TaskStat> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// 快照：`{任务名: {ok_runs, err_runs, last_ok_s_ago, last_err, last_err_s_ago}}`。
    /// 时间给「多少秒之前」而不是绝对时间，读的人不用再对表。
    pub fn snapshot(&self) -> Value {
        let now = unix_now();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = serde_json::Map::new();
        for (k, v) in g.iter() {
            out.insert(
                (*k).to_string(),
                json!({
                    "ok_runs": v.ok_runs,
                    "err_runs": v.err_runs,
                    "last_ok_s_ago": v.last_ok.map(|t| now.saturating_sub(t)),
                    // 成功也要带说明：失败有 last_err，成功什么都不记就等于「刷新成功」
                    // 可以掩盖「拿到 0 个站」。
                    "last_ok": v.last_ok_msg.clone(),
                    "last_err": v.last_err.as_ref().map(|(_, m)| m.clone()),
                    "last_err_s_ago": v.last_err.as_ref().map(|(t, _)| now.saturating_sub(*t)),
                }),
            );
        }
        Value::Object(out)
    }
}

/// 把「每个周期跑一次」的后台任务包成**不会死**的循环。
///
/// - `body` 返回 `Ok(说明)` 记成功、`Err(原因)` 记失败；
/// - `body` 里 panic 会被 `catch_unwind` 隔离，记一次失败并**继续调度**（旧行为：整个任务消失）；
/// - 成功后退回 `period`，失败后按 `retry` 更早重试 —— 网络抖动不该让「公益站刷新」
///   停摆整整一个周期。
pub fn supervise_loop<F, Fut>(
    name: &'static str,
    health: Arc<TaskHealth>,
    period: std::time::Duration,
    retry: std::time::Duration,
    body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
{
    supervise_loop_opts(name, health, period, retry, false, body)
}

/// 与 supervise_loop 相同，但**先跑一轮再进入周期**。
///
/// 用于「周期很长、且冷启动时就该有数据」的任务：24 小时周期的公益站刷新如果先睡
/// 24 小时再跑，全新部署当天面板和 /api/health 一直是「尚未成功过」——用户只能
/// 靠手动点刷新来绕过，而那本该是默认行为。
pub fn supervise_loop_now<F, Fut>(
    name: &'static str,
    health: Arc<TaskHealth>,
    period: std::time::Duration,
    retry: std::time::Duration,
    body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
{
    supervise_loop_opts(name, health, period, retry, true, body)
}

fn supervise_loop_opts<F, Fut>(
    name: &'static str,
    health: Arc<TaskHealth>,
    period: std::time::Duration,
    retry: std::time::Duration,
    run_first: bool,
    mut body: F,
) -> impl std::future::Future<Output = ()> + Send
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<String, String>> + Send,
{
    use futures_util::FutureExt;
    async move {
        health.declare(name);
        // 首次等待：要立刻跑的任务用 ZERO（sleep(0) 只让出一次执行权）。
        let mut wait = if run_first {
            std::time::Duration::ZERO
        } else {
            period
        };
        loop {
            tokio::time::sleep(wait).await;
            let fut = std::panic::AssertUnwindSafe(body()).catch_unwind();
            match fut.await {
                Ok(Ok(msg)) => {
                    health.mark_ok(name, msg.clone());
                    tracing::info!("后台任务 {} 完成：{}", name, msg);
                    wait = period;
                }
                Ok(Err(e)) => {
                    health.mark_err(name, e.clone());
                    tracing::warn!("后台任务 {} 失败：{}（{} 秒后重试）", name, e, retry.as_secs());
                    wait = retry;
                }
                Err(_) => {
                    // panic 的载荷不一定是 &str，这里只关心「发生过」，细节已进 panic hook。
                    health.mark_err(name, "任务 panic 已被隔离（详见日志）");
                    tracing::error!("后台任务 {} 发生 panic，已隔离并继续调度", name);
                    wait = retry;
                }
            }
        }
    }
}

/// 请求门禁（对应旧版 Python `_guard`）：
/// 1. **Host 白名单** —— 挡 DNS 重绑定（攻击者域名解析到 127.0.0.1 后浏览器按同源读数据）；
/// 2. **Origin/Referer 白名单** —— 挡跨站表单/POST（CSRF），例如恶意页面改写 /api/proxy；
/// 3. **令牌** —— 配置了 `panel_token` 时 `/api/*` 与 `/v1/*` 都要校验。
///    旧实现只校验 `/api/*`（理由：`/v1/*` 是 OpenAI 协议面，SDK 直连），但它同时也是
///    **消耗你账号额度的入口**：`--host 0.0.0.0` 时令牌只保护了管理接口，局域网内任何
///    设备都能免密把 `/v1/chat/completions` 打满、把账号池拖进冷却。现在只要面板设了
///    令牌，`/v1/*` 就要求同一个令牌（当作 API Key 传给客户端即可，与 one-api / LiteLLM
///    的网关密钥一致）。未设令牌（默认本机回环）时行为不变。
///
/// 4. **速率闸** —— `/v1/*` 按「每客户端 IP + 全局」每分钟计数（见 `RateLimiter`）。
///    令牌挡的是未授权调用，挡不住「拿到令牌的客户端把账号池打满」。
///
/// 无 Origin/Referer 的请求（curl / OpenAI SDK / CLI）不视为跨站，属预期调用方式。
/// 请求里携带的访问密钥（`guard` 解析后塞进扩展，供模型白名单校验）。
///
/// 只有命中 `data/keys.json` 的 key 才会被塞进来；没带 / 不认识的一律没有这个东西，
/// 下游据此判断「这是一次受限调用吗」。
#[derive(Clone)]
pub struct AccessKeyCtx(pub aigw_store::AccessKey);

async fn guard(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let host = host_only(
        req.headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );
    if host.is_empty() {
        // ★ 旧实现是 `if !host.is_empty() && !host_allowed(..)`：请求不带 Host 头时整条
        //   白名单被静默跳过。HTTP/1.1 下 hyper 会先拒掉无 Host 的请求，所以这条路径
        //   实际只对 HTTP/1.0 裸客户端开放 —— 但它恰好是管理面最该守住的一处。
        //   管理面（/api/*）一律要求带 Host；协议面（/v1/*）放行，免得老 SDK 用
        //   HTTP/1.0 直连时被挡。
        if req.uri().path().starts_with("/api/") {
            return forbidden("缺少 Host 头：管理接口只接受带 Host 的请求（面板只允许本机访问）");
        }
    } else if !host_allowed(&st, &host) {
        return forbidden("Host 不在白名单：面板只允许本机访问");
    }
    for h in [header::ORIGIN, header::REFERER] {
        if let Some(v) = req.headers().get(h) {
            // ★ 旧实现是 `and_then(|v| v.to_str().ok())`：来源头里塞非 UTF-8 字节时
            //   to_str() 失败 → 整条来源校验被静默跳过 = 「用非法字节绕过来源白名单」。
            //   浏览器发不出这种头，所以直接当非法来源拒绝。
            let Ok(v) = v.to_str() else {
                return forbidden("来源头非法（非 UTF-8）：拒绝请求");
            };
            let oh = origin_host(v);
            if oh.is_empty() || !host_allowed(&st, &oh) {
                return forbidden("来源不在白名单：拒绝跨站请求");
            }
        }
    }
    let path = req.uri().path();
    let is_api = path.starts_with("/api/");
    let is_v1 = path.starts_with("/v1/");
    // 访问密钥（多把 + 每把白名单）：与面板令牌分成两条独立通道。
    //
    // 为什么不是「把令牌表化」：面板令牌是**管理面**的全通票（能改配置、能导出
    // 凭据），而访问密钥只该决定「能调哪些模型」。两者混在一起，等于给每个借出去的
    // key 都开了管理权限。
    //
    // 语义：传进来的 key 若命中密钥表，就记下它（供模型白名单校验用）；命中不了
    // 就继续按原来的面板令牌逻辑判定 —— 两张表互不干扰，旧部署行为完全不变。
    let presented = presented_token(&req);
    if is_v1 {
        if let Some(t) = presented.as_deref() {
            if let Some(k) = st.keys.find_by_secret(t) {
                req.extensions_mut().insert(AccessKeyCtx(k));
            }
        }
    }
    let cur_tok = st.current_token();
    if let Some(tok) = cur_tok.as_ref() {
        if is_api || is_v1 {
            // 访问密钥命中时同样算通过「进得来」这一关，但只对协议面（/v1/*）有效；
            // 管理面（/api/*）仍然只认面板令牌 —— 借出去的 key 不该能改配置。
            let hit_key = is_v1 && st.keys.find_by_secret(presented.as_deref().unwrap_or("")).is_some();
            let ok = hit_key || presented.as_deref().map(|t| ct_eq(t, tok)).unwrap_or(false);
            if !ok {
                return if is_v1 {
                    unauthorized(
                        "缺少或错误的访问令牌：本服务设置了面板令牌，/v1/* 也要校验。\n\
                         把面板令牌当作 API Key 传给客户端即可（OpenAI SDK: api_key=\"<面板令牌>\"；\n\
                         curl: -H \"Authorization: Bearer <面板令牌>\"）。\n\
                         面板令牌来自启动参数 --token、环境变量 AIGW_PANEL_TOKEN，或对外监听时自动生成并打印在日志里。",
                    )
                } else {
                    unauthorized("缺少或错误的访问令牌（X-AIGW-Token / Bearer / aigw_token Cookie）")
                };
            }
        }
    }
    // ★ 吊销必须立刻生效。上面那段只在**开了面板令牌**时才判（没开就整块跳过 = 匿名
    //   放行），于是轮换/删除后的旧 key 命中不了密钥表 → 被当成匿名调用 → 照样 200：
    //   用户以为「轮换后旧 key 已作废」，实际它能一直用到重启。这里补一条独立判定 ——
    //   只要密钥表非空、请求又带了 Bearer、却哪把都没命中，就当失效密钥拒绝。
    //   「一把都还没建」的老行为保持不变：免鉴权直连网关是正常用法，不能一概要求 key。
    //   但「把最后一把也删了」也必须立刻失效 —— 那时密钥表是空的，光看表非空会漏判，
    //   所以再加一条：只要传进来的串长得像本服务的密钥（48 位 hex），没命中就是失效。
    //   这样既不误伤随手填的占位符（客户端里写 "no-key" 之类照旧放行），也不会让
    //   「删光密钥」变成「一键恢复匿名全开」。
    if is_v1 {
        if let Some(t) = presented.as_deref() {
            if st.keys.find_by_secret(t).is_none()
                && (!st.keys.is_empty() || looks_like_access_key(t))
            {
                let is_panel_tok = cur_tok.as_ref().map(|tok| ct_eq(t, tok)).unwrap_or(false);
                if !is_panel_tok {
                    return unauthorized(
                        "这把访问密钥无效或已失效（可能已被轮换或删除）。\n\
                         到面板「关于 → 访问密钥」里核对当前密钥；轮换会立刻作废旧密钥。\n\
                         管理面 /api/* 需要的是面板令牌，不是访问密钥。",
                    );
                }
            }
        }
    }
    if is_v1 {
        // 带访问密钥的请求先扣「这把 key 的额度」：本机部署下每 IP 限流≈全局限流，
        // 一把借出去的 key 挂死循环会连面板一起卡住，那是「按 IP」量不到的维度。
        // 放在 IP 判定之前，是为了让越界的那把 key 先被挡，别先吃掉全局份额。
        if let Some(k) = req.extensions().get::<AccessKeyCtx>() {
            if let Err(wait) = st.limits.hit_key(&k.0.id) {
                return rate_limited(wait, &st.limits);
            }
        }
        // 客户端 IP 只取连接的对端地址（`ConnectInfo`），不信 X-Forwarded-For ——
        // 那个头誰都能写，信它等于把「每 IP 额度」变成「每个假头一份额度」。
        let ip = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        if let Err(wait) = st.limits.hit(ip) {
            return rate_limited(wait, &st.limits);
        }
    }
    next.run(req).await
}
pub fn build_app(st: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/usage", get(usage))
        .route("/api/requests", get(requests))
        .route("/api/platforms", get(platforms).post(platform_action))
        .route("/api/custom", get(custom_list).post(custom_action))
        .route("/api/free", get(free_list).post(free_action))
        .route("/api/proxy", get(proxy_get).post(proxy_set))
        .route("/api/accounts", get(accounts).post(accounts_action))
        .route("/api/pool", get(pool_snapshot).post(pool_action))
        // 面板令牌轮换：改一次令牌不该等于重启一次服务。
        .route("/api/token", post(token_action))
        // 访问密钥（多把 + 每把的模型白名单）：借出去的 key 只能调指定模型。
        .route("/api/keys", get(keys_list).post(keys_action))
        .route("/api/combos", get(combos_list).post(combos_action))
        .route("/api/auth", post(auth_action))
        .route("/api/checkin", get(checkin_status_all).post(checkin_claim_all))
        .route("/api/backup", get(backup_export).post(backup_restore))
        // 完整备份（含凭据）：前端「⬇ 备份数据」用它，否则导出的备份恢复不回来
        .route("/api/backup/export", post(backup_export_full))
        .route("/api/update-check", get(update_check))
        .route("/v1/models", get(models))
        // 请求体上限：axum 默认 2MB，长上下文（贴代码/长文档）会被直接 413。
        // Python 版是 32MB，这里对齐；超限由 chat_completions 包成 OpenAI 错误信封。
        .route(
            "/v1/chat/completions",
            post(chat_completions).layer(DefaultBodyLimit::max(MAX_CHAT_BODY)),
        )
        .route(
            "/v1/messages",
            post(messages_completions).layer(DefaultBodyLimit::max(MAX_CHAT_BODY)),
        )
        .route(
            "/v1/responses",
            post(responses_completions).layer(DefaultBodyLimit::max(MAX_CHAT_BODY)),
        )
        .route(
            "/v1/messages/count_tokens",
            post(count_tokens).layer(DefaultBodyLimit::max(MAX_CHAT_BODY)),
        )
        // ★ 2026-10-26（P2）：联网搜索。Anthropic Messages 兼容形态，
        //   Claude Code / DSH 等客户端的搜索 provider 可零改动接入
        //   （对齐 workbuddy2api internal/server/search.go）。
        .route(
            "/v1/search/messages",
            post(search_messages).layer(DefaultBodyLimit::max(MAX_CHAT_BODY)),
        )
        // 请求体上限（全局）：旧实现只给 /v1/chat/completions 设了 32MB，
        // /api/* 走 axum 默认 2MB —— 用「⬆ 恢复备份」贴一个几 MB 的备份包直接 413（且是纯文本非 JSON）。
        .layer(DefaultBodyLimit::max(MAX_CHAT_BODY))
        .fallback(web_file)
        // 门禁层：Host/Origin 白名单 + /api/* 令牌（见 `guard`）
        .layer(middleware::from_fn_with_state(st.clone(), guard))
        .with_state(st)
}

/// 平台清单（声明式 spec 快照；含自定义 API 动态并入；base_url 含占位符时标注）。
async fn platforms(State(st): State<AppState>) -> Json<Value> {
    // 公益站的两条来路（内置 free-*.toml 与聚合刷新）会撞同一个站：动态那条如果
    // 与内置条目同域就不在这里展示，否则同一排里两张一模一样的卡片、状态各说各话。
    let shadowed = shadowed_free_ids(&st);
    let specs: Vec<ProviderSpec> = st
        .all_specs()
        .into_iter()
        .filter(|s| !shadowed.contains(&s.id))
        .collect();
    let custom_rows = st.custom.rows();
    let disabled = st.flags.disabled_models();
    // 近 24h 平台健康（直接复用台账按平台聚合，与 /api/usage 同源同口径）。
    //
    // 为什么放在这里：这是一个 128 个接入源的聚合网关，其中不少是「上游随时改
    // 反爬 / 匿名车道被封」的免费站点。失败率只能靠逐个点开试才知道，用户看到
    // 的是一排长得一样的卡片。台账里本来就有 ok 标记，顺手聚合出来下发即可，
    // 零额外存储代价。
    let health_now = aigw_store::now_unix();
    let health = st
        .store
        .usage_window_by_platform(health_now - PLATFORM_HEALTH_WINDOW_S, health_now);
    // live 平台的静态 `models` 通常是空的（清单只存在于上游），旧实现直接下发
    // spec.models → 前端抽屉里一个模型都列不出来，用户没法逐模型关掉。
    // 这里把已经拉到的 live 清单并进来（没拉到时仍为空，前端会提示「首次调用后可见」）。
    let per = st
        .models_cache_per
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let rows: Vec<Value> = specs
        .iter()
        .map(|s| {
            // 免密车道（auth=anon：kilo / pollinations / anon-zen）不需要账号：
            // 转发时用 `format!("{}:anon", spec.id)` 现造账号名，凭据恒为空串。
            // 历史版本往账号池里塞过 name="匿名公共池" 的占位行，若照实报 accounts_n，
            // 面板会显示「1 账号」并把它算进「已接入/账号凭据」，让人以为要维护一个
            // 并不存在的凭据。这里对 anon 恒报 0，另用 `anon` 字段让前端显示「免凭据」。
            let is_anon = s.auth == AuthKind::Anon;
            let acct_n = if is_anon {
                0
            } else {
                st.accounts.secrets_for(&s.id).len()
            };
            let is_custom = s.id.starts_with("custom-");
            // Key 掩码按字符切：`&key[..4]` 是字节切片，Key 里出现非 ASCII
            // （中文备注、误粘的 Cookie）会落在字符中间 panic 掉请求线程。
            let key_mask = if is_custom {
                custom_rows
                    .iter()
                    .find(|c| c.id == s.id)
                    .map(|c| {
                        if c.key_broken {
                            "需重新填写".to_string()
                        } else {
                            aigw_store::mask_key(&c.api_key)
                        }
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let models: Vec<String> = match (s.models_policy, per.get(&s.id)) {
                (aigw_registry::ModelsPolicy::Live, Some((_, ids, _))) if !ids.is_empty() => ids.clone(),
                _ => s.models.clone(),
            };
            let models_on = models
                .iter()
                .filter(|m| !disabled.contains(aigw_store::bare_model(m)))
                .count();
            json!({
                "id": s.id, "name": s.name, "auth": s.auth,
                "enabled": s.enabled,
                "upstream_kind": s.upstream_kind,
                "region": s.region,
                "models_n": models.len(),
                "models_on": models_on,
                "models": models,
                "models_policy": s.models_policy,
                "placeholder": s.base_url.starts_with('@'),
                "accounts_n": acct_n,
                "anon": is_anon,
                "login_url": s.login_url,
                "notes": s.notes,
                "chat_path": s.chat_path(),
                "chat_supported": s.chat_supported,
                // 这个平台支持的登录方式（qr/sms/pwd/device/paste）：抽屉按它画选项卡，
                // 接入源页的「仅看扫码」也按它筛。没有这个字段，前端就只能按平台 id 写死。
                "login_methods": s.login_methods(),
                // 协议与地址形态也下发给前端：卡片上的「不可用」要能说清是哪一条挡住的
                // （协议不支持透传 / 地址是占位符 / 没凭据），否则用户只会看到一个
                // 无差别的「凭据不可用」然后去填 Key，填完依然不通。
                "format": s.format,
                // 协议与地址形态也下发给前端：卡片上的「不可用」要能说清是哪一条挡住的
                // （协议不支持透传 / 地址是占位符 / 没凭据），否则用户只会看到一个
                // 无差别的「凭据不可用」然后去填 Key，填完依然不通。
                "custom": is_custom,
                "key_mask": key_mask,
                // 是否「真能转发」（已启用 + 有凭据）。前端据此把必然 401 的平台
                // 标成「未接入」而不是假装可用。
                "usable": st.platform_usable(s),
                // 近 24h 该平台的台账聚合：{requests,success,failed,tokensIn,tokensOut}。
                // 24h 内没有请求时是 null（不是零值对象）—— 前端据此区分「没跑过」
                // 与「跑了但全失败」，这两件事的处置方式完全不同。
                "recent": health.get(s.id.as_str()).cloned().unwrap_or(Value::Null),
                // 最近一次失败的原话（HTTP 状态 + 上游 message）。成功一次即清除；
                // 与 recent 的分工：recent 说「坏了多久」，这里说「坏在哪」。
                "last_error": st.last_err.json(s.id.as_str()),
            })
        })
        .collect();
    // 停用清单一起下发，前端不必再猜哪个模型被关了。
    let mut disabled_list: Vec<String> = disabled.iter().cloned().collect();
    disabled_list.sort();
    Json(json!({
        "total": rows.len(),
        "rows": rows,
        "disabled_models": disabled_list,
        "disabled_platforms": st.flags.disabled_platforms(),
    }))
}

/// 平台/模型开关：POST /api/platforms
///   {"action":"toggle_platform","id":"web-glm","enabled":false}
///   {"action":"toggle_model","model":"glm-4-flash"}（不带 enabled = 取反）
/// 平台停用记在 flags.json（文件 spec 不就地改写）；模型停用按**裸模型名**记账。
async fn platform_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "toggle_platform" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            // 不存在的平台 id 直接 404：否则调用方拼错/误传会把垃圾名字永久写进
            // flags.json 的 disabled_platforms，越积越多且无从分辨。
            if !st.all_specs().iter().any(|s| s.id == id) {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "message": format!("平台 {id} 不存在")})),
                )
                    .into_response();
            }
            let cur = !st.flags.is_platform_disabled(id);
            let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(!cur);
            st.flags.set_platform_enabled(id, enabled);
            invalidate_models_cache(&st);
            Json(json!({"ok": true, "id": id, "enabled": enabled})).into_response()
        }
        "toggle_model" => {
            let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("");
            if model.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 model"}))).into_response();
            }
            // 与凭据同理：模型名再长也长不过「模型@平台」的合法形态，超长只可能是粘贴错误。
            if model.chars().count() > MAX_MODEL_NAME_LEN {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": format!("模型名过长（{} 字符，上限 {}）", model.chars().count(), MAX_MODEL_NAME_LEN)})),
                )
                    .into_response();
            }
            // 与 toggle_platform 对称：不认识的模型名直接 404。
            // 否则拼错的模型名会被永久写进 flags.json 的 disabled_models —— 既永远
            // 匹配不到任何请求，又会在「已停用模型」列表里越积越多、无从分辨。
            if !model_known(&st, model) {
                // ★ 别把用户输入整段回显：实测 9000 字符的模型名会原样进 404 文案，
                //   面板日志/浏览器控制台/客户端错误框都会跟着放大一遍。截断即可。
                let shown: String = model.chars().take(60).collect();
                let ellipsis = if model.chars().count() > 60 { "…" } else { "" };
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "message": format!("模型 {shown}{ellipsis} 不存在：没有任何平台声明它")})),
                )
                    .into_response();
            }
            let cur = !st.flags.is_model_disabled(model);
            let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(!cur);
            st.flags.set_model_enabled(model, enabled);
            invalidate_models_cache(&st);
            Json(json!({
                "ok": true,
                "model": aigw_store::bare_model(model),
                "enabled": enabled,
            }))
            .into_response()
        }
        other => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{other}")}))).into_response(),
    }
}

/// 自定义 API 列表（掩码 key）。
async fn custom_list(State(st): State<AppState>) -> Json<Value> {
    Json(st.custom.snapshot())
}

/// 单条自定义 API 的对外形状：**必须与 CustomStore::snapshot 的掩码口径一致**。
///
/// 为什么单独写：add/update 过去直接把整个 CustomApi 序列化回给调用方，
/// 里面带着明文 api_key（aigw_store::CustomApi 的 api_key 字段是原值），
/// 于是「保存成功」的响应体里就出现了用户刚填的 Key —— 面板会把响应打到
/// 浏览器控制台/日志里，等于把凭据又抄了一份。列表接口一直是掩码的，
/// 只有这两个写接口漏了。
fn custom_public(c: &aigw_store::CustomApi) -> Value {
    json!({
        "id": c.id,
        "name": c.name,
        "format": c.format,
        "base_url": c.base_url,
        // 解不开的 Key 不显示掩码（掩码会让人以为「Key 还在，只是看不见」），直接说要重填。
        "api_key_mask": if c.key_broken { "需重新填写".to_string() } else { aigw_store::mask_key(&c.api_key) },
        "models": c.models,
        "models_pinned": c.models_pinned,
        "enabled": c.enabled,
        "key_broken": c.key_broken,
    })
}

/// 自定义 API 写操作：
///   {"action":"add","name","format","base_url","api_key"}
///   {"action":"update","id","name?","format?","base_url?","api_key?"}
///   {"action":"remove","id"}
///   {"action":"sync_models","id","models":[..]}
/// 服务端拉取自定义 API 的模型清单（保存后自动执行）。
///
/// 为什么必须在服务端做：前端 `fetch('{base}/models')` 是跨域请求，绝大多数
/// 上游不带 CORS 头，浏览器会直接拦掉响应——前端那条「同步模型」等于从来没成功过。
/// 服务端还有统一代理、超时和凭据处理，拉回来的清单能直接落进 custom.json。
async fn sync_custom_models(st: &AppState, c: &aigw_store::CustomApi) -> Result<usize, String> {
    let spec = custom_to_spec(c);
    let ids = match st.client.fetch_models_with_secret(&spec, &c.api_key).await {
        Ok(ids) => ids,
        Err(e) => {
            // ★ 失败不许抹掉已有清单：自动同步是「锦上添花」，不是「权威重置」。
            //   旧实现无论成功失败都只用返回值 0 表示，调用方无从分辨，于是上游一次
            //   超时/502 就可能让用户眼里的清单莫名其妙缩水（本机桥在启动过程中回过
            //   半份清单，就是靠这条路径固化的）。保留旧清单 + 把原因说清楚，用户才能判断。
            tracing::warn!("自定义 API {} 拉取模型失败，沿用旧清单：{e}", c.id);
            return Err(format!(
                "拉取上游模型清单失败（HTTP {}）：{}",
                e.status,
                e.body.chars().take(200).collect::<String>()
            ));
        }
    };
    if ids.is_empty() {
        // 上游 200 但清单为空：同样不动旧清单（空的清单对用户没有任何好处，
        // 只会让 /v1/models 里这个平台上的一大片模型突然消失）。
        return Err("上游返回了空的模型清单".to_string());
    }
    // 自动同步代表「跟随上游」语义：写完清单同时解除固定。
    if let Err(e) = st.custom.set_models_pinned(&c.id, ids.clone(), false) {
        tracing::warn!("自定义 API {} 模型清单落库失败：{e}", c.id);
        return Err(format!("模型清单落库失败：{e}"));
    }
    Ok(ids.len())
}

async fn custom_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "add" => {
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let format = v.get("format").and_then(|x| x.as_str()).unwrap_or("openai");
            let base_url = v.get("base_url").and_then(|x| x.as_str()).unwrap_or("");
            let api_key = v.get("api_key").and_then(|x| x.as_str()).unwrap_or("");
            // ★ 2026-10-26（用户排队消息①）：anthropic 已支持原生转发
            //   （/v1/messages 透传 + /v1/chat/completions 协议转换），
            //   只有 responses 等其他协议仍挡在保存这一步。
            if format != "openai" && format != "anthropic" {
                return (StatusCode::BAD_REQUEST, Json(json!({
                    "ok": false,
                    "message": "协议仅支持 openai / anthropic（Responses 等其他协议请填各自的 OpenAI 兼容网关地址）",
                }))).into_response();
            }
            // 上游指向云元数据端点：请求会带着用户填的 Key 打到元数据服务，属 SSRF 提权。
            // 回环与局域网地址有意放行——「接本机另一个 OpenAI 兼容网关」是正常用法（本面板
            // 自己那条 18899 的桥就是回环），一刀切封内网会把它打死。
            let ch = url_host(base_url);
            if blocked_upstream_host(&ch) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": format!("base_url 的主机 {} 是云元数据端点，不能作为上游（会把你的 Key 送到元数据服务）", ch)})),
                )
                    .into_response();
            }
            match st.custom.add(name, format, base_url, api_key) {
                Ok(c) => {
                    let (synced, sync_err) = match sync_custom_models(&st, &c).await {
                        Ok(n) => (n, Value::Null),
                        Err(e) => (0, json!(e)),
                    };
                    invalidate_models_cache(&st);
                    // 条目已经存下了（用户填的 URL/Key 有效），只是清单没拉到 —— 这时
                    // 报 400 会让用户以为没保存成功。如实说「已保存，但没拉到清单」。
                    Json(json!({"ok": true, "api": custom_public(&c), "models_synced": synced, "models_sync_error": sync_err})).into_response()
                }
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        "update" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let format = v.get("format").and_then(|x| x.as_str()).unwrap_or("");
            let base_url = v.get("base_url").and_then(|x| x.as_str()).unwrap_or("");
            let api_key = v.get("api_key").and_then(|x| x.as_str()).unwrap_or("");
            // 与 add 分支保持同一道闸：responses 等其他协议仍不收（anthropic 已放行，
            // 见 add 分支同日注释）。只堵 add 不堵 update，等于留了个后门。
            if !format.is_empty() && format != "openai" && format != "anthropic" {
                return (StatusCode::BAD_REQUEST, Json(json!({
                    "ok": false,
                    "message": "协议仅支持 openai / anthropic",
                }))).into_response();
            }
            // 与 add 同一道闸：只拦 add 不拦 update 等于留后门。
            let ch2 = url_host(base_url);
            if !base_url.trim().is_empty() && blocked_upstream_host(&ch2) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": format!("base_url 的主机 {} 是云元数据端点，不能作为上游（会把你的 Key 送到元数据服务）", ch2)})),
                )
                    .into_response();
            }
            match st.custom.update(id, name, format, base_url, api_key) {
                Ok(c) => {
                    // 固定过清单的条目不自动重拉：用户刚手动收敛好的白名单，
                    // 一刷新就冲回上游全量（死模型全回来），等于白干。
                    let (synced, sync_err) = if c.models_pinned {
                        // 清单被固定：本来就不重拉，不是失败，别报错。
                        (0, Value::Null)
                    } else {
                        match sync_custom_models(&st, &c).await {
                            Ok(n) => (n, Value::Null),
                            Err(e) => (0, json!(e)),
                        }
                    };
                    invalidate_models_cache(&st);
                    Json(json!({"ok": true, "api": custom_public(&c), "models_synced": synced, "models_sync_error": sync_err})).into_response()
                }
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        "toggle" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let mut enabled = v.get("enabled").and_then(|x| x.as_bool());
            if enabled.is_none() {
                enabled = st.custom.rows().iter().find(|c| c.id == id).map(|c| !c.enabled);
            }
            let Some(enabled) = enabled else {
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": "条目不存在"}))).into_response();
            };
            if !st.custom.set_enabled(id, enabled) {
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": "条目不存在"}))).into_response();
            }
            invalidate_models_cache(&st);
            if !enabled {
                st.pool.forget_account(&format!("{id}:key"));
                st.sticky.unbind_account(&format!("{id}:key"));
            }
            Json(json!({"ok": true, "enabled": enabled})).into_response()
        }
        "remove" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let removed = st.custom.remove(id);
            if removed {
                invalidate_models_cache(&st);
            }
            Json(json!({"ok": removed, "message": if removed { "已删除" } else { "条目不存在" }})).into_response()
        }
        "sync_models" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let models: Vec<String> = v
                .get("models")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            // 前端不传 models 时由服务端去上游拉：浏览器跨域拿不到 /models
            let models = if models.is_empty() {
                match st.custom.rows().into_iter().find(|c| c.id == id) {
                    Some(c) => {
                        // 手动同步的语义是「用户明确要求刷新」：失败必须如实报出来
                        // （带上游状态码与原因），而不是一句笼统的「没有返回清单」。
                        // 分支统一 return，让整个 if/else 的类型仍由 else 那条 Vec<String> 决定。
                        return match sync_custom_models(&st, &c).await {
                            Ok(n) => {
                                invalidate_models_cache(&st);
                                Json(json!({"ok": true, "message": format!("已同步 {n} 个模型")}))
                                    .into_response()
                            }
                            Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({
                                "ok": false,
                                "message": format!("同步失败，已保留原有清单：{e}"),
                            })))
                            .into_response(),
                        };
                    }
                    None => {
                        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "自定义 API 不存在"}))).into_response()
                    }
                }
            } else {
                models
            };
            match st.custom.set_models_pinned(id, models.clone(), true) {
                Ok(()) => {
                    invalidate_models_cache(&st);
                    Json(json!({"ok": true, "message": format!(
                        "模型清单已固定（{} 个）：/v1/models 只发布这份清单；点「同步模型」（不带清单）可恢复跟随上游",
                        models.len()
                    )})).into_response()
                }
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{action}")}))).into_response(),
    }
}

// ---- 公益站聚合（手动刷新 + 每日自动） ----

/// 聚合源（静态导航页，页面内嵌 JSON-LD itemListElement：name+url）。
///
/// 第一个是正式站（GitHub Pages），第二个是**同一仓库**在 raw.githubusercontent.com
/// 上的原始 index.html（实测同样含 itemListElement）。两个源分属不同域名/不同 CDN，
/// Pages 构建抖动、证书过期、自定义域名改绑时第二个仍能刷新 —— 旧实现只有第一个，
/// 它一挂「公益站刷新」就整轮失败，只能等聚合站自己恢复。
///
/// 顺序即优先级：按顺序尝试，先成功的那份生效（失败明细进任务台账与 /api/free）。
const GONGYI_SOURCES: &[&str] = &[
    "https://bubblevv.github.io/ai-api-gongyi-nav/",
    "https://raw.githubusercontent.com/bubblevv/ai-api-gongyi-nav/main/index.html",
];

/// 追加自定义聚合源：`data/gongyi-sources.json`（`{"urls":["https://…"]}` 或裸数组）。
///
/// 内置源只收验证过含 itemListElement 的地址 —— 往代码里塞未经核对的镜像，
/// 只会在真出故障时一起 404。用户自己的镜像/内网静态页写这个文件即可，追加在内置源之后。
const GONGYI_SOURCES_FILE: &str = "gongyi-sources.json";

/// 聚合源数量上限：串行尝试、每源 30s 超时，源太多会把一次刷新拖成几分钟。
const MAX_GONGYI_SOURCES: usize = 6;

/// 单批公益站上限：聚合页是外部输入，被投毒塞几千条会把 /v1/models 与
/// 路由表撑爆，也让面板变成别人的扫描器。
const MAX_GONGYI_SITES: usize = 200;

/// JSON-LD ListItem（聚合页内嵌）。
#[derive(Debug, serde::Deserialize)]
struct GongyiItem {
    name: String,
    #[serde(default)]
    url: String,
}

/// 公益站 host 准入：聚合页是外部不可信输入，站点 host 会被直接当成上游。
///
/// 挡三类：① 云元数据端点（面板会带着用户填的 Key 去请求它，等于把 Key 与
/// 云主机临时凭据一起交出去）；② 本机/回环（会形成自指回环：面板转发给自己，
/// 再拉自己的 /v1/models，放大成请求风暴）；③ 明显不是公网域名的畸形 host。
/// 内网上游有意不拦 —— 用户自己填的自定义 API 是另一条路径，走 custom_action。
pub fn gongyi_host_ok(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').to_lowercase();
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    if blocked_upstream_host(&h) {
        return false;
    }
    const LOOPBACK: &[&str] = &[
        "localhost",
        "127.0.0.1",
        "::1",
        "0.0.0.0",
        "[::1]",
    ];
    if LOOPBACK.iter().any(|x| *x == h) {
        return false;
    }
    // 公网域名至少有一个点；没有点的主机名（`api`、`localhost`）不该被当成上游
    if !h.contains('.') {
        return false;
    }
    for seg in h.split('.') {
        if seg.is_empty() || seg.len() > 63 {
            return false;
        }
        if !seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
    }
    true
}
/// 从 host 提取 id 用 slug（去 www.，非法字符转 -）。
fn slug_of(host: &str) -> String {
    let h = host.trim_start_matches("www.");
    let mut s = String::new();
    for ch in h.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            s.push(ch);
        } else {
            s.push('-');
        }
    }
    s.trim_matches('-').to_string()
}

/// 从 URL 提取 host（https://host/path?a=b → host）。
fn host_of(u: &str) -> Option<String> {
    let rest = u.split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.to_string())
}

/// URL 的 scheme 白名单：只认 http/https。
///
/// `host_of` 按第一个 `://` 切主机，挡不住 `javascript://evil.com/%0Aalert(1)`——
/// 主机部分是完全合规的 evil.com，但整个串是可执行的 javascript: URL（浏览器先做
/// 百分号解码，%0A 换行后 //evil.com/ 只是注释，alert(1) 照跑）。这种串一旦作为
/// 公益站的 url 入库，就会经 login_url 流进前端的 <a href>，点一下「注册页」即在
/// 面板源上执行任意 JS。所以入口处先钉死 scheme，不能只靠 host 检查兜底。
fn scheme_ok(u: &str) -> bool {
    let low = u.trim().to_ascii_lowercase();
    low.starts_with("https://") || low.starts_with("http://")
}

/// 聚合源 URL 的合规检查：只收 http(s)、主机可解析、且不是云元数据端点。
///
/// **回环放行**：自建镜像常见于本机 https://127.0.0.1:8443，那是用户自己的静态页，
/// 与「聚合页被投毒塞进来一个站」不是一回事（后者由 `gongyi_host_ok` 单独拦）。
fn gongyi_source_ok(raw: &str) -> Option<String> {
    let u = raw.trim();
    if u.is_empty() {
        return None;
    }
    let low = u.to_ascii_lowercase();
    if !(low.starts_with("https://") || low.starts_with("http://")) {
        return None;
    }
    let h = url_host(u);
    if h.is_empty() || blocked_upstream_host(&h) {
        return None;
    }
    Some(u.to_string())
}

/// URL → 便于阅读的主机名（解析不出时退回原串）。
fn short_host(u: &str) -> String {
    let h = url_host(u);
    if h.is_empty() {
        u.to_string()
    } else {
        h
    }
}

/// 本轮生效的聚合源列表：内置源 + `data/gongyi-sources.json` 追加的源（去重后截断）。
///
/// 配置文件损坏时**不**让刷新失败 —— 只用内置源并留一条 warn：一个写错逗号的
/// JSON 不该让公益站整轮刷不出来。
pub fn gongyi_source_list(data_dir: Option<&std::path::Path>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in GONGYI_SOURCES {
        match gongyi_source_ok(u) {
            Some(u) => out.push(u),
            None => tracing::warn!("内置聚合源不合规，已跳过：{u}"),
        }
    }
    if let Some(dir) = data_dir {
        match std::fs::read_to_string(dir.join(GONGYI_SOURCES_FILE)) {
            // Windows 记事本/PowerShell 5 写文件默认带 UTF-8 BOM：serde_json
            // 见到 \u{feff} 直接报 unexpected character，于是整份配置被静默忽略
            // （实测就是这条：用户写了聚合源，面板上源列表一个都没多）。先剥 BOM。
            Ok(txt) => match serde_json::from_str::<Value>(txt.trim_start_matches('\u{feff}')) {
                Ok(v) => {
                    let arr = v
                        .as_array()
                        .or_else(|| v.get("urls").and_then(|x| x.as_array()))
                        .cloned()
                        .unwrap_or_default();
                    if arr.is_empty() {
                        tracing::warn!(
                            "{} 里没有可用地址（应是数组或 {{\"urls\":[…]}}），本轮只用内置聚合源",
                            GONGYI_SOURCES_FILE
                        );
                    }
                    for it in arr {
                        let Some(raw) = it.as_str() else {
                            tracing::warn!(
                                "{} 里有非字符串条目，已忽略：{it}",
                                GONGYI_SOURCES_FILE
                            );
                            continue;
                        };
                        match gongyi_source_ok(raw) {
                            Some(u) => out.push(u),
                            None => tracing::warn!(
                                "{} 里的聚合源不合规（只收 http/https，且不能是云元数据端点），已忽略：{raw}",
                                GONGYI_SOURCES_FILE
                            ),
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    "{} 解析失败（本轮只用内置聚合源）：{e}",
                    GONGYI_SOURCES_FILE
                ),
            },
            Err(_) => {}
        }
    }
    let mut seen: Vec<String> = Vec::new();
    out.retain(|u| {
        let k = u.trim_end_matches('/').to_ascii_lowercase();
        if seen.iter().any(|x| x == &k) {
            return false;
        }
        seen.push(k);
        true
    });
    out.truncate(MAX_GONGYI_SOURCES);
    out
}

/// 从聚合页 HTML 里抽出 JSON-LD `itemListElement` 数组（括号配对，兼容内部嵌套）。
fn parse_gongyi_items(html: &str) -> Result<Vec<GongyiItem>, String> {
    let start = html
        .find("\"itemListElement\"")
        .ok_or("聚合页未找到 itemListElement（聚合站改版或返回了错误页）")?;
    let rest = &html[start + "\"itemListElement\"".len()..];
    let colon = rest.find(':').ok_or("itemListElement 缺冒号")?;
    let arr = &rest[colon + 1..];
    let arr = arr.trim_start();
    if !arr.starts_with('[') {
        return Err("itemListElement 不是数组".into());
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = 0usize;
    for (i, ch) in arr.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match ch {
            '\\' if in_str => esc = true,
            '"' => in_str = !in_str,
            '[' => {
                if !in_str {
                    depth += 1
                }
            }
            ']' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    if end == 0 {
        return Err("itemListElement 数组未闭合".into());
    }
    serde_json::from_str(&arr[..end]).map_err(|e| format!("解析站点列表失败：{e}"))
}

/// 单个聚合源的尝试次数。
///
/// 实测：服务刚起来的那一轮刷新，两个源会**同时**报「请求失败」（DNS/TLS 还没热），
/// 几十秒后手动刷新就正常了。不重试的话，每次重启面板上都会留下一次「全源失败」的
/// 假故障。两次重试（0.25s / 0.5s 退避）足以盖住这种瞬时抖动。
const GONGYI_FETCH_TRIES: usize = 3;

/// 「近 24h 平台健康」的统计窗口（/api/platforms 的 recent 字段）。
///
/// 一天是刻意的：公益站/免费车道被封、上游改反爬之后，失败会立刻集中出现，而
/// 一周前的历史失败没有决策价值（反而会让刚恢复的平台一直挂着红标）。
const PLATFORM_HEALTH_WINDOW_S: f64 = 24.0 * 3600.0;

/// 每个接入源「最近一次失败」的原始原因（面板接入源卡片用）。
///
/// usage 台账只有 ok/failed 计数：一个公益站挂了两周，面板只会显示「近24h 失败 40%」，
/// 用户完全看不到是 **401 凭据过期**、**402 额度耗尽** 还是 **域名解析不了** —— 这三种
/// 的处理方式完全不同（粘贴 Key / 换站 / 查 DNS）。出错那一刻的信息只在响应体里，
/// 不记下来就永久消失，所以在这里留一份（按 spec id 一条，只保留最近一次）。
pub struct LastErrors {
    inner: Mutex<HashMap<String, ErrNote>>,
}

/// 一条「最近一次失败」。
#[derive(Debug, Clone)]
pub struct ErrNote {
    /// unix 秒。
    pub at: u64,
    /// 上游 HTTP 状态；0 = 连接层失败（DNS/拒绝连接/超时），本地错误记调用方状态。
    pub status: u16,
    /// 上游原话（已压缩/截断）。
    pub text: String,
}

/// 记下来的原因最长多少字符：上游有时回一整页 HTML，全存下来会让面板卡片爆掉。
const LAST_ERR_MAX_CHARS: usize = 300;

/// 主动「验活」失败时的统一说法（既做响应文案，也做平台卡片上的失败原因）。
///
/// 验活只回 bool（各平台 wrapper 的端点不同，具体原因拿不到），所以这里只能说
/// 「凭据可能已过期」这一句；对 cookie 平台补一句该怎么做，避免用户去翻日志。
const VERIFY_FAIL_MSG: &str = "验活未通过：凭据可能已过期（Cookie 平台请重新登录后复制完整字段）";

impl Default for LastErrors {
    fn default() -> Self {
        Self::new()
    }
}

impl LastErrors {
    pub fn new() -> LastErrors {
        LastErrors {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 记一次失败（空 id 忽略：没有平台就无处归属）。
    pub fn note(&self, id: &str, status: u16, text: &str) {
        let id = id.trim();
        if id.is_empty() {
            return;
        }
        let t = compact_err_text(text);
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(
            id.to_string(),
            ErrNote {
                at: aigw_store::now_unix() as u64,
                status,
                text: t,
            },
        );
    }

    /// 这个平台成功了一次：旧的失败原因不再代表现状，必须清掉。
    ///
    /// 不注意这点就会出现「刚跑通的车道卡片上还挂着上周的 402」——
    /// 比没有信息更糟，因为它会让人去修一个已经不存在的问题。
    pub fn clear(&self, id: &str) {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(id);
    }

    /// 面板展示用：at / s_ago / status / text；没有记录返回 Null。
    pub fn json(&self, id: &str) -> Value {
        let now = aigw_store::now_unix() as u64;
        let m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(id) {
            Some(n) => json!({
                "at": n.at,
                "s_ago": now.saturating_sub(n.at),
                "status": n.status,
                "text": n.text,
            }),
            None => Value::Null,
        }
    }
}

/// 把上游错误文本压成一行可读的原因：能解析成 JSON 就取 message 字段，
/// 否则取原文；换行压成空格；超长截断。
fn compact_err_text(raw: &str) -> String {
    let raw = raw.trim();
    let mut t = raw.to_string();
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        for path in [
            ["error", "message"],
            ["error", "msg"],
            ["message", ""],
            ["msg", ""],
            ["detail", ""],
        ] {
            let mut cur = &v;
            let mut hit = true;
            for k in path.iter().filter(|k| !k.is_empty()) {
                match cur.get(*k) {
                    Some(n) => cur = n,
                    None => {
                        hit = false;
                        break;
                    }
                }
            }
            if hit {
                if let Some(x) = cur.as_str() {
                    if !x.trim().is_empty() {
                        t = x.trim().to_string();
                        break;
                    }
                }
            }
        }
    }
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.is_empty() {
        return "（上游没有给出原因）".to_string();
    }
    let mut out: String = one.chars().take(LAST_ERR_MAX_CHARS).collect();
    if one.chars().count() > LAST_ERR_MAX_CHARS {
        out.push('…');
    }
    out
}

/// 从转发错误里取「给人看的原因」。
fn chat_err_text(e: &ChatCoreErr) -> String {
    match e {
        ChatCoreErr::Local { message, .. } => message.clone(),
        ChatCoreErr::Upstream { body, .. } => body.clone(),
    }
}

/// 把**流式**结果写进「最近失败原因」台账。
///
/// 流式的结局不是主循环拿到的：网关把 body 交出去之后就返回了 Ok，成败要等后台
/// 统计任务从 stat_rx 里读。少了这一步，流式失败（真机上最常见的一类）在卡片上
/// 只剩「近24h 失败 N 次」，说不出上游原话。
fn note_stream_outcome(le: &LastErrors, spec_id: &str, error: Option<ErrorClass>, msg: &str) {
    match error {
        None => le.clear(spec_id),
        Some(cl) => {
            // 流里没有 HTTP 状态码：status 记 0，面板据此不显示「HTTP xxx」。
            let text = if msg.trim().is_empty() {
                format!("流式响应中途失败（上游断开或回了错误帧；分类 {cl:?}）")
            } else {
                msg.to_string()
            };
            le.note(spec_id, 0, &text);
        }
    }
}


/// 单次抓取：HTTP 状态也要查（改版页/错误页常是 200，但 404 页必然没有列表）。
/// 第二个返回值表示「是否值得重试」：4xx 多半是地址写错了，重试只是浪费时间。
async fn fetch_one_source_once(
    client: &aigw_relay::OutClient,
    url: &str,
) -> (Result<(Vec<aigw_store::FreeSite>, GongyiStats), String>, bool) {
    let resp = match client
        .http()
        .get(url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return (Err(format!("请求失败：{e}")), true),
    };
    let status = resp.status();
    if !status.is_success() {
        let retry = status.as_u16() == 429 || status.is_server_error();
        return (Err(format!("HTTP {status}")), retry);
    }
    let html = match resp.text().await {
        Ok(h) => h,
        Err(e) => return (Err(format!("读取响应失败：{e}")), true),
    };
    (parse_gongyi_items(&html).map(gongyi_items_to_sites), false)
}

/// 带重试的抓取。最终报错里带「已重试 N 次」，让面板上分得清
/// 「一次就 404」和「反复失败」。
async fn fetch_one_source(
    client: &aigw_relay::OutClient,
    url: &str,
) -> Result<(Vec<aigw_store::FreeSite>, GongyiStats), String> {
    let mut last = String::new();
    let mut tries = 0usize;
    for i in 0..GONGYI_FETCH_TRIES {
        if i > 0 {
            let backoff = 250u64 << (i - 1).min(3);
            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        }
        tries = i + 1;
        let (r, retryable) = fetch_one_source_once(client, url).await;
        match r {
            Ok(v) => return Ok(v),
            Err(e) => {
                last = e;
                if !retryable {
                    break;
                }
            }
        }
    }
    if tries > 1 {
        Err(format!("{last}（已重试 {} 次）", tries - 1))
    } else {
        Err(last)
    }
}

/// 按顺序回退：第一个成功的源生效，失败源的明细全部保留。
///
/// 抽成纯函数是为了能单测「首源失败→次源命中」「全失败时的报错文案」——
/// 这两条正是多源回退真正会走的分支，靠外网写集成测试既慢又不稳。
fn combine_gongyi_attempts(
    attempts: Vec<(String, Result<(Vec<aigw_store::FreeSite>, GongyiStats), String>)>,
) -> Result<(Vec<aigw_store::FreeSite>, GongyiStats), String> {
    if attempts.is_empty() {
        return Err(format!(
            "没有可用的聚合源：内置源为空且 data/{GONGYI_SOURCES_FILE} 里没有合规地址"
        ));
    }
    let mut tried: Vec<SourceStat> = Vec::new();
    for (url, r) in attempts {
        match r {
            Ok((sites, mut st)) => {
                tried.push(SourceStat {
                    url,
                    ok: true,
                    items: sites.len(),
                    err: String::new(),
                });
                st.sources = tried;
                return Ok((sites, st));
            }
            Err(e) => tried.push(SourceStat {
                url,
                ok: false,
                items: 0,
                err: e,
            }),
        }
    }
    let detail = tried
        .iter()
        .map(|s| format!("{}（{}）", short_host(&s.url), s.err))
        .collect::<Vec<_>>()
        .join("；");
    Err(format!(
        "全部 {} 个聚合源都失败：{detail}",
        tried.len()
    ))
}

/// 抓取聚合源并解析出站点列表（按 host 去重，首个保留）。
///
/// 多源回退：按 `sources` 顺序串行尝试，第一个成功的生效；全部失败才返回 Err。
pub async fn fetch_gongyi_sites(
    client: &aigw_relay::OutClient,
    sources: &[String],
) -> Result<(Vec<aigw_store::FreeSite>, GongyiStats), String> {
    let mut attempts = Vec::new();
    for url in sources {
        let r = fetch_one_source(client, url).await;
        let failed = r.is_err();
        if let Err(e) = &r {
            tracing::warn!("聚合源 {url} 失败：{e}");
        }
        attempts.push((url.clone(), r));
        if !failed {
            break;
        }
    }
    combine_gongyi_attempts(attempts)
}

/// 一轮聚合刷新里「为什么少了这么多站」的明细。
///
/// 只把 counts 写进 tracing 日志的话，面板上只能看到「共 N 站」，看不出这个 N 是
/// 「聚合页本来只有 N 条」还是「聚合页给了 60 条、58 条被筛掉」——后者意味着上游
/// 站点集体挂掉或聚合页被投毒，必须能直接看见。
/// 单个聚合源的尝试结果（多源回退的可解释性：到底是谁失败、谁命中）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStat {
    pub url: String,
    /// 该源是否取数成功（成功即本轮生效，后面的源不会再试）。
    pub ok: bool,
    /// 成功时入库的站点条数。
    pub items: usize,
    /// 失败原因（HTTP 状态 / 解析错误 / 网络错误）；成功时为空。
    pub err: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GongyiStats {
    /// 聚合页给出的条目总数。
    pub items: usize,
    /// host 不合规（云元数据 / 回环 / 畸形域名 / 空 url）被跳过。
    pub bad_host: usize,
    /// host 重复（聚合页常见重复条目）。
    pub dup_host: usize,
    /// 触达 MAX_GONGYI_SITES 上限后丢弃。
    pub over_cap: usize,
    /// slug 撞车导致 id 冲突丢弃。
    pub id_conflict: usize,
    /// 与内置 `providers/free-*.toml` 同域，因此没有入库（卡片用内置那一条）。
    ///
    /// 单独计数而不是并进 `dropped()`：这些条目本身没坏，丢掉它们只是为了避免
    /// 同一个站在面板上出现两张卡片。混进「丢弃」会让扣减看起来像上游出了问题。
    pub shadowed: usize,
    /// 本轮尝试过的聚合源（按尝试顺序；最后一个 `ok` 的即生效源）。
    pub sources: Vec<SourceStat>,
}

impl GongyiStats {
    /// 被丢弃的条目总数。
    pub fn dropped(&self) -> usize {
        self.bad_host + self.dup_host + self.over_cap + self.id_conflict
    }

    /// 一行明细，写进任务台账与 /api/free 的响应。计数为 0 的项不占字数。
    pub fn detail(&self) -> String {
        let shadow = if self.shadowed > 0 {
            format!("，另有 {} 个与内置接入源同域（已并入内置条目）", self.shadowed)
        } else {
            String::new()
        };
        if self.dropped() == 0 {
            return format!("聚合页 {} 条，无丢弃{}", self.items, shadow);
        }
        let mut parts: Vec<String> = Vec::new();
        if self.bad_host > 0 {
            parts.push(format!("host 不合规 {}", self.bad_host));
        }
        if self.dup_host > 0 {
            parts.push(format!("重复 {}", self.dup_host));
        }
        if self.over_cap > 0 {
            parts.push(format!("超上限丢弃 {}", self.over_cap));
        }
        if self.id_conflict > 0 {
            parts.push(format!("id 撞车 {}", self.id_conflict));
        }
        format!(
            "聚合页 {} 条，丢弃 {}（{}）{}",
            self.items,
            self.dropped(),
            parts.join("，"),
            shadow
        )
    }

    /// 多源回退的一句话说明：'聚合源 X 命中' / '聚合源 A 失败（…），由 B 命中'。
    ///
    /// 只在真有多个源参与时才值得写；单源命中不占字数（`detail()` 已经说了条数）。
    pub fn sources_note(&self) -> String {
        if self.sources.is_empty() {
            return String::new();
        }
        let Some(hit) = self.sources.iter().position(|s| s.ok) else {
            return String::new();
        };
        if hit == 0 {
            // 单源也是这个源：写出来才能回答「今天用的是正式站还是回退源」——
            // 只写「无丢弃」时，回退源长期接管这件事在面板上看不出来。
            return format!("聚合源 {} 命中", short_host(&self.sources[0].url));
        }
        let failed: Vec<String> = self.sources[..hit]
            .iter()
            .map(|s| format!("{}（{}）", short_host(&s.url), s.err))
            .collect();
        format!(
            "聚合源 {} 失败（{}），由 {} 命中",
            failed.len(),
            failed.join("；"),
            short_host(&self.sources[hit].url)
        )
    }
}

/// JSON-LD 条目 → 公益站条目（纯函数，便于单测）。
///
/// 去重两条线：host 完全相同（聚合页常见重复条目）与 **id 相同**
/// （slug 只保留 [a-z0-9-]，不同 host 可能映射到同一 slug）。
/// 返回 (条目, 明细)：明细让「聚合页给了 60 条但只入库 2 条」在面板上可解释。
fn gongyi_items_to_sites(items: Vec<GongyiItem>) -> (Vec<aigw_store::FreeSite>, GongyiStats) {
    let total = items.len();
    let mut st = GongyiStats {
        items: total,
        ..Default::default()
    };
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<aigw_store::FreeSite> = Vec::new();
    // id 也必须去重：slug 只保留 [a-z0-9-]，不同的 host 完全可能映射到同一个 slug
    // （例如 `api.a-b.com` 与 `api-a-b.com` 都变成 `api-a-b-com`）。旧实现只按 host
    // 去重，于是同一批里出现两条同 id 的站点，upsert_batch 会把后一条当成「已存在」
    // 覆盖前一条的 base_url —— 刷新一次，某个站点就静默指向了另一个域名。
    let mut seen_ids: Vec<String> = Vec::new();
    for (idx, it) in items.into_iter().enumerate() {
        let u = it.url.trim().to_string();
        if !scheme_ok(&u) {
            // javascript://host/… 这类：host_of 能切出合规主机，但整个串是可执行
            // 脚本 URL，绝不能入库当注册页链接（见 scheme_ok 注释）。
            st.bad_host += 1;
            tracing::warn!(
                "公益站 URL 的 scheme 不是 http(s)，已跳过：{}",
                u.chars().take(80).collect::<String>()
            );
            continue;
        }
        let Some(host) = host_of(&u) else {
            // 空 url 或解析不出 host 的条目也算「不合规」：否则明细里 items 与
            // 入库+丢弃对不上，看的人会以为漏算了。
            st.bad_host += 1;
            continue;
        };
        if !gongyi_host_ok(&host) {
            st.bad_host += 1;
            tracing::warn!("公益站 host 不合规，已跳过：{}（来自 {}）", host, u);
            continue;
        }
        if out.len() >= MAX_GONGYI_SITES {
            // 剩余条目一次性丢弃：逐条 warn 会让一次投毒刷新刷满整个日志。
            st.over_cap = total - idx;
            tracing::warn!(
                "公益站数量已达上限 {}，忽略后续 {} 条条目",
                MAX_GONGYI_SITES,
                st.over_cap
            );
            break;
        }
        if seen.contains(&host) {
            st.dup_host += 1;
            continue;
        }
        seen.push(host.clone());
        let slug = slug_of(&host);
        if slug.is_empty() {
            st.bad_host += 1;
            continue;
        }
        let id = format!("free-{slug}");
        if seen_ids.iter().any(|x| x == &id) {
            st.id_conflict += 1;
            tracing::warn!(
                "公益站 id 冲突：{} 与已收录站点的 slug 都是 {}，跳过后者（host={host}）",
                it.name.trim(),
                id
            );
            continue;
        }
        seen_ids.push(id.clone());
        out.push(aigw_store::FreeSite {
            id,
            name: it.name.trim().to_string(),
            base_url: format!("https://{host}"),
            url: u,
            desc: String::new(),
            enabled: true,
        });
    }
    (out, st)
}

/// 公益站刷新的全局单飞闸门。
///
/// 手动点「⟳ 刷新公益站」和每日定时任务走的是同一条抓取链路：两个一起跑会
/// 同时打聚合源（几秒到几十秒）、同时 upsert，并互相覆盖 health 里那句
/// 「这一轮拿到了几个站」。用 try_lock 而不是 lock：并发的一方直接说清楚
/// 「上一次还在进行」，而不是排队等完再抓第二遍。
static FREE_REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 刷新结果：要么真跑完了，要么「上一轮还在进行中」被挡下。
pub enum FreeRefresh {
    Done(usize, usize, usize, GongyiStats),
    Busy,
}

/// 单飞版刷新：拿不到闸门就返回 Busy，不做重复抓取。
pub async fn refresh_free_sites_guarded(
    client: &aigw_relay::OutClient,
    free: &aigw_store::FreeStore,
    sources: &[String],
    static_hosts: &[String],
) -> Result<FreeRefresh, String> {
    let _gate = match FREE_REFRESH_LOCK.try_lock() {
        Ok(g) => g,
        Err(_) => return Ok(FreeRefresh::Busy),
    };
    let (total, added, kept, stats) = refresh_free_sites(client, free, sources, static_hosts).await?;
    Ok(FreeRefresh::Done(total, added, kept, stats))
}

/// 刷新公益站聚合：抓取（多源回退）→ 解析 → 跳过已被内置接入源覆盖的站 → 合并（已存在保留）。
/// 返回 (聚合总数, 新增数, 保留数, 丢弃明细)。
///
/// static_hosts 见 [static_free_hosts]：同域的动态条目不入库，否则同一个站会有
/// 两张卡片（id 不同 → upsert_batch 的按 id 去重拦不住）。
pub async fn refresh_free_sites(
    client: &aigw_relay::OutClient,
    free: &aigw_store::FreeStore,
    sources: &[String],
    static_hosts: &[String],
) -> Result<(usize, usize, usize, GongyiStats), String> {
    let (sites, mut stats) = fetch_gongyi_sites(client, sources).await?;
    let total = sites.len();
    // 与内置接入源同域的条目：不入库（内置那条已经在面板上了）。
    let (shadowed, keep): (Vec<_>, Vec<_>) = sites.into_iter().partition(|s| {
        let h = site_host(&s.base_url);
        !h.is_empty() && static_hosts.iter().any(|x| x == &h)
    });
    stats.shadowed = shadowed.len();
    if stats.shadowed > 0 {
        tracing::info!(
            "公益站刷新：{} 个条目与内置接入源同域，未重复入库（{}）",
            stats.shadowed,
            shadowed.iter().map(|s| s.base_url.clone()).collect::<Vec<_>>().join("，")
        );
    }
    let (added, kept) = free.upsert_batch(keep);
    Ok((total, added, kept, stats))
}

/// 被内置 `providers/free-*.toml` 同域覆盖、且名下没有账号的动态公益站 id。
///
/// 只隐藏「没账号」的行：有账号的条目被藏起来，用户会以为账号丢了，也无从解绑。
/// 隐藏而不是删除 —— 动态条目的 enabled / 账号状态是用户数据，而内置条目的 id 不同，
/// 删掉之后这些状态恢复不了。
fn shadowed_free_ids(st: &AppState) -> Vec<String> {
    let hosts = static_free_hosts(&st.specs);
    if hosts.is_empty() {
        return Vec::new();
    }
    st.free
        .rows()
        .into_iter()
        .filter(|s| {
            let h = site_host(&s.base_url);
            !h.is_empty()
                && hosts.iter().any(|x| x == &h)
                && st.accounts.secrets_for(&s.id).is_empty()
        })
        .map(|s| s.id)
        .collect()
}

/// GET /api/free → 公益站条目列表。
async fn free_list(State(st): State<AppState>) -> Json<Value> {
    // 本轮会用哪些聚合源也一并下发：刷新失败时用户要能一眼看到「是不是只有内置那一个源」，
    // 而不是去翻代码找 GONGYI_SOURCES。
    let sources = gongyi_source_list(st.data_dir.as_deref());
    // 与内置接入源同域的动态条目：不重复展示（否则同一个站两张卡片）。
    // 仍然保留在 free_sites.json 里：用户可能已经给它绑过账号 / 改过开关。
    let shadowed = shadowed_free_ids(&st);
    let rows = st.free.rows();
    let list: Vec<Value> = rows
        .iter()
        .filter(|s| !shadowed.contains(&s.id))
        .map(|s| {
            json!({
                "id": s.id, "name": s.name, "base_url": s.base_url,
                "url": s.url, "desc": s.desc, "enabled": s.enabled,
                // 与 /api/platforms 同一份「最近一次失败原因」：公益站卡片也直接显示，
                // 不必让前端再去 platforms 里按 id 找一遍。
                "last_error": st.last_err.json(&s.id),
            })
        })
        .collect();
    // 上次刷新的结果（多源回退命中/全失败、丢弃明细）一并下发：
    // 面板要能说清「今天这些站是从哪个源拿到的、备用源有没有被用到」，
    // 否则用户只能靠刷新那一刻的 alert 记住，关掉就没了。
    let last_refresh = st
        .tasks
        .snapshot()
        .get("free_refresh")
        .cloned()
        .unwrap_or(Value::Null);
    Json(json!({
        "total": list.len(),
        "rows": list,
        "sources": sources,
        "sources_file": GONGYI_SOURCES_FILE,
        // 被内置接入源同域覆盖、因此没在这里列出的条目数：面板要能解释
        // 「聚合页 47 条，这里只显示 46 条」的差额。
        "shadowed": shadowed.len(),
        "shadowed_ids": shadowed,
        "last_refresh": last_refresh,
    }))
}

/// POST /api/free：
///   {"action":"refresh"}            手动刷新聚合源
///   {"action":"remove","id":"free-…"} 删除站点
async fn free_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "toggle" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let cur = st.free.rows().iter().find(|r| r.id == id).map(|r| r.enabled);
            let Some(cur) = cur else {
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": "站点不存在"}))).into_response();
            };
            let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(!cur);
            st.free.set_enabled(id, enabled);
            invalidate_models_cache(&st);
            Json(json!({"ok": true, "id": id, "enabled": enabled})).into_response()
        }
        "refresh" => {
            // 聚合源每次现算（配置文件可随时改，不必重启）。
            let sources = gongyi_source_list(st.data_dir.as_deref());
            let static_hosts = static_free_hosts(&st.specs);
            match refresh_free_sites_guarded(&st.client, &st.free, &sources, &static_hosts).await {
                Ok(FreeRefresh::Busy) => {
                    // 不记台账、不改 health：这不是失败，只是「别重复抓」。
                    (
                        StatusCode::CONFLICT,
                        Json(json!({
                            "ok": false,
                            "busy": true,
                            "message": "上一次公益站刷新还在进行中（聚合源慢时可能几十秒），稍等再看结果；本次没有重复抓取。",
                        })),
                    )
                        .into_response()
                }
                Ok(FreeRefresh::Done(total, added, kept, skipped)) => {
                    let note = skipped.sources_note();
                    // 明细 + 源命中说明（不含总数，避免和 message 里的总数重复）。
                    let detail_note = if note.is_empty() {
                        skipped.detail()
                    } else {
                        format!("{}；{note}", skipped.detail())
                    };
                    // 台账文案与定时任务保持一致：带上「共几站/新增/保留」，
                    // 否则手动刷新之后 health 里的 last_ok 比 alert 少一截，看着像两次不同的事。
                    let full = format!("共 {total} 站，新增 {added}，保留 {kept}；{detail_note}");
                    // 手动刷新做的**就是**「公益站刷新」这件事：不记进台账的话，面板会
                    // 一直显示「尚未成功过」，直到 24 小时后定时任务自己跑一轮 —— 刚刷
                    // 完就看到「从未成功」，用户只能当成坏了。
                    st.tasks.mark_ok("free_refresh", full.clone());
                    Json(json!({
                        "ok": true, "total": total, "added": added, "kept": kept,
                        // 丢弃明细：手动刷新与每日任务共用同一份，否则「为什么只有 12 站」
                        // 在面板上永远说不清。
                        "dropped": skipped.dropped(), "detail": skipped.detail(),
                        // 多源回退明细：命中的是第几个源、前面的源为什么失败。
                        "source_note": note,
                        "sources": sources,
                        "message": format!(
                            "刷新完成：共 {total} 站，新增 {added}，保留 {kept}（已添加的不会丢失）；{detail_note}"
                        ),
                    }))
                    .into_response()
                }
                Err(e) => {
                    st.tasks.mark_err("free_refresh", e.clone());
                    (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "message": e}))).into_response()
                }
            }
        }
        "remove" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let removed = st.free.remove(id);
            if removed {
                invalidate_models_cache(&st);
            }
            Json(json!({"ok": removed, "message": if removed { "已删除" } else { "条目不存在" }})).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{action}")}))).into_response(),
    }
}

/// 出站代理配置：GET /api/proxy → 当前配置（mode/url）。
///   POST /api/proxy {"mode":"direct"|"global"|"bypass_cn","url":"http://127.0.0.1:7890"}
///   保存 data/proxy.json 并热更新全局（后续所有上游/登录请求立即生效，无需重启）。
async fn proxy_get() -> Json<Value> {
    let cfg = aigw_relay::proxy::current();
    Json(json!({
        "ok": true,
        "mode": cfg.mode,
        "url": cfg.url,
        "modes": ["direct", "global", "bypass_cn"],
    }))
}

/// 面板令牌文件名（数据目录内）。
///
/// 落盘内容与 accounts.json 同一套 DPAPI 加密（purpose=panel，见 aigw_store::secret）：
/// 文件里是 dpapi: 密文，别的 Windows 用户/别的机器拷走也解不开。
/// 换来的是「设过的令牌不会因为重启而丢失」。
const PANEL_TOKEN_FILE: &str = "panel-token.txt";

/// 32 字节随机令牌（hex），与 main.rs 的 gen_token 同强度。
///
/// 面板里轮换出来的令牌不能比启动时自动生成的弱，否则「轮换」就成了降级通道。
fn gen_panel_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 把令牌写进 `dir/panel-token.txt`；`None` 表示删除文件（关闭令牌）。
///
/// 返回是否落盘成功。**失败不阻塞本次生效**（内存里已经换了，只是重启会回到旧状态），
/// 但调用方必须把 false 显式回给用户：静默降级 = 用户以为设好了，下次启动却裸奔。
fn persist_panel_token(dir: &std::path::Path, t: Option<&str>) -> bool {
    let path = dir.join(PANEL_TOKEN_FILE);
    match t {
        Some(tok) => {
            let _ = std::fs::create_dir_all(dir);
            // 加密后落盘；DPAPI 不可用时 wrap_with 原样返回明文（fail-open，与账号池一致）。
            let stored = aigw_store::secret::wrap_with(tok, "panel");
            std::fs::write(&path, stored.as_bytes()).is_ok()
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        },
    }
}

/// 面板令牌轮换（`/api/token`）。
///
/// 背景：令牌此前只能从 `--token` / `AIGW_PANEL_TOKEN` 注入，换一次就得重启；
/// 对外监听自动生成的令牌更是「重启即换、只在日志里出现一次」。
///
/// 鉴权：门禁中间件对 `/api/token` 同样校验令牌，所以只有**已经持有当前令牌**的
/// 调用者（或未设令牌时的本机回环访问）才能换锁 —— 正好是「自己人换锁」的语义。
/// 注意 `clear` 会把门禁降级成「仅回环可访问」，所以回显文案里要讲清风险。
async fn token_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "message": format!("请求体不是合法 JSON：{e}")})),
            )
                .into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    let dd = st.data_dir.clone();
    match action {
        "generate" | "set" => {
            // ★ `action=generate` 只生成随机令牌。旧实现里「带 token 的 generate」会
            //   **静默忽略**那个 token（照样返回一个随机串，响应里 action 还是 generate），
            //   调用方很容易以为「我设的固定令牌生效了」，直到下一次请求 401 才发现。
            //   现在直接 400 并指向 action=set。只有「空串 / 缺字段 / null」算没指定：
            //   很多客户端会把未填字段序列化成 ""；但全空白串（"   "）当作粘贴失败报错，
            //   与 action=set 的空白校验保持一致。
            let supplied = match v.get("token") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if s.is_empty() => None,
                other => other,
            };
            if action == "generate" && supplied.is_some() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "ok": false,
                        "message": "action=generate 只生成随机令牌，不接受 token 字段；要指定固定令牌请用 action=set",
                    })),
                )
                    .into_response();
            }
            let tok = if action == "generate" {
                gen_panel_token()
            } else {
                match v.get("token").and_then(|x| x.as_str()).map(|t| t.trim().to_string()) {
                    Some(t)
                        if t.len() >= 16
                            && t.len() <= 256
                            && !t.chars().any(|c| c.is_whitespace()) =>
                    {
                        t
                    }
                    Some(_) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"ok": false, "message": "令牌至少 16 个字符、最多 256 个字符，且不能含空白（粘贴时别带换行）"})),
                        )
                            .into_response()
                    }
                    None => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"ok": false, "message": "action=set 时必须带 token 字段"})),
                        )
                            .into_response()
                    }
                }
            };
            st.set_panel_token(Some(tok.clone()), "panel");
            let persisted = match dd.as_deref() {
                Some(d) => persist_panel_token(d, Some(&tok)),
                None => false,
            };
            let mut msg = String::from(if action == "generate" {
                "已生成新的面板令牌"
            } else {
                "已设置新的面板令牌"
            });
            msg.push_str("：旧令牌立即失效。请立刻复制保存 —— 令牌只在这里回显一次，之后任何接口都读不出来。");
            if !persisted {
                msg.push_str(" 注意：没能落盘（数据目录未配置或写入失败），本次只在内存生效，重启服务后会回到旧令牌。");
            }
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "action": action,
                    "token": tok,
                    "persisted": persisted,
                    "message": msg,
                })),
            )
                .into_response()
        }
        "clear" => {
            st.set_panel_token(None, "");
            let persisted = match dd.as_deref() {
                Some(d) => persist_panel_token(d, None),
                None => false,
            };
            let mut msg = String::from(
                "已关闭面板令牌：/api/* 与 /v1/* 不再校验令牌 —— 只有在服务仅监听回环地址时才安全。",
            );
            if !persisted {
                msg.push_str(" 注意：删除令牌文件失败，重启后旧令牌会重新生效。");
            }
            (
                StatusCode::OK,
                Json(json!({"ok": true, "action": "clear", "persisted": persisted, "message": msg})),
            )
                .into_response()
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "message": "action 必须是 generate / set / clear"})),
        )
            .into_response(),
    }
}

/// 访问密钥（GET /api/keys）：只下发元信息，**永不下发密钥本身**。
///
/// 密钥只在 add / rotate 的响应里回显一次（与面板令牌同一条纪律），之后
/// 面板上只看得到名字与白名单，避免「看一眼面板就卷走全部 key」。
async fn keys_list(State(st): State<AppState>) -> Json<Value> {
    let rows = st.keys.rows();
    Json(json!({ "ok": true, "rows": rows, "count": rows.len() }))
}

/// 访问密钥增删改（POST /api/keys）：action = add / update / remove。
/// 白名单里「当前一个模型都匹配不到」的条目。
///
/// 为什么要报这个：白名单是**只减不增**的闸门 —— 写错一个字母（`kilo-auto/fre`）、
/// 或者那个模型刚好被停用/平台没凭据（不在 /v1/models 里），结果是「这把 key 什么都调不了」
/// 或者「少调了本该能调的」。默认失败是静默的：请求 403 model_not_allowed，
/// 用户去翻白名单列表看到一串名字，很难发现哪个错了。
///
/// 匹配规则必须与 `AccessKey::allows` 一致（`a == model` 或裸名匹配，
/// 写 `模型@平台` 时等价于该平台下那个模型），否则会把合法条目报成拼错。
fn unknown_allow_entries(st: &AppState, allow: &[String]) -> Vec<String> {
    if allow.is_empty() {
        return Vec::new();
    }
    // 当前真正可调用的模型（与 /v1/models 同源：只含可用平台上的模型）
    let (list, _) = assemble_models(st);
    // id → owned_by（平台）。写「模型@平台」的条目要连平台一起比对。
    let owner: std::collections::HashMap<String, String> = list["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((x["id"].as_str()?.to_string(), x["owned_by"].as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    allow
        .iter()
        .filter(|a| {
            // 是否命中必须与 AccessKey::allows 的语义一致：写裸名 → 任何平台上的这个模型都算命中；
            // 写「模型@平台」→ 只有「该平台 + 该模型」同时成立才算命中。
            // （/v1/models 的 id 是裸名、平台在 owned_by 里，所以两个都要看。）
            let hit = |id: &str| -> bool {
                if let Some((want, plat)) = a.rsplit_once('@') {
                    if st.is_platform_id(plat) {
                        return want == id && owner.get(id).map(|s| s.as_str()) == Some(plat);
                    }
                }
                a.as_str() == id
            };
            !owner.keys().any(|id| hit(id))
        })
        .cloned()
        .collect()
}
/// 模型链列表（GET /api/combos）。
///
/// 每条链额外报一个 `effective` —— 把链里**当前真正可调用**的候选列出来。
/// 面板上「这条链现在还能回退几次」是用户唯一能自证配置对不对的数字：链里写了
/// 三个模型、其中两个平台没凭据时，用户看到三个名字会以为有三层保险。
async fn combos_list(State(st): State<AppState>) -> Json<Value> {
    let (list, _) = assemble_models(&st);
    let callable: std::collections::HashSet<String> = list["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let rows: Vec<Value> = st
        .combos
        .rows()
        .into_iter()
        .map(|c| {
            let effective: Vec<String> = c
                .models
                .iter()
                .filter(|m| {
                    // 写「模型@平台」的条目要按裸名比：/v1/models 的 id 是裸名
                    let bare = m.rsplit_once('@').map(|(b, p)| {
                        if st.is_platform_id(p) { b } else { m.as_str() }
                    }).unwrap_or(m.as_str());
                    callable.contains(bare)
                })
                .cloned()
                .collect();
            let dead: Vec<String> = c.models.iter().filter(|m| !effective.contains(m)).cloned().collect();
            json!({
                "id": c.id,
                "name": c.name,
                "models": c.models,
                "strategy": c.strategy,
                "sticky_limit": c.sticky_limit,
                "enabled": c.enabled,
                "effective": effective,
                "unavailable": dead,
            })
        })
        .collect();
    Json(json!({ "ok": true, "rows": rows, "count": rows.len() }))
}

/// 模型链增删改（POST /api/combos）：action = add / update / remove。
async fn combos_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
    let models_of = |v: &Value| -> Vec<String> {
        v.get("models")
            .and_then(|m| m.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    };
    match action {
        "add" => {
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let strategy = v.get("strategy").and_then(|x| x.as_str()).unwrap_or("fallback");
            let sticky = v.get("sticky_limit").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
            match st.combos.add(name, models_of(&v), strategy, sticky) {
                Ok(c) => Json(json!({"ok": true, "combo": c, "persisted": !st.combos.rows().is_empty()})).into_response(),
                Err(m) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": m}))).into_response(),
            }
        }
        "update" => {
            let Some(id) = v.get("id").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) else {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "update 需要 id"}))).into_response();
            };
            let name = v.get("name").and_then(|x| x.as_str());
            let models = if v.get("models").is_some() { Some(models_of(&v)) } else { None };
            let strategy = v.get("strategy").and_then(|x| x.as_str());
            let sticky = v.get("sticky_limit").and_then(|x| x.as_u64()).map(|n| n as u32);
            let enabled = v.get("enabled").and_then(|x| x.as_bool());
            match st.combos.update(id, name, models, strategy, sticky, enabled) {
                Ok(Some(c)) => Json(json!({"ok": true, "combo": c})).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "message": format!("没有 id 为 {id} 的链")})),
                )
                    .into_response(),
                Err(m) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": m}))).into_response(),
            }
        }
        "remove" => {
            let Some(id) = v.get("id").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) else {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "remove 需要 id"}))).into_response();
            };
            let hit = st.combos.remove(id);
            Json(json!({"ok": hit, "removed": hit})).into_response()
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "message": format!("不认识的 action「{other}」：只接受 add / update / remove")})),
        )
            .into_response(),
    }
}

async fn keys_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    // allow_models 缺省 = 不限制；显式给 [] 也等于不限制（与前端的「留空」同一语义）。
    let models_of = |v: &Value| -> Option<Vec<String>> {
        v.get("allow_models").and_then(|m| m.as_array()).map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        })
    };
    // ★ 白名单没上限时实测能塞 5000 条并落盘成功：keys.json 膨胀、**每个**请求的
    //   `allows()` 都要线性扫一遍、面板每刷一次就把整份清单回吐。这里只堵离谱输入。
    //   注意先归一（去空白去重）再数条数，否则「512 个空格」也会被拒。
    if let Some(list) = models_of(&v) {
        let clean: Vec<String> = {
            let mut out: Vec<String> = Vec::new();
            for m in list {
                let m = m.trim().to_string();
                if m.is_empty() || out.contains(&m) {
                    continue;
                }
                out.push(m);
            }
            out
        };
        if clean.len() > aigw_store::keys::MAX_ALLOW_MODELS {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "message": format!(
                    "白名单最多 {} 个模型（现在 {} 个）。白名单是「只减不增」的闸门，塞太多通常是误把模型清单整段贴进来了；",
                    aigw_store::keys::MAX_ALLOW_MODELS, clean.len()
                )})),
            )
                .into_response();
        }
        if let Some(long) = clean.iter().find(|m| m.chars().count() > aigw_store::keys::MAX_ALLOW_ENTRY_LEN) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "message": format!(
                    "白名单里有条目过长（{} 个字符，上限 {}）：{}",
                    long.chars().count(), aigw_store::keys::MAX_ALLOW_ENTRY_LEN,
                    long.chars().take(60).collect::<String>()
                )})),
            )
                .into_response();
        }
    }
    match action {
        "add" => {
            let name = v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": "name 不能为空（写清这把 key 给谁用，比如「部门脚本」）"})),
                )
                    .into_response();
            }
            let note = v.get("note").and_then(|x| x.as_str()).unwrap_or("");
            let allow = models_of(&v).unwrap_or_default();
            // 先把「匹配不到任何模型」的条目算出来（要在入库前后都成立，用当前清单即可）：
            // 不拒绝创建（白名单允许提前写好还没接入的模型名），但必须让用户当场看见。
            let unknown = unknown_allow_entries(&st, &allow);
            let (k, secret) = st.keys.add(&name, allow, note);
            let persisted = st.keys.persist();
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "action": "add",
                    "row": k,
                    "key": secret,
                    "persisted": persisted,
                    "unknown_models": unknown,
                    "message": "已创建访问密钥。请立刻复制保存 —— 它只在这里回显一次；丢了就在面板上轮换。",
                })),
            )
                .into_response()
        }
        "update" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let rotate = v.get("rotate").and_then(|x| x.as_bool()).unwrap_or(false);
            let name = v.get("name").and_then(|x| x.as_str());
            // ★ 与 action=add 保持同一条规矩：名字可以改，但不能改成空的 ——
            //   面板表格里一行「没有名字」的 key，谁也认不出它是给哪个脚本用的，
            //   等到要轮换/删除时只能靠猜。传了 name 就必须是非空串（不传 = 不改）。
            if let Some(n) = name {
                if n.trim().is_empty() {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "message": "name 不能改成空（写清这把 key 给谁用，比如「部门脚本」）；不想改名就别传 name 字段"})),
                    )
                        .into_response();
                }
            }
            let note = v.get("note").and_then(|x| x.as_str());
            let enabled = v.get("enabled").and_then(|x| x.as_bool());
            let new_allow = models_of(&v);
            let unknown = new_allow
                .as_deref()
                .map(|a| unknown_allow_entries(&st, a))
                .unwrap_or_default();
            match st.keys.update(id, name, new_allow, note, enabled, rotate) {
                Some((k, newkey)) => {
                    let persisted = st.keys.persist();
                    let mut out = json!({
                        "ok": true,
                        "action": "update",
                        "row": k,
                        "persisted": persisted,
                        "unknown_models": unknown,
                    });
                    if let Some(s) = newkey {
                        out["key"] = json!(s);
                        out["message"] = json!("已轮换密钥：旧密钥立即失效，新密钥只在这里回显一次。");
                    }
                    (StatusCode::OK, Json(out)).into_response()
                }
                None => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "message": format!("没有 id = {id} 的访问密钥")})),
                )
                    .into_response(),
            }
        }
        "remove" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if st.keys.remove(id) {
                let persisted = st.keys.persist();
                (
                    StatusCode::OK,
                    Json(json!({"ok": true, "action": "remove", "persisted": persisted, "message": "已删除该访问密钥：它已无法调用任何模型"})),
                )
                    .into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"ok": false, "message": format!("没有 id = {id} 的访问密钥")})),
                )
                    .into_response()
            }
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "message": "action 必须是 add / update / remove"})),
        )
            .into_response(),
    }
}
async fn proxy_set(body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("");
    if !["direct", "global", "bypass_cn"].contains(&mode) {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "mode 必须是 direct | global | bypass_cn"}))).into_response();
    }
    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    // 非回环代理必须显式确认：旧实现只在前端 confirm() 一下，API 直连可以静默换代理，
    // 而代理是所有上游请求（含 Key / Cookie）的必经之路 —— 换掉它等于拿到全部凭据。
    let confirm_nonlocal = v.get("confirm_nonlocal").and_then(|x| x.as_bool()).unwrap_or(false);
    if mode != "direct" && url.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "代理模式需要 url（如 http://127.0.0.1:7890）"}))).into_response();
    }
    let ph = url_host(&url);
    if !url.is_empty() {
        // ★ 旧实现完全不校验 url：可以写 file:// 或指向云元数据地址（169.254.169.254），
        //   等于给面板开了一个 SSRF 出口；而 proxy.rs:127 用 Url::parse(...).ok()，
        //   非法串又被静默当成直连 —— 用户以为生效，其实没走代理。
        let ok = match url.split_once("://") {
            Some((scheme, rest)) => {
                !rest.is_empty()
                    && matches!(
                        scheme.to_ascii_lowercase().as_str(),
                        "http" | "https" | "socks5" | "socks5h"
                    )
            }
            None => false,
        };
        if !ok {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "message": "代理 url 必须形如 http://127.0.0.1:7890（支持 http/https/socks5/socks5h）"})),
            )
                .into_response();
        }
        // 代理 url 指向云元数据端点：所有上游请求都会先过它，等于把面板变成 SSRF 跳板。
        if blocked_upstream_host(&ph) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "message": format!("代理地址 {} 是云元数据端点，不能作为出站代理（会让所有上游请求被拿去换云主机凭据）", ph)})),
            )
                .into_response();
        }
        // 非回环代理：要求调用方显式确认（前端弹窗确认后才会带这个字段）。
        if !is_loopback_host(&ph) && !confirm_nonlocal {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "need_confirm": "nonlocal_proxy",
                    "message": format!(
                        "代理主机 {} 不是本机回环地址：保存后登录、模型同步与每一次上游转发（含请求里的 Key / Cookie）都会先经过它。确认要这样代理，请带上 confirm_nonlocal: true 再提交一次。",
                        ph
                    )
                })),
            )
                .into_response();
        }
    }
    let cfg = aigw_relay::proxy::ProxyConfig { mode: mode.into(), url };
    match aigw_relay::proxy::save_and_apply(&aigw_relay::proxy::default_data_dir(), &cfg) {
        Ok(()) => Json(json!({"ok": true, "message": format!("代理已切换为 {}，所有流程即时生效", mode), "cfg": cfg})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "message": format!("保存失败：{e}")}))).into_response(),
    }
}

async fn accounts(State(st): State<AppState>) -> Json<Value> {
    let mut snap = st.accounts.rows_snapshot();
    // 免密车道的平台 id 集合：这些平台转发时用 `{id}:anon` 现造账号、凭据恒为空，
    // 账号池里任何一条都属于历史遗留的占位行（name="匿名公共池"、secret="public"）。
    // 标成 anon 让前端显示「占位行·可删」，而不是当成一份真凭据去「检查/停用」。
    let anon_platforms: std::collections::HashSet<String> = st
        .all_specs()
        .iter()
        .filter(|s| s.auth == AuthKind::Anon)
        .map(|s| s.id.clone())
        .collect();
    // 号池里的健康结论（正常/鉴权失败/额度不足/被限流）并进账号行：
    // 面板要能一眼看出「哪个号已经过期」，而不是等下一次对话失败才发现。
    if let Some(rows) = snap.get_mut("rows").and_then(|r| r.as_array_mut()) {
        for row in rows.iter_mut() {
            let id = row.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            // 克隆成 String：下面要 &mut row 写字段，借用 row 的 &str 会冲突。
            let plat = row.get("platform").and_then(|x| x.as_str()).unwrap_or("").to_string();
            row["anon"] = json!(anon_platforms.contains(&plat));
            // ★ 免密车道的池键不是账号 id：转发时用 `{平台}:anon` 现造账号，
            //   占位行是历史遗留、id 随机，拿它去查永远查不到。按车道键查，
            //   「这条车道现在被封锁多少秒」才能显示出来（免密车道没有凭据可维护，
            //   用户唯一能做的就是换车道或等）。
            let pool_key = if row["anon"].as_bool().unwrap_or(false) {
                format!("{plat}:anon")
            } else {
                id.clone()
            };
            row["pool_key"] = json!(pool_key);
            row["health"] = match st.pool.health_of(&pool_key) {
                Some((status, label, age)) => json!({
                    "status": status, "label": label, "age_secs": age
                }),
                None => Value::Null,
            };
            // 冷却 / WAF 限速的**剩余秒数**：健康结论说的是「上一次为什么失败」，
            // 这里说的是「现在还能不能用」。两者都要有——用户看到「鉴权失败」之后的
            // 第一反应是「重登之后要等多久才会被重新选用」，答案就在这两个数里。
            let blocked = st.pool.blocked_of(&pool_key);
            row["cooling_s"] = json!(blocked.cooling_s.round());
            row["waf_s"] = json!(blocked.waf_s.round());
            row["blocked"] = json!(!blocked.is_clear());
        }
    }
    // 顶层附「每日自动签到」开关与后台任务台账：账号页要能就地开关，
    // 并看到上一次自动签到到底领没领到（否则开了等于黑箱）。
    // 账号表里的「平台」列用显示名（WorkBuddy 国内站），裸 id 降级成副标题 ——
    // 账号名与平台名都是给人看的，不该让人对着 wb-gateway 猜这是哪个产品。
    if let Some(rows) = snap.get_mut("rows").and_then(|r| r.as_array_mut()) {
        let names: std::collections::HashMap<String, String> = st
            .all_specs()
            .iter()
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect();
        for r in rows.iter_mut() {
            if let Some(pid) = r.get("platform").and_then(|x| x.as_str()) {
                let known = names.get(pid).cloned();
                // platform_known=false：providers/ 里已经没有这个平台的规格了（删过 toml、
                // 或备份是从带该平台的旧版本恢复的）。这种账号永远转不出去，留在表里
                // 只会让人以为「这个号还能用」，所以要显式标出来。
                r["platform_known"] = json!(known.is_some());
                if let Some(nm) = known {
                    r["platform_name"] = json!(nm);
                }
            }
        }
    }
    snap["daily_checkin"] = json!(st.flags.daily_checkin());
    snap["daily_growth"] = json!(st.flags.daily_growth());
    snap["growth_task"] = st
        .tasks
        .snapshot()
        .get("daily_growth")
        .cloned()
        .unwrap_or(Value::Null);
    snap["checkin_task"] = st
        .tasks
        .snapshot()
        .get("daily_checkin")
        .cloned()
        .unwrap_or(Value::Null);
    Json(snap)
}

/// 账号管理：POST /api/accounts
///   {"action":"delete","id":"..."} | {"action":"toggle","id":"...","enabled":false}
/// 删除/停用同时清号池治理状态（否则快照里会挂幽灵账号）。
async fn accounts_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    // 先判 action 再判 id：否则拼错 action 会被误报成「缺 id」，排查时误导。
    if action != "delete"
        && action != "toggle"
        && action != "verify"
        && action != "rename"
        && action != "balance"
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "message": format!("未知 action：{action}（可用 delete / toggle / verify / rename / balance）")})),
        )
            .into_response();
    }
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
    }
    match action {
        "delete" => {
            if st.accounts.remove(&id) {
                st.pool.forget_account(&id);
                st.sticky.unbind_account(&id);
                invalidate_models_cache(&st);
                Json(json!({"ok": true, "message": "账号已删除"})).into_response()
            } else {
                (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": "账号不存在"}))).into_response()
            }
        }
        // 改名：账号名是用户在面板里唯一的辨识依据（同一个平台挂好几个号时尤其重要）。
        // 名字在平台内唯一（登录会覆盖同平台同名行），所以撞名必须报错而不是静默合并。
        "balance" => {
            // ★ 2026-10-26（用户排队消息④）：账号积分余额。仅 wb 族有此端点
            //   （GET /v2/billing/meter/get-user-resource）；其他平台如实报不支持。
            let Some((platform, secret)) = st.accounts.secret_of(&id) else {
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": "账号不存在"}))).into_response();
            };
            let Some(base) = aigw_relay::wb::wb_checkin_base(&platform) else {
                return Json(json!({
                    "ok": false,
                    "supported": false,
                    "message": "该平台没有积分余额端点（仅 WorkBuddy / CodeBuddy 族支持查询）"
                })).into_response();
            };
            let http = aigw_relay::auth_login::auth_http();
            match aigw_relay::wb::query_balance(&http, base, &secret).await {
                Ok(mut d) => {
                    // ★ 2026-10-26（P1）：顺手更新号池积分快照（「快过期优先」选号依据）
                    let credit = d.get("credit").and_then(|x| x.as_i64()).map(|c| c as f64);
                    st.pool.record_credit(&id, credit);
                    d["ok"] = json!(true);
                    d["supported"] = json!(true);
                    d["platform"] = json!(platform);
                    Json(d).into_response()
                }
                Err(e) => Json(json!({"ok": false, "supported": true, "message": e})).into_response(),
            }
        }
        "rename" => {
            const MAX_ACCOUNT_NAME_LEN: usize = 60;
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if name.is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": "name 不能为空（写清这个号是谁的 / 哪条线路，方便以后认）"})),
                )
                    .into_response();
            }
            if name.chars().count() > MAX_ACCOUNT_NAME_LEN {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": format!("名字太长（{} 个字符，上限 {MAX_ACCOUNT_NAME_LEN}）", name.chars().count())})),
                )
                    .into_response();
            }
            let (hit, err, changed) = st.accounts.rename(&id, &name);
            if !hit {
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": err}))).into_response();
            }
            if !err.is_empty() {
                return (StatusCode::CONFLICT, Json(json!({"ok": false, "message": err}))).into_response();
            }
            Json(json!({
                "ok": true,
                "changed": changed,
                "account_id": id,
                "name": name,
                "message": if changed { "账号名已改" } else { "名字没变" },
            }))
            .into_response()
        }
        // 立即验活：过期 Cookie / 被限流的号不用等下一次真实对话才发现。
        "verify" => {
            let Some((platform, secret)) = st.accounts.secret_of(&id) else {
                // 「有这条账号但凭据解不开」和「没有这个账号」是两件事：前者要重新登录，
                // 后者是 id 错了。以前统一报 404「账号不存在」，排查时会被带偏。
                if st.accounts.needs_relogin(&id) {
                    return (
                        StatusCode::CONFLICT,
                        Json(json!({
                            "ok": false,
                            "needs_relogin": true,
                            "message": "这个账号的凭据是在别的 Windows 用户/机器上加密的，本机解不开：请重新登录（或用「手动粘贴」重新填一次凭据）"
                        })),
                    )
                        .into_response();
                }
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": "账号不存在"}))).into_response();
            };
            let Some(spec) = st.all_specs().into_iter().find(|s| s.id == platform) else {
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": format!("平台 {platform} 已不存在（providers 里删掉了？）")}))).into_response();
            };
            let ok = st.client.verify_account(&spec, &secret).await;
            st.pool.mark_health(&id, ok, "");
            // ★ 验活是「发现凭据过期」最常用的主动路径：结论必须同时落到平台卡片上。
            //   旧实现只回一句提示，卡片（/api/platforms 的 last_error）什么都不变 ——
            //   用户点完「验活」看不到任何痕迹，切到接入源页还以为这平台一切正常。
            if ok {
                st.last_err.clear(&platform);
            } else {
                st.last_err.note(&platform, 0, VERIFY_FAIL_MSG);
            }
            let msg = if ok {
                "验活通过 ✓ 凭据可用"
            } else {
                VERIFY_FAIL_MSG
            };
            Json(json!({
                "ok": true,
                "verified": ok,
                "message": msg,
                "platform": platform,
                "last_error": st.last_err.json(&platform),
            }))
            .into_response()
        }
        "toggle" => {
            let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
            if st.accounts.set_enabled(&id, enabled) {
                if !enabled {
                    st.pool.forget_account(&id);
                    st.sticky.unbind_account(&id);
                }
                invalidate_models_cache(&st);
                Json(json!({"ok": true, "enabled": enabled})).into_response()
            } else {
                (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": "账号不存在"}))).into_response()
            }
        }
        other => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{other}")}))).into_response(),
    }
}

/// `/api/pool` 默认最多回多少条粘性绑定（`?sticky=full` 才全量）。
const POOL_STICKY_PREVIEW: usize = 50;

async fn pool_snapshot(
    State(st): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let mut v = st.pool.snapshot();
    // 粘性表也一并暴露：它的大小由**客户端可控**的会话键决定（每请求换一个
    // conversation_id 就能持续新增条目），出问题时没有这个视图只能靠猜。
    //
    // ★ 默认只回前 POOL_STICKY_PREVIEW 条：上限是 8192 条、每条还带完整会话键，
    //   全量返回是几百 KB 的响应，面板也只能渲染前几行。总数照常给（`total`），
    //   真要逐条排查加 `?sticky=full`。
    let full = params
        .get("sticky")
        .map(|s| s.eq_ignore_ascii_case("full"))
        .unwrap_or(false);
    let limit = if full { usize::MAX } else { POOL_STICKY_PREVIEW };
    let sticky = st.sticky.snapshot_limited(limit);
    let sticky_total = sticky.get("total").and_then(|x| x.as_u64()).unwrap_or(0);
    if let Some(o) = v.as_object_mut() {
        o.insert("sticky".to_string(), sticky);
        o.insert(
            "sticky_max_entries".to_string(),
            json!(aigw_pool::STICKY_MAX_ENTRIES),
        );
        // 顶层再给一次总数：前端/脚本只想知道「占用多少」时不必自己数 entries。
        o.insert("sticky_entries".to_string(), json!(sticky_total));
        o.insert("sticky_full".to_string(), json!(full));
    }
    Json(v)
}

/// `POST /api/pool`：目前只有一个动作 —— 手动解冻 `{action:"thaw", account}`。
///
/// 冷却时长是网关**猜**的（WAF/402 一律 30 分钟），上游实际恢复常比这快得多。
/// 面板既然能显示「还要等 109 秒」，就该让用户在确认好了之后一键放行，
/// 而不是干等或重启服务。只清冷却/限速，不动 health（历史结论要真实请求来改写）。
async fn pool_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")})))
                .into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("").trim();
    match action {
        "thaw" => {
            let account = v.get("account").and_then(|x| x.as_str()).unwrap_or("").trim();
            if account.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "thaw 需要 account（面板上显示的那个 ID）"})))
                    .into_response();
            }
            let (had_waf, cooled) = st.pool.thaw(account);
            if !had_waf && cooled == 0 {
                return (StatusCode::NOT_FOUND, Json(json!({"ok": false, "message": format!("{account} 现在没有冷却或限速，不需要解冻")})))
                    .into_response();
            }
            let mut msg = format!("{account} 已解冻");
            if had_waf {
                msg.push_str("（整号限速已解除）");
            }
            if cooled > 0 {
                msg.push_str(&format!("（{cooled} 条模型冷却已清除）"));
            }
            msg.push_str("；下一次请求会真的发到上游，成不成立刻见分晓");
            Json(json!({"ok": true, "account": account, "waf_cleared": had_waf,
                        "cooldown_cleared": cooled, "message": msg}))
                .into_response()
        }
        "" => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺少 action（目前只支持 thaw）"})))
            .into_response(),
        other => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("不认识的 action={other}（目前只支持 thaw）")})))
            .into_response(),
    }
}
/// wb 族每日签到：GET /api/checkin（查全部 wb 账号状态）/ POST /api/checkin（全部领取）。
/// 一手：workbuddy2api-master admin/backend.py:338-368（Buddy 加油站每日 100 积分）。
/// GET 额外带 daily（自动签到开关）与 task（后台任务台账）。
async fn checkin_status_all(State(st): State<AppState>) -> Json<Value> {
    let mut v = checkin_all(&st, false).await;
    v["daily"] = json!(st.flags.daily_checkin());
    v["task"] = st
        .tasks
        .snapshot()
        .get("daily_checkin")
        .cloned()
        .unwrap_or(Value::Null);
    Json(v)
}

/// POST /api/checkin：
///   - 空 body / {} / 无 action → 立刻全部领取（旧前端只发 {}，行为保持不变）
///   - {"action":"schedule","enabled":true|false} → 开/关每日自动签到（存 flags.json）
async fn checkin_claim_all(State(st): State<AppState>, body: Bytes) -> Response {
    // 空 body 也算「领取」：旧版前端/脚本可能直接 POST 空体，不能升级后变成 400。
    let req: Value = if body.is_empty() || String::from_utf8_lossy(&body).trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")})),
                )
                    .into_response()
            }
        }
    };
    let action = req.get("action").and_then(|v| v.as_str()).unwrap_or("claim");
    match action {
        "claim" => Json(checkin_all(&st, true).await).into_response(),
        // ★ 2026-10-26（P0）：成长任务一键完成。对齐 workbuddy2api 17 任务闭环：
        //   读清单 → 接受 → 事件链点亮（桌面指纹 /v2/report）→ 逐个领奖。
        "growth" => {
            let platform = req.get("platform").and_then(|v| v.as_str()).unwrap_or("wb-gateway").to_string();
            let Some(base) = aigw_relay::wb::wb_checkin_base(&platform) else {
                return Json(json!({"ok": false, "message": "成长任务仅 WorkBuddy / CodeBuddy 族支持"})).into_response();
            };
            let web_base = if platform == "codebuddy" {
                "https://www.codebuddy.cn"
            } else {
                "https://www.workbuddy.cn"
            };
            let http = aigw_relay::auth_login::auth_http();
            let accounts = st.accounts.secrets_for(&platform);
            if accounts.is_empty() {
                return Json(json!({"ok": false, "message": format!("{platform} 还没有账号，先登录")})).into_response();
            }
            let mut all: Vec<Value> = Vec::new();
            for (acc_id, secret) in accounts {
                // uid：成长任务的桌面指纹要 userId。Account 没有独立 uid 字段，
                // 用账号 id（登录时由上游 uid 派生）当稳定标识即可——同账号恒定。
                let nick = st
                    .accounts
                    .rows_snapshot()
                    .get("rows")
                    .and_then(|r| r.as_array())
                    .and_then(|arr| {
                        arr.iter()
                            .find(|a| a.get("id").and_then(|x| x.as_str()) == Some(acc_id.as_str()))
                    })
                    .and_then(|a| a.get("name"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let r = aigw_relay::wb_growth::run_all_growth(
                    &http, base, web_base, &secret, &acc_id, &nick,
                )
                .await;
                all.push(json!({"account": acc_id, "result": r}));
            }
            Json(json!({"ok": true, "platform": platform, "accounts": all})).into_response()
        }
        "schedule" => {
            let Some(on) = req.get("enabled").and_then(|v| v.as_bool()) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "ok": false,
                        "message": "schedule 需要布尔字段 enabled（true=开启每日自动签到，false=关闭）"
                    })),
                )
                    .into_response();
            };
            st.flags.set_daily_checkin(on);
            let message = if on {
                // ★ 开启的同时立刻领一轮：用户点「开」就是同意现在领。否则后台要等
                //   12 小时才第一次真跑，台账一直停在「未开启…跳过」，像开关没生效。
                let v = checkin_all(&st, true).await;
                let s = summarize_checkin(&v);
                st.tasks.mark_ok("daily_checkin", s.clone());
                format!(
                    "已开启每日自动签到：已立即领取一次，之后每 12 小时自动领取（开关存 flags.json，重启保持；当天已领过时上游回「今日已领取」，无副作用）。本次结果：{s}"
                )
            } else {
                "已关闭每日自动签到：后台任务不再请求上游".to_string()
            };
            Json(json!({
                "ok": true,
                "daily": on,
                "message": message,
            }))
            .into_response()
        }
        // ★ 2026-10-26（P0）：成长任务自动开关（只写 flags，后台循环自己读）。
        "growth_schedule" => {
            let Some(on) = req.get("enabled").and_then(|v| v.as_bool()) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": "缺 enabled（true/false）"})),
                )
                    .into_response();
            };
            st.flags.set_daily_growth(on);
            Json(json!({
                "ok": true,
                "daily_growth": on,
                "message": if on {
                    "已开启每日自动成长任务：后台每 12 小时跑一轮（开关存 flags.json，重启保持）"
                } else {
                    "已关闭每日自动成长任务：后台不再上报事件链"
                },
            }))
            .into_response()
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "message": format!("未知 action：{other}（可用 claim / schedule）")})),
        )
            .into_response(),
    }
}

pub async fn checkin_all(st: &AppState, claim: bool) -> Value {
    let http = aigw_relay::auth_login::auth_http();
    let mut results: Vec<Value> = Vec::new();
    for platform in ["wb-gateway", "codebuddy"] {
        let Some(base) = aigw_relay::wb::wb_checkin_base(platform) else { continue };
        for (acc_id, secret) in st.accounts.secrets_for(platform) {
            let r = if claim {
                aigw_relay::wb::claim_daily_checkin(&http, base, &secret).await
            } else {
                aigw_relay::wb::checkin_status(&http, base, &secret).await
            };
            // ★ 2026-10-26（P1）：签到回包里的 credit 是「本次领到多少」，
            //   顺手查一次余额写进号池的积分快照，「快过期优先」选号才有依据。
            if claim && r.is_ok() {
                if let Ok(b) = aigw_relay::wb::query_balance(&http, base, &secret).await {
                    let credit = b.get("credit").and_then(|x| x.as_i64()).map(|c| c as f64);
                    st.pool.record_credit(&acc_id, credit);
                }
            }
            results.push(json!({
                "platform": platform,
                "account": acc_id,
                "action": if claim { "claim" } else { "status" },
                "ok": r.is_ok(),
                "data": r.unwrap_or_else(|e| json!({"error": e})),
            }));
        }
    }
    json!({"ok": true, "claim": claim, "results": results, "count": results.len()})
}

/// 把签到结果汇总成一句后台任务台账。results 里的 ok 只代表**传输层**成功，
/// 业务成败在 data.ok / data.code：code=2（参考实现）与 code=10001（2026-10 起
/// copilot.tencent.com 实际返回，HTTP 400 +「今天已签到」）都是「今日已领取」，
/// 不是失败，单列出来 —— 否则每天第二次跑都显示「失败」，用户会以为签到坏了。
pub fn summarize_checkin(v: &Value) -> String {
    let rows = v.get("results").and_then(|r| r.as_array());
    let rows = match rows {
        Some(r) if !r.is_empty() => r,
        _ => return "没有可签到的 wb 族账号（先在面板登录 wb-gateway / codebuddy）".to_string(),
    };
    let mut ok_n = 0usize;
    let mut already = 0usize;
    let mut fail = 0usize;
    let mut first_err = String::new();
    for r in rows {
        let d = r.get("data");
        if d.and_then(|x| x.get("ok")).and_then(|x| x.as_bool()) == Some(true) {
            ok_n += 1;
        } else if matches!(
            d.and_then(|x| x.get("code")).and_then(|x| x.as_i64()),
            Some(2) | Some(10001)
        ) {
            already += 1;
        } else {
            fail += 1;
            if first_err.is_empty() {
                let acc = r
                    .get("account")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string();
                let why = d
                    .and_then(|x| x.get("error").or_else(|| x.get("status")).or_else(|| x.get("msg")))
                    .and_then(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "未知原因".to_string());
                let why: String = why.chars().take(120).collect();
                first_err = format!("{acc}: {why}");
            }
        }
    }
    let mut s = format!(
        "自动签到完成：{} 个账号，成功 {}，今日已领 {}，失败 {}",
        rows.len(),
        ok_n,
        already,
        fail
    );
    if !first_err.is_empty() {
        s.push_str(&format!("；首个失败 {first_err}"));
    }
    s
}

/// 备份导出（M5 交付链，对齐 cline2api tmp+rename 数据纪律）：GET /api/backup
/// 扫描 data 目录全部运行态文件（*.json + panel.db），打包为 JSON 数组供前端下载/保存。
/// 敏感文件识别：备份内容掩码用（凭据不进导出包，但要让人看得见清单）。
fn is_secret_file(name: &str) -> bool {
    // ★ 白名单制（旧实现是子串黑名单）：只有确认不含凭据的台账文件不脱敏，其余一律脱敏。
    //   黑名单每新增一个凭据文件（oauth.json / cookies.json / token.json）都会默认落进
    //   「可明文导出」集合 —— 典型的失败开放。
    const PLAIN: &[&str] = &["usage_events.json", "usage.json", "requests.json"];
    !PLAIN.contains(&name)
}

async fn backup_export(State(st): State<AppState>) -> Json<Value> {
    let dd = aigw_relay::proxy::default_data_dir();
    // ★ 导出前先把 WAL 并回主库：WAL 模式下新写入只在 panel.db-wal 里，主库可能
    //   长期停在 4KB 空壳（实测 4096 vs 1.8MB）。不 checkpoint 就备份 = 备份出
    //   一个没有任何用量台账的空库，恢复回去还以为「备份好了」。
    let ck = st.store.checkpoint();
    collect_backup(&dd, false, st.store.is_db(), ck)
}

/// 完整导出（含明文凭据）。仅供「备份数据」按钮使用：面板本身与备份文件都在同一台
/// 机器的同一用户下，账号文件本来就是明文，这里不额外扩大攻击面；默认的
/// `GET /api/backup` 仍然脱敏，避免随手一取就泄露。
async fn backup_export_full(State(st): State<AppState>) -> Json<Value> {
    let dd = aigw_relay::proxy::default_data_dir();
    let ck = st.store.checkpoint();
    collect_backup(&dd, true, st.store.is_db(), ck)
}

fn collect_backup(
    dd: &std::path::Path,
    include_secrets: bool,
    is_db: bool,
    checkpoint_ok: bool,
) -> Json<Value> {
    let mut files: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dd) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // ★ 以 '.' 开头的是恢复时留下的旧文件副本（.bak-<ts>-accounts.json 之类）。
            //   它们同样以 .json 结尾，不排除的话每备份一次就把上一轮的备份再打包一遍，
            //   备份包越滚越大，且恢复端 safe_restore_name 本来就会拒这些名字（白收）。
            if name.starts_with('.') {
                continue;
            }
            // ★ panel.db-wal 必须一并导出：checkpoint 有可能失败（连接被占、SQLite busy），
            //   这时真实台账还在 -wal 里。旧实现只收 panel.db → 备份包里的主库是 4KB
            //   空壳，而恢复端虽然允许 -wal 却永远收不到它，备份/恢复对用量台账不闭环。
            //   -shm 故意不导出：它只是共享内存索引，SQLite 打开库时会重建；把旧 -shm
            //   塞回去反而可能与新的主库/日志不匹配。
            let ok_ext = name.ends_with(".json") || name == "panel.db" || name == "panel.db-wal";
            if !ok_ext {
                continue;
            }
            // 凭据文件一律不出内容：旧实现把 accounts.json（明文 Cookie/Token）整份
            // base64 回传，等于任何本机进程都能 GET 到全部凭据。导出只给文件名/大小，
            // 供人确认备份范围；真实离线备份请直接拷贝 data 目录。
            if is_secret_file(&name) && !include_secrets {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                files.push(json!({
                    "name": name,
                    "size": size,
                    "redacted": true,
                    "note": "含凭据，已省略内容（请直接拷贝 data 目录做离线备份）",
                }));
                continue;
            }
            match std::fs::read(&p) {
                Ok(bytes) => {
                    files.push(json!({"name": name, "bytes": base64_encode(&bytes), "size": bytes.len()}))
                }
                Err(e) => {
                    // 读失败不能静默跳过：备份里少一份文件 = 恢复后凭据/设置缺失，而用户
                    // 以为「已经备份好了」。宁可整个导出明确失败（前端 backupNow 见到
                    // ok===false 会抛错）。
                    return Json(json!({
                        "ok": false,
                        "message": format!("读取 {name} 失败：{e}"),
                        "files": [],
                        "count": 0,
                        "ts": now_ms(),
                    }));
                }
            }
        }
    }
    files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut out = json!({
        "ok": true,
        "ts": now_ms(),
        "dir": dd.to_string_lossy(),
        "files": files,
        "count": files.len(),
        "db": is_db,
        "checkpoint": checkpoint_ok,
    });
    if is_db && !checkpoint_ok {
        // 明确告知：这次备份里 panel.db 可能不含最新台账，靠 -wal 兜底。
        out["checkpoint_warning"] = json!(
            "WAL checkpoint 未成功：panel.db 可能不含最新用量台账，本次已把 panel.db-wal 一并导出以保留数据；恢复时请把 panel.db 与 panel.db-wal 一起恢复（-shm 只是索引，SQLite 打开时会重建，不用带）"
        );
    }
    if include_secrets {
        out["include_secrets"] = json!(true);
        out["warning"] = json!("此文件含明文凭据（Cookie / Token / API Key），请勿上传或分享");
    }
    Json(out)
}

/// 恢复时允许写入的文件名判定。
///
/// 只用 `contains('/')` + `contains("..")` 挡不住两种真实写法：
/// - `C:x`：Windows 盘符**相对**路径，`Path::join` 遇到盘符前缀会丢掉基点，
///   于是写到「当前盘根目录」而不是数据目录；
/// - `panel.db:evil`：NTFS 备用数据流（ADS），能往任意文件里塞内容。
/// 这里要求：纯文件名、不含盘符/流分隔符、扩展名在白名单内。
fn safe_restore_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '\0'])
        && (name.ends_with(".json")
            || name == "panel.db"
            || name == "panel.db-wal"
            || name == "panel.db-shm")
}

/// 恢复某个文件时的落盘目标。
///
/// ★ 数据库文件不能就地覆盖：面板运行时 SQLite 独占打开 panel.db，Windows 上
///   覆盖写/重命名都会以共享冲突失败。所以改成写 `<name>.restore-pending`，
///   由下次启动的 `Store::open → apply_pending_restore` 原子替换。这样响应里的
///   `restart_required: true` 才是真的生效，而不是只剩一句提示。
fn restore_target(dd: &std::path::Path, name: &str) -> std::path::PathBuf {
    if name == "panel.db" || name == "panel.db-wal" || name == "panel.db-shm" {
        dd.join(format!("{name}.restore-pending"))
    } else {
        dd.join(name)
    }
}

/// 该文件名是否属于数据库文件（恢复时走暂存通道）。
fn is_db_file_name(name: &str) -> bool {
    name == "panel.db" || name == "panel.db-wal" || name == "panel.db-shm"
}

/// 备份恢复（先写 .bak-<ts> 旧文件再覆盖，损坏不静默）：POST /api/backup
///   {"files":[{"name":"accounts.json","bytes":"<b64>"}]}
async fn backup_restore(_st: State<AppState>, body: Bytes) -> Response {
    let dd = aigw_relay::proxy::default_data_dir();
    restore_into(&dd, &body)
}

/// 恢复实现（目录可注入，便于测试；不碰全局 data 目录）。
fn restore_into(dd: &std::path::Path, body: &[u8]) -> Response {
    restore_into_at(dd, body, now_ms())
}

/// 真正的恢复逻辑，时间戳由调用方给（测试要能构造「同名回滚副本已存在」的场景）。
fn restore_into_at(dd: &std::path::Path, body: &[u8], ts: u128) -> Response {
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let Some(files) = v.get("files").and_then(|x| x.as_array()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 files 数组"}))).into_response();
    };
    // ★ 备份包体积：整体 body 已有 32MB 上限，但那是给聊天用的。往数据目录倒 32MB 的
    //   「accounts.json」既没有任何用途，也会把面板后续的每个请求拖慢（读配置、算容量）。
    //   单个文件 8MB 足够覆盖真实的 panel.db + 台账。
    const MAX_RESTORE_FILE: usize = 8 * 1024 * 1024;
    let mut restored = 0usize;
    let mut skipped = Vec::new();
    // 被成功改名成 .bak-<ts>-* 的原文件。响应里逐条报出来：文案承诺了「旧文件保留以便回滚」，
    // 那就必须能证明每一条真的留下来了。
    let mut backed_up: Vec<String> = Vec::new();
    // 数据库文件在服务运行时是 SQLite 独占打开的，Windows 上覆盖写必然失败。
    // 单独收集起来，在响应里给出可执行的下一步，而不是混在「写入失败」里让用户猜。
    let mut db_pending: Vec<String> = Vec::new();
    // 成功暂存（写进 .restore-pending、等下次启动落地）的数据库文件。
    let mut db_staged: Vec<String> = Vec::new();
    for f in files {
        let Some(name) = f.get("name").and_then(|x| x.as_str()) else { continue };
        if !safe_restore_name(name) {
            skipped.push(format!("{name}（非法名跳过）"));
            continue;
        }
        let Some(b64) = f.get("bytes").and_then(|x| x.as_str()) else { continue };
        let bytes = match base64_decode(b64) {
            Some(b) => b,
            None => {
                skipped.push(format!("{name}（b64 解码失败跳过）"));
                continue;
            }
        };
        if bytes.len() > MAX_RESTORE_FILE {
            skipped.push(format!(
                "{name}（{} 字节，超过单个文件 {} 字节的上限；真实备份的 panel.db 远小于此）",
                bytes.len(),
                MAX_RESTORE_FILE
            ));
            continue;
        }
        let is_db_file = is_db_file_name(name);
        let target = restore_target(&dd, name);
        if target.exists() {
            // ★ 回滚副本必须真的留下来才能覆盖：旧实现把 rename 的错误 let _ 吞掉，
            //   而响应文案照旧承诺「旧文件保留为 .bak-<ts>-* 以便回滚」—— 一旦改名失败
            //   （目录只读、副本已存在、被别的进程占用），用户以为还能回滚，其实旧文件
            //   已经被新内容覆盖，只剩备份包这一份了。宁可拒绝这次恢复，也不许静默丢数据。
            let bak = dd.join(format!(".bak-{ts}-{name}"));
            if let Err(e) = std::fs::rename(&target, &bak) {
                skipped.push(format!(
                    "{name}（旧文件备份失败：{e}；为了不把这个文件弄丢，本次没有覆盖它）"
                ));
                continue;
            }
            backed_up.push(name.to_string());
        }
        match aigw_store::write_atomic(&target, &bytes) {
            Ok(_) => {
                restored += 1;
                if is_db_file {
                    db_staged.push(name.to_string());
                }
            }
            Err(e) => {
                // ★ 数据库文件在服务运行时是被 SQLite 独占打开的（Windows 上 rename 覆盖
                //   会以共享冲突失败）。旧实现把失败整个吞掉：restored 不加、skipped 也不加，
                //   用户只看到「已恢复 N 个文件」，其中并不包含 panel.db —— 重启后用量台账
                //   还是旧的。这里必须逐文件说明原因。
                if is_db_file {
                    db_pending.push(name.to_string());
                    skipped.push(format!(
                        "{name}（暂存失败：{e}；请先停止 aigw 服务再恢复，或手动替换 data 目录下的 {name}）"
                    ));
                } else {
                    skipped.push(format!("{name}（写入失败：{e}）"));
                }
            }
        }
    }
    // 一个都没恢复成功时必须报错：原实现一律 200「已恢复文件」，用户以为恢复成功、
    // 重启后配置还是旧的（备份格式不对/文件名非法时最容易踩）。
    if restored == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "restored": 0,
                "skipped": skipped,
                "message": "没有任何文件被恢复；请检查备份包内的文件名与内容（允许：*.json / panel.db / panel.db-wal / panel.db-shm）",
            })),
        )
            .into_response();
    }
    // 恢复后重启服务即全量生效（custom/free/accounts 均以 data 目录文件为源）
    let mut out = json!({
        "ok": true,
        "restored": restored,
        "skipped": skipped,
        "dir": dd.to_string_lossy(),
        "restart_required": true,
        "note": if skipped.is_empty() {
            "已恢复文件；重启服务后全量生效。旧文件保留为 .bak-<ts>-* 以便回滚。"
        } else {
            "部分文件已恢复，但仍有文件被跳过（见 skipped）；重启服务后生效，被跳过的文件需要先停服务再手动处理。"
        },
    });
    // 数据库文件的落地语义要单独说清楚：它们被写成 *.restore-pending，由下次启动时
    // Store::open 原子替换（服务运行时 SQLite 独占打开 panel.db，就地覆盖必失败）。
    out["db_staged"] = json!(db_staged);
    out["backed_up"] = json!(backed_up);
    out["db_restored"] = json!(db_pending.is_empty());
    if !db_pending.is_empty() {
        out["db_pending"] = json!(db_pending);
        out["message"] = json!(format!(
            "配置已恢复，但数据库文件（{}）连暂存都没成功：请先停止 aigw 服务，再用同一备份包恢复一次（或手动替换 {} 目录下的 panel.db*）。",
            db_pending.join("、"),
            dd.to_string_lossy()
        ));
    } else if !db_staged.is_empty() {
        out["message"] = json!(format!(
            "已恢复 {} 个文件；数据库（{}）已暂存为 .restore-pending，重启 aigw 服务后生效（服务运行中无法直接覆盖 panel.db）。",
            restored,
            db_staged.join("、")
        ));
    }
    Json(out).into_response()
}

/// 在线更新检查（对齐 21 号 dsh /v1/status 版本自证纪律）：GET /api/update-check
/// 本地 = 编译版本 + 部署目录时间；远端 = GitHub 仓库最新 main commit。
async fn update_check() -> Json<Value> {
    // 60s 缓存：前端「关于」页每次刷新都会打这个接口，而它要同步等 GitHub API
    // （实测 3s）。没有缓存时每次切页都卡 3 秒，纯属浪费。
    let now = Instant::now();
    {
        let g = UPDATE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = g.as_ref() {
            if now.duration_since(*at) < UPDATE_TTL {
                return Json(v.clone());
            }
        }
    }
    let dd = aigw_relay::proxy::default_data_dir();
    let exe_ts = std::env::current_exe()
        .ok()
        .and_then(|p| p.metadata().ok())
        .map(|m| {
            m.modified()
                .ok()
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis())
                .unwrap_or(0)
        })
        .unwrap_or(0);
    // 远端最新 commit（GitHub API，8s 超时；失败返回 offline 标记不阻断）。
    // 走 reqwest（已有依赖）而不是 `curl` 子进程：精简 Windows / 沙箱里没有 curl 时
    // 旧实现永远 offline，且 `thread::spawn(...).join()` 会阻塞 tokio worker。
    let remote: Value = match aigw_relay::auth_login::auth_http()
        .get("https://api.github.com/repos/520042/aigw-gateway-panel/commits/main")
        .header(header::USER_AGENT, "aigw-panel")
        .header(header::ACCEPT, "application/vnd.github+json")
        .timeout(Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.json::<Value>().await.unwrap_or(Value::Null),
        Ok(r) => {
            tracing::debug!("update-check：GitHub 返回 {}", r.status());
            Value::Null
        }
        Err(e) => {
            tracing::debug!("update-check：请求失败 {e}");
            Value::Null
        }
    };
    let remote_sha = remote.get("sha").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let remote_date = remote
        .get("commit")
        .and_then(|c| c.get("committer"))
        .and_then(|c| c.get("date"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let out = json!({
        "ok": true,
        "local": {
            "version": env!("CARGO_PKG_VERSION"),
            "milestone": "v2-final",
            "exe_built_ts": exe_ts,
            "data_dir": dd.to_string_lossy(),
        },
        "remote": {
            "sha": remote_sha,
            "date": remote_date,
            "reachable": !remote_sha.is_empty(),
        },
        "note": "本地 Rust 工程更新：git pull 后 cargo build + 拷贝 aigw.exe 与 providers/ 覆盖部署目录。",
    });
    {
        let mut g = UPDATE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some((Instant::now(), out.clone()));
    }
    Json(out)
}

/// 更新检查结果缓存（60s）与上次成功时间。
static UPDATE_CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
const UPDATE_TTL: Duration = Duration::from_secs(60);

/// 编译目标三元组（`/api/health.build.target`）。
///
/// ★ 本机同时存在 gnu / msvc 两套工具链，过去真的踩过「跑起来的是另一套产物」的坑；
///   把 target 报出来，就不用再靠猜。
const BUILD_TARGET: &str = if cfg!(target_env = "msvc") {
    "x86_64-pc-windows-msvc"
} else if cfg!(target_env = "gnu") {
    "x86_64-pc-windows-gnu"
} else if cfg!(target_os = "linux") {
    "linux"
} else {
    "unknown"
};

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// 登录会话（批2）：POST /api/auth
///   {"action":"start","platform":"wb-gateway"|"minimax"|"apk-yuanbao"|...}
///   {"action":"poll","id":"ls-..."}
///   {"action":"save","platform":"apk-yuanbao","secret":"Cookie..."}  ← 跳转登录手动粘贴闭环
/// 成功 token/cookie 自动落账号池。
async fn auth_action(State(st): State<AppState>, body: Bytes) -> Response {
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "start" => {
            let platform = v.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            if platform.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 platform"}))).into_response();
            }
            let mut out = st.login.start(&st.client, platform, &st.specs).await;
            // 跳转类登录（会给 login_url）依赖本地回调端口；端口没绑上时自动捕获注定失败，
            // 必须当场说清楚，别让用户等一个永远不会到的回调。
            if out.get("login_url").and_then(|x| x.as_str()).map(|u| !u.is_empty()).unwrap_or(false) {
                let ports = aigw_relay::auth_login::callback_port_status();
                let failed: Vec<Value> = ports
                    .iter()
                    .filter(|(_, ok, _)| !ok)
                    .map(|(p, _, err)| json!({"port": p, "error": err}))
                    .collect();
                if !failed.is_empty() {
                    let list = ports
                        .iter()
                        .filter(|(_, ok, _)| !ok)
                        .map(|(p, _, e)| format!("{p}（{e}）"))
                        .collect::<Vec<String>>()
                        .join("、");
                    if let Some(o) = out.as_object_mut() {
                        o.insert("callback_warning".to_string(), json!(format!(
                            "本机回调端口没绑上：{list}。自动捕获授权回调不可用 —— 请在浏览器里完成授权后，把跳转后地址栏的完整 URL 粘贴到下方输入框，点「提交回调」。"
                        )));
                    }
                }
            }
            Json(out).into_response()
        }
        "poll" => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if id.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 id"}))).into_response();
            }
            let out = st.login.poll(&st.client, id, &st.accounts).await;
            // 又登录了一次（覆盖了同 platform+name 的旧行）→ 清掉旧凭据留下的
            // 号池治理状态与会话粘性，否则新凭据照样被上一次的 401 拉黑挡着。
            if out.get("credential_replaced").and_then(|x| x.as_bool()).unwrap_or(false) {
                if let Some(acc) = out.get("account_id").and_then(|x| x.as_str()) {
                    on_credential_replaced(&st, acc);
                }
            }
            Json(out).into_response()
        }
        "save" => {
            let platform = v.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            let secret = v.get("secret").and_then(|x| x.as_str()).unwrap_or("");
            if platform.is_empty() || secret.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 platform 或 secret"}))).into_response();
            }
            // ★ 凭据长度闸门：实测贴 5MB 的 secret 会 200 落盘 —— accounts.json 从 17KB
            //   涨到 7MB，之后每次读账号表 / 写盘 / 备份打包都变慢，验活还会拿这坨字节
            //   去打上游一次。真实 Cookie / JWT 序列化后最长也就几十 KB。
            if secret.len() > MAX_CREDENTIAL_LEN {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "message": format!(
                        "凭据过长（{} 字节，上限 {} 字节）。真实的 Cookie / Token 通常只有几 KB，把整页内容或整个浏览器存储贴进来多半是贴错了；",
                        secret.len(), MAX_CREDENTIAL_LEN
                    )})),
                )
                    .into_response();
            }
            let saved = st.login.save(platform, secret, &st.accounts);
            // 重新粘贴凭据覆盖了同一行 → 同上，清治理状态与粘性绑定。
            if saved.get("credential_replaced").and_then(|x| x.as_bool()).unwrap_or(false) {
                if let Some(acc) = saved.get("account_id").and_then(|x| x.as_str()) {
                    on_credential_replaced(&st, acc);
                }
            }
            // 保存成功后即时验活（cookie 平台走各自真实端点），让用户立刻知道 Cookie 是否有效
            if saved.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                invalidate_models_cache(&st);
                let specs = st.all_specs();
                if let Some(spec) = specs.iter().find(|s| s.id == platform) {
                    let ok = st.client.verify_account(spec, secret).await;
                    // 记进号池健康快照：面板账号表立刻能看出这个号是否可用。
                    if let Some(acc_id) = st.accounts.id_of(platform, secret) {
                        st.pool.mark_health(&acc_id, ok, "");
                    }
                    // 失败原因按平台类型分开说：抓错方向的提示比「失败了」更难用。
                    let why = if spec.upstream_kind == "kiro" {
                        "确认 accessToken 是否过期、profileArn 是否与账号匹配（建议从 Kiro 客户端重新导出完整 JSON）"
                    } else if spec.upstream_kind == "qwen-web" || spec.upstream_kind == "mimo" {
                        "确认 token 与完整 Cookie 都已粘贴（缺字段、缺 Cookie 或已过期都会失败）"
                    } else if spec.auth == aigw_registry::AuthKind::ApiKey {
                        "确认 API Key 有效、额度未用尽、未被风控"
                    } else if spec.auth == aigw_registry::AuthKind::OAuthDevice {
                        "设备授权可能已过期，重新走一次登录即可"
                    } else if spec.auth == aigw_registry::AuthKind::SmsCode {
                        "短信登录拿到的 session 有时效（约 14 天），过期后重新走一次短信登录即可"
                    } else if spec.auth == aigw_registry::AuthKind::QrScan {
                        // 扫码类（apk-loomy / apk-raccoon）：拿到的 session / access_token 有时效，
                        // 旧实现落进 cookie 兜底文案会让人去查 Cookie 字段，而这类平台压根不收 Cookie。
                        "扫码拿到的 session / access_token 可能已过期，重新扫一次即可（也可以用抽屉里的「手动粘贴」方式）"
                    } else {
                        "请确认所需 Cookie 字段是否完整、是否已过期（可重新复制后再保存）"
                    };
                    // ★ 与 /api/accounts 的 verify 保持一致：验活结论要落到平台卡片上。
                    //   保存凭据这个动作本身就是用户的「修好它」尝试，成功必须清掉旧原因，
                    //   失败要把原因写上去 —— 否则用户回到接入源页看到的还是上一次的旧原因。
                    let mut obj = saved;
                    obj["verified"] = json!(ok);
                    if ok {
                        st.last_err.clear(platform);
                        obj["message"] = json!("凭据已保存，验活通过 ✓");
                    } else {
                        st.last_err.note(platform, 0, &format!("验活未通过：{why}"));
                        obj["message"] = json!(format!("凭据已保存，但验活未通过：{why}"));
                    }
                    obj["last_error"] = st.last_err.json(platform);
                    return Json(obj).into_response();
                }
            }
            Json(saved).into_response()
        }
        // 短信验证码登录（讯飞 Loomy）：两步 —— 先发码拿 msgid，再交验证码换 session。
        // 为什么单独两个 action 而不是复用 start/poll：短信没有「轮询上游状态」这一步，
        // 全程由用户把验证码送进来，硬套会话状态机只会让前端更绕。
        "sms_send" => {
            let platform = v.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            let phone = v.get("phone").and_then(|x| x.as_str()).unwrap_or("");
            if platform.is_empty() || phone.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 platform 或 phone"}))).into_response();
            }
            // 与扫码那条同源：认证类请求都走 auth_http（独立的超时与 UA，不占转发连接池）。
            let http = aigw_relay::auth_login::auth_http();
            match aigw_relay::auth_login::loomy_send_sms(&http, phone).await {
                Ok(msgid) => Json(json!({
                    "ok": true,
                    "msgid": msgid,
                    "message": "验证码已发出，填进来即可（短信通常几秒内到）",
                }))
                .into_response(),
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        "sms_verify" => {
            let platform = v.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            let phone = v.get("phone").and_then(|x| x.as_str()).unwrap_or("");
            let code = v.get("code").and_then(|x| x.as_str()).unwrap_or("");
            let msgid = v.get("msgid").and_then(|x| x.as_str()).unwrap_or("");
            if platform.is_empty() || phone.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 platform 或 phone"}))).into_response();
            }
            let http = aigw_relay::auth_login::auth_http();
            let session =
                match aigw_relay::auth_login::loomy_verify_sms(&http, phone, code, msgid).await {
                    Ok(s) => s,
                    Err(e) => {
                        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response();
                    }
                };
            // 拿到 session 就按普通凭据入库（与手动粘贴同一条路径：覆盖同 platform+name 的旧行、
            // 清掉旧凭据留下的号池治理状态），再即时验活。
            let saved = st.login.save(platform, &session, &st.accounts);
            if saved.get("credential_replaced").and_then(|x| x.as_bool()).unwrap_or(false) {
                if let Some(acc) = saved.get("account_id").and_then(|x| x.as_str()) {
                    on_credential_replaced(&st, acc);
                }
            }
            invalidate_models_cache(&st);
            let mut obj = saved;
            let specs = st.all_specs();
            if let Some(spec) = specs.iter().find(|s| s.id == platform) {
                let ok = st.client.verify_account(spec, &session).await;
                if let Some(acc_id) = st.accounts.id_of(platform, &session) {
                    st.pool.mark_health(&acc_id, ok, "");
                }
                obj["verified"] = json!(ok);
                if ok {
                    st.last_err.clear(platform);
                    obj["message"] = json!("短信登录成功，验活通过 ✓");
                } else {
                    let why = "session 已拿到但验活没过：可能该账号还没开通对应服务，或需要先在官网完成一次绑定";
                    st.last_err.note(platform, 0, &format!("验活未通过：{why}"));
                    obj["message"] = json!(format!("短信登录成功，但验活未通过：{why}"));
                }
            }
            obj["last_error"] = st.last_err.json(platform);
            Json(obj).into_response()
        }

        // 密码登录（讯飞 Loomy）：与短信登录是同一份 session 产物，只是走 RSA 加密那条链路，
        // 好处是**全自动** —— 不需要人读验证码，可以拿来做 session 到期自动续期。
        "pwd_login" => {
            let platform = v.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            let loginid = v.get("loginid").and_then(|x| x.as_str()).unwrap_or("");
            let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
            if platform.is_empty() || loginid.trim().is_empty() || password.is_empty() {
                return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 platform / loginid / password"}))).into_response();
            }
            let http = aigw_relay::auth_login::auth_http();
            let session = match aigw_relay::auth_login::loomy_login_password(&http, loginid, password).await {
                Ok(s) => s,
                Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            };
            let saved = st.login.save(platform, &session, &st.accounts);
            if saved.get("credential_replaced").and_then(|x| x.as_bool()).unwrap_or(false) {
                if let Some(acc) = saved.get("account_id").and_then(|x| x.as_str()) {
                    on_credential_replaced(&st, acc);
                }
            }
            invalidate_models_cache(&st);
            let mut obj = saved;
            let specs = st.all_specs();
            if let Some(spec) = specs.iter().find(|s| s.id == platform) {
                let ok = st.client.verify_account(spec, &session).await;
                if let Some(acc_id) = st.accounts.id_of(platform, &session) {
                    st.pool.mark_health(&acc_id, ok, "");
                }
                obj["verified"] = json!(ok);
                if ok {
                    st.last_err.clear(platform);
                    obj["message"] = json!("密码登录成功，验活通过 ✓");
                } else {
                    let why = "session 已拿到但验活没过：可能该账号还没开通对应服务，或需要先在官网完成一次绑定";
                    st.last_err.note(platform, 0, &format!("验活未通过：{why}"));
                    obj["message"] = json!(format!("密码登录成功，但验活未通过：{why}"));
                }
            }
            obj["last_error"] = st.last_err.json(platform);
            Json(obj).into_response()
        }

        "callback" => {
            // 用户粘贴回调 URL（服务器场景）→ 提取 code/state 入队
            let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
            match aigw_relay::auth_login::submit_callback_url(url) {
                Ok(()) => Json(json!({"ok": true, "message": "回调已捕获，点「查询授权状态」完成入库"})).into_response(),
                Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": e}))).into_response(),
            }
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("未知 action：{action}")}))).into_response(),
    }
}

async fn health(State(st): State<AppState>) -> Json<Value> {
    let skipped = st.skipped_specs.as_ref();
    // ok 只在服务本身能跑时为 true；被跳过的 spec 不影响 ok（否则一个写错的
    // 平台文件会让整机健康检查变红、容器被反复重启），但一定要露出来。
    let mut v = json!({
        "ok": true,
        "service": "aigw",
        "version": env!("CARGO_PKG_VERSION"),
        "milestone": "v2-final",
        "platforms": st.specs.len(),
        // 令牌要求与来源：前端据此判断「/v1 的 API Key 能不能随便填」。
        // 令牌值本身永远不通过 API 回显（面板可能被局域网其他设备打开）。
        "token_required": st.current_token().is_some(),
        "token_source": st.token_source_now(),
        // 后台任务：ok_runs / err_runs / last_ok_s_ago / last_err / last_err_s_ago。
        // 「该在 24 小时前跑过一次的任务已经 3 天没动静」只有这里看得见。
        "tasks": st.tasks.snapshot(),
        // 二进制自证：这次部署到底换没换成功，看 uptime_s 是否归零就知道了
        // （version 一整天都不变，exe 的 mtime 又只有登到机器上才看得见）。
        "build": {
            "version": env!("CARGO_PKG_VERSION"),
            "target": BUILD_TARGET,
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "started_at": now_ms().saturating_sub(st.started_at.elapsed().as_millis()),
            "uptime_s": st.started_at.elapsed().as_secs(),
        },
        // 限流现场：被挡过几次、上次多久前、是全局还是每 IP 触顶。
        // 429 只体现在响应里，不放这里的话「面板突然全失败」根本查不到原因。
        "limits": st.limits.snapshot(),
    });
    // 落盘日志现场：日志在滚没滚、占了多大。只报统计不报路径之外的任何内容。
    // 「日志文件不在滚 / 一直 0 字节」在这里一眼可见（e.g. 目录只读、被误删）。
    // 落盘失败现场：只要有任何一个 json 最近一次没写成功，这里就有内容。
    // 各 Store 的 save_locked 只 warn 不返回错误，面板于是照样回「已保存」——
    // 重启后「停用的模型又活了 / 删掉的 key 又回来了」，而用户全程没被告知过。
    // 有这条，「配置怎么没生效」才查得到源头；同名文件写成功后自动清空。
    // ★ 放在 data_dir 判断之外：写盘失败跟「有没有配数据目录」无关。
    if let Some((name, at, err)) = aigw_store::atomic::last_write_failure() {
        v["persist_error"] = json!({
            "file": name,
            "at": at,
            "s_ago": (now_ms().saturating_sub(at as u128)) / 1000,
            "error": err,
        });
    }
    // OAuth 本地回调端口：1455/54545 等被别的程序占着时，授权码根本进不了队列，
    // 用户对着「等待授权中」反复点也查不出为什么。这里把绑定结果报出来。
    v["oauth_callbacks"] = {
        let ports = aigw_relay::auth_login::callback_port_status();
        let failed: Vec<Value> = ports
            .iter()
            .filter(|(_, ok, _)| !ok)
            .map(|(p, _, err)| json!({"port": p, "error": err}))
            .collect();
        json!({
            "bound": ports.iter().filter(|(_, ok, _)| *ok).map(|(p, _, _)| *p).collect::<Vec<u16>>(),
            "failed": failed,
            "ok": failed.is_empty(),
        })
    };
    if let Some(dd) = st.data_dir.as_deref() {
        let (bytes, files) = aigw_store::rotlog::stats(dd);
        v["logs"] = json!({
            "files": files,
            "bytes": bytes,
            "max_bytes": aigw_store::rotlog::MAX_BYTES,
            "keep": aigw_store::rotlog::KEEP,
        });
        // 留档文件（恢复备份的 .bak-*、坏档 .bad-*）现场：数量与占用。
        // 这些文件每恢复一次就多一组，只在启动时按族裁剪；这里报出来，
        // 「数据目录怎么越来越大」才查得到源头。
        let ar = aigw_store::archive::stats(dd);
        v["archives"] = json!({
            "files": ar.files,
            "bytes": ar.bytes,
            "keep_per_family": aigw_store::archive::KEEP_PER_FAMILY,
        });
        // 用量台账现场：条数 + SQLite 文件占用（含 -wal/-shm：WAL 模式下新写入
        // 可能全在 -wal 里，只报主库会把「台账 1.8MB」显示成「4KB 空壳」）。
        // backend 报出来是因为 JSON 降级模式（SQLite 打不开）此前完全不可见，
        // 用户只会发现「重启后台账没了」。
        let db_bytes: u64 = ["panel.db", "panel.db-wal", "panel.db-shm"]
            .iter()
            .filter_map(|n| std::fs::metadata(dd.join(n)).ok())
            .map(|m| m.len())
            .sum();
        v["db"] = json!({
            "backend": if st.store.is_db() { "sqlite" } else { "json" },
            "events": st.store.usage_count(),
            "bytes": db_bytes,
        });
    }
    if !skipped.is_empty() {
        v["skipped_specs"] = json!(skipped
            .iter()
            .map(|(f, r)| json!({"file": f, "reason": r}))
            .collect::<Vec<_>>());
        v["warning"] = json!(format!(
            "{} 个平台 spec 非法已被跳过（不会出现在面板与 /v1/models）：{}",
            skipped.len(),
            skipped
                .iter()
                .map(|(f, r)| format!("{f}：{r}"))
                .collect::<Vec<_>>()
                .join("；")
        ));
    }
    Json(v)
}

/// 解析台账查询的时间窗参数（unix 秒，含两端）。
///
/// 两个端点（/api/usage 与 /api/requests）共用同一份口径，避免各写一遍后走偏。
/// ★ 非有限值（`NaN` / `inf` / `-inf`）必须当成「没传」，不能当成一个时间点：
///   `"NaN".parse::<f64>()` 是 Ok(NaN)，而 `ts >= NaN` 永远为假 —— 实测
///   `/api/usage?from=NaN` 会返回 200 + 全零台账，用户看到的是「数据全没了」。
fn parse_window(params: &HashMap<String, String>) -> Result<(f64, f64), Response> {
    let from = params
        .get("from")
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.0);
    let to = params
        .get("to")
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(1e18);
    if from > to {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "message": "from 不能大于 to"})),
        )
            .into_response());
    }
    Ok((from, to))
}

async fn usage(
    Query(params): Query<HashMap<String, String>>,
    State(st): State<AppState>,
) -> Response {
    // 时间窗参数：非有限值（NaN / inf）过去会被静默当成「匹配不到任何事件」，
    // 返回 200 + 全零台账 —— 用户拼错一个数字就看到「数据全没了」，会以为库坏了。
    // 现在只接受有限数，其余按「没传」处理（from=0 / to=无上界）。
    let (from, to) = match parse_window(&params) {
        Ok(w) => w,
        Err(r) => return r,
    };
    // `by` 此前**完全没被读取**：`/api/usage?by=bogus` 照样返回三张表，客户端以为
    // 过滤生效了，其实只是白拿了一份更大的响应。现在显式白名单：
    //   不传 / all → 全部三张（与旧行为一致）；platform / model / account → 只要那张。
    let by = params
        .get("by")
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if !by.is_empty() && by != "all" && !USAGE_BY_KINDS.contains(&by.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "message": format!(
                    "不认识的 by={by}：只接受 platform / model / account / all（不传等于 all）"
                ),
            })),
        )
            .into_response();
    }
    // limit: 分组视图只回前 N 名（默认 30）。UI 上写着「共 89 个，只列最多的 30 个」
    // 却没有点开看全的路子 —— 用户想找「第 40 名那个是谁」只能去翻 /api/requests 明细。
    // 这里放开一个可调上限（夹到 200，避免一次把几 MB 的台账全吐出来）。
    // 前端同时传 limit（明细页大小）与 group_limit（分组表名次），这里让分组视图
    // 认 group_limit、明细接口认 limit，两边互不干扰。
    let group_limit = params.get("group_limit").or_else(|| params.get("limit"));
    let limit = match group_limit {
        None => USAGE_TOP_N,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => n.min(USAGE_TOP_MAX),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "ok": false,
                        "message": format!(
                            "limit 必须是正整数（最多 {USAGE_TOP_MAX}）；不传等于 {USAGE_TOP_N}"
                        ),
                    })),
                )
                    .into_response();
            }
        },
    };
    let want = |k: &str| by.is_empty() || by == "all" || by == k;
    let mut out = st.store.usage_window_totals(from, to);
    if let Some(o) = out.as_object_mut() {
        o.insert("from".into(), json!(from));
        // to 的默认值是「无上界」的哨兵值，回给前端时还原成 null 更好读
        o.insert("to".into(), if to >= 1e17 { Value::Null } else { json!(to) });
        // 平台分组与 /api/requests 的 by_platform 同源同口径（都走 GROUP BY platform），
        // 在这里回一份让 /api/usage 自足：只想知道「谁在吃额度」的调用方不必再拉一页明细。
        if want("platform") {
            o.insert(
                "byPlatform".into(),
                top_groups(&st.store.usage_window_by_platform(from, to), limit),
            );
        }
        if want("model") {
            o.insert(
                "byModel".into(),
                top_groups(&st.store.usage_window_by_model(from, to), limit),
            );
        }
        if want("account") {
            o.insert(
                "byAccount".into(),
                top_groups(&st.store.usage_window_by_account(from, to), limit),
            );
        }
    }
    Json(out).into_response()
}

/// 分组视图最多回多少行（够定位问题，又不会把响应撑成几 MB）。
const USAGE_TOP_N: usize = 30;

/// `/api/usage?limit=` 的上限：一次翻太多会把响应撑成几 MB，200 足够覆盖常见台账规模。
const USAGE_TOP_MAX: usize = 200;

/// `/api/usage?by=` 接受的分组维度（`all`/不传表示全部）。
const USAGE_BY_KINDS: &[&str] = &["platform", "model", "account"];

/// 把 `{键: 计数}` 的分组对象整理成**按请求数降序**的数组（前 n 名）。
///
/// 为什么排序不放前端：serde_json 的对象是 BTreeMap，序列化顺序是键字典序而不是量，
/// 前端拿到「按平台/模型名排序」的表格毫无信息量；每个客户端各排一遍也浪费。
fn top_groups(v: &Value, n: usize) -> Value {
    let mut rows: Vec<(String, Value)> = match v.as_object() {
        Some(o) => o.iter().map(|(k, x)| (k.clone(), x.clone())).collect(),
        None => Vec::new(),
    };
    rows.sort_by(|a, b| {
        let ra = a.1.get("requests").and_then(|x| x.as_i64()).unwrap_or(0);
        let rb = b.1.get("requests").and_then(|x| x.as_i64()).unwrap_or(0);
        // 请求数相同再按键名字典序，保证同一份数据每次输出一致（便于对比两次拉取）
        rb.cmp(&ra).then_with(|| a.0.cmp(&b.0))
    });
    let total = rows.len();
    rows.truncate(n);
    json!({
        "total": total,
        "rows": rows
            .into_iter()
            .map(|(k, mut x)| {
                if let Some(o) = x.as_object_mut() {
                    o.insert("key".into(), json!(k));
                }
                x
            })
            .collect::<Vec<_>>(),
    })
}

/// FNV-1a 64 位（std 实现，零新增依赖；仅作稳定压缩，不需加密强度）。
fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 客户端可控的会话标识一律折成定长哈希再当键。
///
/// 为什么不直接用原文：`conversation_id` 是请求体里的字符串，长度由调用方决定
/// （实测有人在 `metadata.conversation_id` 里塞整段对话历史）。把它原样存进粘性表
/// 的 HashMap 键，一个 1MB 的 id 就变成 1MB 的驻留内存，几万个不同 id 直接把进程撑爆。
fn key_id(s: &str) -> String {
    format!("k{:016x}", fnv1a(s.as_bytes()))
}

/// 是否是本服务自己生成的会话键形状（`k` + 16 位小写十六进制）。
fn is_key_id(s: &str) -> bool {
    s.len() == 17
        && s.starts_with('k')
        && s.as_bytes()[1..].iter().all(|b| b.is_ascii_hexdigit())
}

/// 会话粘性键（对齐 workbuddy2api 键优先级 + loomy2api「最后一条 user」修正）：
/// ① `metadata.conversation_id` / `conversationId` / `conversation_id` /
///    `prompt_cache_key`（02 号五键优先级）；
/// ② 都没有 → **最后一条** `role=user` 消息内容 FNV-1a。
///    为什么取最后一条：通用客户端重发全历史时首条 user 恒定，以首条为键会让
///    整个会话永久钉在同一账号（单号霸占、其余饿死）；最后一条随对话推进变化。
///
/// 返回值恒为 17 字节以内（`k` + 16 位十六进制，或 `u` + 16 位，或 `anon`）：幂等且定长，
/// 粘性表的条目大小与客户端发什么都无关（条目数量另有上限，见 `STICKY_MAX_ENTRIES`）。
fn session_key(v: &Value) -> String {
    // 协议转换（/v1/messages、/v1/responses）会重建请求体，原始会话标识必须由
    // 转换函数搬到这个私有字段里，否则粘性路由退化成「按最后一条 user 内容哈希」，
    // 多轮对话每轮都换账号，上游的 prompt cache 全丢。
    if let Some(s) = v.get("_aigw_session").and_then(|x| x.as_str()) {
        if !s.is_empty() {
            // 这个字段是 carry_session 写进去的，已经是折过的定长键；**再折一次**会让
            // `/v1/messages`（经 carry_session）与 `/v1/chat/completions`（直接给
            // conversation_id）对同一个会话算出两个不同的键 —— 同一会话跨协议混用时
            // 粘性就断了，上游 prompt cache 全丢。所以自家形状原样放行。
            // 但客户端也能伪造这个顶层字段，故只放行形状，其余照样折（否则又是无界键）。
            if is_key_id(s) {
                return s.to_string();
            }
            return key_id(s);
        }
    }
    if let Some(md) = v.get("metadata").and_then(|m| m.as_object()) {
        for k in ["conversation_id", "conversationId", "prompt_cache_key"] {
            if let Some(s) = md.get(k).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return key_id(s);
                }
            }
        }
    }
    for k in ["conversation_id", "conversationId"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return key_id(s);
            }
        }
    }
    if let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) {
        for m in msgs.iter().rev() {
            if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                let text = m
                    .get("content")
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                return format!("u{:016x}", fnv1a(text.as_bytes()));
            }
        }
    }
    "anon".to_string()
}

/// `/api/requests` 缺省返回条数（与历史行为一致：前端「最近请求」只看 60 条）。
const REQ_PAGE_DEFAULT: usize = 60;
/// `/api/requests` 单页条数上限：再大就是让调用方把整张台账拖走，
/// 面板自己的「显示更多」也只需要到这个量级。
const REQ_PAGE_MAX: usize = 1000;

/// M6 请求历史：一页请求 + 窗口总数 + 按平台聚合（成本归因雏形）。
///
/// 查询参数：`from`/`to`（unix 秒，含两端，缺省全量）、`limit`（缺省 60，上限 1000）、
/// `offset`（缺省 0）。响应里 `matched` 是窗口内的总条数，`has_more` 表示还有下一页——
/// 列表与统计因此永远自洽。
async fn requests(
    Query(params): Query<HashMap<String, String>>,
    State(st): State<AppState>,
) -> Response {
    // 台账量级由 store 侧兜住：usage_window 走 WHERE ts 区间（走 idx_ue_ts），
    // usage_window_by_platform 走 GROUP BY platform，都不再把整张表搬进内存。
    // 可选查询参数 from/to（unix 秒，含两端）；缺省为全量，保持与旧行为兼容。
    let (from, to) = match parse_window(&params) {
        Ok(w) => w,
        Err(r) => return r,
    };
    // 分页：limit 缺省 60（旧行为），上限 REQ_PAGE_MAX；offset 缺省 0。
    // 非法值一律回落缺省而不是报错——前端会用 Date.now() 拼参数，
    // 偶发的空串/NaN 不该让整个页面白屏。
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(REQ_PAGE_DEFAULT)
        .min(REQ_PAGE_MAX);
    // ★ offset 必须在进 store 之前夹住：`offset as i64` 对 u64::MAX 是有符号饱和（得 i64::MAX），
    //   SQLite 把它绑进 `OFFSET ?` 会报 SQLITE_TOOBIG；而 store 里 `if let Ok(rows)` 把错误
    //   丢掉了，于是连接上一个字节都没写 → 客户端看到 curl 52「Empty reply from server」。
    //   台账最多 CAP_EVENTS 条，超过这个数的 offset 只会是空页，直接夹掉最省事。
    let offset = params
        .get("offset")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(aigw_store::CAP_EVENTS as usize);
    // 先把总数查出来：列表只是「这一页」，统计是整个窗口。
    // 两者必须同时返回，否则「列表 60 条 / 统计 500 条」会让人以为台账丢了数据。
    let matched = st.store.usage_window_count(from, to);
    let page = st.store.usage_window_page(from, to, limit, offset);
    let shown = page.len();
    let recent = page
        .into_iter()
        .map(|e| {
            json!({
                "ts": e.ts, "platform": e.platform, "model": e.model,
                "tin": e.tin, "tout": e.tout, "ms": e.ms, "ok": e.ok,
                // 列表要能区分「失败」和「客户端自己取消」，否则用户点一次停止，
                // 明细里就多一条看起来像上游坏了的「失败」。
                "cancelled": e.cancelled,
                // 后台任务（签到/成长）也记在这张表里，model 是动作名而非模型名。
                // 明细里带上类型，用户才不会把「checkin」当成一个调不通的模型。
                "kind": aigw_store::kind_of(&e),
                "upstream": e.upstream,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "recent": recent,
        "matched": matched,
        "shown": shown,
        "limit": limit,
        "offset": offset,
        "has_more": (offset as u64 + shown as u64) < matched,
        "by_platform": st.store.usage_window_by_platform(from, to),
    }))
    .into_response()
}

/// 单平台 live 模型缓存项：(拉取时刻, 模型清单, 这次拉取是否成功)。
/// 失败的平台也缓存（短 TTL），避免每次请求都去撞不可达的公益站。
/// ★ 第三项 ok 是 stale-on-error 的关键：拉取失败时**保留上一次的成功清单**继续
///   发布/路由，只把 ok 标成 false 让 TTL 走 120s 短档以便较快重试。旧实现失败
///   即塞空清单——慢公益站一次 12s 超时或一轮 429 限流，就把这个平台的全部模型
///   从 /v1/models 和 spec_for_model 的 live 兜底里清空最长 120 秒，chat 直接
///   503「未知模型」，而平台本身明明能对话。
type ModelsEntry = (Instant, Vec<String>, bool);

/// 单平台清单的缓存时长：失败（ok=false）或空清单走短档，让上游恢复后能较快
/// 重新探测；成功且非空走长档。失败但带着旧清单的条目在短档窗口内照常可用。
fn models_ttl(ids: &[String], ok: bool) -> Duration {
    if ids.is_empty() || !ok {
        MODELS_TTL_ERR
    } else {
        MODELS_TTL_OK
    }
}

/// live 拉取并发上限。74 个启用平台里 56 个是 live 策略，串行跑最坏要 20 分钟；
/// 并发 12 路可把冷启动压到十几秒，同时不至于把本机网络/代理打满。
const MODELS_FETCH_CONCURRENCY: usize = 12;
/// 成功结果的单平台缓存 TTL。
const MODELS_TTL_OK: Duration = Duration::from_secs(600);
/// 失败/空结果的单平台缓存 TTL（短一些，让上游恢复后能较快重新探测）。
const MODELS_TTL_ERR: Duration = Duration::from_secs(120);
/// 整表缓存 TTL。
const MODELS_TTL_ALL: Duration = Duration::from_secs(60);

/// 公益站刷新最多等模型预热多久（`warm_done` 信号的上限兜底）。
///
/// 预热实测 3–12 秒；卡在死上游时最坏 80 个平台 × 12s 超时 ÷ 12 并发 ≈ 80 秒。
/// 等满 60 秒仍没结束就照常起跑：公益站刷新比「避免两批并发」更重要。
pub const WARM_WAIT_CAP: Duration = Duration::from_secs(60);

/// 模型名长度上限。合法形态最长的也不过「很长的模型 id@平台 id」，200 字符足够；
/// 超长输入只可能是粘贴错误，让它在写进 flags.json 之前就被挡住。
const MAX_MODEL_NAME_LEN: usize = 200;

/// 单条凭据（Cookie / Token / 扫码 session）的长度上限。
///
/// 真实凭据序列化后最长也就几十 KB（最大的一类是带完整 Cookie 头的网页端登录态）。
/// 没有闸门时实测能贴进 5MB 并成功落盘：accounts.json 从 17KB 涨到 7MB，之后每次
/// 读账号表 / 写盘 / 打包备份都变慢，验活还会拿这坨字节去打上游一次。
const MAX_CREDENTIAL_LEN: usize = 64 * 1024;

/// 对话请求体上限（32MB，对齐 Python 版）。axum 默认只有 2MB，
/// 长上下文请求会被框架直接 413 掉，且错误体不是 OpenAI 信封。
const MAX_CHAT_BODY: usize = 32 * 1024 * 1024;

/// 组装 /v1/models 响应体（live 优先、静态回退、去重、过滤停用模型）。
/// 返回 `(body, index)`：index = (平台 id, 模型名)，供 `spec_for_model` 反查。
fn assemble_models(st: &AppState) -> (Value, Vec<(String, String)>) {
    let per = st
        .models_cache_per
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let disabled = st.flags.disabled_models();
    let mut data: Vec<Value> = Vec::new();
    let mut index: Vec<(String, String)> = Vec::new();
    // 同名模型去重：实测 47 个模型名被多个平台同时声明（kilo 的 18 个 free 模型
    // 里 15 个也被 api-openrouter 声明）。OpenAI 客户端按 id 建索引，重复 id 会让
    // 列表出现重影、选择器行为不确定。保留第一个（usable 平台优先）；
    // 想走别的平台仍可用 `模型id@平台` 显式指定。
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for s in st.all_specs().iter().filter(|s| st.platform_usable(s)) {
        let mut ids: Vec<String> = s.models.clone();
        if s.models_policy == aigw_registry::ModelsPolicy::Live {
            if let Some((_, live, _)) = per.get(&s.id) {
                if !live.is_empty() {
                    ids = live.clone();
                }
            }
        }
        for m in ids {
            if disabled.contains(aigw_store::bare_model(&m)) {
                continue;
            }
            if !seen.insert(m.clone()) {
                continue;
            }
            index.push((s.id.clone(), m.clone()));
            data.push(json!({"id": m, "object": "model", "owned_by": s.id}));
        }
    }
    (json!({"object": "list", "data": data}), index)
}

async fn models(st: State<AppState>, req: Request) -> Response {
    // `?platform=<id>`：只看某个平台的清单。
    // 有它是因为面板/错误文案会让人「去查这个平台到底有哪些模型」——没有过滤就只能
    // 拿到全部 148 个再自己找 owned_by，等于把工作推回给用户。
    let want_platform = req
        .uri()
        .query()
        .and_then(|q| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == "platform")
                .map(|(_, v)| v.to_string())
        })
        .filter(|v| !v.is_empty());
    // 这把 key 的模型白名单：它决定了「这个调用方能看到哪些模型」。
    // 不过滤的后果不是安全问题（chat 侧早已 403），而是**可用性**问题：客户端
    // 拿到 148 个模型，逐个试过去 147 个 403，日志里全是对不上号的报错。
    let key = req.extensions().get::<AccessKeyCtx>().cloned();
    // ① 整表缓存命中
    if let Some((at, _, body)) = st
        .models_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        if at.elapsed() < MODELS_TTL_ALL {
            return (
                [(header::CONTENT_TYPE, "application/json")],
                filter_models_by_platform(
                    &st,
                    &filter_models_for_key(&merge_combo_models(&st, &body), key.as_ref()),
                    want_platform.as_deref(),
                )
                .to_string(),
            )
                .into_response();
        }
    }
    // ② 单飞：冷启动/过期时只让一个请求去拉上游，其余等待复用结果。
    //    旧实现没有这层，N 个并发请求会各自串行跑 56 个平台，直接把进程压死。
    let _guard = st.models_lock.clone().lock_owned().await;
    // 拿到锁后再查一次：可能已被上一个持锁者刷新
    if let Some((at, _, body)) = st
        .models_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        if at.elapsed() < MODELS_TTL_ALL {
            return (
                [(header::CONTENT_TYPE, "application/json")],
                filter_models_by_platform(
                    &st,
                    &filter_models_for_key(&merge_combo_models(&st, &body), key.as_ref()),
                    want_platform.as_deref(),
                )
                .to_string(),
            )
                .into_response();
        }
    }

    // ③ 只对「缓存过期」的平台发请求（按平台独立 TTL，失败也会被缓存）。
    let to_fetch: Vec<ProviderSpec> = {
        let g = st.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
        st.all_specs()
            .into_iter()
            .filter(|s| st.platform_usable(s))
            .filter(|s| s.models_policy == aigw_registry::ModelsPolicy::Live)
            .filter(|s| match g.get(&s.id) {
                None => true,
                Some((at, ids, ok)) => at.elapsed() >= models_ttl(ids, *ok),
            })
            .collect()
    };
    // ④ 并发拉取（有界并发；豁免凭据由 fetch_live_models 自己取）
    st.fetch_live_models(to_fetch).await;

    // ⑤ 组装（抽成独立函数，便于测试与复用）
    let (body, index) = assemble_models(&st);
    let mut g = st.models_cache.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some((Instant::now(), index, body.clone()));
    (
        [(header::CONTENT_TYPE, "application/json")],
        filter_models_by_platform(
                    &st,
                    &filter_models_for_key(&merge_combo_models(&st, &body), key.as_ref()), want_platform.as_deref())
            .to_string(),
    )
        .into_response()
}

/// 把模型链作为虚拟条目并进 `/v1/models`。
///
/// 不进清单的后果很实际：客户端（Cursor / LibreChat / Claude Code）都从 `/v1/models`
/// 拉下拉框，链不出现在里面就等于用户没法选它 —— 只能手输，还得先知道 id。
///
/// `owned_by` 用 `combo`：平台筛选（`?platform=combo`）因此能把所有链单独列出来，
/// 也不会与任何真实平台 id 撞（平台 id 不允许以裸 `combo` 充当链前缀）。
fn merge_combo_models(st: &AppState, body: &Value) -> Value {
    let combos = st.combos.rows();
    if combos.is_empty() {
        return body.clone();
    }
    let mut out = body.clone();
    let Some(arr) = out.get_mut("data").and_then(|d| d.as_array_mut()) else {
        return out;
    };
    // 只有启用的链才发布：停用的链在 chat 侧是 404，出现在清单里会让客户端选到一个死条目
    for c in combos.iter().filter(|c| c.enabled) {
        arr.push(json!({
            "id": c.id,
            "object": "model",
            "created": 0,
            "owned_by": "combo",
            "name": c.name,
        }));
    }
    out
}

/// `?platform=<id>` 的附加说明：这个平台叫什么、开没开、有没有凭据、声明了多少模型。
///
/// 因为用户是从「平台 X 不可用」的报错点过来的，最想知道的是**为什么**它不可用。
/// 停用平台不在 `assemble_models` 的输出里（那是「能用的模型」清单），所以光筛 `data`
/// 会得到一个空清单 —— 用户容易读成「这个平台一个模型都没有」，而真相是它有模型、只是停用。
fn platform_note(st: &AppState, plat: &str, models_count: usize) -> Value {
    let specs = st.all_specs();
    let Some(s) = specs.iter().find(|s| s.id == plat) else {
        return json!({
            "id": plat,
            "known": false,
            "enabled": false,
            "message": format!("没有叫 {plat} 的平台（/api/platforms 可看全部 id）"),
        });
    };
    let enabled = s.enabled;
    let routable = st.platform_routable(s);
    // ★ 别把「平台不可用」一律说成「没凭据」：能转发需要四个条件，缺哪个排查方向
    //   完全不同（填 Key / 改协议 / 换地址）。只判 !routable 会让 `format="anthropic"`
    //   或 `base_url="@lobster"` 的平台显示「没有可用凭据」—— 用户去填了 Key 还是不可用。
    let reason = if !enabled {
        "这个平台是停用的".to_string()
    } else if !routable {
        "这个平台没有可用凭据（去「平台」页填 Key / 扫码 / 登录）".to_string()
    } else if !st.platform_supported(s) {
        format!(
            "这个平台的协议是 {}，转发层只支持 openai 透传（改 format 或换一个 OpenAI 兼容地址）",
            s.format
        )
    } else if !s.chat_supported {
        "这个平台只做登录验活、不支持对话（chat_supported = false）".to_string()
    } else if s.base_url.starts_with('@') {
        format!("这个平台的上游地址是占位符 {}，还没配置真实网关地址", s.base_url)
    } else {
        String::new()
    };
    json!({
        "id": s.id,
        "known": true,
        "enabled": enabled,
        "routable": routable,
        // 平台声明的模型数（含此刻不在 data 里的）：让人看得出「有模型，只是不可用」
        "declared": s.models.len(),
        "listed": models_count,
        "message": reason,
    })
}

/// 按 `?platform=<id>` 只留某个平台的模型；不传则原样返回。
///
/// 平台 id 不认识时返回**空清单**而不是整表：`?platform=typo` 回 148 条会让人以为
/// 「这个平台确实有 148 个模型」。空清单配 `object:"list"` 语义上就是「没有」，
/// 客户端把它当「该平台暂无模型」处理即可；顶层另附 `platform` 段说明缘由。
fn filter_models_by_platform(st: &AppState, body: &Value, platform: Option<&str>) -> Value {
    let Some(plat) = platform else { return body.clone() };
    let Some(arr) = body.get("data").and_then(|d| d.as_array()) else {
        return body.clone();
    };
    let kept: Vec<Value> = arr
        .iter()
        .filter(|m| m.get("owned_by").and_then(|o| o.as_str()) == Some(plat))
        .cloned()
        .collect();
    let mut out = body.clone();
    if let Some(o) = out.as_object_mut() {
        o.insert("data".to_string(), json!(kept));
        o.insert("platform".to_string(), platform_note(st, plat, kept.len()));
    }
    out
}

/// 按访问密钥的白名单裁剪 `/v1/models` 的响应。
///
/// 无 key（匿名/面板令牌）→ 原样返回整表；有白名单的 key → 只留它能调的模型。
/// 白名单匹配规则与 chat 侧一致（裸名放行同名的所有 `模型@平台` 组合），但这里**不能用
/// `allows()` 逐个判**：`allows()` 要的是调用时的模型串，而清单里的 id 是裸名，
/// 用 `allows()` 判会漏掉「白名单写的是 `模型@平台`」的情形。所以两侧各展一次。
fn filter_models_for_key(body: &Value, key: Option<&AccessKeyCtx>) -> Value {
    let Some(k) = key else { return body.clone() };
    if k.0.allow_models.is_empty() {
        return body.clone();
    }
    let allow = &k.0.allow_models;
    let Some(arr) = body.get("data").and_then(|d| d.as_array()) else {
        return body.clone();
    };
    let kept: Vec<Value> = arr
        .iter()
        .filter(|m| {
            let id = m.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let owner = m.get("owned_by").and_then(|x| x.as_str()).unwrap_or("");
            allow.iter().any(|a| {
                a == id
                    || (!a.contains('@') && a == aigw_store::bare_model(id))
                    || *a == format!("{id}@{owner}")
            })
        })
        .cloned()
        .collect();
    let mut out = body.clone();
    out["data"] = json!(kept);
    out
}


/// 让 /v1/models 的整表缓存立即失效。
///
/// 整表缓存 TTL 是 60s，但「停用平台 / 删账号 / 新增自定义 API」这类操作
/// 用户会立刻回看模型清单——等 60s 会让人以为开关没生效。
fn invalidate_models_cache(st: &AppState) {
    // 失效必须真的发生：漏清的后果是「停用平台/删账号/改自定义 API」后
    // /v1/models 与路由索引还拿旧清单，用户以为开关没生效。
    {
        let mut g = st.models_cache.lock().unwrap_or_else(|e| e.into_inner());
        *g = None;
    }
    // 每平台清单（models_cache_per，TTL 600s）也必须一起清。
    // 踩过的坑：只清整表时，删账号/停平台后 per-平台缓存里的旧模型名仍能被
    // spec_for_model 的 ③' 级命中并尝试转发——用户看到的是 401「无可用账号」，
    // 而不是本该出现的 503「未知模型」，排查方向被彻底带偏。
    {
        let mut g = st.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
        g.clear();
    }
}

/// 把原始请求的会话标识搬到转换后的 OpenAI 请求体上。
///
/// /v1/messages 与 /v1/responses 的转换函数会重建请求体，metadata 不跟着走，
/// 于是粘性路由只能退化成「按最后一条 user 内容哈希」——多轮对话里用户每说一句
/// 就可能换一个账号，上游 prompt cache 全丢、免费车道的额度也被摊薄到多个号上。
fn carry_session(oa: &mut Value, orig: &Value) {
    let sk = session_key(orig);
    if sk != "anon" {
        oa["_aigw_session"] = json!(sk);
    }
}

/// 某个模型名是不是「真存在」——静态清单或 live 缓存里有人声明它。
///
/// 用于面板写操作（停用模型）的入参校验：拼错的名字一旦写进 flags.json 的
/// disabled_models 就永远匹配不到任何请求，还会让「已停用模型」列表越积越多。
/// 故意**不**要求平台可用：用户可能想先停用一个暂时没凭据的平台上的模型，
/// 只要清单里有它就该允许。
fn model_known(st: &AppState, model: &str) -> bool {
    let bare = aigw_store::bare_model(model);
    let specs = st.all_specs();
    if specs
        .iter()
        .any(|s| s.models.iter().any(|m| m == model || m == &bare))
    {
        return true;
    }
    let g = st.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
    g.values()
        .any(|(_, ids, _)| ids.iter().any(|m| m == model || m == &bare))
}

/// 上游失败要落成「客户端能识别的错误码」。
///
/// 踩过的坑：上游返回 HTTP 200 但 body 是 HTML / 非 JSON（网页版逆向最常见的
/// 失效形态），`RelayError.status` 会被填成 200，于是 `openai_error(200, ...)`
/// 回了一个 **HTTP 200 + 错误 JSON**——OpenAI SDK 把它当成功响应解析，直接抛
/// 解析异常，调用方完全看不到"上游返回了登录页"这个真实原因。
fn upstream_status(status: u16) -> u16 {
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
fn error_type(status: u16) -> &'static str {
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

fn openai_error(status: u16, msg: &str) -> Response {
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
fn openai_error_coded(status: u16, typ: &str, code: &str, msg: &str) -> Response {
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
fn msg_with_hint(msg: &str, hint: Option<&str>) -> String {
    match hint {
        Some(h) if !msg.contains("建议：") => format!("{msg}（建议：{h}）"),
        _ => msg.to_string(),
    }
}

/// 造一个 OpenAI 错误信封（message + type + code [+ hint]）。
fn upstream_error_json(status: u16, message: &str, hint: Option<&str>) -> Response {
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

fn upstream_error_response(status: u16, body: &str, hint: Option<&str>) -> Response {
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
fn region_label(region: &str) -> &'static str {
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
fn upstream_hint(spec: &ProviderSpec, status: u16, class: Option<ErrorClass>, body: &str) -> Option<String> {
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

#[allow(clippy::too_many_arguments)]
fn record(
    store: &Store,
    pool: &Pool,
    spec_id: &str,
    account: &str,
    model: &str,
    started: Instant,
    ok: bool,
    tin: i64,
    tout: i64,
    class: Option<ErrorClass>,
    waf_fp: bool,
    upstream: &str,
) {
    store.usage_record(UsageEvent {
        ts: aigw_store::now_unix(),
        platform: spec_id.to_string(),
        model: model.to_string(),
        account: account.to_string(),
        tin,
        tout,
        ms: started.elapsed().as_millis() as i64,
        ok,
        upstream: upstream.to_string(),
        cancelled: false,
        kind: "chat".to_string(),
    });
    match class {
        Some(c) => pool.report(spec_id, account, model, false, c, waf_fp),
        None => pool.report(spec_id, account, model, true, ErrorClass::Transient, false),
    }
}

/// 客户端自己取消的流式请求：台账照旧记一次失败（口径不变，记成功会让成功率虚高），
/// 但**完全不碰号池** —— 「下游断开」发生在客户端，不是上游的错，
/// 旧实现把它当 Transient 罚一次，用户每点一次停止就把健康账号冻一轮。
#[allow(clippy::too_many_arguments)]
fn record_cancelled(
    store: &Store,
    spec_id: &str,
    account: &str,
    model: &str,
    started: Instant,
    tin: i64,
    tout: i64,
    upstream: &str,
) {
    store.usage_record(UsageEvent {
        ts: aigw_store::now_unix(),
        platform: spec_id.to_string(),
        model: model.to_string(),
        account: account.to_string(),
        tin,
        tout,
        ms: started.elapsed().as_millis() as i64,
        ok: false,
        upstream: upstream.to_string(),
        cancelled: true,
        kind: "chat".to_string(),
    });
}

/// chat 转发核心的统一出口：非流式 JSON 或已记账的流。
enum ChatCoreOut {
    Json(Value),
    Stream(std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>),
}

/// chat 转发核心的错误：本地错误（由调用方决定 OpenAI/Anthropic 信封）
/// 与上游错误（保留上游 status + body，尽量透传）。
enum ChatCoreErr {
    Local {
        status: u16,
        message: String,
        code: Option<String>,
        retry_after: Option<u16>,
    },
    Upstream {
        status: u16,
        body: String,
        /// 「下一步该做什么」的建议（代理/重新登录/换额度…），由 `upstream_hint` 生成。
        hint: Option<String>,
    },
}

/// 把请求体安全地解析为 JSON 对象，失败返回 OpenAI 信封错误。
fn parse_chat_body(body: Result<Bytes, BytesRejection>) -> Result<Value, Response> {
    let body = match body {
        Ok(b) => b,
        Err(e) => {
            let status = e.status().as_u16();
            return Err(openai_error(status, &format!("请求体读取失败：{e}")));
        }
    };
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return Err(openai_error(400, "invalid JSON body"));
    };
    // 必须是 JSON 对象：后面有 vb["model"] = json!(...) 这样的 IndexMut 赋值，
    // serde_json 对非 object 值做字符串索引会 **panic**（cannot access key
    // `model` in JSON array）。`[1]` / `123` / `"x"` / `true` 都能通过上面的
    // JSON 解析，在单可用平台（spec_for_model 的单平台兜底）时就会走到赋值那一行。
    if !v.is_object() {
        return Err(openai_error(
            400,
            "请求体必须是 JSON 对象（如 {\"model\":…,\"messages\":[…]}）",
        ));
    }
    Ok(v)
}

/// 流式响应头（三个协议端点共用）。
///
/// ★ X-Accel-Buffering: no —— nginx（以及 Cloudflare / 各类反代）默认会把
///   text/event-stream 攒满 buffer 再喷，表现为「网关明明在逐帧推，客户端 40 秒后才
///   一次性收到全部内容」，看上去就像卡死。参考项目里 cherry-studio 的 startup commit
///   纪律也是配套这个头才成立的：既然我们承诺首帧预算（STREAM_HEAD_WAIT），就得同时
///   保证中间层不缓冲。
/// ★ Connection: keep-alive —— 明确告诉 HTTP/1.1 客户端这条连接不要复用成新请求，
///   避免某些客户端把 SSE 连接当短连接反复重连。
/// 访问密钥的模型白名单拒绝文案（三种协议共用同一套措辞）。
fn key_model_denied_message(k: &aigw_store::AccessKey, model: &str) -> String {
    format!(
        "这把访问密钥不能调用模型 {model}。\n\
         它被限定只能调用：{}。\n\
         需要放开就找管理员在「访问密钥」里改白名单（留空 = 不限制）。",
        k.allow_models.join("、")
    )
}

/// /v1/chat/completions 的白名单校验：从请求体里取 model 判断。
///
/// 为什么在看到 body 之后才判、而不是在中间件里：中间件拿不到 body（axum 的 body
/// 是一次性的），而模型名就在 body 里。
fn key_model_denied(key: &Option<aigw_store::AccessKey>, v: &Value) -> Option<Response> {
    let k = key.as_ref()?;
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("unknown");
    if k.allows(model) {
        return None;
    }
    Some(openai_error_coded(
        403,
        "invalid_request_error",
        "model_not_allowed",
        &key_model_denied_message(k, model),
    ))
}

fn sse_response(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap()
}

async fn chat_completions(
    State(st): State<AppState>,
    key: Option<Extension<AccessKeyCtx>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // 访问密钥的模型白名单（没带 key / 没配白名单 = 不限制）。
    let key = key.map(|e| e.0 .0);
    // 超限/读取失败：用 OpenAI 信封回，别让客户端拿到 axum 的纯文本 413。
    let v = match parse_chat_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Some(resp) = key_model_denied(&key, &v) {
        return resp;
    }
    match chat_core(&st, v).await {
        Ok(ChatCoreOut::Json(v)) => Json(v).into_response(),
        Ok(ChatCoreOut::Stream(stream)) => sse_response(Body::from_stream(stream)),
        Err(ChatCoreErr::Local {
            status,
            message,
            code,
            retry_after,
        }) => {
            let mut resp = match code {
                Some(code) => openai_error_coded(status, "invalid_request_error", &code, &message),
                None => openai_error(status, &message),
            };
            if let Some(ra) = retry_after {
                resp.headers_mut().insert(
                    header::RETRY_AFTER,
                    axum::http::HeaderValue::from_str(&ra.to_string()).unwrap(),
                );
            }
            resp
        }
        Err(ChatCoreErr::Upstream {
            status,
            body,
            hint,
        }) => upstream_error_response(upstream_status(status), &body, hint.as_deref()),
    }
}

/// 共享转发核心（/v1/chat/completions 与 /v1/messages 共用）：
/// model 解析 → 账号候选 → 粘性选号 → 号池在途 → 转发 → 记账/自愈。
/// 按模型链转发：依次试链上的候选，第一个能给出结果的赢。
///
/// 与同平台换账号（`failover_worthy`）的分工：那个处理「这个账号坏了」，这里处理
/// 「这个模型/这条车道坏了」—— 额度墙、地域封锁、模型下线、上游 5xx，在本机还有
/// 别的平台能答的时候都不该让用户看到失败。
///
/// 只在前一个候选**确实是这次请求的失败**时才往下走：客户端中途断开（`StreamStat
/// .client_gone`）算成功出口，不再继续烧后面的配额。
async fn chat_via_combo(st: &AppState, v: Value, model: &str) -> Result<ChatCoreOut, ChatCoreErr> {
    let Some(combo) = st.combos.get(model) else {
        return Err(ChatCoreErr::Local {
            status: 404,
            message: format!("没有叫 {model} 的模型链。到面板「模型链」页看一眼现有链的 id，或者先建一条。"),
            code: Some("model_not_found".to_string()),
            retry_after: None,
        });
    };
    if !combo.enabled {
        return Err(ChatCoreErr::Local {
            status: 404,
            message: format!("模型链 {}「{}」当前是停用的：启用后才会参与路由。", combo.id, combo.name),
            code: Some("model_not_found".to_string()),
            retry_after: None,
        });
    }
    let chain = st.combos.candidates(&combo);
    if chain.is_empty() {
        return Err(ChatCoreErr::Local {
            status: 503,
            message: format!("模型链 {}「{}」是空的：加至少一个候选模型。", combo.id, combo.name),
            code: None,
            retry_after: None,
        });
    }
    let mut failures: Vec<String> = Vec::new();
    let mut last: Option<ChatCoreErr> = None;
    for candidate in &chain {
        let mut v2 = v.clone();
        v2["model"] = json!(candidate);
        // 递归调用自身（链上每个候选都要走一遍完整转发），async fn 递归必须装箱。
        match Box::pin(chat_core(st, v2)).await {
            Ok(out) => {
                if failures.is_empty() {
                    return Ok(out);
                }
                tracing::info!(
                    "模型链 {model} 在 {} 个候选后由 {candidate} 给出结果",
                    failures.len()
                );
                return Ok(out);
            }
            Err(e) => {
                // 只有「换一个候选就可能好」的错误才继续；请求本身写错（400/413/422）
                // 换谁都一样，早点把真实原因还给用户，别拖成一条链的等待。
                let worth = match &e {
                    // 「本机查不到这个模型」是链上最该继续的失败：每个平台认的模型名都不一样，
                    // 链里写了一个当前平台不认识的名字太常见了（kilo 有 kilo-auto/free，
                    // 别的平台没有）。第一次实测就是栽在这 —— 候选写成不存在的模型，
                    // 整条链直接 404，后面明明有能用的候选却没试。
                    // 其余 4xx（400 请求体写错 / 413 太大 / 422）换候选也是一样的错，早点返回。
                    ChatCoreErr::Local { status, code, .. } => {
                        *status >= 500
                            || *status == 429
                            || code.as_deref() == Some("model_not_found")
                    }
                    ChatCoreErr::Upstream { status, .. } => {
                        // 上游不认这个模型（404/400）同样换下一个候选：那是「这个平台没有
                        // 它」，不是「整条链都没救」。
                        *status == 404 || failover_worthy(*status, aigw_relay::classify_with_biz(*status, ""))
                    }
                };
                failures.push(format!("{candidate}：{}", chat_core_err_brief(&e)));
                last = Some(e);
                if !worth {
                    break;
                }
            }
        }
    }
    let detail = failures.join("；");
    Err(match last {
        Some(ChatCoreErr::Local { status, message, code, retry_after }) => ChatCoreErr::Local {
            status,
            message: format!("模型链 {}「{}」的候选全部失败 —— {detail}\n最后一条：{message}", combo.id, combo.name),
            code,
            retry_after,
        },
        Some(ChatCoreErr::Upstream { status, body, hint }) => ChatCoreErr::Upstream {
            status,
            body: format!("模型链 {}「{}」的候选全部失败 —— {detail}\n最后一条上游响应：{body}", combo.id, combo.name),
            hint,
        },
        None => ChatCoreErr::Local {
            status: 503,
            message: format!("模型链 {}「{}」没有可用的候选", combo.id, combo.name),
            code: None,
            retry_after: None,
        },
    })
}

/// 把一次失败压成一句可放进「候选全部失败」清单的简述。
fn chat_core_err_brief(e: &ChatCoreErr) -> String {
    match e {
        ChatCoreErr::Local { status, message, .. } => {
            let one: String = message.chars().take(120).collect();
            format!("HTTP {status} {one}")
        }
        ChatCoreErr::Upstream { status, body, .. } => {
            let one: String = body.chars().take(120).collect();
            format!("上游 {status} {one}")
        }
    }
}

async fn chat_core(st: &AppState, v: Value) -> Result<ChatCoreOut, ChatCoreErr> {
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    // 模型链（combo:xxx）：一个名字背后是一条有序候选链，跨平台/跨模型回退。
    // 必须在 resolve_spec 之前拦下 —— 链名不是任何平台声明的模型，走普通路由只会 404。
    if model.starts_with(aigw_store::combos::COMBO_PREFIX) {
        return chat_via_combo(st, v, &model).await;
    }
    // 「模型id@平台」后缀路由（旧版语义）；转发给上游时剥掉。
    // 只有后缀**确实是一个平台 id** 时才剥：上游模型名本身可能带 @
    // （`qwen3@2025-09` 这类版本号写法），无条件 rsplit 会把它截成另一个名字，
    // 于是路由命中 A 平台、上游却收到一个不存在的模型。
    let (upstream_model, named_platform) = match model.rsplit_once("@") {
        Some((m, p)) if st.is_platform_id(p) => (m.to_string(), Some(p.to_string())),
        _ => (model.clone(), None),
    };
    let Some(spec) = st.resolve_spec(&model).await else {
        // 一个平台都没启用 = 本服务自己没配好，属于「暂时不可用」，503 让客户端可以稍后重试。
        if !st.all_specs().iter().any(|s| s.enabled) {
            return Err(ChatCoreErr::Local {
                status: 503,
                message: format!("未知模型 {model}：当前没有任何启用的平台（没有配置可用上游）。\n请到「平台」页启用至少一个平台，或到「接入源」里接入一个。"),
                code: None,
                retry_after: None,
            });
        }
        // 平台有启用的，但没人声明这个模型 = 调用方把模型名写错了，是**永久性**错误。
        // 旧实现回 503（type=upstream_error），LiteLLM / Claude Code 这类按状态码决定
        // 重试的客户端会把它当上游抖动无限重试一个永远不可能成功的请求。
        // 改回 OpenAI 官方语义：404 + type=invalid_request_error + code=model_not_found。
        // 点名了平台却选不到：多半是那个平台被停用/没凭据，或者它不声明这个模型。
        // 旧文案会把整串 `模型@平台` 当成模型名报「未知模型 x@plat」，用户据此去查
        // 模型名，实际要查的是平台开关 —— 两个完全不同的排查方向。
        if let Some(plat) = named_platform.as_deref() {
            let why = st
                .all_specs()
                .into_iter()
                .find(|s| s.id == plat)
                .map(|s| {
                    if !s.enabled {
                        "这个平台当前是停用的".to_string()
                    } else if !st.platform_routable(&s) {
                        "这个平台没有可用凭据".to_string()
                    } else {
                        format!("这个平台没有声明模型 {upstream_model}")
                    }
                })
                .unwrap_or_else(|| "这个平台不在配置里".to_string());
            return Err(ChatCoreErr::Local {
                status: 404,
                message: format!(
                    "无法按 {model} 转发：平台 {plat} 不可用 —— {why}。\n处理办法：到「平台」页启用它/补凭据，或改用该平台上确实存在的模型（/v1/models?platform={plat} 可查）。"
                ),
                code: Some("model_not_found".to_string()),
                retry_after: None,
            });
        }
        return Err(ChatCoreErr::Local {
            status: 404,
            message: format!("未知模型 {model}：没有启用的平台声明它（多平台下不再静默兜底）。\n可用「模型id@平台」指定，或到「模型」页开启该模型。"),
            code: Some("model_not_found".to_string()),
            retry_after: None,
        });
    };
    // 协议不支持（anthropic/responses 自定义 API）：转发层只有 OpenAI 透传，
    // 硬发过去只会得到上游 404/400，不如直接说明。
    if !st.platform_supported(&spec) {
        return Err(ChatCoreErr::Local {
            status: 501,
            message: format!("平台 {} 声明 format={}，本面板只支持 openai 协议透传（不做协议转换）：请改用 OpenAI 兼容端点，或把 base_url 指向已转换的网关。", spec.id, spec.format),
            code: None,
            retry_after: None,
        });
    }
    // 占位符上游（如 @lobster 未配置 lobster_server）：明确报错，别让 reqwest
    // 的 "network: builder error" 冒出来——用户根本不知道要去配什么。
    if spec.base_url.starts_with("@") {
        return Err(ChatCoreErr::Local {
            status: 503,
            message: format!("平台 {} 的上游地址未配置（占位符 {}）：请先在设置里填写对应服务地址，或停用该平台。", spec.id, spec.base_url),
            code: None,
            retry_after: None,
        });
    }
    let is_stream = v.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    // 上游只认它自己声明的那个字符串：客户端常用带日期的官方全名
    // （`claude-sonnet-4-5-20250929`）或点号版本（`claude-sonnet-4.5`），
    // 这里回填成平台清单里的写法，否则上游会回 400 模型不存在。
    let upstream_model = st.canonical_model_for(&spec, &upstream_model);
    // body 的 model 改写为剥后缀/回填后的版本
    let mut vb = v.clone();
    vb["model"] = json!(upstream_model);
    // 私有字段（协议转换时携带的会话标识）绝不能发给上游
    if let Some(o) = vb.as_object_mut() {
        o.remove("_aigw_session");
    }
    let body: Bytes = Bytes::from(vb.to_string());

    // 账号 candidates：anon 平台走免密单账号；自定义 API 用 CustomStore 里的 Key；
    // 秘塔无账号时回退匿名指纹；其余从 AccountStore 取启用账号（明文 secret）。
    let anon = spec.auth == AuthKind::Anon;
    let mut pool_accounts: Vec<(String, String)> = if anon {
        vec![(format!("{}:anon", spec.id), String::new())]
    } else if spec.id.starts_with("custom-") {
        st.custom
            .rows()
            .into_iter()
            .find(|c| c.id == spec.id && !c.key_broken)
            .map(|c| c.api_key)
            .filter(|k| !k.trim().is_empty())
            .map(|k| vec![(format!("{}:key", spec.id), k)])
            .unwrap_or_default()
    } else {
        st.accounts.secrets_for(&spec.id)
    };
    // 秘塔支持免登录指纹：未粘贴 uid-sid 时回退匿名单账号（约 13 发/出口/窗口）
    if pool_accounts.is_empty() && spec.upstream_kind == "metaso" {
        pool_accounts = vec![(format!("{}:anon", spec.id), String::new())];
    }
    if pool_accounts.is_empty() {
        return Err(ChatCoreErr::Local {
            status: 503,
            message: format!("平台 {} 无可用账号（本服务端还没接入该平台的凭据），请到「账号」页登录或粘贴 Key 后重试", spec.id),
            code: None,
            retry_after: None,
        });
    }
    let ids: Vec<String> = pool_accounts.iter().map(|(id, _)| id.clone()).collect();
    let skey = session_key(&v);
    // 失败换号：**一个账号出事 ≠ 整个平台不可用**。旧实现只试 `pick` 挑出来的那一个，
    // 于是账号池里还有 3 个健康号时，一个 401 死 cookie / 429 限流 / 402 额度耗尽也会
    // 原样透给调用方。换号有代价（会话粘性与上游 prompt cache 都会掉），所以：
    // ① 只在「账号自身」类错误上换号（见 `failover_worthy`）；
    // ② 一次请求最多试 MAX_ACCOUNT_TRIES 个账号。
    const MAX_ACCOUNT_TRIES: usize = 3;
    let mut tried: Vec<String> = Vec::new();
    let mut last_err: Option<ChatCoreErr> = None;
    loop {
        let remaining: Vec<String> = ids.iter().filter(|i| !tried.contains(i)).cloned().collect();
        if remaining.is_empty() || tried.len() >= MAX_ACCOUNT_TRIES {
            // 上一轮的真实错误比「全被挡」更有信息量：用户要看到的是那个 401 的 hint。
            if let Some(e) = last_err {
                return Err(e);
            }
            return Err(ChatCoreErr::Local {
                status: 503,
                message: format!(
                    "平台 {} 无可用账号（本服务端还没接入该平台的凭据），请到「账号」页登录或粘贴 Key 后重试",
                    spec.id
                ),
                code: None,
                retry_after: None,
            });
        }
        let account = match st.sticky.route(&skey, &remaining) {
            Some(a) if st.pool.available(&a, &model) => a,
            other => {
                if let Some(stale) = other {
                    st.sticky.unbind(&skey);
                    tracing::info!("粘性命中 {stale} 但该账号当前不可用，重新选号");
                }
                let Some(a) = st.pool.pick(&spec.id, &remaining, &model) else {
                    if let Some(e) = last_err {
                        return Err(e);
                    }
                    tracing::warn!("pool 全挡：{} 的账号均处于冷却/凭据隔离中", spec.id);
                    return Err(ChatCoreErr::Local {
                        status: 503,
                        message: format!("平台 {} 的账号当前全部不可用（冷却中或凭据失效隔离），请稍后重试；若长期如此请到「账号」页重新登录该平台。", spec.id),
                        code: None,
                        retry_after: None,
                    });
                };
                st.sticky.bind(&skey, &a);
                a
            }
        };
        let secret = pool_accounts
            .iter()
            .find(|(id, _)| *id == account)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        // WorkBuddy / CodeBuddy 族转发必须带 X-Domain（账号登录时上游给的归属域）。
        // 其余平台的 domain 恒为空串，注入端会跳过这个头。
        let domain = st.accounts.domain_of(&spec.id, &secret);
        let lease = {
            const ACQUIRE_WAIT: Duration = Duration::from_secs(15);
            let deadline = Instant::now() + ACQUIRE_WAIT;
            loop {
                if let Some(l) = st.pool.acquire(&spec.id, &account, &model) {
                    break l;
                }
                if Instant::now() >= deadline {
                    return Err(ChatCoreErr::Local {
                        status: 429,
                        message: "provider busy: 该账号在途请求已达上限（2 路），请稍后重试".to_string(),
                        code: None,
                        retry_after: Some(2),
                    });
                }
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
        };
        let started = Instant::now();
        // 每一轮尝试的产物：成功直接返回；失败带上「原始状态码 + 分类」出来，
        // 由循环尾部决定「换下一个账号」还是「把错误交给调用方」。
        let attempt: Result<ChatCoreOut, (ChatCoreErr, u16, ErrorClass)> = if is_stream {
            match st.client.forward_stream(&spec, &secret, &domain, &body).await {
                Ok((stream, mut stat_rx)) => {
                    // ★ 下发第一个字节之前先做首帧体检：上游常见「HTTP 200 + 流里报错」，
                    //   原样透传会让客户端拿到 200 和解析不了的东西，failover 也无从触发。
                    let (stream, head_bad) = sniff_stream_head(stream).await;
                    if let Some((msg, class, raw_status)) = head_bad {
                        drop(stat_rx);
                        record(
                            &st.store, &st.pool, &spec.id, &account, &model, started, false,
                            0, 0, Some(class), false, &spec.base_url,
                        );
                        let hint = Some(
                            "上游回了 200，但流式内容一开头就是错误帧/直接结束：多半是这把凭据已失效、或该账号在当前模型上被挡。换号重试后仍失败就重新粘贴凭据。"
                                .to_string(),
                        );
                        Err((
                            ChatCoreErr::Upstream {
                                status: upstream_status(raw_status),
                                body: msg,
                                hint,
                            },
                            raw_status,
                            class,
                        ))
                    } else {
                        let store = st.store.clone();
                        let pool = st.pool.clone();
                        let spec_id = spec.id.clone();
                        let spec_url = spec.base_url.clone();
                        let model2 = model.clone();
                        let account2 = account.clone();
                        let last_err2 = st.last_err.clone();
                        tokio::spawn(async move {
                            let _lease = lease;
                            match stat_rx.recv().await {
                                Some(stat) => {
                                    let stat: StreamStat = stat;
                                    let ok = stat.error.is_none();
                                    if stat.client_gone {
                                        // 用户点了停止：不罚账号、不往卡片上写「平台坏了」。
                                        record_cancelled(
                                            &store, &spec_id, &account2, &model2, started,
                                            stat.tokens_in, stat.tokens_out, &spec_url,
                                        );
                                    } else {
                                        note_stream_outcome(
                                            &last_err2, &spec_id, stat.error, &stat.error_msg,
                                        );
                                        record(
                                            &store, &pool, &spec_id, &account2, &model2, started, ok,
                                            stat.tokens_in, stat.tokens_out, stat.error, false, &spec_url,
                                        );
                                    }
                                }
                                None => {
                                    last_err2.note(
                                        &spec_id,
                                        0,
                                        "流式响应结束了却没收到任何统计（连接被上游掐断或代理中断）",
                                    );
                                    record(
                                        &store, &pool, &spec_id, &account2, &model2, started, false,
                                        0, 0, Some(ErrorClass::Transient), false, &spec_url,
                                    );
                                }
                            }
                        });
                        Ok(ChatCoreOut::Stream(stream))
                    }
                }
                Err(e) => {
                    record(
                        &st.store, &st.pool, &spec.id, &account, &model, started, false,
                        0, 0, Some(e.class), false, &spec.base_url,
                    );
                    // 只有「凭据本身被拒」才值得续期重试：429/500/400 之类刷新也没用
                    // （对每次失败都刷一遍，等于给上游续期端点白送一轮压力，还会把
                    // 真正的限流/额度问题伪装成凭据问题）。
                    let auth_rejected =
                        e.status == 401 || e.status == 403 || matches!(e.class, ErrorClass::Auth);
                    if auth_rejected {
                        if let Some(kind) = refreshable_kind(&spec, &secret) {
                            if let Some(ret) = credential_retry_after_refresh(
                                &st, &spec, &account, &secret, kind, &body, is_stream, started, &model,
                                lease,
                            )
                            .await
                            {
                                // 自愈重试成功也是一次成功：否则 401 → 刷新成功 → 重试成功之后，
                                // 卡片会一直挂着那条已经修好的 401，直到下一次「普通」成功。
                                if ret.is_ok() {
                                    st.last_err.clear(&spec.id);
                                }
                                return ret;
                            }
                        }
                    }
                    let (st_code, st_class) = (e.status, e.class);
                    let hint = upstream_hint(&spec, e.status, Some(e.class), &e.body);
                    Err((
                        ChatCoreErr::Upstream {
                            status: upstream_status(e.status),
                            body: e.body,
                            hint,
                        },
                        st_code,
                        st_class,
                    ))
                }
            }
        } else {
            match st.client.forward_nonstream(&spec, &secret, &domain, &body).await {
                Ok(v) => {
                    let (tin, tout) = usage_or_estimate(&v, body.len());
                    record(
                        &st.store, &st.pool, &spec.id, &account, &model, started, true,
                        tin, tout, None, false, &spec.base_url,
                    );
                    Ok(ChatCoreOut::Json(v))
                }
                Err(e) => {
                    let waf_fp = e.status == 200;
                    record(
                        &st.store, &st.pool, &spec.id, &account, &model, started, false,
                        0, 0, Some(e.class), waf_fp, &spec.base_url,
                    );
                    // 只有「凭据本身被拒」才值得续期重试：429/500/400 之类刷新也没用
                    // （对每次失败都刷一遍，等于给上游续期端点白送一轮压力，还会把
                    // 真正的限流/额度问题伪装成凭据问题）。
                    let auth_rejected =
                        e.status == 401 || e.status == 403 || matches!(e.class, ErrorClass::Auth);
                    if auth_rejected {
                        if let Some(kind) = refreshable_kind(&spec, &secret) {
                            if let Some(ret) = credential_retry_after_refresh(
                                &st, &spec, &account, &secret, kind, &body, is_stream, started, &model,
                                lease,
                            )
                            .await
                            {
                                return ret;
                            }
                        }
                    }
                    let (st_code, st_class) = (e.status, e.class);
                    let hint = upstream_hint(&spec, e.status, Some(e.class), &e.body);
                    Err((
                        ChatCoreErr::Upstream {
                            status: upstream_status(e.status),
                            body: e.body,
                            hint,
                        },
                        st_code,
                        st_class,
                    ))
                }
            }
        };

        match attempt {
            Ok(out) => {
                // 成功一次就把旧失败原因清掉：卡片上挂着「上周 402」会骗人去修一个
                // 已经不存在的问题。
                st.last_err.clear(&spec.id);
                return Ok(out);
            }
            Err((err, status, class)) => {
                // 记下原始原因（401 凭据过期 / 402 额度耗尽 / 连不上 三种处理方式完全不同，
                // 面板必须能说清是哪一种）。
                st.last_err.note(&spec.id, status, &chat_err_text(&err));
                // 只判「这个错值得换号」，次数上限由循环头的 `tried.len() >= MAX_ACCOUNT_TRIES` 统管；
                // 两处都判会让上限成为两处独立维护的常量，改一处另一处仍生效、看起来像没改到。
                if failover_worthy(status, class) {
                    tracing::warn!(
                        "账号 {} 在 {} 上失败（HTTP {} / {:?}），换下一个账号重试",
                        account,
                        spec.id,
                        status,
                        class
                    );
                    tried.push(account);
                    st.sticky.unbind(&skey);
                    last_err = Some(err);
                    continue;
                }
                return Err(err);
            }
        }
    }
}

/// 换账号重试是否值得：只有「这个账号自己出事」的错误才值得。
///
/// - `Auth` / `Waf` / `Rate` / `Quota`：账号级问题（死 cookie、指纹被封、这把 Key 被限流
///   或额度耗尽），池里换一个健康账号很可能直接成功；
/// - `Transient` 且 `status >= 500`：上游这次真抽了，换号是廉价的一次机会。**连接层失败
///   （`status == 0`：DNS/连不上/超时）不换** —— 那与账号无关，三连重试只会把一次失败
///   变成三倍等待；
/// - `Rejected`（400/404/410/413/415/422）：请求本身写错了，换号也是同一个错；
/// - `LaneLocked` / `Region`：车道级/地域级封锁，池里每个账号都会被同样拒绝。
/// 流式首帧体检的等待预算：**总预算**，不是每次读取的预算。
///
/// 旧实现把 30s 当成「每次 s.next() 的超时」——上游只要每 29 秒挤一个字节，就能让这个
/// 循环无限续命，HTTP 200 迟迟不提交、客户端一直挂在那里，而我们又永远判不出首帧好坏
/// （对应 cherry-studio 的「延迟提交」纪律：pending 态必须有天花板）。
const STREAM_HEAD_WAIT: Duration = Duration::from_secs(30);
/// 首帧体检的宽限：读到过哪怕是半截帧/心跳的上游，说明它确实在干活（只是慢），
/// 给到更长的总预算；一个字节都没给的上游没有任何理由拖过 STREAM_HEAD_WAIT。
const STREAM_HEAD_WAIT_AFTER_BYTES: Duration = Duration::from_secs(60);
/// 首帧窗口上限：超过这么多字节还没判定，说明上游在正常流式吐内容，放行。
const STREAM_HEAD_MAX: usize = 16 * 1024;

/// 把预算时长写成人类读得懂的样子：整秒就写秒，亚秒（只有测试会给）写毫秒。
///
/// ★ 踩过：直接用 as_secs() 在毫秒级预算下会被截成 0，文案变成「上游 0 秒内没有发出
///   任何流式内容」——既不准确，也让测试断言无从下手。
fn fmt_budget(d: Duration) -> String {
    if d.as_secs() > 0 {
        format!("{} 秒", d.as_secs())
    } else {
        format!("{} 毫秒", d.as_millis())
    }
}

/// 首帧体检结论。
enum HeadProbe {
    /// 正常流：带 choices 的帧、或非 SSE 的正常 JSON。
    Pass,
    /// 坏流：把上游真实原因、错误分类、对应 HTTP 状态带出来（分类决定要不要换号）。
    Fail(String, ErrorClass, u16),
}

/// 上游「HTTP 200 + 流里当场报错」是聚合站/中转站最常见的坏法：状态码骗人，
/// 真实原因（凭据失效、模型不可用、额度耗尽）藏在第一个 SSE 帧里。
///
/// 旧实现原样透传 → 客户端拿到 200 和一段它解析不了的东西，`failover` 也无从触发
/// （它只挂在 `forward_stream` 返回 Err 的时候）。此处把判定提前到「下发第一个字节之前」，
/// 于是这仍然是一次**可以换号重试**的失败。判定只看「首个带 choices 的帧之前」的字节，
/// 无论结论如何，已读到的字节都原样接回流首，不吞不改。
async fn sniff_stream_head(
    s: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
) -> (
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    Option<(String, ErrorClass, u16)>,
) {
    sniff_stream_head_within(s, STREAM_HEAD_WAIT, STREAM_HEAD_WAIT_AFTER_BYTES).await
}

/// 同 sniff_stream_head，但超时预算可注入（测试要能在毫秒级验证「拖不出首帧」的判定）。
///
/// ★ 预算语义是**总预算**：从进函数开始计时，读到过字节后放宽到 after_bytes，两者都不随
///   读次数刷新。旧实现把 timeout 写在循环里，等于「每读一次就给 30 秒」，上游一秒挤一个
///   字节就能把提交无限拖后；客户端那边是「连上了、什么也没有」，这种半开状态比直接 5xx
///   更难排查（对应 cherry-studio 的「延迟提交」纪律：pending 态必须有天花板）。
/// 首帧预算用光时的判定（两条路径共用：`left` 减到 0、以及某次等待超时）。
///
/// ★ 措辞按「已收到多少字节」定性，而不是按「哪条分支先触发」——两者对运维是同一件事：
///   上游回了 200 却一直凑不出首帧。旧实现两条分支各写一套文案，同一现象会报出
///   「只挤了 N 字节」或「卡住了」两种说法，日志里根本没法聚合统计。
fn head_timeout_verdict(buf_len: usize, first_wait: Duration, after_bytes: Duration) -> (String, ErrorClass, u16) {
    let msg = if buf_len == 0 {
        format!(
            "上游 {}内没有发出任何流式内容（HTTP 200，连接打开但一直空转）",
            fmt_budget(first_wait)
        )
    } else {
        format!(
            "上游 {}内只挤了 {} 字节、始终凑不出一个完整的首帧（疑似被限速或连接半死）",
            fmt_budget(after_bytes),
            buf_len
        )
    };
    (msg, ErrorClass::Transient, 504)
}

async fn sniff_stream_head_within(
    mut s: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
    >,
    first_wait: Duration,
    after_bytes: Duration,
) -> (
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    Option<(String, ErrorClass, u16)>,
) {
    use futures_util::StreamExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut verdict: Option<(String, ErrorClass, u16)> = None;
    let started = Instant::now();
    loop {
        if let Some(probe) = scan_stream_head(&buf, false) {
            if let HeadProbe::Fail(m, c, st) = probe {
                verdict = Some((m, c, st));
            }
            break;
        }
        if buf.len() > STREAM_HEAD_MAX {
            break;
        }
        // 总预算：读到过字节的按 after_bytes 算，一个字节没给的按 first_wait 算。
        let budget = if buf.is_empty() { first_wait } else { after_bytes };
        let left = match budget.checked_sub(started.elapsed()) {
            Some(d) if !d.is_zero() => d,
            // 预算用光：不许再无限等下去。
            _ => {
                verdict = Some(head_timeout_verdict(buf.len(), first_wait, after_bytes));
                break;
            }
        };
        match tokio::time::timeout(left, s.next()).await {
            Ok(Some(Ok(chunk))) => buf.extend_from_slice(&chunk),
            Ok(Some(Err(e))) => {
                // 还没吐出任何有效帧就读失败：对客户端来说与「上游立刻挂了」等价，
                // 此刻仍可换号，比下发一个半截流更有用。
                verdict = Some((
                    format!("上游流式读取失败（首帧前）：{e}"),
                    ErrorClass::Transient,
                    502,
                ));
                break;
            }
            Ok(None) => {
                // 流结束：最后一次判定按 eof 语义（空流/只有心跳 = 失败）。
                if let Some(HeadProbe::Fail(m, c, st)) = scan_stream_head(&buf, true) {
                    verdict = Some((m, c, st));
                }
                break;
            }
            Err(_) => {
                // 这一次等满了剩余预算还没有新字节：不再「放行」（旧实现如此），
                // 因为放行的结果就是客户端挂在 200 上什么都收不到。
                verdict = Some(head_timeout_verdict(buf.len(), first_wait, after_bytes));
                break;
            }
        }
    }
    let prefix = futures_util::stream::once(async move {
        Ok::<Bytes, std::io::Error>(Bytes::from(buf))
    });
    (Box::pin(prefix.chain(s)), verdict)
}

/// 在已读字节里找「首个有效帧」或「错误信号」。返回 `None` 表示信息还不够、继续读。
fn scan_stream_head(buf: &[u8], eof: bool) -> Option<HeadProbe> {
    let text = String::from_utf8_lossy(buf);
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return if eof {
            Some(HeadProbe::Fail(
                "上游流式响应在发出任何内容前就结束了（HTTP 200，但流是空的）".to_string(),
                ErrorClass::Transient,
                502,
            ))
        } else {
            None
        };
    }
    // 有些上游无视 stream:true 直接回一个 JSON：错误体按错误处理，正常体放行。
    if trimmed.starts_with('{') {
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => Some(probe_json(&v)),
            Err(_) => {
                if eof {
                    Some(HeadProbe::Pass)
                } else {
                    None
                }
            }
        };
    }
    let mut saw_frame = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let Some(payload) = line.strip_prefix("data:") else {
            if line.starts_with("event:") && line.to_ascii_lowercase().contains("error") {
                saw_frame = true;
            }
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            continue; // 半截帧：等下一块
        };
        saw_frame = true;
        let probe = probe_json(&v);
        if let HeadProbe::Fail(..) = probe {
            return Some(probe);
        }
        if v.get("choices").is_some() {
            return Some(HeadProbe::Pass);
        }
    }
    if eof && !saw_frame {
        return Some(HeadProbe::Fail(
            "上游流式响应只有心跳/空帧，没有发出任何内容".to_string(),
            ErrorClass::Transient,
            502,
        ));
    }
    None
}

/// 单个 JSON 载荷的判定：有非空 `error` 就是坏流，其余放行。
fn probe_json(v: &Value) -> HeadProbe {
    if let Some(e) = v.get("error").filter(|x| !x.is_null()) {
        let msg = e
            .as_str()
            .map(str::to_string)
            .or_else(|| e.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| e.to_string());
        return classify_stream_error(&msg);
    }
    HeadProbe::Pass
}

/// 把流内错误文案归类：分类决定「换号有没有用」，状态码决定客户端看到什么。
fn classify_stream_error(msg: &str) -> HeadProbe {
    let m = msg.to_ascii_lowercase();
    let has = |k: &str| m.contains(k);
    let (class, status) = if has("api key")
        || has("apikey")
        || has("unauthorized")
        || has("invalid token")
        || has("token expired")
        || has("认证")
        || has("登录")
        || has("401")
    {
        (ErrorClass::Auth, 401)
    } else if has("quota") || has("额度") || has("余额") || has("balance") || has("402") {
        (ErrorClass::Quota, 402)
    } else if has("rate limit") || has("too many") || has("限流") || has("429") {
        (ErrorClass::Rate, 429)
    } else {
        (ErrorClass::Transient, 502)
    };
    let short: String = msg.chars().take(400).collect();
    HeadProbe::Fail(format!("上游流式响应首帧报错：{short}"), class, status)
}
fn failover_worthy(status: u16, class: ErrorClass) -> bool {
    match class {
        ErrorClass::Auth | ErrorClass::Waf | ErrorClass::Rate | ErrorClass::Quota => true,
        ErrorClass::Transient => status >= 500,
        _ => false,
    }
}
/// 这个平台/这条凭据能不能在 401/403 时自动续期自救。
///
/// 返回 `Some(RefreshKind)` 表示值得一试；`None` 表示没有续期链路（绝大多数平台：
/// 粘进来的就是最终 Key/Cookie，没有 refreshToken 可用）。返回 None 时上层直接把
/// 原始错误透给用户，不会多打一次上游。
fn refreshable_kind(spec: &ProviderSpec, secret: &str) -> Option<RefreshKind> {
    // cline：secret 形如 workos:{at}:{rt}，走 WorkOS 双通道续期。
    if spec.id == "cline" && secret.matches(':').count() >= 2 {
        return Some(RefreshKind::Cline);
    }
    match spec.upstream_kind.as_str() {
        // trae：上游续期会轮换 RefreshToken，不回写账号池的话旧 rt 一作废这个号就死。
        "trae" => Some(RefreshKind::Trae),
        // kiro：social / idc 两形态都有 refreshToken 端点。
        "kiro" if !secret.trim().is_empty() => Some(RefreshKind::Kiro),
        // qoder：设备授权只发 access token，refresh_secret 是显式报错桩 —— 但仍然走
        // 一遍，好让用户看到「该平台没有 refreshToken 可续期」而不是含糊的「重新登录」。
        "qoder" if !secret.trim().is_empty() => Some(RefreshKind::Qoder),
        _ => None,
    }
}

/// 可自动续期的凭据种类（见 `refreshable_kind`）。
enum RefreshKind {
    Cline,
    Trae,
    Kiro,
    Qoder,
}

/// 按种类调各自 adapter 的续期实现，返回**可直接写回账号池**的新 secret。
async fn try_refresh_secret(kind: RefreshKind, secret: &str) -> Option<String> {
    let http = aigw_relay::auth_login::auth_http();
    match kind {
        RefreshKind::Cline => aigw_relay::auth_login::cline_refresh(&http, secret).await.ok(),
        RefreshKind::Trae => aigw_relay::trae::refresh_secret(&http, secret).await.ok(),
        RefreshKind::Kiro => aigw_relay::kiro::refresh_secret(&http, secret).await.ok(),
        RefreshKind::Qoder => aigw_relay::qoder::refresh_secret(&http, secret).await.ok(),
    }
}

/// 账号凭据被替换后的收尾：清号池治理状态 + 清会话粘性绑定。
///
/// 为什么两处都要清：
/// - 号池状态（waf 鉴权隔离 / 冷却 / 在途）：旧凭据 401 留下的 30 分钟账号级
///   拉黑不会因为「用户重新登录了」而自动解除，于是新凭据照样被判不可用；
/// - 会话粘性：会话可能还钉在这个账号上（route 命中但 available() 为 false，
///   于是每次请求都走一遍「命中 → 不可用 → 重选」，且旧绑定一直占位）。
///
/// 不清的后果对用户是不可见的：面板显示「登录成功」，但下一次对话仍然 401，
/// 看起来像登录根本没生效。
fn on_credential_replaced(st: &AppState, account: &str) {
    if account.is_empty() {
        return;
    }
    st.pool.forget_account(account);
    st.sticky.unbind_account(account);
    invalidate_models_cache(st);
}
/// 401/403 自愈重试：刷新凭据 → 更新账号 secret → 重试一次转发。
/// 刷新失败或仍失败返回 None（上层保留原错误）。
#[allow(clippy::too_many_arguments)]
async fn credential_retry_after_refresh(
    st: &AppState,
    spec: &ProviderSpec,
    account: &str,
    secret: &str,
    kind: RefreshKind,
    body: &Bytes,
    is_stream: bool,
    started: Instant,
    model: &str,
    lease: Lease,
) -> Option<Result<ChatCoreOut, ChatCoreErr>> {
    let new_secret = try_refresh_secret(kind, secret).await?;
    let (hit, changed) = st.accounts.update_secret(account, &new_secret);
    // 凭据真的换了（不是拿同一个值覆盖一遍）才清治理状态：
    // 否则一次成功的「刷新回来同一个 token」也会把刚建好的粘性绑定义掉，
    // 白白丢掉 prompt cache 命中。
    if hit && changed {
        on_credential_replaced(st, account);
    }
    tracing::info!("{} 账号 {account} 凭据自愈刷新成功，重试一次转发", spec.id);
    let spec2 = spec.clone();
    let body2 = body.clone();
    let domain2 = st.accounts.domain_of(&spec.id, &new_secret);
    if is_stream {
        match st.client.forward_stream(&spec2, &new_secret, &domain2, &body2).await {
            Ok((stream, mut stat_rx)) => {
                let store = st.store.clone();
                let pool = st.pool.clone();
                let spec_id = spec.id.clone();
                let spec_url = spec.base_url.clone();
                let model2 = model.to_string();
                let account2 = account.to_string();
                let last_err2 = st.last_err.clone();
                tokio::spawn(async move {
                    let _lease = lease;
                    match stat_rx.recv().await {
                        Some(stat) => {
                            if stat.client_gone {
                                record_cancelled(
                                    &store, &spec_id, &account2, &model2, started,
                                    stat.tokens_in, stat.tokens_out, &spec_url,
                                );
                            } else {
                                note_stream_outcome(
                                    &last_err2, &spec_id, stat.error, &stat.error_msg,
                                );
                                record(
                                    &store, &pool, &spec_id, &account2, &model2, started,
                                    stat.error.is_none(), stat.tokens_in, stat.tokens_out,
                                    stat.error, false, &spec_url,
                                )
                            }
                        }
                        None => {
                            last_err2.note(
                                &spec_id,
                                0,
                                "流式响应结束了却没收到任何统计（连接被上游掐断或代理中断）",
                            );
                            record(
                                &store, &pool, &spec_id, &account2, &model2, started, false,
                                0, 0, Some(ErrorClass::Transient), false, &spec_url,
                            )
                        }
                    }
                });
                Some(Ok(ChatCoreOut::Stream(stream)))
            }
            Err(e2) => {
                record(
                    &st.store, &st.pool, &spec.id, &account, &model, started, false,
                    0, 0, Some(e2.class), false, &spec.base_url,
                );
                let hint = upstream_hint(&spec, e2.status, Some(e2.class), &e2.body);
                Some(Err(ChatCoreErr::Upstream {
                    status: upstream_status(e2.status),
                    body: e2.body,
                    hint,
                }))
            }
        }
    } else {
        drop(lease);
        match st.client.forward_nonstream(&spec2, &new_secret, &domain2, &body2).await {
            Ok(v) => {
                let (tin, tout) = usage_or_estimate(&v, body2.len());
                record(
                    &st.store, &st.pool, &spec.id, &account, &model, started, true,
                    tin, tout, None, false, &spec.base_url,
                );
                Some(Ok(ChatCoreOut::Json(v)))
            }
            Err(e2) => {
                record(
                    &st.store, &st.pool, &spec.id, &account, &model, started, false,
                    0, 0, Some(e2.class), false, &spec.base_url,
                );
                let hint = upstream_hint(&spec, e2.status, Some(e2.class), &e2.body);
                Some(Err(ChatCoreErr::Upstream {
                    status: upstream_status(e2.status),
                    body: e2.body,
                    hint,
                }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// /v1/messages（Anthropic Messages 兼容端点）
// ---------------------------------------------------------------------------

/// Anthropic 形状错误信封：{"type":"error","error":{"type":…,"message":…}}。
fn anthropic_error(status: u16, msg: &str) -> Response {
    let etype = match status {
        400 => "invalid_request_error",
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

/// Anthropic 内容块数组里的文本（字符串或 [{type:text,text}]）
fn anthropic_blocks_text(c: Option<&Value>) -> String {
    let Some(c) = c else { return String::new() };
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(arr) = c.as_array() {
        for b in arr {
            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            } else if let Some(s) = b.as_str() {
                out.push_str(s);
            }
        }
    }
    out
}

/// Anthropic Messages 请求 → OpenAI chat.completions 请求形状。
/// - system（字符串或块数组）→ 前缀 system 消息
/// - content 字符串或块数组：text 拼接、image → OpenAI image_url data URI
/// - assistant 的 tool_use 块 → OpenAI tool_calls；user 的 tool_result 块 → role=tool 消息
///   （OpenAI 要求 tool 消息紧跟带 tool_calls 的 assistant 消息，故按块顺序先发）
/// - tools[{name,description,input_schema}] → tools[{type:function,…}]；tool_choice 三态映射
fn anthropic_to_openai(v: &Value) -> Result<Value, String> {
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = v.get("system") {
        let text = anthropic_blocks_text(Some(sys));
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) else {
        return Err("messages 字段缺失或不是数组".to_string());
    };
    for m in msgs {
        let role = m
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("user")
            .to_string();
        match m.get("content") {
            Some(Value::String(s)) => messages.push(json!({"role": role, "content": s})),
            Some(Value::Array(blocks)) => {
                if role == "assistant" {
                    let mut text = String::new();
                    let mut tool_calls: Vec<Value> = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(x) = b.get("text").and_then(|x| x.as_str()) {
                                    text.push_str(x);
                                }
                            }
                            Some("tool_use") => {
                                let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
                                if name.is_empty() {
                                    continue;
                                }
                                let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {"name": name, "arguments": input.to_string()}
                                }));
                            }
                            _ => {}
                        }
                    }
                    if tool_calls.is_empty() {
                        messages.push(json!({"role": "assistant", "content": text}));
                    } else {
                        let content = if text.is_empty() { Value::Null } else { json!(text) };
                        messages.push(json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": tool_calls
                        }));
                    }
                } else {
                    // user（或罕见的 system 块数组）：先发 tool 结果，再发剩余文本/图片
                    let mut parts: Vec<Value> = Vec::new();
                    let mut tool_msgs: Vec<Value> = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("tool_result") => {
                                let id = b
                                    .get("tool_use_id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("");
                                let mut body = anthropic_blocks_text(b.get("content"));
                                if body.is_empty() {
                                    body = "(空结果)".to_string();
                                }
                                tool_msgs.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": body
                                }));
                            }
                            Some("text") => {
                                if let Some(x) = b.get("text").and_then(|x| x.as_str()) {
                                    parts.push(json!({"type": "text", "text": x}));
                                }
                            }
                            Some("image") => {
                                let src = b.get("source");
                                let media = src
                                    .and_then(|s| s.get("media_type"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("image/png");
                                let data = src
                                    .and_then(|s| s.get("data"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");
                                parts.push(json!({
                                    "type": "image_url",
                                    "image_url": {"url": format!("data:{media};base64,{data}")}
                                }));
                            }
                            _ => {}
                        }
                    }
                    for tm in tool_msgs {
                        messages.push(tm);
                    }
                    if !parts.is_empty() {
                        let all_text = parts
                            .iter()
                            .all(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"));
                        let content = if all_text {
                            json!(parts
                                .iter()
                                .filter_map(|p| p.get("text"))
                                .filter_map(|t| t.as_str())
                                .collect::<Vec<_>>()
                                .join("\n"))
                        } else {
                            json!(parts)
                        };
                        messages.push(json!({"role": role, "content": content}));
                    }
                }
            }
            _ => messages.push(json!({"role": role, "content": ""})),
        }
    }
    let mut out = json!({"model": model, "messages": messages});
    for k in ["max_tokens", "temperature", "top_p", "stream"] {
        if let Some(x) = v.get(k) {
            out[k] = x.clone();
        }
    }
    if let Some(stops) = v.get("stop_sequences").and_then(|s| s.as_array()) {
        out["stop"] = json!(stops);
    }
    // 工具定义：Anthropic 的 input_schema 就是 JSON Schema，直接搬到 parameters。
    // 服务端工具（web_search_20250305 等只有 type/name，没有 input_schema）OpenAI 形状
    // 表达不了，跳过——否则上游会因为缺 parameters 直接 400。
    if let Some(tools) = v.get("tools").and_then(|t| t.as_array()) {
        let mut conv: Vec<Value> = Vec::new();
        for t in tools {
            let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() || t.get("input_schema").is_none() {
                continue;
            }
            let mut f = json!({
                "name": name,
                "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({})),
            });
            if let Some(d) = t.get("description").and_then(|x| x.as_str()) {
                f["description"] = json!(d);
            }
            conv.push(json!({"type": "function", "function": f}));
        }
        if !conv.is_empty() {
            out["tools"] = json!(conv);
        }
    }
    if let Some(tc) = v.get("tool_choice") {
        let kind = tc.get("type").and_then(|x| x.as_str()).unwrap_or("auto");
        out["tool_choice"] = match kind {
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => {
                let name = tc.get("name").and_then(|x| x.as_str()).unwrap_or("");
                json!({"type": "function", "function": {"name": name}})
            }
            _ => json!("auto"),
        };
    }
    Ok(out)
}
/// OpenAI 的 tool_calls → Anthropic 的 tool_use 内容块（arguments 是 JSON 字符串）。
fn openai_tool_use_blocks(msg: Option<&Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let Some(msg) = msg else { return out };
    if let Some(arr) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for (i, tc) in arr.iter().enumerate() {
            let f = tc.get("function");
            let name = f.and_then(|f| f.get("name")).and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let id = tc
                .get("id")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("toolu_{:016x}_{i}", nanos_hex_int()));
            let args = f
                .and_then(|f| f.get("arguments"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let input = if args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({"__raw": args}))
            };
            out.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
        }
    }
    // 老式 function_call 单个工具调用
    if let Some(fc) = msg.get("function_call") {
        let name = fc.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if !name.is_empty() {
            let args = fc.get("arguments").and_then(|x| x.as_str()).unwrap_or("");
            let input = if args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({"__raw": args}))
            };
            out.push(json!({
                "type": "tool_use",
                "id": format!("toolu_{:016x}_fc", nanos_hex_int()),
                "name": name,
                "input": input
            }));
        }
    }
    out
}

fn nanos_hex_int() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn anthropic_stop_reason(finish: Option<&str>, has_tool: bool) -> &'static str {
    match finish {
        Some("length") | Some("max_tokens") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("stop") => "end_turn",
        _ => {
            if has_tool {
                "tool_use"
            } else {
                "end_turn"
            }
        }
    }
}

/// OpenAI chat.completions 响应 → Anthropic Messages 响应形状。
fn openai_to_anthropic(v: &Value, model: &str) -> Value {
    let first = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let msg = first.and_then(|c| c.get("message"));
    let text = msg
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let finish = first
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str());
    let tool_blocks = openai_tool_use_blocks(msg);
    let has_tool = !tool_blocks.is_empty();
    let mut content: Vec<Value> = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    content.extend(tool_blocks);
    let stop_reason = anthropic_stop_reason(finish, has_tool);
    let (input_tokens, output_tokens) = conv_usage(v);
    json!({
        "id": format!("msg_{}", v.get("id").and_then(|i| i.as_str()).unwrap_or("unknown")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
    })
}

/// ★ 2026-10-26（用户排队消息①）：按「模型名（可带 @平台后缀）」找到声明它的
/// format=anthropic 平台——命中即走原生 Anthropic 透传，不做 OpenAI 往返转换。
fn native_anthropic_spec(st: &AppState, model: &str) -> Option<aigw_registry::ProviderSpec> {
    let (bare, named) = match model.rsplit_once('@') {
        Some((b, p)) => (b.to_string(), Some(p.to_string())),
        None => (model.to_string(), None),
    };
    // 静态平台（providers TOML，理论上都是 openai）——留保险
    if let Some(np) = &named {
        if let Some(p) = st.specs.iter().find(|p| &p.id == np) {
            if p.format == "anthropic" && p.enabled {
                return Some(p.clone());
            }
        }
    } else if let Some(p) = st
        .specs
        .iter()
        .find(|p| p.format == "anthropic" && p.enabled && p.models.iter().any(|m| m == &bare))
    {
        return Some(p.clone());
    }
    // 自定义 API：anthropic 条目命中（@平台 指定优先，否则按模型名）
    let customs = st.custom.rows();
    if let Some(np) = &named {
        if let Some(c) = customs
            .iter()
            .find(|c| &c.id == np && c.format == "anthropic" && c.enabled && !c.key_broken)
        {
            return Some(custom_to_spec(c));
        }
    } else if let Some(c) = customs.iter().find(|c| {
        c.format == "anthropic" && c.enabled && !c.key_broken && c.models.iter().any(|m| m == &bare)
    }) {
        return Some(custom_to_spec(c));
    }
    None
}

/// Anthropic 原生透传：请求体原样（改 model 为上游名）打上游 /v1/messages，
/// 响应（JSON 或 SSE）原样回客户端——工具调用/thinking 块零损失。
async fn anthropic_passthrough(
    st: &AppState,
    spec: aigw_registry::ProviderSpec,
    v: &Value,
    model: &str,
) -> Response {
    let upstream_model = st.canonical_model_for(&spec, model);
    let mut body = v.clone();
    body["model"] = json!(upstream_model);
    let is_stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
    let raw = serde_json::to_vec(&body).unwrap_or_default();
    let started = std::time::Instant::now();
    // 凭据：custom 条目从 CustomStore 取（open 时已解 dpapi 为明文）
    let secret = st
        .custom
        .rows()
        .into_iter()
        .find(|c| c.id == spec.id && !c.key_broken)
        .map(|c| c.api_key)
        .filter(|k| !k.trim().is_empty())
        .unwrap_or_default();
    let domain = "custom-anthropic";
    if is_stream {
        match st.client.forward_stream(&spec, &secret, domain, &raw).await {
            Ok((stream, _stat_rx)) => sse_response(Body::from_stream(stream)),
            Err(e) => anthropic_error(upstream_status(e.status), &e.body),
        }
    } else {
        match st.client.forward_nonstream(&spec, &secret, domain, &raw).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => anthropic_error(upstream_status(e.status), &e.body),
        }
    }
}
/// OpenAI SSE → Anthropic SSE 事件流：message_start → content_block_start →
/// content_block_delta(text_delta) → content_block_stop → message_delta → message_stop。
fn anthropic_stream(
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    model: String,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    use futures_util::StreamExt;
    let msg_id = format!("msg_{:016x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let state = AnthState {
        inner,
        parser: aigw_relay::SseParser::new(),
        model,
        msg_id,
        started: false,
        blocks_opened: 0,
        text_index: 0,
        text_open: false,
        tool_open: None,
        tool_meta: Vec::new(),
        closed: false,
        stop_reason: "end_turn".to_string(),
        usage_seen: false,
        delta_sent: false,
        out_text: String::new(),
        pending: std::collections::VecDeque::new(),
    };
    Box::pin(futures_util::stream::unfold(state, |mut s| async move {
        loop {
            if let Some(b) = s.pending.pop_front() {
                return Some((Ok::<Bytes, std::io::Error>(b), s));
            }
            if s.closed {
                return None;
            }
            match s.inner.next().await {
                Some(Ok(bytes)) => {
                    let evs = s.parser.feed(&bytes);
                    s.consume(evs);
                }
                Some(Err(_e)) => {
                    s.finish();
                }
                None => {
                    s.finish();
                }
            }
        }
    }))
}

/// /v1/messages 流式转换状态机。
/// 文本块与工具块按到达顺序依次开启：tool_use 用 content_block_start +
/// input_json_delta 表达，文本用 text_delta，块之间必须有 content_block_stop。
struct AnthState {
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    parser: aigw_relay::SseParser,
    model: String,
    msg_id: String,
    /// 是否已发 message_start
    started: bool,
    /// 已开启的内容块数量（= 下一个块的 index）
    blocks_opened: usize,
    /// 当前打开文本块的 index
    text_index: usize,
    text_open: bool,
    /// 当前打开的工具块：(block index, tool id)
    tool_open: Option<(usize, String)>,
    /// 上游 tool_calls[i] 的 (id, name) 记忆（id/name 只在首帧出现）
    tool_meta: Vec<(String, String)>,
    closed: bool,
    stop_reason: String,
    /// 是否已收到上游 usage 帧（让 message_delta 携带准确 output_tokens）
    usage_seen: bool,
    /// 是否已发 message_delta（幂等）
    delta_sent: bool,
    /// 已下发的文本累计（上游无 usage 帧时用于估算 output_tokens）
    out_text: String,
    pending: std::collections::VecDeque<Bytes>,
}

impl AnthState {
    fn event(&mut self, name: &str, data: &Value) {
        let s = format!("event: {name}\ndata: {data}\n\n");
        self.pending.push_back(Bytes::from(s));
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": self.parser.prompt_tokens, "output_tokens": 0}
                }
            }),
        );
    }

    fn close_text_block(&mut self) {
        if self.text_open {
            let idx = self.text_index;
            self.event(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": idx}),
            );
            self.text_open = false;
        }
    }

    fn close_tool_block(&mut self) {
        if let Some((idx, _)) = self.tool_open.take() {
            self.event(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": idx}),
            );
        }
    }

    fn close_blocks(&mut self) {
        self.close_text_block();
        self.close_tool_block();
    }

    fn open_text_block(&mut self) {
        if self.text_open {
            return;
        }
        self.close_tool_block();
        let idx = self.blocks_opened;
        self.blocks_opened += 1;
        self.text_index = idx;
        self.text_open = true;
        self.event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": {"type": "text", "text": ""}
            }),
        );
    }

    fn delta(&mut self, text: &str) {
        self.ensure_started();
        self.open_text_block();
        self.out_text.push_str(text);
        let idx = self.text_index;
        self.event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": {"type": "text_delta", "text": text}
            }),
        );
    }

    /// 工具调用增量：第一个分片带 id/name（开块），后续分片只带 arguments。
    fn tool_delta(&mut self, id: &str, name: &str, args: &str) {
        self.ensure_started();
        let reuse = matches!(&self.tool_open, Some((_, cur)) if cur == id);
        if !reuse {
            self.close_text_block();
            self.close_tool_block();
            let idx = self.blocks_opened;
            self.blocks_opened += 1;
            self.tool_open = Some((idx, id.to_string()));
            self.event(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {}
                    }
                }),
            );
        }
        if !args.is_empty() {
            if let Some((idx, _)) = &self.tool_open {
                let idx = *idx;
                self.event(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": {"type": "input_json_delta", "partial_json": args}
                    }),
                );
            }
        }
        self.stop_reason = "tool_use".to_string();
    }

    /// message_delta（幂等）：携带当前累计 output_tokens。
    fn send_delta(&mut self) {
        if self.delta_sent {
            return;
        }
        self.delta_sent = true;
        let sr: String = if self.stop_reason.is_empty() {
            "end_turn".to_string()
        } else {
            self.stop_reason.clone()
        };
        // 上游流式常不带 usage 帧（如 apk-doubao）：有真实计数用它，否则按字符估算，
        // 避免 output_tokens 恒为 0 让客户端计费/展示失真。
        let out_tokens = if self.parser.completion_tokens > 0 {
            self.parser.completion_tokens
        } else {
            (self.out_text.chars().count() as i64 / 2).max(1)
        };
        self.event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {"stop_reason": sr},
                "usage": {"output_tokens": out_tokens}
            }),
        );
    }

    /// 收尾：所有块 content_block_stop → message_delta → message_stop。
    fn finish(&mut self) {
        if self.closed {
            return;
        }
        if !self.started {
            self.ensure_started();
        }
        self.close_blocks();
        self.send_delta();
        self.event("message_stop", &json!({"type": "message_stop"}));
        self.closed = true;
    }

    fn consume(&mut self, evs: Vec<aigw_relay::SseEvent>) {
        use aigw_relay::SseEvent;
        for ev in evs {
            match ev {
                SseEvent::Data(v) => {
                    if v.get("error").is_some() {
                        let msg = v
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("上游返回流式错误");
                        self.event(
                            "error",
                            &json!({"type": "error", "error": {"type": "api_error", "message": msg}}),
                        );
                        self.finish();
                        continue;
                    }
                    if v.get("usage").is_some() {
                        self.usage_seen = true;
                    }
                    let first = v
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first());
                    if let Some(f) = first {
                        if let Some(text) = f
                            .get("delta")
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !text.is_empty() {
                                self.delta(text);
                            }
                        }
                        if let Some(tcs) = f
                            .get("delta")
                            .and_then(|d| d.get("tool_calls"))
                            .and_then(|t| t.as_array())
                        {
                            for tc in tcs {
                                let up = tc
                                    .get("index")
                                    .and_then(|i| i.as_u64())
                                    .unwrap_or(0) as usize;
                                while self.tool_meta.len() <= up {
                                    self.tool_meta.push((String::new(), String::new()));
                                }
                                if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                                    if !id.is_empty() {
                                        self.tool_meta[up].0 = id.to_string();
                                    }
                                }
                                if let Some(n) = tc
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|x| x.as_str())
                                {
                                    if !n.is_empty() {
                                        self.tool_meta[up].1 = n.to_string();
                                    }
                                }
                                let args = tc
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("");
                                let (id, name) = self.tool_meta[up].clone();
                                let id = if id.is_empty() {
                                    format!("toolu_{:016x}_{up}", nanos_hex_int())
                                } else {
                                    id
                                };
                                self.tool_delta(&id, &name, args);
                            }
                        }
                        if let Some(fr) = f.get("finish_reason").and_then(|x| x.as_str()) {
                            let has_tool = !self.tool_meta.is_empty();
                            self.stop_reason = anthropic_stop_reason(Some(fr), has_tool).to_string();
                        }
                    }
                }
                SseEvent::Done => self.finish(),
                SseEvent::Other => {}
            }
        }
        // finish_reason 与 usage 都到齐后提前发 message_delta（不必等流结束），
        // 这样 output_tokens 携带的是上游 usage 帧的真实值，而非 0。
        // 工具块打开期间不能提前收尾（否则 input_json_delta 会被截断）。
        if self.parser.finish_reason.is_some()
            && self.usage_seen
            && self.tool_open.is_none()
            && !self.closed
            && self.delta_sent == false
        {
            self.close_text_block();
            self.send_delta();
        }
    }
}
/// 从上游错误 body 里尽力提取人类可读 message（回退到原文截断）。
fn extract_upstream_message(body: &str) -> String {
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

/// 粗粒度 token 估算：CJK 字符按 1 token，其余非空白字符按 4 字符/token。
fn estimate_tokens(text: &str) -> i64 {
    let mut cjk = 0i64;
    let mut other = 0i64;
    for ch in text.chars() {
        if (ch as u32) >= 0x2E80 {
            cjk += 1;
        } else if !ch.is_whitespace() {
            other += 1;
        }
    }
    (cjk + (other + 3) / 4).max(1)
}

/// ★ 2026-10-26（P2）：联网搜索（Anthropic Messages 兼容形态）。
///
/// 入站（Claude Code / DSH 的搜索 provider 直接可用）：
/// ```json
/// {"query": "关键词", "max_results": 5}
/// ```
/// 出站：`POST {wbBase}/agenttool/v1/search`（复用账号池选号 + 在途租约）。
/// 响应：保持上游扁平形态 `{query, results:[{title,url,snippet}], total_results}`，
/// 同时附一份 Anthropic content-block 形态（`content: [{type:"text", text:"…"}]`），
/// 两种客户端都能直接吃。
async fn search_messages(
    State(st): State<AppState>,
    key: Option<Extension<AccessKeyCtx>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let key = key.map(|e| e.0 .0);
    let raw = match body {
        Ok(b) => b,
        Err(e) => {
            let status = e.status().as_u16();
            return anthropic_error(status, &format!("请求体读取失败：{e}"));
        }
    };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return anthropic_error(400, "invalid JSON body");
    };
    // query 三种来源：query / messages[].content（Anthropic Messages 形态）/ input
    let query = v
        .get("query")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            v.get("messages")
                .and_then(|m| m.as_array())
                .and_then(|arr| arr.last())
                .and_then(|m| m.get("content"))
                .and_then(|c| match c {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(a) => a
                        .iter()
                        .find_map(|x| x.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())),
                    _ => None,
                })
        })
        .or_else(|| v.get("input").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    let max_results = v
        .get("max_results")
        .and_then(|x| x.as_i64())
        .unwrap_or(aigw_relay::wb::SEARCH_DEFAULT_MAX);
    // 白名单：搜索也认模型（可选），有 key 限制时按 model 判一次
    if let (Some(k), Some(model)) = (
        key.as_ref(),
        v.get("model").and_then(|x| x.as_str()),
    ) {
        if !k.allows(model) {
            return anthropic_error(403, &key_model_denied_message(k, model));
        }
    }
    // 搜索目前只有 wb 族上游（/agenttool/v1/search）
    let platform = v
        .get("platform")
        .and_then(|x| x.as_str())
        .unwrap_or("wb-gateway");
    let Some(base) = aigw_relay::wb::wb_checkin_base(platform) else {
        return anthropic_error(
            501,
            "联网搜索目前仅支持 WorkBuddy / CodeBuddy 族上游（/agenttool/v1/search）",
        );
    };
    let accounts = st.accounts.secrets_for(platform);
    if accounts.is_empty() {
        return anthropic_error(
            503,
            &format!("{platform} 还没有账号，先到「接入源」登录后再用搜索"),
        );
    }
    // 选号：走号池（快过期积分优先 + 冷却/在途治理都在里面）
    let ids: Vec<String> = accounts.iter().map(|(id, _)| id.clone()).collect();
    let pick = st.pool.pick(platform, &ids, "search").or_else(|| ids.first().cloned());
    let Some(acc) = pick else {
        return anthropic_error(503, "所有账号当前都在冷却/在飞打满，稍后重试");
    };
    let secret = accounts
        .iter()
        .find(|(id, _)| id == &acc)
        .map(|(_, s)| s.clone())
        .unwrap_or_default();
    let http = aigw_relay::auth_login::auth_http();
    let started = std::time::Instant::now();
    match aigw_relay::wb::search(&http, base, &secret, &query, max_results).await {
        Ok(mut d) => {
            st.pool.mark_health(&acc, true, "");
            let ms = started.elapsed().as_millis() as i64;
            tracing::info!(account = %acc, query = %query, ms, "搜索成功");
            // Anthropic content-block 形态：把结果拼成一段可读文本，
            // 让只认 content 数组的客户端也能直接用。
            let results = d.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            let mut text = String::new();
            for (i, r) in results.iter().enumerate() {
                let title = r.get("title").and_then(|x| x.as_str()).unwrap_or("");
                let url = r.get("url").and_then(|x| x.as_str()).unwrap_or("");
                let snip = r.get("snippet").and_then(|x| x.as_str()).unwrap_or("");
                text.push_str(&format!("{}. {}\n{}\n", i + 1, title, url));
                if !snip.is_empty() {
                    text.push_str(&format!("{}\n", snip));
                }
                text.push('\n');
            }
            if let Some(o) = d.as_object_mut() {
                o.insert(
                    "content".to_string(),
                    json!([{"type": "text", "text": text.trim_end()}]),
                );
                o.insert("account".to_string(), json!(acc));
                o.insert("elapsed_ms".to_string(), json!(ms));
                o.insert("type".to_string(), json!("search_result"));
            }
            Json(d).into_response()
        }
        Err(e) => {
            st.pool.mark_health(&acc, false, "搜索失败");
            anthropic_error(502, &format!("搜索上游失败：{e}"))
        }
    }
}

/// /v1/messages/count_tokens：Claude Code 等客户端在发送前会调它做预算。
/// 这是本地估算（不消耗上游额度），把 Anthropic 请求转成 OpenAI 形状后按字符估算。
async fn count_tokens(    State(_st): State<AppState>,
    key: Option<Extension<AccessKeyCtx>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // ★ 白名单必须在这里也判一次：这个接口不碰上游（纯本地估算），但它照样认 model 字段，
    //   而且是四条协议入口里唯一会「按模型名回话」的旁路 —— 漏判就变成
    //   「白名单之外的模型名也能过」，客户端据此以为可用（另三条入口却是 403）。
    //   Claude Code 发正式请求前都会先调它做预算，口径必须与 messages 一致。
    let key = key.map(|e| e.0 .0);
    let raw = match body {
        Ok(b) => b,
        Err(e) => {
            let status = e.status().as_u16();
            return anthropic_error(status, &format!("请求体读取失败：{e}"));
        }
    };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return anthropic_error(400, "invalid JSON body");
    };
    if !v.is_object() {
        return anthropic_error(400, "请求体必须是 JSON 对象");
    }
    if let Some(k) = key.as_ref() {
        let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("unknown");
        if !k.allows(model) {
            return anthropic_error(403, &key_model_denied_message(k, model));
        }
    }
    let oa = match anthropic_to_openai(&v) {
        Ok(oa) => oa,
        Err(msg) => return anthropic_error(400, &msg),
    };
    let mut text = String::new();
    if let Some(msgs) = oa.get("messages") {
        text.push_str(&msgs.to_string());
    }
    if let Some(tools) = oa.get("tools") {
        text.push_str(&tools.to_string());
    }
    if let Some(sys) = v.get("system") {
        text.push_str(&sys.to_string());
    }
    Json(json!({"input_tokens": estimate_tokens(&text)})).into_response()
}
async fn messages_completions(
    State(st): State<AppState>,
    key: Option<Extension<AccessKeyCtx>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let key = key.map(|e| e.0 .0);
    let raw = match body {
        Ok(b) => b,
        Err(e) => {
            let status = e.status().as_u16();
            return anthropic_error(status, &format!("请求体读取失败：{e}"));
        }
    };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return anthropic_error(400, "invalid JSON body");
    };
    if !v.is_object() {
        return anthropic_error(400, "请求体必须是 JSON 对象");
    }
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    if let Some(k) = key.as_ref() {
        if !k.allows(&model) {
            return anthropic_error(403, &key_model_denied_message(k, &model));
        }
    }
    // ★ 2026-10-26（用户排队消息①）：format=anthropic 的平台（自定义 API）走
    //   原生 Anthropic 透传——不再 anthropic_to_openai 绕一圈（工具调用/系统提示词/
    //   thinking 块在往返转换里会丢细节），直接把请求体打到上游 /v1/messages。
    if let Some(spec) = native_anthropic_spec(&st, &model) {
        return anthropic_passthrough(&st, spec, &v, &model).await;
    }
    let mut oa = match anthropic_to_openai(&v) {
        Ok(oa) => oa,
        Err(msg) => return anthropic_error(400, &msg),
    };
    carry_session(&mut oa, &v);
    match chat_core(&st, oa).await {
        Ok(ChatCoreOut::Json(v)) => Json(openai_to_anthropic(&v, &model)).into_response(),
        Ok(ChatCoreOut::Stream(stream)) => {
            let conv = anthropic_stream(stream, model);
            sse_response(Body::from_stream(conv))
        }
        Err(ChatCoreErr::Local {
            status,
            message,
            code: _,
            retry_after,
        }) => {
            let mut resp = anthropic_error(status, &message);
            if let Some(ra) = retry_after {
                resp.headers_mut().insert(
                    header::RETRY_AFTER,
                    axum::http::HeaderValue::from_str(&ra.to_string()).unwrap(),
                );
            }
            resp
        }
        Err(ChatCoreErr::Upstream {
            status,
            body,
            hint,
        }) => {
            let msg = extract_upstream_message(&body);
            // 幂等兜底：chat_core 里所有 ChatCoreErr::Upstream 已经过 upstream_status，
            // 这里再过一次是防御性的（502→502 不变）。留着是因为将来若有人在 chat_core
            // 里加新的 Upstream 分支却忘了映射，200+HTML 会直接变成「HTTP 200 + 错误 JSON」，
            // Claude Code 把它当成功消息解析，用户只看到「响应格式错误」而看不到真实原因。
            anthropic_error(upstream_status(status), &msg_with_hint(&msg, hint.as_deref()))
        }
    }
}

// ================= /v1/responses（OpenAI Responses API） =================

fn nanos_hex() -> String {
    format!(
        "{:016x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

fn responses_error(status: u16, msg: &str) -> Response {
    let kind = match status {
        400 => "invalid_request_error",
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

/// Responses 的 content 可以是字符串，也可以是 [{"type":"input_text","text":…}] 块数组。
fn responses_text(c: Option<&Value>) -> String {
    let Some(c) = c else { return String::new() };
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(arr) = c.as_array() {
        for b in arr {
            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                out.push_str(t);
            } else if let Some(s) = b.as_str() {
                out.push_str(s);
            }
        }
    }
    out
}

/// Responses 请求 → OpenAI chat 请求（input 支持字符串、消息数组或裸字符串数组）。
fn responses_to_openai(v: &Value) -> Result<Value, String> {
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(ins) = v.get("instructions").and_then(|x| x.as_str()) {
        if !ins.trim().is_empty() {
            msgs.push(json!({"role": "system", "content": ins}));
        }
    }
    match v.get("input") {
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                return Err("input 不能为空".to_string());
            }
            msgs.push(json!({"role": "user", "content": s}));
        }
        Some(Value::Array(items)) => {
            for it in items {
                if let Some(s) = it.as_str() {
                    if !s.trim().is_empty() {
                        msgs.push(json!({"role": "user", "content": s}));
                    }
                    continue;
                }
                // ★ 工具调用链必须转换，否则 Codex CLI 的多轮工具对话是**断的**：
                //   Responses 把工具往返放在 input 数组里（function_call /
                //   function_call_output 两种 item），旧实现把非 user/assistant/system
                //   的 item 一律 continue 丢弃 —— 上游看到的是一串「用户说了句话」
                //   却没有任何工具调用与结果，模型只能重新猜一遍或直接答错。
                match it.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "function_call" => {
                        let call_id = it
                            .get("call_id")
                            .or_else(|| it.get("id"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = it
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if name.is_empty() {
                            continue;
                        }
                        // arguments 在 Responses 里是**字符串**；个别客户端给对象，
                        // 统一成 OpenAI 要求的字符串形态。
                        let args = match it.get("arguments") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v @ Value::Object(_)) => v.to_string(),
                            _ => "{}".to_string(),
                        };
                        msgs.push(json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [{
                                "id": call_id,
                                "type": "function",
                                "function": {"name": name, "arguments": args},
                            }],
                        }));
                        continue;
                    }
                    "function_call_output" => {
                        let call_id = it
                            .get("call_id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if call_id.is_empty() {
                            continue;
                        }
                        let text = match it.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => v.to_string(),
                            None => String::new(),
                        };
                        msgs.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": text,
                        }));
                        continue;
                    }
                    // reasoning / item_reference 等纯客户端状态，丢弃无害
                    "reasoning" | "item_reference" => continue,
                    _ => {}
                }
                let raw_role = it.get("role").and_then(|x| x.as_str()).unwrap_or("user");
                let role = if raw_role == "developer" { "system" } else { raw_role };
                if role != "user" && role != "assistant" && role != "system" {
                    continue;
                }
                let text = responses_text(it.get("content"));
                if text.trim().is_empty() {
                    continue;
                }
                msgs.push(json!({"role": role, "content": text}));
            }
        }
        _ => return Err("缺少 input 字段（字符串或消息数组）".to_string()),
    }
    if msgs.is_empty() || !msgs.iter().any(|m| m.get("role").and_then(|r| r.as_str()) != Some("system")) {
        return Err("input 里没有可发送的用户/助手消息".to_string());
    }
    let mut oa = json!({
        "model": v.get("model").cloned().unwrap_or_else(|| json!("unknown")),
        "messages": msgs,
    });
    let val = oa.as_object_mut().expect("object");
    for (from, to) in [
        ("max_output_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stream", "stream"),
    ] {
        if let Some(x) = v.get(from) {
            val.insert(to.to_string(), x.clone());
        }
    }
    if let Some(r) = v.get("reasoning").and_then(|x| x.get("effort")) {
        let _ = r; // 上游多为 OpenAI 兼容端点，effort 无对应字段，忽略
    }
    // ★ 工具定义也要换形状：Responses 是**扁平**的
    //   {type:"function", name, description, parameters, strict}
    //   OpenAI chat 是**嵌套**的
    //   {type:"function", function:{name, description, parameters}}
    //   旧实现完全不转 tools，于是 Codex CLI 发过来的工具在上游眼里不存在 ——
    //   模型只能凭文字瞎猜，多轮工具流程根本跑不起来。
    if let Some(Value::Array(tools)) = v.get("tools") {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let ty = t.get("type").and_then(|x| x.as_str()).unwrap_or("function");
                // 非 function 类（web_search / file_search / mcp 等托管工具）没有
                // OpenAI chat 等价物，直接跳过而不是原样透传（透传会让上游 400）。
                if ty != "function" {
                    return None;
                }
                // 已经是嵌套形状的（个别客户端混着发）原样保留
                if t.get("function").is_some() {
                    return Some(t.clone());
                }
                let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("");
                if name.is_empty() {
                    return None;
                }
                let mut f = serde_json::Map::new();
                f.insert("name".into(), json!(name));
                if let Some(d) = t.get("description") {
                    f.insert("description".into(), d.clone());
                }
                f.insert(
                    "parameters".into(),
                    t.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                );
                Some(json!({"type": "function", "function": Value::Object(f)}))
            })
            .collect();
        if !converted.is_empty() {
            val.insert("tools".into(), Value::Array(converted));
            // tool_choice：Responses 的 {"type":"function","name":"x"} → chat 的
            // {"type":"function","function":{"name":"x"}}；"auto"/"none"/"required" 直传。
            match v.get("tool_choice") {
                Some(Value::String(s)) => {
                    val.insert("tool_choice".into(), json!(s));
                }
                Some(tc @ Value::Object(_)) => {
                    let name = tc.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    if !name.is_empty() {
                        val.insert(
                            "tool_choice".into(),
                            json!({"type": "function", "function": {"name": name}}),
                        );
                    }
                }
                _ => {}
            }
        }
    }
    Ok(oa)
}

fn responses_usage(tin: i64, tout: i64) -> Value {
    json!({
        "input_tokens": tin,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": tout,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": tin + tout,
    })
}

fn responses_output(text: &str, msg_id: &str) -> Value {
    json!({
        "id": msg_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

/// OpenAI chat.completion → Responses 对象。
fn openai_to_responses(v: &Value, model: &str) -> Value {
    let first = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let text = first
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let reasoning = first
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let (tin, tout) = conv_usage(v);
    let msg_id = format!("msg_{}", nanos_hex());
    let mut output: Vec<Value> = Vec::new();
    if !reasoning.trim().is_empty() {
        output.push(json!({
            "id": format!("rs_{}", nanos_hex()),
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": reasoning}],
        }));
    }
    // ★ 反向：上游返回的 tool_calls 必须变成 Responses 的 function_call item，
    //   否则 Codex CLI 收到的响应里只有一段空文本，工具调用被静默吞掉 ——
    //   客户端永远等不到要执行的动作，整个 agent 循环卡死。
    let tool_calls = first
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array());
    if text.trim().is_empty() && tool_calls.map(|t| !t.is_empty()).unwrap_or(false) {
        // 纯工具调用：不产生空 message item（客户端会把空 assistant 消息当答案）
    } else {
        output.push(responses_output(text, &msg_id));
    }
    if let Some(calls) = tool_calls {
        for c in calls {
            let call_id = c
                .get("id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{}", nanos_hex()));
            let f = c.get("function");
            let name = f
                .and_then(|x| x.get("name"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let args = match f.and_then(|x| x.get("arguments")) {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => "{}".to_string(),
            };
            output.push(json!({
                "id": format!("fc_{}", nanos_hex()),
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": name,
                "arguments": args,
            }));
        }
    }
    json!({
        "id": format!("resp_{}", nanos_hex()),
        "object": "response",
        "created_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "status": "completed",
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "instructions": Value::Null,
        "model": model,
        "output": output,
        "output_text": text,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
        "usage": responses_usage(tin, tout),
    })
}

/// Responses 流式转换：OpenAI chunk → response.output_text.delta 等事件序列。
struct RespState {
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    parser: aigw_relay::SseParser,
    model: String,
    resp_id: String,
    item_id: String,
    created_at: u64,
    seq: u64,
    started: bool,
    item_open: bool,
    closed: bool,
    out_text: String,
    /// 上游流里累积的工具调用（Responses 要的是 function_call item）。
    /// 旧实现完全忽略 delta.tool_calls —— Codex CLI 流式跑工具时收到的只有空文本，
    /// agent 循环直接卡死（它永远等不到要执行的动作）。
    tool_calls: Vec<StreamToolCall>,
    pending: std::collections::VecDeque<Bytes>,
}

/// 流式工具调用累积器。
#[derive(Default, Clone)]
struct StreamToolCall {
    call_id: String,
    name: String,
    args: String,
}

impl RespState {
    fn event(&mut self, name: &str, mut data: Value) {
        if let Some(o) = data.as_object_mut() {
            o.insert("type".to_string(), json!(name));
            o.insert("sequence_number".to_string(), json!(self.seq));
        }
        self.seq += 1;
        let s = format!("event: {name}\ndata: {data}\n\n");
        self.pending.push_back(Bytes::from(s));
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let resp = json!({
            "id": self.resp_id,
            "object": "response",
            "created_at": self.created_at,
            "status": "in_progress",
            "model": self.model,
            "output": [],
        });
        self.event("response.created", json!({"response": resp}));
    }

    fn ensure_item(&mut self) {
        if self.item_open {
            return;
        }
        self.ensure_started();
        self.item_open = true;
        let item = json!({
            "id": self.item_id,
            "type": "message",
            "status": "in_progress",
            "role": "assistant",
            "content": [],
        });
        self.event(
            "response.output_item.added",
            json!({"output_index": 0, "item": item}),
        );
        self.event(
            "response.content_part.added",
            json!({
                "item_id": self.item_id,
                "output_index": 0,
                "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []},
            }),
        );
    }

    fn delta(&mut self, text: &str) {
        self.ensure_item();
        self.out_text.push_str(text);
        self.event(
            "response.output_text.delta",
            json!({
                "item_id": self.item_id,
                "output_index": 0,
                "content_index": 0,
                "delta": text,
            }),
        );
    }

    /// 累积上游 delta.tool_calls：按 index 归并，id/name 只在首帧出现，
    /// arguments 逐帧拼接（OpenAI 流式工具调用的标准形状）。
    fn push_tool_calls(&mut self, calls: &[Value]) {
        for c in calls {
            let idx = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            while self.tool_calls.len() <= idx {
                self.tool_calls.push(StreamToolCall::default());
            }
            let slot = &mut self.tool_calls[idx];
            if let Some(id) = c.get("id").and_then(|x| x.as_str()) {
                if !id.is_empty() {
                    slot.call_id = id.to_string();
                }
            }
            if let Some(f) = c.get("function") {
                if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                    if !n.is_empty() {
                        slot.name = n.to_string();
                    }
                }
                if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                    slot.args.push_str(a);
                }
            }
        }
    }

    fn finish(&mut self) {
        if self.closed {
            return;
        }
        let has_tools = self.tool_calls.iter().any(|c| !c.name.is_empty());
        // 纯工具调用（没有正文）时不能凭空造一个空 message item：
        // 客户端会把空 assistant 消息当成最终答案，直接结束回合。
        if !has_tools || !self.out_text.is_empty() {
            self.ensure_item();
        }
        let text = self.out_text.clone();
        if self.item_open {
            self.event(
                "response.output_text.done",
                json!({
                    "item_id": self.item_id,
                    "output_index": 0,
                    "content_index": 0,
                    "text": text,
                }),
            );
            self.event(
                "response.content_part.done",
                json!({
                    "item_id": self.item_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": text, "annotations": []},
                }),
            );
            self.event(
                "response.output_item.done",
                json!({"output_index": 0, "item": responses_output(&text, &self.item_id)}),
            );
        }
        // 工具调用补成 function_call item（output_index 顺延到正文之后）
        let mut output: Vec<Value> = Vec::new();
        if self.item_open {
            output.push(responses_output(&text, &self.item_id));
        }
        for c in self.tool_calls.clone() {
            if c.name.is_empty() {
                continue;
            }
            let idx = output.len();
            let call_id = if c.call_id.is_empty() {
                format!("call_{}", nanos_hex())
            } else {
                c.call_id.clone()
            };
            let item = json!({
                "id": format!("fc_{}", nanos_hex()),
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": c.name,
                "arguments": if c.args.trim().is_empty() { "{}".to_string() } else { c.args.clone() },
            });
            self.event(
                "response.output_item.added",
                json!({"output_index": idx, "item": {
                    "id": item["id"],
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": item["call_id"],
                    "name": item["name"],
                    "arguments": "",
                }}),
            );
            self.event(
                "response.output_item.done",
                json!({"output_index": idx, "item": item}),
            );
            output.push(item);
        }
        let tin = self.parser.prompt_tokens;
        let tout = if self.parser.completion_tokens > 0 {
            self.parser.completion_tokens
        } else {
            (self.out_text.chars().count() as i64 / 2).max(1)
        };
        let resp = json!({
            "id": self.resp_id,
            "object": "response",
            "created_at": self.created_at,
            "status": "completed",
            "model": self.model,
            "output": output,
            "output_text": text,
            "usage": responses_usage(tin, tout),
        });
        self.event("response.completed", json!({"response": resp}));
        self.closed = true;
    }

    fn consume(&mut self, evs: Vec<aigw_relay::SseEvent>) {
        use aigw_relay::SseEvent;
        for ev in evs {
            match ev {
                SseEvent::Data(v) => {
                    if let Some(err) = v.get("error") {
                        let msg = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("上游返回流式错误");
                        self.ensure_started();
                        self.event(
                            "error",
                            json!({"message": msg, "code": "upstream_error"}),
                        );
                        self.finish();
                        continue;
                    }
                    let first = v
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first());
                    if let Some(f) = first {
                        let delta = f.get("delta");
                        if let Some(text) = delta
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !text.is_empty() {
                                self.delta(text);
                            }
                        }
                        if let Some(calls) =
                            delta.and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array())
                        {
                            if !calls.is_empty() {
                                // 先发 response.created，客户端才知道响应已开始
                                self.ensure_started();
                                self.push_tool_calls(calls);
                            }
                        }
                    }
                }
                SseEvent::Done => self.finish(),
                SseEvent::Other => {}
            }
        }
    }
}

fn responses_stream(
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    model: String,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    use futures_util::StreamExt;
    let state = RespState {
        inner,
        parser: aigw_relay::SseParser::new(),
        model,
        resp_id: format!("resp_{}", nanos_hex()),
        item_id: format!("msg_{}", nanos_hex()),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        seq: 0,
        started: false,
        item_open: false,
        closed: false,
        out_text: String::new(),
        tool_calls: Vec::new(),
        pending: std::collections::VecDeque::new(),
    };
    Box::pin(futures_util::stream::unfold(state, |mut s| async move {
        loop {
            if let Some(b) = s.pending.pop_front() {
                return Some((Ok::<Bytes, std::io::Error>(b), s));
            }
            if s.closed {
                return None;
            }
            match s.inner.next().await {
                Some(Ok(bytes)) => {
                    let evs = s.parser.feed(&bytes);
                    s.consume(evs);
                }
                Some(Err(_e)) => s.finish(),
                None => s.finish(),
            }
        }
    }))
}

async fn responses_completions(
    State(st): State<AppState>,
    key: Option<Extension<AccessKeyCtx>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let key = key.map(|e| e.0 .0);
    let raw = match body {
        Ok(b) => b,
        Err(e) => {
            let status = e.status().as_u16();
            return responses_error(status, &format!("请求体读取失败：{e}"));
        }
    };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return responses_error(400, "invalid JSON body");
    };
    if !v.is_object() {
        return responses_error(400, "请求体必须是 JSON 对象");
    }
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    if let Some(k) = key.as_ref() {
        if !k.allows(&model) {
            return responses_error(403, &key_model_denied_message(k, &model));
        }
    }
    let mut oa = match responses_to_openai(&v) {
        Ok(oa) => oa,
        Err(msg) => return responses_error(400, &msg),
    };
    carry_session(&mut oa, &v);
    match chat_core(&st, oa).await {
        Ok(ChatCoreOut::Json(v)) => Json(openai_to_responses(&v, &model)).into_response(),
        Ok(ChatCoreOut::Stream(stream)) => {
            let conv = responses_stream(stream, model);
            sse_response(Body::from_stream(conv))
        }
        Err(ChatCoreErr::Local {
            status,
            message,
            code: _,
            retry_after,
        }) => {
            let mut resp = responses_error(status, &message);
            if let Some(ra) = retry_after {
                resp.headers_mut().insert(
                    header::RETRY_AFTER,
                    axum::http::HeaderValue::from_str(&ra.to_string()).unwrap(),
                );
            }
            resp
        }
        Err(ChatCoreErr::Upstream {
            status,
            body,
            hint,
        }) => {
            let msg = extract_upstream_message(&body);
            // 同 /v1/messages：幂等兜底（chat_core 已映射过），保证 200+HTML
            // 绝不会以 200 返回给 Codex CLI。
            responses_error(upstream_status(status), &msg_with_hint(&msg, hint.as_deref()))
        }
    }
}
/// 协议转换面（/v1/messages、/v1/responses 的非流式响应）用的 usage 兜底：
/// 上游没给 completion_tokens 时按正文/工具参数字符估算，别让客户端看到恒 0 的 output_tokens。
fn conv_usage(v: &Value) -> (i64, i64) {
    let (tin, tout) = extract_usage(v);
    if tout > 0 {
        return (tin, tout);
    }
    let text_chars = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let args_chars: usize = v
        .pointer("/choices/0/message/tool_calls")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.pointer("/function/arguments").and_then(|x| x.as_str()))
                .map(|s| s.chars().count())
                .sum()
        })
        .unwrap_or(0);
    let chars = text_chars + args_chars;
    if chars > 0 {
        (tin, (chars / 2).max(1) as i64)
    } else {
        (tin, 0)
    }
}

fn extract_usage(v: &Value) -> (i64, i64) {
    let u = v.get("usage");
    (
        u.and_then(|u| u.get("prompt_tokens"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        u.and_then(|u| u.get("completion_tokens"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
    )
}

/// 非流式记账口径：上游没给 usage 时按「入参字节 / 4、出参字符 / 2」兜底估算。
///
/// ★ 旧实现直接把缺失的 usage 记成 0：不少 OpenAI 兼容站（自建网关、部分免费站）
/// 非流式响应里不带 usage 字段，台账就整年记 0 token，看板与限流判断全部失真。
/// 口径与 aigw_relay::estimate_stream_tokens 及流式 fill_missing_tokens 保持一致。
fn usage_or_estimate(v: &Value, in_bytes: usize) -> (i64, i64) {
    let (tin, tout) = extract_usage(v);
    let tin = if tin > 0 { tin } else { (in_bytes / 4).max(1) as i64 };
    let tout = if tout > 0 {
        tout
    } else {
        let chars = v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map(|s| s.chars().count())
            .unwrap_or(0);
        if chars > 0 {
            (chars / 2).max(1) as i64
        } else {
            0
        }
    };
    (tin, tout)
}

async fn web_file(uri: Uri) -> Response {
    // 未知的 /api/* 与 /v1/* 必须是 404 JSON，不能 SPA 回退成 index.html：
    // 旧实现让 /v1/embeddings 拿到 HTTP 200 + HTML，SDK 报「unexpected token <」
    // 而不是「该端点不支持」，误导性极强；前端也会把 HTML 当 JSON 解析。
    let raw = uri.path();
    if raw.starts_with("/api/") || raw.starts_with("/v1/") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "message": format!("未实现的端点 {raw}（本面板只提供 /v1/models、/v1/chat/completions、/v1/messages、/v1/messages/count_tokens 与 /v1/responses）"),
                    "type": "invalid_request_error",
                    "code": 404,
                }
            })),
        )
            .into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Web::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    // 前端迭代期禁强缓存：浏览器每次向服务端校验，避免旧 JS 卡死交互
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                f.data.to_vec(),
            )
                .into_response()
        }
        // 带扩展名的静态资源（/foo.js、/nope.png）找不到就是 404：
        // 回 index.html 会让浏览器拿 HTML 当 JS/CSS 解析，控制台报一堆
        // 「Unexpected token '<'」，比干脆的 404 难查得多。
        None if path.rsplit('/').next().is_some_and(|f| f.contains('.')) => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string())],
            "404 Not Found",
        )
            .into_response(),
        // SPA 回退：未知路径一律回首页
        None => match Web::get("index.html") {
            Some(f) => (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                    (header::CACHE_CONTROL, "no-cache".to_string()),
                ],
                f.data.to_vec(),
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_string(resp: Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// 真实客户端一定会带 Host（HTTP/1.1 强制），测试里也照此构造：
    /// 门禁现在会拒绝管理面上的无 Host 请求，不带 Host 的 helper 会全线误报。
    fn req(uri: &str) -> Request<Body> {
        Request::get(uri)
            .header("host", "127.0.0.1:8800")
            .body(Body::empty())
            .unwrap()
    }

    /// 完全裸的请求（连 Host 都没有），只给门禁密闭测试用。
    fn raw_req(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    fn spec_toml(raw: &str) -> ProviderSpec {
        toml::from_str(raw).unwrap()
    }

    fn state(specs: Vec<ProviderSpec>) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(
            specs,
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        )
    }

    /// 带公益站存储的状态（公益站同域去重 / 展示过滤用）。
    fn state_with_free(specs: Vec<ProviderSpec>, accounts: AccountStore, free: FreeStore) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(
            specs,
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(accounts),
            Arc::new(CustomStore::empty()),
            Arc::new(free),
        )
    }

    fn state_with_accounts(specs: Vec<ProviderSpec>, accounts: AccountStore) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(
            specs,
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(accounts),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        )
    }

    #[test]
    fn backup_includes_wal_and_flags_checkpoint_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("accounts.json"), b"{\"accounts\":[]}").unwrap();
        std::fs::write(dir.path().join("panel.db"), b"MAIN").unwrap();
        std::fs::write(dir.path().join("panel.db-wal"), b"WALDATA").unwrap();
        std::fs::write(dir.path().join("panel.db-shm"), b"SHM").unwrap();

        // 脱敏导出：凭据文件只留名字+大小；但 panel.db-wal 必须收（checkpoint 失败时它是
        // 唯一还带着台账的文件）
        let v = collect_backup(dir.path(), false, true, true);
        let names: Vec<String> = v["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"panel.db".to_string()), "names={names:?}");
        assert!(names.contains(&"panel.db-wal".to_string()), "names={names:?}");
        assert!(!names.contains(&"panel.db-shm".to_string()), "-shm 是重建用的索引，不该导出");
        let acct = v["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "accounts.json")
            .unwrap();
        assert_eq!(acct["redacted"], true, "脱敏导出不能带凭据内容");

        // checkpoint 失败时导出必须自证：否则用户拿到一个 4KB 空壳主库还以为备份完整
        let bad = collect_backup(dir.path(), true, true, false);
        assert_eq!(bad["checkpoint"], false);
        assert!(bad["checkpoint_warning"].as_str().unwrap().contains("WAL"));

        // 完整导出：凭据文件带内容
        let full = collect_backup(dir.path(), true, true, true);
        let acct = full["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "accounts.json")
            .unwrap();
        assert!(acct.get("bytes").is_some(), "完整导出应带 bytes");
    }
    #[tokio::test]
    async fn unknown_api_path_is_404_json_not_spa() {
        // 旧实现：/api/nope 走 SPA 回退 → HTTP 200 + index.html，前端把 HTML 当 JSON 解析。
        let app = build_app(state(vec![]));
        let res = app.oneshot(req("/api/nope")).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_string(res).await;
        assert!(body.contains("未实现的端点"), "body={body}");
    }

    #[tokio::test]
    async fn unknown_v1_path_is_404_json_not_spa() {
        // 旧实现：/v1/embeddings 拿到 HTTP 200 + HTML，SDK 报「unexpected token <」。
        let app = build_app(state(vec![]));
        // 提示文案要列全真实存在的端点，否则用户按它去试 count_tokens 会以为不支持
        // （Claude Code 每次发请求前都要调它）。
        let app2 = build_app(state(vec![]));
        let body0 = body_string(app2.oneshot(req("/v1/embeddings")).await.unwrap()).await;
        for p in ["/v1/messages/count_tokens/x", "/v1/embeddings", "/v1/completions", "/v1/unknown"] {
            let res = app.clone().oneshot(req(p)).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "path={p}");
            let body = body_string(res).await;
            assert!(body.contains("invalid_request_error"), "path={p} body={body}");
        }
        assert!(
            body0.contains("/v1/messages/count_tokens"),
            "404 文案要把 count_tokens 列进去：{body0}"
        );
    }

    #[tokio::test]
    async fn spa_fallback_still_serves_index() {
        // 前端路由（非 /api、非 /v1）必须继续回 index.html，否则刷新页面就白屏。
        let app = build_app(state(vec![]));
        let res = app.oneshot(req("/sources")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/html"), "content-type={ct}");
    }

    #[tokio::test]
    async fn chat_body_larger_than_axum_default_is_accepted() {
        // axum 默认 2MB：旧实现 3MB 的 prompt 直接被框架 413 掉。
        // 现在上限 32MB，超大 body 应该走到业务层（这里没有平台 → 503 未知模型）。
        let app = build_app(state(vec![]));
        let big = "x".repeat(3 * 1024 * 1024);
        let payload = format!(r#"{{"model":"nobody","messages":[{{"role":"user","content":"{big}"}}]}}"#);
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "3MB 请求体不应再被框架 413"
        );
        let body = body_string(res).await;
        assert!(body.contains("\"error\""), "body={body}");
    }

    #[tokio::test]
    async fn chat_unsupported_platform_hidden_from_models() {
        // apk-wps / apk-nano：chat_supported=false，只做登录验活。
        // 旧实现把它们算「可用」，卡片显示可用、模型进清单，点开必 501。
        let spec = spec_toml(
            r#"
id = "verify-only"
name = "VerifyOnly"
enabled = true
auth = "anon"
base_url = "https://example.invalid/v1"
format = "openai"
models = ["only-verify"]
models_policy = "static"
stream_dialect = "sse_delta"
chat_supported = false
"#,
        );
        let st = state(vec![spec.clone()]);
        assert!(!st.platform_usable(&spec), "chat_supported=false 不应算可用");
        let app = build_app(st);
        let res = app.oneshot(req("/v1/models")).await.unwrap();
        let body = body_string(res).await;
        assert!(!body.contains("only-verify"), "body={body}");
    }

    fn with_connect(mut r: Request<Body>, ip: [u8; 4]) -> Request<Body> {
        r.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((ip, 5555))));
        r
    }

    #[tokio::test]
    async fn v1_is_rate_limited_and_api_is_not() {
        // 全局 2/分钟：前两条放行，第三条 429；/api/* 不吃这个额度（面板轮询会把它吃光）。
        let app = build_app(state(vec![]).with_limits(0, 2));
        for i in 0..2 {
            let res = app.clone().oneshot(req("/v1/models")).await.unwrap();
            assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS, "第 {i} 条不该被限流");
        }
        let res = app.clone().oneshot(req("/v1/models")).await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry = res
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        assert!(matches!(retry, Some(s) if s >= 1 && s <= 60), "retry-after={retry:?}");
        let body = body_string(res).await;
        assert!(body.contains("rate_limited"), "body={body}");
        // 管理接口不受影响
        let res = app.clone().oneshot(req("/api/health")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn models_list_is_trimmed_to_the_keys_allow_list() {
        // 一把只允许 kilo-auto/free 的 key，不该从 /v1/models 里看到别的模型：
        // 否则客户端会逐个试 147 个必然 403 的名字，日志里全是对不上号的报错。
        let spec = spec_toml(
            r#"
id = "model-probe"
name = "ModelProbe"
enabled = true
auth = "anon"
base_url = "https://example.invalid/v1"
format = "openai"
models = ["alpha", "beta"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let keys = aigw_store::KeyStore::empty();
        let (_k, secret) = keys.add("窄口", vec!["alpha".into()], "");
        let mut st = state(vec![spec]);
        st.keys = std::sync::Arc::new(keys);
        let app = build_app(st);
        let mut r = req("/v1/models");
        r.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {secret}").parse().unwrap(),
        );
        let body = body_string(app.clone().oneshot(r).await.unwrap()).await;
        assert!(body.contains("alpha"), "白名单里的模型必须在：{body}");
        assert!(!body.contains("beta"), "白名单外的模型不该出现：{body}");
        // 不带 key 的请求仍是完整清单
        let all = body_string(app.oneshot(req("/v1/models")).await.unwrap()).await;
        assert!(all.contains("alpha") && all.contains("beta"), "{all}");
    }

    #[tokio::test]
    async fn key_rate_limit_is_per_access_key_and_leaves_other_keys_alone() {
        // 每把 key 1/分钟：A 第 2 条 429，B 仍能过 1 条，不带 key 的匿名请求不受影响。
        // 这条是「借出去的 key 别把大家的额度吃光」的机制证明。
        let keys = aigw_store::KeyStore::empty();
        let (_ka, sa) = keys.add("脚本A", vec![], "");
        let (_kb, sb) = keys.add("脚本B", vec![], "");
        let mut st = state(vec![]);
        st.keys = std::sync::Arc::new(keys);
        let app = build_app(st.with_limits(0, 0).with_key_rate(1));
        let call = |s: String| {
            let mut r = req("/v1/models");
            r.headers_mut().insert(
                header::AUTHORIZATION,
                format!("Bearer {s}").parse().unwrap(),
            );
            r
        };
        let res = app.clone().oneshot(call(sa.clone())).await.unwrap();
        assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS, "A 第 1 条应放行");
        let res = app.clone().oneshot(call(sa.clone())).await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS, "A 第 2 条应被限流");
        let res = app.clone().oneshot(call(sb.clone())).await.unwrap();
        assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS, "B 有自己的额度，不该被 A 连累");
        // 不带 key 的请求走原来的 IP/全局闸，不受 key 额度影响
        let res = app.clone().oneshot(req("/v1/models")).await.unwrap();
        assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn per_ip_limit_counts_each_peer_separately() {
        // 每 IP 1/分钟、全局关：同一个 IP 第 2 条就 429，另一个 IP 还有自己的额度。
        let app = build_app(state(vec![]).with_limits(1, 0));
        let res = app
            .clone()
            .oneshot(with_connect(req("/v1/models"), [10, 0, 0, 1]))
            .await
            .unwrap();
        assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let res = app
            .clone()
            .oneshot(with_connect(req("/v1/models"), [10, 0, 0, 1]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS, "同 IP 第 2 条应被限流");
        let res = app
            .clone()
            .oneshot(with_connect(req("/v1/models"), [10, 0, 0, 2]))
            .await
            .unwrap();
        assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS, "别的 IP 不该被牵连");
        // 没有 ConnectInfo（单测 oneshot / 反代剥掉对端信息）时不按 IP 计，只走全局
        let res = app.clone().oneshot(req("/v1/models")).await.unwrap();
        assert_ne!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn rate_limit_rejections_show_up_in_health() {
        // 429 只在响应体里出现过一次，事后没人知道「面板刚才为什么全失败」。
        // 这里要求 health 里能看出：被挡过几次、上次多久前、是哪个维度触顶。
        let app = build_app(state(vec![]).with_limits(1, 0));
        // 第 1 条放行
        let _ = app
            .clone()
            .oneshot(with_connect(req("/v1/models"), [10, 0, 0, 1]))
            .await
            .unwrap();
        // 第 2、3 条被挡
        for _ in 0..2 {
            let res = app
                .clone()
                .oneshot(with_connect(req("/v1/models"), [10, 0, 0, 1]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        let res = app.clone().oneshot(req("/api/health")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "/api/health 不该被 /v1 的限流牵连");
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["limits"]["enabled"], true, "{v}");
        assert_eq!(v["limits"]["per_ip_rpm"], 1);
        assert_eq!(v["limits"]["rejected_total"], 2, "两次被挡都要计数：{v}");
        assert_eq!(v["limits"]["last_reject_scope"], "ip", "{v}");
        assert!(v["limits"]["last_reject_s_ago"].as_u64().unwrap() <= 5, "{v}");
        assert!(v["limits"]["last_retry_after_s"].as_u64().unwrap() >= 1, "{v}");
    }
    #[tokio::test]
    async fn supervisor_survives_panic_and_keeps_scheduling() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // 旧实现里后台循环 panic 一次就永久消失（进程还活着、health 还是 ok）。
        // 这里要求：panic 被记一次失败，且任务继续按周期跑下去。
        let h = Arc::new(TaskHealth::new());
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let body = move || {
            let n = n2.clone();
            async move {
                let i = n.fetch_add(1, Ordering::SeqCst);
                if i == 0 {
                    panic!("模拟预热 panic");
                }
                Ok(format!("第 {i} 次"))
            }
        };
        let handle = tokio::spawn(supervise_loop(
            "t_panic",
            h.clone(),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_millis(10),
            body,
        ));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        handle.abort();
        let st = h.stat("t_panic").expect("必须有台账");
        assert_eq!(st.err_runs, 1, "panic 应记一次失败");
        assert!(st.ok_runs >= 2, "panic 之后必须继续调度，实际 ok_runs={}", st.ok_runs);
        assert!(st.last_ok.is_some());
        let snap = h.snapshot();
        assert!(snap["t_panic"]["ok_runs"].as_u64().unwrap() >= 2);
        assert!(snap["t_panic"]["last_ok_s_ago"].as_u64().is_some());
    }

    #[tokio::test]
    async fn supervisor_runs_first_round_immediately_when_asked() {
        // 先睡一整个周期再跑 = 冷启动当天面板一直「尚未成功过」。这里要求 run_now
        // 变体在第一个周期到来前就已经完成一轮。
        let h = Arc::new(TaskHealth::new());
        let handle = tokio::spawn(supervise_loop_now(
            "t_now",
            h.clone(),
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(60),
            || async { Ok::<String, String>("首轮已跑".into()) },
        ));
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        handle.abort();
        let st = h.stat("t_now").expect("必须有台账");
        assert_eq!(st.ok_runs, 1, "启动后应立刻跑一轮，不等 3600 秒");
        assert_eq!(st.last_ok_msg.as_deref(), Some("首轮已跑"));
        assert!(st.last_ok.is_some());
    }

    #[tokio::test]
    async fn supervisor_declares_entry_before_first_run() {
        // 「24 小时才跑一次」的任务在头一天必须已经出现在台账里，否则界面上一片空白，
        // 用户看不出「它只是还没到点」还是「它根本不存在」。
        let h = Arc::new(TaskHealth::new());
        let handle = tokio::spawn(supervise_loop(
            "t_idle",
            h.clone(),
            std::time::Duration::from_secs(3600),
            std::time::Duration::from_secs(60),
            || async { Ok::<String, String>("never".into()) },
        ));
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        handle.abort();
        let st = h.stat("t_idle").expect("任务启动时就该登记");
        assert_eq!(st.ok_runs, 0);
        assert_eq!(st.err_runs, 0);
        assert!(st.last_ok.is_none());
        assert!(h.snapshot()["t_idle"]["last_ok_s_ago"].is_null());
    }

    #[tokio::test]
    async fn supervisor_retries_early_when_body_fails() {
        // period 60ms / retry 5ms：300ms 内若只按 period 只能跑 5 次，
        // 按 retry 才有几十次 —— 用次数差证明「失败后不等一整个周期」。
        let h = Arc::new(TaskHealth::new());
        let handle = tokio::spawn(supervise_loop(
            "t_fail",
            h.clone(),
            std::time::Duration::from_millis(60),
            std::time::Duration::from_millis(5),
            || async { Err::<String, String>("上游 502".to_string()) },
        ));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        handle.abort();
        let st = h.stat("t_fail").expect("必须有台账");
        assert!(st.err_runs >= 10, "应按 retry 快速重试（只按周期最多 5 次），实际 err_runs={}", st.err_runs);
        assert_eq!(st.ok_runs, 0);
        assert_eq!(st.last_err.as_ref().map(|(_, m)| m.as_str()), Some("上游 502"));
        let snap = h.snapshot();
        assert_eq!(snap["t_fail"]["last_err"], "上游 502");
    }

    #[tokio::test]
    async fn health_reports_background_task_activity() {
        let st = state(vec![]);
        st.tasks.mark_ok("warm_models", "模型清单已预热（148 个）");
        st.tasks.mark_err("free_refresh", "上游 502");
        st.tasks.mark_ok("free_refresh", "共 43 站，新增 2，保留 41；聚合页 43 条，丢弃 3（重复 3）");
        let app = build_app(st);
        let body = body_string(app.oneshot(req("/api/health")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let tasks = &v["tasks"];
        assert_eq!(tasks["warm_models"]["ok_runs"], 1);
        // 成功说明必须落在台账里：只看 ok_runs 的话「跑了但一个站都没拿到」看不出异常。
        assert_eq!(tasks["warm_models"]["last_ok"], "模型清单已预热（148 个）");
        assert_eq!(tasks["free_refresh"]["ok_runs"], 1);
        assert_eq!(tasks["free_refresh"]["err_runs"], 1);
        assert_eq!(tasks["free_refresh"]["last_err"], "上游 502");
        assert_eq!(
            tasks["free_refresh"]["last_ok"],
            "共 43 站，新增 2，保留 41；聚合页 43 条，丢弃 3（重复 3）"
        );
        assert!(v["tasks"]["warm_models"]["last_ok_s_ago"].is_u64());
    }

    #[tokio::test]
    async fn health_reports_log_rotation_state() {
        // 日志是否真的在落盘、有没有滚动，必须能从 /api/health 看见：
        // 之前日志只写 stdout，用计划任务启动时一行都没有，出问题只能猜。
        let tmp = tempfile::tempdir().unwrap();
        let dir = aigw_store::rotlog::log_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("aigw.log"), vec![b'x'; 100]).unwrap();
        std::fs::write(dir.join("aigw.log.1"), vec![b'x'; 50]).unwrap();
        // 无关文件不该被算进日志占用
        std::fs::write(dir.join("notes.txt"), vec![b'x'; 999]).unwrap();
        let app = build_app(state(vec![]).with_data_dir(tmp.path().to_path_buf()));
        let body = body_string(app.oneshot(req("/api/health")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let logs = &v["logs"];
        assert_eq!(logs["files"], 2, "body={body}");
        assert_eq!(logs["bytes"], 150, "body={body}");
        assert_eq!(logs["max_bytes"], aigw_store::rotlog::MAX_BYTES);
        assert_eq!(logs["keep"], aigw_store::rotlog::KEEP);
    }

    /// ★ 落盘失败要在 /api/health 里看得见：各 Store 的 save_locked 只 warn 不返回错误，
    /// 面板照样回「已保存」，重启后配置回退而用户全程无感。这条守住那个出口。
    #[tokio::test]
    async fn health_surfaces_the_last_failed_disk_write() {
        // 造一次真实的写盘失败（目标路径被非空目录占住），让 atomic 登记现场。
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("persist-probe-health.json");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("keep"), b"x").unwrap();
        assert!(aigw_store::atomic::write_json_atomic(&bad, &vec![0u8]).is_err());

        let app = build_app(state(vec![]));
        let body = body_string(app.oneshot(req("/api/health")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let pe = &v["persist_error"];
        assert!(!pe.is_null(), "写盘失败后 health 必须露出 persist_error：{body}");
        assert_eq!(pe["file"], json!("persist-probe-health.json"), "{body}");
        assert!(pe["error"].as_str().unwrap_or("").len() > 0, "{body}");
        // 顺手把记录清掉，别把后来的 health 测试带脏
        std::fs::remove_dir_all(&bad).unwrap();
        assert!(aigw_store::atomic::write_json_atomic(&bad, &vec![0u8]).is_ok());
        assert!(aigw_store::atomic::last_write_failure().is_none(), "同名文件写成功后应清空");
    }

    #[tokio::test]
    async fn health_reports_backup_archive_footprint() {
        // 恢复备份/坏档留档会一直堆在数据目录里：这里报数量与占用，
        // 否则「数据目录怎么越来越大」只能靠肉眼数文件。
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::write(d.join(".bak-1792651587138-accounts.json"), b"0123456789").unwrap();
        std::fs::write(d.join("flags.json.bad-1792510359775"), b"01234").unwrap();
        // 真实数据文件与日志不要算进留档占用
        std::fs::write(d.join("accounts.json"), vec![b'x'; 500]).unwrap();
        let app = build_app(state(vec![]).with_data_dir(d.to_path_buf()));
        let body = body_string(app.oneshot(req("/api/health")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let ar = &v["archives"];
        assert_eq!(ar["files"], 2, "body={body}");
        assert_eq!(ar["bytes"], 15, "body={body}");
        assert_eq!(ar["keep_per_family"], aigw_store::archive::KEEP_PER_FAMILY);
    }

    #[tokio::test]
    async fn health_without_data_dir_omits_logs() {
        // 没配数据目录（纯测试/内存态）时不要凭空报一个 0 字节的日志块，
        // 否则「日志是空的」和「根本不知道在哪」分不开。
        let app = build_app(state(vec![]));
        let body = body_string(app.oneshot(req("/api/health")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("logs").is_none(), "body={body}");
        assert!(v.get("archives").is_none(), "body={body}");
        assert!(v.get("db").is_none(), "body={body}");
    }

    #[tokio::test]
    async fn health_reports_ledger_footprint() {
        // 台账占多大、有多少条、是不是 SQLite 后端 —— 「关于」页的数据占用卡片全靠这块。
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::write(d.join("panel.db"), vec![b'x'; 10]).unwrap();
        std::fs::write(d.join("panel.db-wal"), vec![b'y'; 5]).unwrap();
        let st = state(vec![]).with_data_dir(d.to_path_buf());
        st.store.usage_record(aigw_store::UsageEvent {
            ts: 1.0,
            platform: "p".into(),
            model: "m".into(),
            account: "a".into(),
            tin: 1,
            tout: 2,
            ms: 3,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        let app = build_app(st);
        let body = body_string(app.oneshot(req("/api/health")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["db"]["backend"], json!("sqlite"), "body={body}");
        assert_eq!(v["db"]["events"], json!(1), "body={body}");
        assert_eq!(v["db"]["bytes"], json!(15), "-wal 必须算进去：{body}");
    }

    #[tokio::test]
    async fn health_ok() {
        let app = build_app(state(vec![]));
        let res = app.oneshot(req("/api/health")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"ok\":true"), "body={body}");
    }

    #[tokio::test]
    async fn health_self_reports_which_binary_is_running() {
        // 「我部署的新二进制真的生效了吗 / 这次重启是几点」以前只能靠 exe 的 mtime 猜。
        // 假装 90 秒前启动：uptime 必须报 ~90，started_at 必须回推 90 秒。
        let st = state(vec![]).with_started_at(Instant::now() - Duration::from_secs(90));
        let app = build_app(st);
        let res = app.oneshot(req("/api/health")).await.unwrap();
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let b = &v["build"];
        assert_eq!(b["version"], json!(env!("CARGO_PKG_VERSION")), "{body}");
        assert_eq!(b["target"], json!(BUILD_TARGET), "{body}");
        assert_eq!(
            b["profile"],
            json!(if cfg!(debug_assertions) { "debug" } else { "release" }),
            "{body}"
        );
        // ★ 光断言「started_at 是个不太久的过去时刻」是没用的：直接写 now_ms() 也满足
        //   （差 0ms），uptime 恒 0 也满足「< 60」。必须让两个字段互相印证：
        //   由 now - started_at 推出的秒数应当 ≤ uptime_s（+1 秒取整容差）。
        let up = b["uptime_s"].as_u64().expect("uptime_s 必须是数字");
        assert!(
            (89..=92).contains(&up),
            "注入的是 90 秒前启动，uptime_s 却报 {up}"
        );
        let started_ms = b["started_at"].as_u64().expect("started_at 必须是数字");
        let now = now_ms() as u64;
        assert!(started_ms <= now, "started_at 不能在未来：{started_ms} > {now}");
        let derived_s = (now - started_ms) / 1000;
        // 必须双向吻合：只写「derived <= up + 1」的话，把 started_at 直接写成 now_ms()
        // （derived 恒 0）同样能过——恰好放过最可能的写错方式。
        assert!(
            derived_s.abs_diff(up) <= 2,
            "started_at 与 uptime_s 对不上：由 started_at 推出 {derived_s}s，uptime_s 却是 {up}s"
        );
    }

    #[tokio::test]
    async fn health_reports_skipped_specs() {
        // 被跳过的非法 spec 必须在健康检查里长期可见：日志会随重启滚掉，
        // 而「平台静默消失」是真实踩过的坑（anon-zen 少了 models_policy）。
        let st = state(vec![]).with_skipped_specs(vec![(
            "anon-zen.toml".to_string(),
            "toml 解析：missing field `models_policy`".to_string(),
        )]);
        let app = build_app(st);
        let res = app.oneshot(req("/api/health")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("skipped_specs"), "body={body}");
        assert!(body.contains("anon-zen.toml"), "body={body}");
        assert!(body.contains("models_policy"), "body={body}");
        // 仍然 ok=true：一个写错的平台文件不该让整机健康检查变红。
        assert!(body.contains("\"ok\":true"), "body={body}");
    }

    #[tokio::test]
    async fn models_static_contract() {
        // auth=anon：/v1/models 现在会过滤「没有凭据」的平台（api_key 无账号必然 401），
        // 静态契约用免密平台验，避免把「过滤」当成契约回归。
        let spec = spec_toml(
            r#"
id = "demo"
name = "Demo"
enabled = true
auth = "anon"
base_url = "https://example.invalid/v1"
format = "openai"
models = ["m-1", "m-2"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let app = build_app(state(vec![spec]));
        let res = app.oneshot(req("/v1/models")).await.unwrap();
        let body = body_string(res).await;
        assert!(body.contains("\"m-1\""), "body={body}");
        assert!(body.contains("\"owned_by\":\"demo\""), "body={body}");
    }

    #[tokio::test]
    async fn models_live_falls_back_to_static() {
        // base_url 指向不可达端口 → live 拉取失败 → 回退静态清单
        let spec = spec_toml(
            r#"
id = "down"
name = "Down"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["static-fallback"]
models_policy = "live"
stream_dialect = "sse_delta"
"#,
        );
        let app = build_app(state(vec![spec]));
        let res = app.oneshot(req("/v1/models")).await.unwrap();
        let body = body_string(res).await;
        assert!(body.contains("static-fallback"), "body={body}");
    }

    #[tokio::test]
    async fn chat_without_provider_is_503() {
        let app = build_app(state(vec![]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"x","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn usage_endpoint_reflects_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        store.usage_record(UsageEvent {
            ts: aigw_store::now_unix(),
            platform: "t".into(),
            account: "acc-1".to_string(),
            model: "m".into(),
            tin: 3,
            tout: 5,
            ms: 9,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let res = app.oneshot(req("/api/usage")).await.unwrap();
        let body = body_string(res).await;
        assert!(body.contains("\"requests\":1"), "body={body}");
    }

    /// 面板口径：客户端取消要能从 /api/usage 里单独看到，且不计入 failed ——
    /// 否则用户每按一次停止，平台成功率就掉一点，看着像上游不稳。
    #[tokio::test]
    async fn usage_endpoint_splits_client_cancels_from_failures() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let mk = |ok: bool, cancelled: bool| UsageEvent {
            ts: aigw_store::now_unix(),
            platform: "t".into(),
            account: "acc-1".to_string(),
            model: "m".into(),
            tin: 1,
            tout: 1,
            ms: 1,
            ok,
            cancelled,
            upstream: "u".into(),
            kind: String::new(),
        };
        store.usage_record(mk(true, false));
        store.usage_record(mk(false, false));
        store.usage_record(mk(false, true));
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let res = app.oneshot(req("/api/usage")).await.unwrap();
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["requests"], 3, "body={body}");
        assert_eq!(v["success"], 1, "body={body}");
        assert_eq!(v["failed"], 1, "取消不能算失败：{body}");
        assert_eq!(v["cancelled"], 1, "取消要单列：{body}");
        // 请求明细也得带上标记，前端才能把这行画成「已取消」而不是「失败」。
        let app2 = build_app(AppState::new(
            vec![],
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        ));
        let body2 = body_string(app2.oneshot(req("/api/requests")).await.unwrap()).await;
        let v2: Value = serde_json::from_str(&body2).unwrap();
        assert_eq!(v2["recent"].as_array().unwrap().len(), 3, "body={body2}");
        let cancels = v2["recent"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["cancelled"] == true)
            .count();
        assert_eq!(cancels, 1, "取消的那条必须能被识别：{body2}");
    }
    #[tokio::test]
    async fn usage_window_only_counts_events_in_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        store.usage_record(UsageEvent {
            ts: now - 1000.0,
            platform: "a".into(),
            account: "acc-1".to_string(),
            model: "m".into(),
            tin: 1,
            tout: 1,
            ms: 1,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        store.usage_record(UsageEvent {
            ts: now,
            platform: "b".into(),
            account: "acc-1".to_string(),
            model: "m".into(),
            tin: 2,
            tout: 2,
            ms: 2,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        // 窗口命中 now 那条，排除 now-1000
        let res = app.clone()
            .oneshot(req(&format!("/api/usage?from={}&to={}", now - 500.0, now + 500.0)))
            .await
            .unwrap();
        let body = body_string(res).await;
        assert!(body.contains("\"requests\":1"), "body={body}");
        // 窗口完全在更早时间：零
        let res = app.clone()
            .oneshot(req(&format!("/api/usage?from=0&to={}", now - 1100.0)))
            .await
            .unwrap();
        let body = body_string(res).await;
        assert!(body.contains("\"requests\":0"), "body={body}");
        // from > to → 400
        let res = app.clone().oneshot(req("/api/usage?from=100&to=50")).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    /// 时间窗参数的非有限值：`"NaN".parse::<f64>()` 是合法 Ok(NaN)，而 `ts >= NaN`
    /// 永远为假 —— 过去 `/api/usage?from=NaN` 会 200 + 全零台账，看起来像数据丢了。
    /// 现在一律按「没传」处理，返回真实数据；`from > to` 仍然 400。
    #[tokio::test]
    async fn non_finite_window_params_fall_back_instead_of_wiping_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        store.usage_record(UsageEvent {
            ts: now,
            platform: "p".into(),
            account: "a".into(),
            model: "m".into(),
            tin: 1,
            tout: 1,
            ms: 1,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        for q in [
            "/api/usage?from=NaN",
            "/api/usage?to=NaN",
            "/api/usage?from=inf&to=inf",
            "/api/usage?from=-inf",
            "/api/usage?from=NaN&to=NaN&by=model",
        ] {
            let res = app.clone().oneshot(req(q)).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{q}");
            let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
            assert_eq!(
                v["requests"], 1,
                "{q} 应当按「没传时间窗」处理并返回真实台账，而不是全零：{v}"
            );
        }
        // 真·非法窗口仍然 400
        let res = app
            .clone()
            .oneshot(req("/api/usage?from=100&to=50"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        // /api/requests 同口径
        let res = app
            .clone()
            .oneshot(req("/api/requests?from=NaN"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["matched"], 1, "/api/requests 也要按没传处理：{v}");
    }

    #[tokio::test]
    async fn usage_breaks_down_by_model_and_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        let mk = |model: &str, account: &str, ok: bool, tin: i64| UsageEvent {
            ts: now,
            platform: "p".into(),
            account: account.into(),
            model: model.into(),
            tin,
            tout: 1,
            ms: 1,
            ok,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        };
        // 模型 m-a 打了 3 次（2 成 1 败），m-b 1 次成功；账号按成败分开
        store.usage_record(mk("m-a", "acc-1", true, 10));
        store.usage_record(mk("m-a", "acc-1", true, 10));
        store.usage_record(mk("m-a", "acc-2", false, 0));
        store.usage_record(mk("m-b", "acc-1", true, 5));
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let res = app
            .oneshot(req(&format!("/api/usage?from={}&to={}", now - 10.0, now + 10.0)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let models = v["byModel"]["rows"].as_array().unwrap();
        assert_eq!(v["byModel"]["total"], 2, "两个模型：{v}");
        assert_eq!(models[0]["key"], "m-a", "按请求数降序：{v}");
        assert_eq!(models[0]["requests"], 3);
        assert_eq!(models[0]["failed"], 1);
        assert_eq!(models[1]["key"], "m-b");
        let accounts = v["byAccount"]["rows"].as_array().unwrap();
        let acc1 = accounts.iter().find(|r| r["key"] == "acc-1").expect("acc-1 要有分组");
        assert_eq!(acc1["requests"], 3);
        assert_eq!(acc1["failed"], 0);
        let acc2 = accounts.iter().find(|r| r["key"] == "acc-2").expect("acc-2 要有分组");
        assert_eq!(acc2["failed"], 1);
    }

    /// `/api/usage?limit=`：分组视图默认只回前 30 名，但 UI 上写着「共 N 个，只列最多的 30 个」
    /// 却没法点开看全 —— 想找第 40 名只能去翻明细。limit 放开上限 200，非法值直接 400。
    #[tokio::test]
    async fn usage_limit_controls_how_many_groups_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        // 40 个模型各打 1 次：默认 30 名装不下，limit=40 应该全给
        for i in 0..40 {
            store.usage_record(UsageEvent {
                ts: now,
                platform: "p".into(),
                account: "a".into(),
                model: format!("m-{i:02}"),
                tin: 0,
                tout: 0,
                ms: 0,
                ok: true,
                upstream: "u".into(),
                cancelled: false,
                kind: "chat".to_string(),
            });
        }
        let st = AppState::new(
            vec![],
            store.clone(),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let url = format!("/api/usage?from={}&to={}&by=model", now - 10.0, now + 10.0);
        // 不传 limit → 默认 30 条，但 total 要报 40（不能让人以为只有 30 个模型）
        let res = app.clone().oneshot(req(&url)).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["byModel"]["total"], 40, "{v}");
        assert_eq!(v["byModel"]["rows"].as_array().unwrap().len(), 30, "{v}");
        // limit=40 → 40 条全给
        let res = app
            .clone()
            .oneshot(req(&format!("{url}&limit=40")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["byModel"]["rows"].as_array().unwrap().len(), 40, "{v}");
        // limit 超大要夹到 200：台账真到几百个模型时，一次全吐会把响应撑成几 MB。
        for i in 40..250 {
            store.usage_record(UsageEvent {
                ts: now,
                platform: "p".into(),
                account: "a".into(),
                model: format!("x-{i:03}"),
                tin: 0,
                tout: 0,
                ms: 0,
                ok: true,
                upstream: "u".into(),
                cancelled: false,
                kind: "chat".to_string(),
            });
        }
        let res = app
            .clone()
            .oneshot(req(&format!("{url}&limit=999999")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["byModel"]["total"], 250, "{v}");
        assert_eq!(
            v["byModel"]["rows"].as_array().unwrap().len(),
            USAGE_TOP_MAX,
            "limit 再大也只能回 {USAGE_TOP_MAX} 条：{v}"
        );
        // group_limit 是前端分组表用的别名（与明细接口的 limit 分开传），效果相同
        let res = app
            .clone()
            .oneshot(req(&format!("{url}&group_limit=40")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["byModel"]["rows"].as_array().unwrap().len(), 40, "{v}");
        // 非法值必须 400，不能静默当默认值（用户会以为翻页生效了）
        for bad in ["0", "-5", "abc", "1.5"] {
            let res = app
                .clone()
                .oneshot(req(&format!("{url}&limit={bad}")))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "limit={bad} 必须 400");
        }
    }

    /// 明细行必须标明类型：后台任务（签到/成长）也记在这张表里，model 字段写的就是
    /// 动作名。不带 kind，用户会把「checkin」当成一个调不通的模型去排查。
    #[tokio::test]
    async fn requests_rows_carry_their_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        let mk = |model: &str, kind: &str, ts: f64| UsageEvent {
            ts,
            platform: "p".into(),
            account: "a".into(),
            model: model.into(),
            tin: 0,
            tout: 0,
            ms: 0,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: kind.to_string(),
        };
        store.usage_record(mk("real-model", "chat", now - 10.0));
        store.usage_record(mk("checkin", "checkin", now - 1.0));
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let res = app.oneshot(req("/api/requests")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let rows = v["recent"].as_array().unwrap();
        let chat = rows.iter().find(|r| r["model"] == "real-model").expect("要有一条对话行");
        let task = rows.iter().find(|r| r["model"] == "checkin").expect("要有一条签到行");
        assert_eq!(chat["kind"], "chat", "{v}");
        assert_eq!(task["kind"], "checkin", "后台任务必须标成 checkin：{v}");
    }

    /// 端点级回归：后台任务（签到/成长）在台账里是独立的一行，不能顶替真模型占据
    /// 模型榜的名次 —— 实测历史数据里有 17 条 model="checkin"，排到第 3 名。
    #[tokio::test]
    async fn usage_model_breakdown_ignores_background_task_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        let mk = |model: &str, kind: &str| UsageEvent {
            ts: now,
            platform: "p".into(),
            account: "acc-1".into(),
            model: model.into(),
            tin: 0,
            tout: 0,
            ms: 0,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: kind.to_string(),
        };
        store.usage_record(mk("real-model", "chat"));
        store.usage_record(mk("checkin", "checkin"));
        store.usage_record(mk("checkin", "checkin"));
        store.usage_record(mk("checkin", "checkin"));
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let res = app
            .oneshot(req(&format!("/api/usage?from={}&to={}", now - 10.0, now + 10.0)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let models = v["byModel"]["rows"].as_array().unwrap();
        assert_eq!(v["byModel"]["total"], 1, "只有真模型：{v}");
        assert_eq!(models[0]["key"], "real-model", "{v}");
        // 总数与平台维度不受影响（签到确实发生了，只是不算对话）
        assert_eq!(v["requests"], 4, "{v}");
        assert_eq!(v["byPlatform"]["rows"][0]["requests"], 4, "{v}");
    }

    #[tokio::test]
    async fn usage_by_param_selects_breakdowns_and_rejects_unknown() {
        // 旧行为：by 参数根本没人读 —— `?by=bogus` 照样 200 并返回全部三张表，
        // 调用方以为过滤生效了。现在必须是白名单 + 只有被选中的那张。
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        for (plat, model, acc) in [("plat-a", "m-a", "acc-1"), ("plat-b", "m-b", "acc-2")] {
            store.usage_record(UsageEvent {
                ts: now - 5.0,
                platform: plat.into(),
                account: acc.to_string(),
                model: model.into(),
                tin: 1,
                tout: 1,
                ms: 1,
                ok: true,
                upstream: "u".into(),
                cancelled: false,
                kind: "chat".to_string(),
            });
        }
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let fetch = |q: String| {
            let app = app.clone();
            async move {
                let res = app.oneshot(req(&format!("/api/usage{q}"))).await.unwrap();
                let sc = res.status();
                let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
                (sc, v)
            }
        };

        // 不传 = all：三张表都在（含新增的 byPlatform）
        let (sc, v) = fetch(String::new()).await;
        assert_eq!(sc, StatusCode::OK);
        for k in ["byPlatform", "byModel", "byAccount"] {
            assert!(v.get(k).is_some(), "默认要回 {k}：{v}");
        }
        assert_eq!(v["byPlatform"]["total"], 2, "两个平台：{v}");

        // by=model：只要 byModel，别把另外两张也塞回来
        let (sc, v) = fetch("?by=model".to_string()).await;
        assert_eq!(sc, StatusCode::OK);
        assert!(v.get("byModel").is_some(), "by=model 要有 byModel：{v}");
        assert!(v.get("byPlatform").is_none(), "by=model 不该带 byPlatform：{v}");
        assert!(v.get("byAccount").is_none(), "by=model 不该带 byAccount：{v}");
        assert_eq!(v["requests"], 2, "总量统计不受 by 影响：{v}");

        // 大小写/空格容忍
        let (sc, v) = fetch("?by=ACCOUNT%20".to_string()).await;
        assert_eq!(sc, StatusCode::OK, "{v}");
        assert!(v.get("byAccount").is_some() && v.get("byModel").is_none(), "{v}");

        // 未知值 400 且提示可选值（旧行为是静默 200）
        let (sc, v) = fetch("?by=bogus".to_string()).await;
        assert_eq!(sc, StatusCode::BAD_REQUEST, "未知 by 必须报错：{v}");
        let msg = v["message"].as_str().unwrap_or("");
        assert!(msg.contains("platform") && msg.contains("model"), "要点名可选值：{msg}");
    }

    /// ★ 回归：`offset=18446744073709551615`（u64::MAX）必须回 200 空页，
    /// 不能把连接搞崩。根因是 `offset as i64` 有符号饱和成 i64::MAX，SQLite 绑定
    /// 这个值报 SQLITE_TOOBIG，而 store 里 `if let Ok(rows)` 吞掉了错误 —— 于是一个
    /// 字节都没写出去，客户端只看到「Empty reply from server」(curl 52)。
    #[tokio::test]
    async fn absurd_offset_still_answers_with_an_empty_page() {
        let dir = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        for i in 0..3 {
            store.usage_record(UsageEvent {
                ts: now + i as f64,
                platform: "p".into(),
                account: "a".into(),
                model: "m".into(),
                tin: 0,
                tout: 0,
                ms: 0,
                ok: true,
                upstream: "u".into(),
                cancelled: false,
                kind: "chat".to_string(),
            });
        }
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        // store 层：u64::MAX 必须回空页（不是 panic、也不是把 SQLITE_TOOBIG 吞掉）
        let raw = aigw_store::Store::open(dir2.path());
        raw.usage_record(UsageEvent {
            ts: now,
            platform: "p".into(),
            account: "a".into(),
            model: "m".into(),
            tin: 0,
            tout: 0,
            ms: 0,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        assert!(
            raw.usage_window_page(0.0, 1e18, 10, usize::MAX).is_empty(),
            "store 层拿到 u64::MAX offset 必须回空页"
        );
        for o in [
            "18446744073709551615",
            "9223372036854775807",
            "999999999",
        ] {
            let res = app
                .clone()
                .oneshot(req(&format!("/api/requests?offset={o}&limit=2")))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "offset={o} 必须正常响应");
            let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
            assert_eq!(v["shown"], 0, "offset={o} 应该是空页：{v}");
            assert_eq!(v["matched"], 3, "总数不受 offset 影响：{v}");
            assert_eq!(v["has_more"], false);
        }
        // 正常的 offset 仍然要能翻页
        let res = app
            .oneshot(req("/api/requests?offset=2&limit=5"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["shown"], 1, "3 条里跳过 2 条还剩 1 条：{v}");
    }

    #[tokio::test]
    async fn requests_window_honors_from_to() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        store.usage_record(UsageEvent {
            ts: now - 1000.0,
            platform: "a".into(),
            account: "acc-1".to_string(),
            model: "m".into(),
            tin: 1,
            tout: 1,
            ms: 1,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        store.usage_record(UsageEvent {
            ts: now,
            platform: "b".into(),
            account: "acc-1".to_string(),
            model: "m".into(),
            tin: 2,
            tout: 2,
            ms: 2,
            ok: true,
            upstream: "u".into(),
            cancelled: false,
            kind: "chat".to_string(),
        });
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let res = app.clone()
            .oneshot(req(&format!("/api/requests?from={}&to={}", now - 500.0, now + 500.0)))
            .await
            .unwrap();
        let body = body_string(res).await;
        assert!(body.contains("\"recent\":["), "body={body}");
        assert!(body.matches("\"platform\":\"b\"").count() == 1, "body={body}");
        assert!(body.matches("\"platform\":\"a\"").count() == 0, "body={body}");
        let res = app.clone().oneshot(req("/api/requests?from=100&to=50")).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn requests_paging_matches_window_total() {
        // 列表只显示一页、统计是整个窗口：两者必须一起返回，
        // 否则「列表 60 条 / 统计 500 条」会让人以为台账丢了数据。
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let now = aigw_store::now_unix();
        for i in 0..120u32 {
            store.usage_record(UsageEvent {
                ts: now - i as f64,
                platform: format!("p{}", i % 3),
                account: "acc-1".to_string(),
                model: "m".into(),
                tin: 1,
                tout: 1,
                ms: 1,
                ok: i % 2 == 0,
                upstream: "u".into(),
                cancelled: false,
                kind: "chat".to_string(),
            });
        }
        let st = AppState::new(
            vec![],
            store,
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        );
        let app = build_app(st);
        let get = |u: String| {
            let app = app.clone();
            async move {
                let res = app.oneshot(req(&u)).await.unwrap();
                assert_eq!(res.status(), StatusCode::OK, "{u}");
                let body = body_string(res).await;
                serde_json::from_str::<serde_json::Value>(&body).expect("json")
            }
        };

        // 缺省页：60 条，总数是 120，还有下一页
        let v = get("/api/requests".into()).await;
        assert_eq!(v["matched"], 120, "{v}");
        assert_eq!(v["shown"], 60, "{v}");
        assert_eq!(v["limit"], 60, "{v}");
        assert_eq!(v["offset"], 0, "{v}");
        assert_eq!(v["has_more"], true, "{v}");
        assert_eq!(v["recent"].as_array().unwrap().len(), 60, "{v}");

        // 第二页：还剩 60 条，翻完就没有下一页了
        let v = get("/api/requests?limit=60&offset=60".into()).await;
        assert_eq!(v["shown"], 60, "{v}");
        assert_eq!(v["has_more"], false, "{v}");

        // 越过末尾：一条都不给，但仍要说清楚总数，不能装作窗口是空的
        let v = get("/api/requests?offset=500".into()).await;
        assert_eq!(v["shown"], 0, "{v}");
        assert_eq!(v["matched"], 120, "{v}");

        // limit 非法/为 0 → 回落缺省；过大 → 夹到上限（120 条全给）
        let v = get("/api/requests?limit=0".into()).await;
        assert_eq!(v["limit"], 60, "{v}");
        let v = get("/api/requests?limit=abc".into()).await;
        assert_eq!(v["limit"], 60, "{v}");
        let v = get("/api/requests?limit=99999".into()).await;
        assert_eq!(v["limit"], 1000, "{v}");
        assert_eq!(v["shown"], 120, "{v}");


        // 和统计口径对齐：/api/usage 的 requests 就是 matched
        let u = get("/api/usage".into()).await;
        assert_eq!(u["requests"], v["matched"], "usage={u} page={v}");
    }

    // ---------- fake upstream 密闭测试（Patrick/loomy 纪律：不碰真网） ----------

    /// 起 mock 上游，返回其 base（http://127.0.0.1:port）。
    async fn spawn_mock(body: &'static str, content_type: &'static str) -> String {
        use axum::routing::post as post_route;
        let app = Router::new().route(
            "/chat/completions",
            post_route(move || async move {
                ([(header::CONTENT_TYPE, content_type)], body.to_string())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    /// 模型清单 mock：GET /models 回 {"data":[{"id":…},…]}。
    /// 测自定义 API 的「同步模型 / 固定清单」链路用（spawn_mock 只挂了
    /// /chat/completions，拉清单会 404）。
    async fn spawn_models_mock(ids: &'static [&'static str]) -> String {
        use axum::routing::get as get_route;
        let body = format!(
            r#"{{"object":"list","data":[{}]}}"#,
            ids.iter()
                .map(|i| format!(r#"{{"id":"{i}","object":"model"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let app = Router::new().route(
            "/models",
            get_route(move || async move {
                ([(header::CONTENT_TYPE, "application/json")], body.clone())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    /// 只回失败的 /models（模拟上游接口 500 / 网关在启动过程中回半份清单）。
    async fn spawn_status_models_mock(status: axum::http::StatusCode) -> String {
        use axum::routing::get as get_route;
        let app = Router::new().route(
            "/models",
            get_route(move || async move {
                (status, [(header::CONTENT_TYPE, "application/json")], r#"{"error":{"message":"boom"}}"#.to_string())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    /// 有状态的 chat mock：前 fail_times 次回 `status` + fail_body，之后回正常补全。
    /// 用来验证「失败原因落台账 → 成功一次即清除」这条真实网络路径。
    async fn spawn_flaky_chat(
        fail_times: usize,
        status: axum::http::StatusCode,
        fail_body: &'static str,
    ) -> String {
        use axum::routing::post as post_route;
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = Router::new().route(
            "/chat/completions",
            post_route(move || {
                let hits = hits.clone();
                async move {
                    let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < fail_times {
                        (
                            status,
                            [(header::CONTENT_TYPE, "application/json")],
                            fail_body.to_string(),
                        )
                    } else {
                        (
                            axum::http::StatusCode::OK,
                            [(header::CONTENT_TYPE, "application/json")],
                            r#"{"id":"cmpl-1","choices":[{"message":{"role":"assistant","content":"OK"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.to_string(),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }
    /// 慢速 SSE mock：每 gap_ms 吐一帧，共 frames 帧。
    /// 用来制造「客户端在流中途退出」这个真实现场（上游其实一直健康）。
    async fn spawn_slow_sse(frames: usize, gap_ms: u64) -> String {
        use axum::routing::post as post_route;
        let app = Router::new().route(
            "/chat/completions",
            post_route(move || async move {
                let stream = futures_util::stream::unfold(0usize, move |i| async move {
                    if i >= frames {
                        return None;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(gap_ms)).await;
                    let chunk =
                        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"c{i}\"}}}}]}}\n\n");
                    Some((
                        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(chunk)),
                        i + 1,
                    ))
                });
                (
                    axum::http::StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(stream),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    /// 起一个只响应 GET /nav 的 mock 源（校验多源回退的真实网络路径）。
    async fn spawn_nav(status: axum::http::StatusCode, body: &'static str) -> String {
        use axum::routing::get as get_route;
        let app = Router::new().route(
            "/nav",
            get_route(move || async move { (status, body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}/nav")
    }

    /// 有状态的 mock 源：前 fail_times 次返回 500，之后返回合法聚合页。
    /// 用来验证「瞬时抖动 → 重试 → 成功」这条真实网络路径。
    async fn spawn_flaky_nav(fail_times: usize, status: axum::http::StatusCode) -> String {
        use axum::routing::get as get_route;
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = Router::new().route(
            "/nav",
            get_route(move || {
                let hits = hits.clone();
                async move {
                    let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < fail_times {
                        (status, "boom".to_string())
                    } else {
                        (
                            axum::http::StatusCode::OK,
                            r#"<script type="application/ld+json">{"itemListElement":[{"name":"R","url":"https://retry.example.com"}]}</script>"#
                                .to_string(),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}/nav")
    }

    #[tokio::test]
    async fn gongyi_fetch_retries_transient_failures() {
        // 前两次 500、第三次成功：只抓一次的旧实现会把这轮刷新判成失败，
        // 而服务刚启动时恰恰就是这种抖动（实测两个源同时报「请求失败」）。
        let url = spawn_flaky_nav(2, axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        let client = aigw_relay::OutClient::new();
        let (sites, _st) = fetch_one_source(&client, &url)
            .await
            .expect("瞬时失败应被重试救回来");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].base_url, "https://retry.example.com");
    }

    #[tokio::test]
    async fn gongyi_fetch_does_not_retry_404() {
        // 404 重试三次只会让「地址写错」的用户多等，且报错要说明「没重试」。
        let url = spawn_flaky_nav(99, axum::http::StatusCode::NOT_FOUND).await;
        let client = aigw_relay::OutClient::new();
        let err = fetch_one_source(&client, &url).await.expect_err("404 要报错");
        assert!(err.contains("404"), "{err}");
        assert!(!err.contains("已重试"), "404 不该重试：{err}");
    }

    #[tokio::test]
    async fn gongyi_refresh_falls_back_over_real_http() {
        // 首源真返回 500、次源返回合法聚合页：覆盖「HTTP 状态检查 → 回退」这段
        // 纯函数测不到的网络层（旧实现只看第一个源，首个源 500 就整轮失败）。
        let bad = spawn_nav(axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").await;
        let good = spawn_nav(
            axum::http::StatusCode::OK,
            r#"<script type="application/ld+json">{"itemListElement":[{"name":"A","url":"https://a.example.com"}]}</script>"#,
        )
        .await;
        let client = aigw_relay::OutClient::new();
        let (sites, st) = fetch_gongyi_sites(&client, &[bad.clone(), good.clone()])
            .await
            .expect("首个源 500 时应回退到第二个源");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].base_url, "https://a.example.com");
        assert_eq!(st.sources.len(), 2, "两个源都要留痕：{:?}", st.sources);
        assert!(!st.sources[0].ok);
        assert!(st.sources[0].err.contains("500"), "{:?}", st.sources[0]);
        assert!(st.sources[1].ok);
        assert!(st.sources_note().contains("500"), "{}", st.sources_note());

        // 两个源全 500：报错里必须逐个点名（面板上直接能看出是哪两个源）
        let err = fetch_gongyi_sites(&client, &[bad.clone(), bad])
            .await
            .expect_err("全部失败要报错");
        assert!(err.contains("全部 2 个聚合源都失败"), "{err}");
    }

    /// 带延迟的 mock 源：用来把「刷新正在进行中」这个窗口撑开。
    async fn spawn_nav_delayed(body: &'static str, delay_ms: u64) -> String {
        use axum::routing::get as get_route;
        let app = Router::new().route(
            "/nav",
            get_route(move || async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                (axum::http::StatusCode::OK, body)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}/nav")
    }

    #[tokio::test]
    async fn free_refresh_is_single_flight() {
        // 手动刷新与每日任务共用一条抓取链路：撞上时必须明确「上一轮还在进行」，
        // 而不是两个一起打聚合源、一起 upsert、互相覆盖台账。
        let slow = spawn_nav_delayed(
            r#"<script type="application/ld+json">{"itemListElement":[{"name":"S","url":"https://s.example.com"}]}</script>"#,
            400,
        )
        .await;
        let srcs = std::sync::Arc::new(vec![slow]);
        let client = std::sync::Arc::new(aigw_relay::OutClient::new());
        let free = std::sync::Arc::new(aigw_store::FreeStore::empty());
        let no_static: Vec<String> = Vec::new();
        let (c2, f2, s2, h2) = (client.clone(), free.clone(), srcs.clone(), no_static.clone());
        let first = tokio::spawn(async move { refresh_free_sites_guarded(&c2, &f2, &s2, &h2).await });
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let second = refresh_free_sites_guarded(&client, &free, &srcs, &no_static).await.unwrap();
        assert!(matches!(second, FreeRefresh::Busy), "第二轮应被闸门挡住");
        assert!(matches!(first.await.unwrap().unwrap(), FreeRefresh::Done(..)));
        // 闸门真的释放了：跑完再点一次能正常刷新（否则「刷新一次就永久忙」）。
        assert!(matches!(
            refresh_free_sites_guarded(&client, &free, &srcs, &no_static).await.unwrap(),
            FreeRefresh::Done(..)
        ));
    }

    /// 内置公益站 spec（`providers/free-*.toml` 那一批的形态）。
    fn free_spec(id: &str, base: &str, login: &str) -> ProviderSpec {
        spec_toml(&format!(
            r#"
id = "{id}"
name = "{id}"
enabled = true
auth = "api_key"
base_url = "{base}"
format = "openai"
models = []
models_policy = "static"
stream_dialect = "sse_delta"
login_url = "{login}"
"#
        ))
    }

    fn dynamic_site(id: &str, base: &str) -> aigw_store::FreeSite {
        aigw_store::FreeSite {
            id: id.into(),
            name: id.into(),
            base_url: base.into(),
            url: format!("{base}/register?aff=x"),
            desc: String::new(),
            enabled: true,
        }
    }

    #[test]
    fn static_free_hosts_normalizes_and_ignores_other_specs() {
        // 同一个站的两条来路可以写成不同形态：带 www、带路径、base 与 login 分属不同主机。
        // 归一化后必须塌成同一个 host，否则去重永远命不中。
        let specs = vec![
            free_spec("free-dup", "https://www.dup.example", "https://dup.example/register"),
            free_spec("free-other", "https://API.Other.Example/v1", ""),
            spec_toml(r#"
id = "api-notfree"
name = "notfree"
enabled = true
auth = "api_key"
base_url = "https://pay.example"
format = "openai"
models = []
models_policy = "static"
stream_dialect = "sse_delta"
"#),
        ];
        let hosts = static_free_hosts(&specs);
        assert_eq!(hosts, vec!["dup.example".to_string(), "api.other.example".to_string()]);
        assert!(static_free_hosts(&[]).is_empty());
    }

    #[tokio::test]
    async fn free_refresh_skips_sites_already_covered_by_static_providers() {
        // 同一个站同时出现在 providers/free-*.toml 与聚合页时，两条 id 不同
        // （free-dup vs free-dup-example），按 id 去重的 upsert_batch 拦不住 ——
        // 面板上就是两张一模一样的卡片。这里锁死「同域的动态条目不入库」。
        let nav = spawn_nav(
            StatusCode::OK,
            r#"<script type="application/ld+json">{"itemListElement":[                {"name":"Dup","url":"https://dup.example/register?aff=x"},                {"name":"Fresh","url":"https://fresh.example"}]}</script>"#,
        )
        .await;
        let specs = vec![free_spec("free-dup", "https://www.dup.example", "https://dup.example/register")];
        let hosts = static_free_hosts(&specs);
        assert_eq!(hosts, vec!["dup.example".to_string()]);
        let free = aigw_store::FreeStore::empty();
        let client = aigw_relay::OutClient::new();
        let (total, added, kept, stats) = refresh_free_sites(&client, &free, &[nav], &hosts)            .await.unwrap();
        assert_eq!((total, added, kept), (2, 1, 0), "只有 fresh.example 该入库");
        assert_eq!(stats.shadowed, 1, "dup.example 应被记成「与内置接入源同域」");
        assert_eq!(stats.detail(), "聚合页 2 条，无丢弃，另有 1 个与内置接入源同域（已并入内置条目）");
        let ids: Vec<String> = free.rows().into_iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 1, "实际：{ids:?}");
        assert!(ids[0].starts_with("free-fresh"), "实际：{ids:?}");
        // 不传内置 host 时照旧入库（不能把同域去重做成「什么都收不进来」）。
        let nav2 = spawn_nav(
            StatusCode::OK,
            r#"<script type="application/ld+json">{"itemListElement":[                {"name":"Dup","url":"https://dup.example/register?aff=x"},                {"name":"Fresh","url":"https://fresh.example"}]}</script>"#,
        )
        .await;
        let free2 = aigw_store::FreeStore::empty();
        let (_, added2, _, stats2) = refresh_free_sites(&client, &free2, &[nav2], &[])
            .await
            .unwrap();
        assert_eq!(added2, 2, "没有内置条目时两条都该入库");
        assert_eq!(stats2.shadowed, 0);
    }

    #[tokio::test]
    async fn shadowed_free_row_is_hidden_from_free_and_platforms() {
        // 已经躺在 free_sites.json 里的那条重复条目（本次修复之前刷进去的）也要隐藏，
        // 否则修复只对新数据生效、面板上那两张卡片永远留着。
        let specs = vec![free_spec("free-dup", "https://dup.example", "https://dup.example/register")];
        let free = aigw_store::FreeStore::empty();
        free.upsert_batch(vec![dynamic_site("free-dup-example", "https://dup.example")]);
        let st = state_with_free(specs, AccountStore::empty(), free);

        let app = build_app(st.clone());
        let res = app.oneshot(req("/api/free")).await.unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["total"], json!(0), "重复条目不该出现在公益站列表里：{v}");
        assert_eq!(v["shadowed"], json!(1), "但要能解释差额：{v}");

        let app = build_app(st);
        let res = app.oneshot(req("/api/platforms")).await.unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let ids: Vec<&str> = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["free-dup"], "只能剩内置那条：{ids:?}");
    }

    #[tokio::test]
    async fn shadowed_free_row_with_an_account_stays_visible() {
        // 有账号的动态条目不能藏：用户会以为账号丢了，也无从解绑。
        let specs = vec![free_spec("free-dup", "https://dup.example", "https://dup.example/register")];
        let free = aigw_store::FreeStore::empty();
        free.upsert_batch(vec![dynamic_site("free-dup-example", "https://dup.example")]);
        let accs = AccountStore::empty();
        accs.add_account("free-dup-example", "我的号", "api_key", "sk-123456");
        let st = state_with_free(specs, accs, free);
        let app = build_app(st);
        let res = app.oneshot(req("/api/free")).await.unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["total"], json!(1), "有账号就必须留在列表里：{v}");
        assert_eq!(v["shadowed"], json!(0), "实际：{v}");
        assert_eq!(v["rows"][0]["id"], json!("free-dup-example"));
    }

    fn spec_for(base: &str) -> ProviderSpec {
        let raw = format!(
            r#"
id = "mock"
name = "Mock"
enabled = true
auth = "anon"
base_url = "{base}"
format = "openai"
models = []
models_policy = "live"
stream_dialect = "sse_delta"
"#
        );
        toml::from_str(&raw).unwrap()
    }

    /// 声明静态模型的 spec（`models_policy=static`），用于别名路由测试。
    fn spec_static(base: &str, id: &str, models: &[&str]) -> ProviderSpec {
        let list = models
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let raw = format!(
            r#"
id = "{id}"
name = "{id}"
enabled = true
auth = "anon"
base_url = "{base}"
format = "openai"
models = [{list}]
models_policy = "static"
stream_dialect = "sse_delta"
"#
        );
        toml::from_str(&raw).unwrap()
    }

    /// 起一个「把收到的 model 原样回显成正文」的 mock 上游。
    /// 用来断言路由层到底把哪个模型名发给了上游（别名改写是否生效）。
    async fn spawn_model_echo_mock() -> String {
        use axum::routing::post as post_route;
        let app = Router::new().route(
            "/chat/completions",
            post_route(|body: Bytes| async move {
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let m = v
                    .get("model")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let out = json!({
                    "id": "echo-1",
                    "choices": [{"message": {"role": "assistant", "content": m}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                });
                ([(header::CONTENT_TYPE, "application/json")], out.to_string())
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    /// 按 Authorization 分流的上游：`bad-key` → 401，其他 Key → 200。
    /// 换号重试的测试必须让**同一个上游**区分「这个账号坏了」与「平台坏了」，
    /// 否则只能证明「重试了」，证明不了「换到了健康账号」。
    async fn spawn_key_aware_mock() -> String {
        use axum::routing::post as post_route;
        let app = Router::new().route(
            "/chat/completions",
            post_route(|headers: axum::http::HeaderMap| async move {
                let auth = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if auth.contains("bad-key") {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"error": {"message": "invalid api key"}})),
                    )
                        .into_response();
                }
                let out = json!({
                    "id": "ok-1",
                    "choices": [{"message": {"role": "assistant", "content": "from-good"}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                });
                ([(header::CONTENT_TYPE, "application/json")], out.to_string()).into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    /// 上游「HTTP 200 + 首帧就报错」：聚合站/中转站的常见坏法（状态码骗人）。
    async fn spawn_sse_head_error_mock() -> String {
        use axum::routing::post as post_route;
        let app = Router::new().route(
            "/chat/completions",
            post_route(|headers: axum::http::HeaderMap| async move {
                let auth = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if auth.contains("bad-key") {
                    return (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"error\":{\"message\":\"invalid api key\"}}\n\n",
                    )
                        .into_response();
                }
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"from-good\"}}]}\n\ndata: [DONE]\n\n",
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}")
    }

    /// 永远返回同一个状态码并统计被打了多少次：用来证明「调用方写错」不会被换号重试，
    /// 以及换号次数有上限。
    async fn spawn_counting_mock(
        status: StatusCode,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use axum::routing::post as post_route;
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        let app = Router::new().route(
            "/chat/completions",
            post_route(move || {
                let h = h.clone();
                async move {
                    h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (
                        status,
                        Json(json!({"error": {"message": "upstream says no"}})),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}"), hits)
    }

    /// `auth = "api_key"` 的平台才会从账号池取凭据（anon 平台是现造的 `{id}:anon`）。
    fn spec_key(base: &str, id: &str, models: &[&str]) -> ProviderSpec {
        let list = models
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        spec_toml(&format!(
            r#"
id = "{id}"
name = "{id}"
enabled = true
auth = "api_key"
base_url = "{base}"
format = "openai"
models = [{list}]
models_policy = "static"
stream_dialect = "sse_delta"
"#
        ))
    }

    /// 只有「换一个账号真可能成功」的失败才值得换号。
    ///
    /// 连接层失败（status 0：DNS 解析不了 / 连不上 / 超时）与调用方写错（Rejected）
    /// 在池里每个账号上都会得到同一个结果，换号只会把一次失败放大成 N 倍等待与额度消耗。
    #[test]
    fn last_errors_keep_only_the_latest_reason_and_compact_it() {
        let le = LastErrors::new();
        assert_eq!(le.json("p"), Value::Null);
        // 空 id 不登记（没有平台就无处归属）
        le.note("  ", 500, "boom");
        le.note("", 500, "boom");

        // JSON 体里取 message，别把整段 JSON 塞给用户
        le.note("p", 402, r#"{"error":{"message":"quota exceeded: free tier drained"}}"#);
        let j = le.json("p");
        assert_eq!(j["status"], 402);
        assert_eq!(j["text"], "quota exceeded: free tier drained");
        assert!(j["s_ago"].as_u64().unwrap() < 5, "s_ago={}", j["s_ago"]);

        // 后一次覆盖前一次（只留最近）
        le.note("p", 0, "error sending request for url (https://x.example)");
        let j = le.json("p");
        assert_eq!(j["status"], 0);
        assert!(j["text"].as_str().unwrap().contains("sending request"));

        // 多行压成一行（面板一行展示）
        le.note("multi", 500, "boom\n  second line");
        assert_eq!(le.json("multi")["text"], "boom second line");

        // 整页 HTML 要截断，否则会把卡片撑爆
        le.note("long", 500, &"x".repeat(1000));
        let t = le.json("long");
        let txt = t["text"].as_str().unwrap();
        assert_eq!(txt.chars().count(), LAST_ERR_MAX_CHARS + 1);
        assert!(txt.ends_with('…'));

        // 空原因也要有一句人话，不能是空串
        le.note("empty", 500, "   ");
        assert_eq!(le.json("empty")["text"], "（上游没有给出原因）");

        // 成功一次即清除
        le.clear("p");
        assert_eq!(le.json("p"), Value::Null);
        assert_eq!(le.json("multi")["status"], 500, "清一个不能连带清别的");
    }

    #[tokio::test]
    async fn platform_rows_carry_the_original_failure_reason_until_next_success() {
        let base = spawn_flaky_chat(
            1,
            StatusCode::PAYMENT_REQUIRED,
            r#"{"error":{"message":"quota exceeded: free tier drained"}}"#,
        )
        .await;
        let st = state(vec![spec_for(&base)]);
        let app = build_app(st.clone());
        let mk = || {
            Request::post("/v1/chat/completions")
                .header("host", "127.0.0.1:8800")
                .body(Body::from(
                    r#"{"model":"mock-1","messages":[{"role":"user","content":"hi"}]}"#,
                ))
                .unwrap()
        };

        // ① 上游 402：失败原因要落到这个平台的行上，且是上游原话。
        let res = app.clone().oneshot(mk()).await.unwrap();
        assert_eq!(res.status(), StatusCode::PAYMENT_REQUIRED);
        let body = body_string(app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let row = &v["rows"][0];
        assert_eq!(row["id"], "mock");
        assert_eq!(row["last_error"]["status"], 402, "body={body}");
        assert_eq!(
            row["last_error"]["text"],
            "quota exceeded: free tier drained",
            "body={body}"
        );

        // ② 号池因为 402 把这条车道冻住了：解冻后再打一次，这次上游回 200。
        st.pool.thaw("mock:anon");
        let res = app.clone().oneshot(mk()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // ③ 成功一次必须清掉旧原因 —— 否则卡片会一直挂着已经过去的 402。
        let body = body_string(app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["rows"][0]["last_error"].is_null(),
            "成功之后不该再显示失败原因：{body}"
        );
    }
    #[test]
    fn stream_outcomes_feed_the_failure_ledger() {
        let le = LastErrors::new();
        // 流里没有 HTTP 状态码：status 记 0，原因用统计任务带上来的原文
        note_stream_outcome(&le, "p", Some(ErrorClass::Transient), "");
        let j = le.json("p");
        assert_eq!(j["status"], 0);
        assert!(
            j["text"].as_str().unwrap().contains("流式响应中途失败"),
            "{}",
            j["text"]
        );
        // 有原文就用原文（压成一行）
        note_stream_outcome(&le, "p", Some(ErrorClass::Auth), "  invalid api key\n ") ;
        assert_eq!(le.json("p")["text"], "invalid api key");
        // 成功必须清掉
        note_stream_outcome(&le, "p", None, "");
        assert_eq!(le.json("p"), Value::Null);
    }

    #[tokio::test]
    async fn stream_failure_mid_way_records_a_reason_for_the_card() {
        // 首帧正常、随后上游在流里报错：客户端已经拿到 200，失败只有后台统计任务看得到。
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"error\":{\"message\":\"upstream melted mid-stream\"}}\n\n";
        let base = spawn_mock(Box::leak(sse.to_string().into_boxed_str()), "text/event-stream").await;
        let st = state(vec![spec_for(&base)]);
        let app = build_app(st.clone());
        let res = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let _ = body_string(res).await;
        // 记账与失败原因都是 spawn 出来的，轮询等它落定。
        let mut note = Value::Null;
        for _ in 0..50 {
            let body = body_string(
                app.clone()
                    .oneshot(get_with("/api/platforms", &[]))
                    .await
                    .unwrap(),
            )
            .await;
            let v: Value = serde_json::from_str(&body).unwrap();
            note = v["rows"][0]["last_error"].clone();
            if !note.is_null() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!note.is_null(), "流式失败必须留下原因，否则卡片只会显示失败次数");
        assert_eq!(note["status"], 0, "note={note}");
        assert!(
            note["text"].as_str().unwrap().contains("melted"),
            "note={note}"
        );
    }
#[tokio::test]
    async fn client_cancel_mid_stream_is_not_charged_to_the_account() {
        // 用户点「停止」：上游一直健康，错在「下游断开」。旧实现把它按 Transient 罚号池
        // 一次（健康账号被冻），还往卡片上写「平台最近失败」——取消几次就误判成平台挂了。
        let base = spawn_slow_sse(40, 30).await;
        let st = state(vec![spec_for(&base)]);
        let app = build_app(st.clone());
        let res = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // 丢掉 body = 客户端拔线：后台统计任务要等下一个 chunk 才发现下游断了。
        drop(res);
        let mut recent = Value::Null;
        for _ in 0..80 {
            let body = body_string(
                app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap(),
            )
            .await;
            let v: Value = serde_json::from_str(&body).unwrap();
            recent = v["rows"][0]["recent"].clone();
            if recent["requests"].as_i64().unwrap_or(0) >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // 台账仍记一次「没成功」（上游确实没跑完，记成功会让成功率虚高），
        // 但要标成 cancelled 而不是 failed —— 否则「近 24h 失败率」把用户
        // 自己按的停止也算成平台挂了的证据。
        assert_eq!(recent["requests"], 1, "recent={recent}");
        assert_eq!(recent["failed"], 0, "按停止不能算平台失败：{recent}");
        assert_eq!(recent["cancelled"], 1, "recent={recent}");
        let body = body_string(
            app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap(),
        )
        .await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["rows"][0]["last_error"].is_null(),
            "客户端自己取消不该被写成「平台最近失败」：{}",
            v["rows"][0]["last_error"]
        );
        let body = body_string(
            app.clone().oneshot(get_with("/api/pool", &[])).await.unwrap(),
        )
        .await;
        let p: Value = serde_json::from_str(&body).unwrap();
        assert!(
            p["cooling"].as_array().map(|a| a.is_empty()).unwrap_or(true),
            "用户点停止不该给账号记冷却：{p}"
        );
        assert!(
            p["waf"].as_array().map(|a| a.is_empty()).unwrap_or(true),
            "用户点停止不该动 WAF 限速：{p}"
        );
    }
    #[tokio::test]
    async fn pool_thaw_clears_a_lane_lock_and_refuses_noops() {
        let st = state_with_custom(CustomStore::empty(), AccountStore::empty());
        // 免密车道被上游 403 之后记的是 `{平台}:anon` 这个车道键。
        st.pool
            .report("anon-zen", "anon-zen:anon", "flint-40b-free", false, ErrorClass::LaneLocked, false);
        assert!(!st.pool.blocked_of("anon-zen:anon").is_clear(), "种子没生效");
        let app = build_app(st.clone());

        let thaw = |acct: &str| {
            Request::post("/api/pool")
                .header("host", "127.0.0.1:8800")
                .body(Body::from(format!(r#"{{"action":"thaw","account":"{acct}"}}"#)))
                .unwrap()
        };

        let res = app.clone().oneshot(thaw("anon-zen:anon")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let b = body_string(res).await;
        assert!(b.contains("已解冻"), "body={b}");
        assert!(b.contains("1 条模型冷却已清除"), "body={b}");
        assert!(
            st.pool.blocked_of("anon-zen:anon").is_clear(),
            "解冻后 /api/pool 必须看不到这条冷却"
        );

        // 再解冻一次 = 没有可解的东西，要明确告知而不是假装成功。
        let res = app.clone().oneshot(thaw("anon-zen:anon")).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert!(body_string(res).await.contains("不需要解冻"));

        // 缺 account / 未知 action 都是客户端错误。
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/pool")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"action":"thaw"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = app
            .oneshot(
                Request::post("/api/pool")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"action":"nuke"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn pool_snapshot_exposes_sticky_table_view() {
        let app = build_app(state(vec![spec_for("http://127.0.0.1:1")]));
        let res = app.oneshot(req("/api/pool")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"sticky\""), "body={body}");
        assert!(body.contains("sticky_max_entries"), "body={body}");
        assert!(body.contains("ttl_s"), "body={body}");
    }

    /// 粘性表上限 8192 条、每条带完整会话键：诊断接口全量返回就是几百 KB。
    /// 默认必须截断并给出总数，`?sticky=full` 才全量。
    #[tokio::test]
    async fn pool_snapshot_bounds_sticky_entries_and_reports_total() {
        let st = state(vec![spec_for("http://127.0.0.1:1")]);
        for i in 0..60 {
            st.sticky.bind(&format!("conv-{i}"), "acc-1");
        }
        let app = build_app(st);

        let res = app
            .clone()
            .oneshot(req("/api/pool"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["sticky"]["total"], 60, "总数要如实报：{v}");
        assert_eq!(
            v["sticky"]["entries"].as_array().unwrap().len(),
            50,
            "默认只回前 50 条：{v}"
        );
        assert_eq!(v["sticky"]["truncated"], json!(true), "{v}");
        assert_eq!(v["sticky_entries"], 60, "顶层也要给占用数：{v}");

        let res = app.oneshot(req("/api/pool?sticky=full")).await.unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(
            v["sticky"]["entries"].as_array().unwrap().len(),
            60,
            "?sticky=full 要全量：{v}"
        );
        assert_eq!(v["sticky"]["truncated"], json!(false), "{v}");
    }

    #[test]
    fn session_key_is_fixed_length_even_for_huge_client_ids() {
        // 会话键进的是粘性表的 HashMap 键：长度必须与客户端发什么无关。
        let huge = "x".repeat(1 << 20);
        let k = session_key(&json!({"metadata": {"conversation_id": huge}}));
        assert_eq!(k.len(), 17, "会话键必须定长，实际 {} 字节：{k}", k.len());
        assert!(k.starts_with('k'));
        // 同一会话稳定（粘性不能因为折哈希而失效）
        let same = session_key(&json!({"metadata": {"conversation_id": "x".repeat(1 << 20)}}));
        assert_eq!(same, k);
        // 不同会话不撞
        assert_ne!(session_key(&json!({"conversation_id": "conv-a"})), session_key(&json!({"conversation_id": "conv-b"})));
        // 没有任何标识 → 定长 anon
        assert_eq!(session_key(&json!({"messages": []})), "anon");
    }

    #[test]
    fn failover_worthy_only_retries_account_scoped_failures() {
        assert!(failover_worthy(401, ErrorClass::Auth));
        assert!(failover_worthy(403, ErrorClass::Waf));
        assert!(failover_worthy(429, ErrorClass::Rate));
        assert!(failover_worthy(402, ErrorClass::Quota));
        assert!(failover_worthy(502, ErrorClass::Transient));
        assert!(!failover_worthy(0, ErrorClass::Transient));
        assert!(!failover_worthy(404, ErrorClass::Rejected));
        assert!(!failover_worthy(400, ErrorClass::Rejected));
        assert!(!failover_worthy(403, ErrorClass::LaneLocked));
        assert!(!failover_worthy(403, ErrorClass::Region));
    }

    #[tokio::test]
    async fn bad_account_fails_over_to_healthy_one() {
        let base = spawn_key_aware_mock().await;
        let accs = AccountStore::empty();
        accs.add_account("mock", "bad", "api_key", "bad-key");
        accs.add_account("mock", "good", "api_key", "good-key");
        let app = build_app(state_with_accounts(
            vec![spec_key(&base, "mock", &["m-1"])],
            accs,
        ));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "第一个账号 401 之后必须换到健康账号，而不是把 401 直接甩给客户端"
        );
        let body = body_string(res).await;
        assert!(body.contains("from-good"), "body={body}");
    }

    #[tokio::test]
    async fn stream_head_error_is_not_a_silent_200() {
        let base = spawn_sse_head_error_mock().await;
        let accs = AccountStore::empty();
        accs.add_account("mock", "bad", "api_key", "bad-key");
        let app = build_app(state_with_accounts(vec![spec_key(&base, "mock", &["m-1"])], accs));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"m-1","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "上游 200 但首帧报鉴权错：必须变成一个真实错误码，而不是 200 + 客户端解析不了的内容"
        );
        let body = body_string(res).await;
        assert!(body.contains("invalid api key"), "上游真实原因要透出来：body={body}");
        assert!(!body.contains("data:"), "错误帧不能被当成正常流转发：body={body}");
    }

    /// ★ 回归：上游 200 之后一直不给内容（连接打开但空转）。旧实现把超时写在循环里的
    /// timeout(30s, s.next()) 上，等于每次读取都重新给 30 秒，慢速上游可以把「未提交」的
    /// 状态无限拖下去，客户端只能干挂着 200。这里用毫秒级预算直接验证总预算语义。
    #[tokio::test]
    async fn sniff_gives_up_when_upstream_never_sends_a_head() {
        // 永远不吐数据、也不结束的流（真实场景=上游接受连接后挂死）。
        let stuck = futures_util::stream::pending::<Result<Bytes, std::io::Error>>();
        let (mut out, verdict) = sniff_stream_head_within(
            Box::pin(stuck),
            Duration::from_millis(80),
            Duration::from_millis(200),
        )
        .await;
        use futures_util::StreamExt;
        // 判定必须是失败，而且不能把「空转」说成正常结束。
        let (msg, class, status) = verdict.expect("空转的上游必须被判定为失败，不能放行");
        assert_eq!(class, ErrorClass::Transient, "{msg}");
        assert_eq!(status, 504, "网关侧判定出来的超时用 504：{msg}");
        assert!(msg.contains("没有发出任何流式内容"), "{msg}");
        // 流本身仍可读（读出来就是空的/或直接结束），不会 hang 住调用方。
        let _ = tokio::time::timeout(Duration::from_millis(300), out.next()).await;
    }

    /// 慢速上游：一直挤不完整帧。总预算用光后按「凑不出首帧」判定，而不是无限续命。
    #[tokio::test]
    async fn sniff_gives_up_when_upstream_only_dribbles_incomplete_frames() {
        // 每 10ms 吐一个不完整的 SSE 片段（永远凑不出带 choices 的完整帧）。
        // 无限长的滴流：永不到 eof，只有总预算能终止它（若预算被每次读取重置，
        // 这个测试就会一直挂着——那正是我们要防的线上行为）。
        //
        // ★ 间隔(10ms) 必须远小于预算(200ms)：两者接近时（原本 30ms/150ms）最后一次
        //   tick 只剩几毫秒余量，全量测试并行跑起来会被调度抖动推过线，于是报出
        //   「150 毫秒无新数据」而不是「凑不出完整帧」——测试变脆，且掩盖真问题。
        let dribble = futures_util::stream::unfold(0usize, |i| async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some((
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: {\"choi")),
                i.wrapping_add(1),
            ))
        });
        let (mut out, verdict) = sniff_stream_head_within(
            Box::pin(dribble),
            Duration::from_millis(80),
            Duration::from_millis(200),
        )
        .await;
        use futures_util::StreamExt;
        let (msg, class, status) = verdict.expect("只挤碎片的上游不能被放行");
        assert_eq!(class, ErrorClass::Transient, "{msg}");
        assert_eq!(status, 504, "{msg}");
        assert!(msg.contains("凑不出一个完整的首帧"), "{msg}");
        let _ = tokio::time::timeout(Duration::from_millis(300), out.next()).await;
    }

    /// 正常快流必须照旧放行（别把新预算写成误杀健康上游）。
    #[tokio::test]
    async fn sniff_passes_a_normal_first_frame() {
        let ok = futures_util::stream::once(async {
            Ok::<Bytes, std::io::Error>(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            ))
        });
        let (mut out, verdict) =
            sniff_stream_head_within(Box::pin(ok), Duration::from_millis(500), Duration::from_millis(900))
                .await;
        assert!(verdict.is_none(), "正常首帧不该被判失败：{verdict:?}");
        use futures_util::StreamExt;
        let chunk = tokio::time::timeout(Duration::from_millis(300), out.next())
            .await
            .ok()
            .flatten()
            .expect("已读字节必须原样接回流首")
            .expect("流本身不该出错");
        let text = String::from_utf8_lossy(&chunk);
        assert!(text.contains("choices"), "回流的首字节被改了：{text}");
    }

    #[tokio::test]
    async fn stream_head_error_fails_over_to_healthy_account() {
        let base = spawn_sse_head_error_mock().await;
        let accs = AccountStore::empty();
        accs.add_account("mock", "bad", "api_key", "bad-key");
        accs.add_account("mock", "good", "api_key", "good-key");
        let app = build_app(state_with_accounts(vec![spec_key(&base, "mock", &["m-1"])], accs));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"m-1","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "首帧报错的账号要换掉，不能让客户端吃一段错误流"
        );
        let body = body_string(res).await;
        assert!(body.contains("from-good"), "body={body}");
    }
    #[tokio::test]
    async fn rejected_request_is_not_retried_across_accounts() {
        let (base, hits) = spawn_counting_mock(StatusCode::BAD_REQUEST).await;
        let accs = AccountStore::empty();
        accs.add_account("mock", "a1", "api_key", "k1");
        accs.add_account("mock", "a2", "api_key", "k2");
        let app = build_app(state_with_accounts(
            vec![spec_key(&base, "mock", &["m-1"])],
            accs,
        ));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "400 是请求本身写错，换账号重试只会白烧额度并放大延迟"
        );
    }

    #[tokio::test]
    async fn failover_stops_after_three_accounts() {
        // 换号不是免费的：每个账号都要多一次上游往返。池里 5 个号全坏时必须停在 3 次，
        // 否则一个全池失效的平台会把客户端超时时间全部耗在重试上。
        let (base, hits) = spawn_counting_mock(StatusCode::UNAUTHORIZED).await;
        let accs = AccountStore::empty();
        for i in 0..5 {
            accs.add_account("mock", &format!("a{i}"), "api_key", &format!("k{i}"));
        }
        let app = build_app(state_with_accounts(
            vec![spec_key(&base, "mock", &["m-1"])],
            accs,
        ));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"m-1","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "全池失效时必须停在上限 3 次尝试"
        );
    }

    #[test]
    fn norm_model_id_folds_official_date_and_dot_forms() {
        // 官方带日期全名 → 去日期
        assert_eq!(
            norm_model_id("claude-sonnet-4-5-20250929"),
            "claude-sonnet-4-5"
        );
        assert_eq!(norm_model_id("gpt-4o-2024-08-06"), "gpt-4o");
        // 点号版本 → 连字符版本
        assert_eq!(norm_model_id("claude-sonnet-4.5"), "claude-sonnet-4-5");
        assert_eq!(norm_model_id("Qwen3.7-Max"), "qwen3-7-max");
        // -latest 尾巴
        assert_eq!(norm_model_id("openrouter/auto-latest"), "openrouter/auto");
        // 别名折叠必须双向等价，且不能把不同模型混为一谈
        assert!(same_model("claude-sonnet-4-5-20250929", "claude-sonnet-4.5"));
        assert!(same_model("gpt-4o", "gpt-4o-2024-08-06"));
        assert!(!same_model("gpt-4", "gpt-4o"));
        assert!(!same_model("gpt-4o-mini", "gpt-4o"));
    }

    // ---------- 上游失败的可行动建议 ----------

    fn hint_spec(region: &str) -> ProviderSpec {
        let extra = if region.is_empty() {
            String::new()
        } else {
            format!("region = \"{region}\"\n")
        };
        spec_toml(&format!(
            r#"
id = "hint-demo"
name = "HintDemo"
enabled = true
{extra}auth = "api_key"
base_url = "https://example.invalid/v1"
format = "openai"
models = ["m-1"]
models_policy = "static"
stream_dialect = "sse_delta"
"#
        ))
    }

    #[test]
    fn upstream_hint_distinguishes_proxy_from_credentials() {
        // 海外平台连不上 → 必须指向代理设置，而不是让人去重新登录
        let intl = hint_spec("intl");
        let h = upstream_hint(
            &intl,
            502,
            Some(ErrorClass::Transient),
            "error sending request for url (https://api.kimi.ai/coding/v1)",
        )
        .unwrap();
        assert!(h.contains("代理"), "hint={h}");
        assert!(h.contains("国际站"), "hint={h}");
        assert!(!h.contains("重新登录"), "连不上时不该让人重登：{h}");

        // 国内平台 401 + 区域信息 → 既要重登，也要提醒两区账号不通用
        let cn = hint_spec("cn");
        let h = upstream_hint(&cn, 401, Some(ErrorClass::Auth), r#"{"code":"TOKEN_EXPIRE"}"#).unwrap();
        assert!(h.contains("重新登录"), "hint={h}");
        assert!(h.contains("国内站"), "hint={h}");
        assert!(h.contains("国内站") && h.contains("互不通用"), "hint={h}");

        // 全球平台额度耗尽 → 建议加账号/换免费车道，不提区域
        let global = hint_spec("");
        let h = upstream_hint(&global, 402, Some(ErrorClass::Quota), "quota exceeded").unwrap();
        assert!(h.contains("额度"), "hint={h}");
        assert!(!h.contains("互不通用"), "hint={h}");

        // 认不出的失败不硬编建议（宁可只说上游原文）
        assert!(upstream_hint(&global, 418, None, "i am a teapot").is_none());
    }

    #[tokio::test]
    async fn upstream_error_response_carries_hint_and_keeps_upstream_fields() {
        let res = upstream_error_response(
            401,
            r#"{"error":{"message":"invalid token","type":"auth"}}"#,
            Some("到「接入源 → X」重新登录"),
        );
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let body = body_string(res).await;
        assert!(body.contains("建议：到「接入源"), "body={body}");
        assert!(body.contains(r#""hint":"到「接入源 → X」重新登录""#), "body={body}");
        // 上游自己的 type 不能被建议覆盖掉（客户端靠它判类型）
        assert!(body.contains(r#""type":"auth""#), "body={body}");
    }

    #[tokio::test]
    async fn chat_nonstream_via_mock_upstream() {
        let mock_resp = r#"{"id":"cmpl-1","choices":[{"message":{"role":"assistant","content":"OK"}}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let app = build_app(state(vec![spec_for(&base)]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"OK\""), "body={body}");
    }

    #[tokio::test]
    async fn chat_rewrites_dated_alias_to_declared_model() {
        // 真实痛点：Claude Code / 各种 SDK 发的是官方带日期全名
        // `claude-sonnet-4-5-20250929`，而 sub-kiro 这类平台声明的是
        // `claude-sonnet-4.5`。旧实现字符串相等比较 → 404 model_not_found。
        let base = spawn_model_echo_mock().await;
        let app = build_app(state(vec![spec_static(
            &base,
            "mock",
            &["claude-sonnet-4.5"],
        )]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"claude-sonnet-4-5-20250929","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        // 上游必须收到平台声明的写法，而不是客户端那句带日期的全名
        assert!(body.contains("\"content\":\"claude-sonnet-4.5\""), "body={body}");
    }

    #[tokio::test]
    async fn chat_rewrites_dash_alias_to_dotted_model() {
        // 反方向：客户端写 `claude-sonnet-4-5`，平台声明 `claude-sonnet-4.5`。
        let base = spawn_model_echo_mock().await;
        let app = build_app(state(vec![spec_static(
            &base,
            "mock",
            &["claude-sonnet-4.5"],
        )]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"content\":\"claude-sonnet-4.5\""), "body={body}");
    }

    #[tokio::test]
    async fn chat_alias_does_not_fuzzy_match_unrelated_models() {
        // 别名折叠只认「同一模型的另一种写法」，不能变成模糊匹配：
        // `gpt-4o-mini` 不能被当成 `gpt-4o`。
        let base = spawn_model_echo_mock().await;
        let app = build_app(state(vec![
            spec_static(&base, "mock-a", &["gpt-4o"]),
            spec_static(&base, "mock-b", &["claude-sonnet-4.5"]),
        ]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"gpt-4o-mini-2024-07-18","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_string(res).await;
        assert!(body.contains("model_not_found"), "body={body}");
    }

    #[tokio::test]
    async fn chat_keeps_exact_declared_model_untouched() {
        // 精确命中的模型名必须原样发给上游（不能被归一化改写成别的写法）。
        let base = spawn_model_echo_mock().await;
        let app = build_app(state(vec![spec_static(
            &base,
            "mock",
            &["Qwen3.7-Max"],
        )]));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"Qwen3.7-Max","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"content\":\"Qwen3.7-Max\""), "body={body}");
    }

    #[tokio::test]
    async fn chat_stream_via_mock_upstream() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
        let base = spawn_mock(Box::leak(sse.to_string().into_boxed_str()), "text/event-stream").await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let app = build_app(AppState::new(
            vec![spec_for(&base)],
            store.clone(),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        ));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"mock-1","stream":true,"messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"), "ct={ct}");
        // ★ 流式响应头三件套：中间层不许缓冲、不许变换、连接保持。
        //   少了 X-Accel-Buffering，nginx/CF 会把 SSE 攒满 buffer 一次喷出，
        //   用户看到的就是「首帧预算明明 30 秒，却 40 秒后才一次性出全部内容」。
        let hdr = |k: header::HeaderName| {
            res.headers()
                .get(k)
                .map(|v| v.to_str().unwrap_or("").to_string())
                .unwrap_or_default()
        };
        assert_eq!(hdr(header::CACHE_CONTROL), "no-cache, no-transform");
        assert_eq!(hdr("X-Accel-Buffering".parse().unwrap()), "no");
        assert_eq!(hdr(header::CONNECTION), "keep-alive");
        let body = body_string(res).await;
        assert!(body.contains("Hi"), "透传应保留原始 SSE：{body}");
        assert!(body.contains("[DONE]"), "body={body}");
        // 记账是 spawn 的，稍等一拍
        for _ in 0..20 {
            if store.usage_totals()["requests"].as_u64().unwrap_or(0) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let totals = store.usage_totals();
        assert_eq!(totals["requests"], 1);
        assert_eq!(totals["tokensIn"], 4);
        assert_eq!(totals["tokensOut"], 2);
    }

    // ---------- glm adapter 密闭测试（refresh + stream 全 mock） ----------

    fn glm_spec_for(base: &str) -> ProviderSpec {
        let raw = format!(
            r#"
id = "web-glm"
name = "GLM Mock"
enabled = true
auth = "cookie"
base_url = "{base}"
format = "openai"
models = ["glm-4-flash"]
models_policy = "static"
stream_dialect = "event_machine"
upstream_kind = "chatglm"
signer = "glm"
biz_err = ["登录", "失效"]
"#
        );
        toml::from_str(&raw).unwrap()
    }

    #[tokio::test]
    async fn glm_nonstream_via_mock_upstream() {
        use axum::routing::post as post_route;
        let app = Router::new()
            .route(
                "/user-api/user/refresh",
                post_route(|| async { Json(json!({"data": {"access_token": "at-test"}})) }),
            )
            .route(
                "/backend-api/assistant/stream",
                post_route(|| async {
                    ([(header::CONTENT_TYPE, "text/event-stream")],
                     "data: {\"parts\":[{\"content\":\"你好\"}]}\n\ndata: {\"parts\":[{\"content\":\"你好世界\"}]}\n\n")
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let accounts = AccountStore::from_rows(vec![aigw_store::Account {
            id: "g1".into(),
            platform: "web-glm".into(),
            name: "t".into(),
            secret: "rt-test".into(),
            kind: "cookie".into(),
            enabled: true,
            status: String::new(),
            unreadable: false,
            domain: String::new(),
        }]);
        let app = build_app(state_with_accounts(vec![glm_spec_for(&format!("http://{addr}"))], accounts));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"glm-4-flash","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("你好世界"), "聚合最后快照：{body}");
        assert!(body.contains("chat.completion"), "OpenAI 形态：{body}");
    }

    // ---------- 门禁（Host/Origin/令牌）密闭测试 ----------

    fn get_with(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut b = Request::get(uri);
        // 默认补回环 Host（真实客户端必有），除非用例自己要测 Host 白名单。
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            b = b.header("host", "127.0.0.1:8800");
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn guard_allows_loopback_origin() {
        let app = build_app(state(vec![]));
        let res = app
            .oneshot(get_with(
                "/api/health",
                &[("origin", "http://127.0.0.1:8800"), ("host", "127.0.0.1:8800")],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn guard_rejects_cross_site_origin() {
        let app = build_app(state(vec![]));
        let res = app
            .oneshot(get_with(
                "/api/accounts",
                &[("origin", "https://evil.example")],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn guard_requires_host_on_management_plane() {
        let app = build_app(state(vec![]));
        // 管理面：不带 Host 头一律 403（旧实现会静默跳过整条 Host 白名单）
        let res = app.clone().oneshot(raw_req("/api/accounts")).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        // 协议面：不带 Host 的 HTTP/1.0 裸客户端仍可用，不能因为门禁改动挡死 SDK
        let res = app.oneshot(raw_req("/v1/models")).await.unwrap();
        assert_ne!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn guard_rejects_dns_rebinding_host() {
        let app = build_app(state(vec![]));
        let res = app
            .oneshot(get_with("/api/accounts", &[("host", "evil.example:8800")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn token_required_when_configured() {
        let st = state(vec![]).with_security(Some("s3cret".into()), loopback_hosts());
        let app = build_app(st);
        // 无令牌 → 401
        let res = app.clone().oneshot(req("/api/accounts")).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // 错误令牌 → 401
        let res = app
            .clone()
            .oneshot(get_with("/api/accounts", &[("x-aigw-token", "wrong")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // 正确令牌 → 200
        let res = app
            .oneshot(get_with("/api/accounts", &[("x-aigw-token", "s3cret")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    fn post_with(uri: &str, body: serde_json::Value, headers: &[(&str, &str)]) -> Request<Body> {
        let mut b = Request::post(uri).header("host", "127.0.0.1:8800");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn token_generate_rotates_immediately_and_persists() {
        // 轮换要「立刻生效」：旧令牌下一次请求就该 401，新令牌立刻可用。
        // 同时必须落盘 —— 否则重启后回到旧令牌，等于用户以为换了锁其实没换。
        let tmp = tempfile::tempdir().unwrap();
        let st = state(vec![])
            .with_security(Some("old-token-000000".into()), loopback_hosts())
            .with_token_source("cli")
            .with_data_dir(tmp.path().to_path_buf());
        let app = build_app(st);

        let res = app
            .clone()
            .oneshot(get_with("/api/accounts", &[("x-aigw-token", "old-token-000000")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "旧令牌初始可用");

        let res = app
            .clone()
            .oneshot(post_with(
                "/api/token",
                json!({"action": "generate"}),
                &[("x-aigw-token", "old-token-000000")],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(res).await).unwrap();
        let new_tok = v["token"].as_str().expect("生成的令牌必须回显一次").to_string();
        assert!(new_tok.len() >= 32, "令牌强度不能低于启动时自动生成的：{new_tok}");
        assert_eq!(v["persisted"], json!(true), "必须落盘：{v}");
        // 落盘是 DPAPI 密文（purpose=panel）：文件里不能出现令牌明文，但要能解回同一个令牌。
        let on_disk = std::fs::read_to_string(tmp.path().join("panel-token.txt"))
            .unwrap()
            .trim()
            .to_string();
        assert!(
            !on_disk.contains(&new_tok),
            "panel-token.txt 不能存明文令牌：{on_disk}"
        );
        assert_eq!(
            aigw_store::secret::unwrap_with(&on_disk, "panel").unwrap(),
            new_tok,
            "落盘密文要能解回回显的令牌"
        );

        let res = app
            .clone()
            .oneshot(get_with("/api/accounts", &[("x-aigw-token", "old-token-000000")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "旧令牌必须立刻失效");

        let res = app
            .clone()
            .oneshot(get_with("/api/accounts", &[("x-aigw-token", &new_tok)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "新令牌立刻可用");

        let res = app
            .oneshot(get_with("/api/health", &[("x-aigw-token", &new_tok)]))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["token_source"], json!("panel"), "来源要变成 panel");
        assert!(!serde_json::to_string(&v).unwrap().contains(&new_tok), "health 不得回显令牌");
    }

    #[tokio::test]
    async fn token_generate_with_explicit_token_is_rejected_not_silently_ignored() {
        // 旧行为：{"action":"generate","token":"my-fixed"} → 200 但返回随机令牌，
        // 用户以为设上了固定令牌，下次请求却 401。现在必须 400 并指向 action=set。
        let tmp = tempfile::tempdir().unwrap();
        let st = state(vec![])
            .with_security(Some("cur-token-000000".into()), loopback_hosts())
            .with_data_dir(tmp.path().to_path_buf());
        let app = build_app(st);
        let h = [("x-aigw-token", "cur-token-000000")];

        for bad in [
            json!({"action":"generate","token":"my-own-token-123456"}),
            json!({"action":"generate","token":"   "}),
            json!({"action":"generate","token":123}),
        ] {
            let res = app
                .clone()
                .oneshot(post_with("/api/token", bad.clone(), &h))
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::BAD_REQUEST,
                "带 token 的 generate 不能静默忽略：{bad}"
            );
            let v: serde_json::Value = serde_json::from_str(&body_string(res).await).unwrap();
            assert!(
                v["message"].as_str().unwrap_or("").contains("action=set"),
                "错误文案要指向 action=set：{v}"
            );
        }

        // 被拒绝 = 什么都没发生：旧令牌仍然有效（不能被顺手轮换掉）
        let res = app.clone().oneshot(get_with("/api/accounts", &h)).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "被拒绝的 generate 不能顺手换令牌");
        assert!(!tmp.path().join("panel-token.txt").exists(), "被拒绝时不落盘");

        // 空串视为「没指定」（很多客户端把未填字段序列化成 ""），仍走随机生成
        let res = app
            .oneshot(post_with(
                "/api/token",
                json!({"action":"generate","token":""}),
                &h,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "空串 token 不该被拒：{res:?}");
    }

    #[tokio::test]
    async fn token_set_validates_and_clear_returns_to_no_token() {
        let tmp = tempfile::tempdir().unwrap();
        let st = state(vec![])
            .with_security(Some("cur-token-000000".into()), loopback_hosts())
            .with_data_dir(tmp.path().to_path_buf());
        let app = build_app(st);
        let h = [("x-aigw-token", "cur-token-000000")];

        // 太短 / 含空白 / 缺字段 一律 400：弱令牌等于没设令牌。
        for bad in [json!({"action":"set","token":"short"}), json!({"action":"set","token":"has space in it 12345"}), json!({"action":"set"})] {
            let res = app.clone().oneshot(post_with("/api/token", bad.clone(), &h)).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "应拒绝：{bad}");
        }

        let res = app
            .clone()
            .oneshot(post_with("/api/token", json!({"action":"set","token":"my-own-token-123456"}), &h))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(post_with("/api/token", json!({"action":"clear"}), &[("x-aigw-token", "my-own-token-123456")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!tmp.path().join("panel-token.txt").exists(), "clear 要删掉令牌文件");

        let res = app
            .clone()
            .oneshot(get_with("/api/health", &[]))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["token_required"], json!(false), "clear 后回到本机免令牌");
        assert_eq!(v["token_source"], json!(""));
    }

    #[tokio::test]
    async fn token_rotation_without_data_dir_reports_not_persisted() {
        // 没注入数据目录（测试/嵌入场景）时仍要在内存里换，但必须老实回报「没落盘」。
        let st = state(vec![]).with_security(Some("mem-token-000000".into()), loopback_hosts());
        let app = build_app(st);
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/token",
                json!({"action": "generate"}),
                &[("x-aigw-token", "mem-token-000000")],
            ))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["persisted"], json!(false));
        assert!(v["message"].as_str().unwrap().contains("只在内存生效"), "要说清重启会失效：{v}");
    }

    #[tokio::test]
    async fn health_reports_token_requirement_and_source() {
        // 面板要能回答「为什么 SDK 报 401」，但令牌值不能经 API 回显：
        // 只报「要不要令牌」和「它从哪来」，让前端把用户指到启动参数/环境变量/日志。
        let res = build_app(state(vec![]))
            .oneshot(req("/api/health"))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["token_required"], json!(false), "本机回环默认不要求令牌");
        assert_eq!(v["token_source"], json!(""), "未启用时来源为空");
        assert!(v.get("token").is_none(), "绝不能回显令牌值");

        let st = state(vec![])
            .with_security(Some("t0k".into()), loopback_hosts())
            .with_token_source("auto");
        let res = build_app(st)
            .oneshot(get_with("/api/health", &[("x-aigw-token", "t0k")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["token_required"], json!(true));
        assert_eq!(v["token_source"], json!("auto"));
        let body = serde_json::to_string(&v).unwrap();
        assert!(!body.contains("t0k"), "令牌值不能出现在 health 里：{body}");
    }

    #[tokio::test]
    async fn health_always_reachable_without_token() {
        // /api/health 也要令牌？—— 是（同一命名空间），但 / 首页不受影响，
        // 这样前端能先拿到 401 再提示登录，而不是整页白屏。
        let st = state(vec![]).with_security(Some("t".into()), loopback_hosts());
        let app = build_app(st);
        let res = app.clone().oneshot(req("/")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn v1_api_shares_the_panel_token_when_one_is_set() {
        // /v1/* 是花钱/花额度的入口：非回环监听时面板令牌若不覆盖它，局域网内任何设备
        // 都能免密打满账号池。规则是「面板设了令牌 ⇒ /v1/* 用同一个令牌」。
        let body = r#"{"model":"whatever","messages":[]}"#;
        let mk = || {
            Request::post("/v1/chat/completions")
                .header("host", "127.0.0.1:8800")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap()
        };

        // ① 没设令牌（默认本机回环）：/v1 不要求令牌，落到模型解析（404）而不是 401。
        let app = build_app(state(vec![]));
        let res = app.oneshot(mk()).await.unwrap();
        assert_ne!(res.status(), StatusCode::UNAUTHORIZED, "未设令牌时 /v1 不该要求令牌");
        // 空 spec 表 → 503「没有任何启用的平台」；关键是它走到了 handler 而不是门禁。
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

        // ② 设了令牌：没有令牌 → 401，且提示里要写清「把它当 API Key 用」。
        let app = build_app(state(vec![]).with_security(Some("s3cret".into()), loopback_hosts()));
        let res = app.clone().oneshot(mk()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let b = body_string(res).await;
        assert!(b.contains("API Key"), "实际：{b}");
        assert!(b.contains("Authorization: Bearer"), "实际：{b}");

        // ③ 错误令牌 → 仍是 401（不能因为「格式对」就放行）。
        let bad = Request::post("/v1/chat/completions")
            .header("host", "127.0.0.1:8800")
            .header("authorization", "Bearer nope")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let res = app.clone().oneshot(bad).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // ④ 正确令牌（Bearer）→ 通过门禁，落到模型解析。
        let ok = Request::post("/v1/chat/completions")
            .header("host", "127.0.0.1:8800")
            .header("authorization", "Bearer s3cret")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let res = app.clone().oneshot(ok).await.unwrap();
        assert_ne!(res.status(), StatusCode::UNAUTHORIZED);

        // ⑤ /v1/models 同一规则（SDK 列模型也会被拦）。
        let res = app
            .clone()
            .oneshot(get_with("/v1/models", &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app
            .oneshot(get_with("/v1/models", &[("x-aigw-token", "s3cret")]))
            .await
            .unwrap();
        assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // ---------- 模型路由不再静默兜底 ----------

    fn two_platforms() -> Vec<ProviderSpec> {
        let mk = |id: &str, m: &str| {
            spec_toml(&format!(
                r#"
id = "{id}"
name = "{id}"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["{m}"]
models_policy = "static"
stream_dialect = "sse_delta"
"#
            ))
        };
        vec![mk("plat-a", "model-a"), mk("plat-b", "model-b")]
    }

    /// 带凭据的单平台：账号 id 就是号池里的键。
    ///
    /// 不能用 `two_platforms()`——那两条是 `auth = "anon"` 的免密车道，
    /// 转发时用 `{平台}:anon` 现造账号，账号行查池要按车道键走另一条路径。
    fn cred_platform(id: &str) -> Vec<ProviderSpec> {
        vec![spec_toml(&format!(
            r#"
id = "{id}"
name = "{id}"
enabled = true
auth = "cookie"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["m1"]
models_policy = "static"
stream_dialect = "sse_delta"
"#
        ))]
    }
    #[tokio::test]
    async fn unknown_model_is_404_not_silent_fallback() {
        // 有平台启用、但没人声明该模型 = 调用方写错模型名（永久性错误）。必须 404 +
        // code=model_not_found，不能 503：按状态码重试的客户端会无限重试。
        // 这里同时守住「多平台下不再静默兜底」这一行为。
        let app = build_app(state(two_platforms()));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"nobody-knows","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_string(res).await;
        assert!(body.contains("nobody-knows"), "body={body}");
        assert!(body.contains("model_not_found"), "body={body}");
        assert!(body.contains("invalid_request_error"), "body={body}");
    }

    #[tokio::test]
    async fn model_toggle_hides_from_models_and_blocks_routing() {
        let app = build_app(state(two_platforms()));
        // 关掉 model-a
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/platforms").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"action":"toggle_model","model":"model-a","enabled":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // 聚合清单里消失
        let body = body_string(app.clone().oneshot(req("/v1/models")).await.unwrap()).await;
        assert!(!body.contains("model-a"), "body={body}");
        assert!(body.contains("model-b"), "body={body}");
        // 显式 模型@平台 也被拒（404 model_not_found）
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"model-a@plat-a","messages":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn converted_requests_keep_session_affinity_and_strip_private_field() {
        // ★ 回归：/v1/messages 与 /v1/responses 会重建请求体，metadata 不跟着走。
        //   不补会话标识时粘性路由退化成「按最后一条 user 内容哈希」，多轮对话
        //   每轮换账号，上游 prompt cache 全丢。私有字段绝不能漏给上游。
        let body = r#"{"model":"mock-1","metadata":{"conversation_id":"conv-42"},
            "messages":[{"role":"user","content":"hi"}]}"#;
        let base = spawn_mock(
            r#"{"id":"c1","choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
            "application/json",
        )
        .await;
        let app = build_app(state(vec![spec_for(&base)]));
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // 转换后的请求体必须带会话标识，且该字段在转发前被剥掉
        let orig: Value = serde_json::from_str(body).unwrap();
        let mut oa = anthropic_to_openai(&orig).unwrap();
        assert!(oa.get("_aigw_session").is_none(), "转换函数不该自己加字段");
        carry_session(&mut oa, &orig);
        // 转换层搬进来的就是折过的定长键（17 字节），不是原始 conversation_id。
        assert_eq!(oa["_aigw_session"], key_id("conv-42"));
        // 转换前后必须是同一个键 —— 否则多轮对话每轮换账号，上游 prompt cache 全丢。
        assert_eq!(session_key(&oa), session_key(&orig));
        // 客户端伪造这个字段塞超长内容：仍然必须折成定长。
        let forged = json!({"_aigw_session": "z".repeat(1 << 20)});
        assert_eq!(session_key(&forged).len(), 17);
        // 键会折成定长哈希：同一个 conversation_id 仍映射到同一个键（粘性不变），
        // 但不会把原文原样驻留（长度由调用方决定 → 内存放大）。
        assert_eq!(session_key(&oa), key_id("conv-42"));
        assert_eq!(session_key(&oa), session_key(&oa));
    }
    #[tokio::test]
    async fn upstream_never_receives_private_session_field() {
        // ★ 私有字段 _aigw_session 只给粘性路由用，绝不能出现在发给上游的 body 里：
        //   多数 OpenAI 兼容上游对未知顶层字段是容忍的，但这属于把内部约定漏给第三方。
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sink = seen.clone();
        let app = Router::new().route(
            "/chat/completions",
            axum::routing::post(move |body: String| {
                let sink = sink.clone();
                async move {
                    *sink.lock().unwrap() = body;
                    ([(header::CONTENT_TYPE, "application/json")],
                     r#"{"id":"c1","choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#.to_string())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        let base = format!("http://{addr}");
        let gw = build_app(state(vec![spec_for(&base)]));
        let res = gw
            .oneshot(
                Request::post("/v1/responses").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","metadata":{"conversation_id":"conv-7"},"input":[{"role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let got = seen.lock().unwrap().clone();
        assert!(!got.is_empty(), "上游没收到请求");
        assert!(!got.contains("_aigw_session"), "私有字段漏给上游了：{got}");
    }
    #[tokio::test]
    async fn model_toggle_rejects_unknown_model_name() {
        // ★ 与 toggle_platform 对称：拼错的模型名必须 404，不能写进 flags.json。
        //   旧实现只校验非空，任何字符串都会被永久记成「已停用模型」。
        let app = build_app(state(two_platforms()));
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/platforms").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"action":"toggle_model","model":"model-typo","enabled":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_string(res).await;
        assert!(body.contains("model-typo"), "body={body}");
        // 已知模型仍然可以停用
        let res = app
            .oneshot(
                Request::post("/api/platforms").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"action":"toggle_model","model":"model-a","enabled":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn platform_toggle_disables_platform() {
        let app = build_app(state(two_platforms()));
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/platforms").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"action":"toggle_platform","id":"plat-a","enabled":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(app.clone().oneshot(req("/v1/models")).await.unwrap()).await;
        assert!(!body.contains("model-a"), "body={body}");
        // 平台清单里 enabled=false
        let pbody = body_string(app.oneshot(req("/api/platforms")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&pbody).unwrap();
        let row = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "plat-a")
            .unwrap();
        assert_eq!(row["enabled"], false);
    }

    #[test]
    fn upstream_status_never_leaks_success_codes() {
        // 上游 200 + HTML（网页版逆向失效）必须变成 502，不能回 200
        assert_eq!(upstream_status(0), 502);
        assert_eq!(upstream_status(200), 502);
        assert_eq!(upstream_status(204), 502);
        assert_eq!(upstream_status(302), 502);
        // 真实错误码原样透出
        assert_eq!(upstream_status(401), 401);
        assert_eq!(upstream_status(429), 429);
        assert_eq!(upstream_status(502), 502);
    }

    #[tokio::test]
    async fn anthropic_custom_api_is_501_not_upstream_404() {
        // 转发层只有 OpenAI 透传：anthropic 自定义 API 必须明确 501，
        // 不能带着 /chat/completions 去打 Anthropic 端点（那只会得到上游 404）。
        let custom = CustomStore::empty();
        let c = custom
            .add("克劳德", "anthropic", "https://api.anthropic.com", "sk-ant-test")
            .unwrap();
        custom.set_models(&c.id, vec!["claude-x".into()]).unwrap();
        let app = build_app(state_with_custom(custom, AccountStore::empty()));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"claude-x","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
        let body = body_string(res).await;
        assert!(body.contains("anthropic"), "body={body}");
    }

    #[tokio::test]
    async fn platform_toggle_invalidates_models_cache() {
        // 整表缓存 60s；停用平台后必须立刻从 /v1/models 消失（否则用户以为没生效）
        let app = build_app(state(two_platforms()));
        let body = body_string(app.clone().oneshot(req("/v1/models")).await.unwrap()).await;
        assert!(body.contains("model-a"), "首次应含 model-a：{body}");
        app.clone()
            .oneshot(
                Request::post("/api/platforms").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"action":"toggle_platform","id":"plat-a","enabled":false}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body2 = body_string(app.oneshot(req("/v1/models")).await.unwrap()).await;
        assert!(!body2.contains("model-a"), "缓存未失效：{body2}");
        assert!(body2.contains("model-b"), "body={body2}");
    }

    #[tokio::test]
    async fn invalidate_clears_both_table_and_per_platform_cache() {
        // 只清整表时，per-平台缓存里的旧模型名仍能被 spec_for_model 的 ③' 命中，
        // 用户拿到的是 401「无可用账号」而不是 503「未知模型」。
        let st = state(two_platforms());
        {
            let mut g = st.models_cache.lock().unwrap();
            *g = Some((Instant::now(), Vec::new(), serde_json::json!({"data": []})));
        }
        st.models_cache_per
            .lock()
            .unwrap()
            .insert("plat-a".into(), (Instant::now(), vec!["stale-model".into()], true));
        invalidate_models_cache(&st);
        assert!(st.models_cache.lock().unwrap().is_none(), "整表缓存未清");
        assert!(st.models_cache_per.lock().unwrap().is_empty(), "每平台缓存未清");
    }

    #[tokio::test]
    async fn platforms_api_reports_live_models() {
        // live 平台的静态 models 是空的（清单只存在于上游）。旧实现直接下发
        // spec.models → 接入源抽屉里一个模型都列不出来，用户无法逐模型停用。
        let spec = spec_toml(
            r#"
id = "live-plat"
name = "LivePlat"
enabled = true
region = "cn"
auth = "anon"
base_url = "https://example.invalid/v1"
format = "openai"
models = []
models_policy = "live"
stream_dialect = "sse_delta"
"#,
        );
        let st = state(vec![spec]);
        {
            let mut g = st.models_cache_per.lock().unwrap();
            g.insert(
                "live-plat".into(),
                (Instant::now(), vec!["live-a".into(), "live-b".into()], true),
            );
        }
        let app = build_app(st);
        let res = app.oneshot(req("/api/platforms")).await.unwrap();
        let body = body_string(res).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let row = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "live-plat")
            .expect("live-plat row");
        assert_eq!(row["models_n"], 2, "row={row}");
        assert_eq!(row["models_on"], 2, "row={row}");
        // 区域必须下发：前端要给「国内/国际」徽章，上游连不上时提示该不该挂代理
        assert_eq!(row["region"], "cn", "row={row}");
        let ids: Vec<&str> = row["models"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert!(ids.contains(&"live-a") && ids.contains(&"live-b"), "ids={ids:?}");
        // 协议与地址形态必须下发：卡片上的「不可用」要能说清是哪一条挡住的
        // （协议不支持透传 / 地址是占位符 / 没凭据），否则只能笼统说「凭据不可用」，
        // 用户会去填 Key 然后依然不通。
        assert_eq!(row["format"], "openai", "row={row}");
        assert_eq!(row["placeholder"], false, "row={row}");
    }

    #[tokio::test]
    async fn platforms_report_24h_health_from_usage_ledger() {
        // 128 个接入源没法逐个点开试：失败率必须由后端从台账聚合下发，
        // 前端才能在卡片上标「近 24h N/M 失败」。
        let st = state(two_platforms());
        let now = aigw_store::now_unix();
        let mk = |platform: &str, ts: f64, ok: bool| aigw_store::UsageEvent {
            ts,
            platform: platform.to_string(),
            model: "model-a".to_string(),
            account: String::new(),
            tin: 0,
            tout: 0,
            ms: 10,
            ok,
            upstream: String::new(),
            cancelled: false,
            kind: "chat".to_string(),
        };
        // plat-a：窗口内 1 成功 3 失败；plat-b 没有记录 → null（不是 0 值对象）
        st.store.usage_record(mk("plat-a", now - 60.0, true));
        for _ in 0..3 {
            st.store.usage_record(mk("plat-a", now - 120.0, false));
        }
        // 24h 之外的一条失败：不能算进 recent（否则「刚恢复的平台」永远挂红标）
        st.store.usage_record(mk("plat-a", now - PLATFORM_HEALTH_WINDOW_S - 3600.0, false));

        let app = build_app(st);
        let body = body_string(app.oneshot(req("/api/platforms")).await.unwrap()).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let row = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "plat-a")
            .expect("plat-a row");
        assert_eq!(row["recent"]["requests"], 4, "row={row}");
        assert_eq!(row["recent"]["success"], 1, "row={row}");
        assert_eq!(row["recent"]["failed"], 3, "row={row}");
        let idle = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "plat-b")
            .expect("plat-b row");
        assert!(idle["recent"].is_null(), "没有请求的平台必须是 null：{idle}");
    }

    #[tokio::test]
    async fn warm_models_signals_done_so_free_refresh_can_start() {
        // 公益站刷新的起跑时刻由这个信号决定（旧实现是固定 sleep 10s：预热只用 3 秒时
        // 白等 7 秒，预热要 12 秒时又照样撞车）。断言「跑完一定发信号」，否则刷新会一直
        // 等到 WARM_WAIT_CAP（60 秒）才起跑。
        let st = state_with_custom(CustomStore::empty(), AccountStore::empty());
        assert!(!*st.warm_done_rx().borrow(), "刚启动时预热尚未完成");
        st.clone().warm_models_supervised().await;
        assert!(*st.warm_done_rx().borrow(), "预热结束后必须放行公益站刷新");
    }

    #[tokio::test]
    async fn duplicate_model_ids_are_deduped() {
        // 两个平台声明同一个模型名 → 聚合清单只能出现一次（OpenAI 客户端按 id 索引）
        let mk = |id: &str| {
            spec_toml(&format!(
                r#"
id = "{id}"
name = "{id}"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["same-model"]
models_policy = "static"
stream_dialect = "sse_delta"
"#
            ))
        };
        let app = build_app(state(vec![mk("p1"), mk("p2")]));
        let body = body_string(app.oneshot(req("/v1/models")).await.unwrap()).await;
        assert_eq!(body.matches("\"same-model\"").count(), 1, "body={body}");
    }

    #[tokio::test]
    async fn placeholder_upstream_is_503_with_hint() {
        // @lobster 未配置时上游地址是占位符：必须给可读的 503，而不是
        // reqwest 的 "network: builder error"。
        let spec = spec_toml(
            r#"
id = "lob"
name = "Lob"
enabled = true
auth = "api_key"
base_url = "@lobster"
format = "openai"
models = ["m"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let accounts = AccountStore::from_rows(vec![aigw_store::Account {
            id: "l1".into(),
            platform: "lob".into(),
            name: "t".into(),
            secret: "k".into(),
            kind: "api_key".into(),
            enabled: true,
            status: String::new(),
            unreadable: false,
            domain: String::new(),
        }]);
        let app = build_app(state_with_accounts(vec![spec], accounts));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"m","messages":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_string(res).await;
        assert!(body.contains("未配置"), "body={body}");
        assert!(!body.contains("builder error"), "body={body}");
    }

    // ---------- 凭据可用性：自定义 API / 无凭据平台 ----------

    fn state_with_custom(custom: CustomStore, accounts: AccountStore) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState::new(
            vec![],
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(accounts),
            Arc::new(custom),
            Arc::new(FreeStore::empty()),
        )
    }

    #[tokio::test]
    async fn custom_api_uses_its_own_key() {
        // 自定义 API 的 Key 存在 custom.json（不在账号池）。旧实现只查账号池，
        // 于是「自定义 API 永远 401 无可用账号」——这里锁死该回归。
        let mock_resp = r#"{"id":"cmpl-c","choices":[{"message":{"role":"assistant","content":"CUSTOM-OK"}}]}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let custom = CustomStore::empty();
        let c = custom
            .add("我的中转", "openai", &base, "sk-custom-test")
            .unwrap();
        custom.set_models(&c.id, vec!["mock-1".into()]).unwrap();
        let app = build_app(state_with_custom(custom, AccountStore::empty()));
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("CUSTOM-OK"), "body={body}");
    }

    /// 起一个有 2 个模型的 custom 上游，返回 (state, key 明文)。
    async fn state_with_key(allow: Vec<String>) -> (AppState, String) {
        let mock_resp = r#"{"id":"cmpl-k","choices":[{"message":{"role":"assistant","content":"KEY-OK"}}]}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let custom = CustomStore::empty();
        let c = custom
            .add("我的中转", "openai", &base, "sk-custom-test")
            .unwrap();
        custom
            .set_models(&c.id, vec!["mock-1".into(), "mock-2".into()])
            .unwrap();
        let keys = aigw_store::KeyStore::empty();
        let (_k, plain) = keys.add("借用", allow, "");
        let st = state_with_custom(custom, AccountStore::empty()).with_keys(keys);
        (st, plain)
    }

    #[tokio::test]
    async fn access_key_outside_its_allow_list_is_rejected_with_403() {
        // 白名单的全部意义：这把 key 只能调它被允许的模型。
        // 不拦的话「借给同事一把 key」等于把全部模型都给出去。
        let (st, key) = state_with_key(vec!["mock-1".to_string()]).await;
        let app = build_app(st);
        let hit = |m: &str| {
            Request::post("/v1/chat/completions")
                .header("host", "127.0.0.1:8800")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::from(format!(
                    r#"{{"model":"{m}","messages":[{{"role":"user","content":"hi"}}]}}"#
                )))
                .unwrap()
        };
        let ok = app.clone().oneshot(hit("mock-1")).await.unwrap();
        assert_eq!(ok.status(), StatusCode::OK, "白名单内的模型必须放行");
        let res = app.clone().oneshot(hit("mock-2")).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "白名单外的模型必须 403");
        let body = body_string(res).await;
        assert!(body.contains("model_not_allowed"), "body={body}");
        assert!(body.contains("mock-1"), "拒绝文案要说明它被限定只能调什么：{body}");
    }

    #[tokio::test]
    async fn access_key_with_empty_allow_list_reaches_every_model() {
        // 空白名单 = 不限制（与面板令牌等价），别把「没填白名单」误判成「一个都不许」。
        let (st, key) = state_with_key(vec![]).await;
        let app = build_app(st);
        let res = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("host", "127.0.0.1:8800")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::from(
                        r#"{"model":"mock-2","messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn anthropic_and_responses_endpoints_also_honour_the_allow_list() {
        // 三条协议面各有一条独立的入口；只在 chat/completions 判白名单，
        // 换 /v1/messages 就能绕过 —— 所以每条都必须判。
        let (st, key) = state_with_key(vec!["mock-1".to_string()]).await;
        let app = build_app(st);
        let anth = app
            .clone()
            .oneshot(
                Request::post("/v1/messages")
                    .header("host", "127.0.0.1:8800")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::from(
                        r#"{"model":"mock-2","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anth.status(), StatusCode::FORBIDDEN, "/v1/messages 也要拦");
        let resp = app
            .oneshot(
                Request::post("/v1/responses")
                    .header("host", "127.0.0.1:8800")
                    .header("authorization", format!("Bearer {key}"))
                    .body(Body::from(r#"{"model":"mock-2","input":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "/v1/responses 也要拦");
    }

    #[tokio::test]
    async fn access_keys_never_come_back_out_of_the_list_endpoint() {
        // 密钥只在创建那一刻回显一次。列表接口要是把明文吐出来，
        // 一次截图/一次导出就等于把全部凭据送人。
        let keys = aigw_store::KeyStore::empty();
        let (_k, plain) = keys.add("脚本", vec!["mock-1".into()], "");
        let st = state(vec![]).with_keys(keys);
        let app = build_app(st);
        let res = app.oneshot(get_with("/api/keys", &[])).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("脚本"), "名字要能看到：{body}");
        assert!(!body.contains(&plain), "列表绝不能回显密钥本身：{body}");
    }

    #[tokio::test]
    async fn access_keys_crud_creates_lists_rotates_and_removes() {
        let keys = aigw_store::KeyStore::empty();
        let app = build_app(state(vec![]).with_keys(keys));
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/keys",
                json!({"action":"add","name":"部门脚本","allow_models":["mock-1"]}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let id = v["row"]["id"].as_str().unwrap().to_string();
        let created = v["key"].as_str().unwrap().to_string();
        assert!(v["message"].as_str().unwrap_or("").contains("只在这里回显一次"));

        // 轮换：新 key 与旧 key 必须不同，且旧的立刻失效。
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/keys",
                json!({"action":"update","id":id,"rotate":true}),
                &[],
            ))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let rotated = v["key"].as_str().unwrap().to_string();
        assert_ne!(rotated, created, "轮换必须换出新密钥");

        let res = app
            .clone()
            .oneshot(post_with("/api/keys", json!({"action":"remove","id":id}), &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .oneshot(post_with("/api/keys", json!({"action":"remove","id":id}), &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "删不存在的 key 要 404");
    }

    /// ★ 回归：轮换/删除后的旧 key 必须立刻失效 —— 而且不能因为「面板令牌没开」就漏判
    /// （那正是旧实现的形态：没开令牌时 guard 整块跳过，旧 key 被当匿名放行 200）。
    #[tokio::test]
    async fn revoked_access_keys_stop_working_even_without_a_panel_token() {
        let (st, key1) = state_with_key(vec!["mock-1".to_string()]).await;
        assert!(
            st.current_token().is_none(),
            "本用例的前提是没开面板令牌，否则测不到吊销漏洞"
        );
        let app = build_app(st.clone());
        let call = |k: String| {
            let app = app.clone();
            async move {
                app.oneshot(
                    Request::post("/v1/chat/completions")
                        .header("host", "127.0.0.1:8800")
                        .header("authorization", format!("Bearer {k}"))
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"model": "mock-1", "messages": [{"role":"user","content":"hi"}]})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };
        assert_eq!(call(key1.clone()).await.status(), StatusCode::OK, "轮换前旧 key 应该能用");

        // 轮换
        let id = st.keys.snapshot()[0].id.clone();
        let (_, fresh) = st.keys.update(&id, None, None, None, None, true).unwrap();
        let fresh = fresh.expect("轮换必须给出新密钥");
        assert_ne!(fresh, key1);

        let resp = call(key1.clone()).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "轮换后旧 key 必须立刻 401");
        let body = body_string(resp).await;
        assert!(body.contains("已失效"), "文案要说清楚它是失效了：{body}");
        assert_eq!(call(fresh.clone()).await.status(), StatusCode::OK, "新 key 应该能用");

        // 删除后同样立刻失效
        assert!(st.keys.remove(&id));
        assert_eq!(call(fresh).await.status(), StatusCode::UNAUTHORIZED);
    }

    /// 白名单拼错是**静默失败**：请求 403 model_not_allowed，用户在面板上看到一串
    /// 名字，很难发现哪个字母错了。接口要把匹配不到的条目明说。
    /// 白名单必须有闸门：实测过去能塞 5000 条并落盘成功。
    /// 实测：贴 5MB 的 secret 曾 200 落盘（accounts.json 从 17KB 涨到 7MB）。
    /// 超长模型名：写进 flags.json 之前就该挡住；不存在的短名字也要截断回显。
    #[tokio::test]
    async fn toggle_model_rejects_an_overlong_name_and_truncates_the_echo() {
        let app = build_app(state(vec![]));
        let long = "m".repeat(MAX_MODEL_NAME_LEN + 1);
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/platforms",
                json!({"action": "toggle_model", "model": long}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "超长模型名要 400");
        // 不存在的模型：404，但回显必须截断（整段回显会被日志/控制台放大）
        let res = app
            .oneshot(post_with(
                "/api/platforms",
                json!({"action": "toggle_model", "model": "nope-model"}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_string(res).await;
        assert!(!body.contains(&"m".repeat(200)), "不许整段回显：{body}");
    }

    /// 短信接口的参数校验与自己能拦住的部分：真实发短信要连讯飞，单测里只验「先拦在本地」。
    /// 重点是不合法的手机号**不能**被拿去问上游（那会变成「号码未注册」这种误导报错）。
    /// 密码登录缺字段必须本地就 400 —— 不能带着空账号去打讯飞（会被风控记一笔）。
    #[tokio::test]
    async fn password_login_rejects_missing_fields_before_calling_upstream() {
        let app = build_app(state(vec![]).with_keys(aigw_store::KeyStore::empty()));
        for body in [
            json!({"action": "pwd_login"}),
            json!({"action": "pwd_login", "platform": "apk-loomy"}),
            json!({"action": "pwd_login", "platform": "apk-loomy", "loginid": "13800138000"}),
            // 只有空白的账号也算缺
            json!({"action": "pwd_login", "platform": "apk-loomy", "loginid": "   ", "password": "x"}),
        ] {
            let res = app.clone().oneshot(post_with("/api/auth", body.clone(), &[])).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body={body}");
            let out = body_string(res).await;
            assert!(out.contains("缺 platform / loginid / password"), "body={body} out={out}");
        }
    }

    #[tokio::test]
    async fn sms_login_rejects_bad_input_before_calling_upstream() {
        let app = build_app(state(vec![]));
        for body in [
            json!({"action": "sms_send", "phone": "13800138000"}),      // 缺 platform
            json!({"action": "sms_send", "platform": "apk-loomy"}),     // 缺 phone
            json!({"action": "sms_verify", "platform": "apk-loomy", "phone": "13800138000"}), // 缺 code/msgid
        ] {
            let res = app.clone().oneshot(post_with("/api/auth", body.clone(), &[])).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body={body}");
        }
        // 号码格式错：本地就拒，且提示里给出正确样例
        let res = app
            .oneshot(post_with(
                "/api/auth",
                json!({"action": "sms_send", "platform": "apk-loomy", "phone": "12345"}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_string(res).await;
        assert!(body.contains("手机号格式不对"), "{body}");
        assert!(body.contains("13800138000"), "要给个正确样例：{body}");
    }

    #[tokio::test]
    async fn auth_save_rejects_an_oversized_credential() {
        let app = build_app(state(vec![]));
        let big = "A".repeat(MAX_CREDENTIAL_LEN + 1);
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/auth",
                json!({"action": "save", "platform": "apk-wps", "secret": big}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "超长凭据必须拒绝");
        let body = body_string(res).await;
        assert!(body.contains("凭据过长"), "要说清上限：{body}");
    }

    #[tokio::test]
    async fn access_keys_reject_an_oversized_allow_list() {
        let app = build_app(state(vec![]).with_keys(aigw_store::KeyStore::empty()));
        let many: Vec<String> = (0..(aigw_store::keys::MAX_ALLOW_MODELS + 1))
            .map(|i| format!("m{i}"))
            .collect();
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/keys",
                json!({"action": "add", "name": "big", "allow_models": many}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "超限必须拒绝，不能静默收下");
        let body = body_string(res).await;
        assert!(body.contains("白名单最多"), "要说清上限是多少：{body}");
        // 单条过长同样要拒（一条超长字符串会让比对与落盘都变慢）
        let long = "x".repeat(aigw_store::keys::MAX_ALLOW_ENTRY_LEN + 1);
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/keys",
                json!({"action": "add", "name": "long", "allow_models": [long]}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        // 正常规模必须放行（别把闸门设得让人没法用）
        let res = app
            .oneshot(post_with(
                "/api/keys",
                json!({"action": "add", "name": "ok", "allow_models": ["a", "b", "c"]}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn access_key_warns_about_allow_entries_that_match_no_model() {
        // 一个可用平台，静态声明一个模型 → /v1/models 里只有 kilo-模型这一个 id
        let spec = spec_toml(r#"
id = "plat-k"
name = "plat-k"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["k-model"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let app = build_app(state(vec![spec]));
        // k-model@wrong-plat：模型名对、平台写错 —— 这条**必须**被报出来（它一个模型都调不到），
        // 是「写 模型@平台 时平台也要比对」这条逻辑的对称断言。
        let body = json!({"action": "add", "name": "probe",
            "allow_models": ["k-model", "k-modle", "k-model@plat-k", "k-model@wrong-plat"]});
        let out = body_string(
            app.clone()
                .oneshot(post_with("/api/keys", body, &[]))
                .await
                .unwrap(),
        )
        .await;
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], json!(true), "拼错不该拒绝创建：{out}");
        assert_eq!(
            v["unknown_models"],
            json!(["k-modle", "k-model@wrong-plat"]),
            "只该报真正调不到的那两个（写对的裸名与 模型@平台 都不算错）：{out}"
        );
    }

    #[tokio::test]
    async fn access_keys_reject_unknown_actions_and_blank_names() {
        let app = build_app(state(vec![]).with_keys(aigw_store::KeyStore::empty()));
        let res = app
            .clone()
            .oneshot(post_with("/api/keys", json!({"action":"nope"}), &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/keys",
                json!({"action":"add","name":"   "}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "空名字要 400");
        // ★ 改名同样不许清空：add 有校验、update 之前没有 —— 结果是面板上出现一行
        //   没有名字的 key，谁也认不出它是给哪个脚本用的，要轮换时只能靠猜。
        let (row, _) = app_keys_add(&app).await;
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/keys",
                json!({"action":"update","id":row,"name":"   "}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "update 改成空名字也要 400");
    }

    /// 建一把 key 并返回它的 id（写面板接口的测试共用）。
    async fn app_keys_add(app: &axum::Router) -> (String, String) {
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/keys",
                json!({"action":"add","name":"临时脚本"}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let raw = body_string(res).await;
        let v: Value = serde_json::from_str(&raw).unwrap();
        (
            v["row"]["id"].as_str().unwrap_or("").to_string(),
            v["key"].as_str().unwrap_or("").to_string(),
        )
    }
    #[tokio::test]
    async fn combos_api_round_trips_and_reports_what_is_actually_callable() {
        // 链的价值全在「这条链现在还能回退几次」。只回显 models 字段的接口会让用户
        // 以为写了 3 个就是 3 层保险，而其中两个平台其实没凭据。
        let app = build_app(state(vec![]));
        // 空表时是空列表而不是 404：前端一进页面就要能画出来
        let res = app.clone().oneshot(get_with("/api/combos", &[])).await.unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["count"], json!(0));
        assert!(v["rows"].as_array().unwrap().is_empty());

        let res = app
            .clone()
            .oneshot(post_with(
                "/api/combos",
                json!({"action":"add","name":"快速链","models":["m1","m2"],"strategy":"round-robin","sticky_limit":2}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let raw = body_string(res).await;
        let v: Value = serde_json::from_str(&raw).unwrap();
        let id = v["combo"]["id"].as_str().unwrap().to_string();
        // 中文名也要生成稳定、带 combo: 前缀、且 URL 安全的 id
        assert!(id.starts_with("combo:"), "{id}");
        assert!(id.is_ascii(), "id 必须是 ASCII（要能放进 URL 与 JSON 的模型名）：{id}");
        assert_eq!(v["combo"]["strategy"], json!("round-robin"));
        assert_eq!(v["combo"]["sticky_limit"], json!(2));

        // 没有任何平台声明 m1/m2 → effective 空、unavailable 两个都要报出来
        let res = app.clone().oneshot(get_with("/api/combos", &[])).await.unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["rows"][0]["effective"], json!([]));
        assert_eq!(v["rows"][0]["unavailable"], json!(["m1", "m2"]));

        // 停用后 enabled=false，但链还在（不是被删掉）
        let res = app
            .clone()
            .oneshot(post_with("/api/combos", json!({"action":"update","id":id,"enabled":false}), &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["combo"]["enabled"], json!(false));

        // 空名字必须拒绝，不能建出一条没有名字的链
        let res = app
            .clone()
            .oneshot(post_with("/api/combos", json!({"action":"add","name":"   ","models":["m1"]}), &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = app
            .clone()
            .oneshot(post_with("/api/combos", json!({"action":"remove","id":id}), &[]))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["removed"], json!(true));
    }

    #[tokio::test]
    async fn a_missing_head_model_does_not_abort_the_chain() {
        // ★ 第一次真机实测就栽在这：候选写成本机不认识的模型名时，整条链直接 404，
        //   后面明明有能用的候选一个都没试。每个平台认的模型名都不一样（kilo 有
        //   kilo-auto/free，别的平台没有），链里写错一个名字是常事，不该整条链作废。
        // ★ 必须凑出「两个可用平台」：只有一个平台时 spec_for_model 的单平台兜底
        //   会把任何模型名都转给它（本机没有这个模型名册，只能让上游裁决），
        //   于是第一个候选「成功」，回退逻辑根本不会被触发 —— 这正是本测试第一版
        //   假绿的原因（把 model_not_found 从继续条件里删掉，测试照样通过）。
        let mock_resp = r#"{"id":"cmpl-f","choices":[{"message":{"role":"assistant","content":"SECOND-OK"}}]}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let other = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let custom = CustomStore::empty();
        let c = custom.add("mock", "openai", &base, "k").unwrap();
        custom.set_models(&c.id, vec!["mock-model".into()]).unwrap();
        // 第二个平台也是可用平台（有清单、有 Key），只为把 usable.len() 顶到 2，
        // 让「本机不认识的模型名」真的走到 404 model_not_found。
        let c2 = custom.add("other", "openai", &other, "k2").unwrap();
        custom
            .set_models(&c2.id, vec!["other-model".into()])
            .unwrap();
        let app = build_app(state_with_custom(custom, AccountStore::empty()));
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/combos",
                json!({"action":"add","name":"head-missing","models":["nobody-declares-this","mock-model"]}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .clone()
            .oneshot(post_with(
                "/v1/chat/completions",
                json!({"model":"combo:head-missing","messages":[{"role":"user","content":"hi"}]}),
                &[],
            ))
            .await
            .unwrap();
        let raw = body_string(res).await;
        assert!(
            !raw.contains("候选全部失败"),
            "第一个候选本机不认识时不该放弃整条链：{raw}"
        );
        assert!(raw.contains("SECOND-OK"), "应该由第二个候选答出来：{raw}");
    }

    #[tokio::test]
    async fn enabled_combos_are_published_as_virtual_models() {
        // 客户端只从 /v1/models 拉下拉框：链不进清单，用户根本选不到它。
        // 停用的链必须**不**出现 —— 那是 chat 侧会 404 的死条目。
        let app = build_app(state(vec![]));
        for (name, off) in [("on-chain", false), ("off-chain", true)] {
            let res = app
                .clone()
                .oneshot(post_with(
                    "/api/combos",
                    json!({"action":"add","name":name,"models":["m1"]}),
                    &[],
                ))
                .await
                .unwrap();
            let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
            let id = v["combo"]["id"].as_str().unwrap().to_string();
            if off {
                app.clone()
                    .oneshot(post_with("/api/combos", json!({"action":"update","id":id,"enabled":false}), &[]))
                    .await
                    .unwrap();
            }
        }
        let res = app.clone().oneshot(get_with("/v1/models", &[])).await.unwrap();
        let raw = body_string(res).await;
        let v: Value = serde_json::from_str(&raw).unwrap();
        let ids: Vec<&str> = v["data"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
        assert!(ids.contains(&"combo:on-chain"), "启用的链必须出现在清单里：{ids:?}");
        assert!(!ids.contains(&"combo:off-chain"), "停用的链不该出现：{ids:?}");
        // 链条目的 owned_by 要能筛出来
        let row = v["data"].as_array().unwrap().iter().find(|m| m["id"] == json!("combo:on-chain")).unwrap();
        assert_eq!(row["owned_by"], json!("combo"));
    }

    #[tokio::test]
    async fn a_combo_name_that_matches_nothing_is_a_clear_404() {
        // 把链名写错时，报错必须指向「链」而不是「模型名拼错」—— 两个排查方向完全不同。
        let app = build_app(state(vec![]));
        let res = app
            .clone()
            .oneshot(post_with(
                "/v1/chat/completions",
                json!({"model":"combo:nope","messages":[{"role":"user","content":"hi"}]}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let raw = body_string(res).await;
        assert!(raw.contains("模型链"), "必须说清是链的问题：{raw}");
        assert!(raw.contains("combo:nope"), "{raw}");
    }

    #[tokio::test]
    async fn a_disabled_combo_refuses_instead_of_silently_routing() {
        // 停用的链必须拒绝：静默换成链里的模型会把「我关了它」变成没关。
        let app = build_app(state(vec![]));
        let res = app
            .clone()
            .oneshot(post_with(
                "/api/combos",
                json!({"action":"add","name":"off-chain","models":["m1"],"enabled":false}),
                &[],
            ))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let id = v["combo"]["id"].as_str().unwrap().to_string();
        assert_eq!(v["combo"]["enabled"], json!(true), "add 默认启用，停用要靠 update");
        app.clone()
            .oneshot(post_with("/api/combos", json!({"action":"update","id":id,"enabled":false}), &[]))
            .await
            .unwrap();
        let res = app
            .clone()
            .oneshot(post_with(
                "/v1/chat/completions",
                json!({"model":id,"messages":[{"role":"user","content":"hi"}]}),
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let raw = body_string(res).await;
        assert!(raw.contains("停用"), "{raw}");
    }

    #[tokio::test]
    async fn free_sites_expose_registration_links_for_the_card() {
        // 公益站卡片上的「注册页 / 复制邀请链接」完全靠 login_url 渲染，而聚合站给的
        // url 里带 aff 邀请码。这条链路上只要有一处丢字段，用户就拿不到返利链接：
        // /api/free 用 `url` 原样下发，/api/platforms 把它映射成 `login_url`。
        let free = std::sync::Arc::new(FreeStore::empty());
        free.upsert_batch(vec![aigw_store::FreeSite {
            id: "free-aff-example".to_string(),
            name: "Aff 示例站".to_string(),
            base_url: "https://api.aff.example".to_string(),
            url: "https://aff.example/sign-up?aff=ABC123".to_string(),
            desc: "示例".to_string(),
            enabled: true,
        }]);
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(
            vec![],
            Arc::new(Store::open(dir.path())),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            free,
        );
        let app = build_app(st);
        let body = body_string(app.clone().oneshot(get_with("/api/free", &[])).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let f = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "free-aff-example")
            .unwrap_or_else(|| panic!("/api/free 少了这条站：{body}"));
        assert_eq!(f["url"], "https://aff.example/sign-up?aff=ABC123", "body={body}");

        let body = body_string(app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap()).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let p = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "free-aff-example")
            .unwrap_or_else(|| panic!("平台卡片少了这条公益站：{body}"));
        assert_eq!(
            p["login_url"], "https://aff.example/sign-up?aff=ABC123",
            "卡片按钮要拿这个链接，body={body}"
        );
    }

    #[tokio::test]
    async fn free_list_reports_sources_and_last_refresh() {
        // 面板要显示「用了哪些聚合源、上次刷新是命中还是回退」：
        // 这依赖 /api/free 把 sources 和 last_refresh 一起下发。
        let st = state_with_custom(CustomStore::empty(), AccountStore::empty());
        st.tasks
            .mark_ok("free_refresh", "共 43 站；聚合源 raw.githubusercontent.com 命中");
        let app = build_app(st);
        let res = app
            .oneshot(
                Request::get("/api/free")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let b: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert!(
            !b["sources"].as_array().unwrap().is_empty(),
            "sources 至少要带内置源"
        );
        assert_eq!(
            b["last_refresh"]["last_ok"],
            "共 43 站；聚合源 raw.githubusercontent.com 命中"
        );
        assert!(
            b["last_refresh"]["last_ok_s_ago"].is_number(),
            "要给「多少秒之前」而不是绝对时间"
        );
    }

    #[test]
    fn custom_entry_with_undecryptable_key_is_hidden_and_has_no_credential() {
        // 换机器/换 Windows 用户之后，custom.json 里的密文解不开。必须做到三件事：
        // 条目不算可用（不出现在 /v1/models）、凭据取不到（不拿密文去换 401）、
        // 面板直说「需重新填写」——否则用户只能看到一个莫名的鉴权失败。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("custom.json"),
            r#"[{"id":"custom-dead-0","name":"别处的中转","format":"openai","base_url":"https://x.example","api_key":"dpapi:AAAA","models":["m1"],"enabled":true}]"#,
        )
        .unwrap();
        let st = state_with_custom(CustomStore::open(dir.path()), AccountStore::empty());
        let specs = st.all_specs();
        let sp = specs
            .iter()
            .find(|s| s.id == "custom-dead-0")
            .expect("条目仍要在册（只是不可用）");
        assert!(!sp.enabled, "解不开的 Key 不能让条目算可用");
        assert!(sp.notes.contains("重新填"), "notes 要给出修复方式：{}", sp.notes);
        assert!(st.platform_secret(sp).is_none(), "绝不能把密文当凭据发出去");
        assert_eq!(
            st.custom.snapshot()["rows"][0]["api_key_mask"],
            "需重新填写"
        );
    }

    #[tokio::test]
    async fn sync_failure_keeps_the_previous_model_list() {
        // ★ 回归：「同步模型」失败绝不等于「清空模型」。
        //   旧实现无论成功失败都返回 0，调用方无从分辨；这里锁住两件事：
        //   ① 上游 500 时清单原样保留（否则 /v1/models 上这个平台的模型会整片消失）；
        //   ② 失败原因要说出来，而不是一句笼统的「没有返回清单」。
        let ok = spawn_models_mock(&["m-1", "m-2"]).await;
        let cs = CustomStore::empty();
        let c = cs.add("我的中转", "openai", &ok, "sk-1").unwrap();
        let st = state_with_custom(cs, AccountStore::empty());
        // 先成功同步一次，落下一份清单
        let n = sync_custom_models(&st, &st.custom.rows()[0]).await.expect("第一次同步应成功");
        assert_eq!(n, 2);
        assert_eq!(st.custom.rows()[0].models.len(), 2);

        // 把 base_url 指到一个必定失败的 /models 上，再同步
        let bad = spawn_status_models_mock(StatusCode::INTERNAL_SERVER_ERROR).await;
        st.custom
            .update(&c.id, "我的中转", "openai", &bad, "")
            .unwrap();
        let before = st.custom.rows()[0].models.clone();
        let err = sync_custom_models(&st, &st.custom.rows()[0])
            .await
            .expect_err("上游 500 必须报错");
        assert!(err.contains("500"), "错误里要带上游状态码：{err}");
        assert_eq!(
            st.custom.rows()[0].models, before,
            "同步失败不许动旧清单（否则模型会整片消失）"
        );
    }

    #[tokio::test]
    async fn refresh_all_custom_apis_reports_per_entry_result() {
        // 定时任务要能同时处理「一个能连、一个连不上」：成功的记账、
        // 失败的带名字报出来，且不把整轮判成成功。
        let ok = spawn_models_mock(&["m1", "m2"]).await;
        let cs = CustomStore::empty();
        cs.add("好的中转", "openai", &ok, "sk-1").unwrap();
        cs.add("坏的中转", "openai", "http://127.0.0.1:1/v1", "sk-2")
            .unwrap();
        let st = state_with_custom(cs, AccountStore::empty());
        let err = st
            .refresh_custom_models_now()
            .await
            .expect_err("有失败就整体报错，好让调度按 retry 早起重试");
        assert!(err.contains("坏的中转"), "失败条目要报名字：{err}");
        assert!(err.contains("1 个成功"), "成功数量也要说清楚：{err}");
        // 好的那条确实拿到了清单
        let good = st
            .custom
            .rows()
            .into_iter()
            .find(|c| c.name == "好的中转")
            .unwrap();
        assert_eq!(good.models, vec!["m1".to_string(), "m2".to_string()]);
    }

    #[tokio::test]
    async fn refresh_without_custom_apis_is_not_an_error() {
        // 没配自定义 API 是常态，不能因此每 6 小时报一次失败。
        let st = state_with_custom(CustomStore::empty(), AccountStore::empty());
        let msg = st.refresh_custom_models_now().await.unwrap();
        assert!(msg.contains("没有自定义 API"), "{msg}");
    }

    #[tokio::test]
    async fn empty_upstream_list_does_not_wipe_the_previous_one() {
        // 上游 200 但清单为空，同样不是「权威重置」：保留旧清单并报错。
        // （真实场景：聚合桥重新加载时会先回一份空清单。）
        let ok = spawn_models_mock(&["only-1"]).await;
        let cs = CustomStore::empty();
        let c = cs.add("我的中转", "openai", &ok, "sk-1").unwrap();
        let st = state_with_custom(cs, AccountStore::empty());
        sync_custom_models(&st, &st.custom.rows()[0]).await.unwrap();
        assert_eq!(st.custom.rows()[0].models, vec!["only-1".to_string()]);

        let empty = spawn_models_mock(&[]).await;
        st.custom
            .update(&c.id, "我的中转", "openai", &empty, "")
            .unwrap();
        let err = sync_custom_models(&st, &st.custom.rows()[0])
            .await
            .expect_err("空清单要当失败报");
        assert!(err.contains("空"), "错误要说清是空清单：{err}");
        assert_eq!(
            st.custom.rows()[0].models,
            vec!["only-1".to_string()],
            "空清单不许把旧清单冲掉"
        );
    }

    #[tokio::test]
    async fn pinned_custom_list_beats_live_cache_and_survives_update() {
        // 聚合桥这类上游的「过度承诺」清单不能冲掉用户收敛好的白名单：
        // ① live 缓存里就算躺着上游全量，/v1/models 也只发布固定清单；
        // ② update 条目后不自动重拉（一重拉死模型就全回来了）。
        let upstream = spawn_models_mock(&["up-1", "up-2", "up-3"]).await;
        let cs = CustomStore::empty();
        let c = cs.add("我的桥", "openai", &upstream, "sk-pin").unwrap();
        cs.set_models_pinned(&c.id, vec!["good-1".into(), "good-2".into()], true)
            .unwrap();
        let st = state_with_custom(cs, AccountStore::empty());
        // 模拟预热已把上游全量清单写进 live 缓存
        st.models_cache_per.lock().unwrap().insert(
            c.id.clone(),
            (Instant::now(), vec!["up-1".into(), "up-2".into(), "up-3".into()], true),
        );
        let app = build_app(st.clone());
        let res = app
            .clone()
            .oneshot(
                Request::get("/v1/models")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let b: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let ids: Vec<&str> = b["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["owned_by"].as_str() == Some(c.id.as_str()))
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["good-1", "good-2"], "固定条目只该发布白名单：{b}");

        // ② update 改名不该触发自动重拉
        let body = format!(r#"{{"action":"update","id":"{}","name":"桥改名"}}"#, c.id);
        let res2 = app
            .oneshot(
                Request::post("/api/custom")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::OK);
        let v2: Value = serde_json::from_str(&body_string(res2).await).unwrap();
        assert_eq!(
            v2["models_synced"],
            json!(0),
            "固定条目 update 不该自动重拉上游：{v2}"
        );
        let snap = st.custom.snapshot();
        assert_eq!(snap["rows"][0]["models_pinned"], json!(true));
        assert_eq!(
            snap["rows"][0]["models"],
            json!(["good-1", "good-2"]),
            "白名单不许被 update 冲掉"
        );
    }

    #[test]
    fn models_ttl_prefers_short_retry_after_failure() {
        // 失败条目必须走短档（120s）以便较快重试——即使它带着可继续服务的旧清单。
        assert_eq!(models_ttl(&[], true), MODELS_TTL_ERR);
        assert_eq!(models_ttl(&["a".into()], true), MODELS_TTL_OK);
        assert_eq!(models_ttl(&["a".into()], false), MODELS_TTL_ERR);
    }

    #[tokio::test]
    async fn failed_models_refetch_keeps_previous_list_and_marks_entry_failed() {
        // ★ 回归：旧实现失败即塞空清单（unwrap_or_default）——慢公益站一次 12s
        //   超时或一轮 429，就把平台的全部模型从 /v1/models 和路由兜底里清空
        //   最长 120 秒。现在必须保留上一次的成功清单，只把 ok 标成 false。
        let dead = spawn_mock("{}", "application/json").await; // 只挂 /chat/completions，拉 /models 必 404
        let spec = spec_toml(&format!(
            r#"
id = "flaky"
name = "flaky"
region = "cn"
auth = "anon"
base_url = "{dead}"
format = "openai"
models = []
models_policy = "live"
stream_dialect = "sse_delta"
"#,
        ));
        let st = state(vec![spec.clone()]);
        st.models_cache_per.lock().unwrap().insert(
            "flaky".to_string(),
            (Instant::now(), vec!["good-1".to_string()], true),
        );
        st.fetch_live_models(vec![spec]).await;
        let g = st.models_cache_per.lock().unwrap();
        let (_, ids, ok) = g.get("flaky").expect("失败也要写缓存条目（短 TTL 防反复撞）");
        assert_eq!(ids, &vec!["good-1".to_string()], "旧清单不许被一次失败清空");
        assert!(!ok, "必须标记失败，让 TTL 走短档");
    }

    #[tokio::test]
    async fn failed_platform_still_publishes_and_routes_its_stale_list() {
        // stale-on-error 的用户可见面：条目 ok=false 但清单还在时，
        // /v1/models 照常发布、spec_for_model 照常路由（chat 不再 503「未知模型」）。
        let spec = spec_toml(
            r#"
id = "flaky"
name = "flaky"
region = "cn"
auth = "anon"
base_url = "https://example.invalid/v1"
format = "openai"
models = []
models_policy = "live"
stream_dialect = "sse_delta"
"#,
        );
        let st = state(vec![spec]);
        st.models_cache_per.lock().unwrap().insert(
            "flaky".to_string(),
            (Instant::now(), vec!["good-1".to_string()], false),
        );
        // 路由兜底立刻可用
        assert_eq!(
            st.spec_for_model("good-1").map(|s| s.id),
            Some("flaky".to_string())
        );
        // /v1/models 照常发布旧清单
        let app = build_app(st.clone());
        let res = app
            .oneshot(
                Request::get("/v1/models")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let row = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "good-1")
            .unwrap_or_else(|| panic!("旧清单必须继续发布：{v}"))
            .clone();
        assert_eq!(row["owned_by"], json!("flaky"));
    }

    #[tokio::test]
    async fn sync_models_explicit_list_pins_and_refetch_unpins() {
        // sync_models 显式给清单 = 「以这份为准」（固定）；不带清单 = 「重新跟随上游」。
        let upstream = spawn_models_mock(&["up-1", "up-2"]).await;
        let cs = CustomStore::empty();
        let c = cs.add("我的桥", "openai", &upstream, "sk-pin2").unwrap();
        let st = state_with_custom(cs, AccountStore::empty());
        let app = build_app(st.clone());
        let body = format!(
            r#"{{"action":"sync_models","id":"{}","models":["only-1","only-2"]}}"#,
            c.id
        );
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/custom")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert!(
            v["message"].as_str().unwrap_or("").contains("已固定"),
            "要明确告诉用户清单已固定：{v}"
        );
        let snap = st.custom.snapshot();
        assert_eq!(snap["rows"][0]["models_pinned"], json!(true));
        assert_eq!(snap["rows"][0]["models"], json!(["only-1", "only-2"]));

        // 不带清单 = 恢复跟随上游：重拉并解除固定
        let body2 = format!(r#"{{"action":"sync_models","id":"{}"}}"#, c.id);
        let res2 = app
            .oneshot(
                Request::post("/api/custom")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::from(body2))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::OK);
        let snap2 = st.custom.snapshot();
        assert_eq!(
            snap2["rows"][0]["models_pinned"],
            json!(false),
            "重拉上游后应解除固定"
        );
        assert_eq!(snap2["rows"][0]["models"], json!(["up-1", "up-2"]));
    }

    #[tokio::test]
    async fn custom_add_and_update_never_echo_plaintext_key() {
        // ★ 回归：add/update 过去把整个 CustomApi 序列化回给调用方，里面是明文 api_key。
        //   列表接口一直是掩码的，只有这两个写接口漏了 —— 面板会把响应打进控制台/日志，
        //   等于把凭据又抄了一份到浏览器侧。
        let app = build_app(state_with_custom(CustomStore::empty(), AccountStore::empty()));
        let body = r#"{"action":"add","name":"echo-test","format":"openai",
            "base_url":"http://127.0.0.1:9/v1","api_key":"sk-plain-secret-1234567890"}"#;
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/custom").header("host", "127.0.0.1:8800")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_string(res).await;
        assert!(
            !text.contains("sk-plain-secret-1234567890"),
            "保存响应里回显了明文 Key：{text}"
        );
        assert!(text.contains("api_key_mask"), "body={text}");
        let id = serde_json::from_str::<Value>(&text).unwrap()["api"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // update 分支同样不能回显
        let upd = format!(
            r#"{{"action":"update","id":"{id}","name":"echo-test-2",
                "base_url":"http://127.0.0.1:9/v1","api_key":"sk-second-secret-abcdef"}}"#
        );
        let res = app
            .oneshot(
                Request::post("/api/custom").header("host", "127.0.0.1:8800")
                    .body(Body::from(upd))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_string(res).await;
        assert!(
            !text.contains("sk-second-secret-abcdef"),
            "更新响应里回显了明文 Key：{text}"
        );
        assert!(text.contains("api_key_mask"), "body={text}");
    }

    #[tokio::test]
    async fn models_hide_platforms_without_credentials() {
        // 无凭据平台的模型必然 401，不该出现在聚合清单里
        let with_acc = spec_toml(
            r#"
id = "has-key"
name = "HasKey"
enabled = true
auth = "api_key"
base_url = "http://127.0.0.1:9/v1"
format = "openai"
models = ["m-good"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let no_acc = spec_toml(
            r#"
id = "no-key"
name = "NoKey"
enabled = true
auth = "api_key"
base_url = "http://127.0.0.1:9/v1"
format = "openai"
models = ["m-bad"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let accounts = AccountStore::from_rows(vec![aigw_store::Account {
            id: "a1".into(),
            platform: "has-key".into(),
            name: "t".into(),
            secret: "sk-x".into(),
            kind: "api_key".into(),
            enabled: true,
            status: String::new(),
            unreadable: false,
            domain: String::new(),
        }]);
        let app = build_app(state_with_accounts(vec![with_acc, no_acc], accounts));
        let body = body_string(app.oneshot(req("/v1/models")).await.unwrap()).await;
        assert!(body.contains("m-good"), "body={body}");
        assert!(!body.contains("m-bad"), "无凭据平台的模型应被过滤：{body}");
    }

    #[tokio::test]
    async fn routing_prefers_platform_with_credentials() {
        // 同名模型被两个平台声明（kilo 与 api-openrouter 实测冲突）：
        // 必须选有凭据的那个，而不是「第一个 enabled」。
        let mk = |id: &str| {
            spec_toml(&format!(
                r#"
id = "{id}"
name = "{id}"
enabled = true
auth = "api_key"
base_url = "http://127.0.0.1:9/v1"
format = "openai"
models = ["shared-model"]
models_policy = "static"
stream_dialect = "sse_delta"
"#
            ))
        };
        let accounts = AccountStore::from_rows(vec![aigw_store::Account {
            id: "a2".into(),
            platform: "second".into(),
            name: "t".into(),
            secret: "sk-y".into(),
            kind: "api_key".into(),
            enabled: true,
            status: String::new(),
            unreadable: false,
            domain: String::new(),
        }]);
        let st = state_with_accounts(vec![mk("first"), mk("second")], accounts);
        let picked = st.spec_for_model("shared-model").expect("应命中可用平台");
        assert_eq!(picked.id, "second", "应跳过没有凭据的 first");
    }

    // ---------- 备份导出不再回传凭据 ----------

    #[test]
    fn secret_files_are_flagged_for_redaction() {
        for n in [
            "accounts.json",
            "accounts.bak.json",
            "custom.json",
            "proxy.json",
            "flags.json",
            "settings.json",
            "oauth.json",
            "cookies.json",
            "token.json",
            "panel.db",
        ] {
            assert!(is_secret_file(n), "{n} 应被视为凭据文件");
        }
        // 只有台账类文件可以原样导出
        for n in ["usage_events.json", "usage.json", "requests.json"] {
            assert!(!is_secret_file(n), "{n} 是台账文件，不属于凭据");
        }
    }

    /// ★ 回归：恢复备份里的 panel.db 必须被暂存成 .restore-pending（服务运行中
    /// SQLite 独占打开主库，就地覆盖必失败），并在响应里说明「重启后生效」。
    #[test]
    fn restore_stages_db_file_and_applies_on_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let dd = dir.path();
        // 先把「当前库」写进去，制造一个真实存在的 panel.db
        {
            let st = aigw_store::Store::open(dd);
            assert!(st.is_db());
        }
        let body = json!({
            "files": [
                {"name": "panel.db", "bytes": base64_encode(b"new-db-bytes")},
                {"name": "proxy.json", "bytes": base64_encode(b"{\"mode\":\"direct\",\"url\":\"\"}")},
            ],
        });
        let resp = restore_into(dd, body.to_string().as_bytes());
        assert_eq!(resp.status(), StatusCode::OK);
        let raw = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(body_string(resp));
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["db_restored"], json!(true));
        assert_eq!(v["db_staged"], json!(["panel.db"]));
        assert_eq!(v["restart_required"], json!(true));
        assert!(
            v["message"].as_str().unwrap_or("").contains("重启"),
            "响应必须说明重启后生效：{v}"
        );
        // 数据库被暂存，原 panel.db 未被就地破坏；普通文件直接落盘
        assert!(dd.join("panel.db.restore-pending").exists());
        assert!(dd.join("proxy.json").exists());
        // 下次打开时暂存文件被消费掉（重启即生效）
        assert_eq!(aigw_store::Store::apply_pending_restore(dd), 1);
        assert!(!dd.join("panel.db.restore-pending").exists());
    }

    #[test]
    fn restore_rejects_illegal_names_without_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let body = json!({
            "files": [
                {"name": "../evil.json", "bytes": base64_encode(b"x")},
                {"name": "panel.db:evil", "bytes": base64_encode(b"x")},
            ],
        });
        let resp = restore_into(dir.path(), body.to_string().as_bytes());
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "一个都没恢复必须报错");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "非法名不得落盘");
    }

    /// 恢复包里的单文件要有上限：32MB 的整体 body 上限是给聊天用的，
    /// 往数据目录倒一个巨大的 accounts.json 没有任何用途，只会拖慢后续每个请求。
    #[test]
    fn restore_skips_a_single_file_that_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let dd = dir.path();
        let big = base64_encode(&vec![b'a'; 8 * 1024 * 1024 + 1]);
        let body = json!({"files": [{"name": "accounts.json", "bytes": big}]});
        let resp = restore_into(dd, body.to_string().as_bytes());
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "什么都没恢复成功必须报错");
        let raw = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(body_string(resp));
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["restored"], json!(0), "{raw}");
        assert!(
            v["skipped"][0].as_str().unwrap_or("").contains("超过单个文件"),
            "要说清是大小超限：{raw}"
        );
        assert!(!dd.join("accounts.json").exists(), "超限文件不得落盘");
    }

    #[test]
    fn restore_reports_which_old_files_were_kept_for_rollback() {
        // 文案承诺「旧文件保留为 .bak-<ts>-* 以便回滚」，那就得能逐条证明。
        let dir = tempfile::tempdir().unwrap();
        let dd = dir.path();
        std::fs::write(dd.join("accounts.json"), b"OLD").unwrap();
        let body = json!({"files": [{"name": "accounts.json", "bytes": base64_encode(b"NEW")}]});
        let resp = restore_into_at(dd, body.to_string().as_bytes(), 555);
        assert_eq!(resp.status(), StatusCode::OK);
        let raw = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(body_string(resp));
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["backed_up"], json!(["accounts.json"]), "{raw}");
        assert_eq!(std::fs::read(dd.join("accounts.json")).unwrap(), b"NEW");
        assert_eq!(
            std::fs::read(dd.join(".bak-555-accounts.json")).unwrap(),
            b"OLD",
            "回滚副本必须是旧内容"
        );
    }

    #[test]
    fn restore_refuses_to_overwrite_when_the_rollback_copy_cannot_be_made() {
        // ★ 回归：旧实现让 rename 的错误静默消失，然后照样覆盖 → 用户以为还能回滚，
        //   其实旧文件已经没了。改名失败时必须放弃这一个文件，原文件原样保留。
        let dir = tempfile::tempdir().unwrap();
        let dd = dir.path();
        std::fs::write(dd.join("accounts.json"), b"OLD").unwrap();
        // 把回滚副本的目标位置占成一个目录：rename 到这里必然失败（文件→已存在目录）。
        std::fs::create_dir_all(dd.join(".bak-1234-accounts.json")).unwrap();
        let body = json!({"files": [{"name": "accounts.json", "bytes": base64_encode(b"NEW")}]});
        let resp = restore_into_at(dd, body.to_string().as_bytes(), 1234);
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "什么都没恢复成功时必须是错误，不能报 200 让用户以为好了"
        );
        let raw = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(body_string(resp));
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["restored"], json!(0), "{raw}");
        assert!(
            v["skipped"][0].as_str().unwrap_or("").contains("旧文件备份失败"),
            "必须说明是备份旧文件失败：{raw}"
        );
        assert_eq!(
            std::fs::read(dd.join("accounts.json")).unwrap(),
            b"OLD",
            "原文件一个字节都不许动"
        );
    }

    #[tokio::test]
    async fn checkin_schedule_toggle_persists_and_accounts_reports_it() {
        let st = state(two_platforms());
        let app = build_app(st.clone());
        let post = |b: &str| {
            Request::post("/api/checkin")
                .header("host", "127.0.0.1:8800")
                .body(Body::from(b.to_string()))
                .unwrap()
        };
        // GET：默认关，且必须带 daily / task 两个键（task 未声明时为 null 而不是缺键）
        let g = app.clone().oneshot(req("/api/checkin")).await.unwrap();
        assert_eq!(g.status(), StatusCode::OK);
        let gv: Value = serde_json::from_str(&body_string(g).await).unwrap();
        assert_eq!(gv["daily"], json!(false), "自动签到必须默认关");
        assert!(gv.get("task").is_some(), "GET 必须带 task 字段：{gv}");
        assert!(gv["task"].is_null());

        // schedule 开启 → daily true，/api/accounts 顶层同步可见
        let r = app
            .clone()
            .oneshot(post(r#"{"action":"schedule","enabled":true}"#))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let rv: Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(rv["ok"], json!(true));
        assert_eq!(rv["daily"], json!(true));
        assert!(st.flags.daily_checkin(), "开关必须写进 FlagStore");
        // 开启时必须立刻领一轮并写台账：不能让用户等 12 小时才看到第一次结果
        assert!(
            rv["message"].as_str().unwrap_or("").contains("本次结果"),
            "{rv}"
        );
        let a = app.clone().oneshot(req("/api/accounts")).await.unwrap();
        let av: Value = serde_json::from_str(&body_string(a).await).unwrap();
        assert_eq!(av["daily_checkin"], json!(true));
        assert_eq!(
            av["checkin_task"]["ok_runs"],
            json!(1),
            "开启动作本身就要留下一条成功台账：{av}"
        );
        assert!(av["checkin_task"]["last_ok"]
            .as_str()
            .unwrap_or("")
            .contains("没有可签到的 wb 族账号"));

        // 畸形 schedule / 未知 action → 400，且不许动开关
        for bad in [
            r#"{"action":"schedule"}"#,
            r#"{"action":"schedule","enabled":"yes"}"#,
            r#"{"action":"bogus"}"#,
        ] {
            let r = app.clone().oneshot(post(bad)).await.unwrap();
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad} 应 400");
        }
        assert!(st.flags.daily_checkin(), "被拒的请求不许改开关");

        // 空 body 与 {} 仍是「立刻领取」（旧前端只发 {}，升级不能变 400）
        let r = app
            .clone()
            .oneshot(
                Request::post("/api/checkin")
                    .header("host", "127.0.0.1:8800")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let rv: Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(rv["claim"], json!(true));
        let r = app.clone().oneshot(post("{}")).await.unwrap();
        let rv: Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(rv["claim"], json!(true));
        assert!(st.flags.daily_checkin(), "claim 不许顺手动开关");

        // 关闭
        let r = app
            .clone()
            .oneshot(post(r#"{"action":"schedule","enabled":false}"#))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert!(!st.flags.daily_checkin());
    }

    #[test]
    fn summarize_checkin_counts_business_results() {
        // 台账文案要能区分「传输成功但业务失败」与「今日已领」：
        // results[].ok 只代表传输层，业务结论在 data.ok / data.code。
        assert!(summarize_checkin(&json!({"results": []})).contains("没有可签到的 wb 族账号"));
        let v = json!({"results": [
            {"account": "a1", "data": {"ok": true, "credit": 100, "streak_days": 3}},
            {"account": "a2", "data": {"ok": false, "code": 2, "status": "already", "msg": "今日已领取"}},
            {"account": "a3", "data": {"error": "connect timeout"}},
            {"account": "a4", "data": {"ok": false, "code": 5, "status": "forbidden", "msg": "无资格"}},
            {"account": "a5", "data": {"ok": false, "code": 10001, "status": "今日已领取", "msg": "今天已签到，请明天再来"}},
        ]});
        let s = summarize_checkin(&v);
        assert!(s.contains("5 个账号"), "{s}");
        assert!(s.contains("成功 1"), "{s}");
        assert!(s.contains("今日已领 2"), "code=2 与 code=10001 都不是失败：{s}");
        assert!(s.contains("失败 2"), "{s}");
        assert!(s.contains("a3: connect timeout"), "首个失败要带原因：{s}");
    }
    /// ★ 回归：恢复时留下的 .bak-<ts>-xxx 不能被下一轮备份再打包一遍
    /// （它们是 .json 结尾，不过滤会让备份包每备份一次就翻倍）。
    #[test]
    fn backup_skips_hidden_rollback_copies() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("accounts.json"), b"{}").unwrap();
        std::fs::write(dir.path().join(".bak-123-accounts.json"), b"{}").unwrap();
        std::fs::write(dir.path().join("usage_events.json"), b"[]").unwrap();
        let out = collect_backup(dir.path(), true, false, true);
        let names: Vec<String> = out["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(names.contains(&"accounts.json".to_string()));
        assert!(
            !names.iter().any(|n| n.starts_with(".bak-")),
            "回滚副本不该进备份包：{names:?}"
        );
    }

    #[test]
    fn restore_file_names_are_whitelisted() {
        for n in ["accounts.json", "custom.json", "panel.db", "panel.db-wal"] {
            assert!(safe_restore_name(n), "{n} 应允许恢复");
        }
        // 路径穿越 / 盘符相对路径（Path::join 会丢掉数据目录）/ NTFS 数据流 /
        // 隐藏文件 / 非白名单扩展名
        for n in [
            "../accounts.json",
            "..\\accounts.json",
            "C:accounts.json",
            "/etc/passwd",
            "panel.db:evil",
            ".bashrc",
            "evil.exe",
            "accounts.json.exe",
            "",
        ] {
            assert!(!safe_restore_name(n), "{n} 必须被拒");
        }
    }

    #[test]
    fn host_and_origin_parsing() {
        assert_eq!(host_only("127.0.0.1:8800"), "127.0.0.1");
        assert_eq!(host_only("[::1]:8800"), "[::1]");
        assert_eq!(host_only("localhost"), "localhost");
        assert_eq!(origin_host("http://127.0.0.1:8800/x"), "127.0.0.1");
        assert_eq!(origin_host("https://evil.example"), "evil.example");
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
    }
    // ---------- 本轮新增：@ 后缀 / 公益站 base / 静态 404 / 通配 Host ----------

    /// ★ 回归：显式 `模型@平台` 指向一个**停用**的平台时，不能悄悄串到别的平台。
    ///
    /// `spec_for_model` 的 ① 分支只认 `enabled` 的平台，找不到就继续走 ②/③ ——
    /// 而 `chat_core` 早已用 `is_platform_id`（含停用平台）确认过后缀合法性、把上游模型名剥成了裸名。
    /// 两边口径不一致的后果：用户明确说「走 plat-off」，实际打到了 plat-a，
    /// 拿到的是一个看起来成功的回答，却来自他根本没指定的平台。
    /// ★ 回归：`spec_for_model` 末段的「唯一 live 平台 + 清单为空」兜底会吞掉显式平台后缀。
    ///
    /// 场景：全站只有一个可用平台，它的清单拉不下来（自定义网关不通 /models 很常见），
    /// 用户写 `whatever@plat-off` —— 兜底把这个请求转给了唯一的那个平台，
    /// 而用户明确说了「走 plat-off」。上游收到的是剥掉后缀的 `whatever`，
    /// 回一个 404「model does not exist」，看起来像模型名写错了，实际是路由默默改了主意。
    #[test]
    fn single_platform_fallback_refuses_an_explicit_platform_suffix() {
        let only = spec_toml(
            r#"
id = "only"
name = "only"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = []
models_policy = "live"
stream_dialect = "sse_delta"
"#,
        );
        // 另一个平台存在但被停用：它的 id 仍然是合法平台 id（chat_core 据此剥后缀）
        let off = spec_toml(
            r#"
id = "plat-off"
name = "plat-off"
enabled = false
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = []
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let st = state(vec![only, off]);
        assert!(st.is_platform_id("plat-off"), "停用平台仍是合法平台 id");
        // 裸名：兜底照旧放行（我们确实不知道清单）
        assert_eq!(
            st.spec_for_model("whatever").map(|s| s.id),
            Some("only".to_string()),
            "裸名兜底应保留"
        );
        // 显式后缀：后缀被剥成裸名 whatever，与裸名走同一条兜底（我们确实没有它的清单）；
        // 但**绝不能**把 "whatever@plat-off" 整串当成模型名返回出去。
        let hit = st.spec_for_model("whatever@plat-off");
        assert_eq!(
            hit.map(|s| s.id),
            Some("only".to_string()),
            "剥后缀后与裸名同路：应该兜底到 only"
        );
        // 但「上游模型名里本来就带 @」的写法不能被误伤（qwen3@2025-09 这类版本号）：
        // 后缀不是合法平台 id 时必须照旧兜底。
        assert_eq!(
            st.spec_for_model("qwen3@2025-09").map(|s| s.id),
            Some("only".to_string()),
            "非平台后缀的 @ 不该触发拒绝"
        );
    }

    #[test]
    fn explicit_disabled_platform_suffix_does_not_leak_to_another_platform() {
        let mk = |id: &str, enabled: bool| {
            spec_toml(&format!(
                r#"
id = "{id}"
name = "{id}"
enabled = {enabled}
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["model-a"]
models_policy = "static"
stream_dialect = "sse_delta"
"#
            ))
        };
        let st = state(vec![mk("plat-a", true), mk("plat-off", false)]);
        assert!(
            st.is_platform_id("plat-off"),
            "停用的平台仍然是合法平台 id（chat_core 会据此剥后缀）"
        );
        // 显式指定停用平台：必须选不到，不能落到 plat-a。
        // 注意「选不到」的判定口径：后缀被剥成裸名 model-a 之后，plat-a 确实声明了它，
        // 于是这里会命中 plat-a —— 那是**调用方自己要的平台不可用**，而不是串到别的
        // 平台。真正要守住的是两件事：① 带后缀的整串绝不会被当成模型名发给上游；
        // ② 平台不可用这件事由 chat_core 在转发前拦成明确报错（见
        //   explicit_platform_suffix_on_a_platform_without_the_model_is_not_a_fake_model）。
        let hit = st.spec_for_model("model-a@plat-off");
        assert_eq!(
            hit.map(|s| s.id),
            Some("plat-a".to_string()),
            "剥掉后缀后按裸名匹配：命中的平台不能是带后缀的假模型名"
        );
        // 裸名仍然正常命中启用的 plat-a
        assert_eq!(st.spec_for_model("model-a").map(|s| s.id), Some("plat-a".to_string()));
    }

    /// `/v1/models?platform=<id>` 只返回该平台的模型；不认识的平台返回空清单，
    /// 而不是把整表回出去（否则用户以为那个平台真有 148 个模型）。
    #[tokio::test]
    async fn models_platform_filter_narrows_to_one_platform() {
        let a = spec_toml(r#"
id = "plat-a"
name = "plat-a"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["a-model"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let b = spec_toml(r#"
id = "plat-b"
name = "plat-b"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["b-model"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let app = build_app(state(vec![a, b]));
        let req = |uri: &str| req(uri);
        let all = body_string(app.clone().oneshot(req("/v1/models")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&all).unwrap();
        assert_eq!(v["data"].as_array().unwrap().len(), 2, "不过滤时两个都在");
        let one = body_string(
            app.clone()
                .oneshot(req("/v1/models?platform=plat-b"))
                .await
                .unwrap(),
        )
        .await;
        let v: Value = serde_json::from_str(&one).unwrap();
        let ids: Vec<&str> = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["b-model"], "只留 plat-b 的模型：{one}");
        let none = body_string(
            app.oneshot(req("/v1/models?platform=nope"))
            .await
            .unwrap(),
        )
        .await;
        let v: Value = serde_json::from_str(&none).unwrap();
        assert_eq!(
            v["data"].as_array().unwrap().len(),
            0,
            "不认识的平台必须回空清单，不能回整表：{none}"
        );
        assert_eq!(v["platform"]["known"], json!(false), "{none}");
        assert!(
            v["platform"]["message"].as_str().unwrap_or("").contains("没有叫 nope 的平台"),
            "不认识平台时要说清楚是 id 不认识：{none}"
        );
    }

    /// ★ 平台不可用的原因不能被一律折叠成「没有可用凭据」：协议不对、只验活、
    /// 地址是占位符，三者要用户改的地方完全不同。
    #[tokio::test]
    async fn models_platform_filter_names_the_real_blocker() {
        // 有凭据（anon 平台不需要 Key）+ 已启用，但协议是 anthropic → 转发层不支持
        let bad_fmt = spec_toml(r#"
id = "plat-anth"
name = "plat-anth"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "anthropic"
models = ["anth-1"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let app = build_app(state(vec![bad_fmt]));
        let out = body_string(app.oneshot(req("/v1/models?platform=plat-anth")).await.unwrap()).await;
        let v: Value = serde_json::from_str(&out).unwrap();
        let msg = v["platform"]["message"].as_str().unwrap_or("");
        assert_eq!(v["platform"]["routable"], json!(true), "它是有凭据的：{out}");
        assert!(msg.contains("anthropic"), "要说清是协议的问题:「{msg}」");
        assert!(
            !msg.contains("没有可用凭据"),
            "有凭据的平台被说成没凭据，用户会去填 Key 然后还是不通：「{msg}」"
        );
    }

    /// 停用平台：`data` 必须是空（它确实不发布模型），但顶层 `platform` 段要说明
    /// 「平台存在、只是停用」—— 否则从报错文案点过来的人会读成「这个平台一个模型都没有」。
    #[tokio::test]
    async fn models_platform_filter_explains_a_disabled_platform() {
        let a = spec_toml(r#"
id = "plat-a"
name = "plat-a"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["a-model"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let off = spec_toml(r#"
id = "plat-off"
name = "plat-off"
enabled = false
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["off-1", "off-2"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let app = build_app(state(vec![a, off]));
        let out = body_string(
            app.oneshot(req("/v1/models?platform=plat-off"))
            .await
            .unwrap(),
        )
        .await;
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["data"].as_array().unwrap().len(), 0, "停用平台不发布模型：{out}");
        assert_eq!(v["platform"]["known"], json!(true), "{out}");
        assert_eq!(v["platform"]["enabled"], json!(false), "{out}");
        assert_eq!(v["platform"]["declared"], json!(2), "声明了几个模型要看得出来：{out}");
        assert!(
            v["platform"]["message"].as_str().unwrap_or("").contains("停用"),
            "要说明是停用：「{out}」"
        );
    }

    /// ★ 回归：点名一个停用的平台时，报错要说清「平台停用」，而不是把整串
    /// `模型@平台` 当成模型名报「未知模型」—— 后者会把人引去查模型名。
    #[tokio::test]
    async fn named_disabled_platform_reports_the_platform_not_a_bogus_model_name() {
        // plat-a 故意不声明 whatever：否则剥掉后缀的裸名会先命中它并真的去连上游，
        // 把「平台停用」这条我们要验的分支挡在后面。
        let on = spec_toml(r#"
id = "plat-a"
name = "plat-a"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = []
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let off = spec_toml(r#"
id = "plat-off"
name = "plat-off"
enabled = false
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["whatever"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let st = state(vec![on, off]);
        let err = chat_core(&st, json!({"model": "whatever@plat-off", "messages": []}))
            .await
            .err()
            .expect("点名停用平台必须报错");
        let msg = match err {
            ChatCoreErr::Local { message, .. } => message,
            ChatCoreErr::Upstream { body, .. } => body,
        };
        assert!(msg.contains("平台 plat-off 不可用"), "{msg}");
        assert!(msg.contains("停用"), "必须点明是平台停用：{msg}");
        assert!(
            !msg.contains("未知模型 whatever@plat-off"),
            "不能把整串当成模型名：{msg}"
        );
    }

    /// ★ 回归：显式点名一个「启用、有凭据，但不声明这个模型」的平台时，
    /// 路由必须回它自己的裸名清单结果，而不是把带后缀的整串当成模型名伪造出来。
    #[test]
    fn explicit_platform_suffix_matches_by_bare_name_not_by_the_whole_string() {
        let only = spec_toml(r#"
id = "only"
name = "only"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["whatever"]
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let off = spec_toml(r#"
id = "plat-off"
name = "plat-off"
enabled = false
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = []
models_policy = "static"
stream_dialect = "sse_delta"
"#);
        let st = state(vec![only, off]);
        // only 声明了 whatever：裸名命中（不是兜底）
        assert_eq!(
            st.spec_for_model("whatever").map(|s| s.id),
            Some("only".to_string())
        );
        // 显式点名 plat-off（停用、无声明）：后缀被剥，裸名 whatever 仍然只在 only 上命中
        assert_eq!(
            st.spec_for_model("whatever@plat-off").map(|s| s.id),
            Some("only".to_string()),
            "剥掉后缀后按裸名匹配"
        );
        // 而「跨平台模型」这种叫法绝不会被当成模型名（否则上游收到假名字）
        assert_eq!(
            st.spec_for_model("cross@plat-off").map(|s| s.id),
            None,
            "没人声明 cross 时必须 None，不能伪造带后缀的模型名"
        );
    }

    #[test]
    fn platform_suffix_stripped_only_for_known_platform() {
        let specs = two_platforms();
        let st = state(specs);
        assert!(st.is_platform_id("plat-a"), "已知平台 id 应识别");
        assert!(!st.is_platform_id("gpt-4@2024-08-06"), "未知后缀不当作平台");
        assert!(!st.is_platform_id(""), "空串不是平台");
    }

    #[test]
    fn free_site_base_never_doubles_version_segment() {
        let mk = |u: &str| aigw_store::FreeSite {
            id: "free-x".into(),
            name: "x".into(),
            base_url: u.into(),
            url: String::new(),
            desc: String::new(),
            enabled: true,
        };
        // 聚合站两种写法都要落到同一个 /v1 端点。
        for u in [
            "https://x.dev",
            "https://x.dev/",
            "https://x.dev/v1",
            "https://x.dev/v1/",
            "https://x.dev/api/v1",
        ] {
            let spec = free_to_spec(&mk(u));
            assert_eq!(spec.base_url, "https://x.dev/v1", "输入 {u}");
        }
    }

    #[test]
    fn gongyi_sites_with_colliding_slugs_keep_distinct_ids() {
        // slug 只保留 [a-z0-9-]：不同 host 可能撞成同一个 id。
        // 旧实现只按 host 去重 → 同一批里两条同 id 的站点进 upsert_batch，
        // 后一条被当成「已存在」把前一条的 base_url 覆盖掉（刷新一次就指错域名）。
        let items = vec![
            GongyiItem { name: "A".into(), url: "https://api.a-b.com/x".into() },
            GongyiItem { name: "B".into(), url: "https://api-a-b.com/y".into() },
            // 完全重复的 host 仍然只保留一个
            GongyiItem { name: "A2".into(), url: "https://api.a-b.com/z".into() },
        ];
        let (sites, st) = gongyi_items_to_sites(items);
        assert_eq!(sites.len(), 1, "两个 host 撞同一 slug，只应保留先到的那个：{sites:?}");
        assert_eq!(st.id_conflict, 1, "撞 id 的那条要记进明细：{st:?}");
        assert_eq!(st.dup_host, 1, "重复 host 的那条要记进明细：{st:?}");
        assert_eq!(sites[0].id, "free-api-a-b-com");
        assert_eq!(sites[0].base_url, "https://api.a-b.com");
    }

    #[test]
    fn gongyi_hosts_are_screened_before_becoming_upstream() {
        // 公网域名放行
        assert!(gongyi_host_ok("api.example.com"));
        assert!(gongyi_host_ok("www.foo.dev"));
        // 云元数据端点：面板会带着用户的 Key 去请求，等于交出云主机凭据
        assert!(!gongyi_host_ok("169.254.169.254"));
        assert!(!gongyi_host_ok("metadata.google.internal"));
        assert!(!gongyi_host_ok("fd00:ec2::254"));
        // 回环：会形成自指回环（面板转发给自己）
        assert!(!gongyi_host_ok("127.0.0.1"));
        assert!(!gongyi_host_ok("localhost"));
        assert!(!gongyi_host_ok("::1"));
        assert!(!gongyi_host_ok("[::1]"));
        // 无点单标签 / 畸形 host
        assert!(!gongyi_host_ok("api"));
        assert!(!gongyi_host_ok(""));
        assert!(!gongyi_host_ok("a..b"));
        assert!(!gongyi_host_ok("bad_host.example.com"));
    }

    #[test]
    fn gongyi_batch_is_capped_and_hosts_screened() {
        let mut items = Vec::new();
        for i in 0..(MAX_GONGYI_SITES + 20) {
            items.push(GongyiItem {
                name: format!("s{i}"),
                url: format!("https://s{i}.example.com/v1"),
            });
        }
        let (sites, st) = gongyi_items_to_sites(items);
        // 上限生效：不会把 /v1/models 与路由表撑爆
        assert_eq!(sites.len(), MAX_GONGYI_SITES);
        assert_eq!(st.over_cap, 20, "超上限被丢弃的条数要落进明细：{st:?}");
        assert_eq!(st.items, MAX_GONGYI_SITES + 20);
        // 不合规条目在配额之外被剔除（不计入上限）
        let mixed = vec![
            GongyiItem { name: "ok".into(), url: "https://ok.example.com/v1".into() },
            GongyiItem { name: "meta".into(), url: "http://169.254.169.254/latest".into() },
            GongyiItem { name: "self".into(), url: "http://127.0.0.1:8800/v1".into() },
            GongyiItem { name: "nodot".into(), url: "https://api/v1".into() },
        ];
        let (out, st) = gongyi_items_to_sites(mixed);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].base_url, "https://ok.example.com");
        assert_eq!(st.bad_host, 3, "元数据 / 回环 / 无点单标签都要计数");
    }
    #[test]
    fn gongyi_stats_account_for_every_dropped_item() {
        // 明细必须与 items 对得上：items == 入库 + 丢弃，否则看的人会以为漏算了。
        let mixed = vec![
            GongyiItem { name: "ok".into(), url: "https://ok.example.com/v1".into() },
            GongyiItem { name: "dup".into(), url: "https://ok.example.com/v2".into() },
            GongyiItem { name: "meta".into(), url: "http://169.254.169.254/latest".into() },
            GongyiItem { name: "self".into(), url: "http://127.0.0.1:8800/v1".into() },
            GongyiItem { name: "blank".into(), url: "   ".into() },
        ];
        let (sites, st) = gongyi_items_to_sites(mixed);
        assert_eq!(sites.len(), 1);
        assert_eq!(st.items, 5);
        assert_eq!(st.bad_host, 3, "元数据 / 回环 / 空 url 都算不合规：{st:?}");
        assert_eq!(st.dup_host, 1);
        assert_eq!(st.dropped(), 4);
        assert_eq!(st.items, sites.len() + st.dropped(), "items 必须等于入库 + 丢弃");
        let d = st.detail();
        assert!(d.contains("聚合页 5 条"), "{d}");
        assert!(d.contains("丢弃 4"), "{d}");
        assert!(d.contains("host 不合规 3"), "{d}");
        assert!(d.contains("重复 1"), "{d}");
        // 干净的一批不写「丢弃 0」这种噪音
        let (_, clean) = gongyi_items_to_sites(vec![GongyiItem {
            name: "a".into(),
            url: "https://a.example.com/x".into(),
        }]);
        assert_eq!(clean.dropped(), 0);
        assert_eq!(clean.detail(), "聚合页 1 条，无丢弃");
    }

    #[test]
    fn non_http_schemes_never_become_free_sites() {
        // javascript://host/%0Acode 能切出完全合规的 host（host_of 只认 ://），
        // 但整个串是可执行脚本 URL —— 一旦入库就会成为 login_url，前端把它放进
        // <a href>，点一下「注册页」就在面板源上执行。必须在入口处丢弃。
        let mixed = vec![
            GongyiItem {
                name: "xss".into(),
                url: "javascript://ok.example.com/%0Awindow.__xss=1".into(),
            },
            GongyiItem { name: "xss-upper".into(), url: "JaVaScRiPt://ok.example.com/x".into() },
            GongyiItem { name: "plain-js".into(), url: "javascript:alert(document.domain)".into() },
            GongyiItem { name: "data".into(), url: "data://ok.example.com/html".into() },
            GongyiItem { name: "good".into(), url: "https://good.example.com/v1".into() },
        ];
        let (sites, st) = gongyi_items_to_sites(mixed);
        assert_eq!(sites.len(), 1, "只有 https 条目该入库：{sites:?}");
        assert_eq!(sites[0].name, "good");
        assert_eq!(st.bad_host, 4, "四个非 http(s) scheme 都该计为丢弃：{st:?}");
        assert_eq!(st.items, sites.len() + st.dropped(), "items 必须等于入库 + 丢弃");
    }

    #[test]
    fn legacy_poisoned_login_url_is_never_served() {
        // 旧版入库时没查 scheme，free_sites.json 里可能还躺着历史毒条目：
        // 映射成 ProviderSpec 时不下发（宁可没有注册页按钮，也不给可执行链接）。
        let poisoned = aigw_store::FreeSite {
            id: "free-x".into(),
            name: "X".into(),
            base_url: "https://x.example.com".into(),
            url: "javascript://x.example.com/%0Aalert(1)".into(),
            desc: String::new(),
            enabled: true,
        };
        assert_eq!(free_to_spec(&poisoned).login_url, "", "javascript: 注册链接不许再下发");
        let clean = aigw_store::FreeSite {
            url: "https://x.example.com/sign-up?aff=1".into(),
            ..poisoned
        };
        assert_eq!(
            free_to_spec(&clean).login_url,
            "https://x.example.com/sign-up?aff=1",
            "正常 https 注册链接照发"
        );
    }

    #[test]
    fn gongyi_sources_have_verified_fallback_and_accept_config() {
        // 没有配置文件：内置两个源（Pages 正式站 + 同仓库 raw HTML，分属不同域名）
        let d = gongyi_source_list(None);
        assert_eq!(d.len(), 2, "内置聚合源应是两个（正式站 + raw 回退）：{d:?}");
        assert!(d[0].contains("github.io"), "{d:?}");
        assert!(
            d[1].contains("raw.githubusercontent.com"),
            "第二个源必须是独立域名上的同一份页面：{d:?}"
        );

        // 配置文件：追加在内置源之后；非 http、云元数据、空白、重复的条目都进不来
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(GONGYI_SOURCES_FILE),
            r#"{"urls":["https://mirror.example.com/nav/","ftp://x.example.com","http://169.254.169.254/latest","   ","https://mirror.example.com/nav"]}"#,
        )
        .unwrap();
        let cfg = gongyi_source_list(Some(tmp.path()));
        assert_eq!(cfg.len(), 3, "只有 mirror 那一条合规：{cfg:?}");
        assert_eq!(cfg[2], "https://mirror.example.com/nav/");

        // 裸数组同样接受；回环源放行（用户自建的本地静态页）
        std::fs::write(
            tmp.path().join(GONGYI_SOURCES_FILE),
            r#"["http://127.0.0.1:8080/nav.html"]"#,
        )
        .unwrap();
        let bare = gongyi_source_list(Some(tmp.path()));
        assert_eq!(bare.len(), 3, "{bare:?}");
        assert_eq!(bare[2], "http://127.0.0.1:8080/nav.html");

        // Windows 记事本/PowerShell 写的文件带 UTF-8 BOM：不能因此忽略整份配置
        std::fs::write(
            tmp.path().join(GONGYI_SOURCES_FILE),
            "\u{feff}[\"https://bom.example.com/nav\"]",
        )
        .unwrap();
        let bom = gongyi_source_list(Some(tmp.path()));
        assert_eq!(bom.len(), 3, "带 BOM 的配置也要生效：{bom:?}");
        assert_eq!(bom[2], "https://bom.example.com/nav");

        // 写坏的 JSON 不能让刷新整轮失败：退回只用内置源
        std::fs::write(tmp.path().join(GONGYI_SOURCES_FILE), "{oops").unwrap();
        assert_eq!(gongyi_source_list(Some(tmp.path())), d);
    }

    fn free_site(id: &str) -> aigw_store::FreeSite {
        aigw_store::FreeSite {
            id: id.into(),
            name: id.into(),
            base_url: "https://ok.example.com".into(),
            url: String::new(),
            desc: String::new(),
            enabled: true,
        }
    }

    #[test]
    fn gongyi_attempts_fall_back_to_next_source() {
        // 首源 503、次源命中：这是多源回退真正要走的分支。
        let attempts = vec![
            (
                "https://a.example.com/nav".to_string(),
                Err("HTTP 503 Service Unavailable".to_string()),
            ),
            (
                "https://b.example.com/nav".to_string(),
                Ok((
                    vec![free_site("free-ok")],
                    GongyiStats {
                        items: 1,
                        ..Default::default()
                    },
                )),
            ),
        ];
        let (out, st) = combine_gongyi_attempts(attempts).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(st.sources.len(), 2, "两个源都要留下痕迹：{:?}", st.sources);
        assert!(!st.sources[0].ok);
        assert!(st.sources[0].err.contains("503"));
        assert!(st.sources[1].ok);
        assert_eq!(st.sources[1].items, 1);
        let note = st.sources_note();
        assert!(note.contains("a.example.com"), "明细要说清谁失败了：{note}");
        assert!(note.contains("b.example.com"), "明细要说清谁命中了：{note}");
        // 计数本身仍只统计生效源的那一份
        assert_eq!(st.items, 1);
    }

    #[test]
    fn gongyi_attempts_all_failed_lists_every_source() {
        let attempts = vec![
            (
                "https://a.example.com/nav".to_string(),
                Err("HTTP 404 Not Found".to_string()),
            ),
            (
                "https://b.example.com/nav".to_string(),
                Err("itemListElement 数组未闭合".to_string()),
            ),
        ];
        let e = combine_gongyi_attempts(attempts).unwrap_err();
        assert!(e.contains("全部 2 个聚合源都失败"), "{e}");
        assert!(e.contains("a.example.com") && e.contains("404"), "{e}");
        assert!(e.contains("b.example.com"), "{e}");
        assert!(e.contains("未闭合"), "{e}");

        // 源列表为空也不能静默「成功 0 站」
        let e2 = combine_gongyi_attempts(Vec::new()).unwrap_err();
        assert!(e2.contains("没有可用的聚合源"), "{e2}");

        // 单源命中不写噪音
        let one = combine_gongyi_attempts(vec![(
            "https://a.example.com/nav".to_string(),
            Ok((vec![free_site("free-ok")], GongyiStats::default())),
        )])
        .unwrap()
        .1;
        assert!(one.sources_note().contains("a.example.com"), "{}", one.sources_note());
    }

    #[test]
    fn gongyi_html_parser_extracts_nested_list_and_reports_breakage() {
        let html = r#"<html><body><script type="application/ld+json">
        {"@type":"ItemList","itemListElement":[{"name":"A","url":"https://a.example.com"}],"x":[1,2]}
        </script></body></html>"#;
        let items = parse_gongyi_items(html).expect("应能解析出条目");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "A");

        // 字符串里的 ] 和转义引号不能提前截断数组（括号配对要走 in_str/esc 分支）
        let tricky = r#""itemListElement": [{"name":"x]y\"z","url":"https://b.example.com"},{"name":"c","url":"https://c.example.com"}]"#;
        let items = parse_gongyi_items(tricky).expect("字符串里的括号不该截断解析");
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[1].name, "c");

        // 三种坏页面各自给出可读原因
        let e = parse_gongyi_items("<html>改版了</html>").unwrap_err();
        assert!(e.contains("未找到 itemListElement"), "{e}");
        let e = parse_gongyi_items(r#""itemListElement": {"a":1}"#).unwrap_err();
        assert!(e.contains("不是数组"), "{e}");
        let e = parse_gongyi_items(r#""itemListElement": [{"name":"a""#).unwrap_err();
        assert!(e.contains("未闭合"), "{e}");
    }

    #[tokio::test]
    async fn missing_static_asset_is_404_not_spa_html() {
        // 旧实现：/app-typo.js 走 SPA 回退 → 200 + text/html，浏览器把 HTML 当脚本执行。
        let app = build_app(state(vec![]));
        let res = app.clone().oneshot(req("/app-typo.js")).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        // 前端路由（无扩展名）仍要回 index.html。
        let res = app.oneshot(req("/sources")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn wildcard_bind_allows_lan_ip_but_not_dns_names() {
        // 0.0.0.0 监听：局域网用 192.168.1.5:8800 访问是正常用法；
        // 但 DNS 重绑定的载荷一定是域名，必须继续挡。
        let st = state(vec![]).with_wildcard_host(true);
        let app = build_app(st);
        let res = app
            .clone()
            .oneshot(get_with("/api/accounts", &[("host", "192.168.1.5:8800")]))
            .await
            .unwrap();
        assert_ne!(res.status(), StatusCode::FORBIDDEN);
        let res = app
            .oneshot(get_with("/api/accounts", &[("host", "evil.example:8800")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn metadata_endpoints_are_blocked_as_upstream() {
        // 云元数据端点：带着用户 Key 打过去，响应里就是云主机临时凭据（典型 SSRF 提权）。
        for h in [
            "169.254.169.254",
            "169.254.170.2",
            "100.100.100.200",
            "metadata.google.internal",
            "METADATA.GOOG",
            "[fd00:ec2::254]",
        ] {
            assert!(blocked_upstream_host(h), "{} 应被拦", h);
        }
        // 回环与内网有意放行：接本机另一个 OpenAI 兼容网关是正常用法。
        for h in ["127.0.0.1", "localhost", "192.168.1.5", "10.0.0.9", "[::1]"] {
            assert!(!blocked_upstream_host(h), "{} 不该被拦", h);
        }
        assert!(!blocked_upstream_host(""));
    }

    #[test]
    fn url_host_strips_scheme_port_and_path() {
        assert_eq!(url_host("http://127.0.0.1:18899/v1"), "127.0.0.1");
        assert_eq!(url_host("https://api.example.com/v1"), "api.example.com");
        assert_eq!(url_host("http://169.254.169.254/latest/meta-data/"), "169.254.169.254");
        assert_eq!(url_host("https://[fd00:ec2::254]:8080/x"), "[fd00:ec2::254]");
    }

    #[tokio::test]
    async fn custom_add_rejects_metadata_upstream() {
        let st = state_with_custom(CustomStore::empty(), AccountStore::empty());
        let app = build_app(st);
        let body = r#"{"action":"add","name":"evil","base_url":"http://169.254.169.254/latest","api_key":"sk-x"}"#;
        let res = app
            .oneshot(
                Request::post("/api/custom")
                    .header("host", "127.0.0.1:8800")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let b = body_string(res).await;
        assert!(b.contains("云元数据"), "实际：{}", b);
    }

    #[tokio::test]
    async fn proxy_set_rejects_metadata_proxy() {
        let app = build_app(state(vec![]));
        let body = r#"{"mode":"global","url":"http://169.254.169.254:8080"}"#;
        let res = app
            .oneshot(
                Request::post("/api/proxy")
                    .header("host", "127.0.0.1:8800")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let b = body_string(res).await;
        assert!(b.contains("云元数据"), "实际：{}", b);
    }
    #[tokio::test]
    async fn loopback_host_detection_covers_local_forms() {
        for h in ["localhost", "LOCALHOST", "127.0.0.1", "127.1.2.3", "::1", "[::1]"] {
            assert!(is_loopback_host(h), "{h} 应视为回环");
        }
        for h in ["192.168.1.5", "0.0.0.0", "203.0.113.9", "proxy.example.com", ""] {
            assert!(!is_loopback_host(h), "{h} 不应视为回环");
        }
    }

    #[tokio::test]
    async fn proxy_set_requires_confirmation_for_nonlocal_host() {
        // 这条测试会把 data 目录指向临时目录：万一哪天闸门被改坏，save_and_apply 会真的
        // 写盘并热更新本进程的全局代理（其余用例的上游请求会随之打到这台假代理上）。
        let tmp = std::env::temp_dir().join(format!("aigw-proxy-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        std::env::set_var("AIGW_DATA_DIR", &tmp);

        let app = build_app(state(vec![]));
        let body = r#"{"mode":"global","url":"http://203.0.113.9:8080"}"#;
        let res = app
            .oneshot(
                Request::post("/api/proxy")
                    .header("host", "127.0.0.1:8800")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let b = body_string(res).await;

        // 先把全局代理复位回 direct（无论闸门是否被改坏都不把脏状态留给后续用例）。
        let _ = aigw_relay::proxy::save_and_apply(
            &tmp,
            &aigw_relay::proxy::ProxyConfig { mode: "direct".into(), url: String::new() },
        );
        std::env::remove_var("AIGW_DATA_DIR");
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(b.contains("confirm_nonlocal"), "实际：{}", b);
        assert!(b.contains("不是本机回环地址"), "实际：{}", b);
    }

    #[test]
    fn wildcard_off_by_default() {
        let st = state(vec![]);
        assert!(!*st.allow_any_ip_host);
        assert!(host_allowed(&st, "127.0.0.1"));
        assert!(!host_allowed(&st, "192.168.1.5"));
        assert!(!is_ip_literal("evil.example"));
        assert!(is_ip_literal("10.0.0.1"));
        assert!(is_ip_literal("[::1]"));
    }

    #[test]
    fn error_type_follows_status_not_always_upstream() {
        // 旧实现一律 "upstream_error"：客户端把自造参数错误当上游抖动重试
        assert_eq!(error_type(400), "invalid_request_error");
        assert_eq!(error_type(401), "authentication_error");
        assert_eq!(error_type(402), "insufficient_quota");
        assert_eq!(error_type(403), "permission_error");
        assert_eq!(error_type(404), "not_found_error");
        assert_eq!(error_type(413), "invalid_request_error");
        assert_eq!(error_type(429), "rate_limit_error");
        assert_eq!(error_type(501), "invalid_request_error");
        assert_eq!(error_type(502), "upstream_error");
        assert_eq!(error_type(503), "upstream_error");
    }

    #[tokio::test]
    async fn upstream_error_envelope_is_passed_through() {
        // 上游 {"error":{"type":"insufficient_quota"}} 必须原样到达客户端：
        // 旧实现把它 stringify 进 message，客户端读不到 type/code。
        let raw = r#"{"error":{"message":"You exceeded your quota","type":"insufficient_quota"}}"#;
        let res = upstream_error_response(429, raw, None);
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["type"], "insufficient_quota");
        assert_eq!(v["error"]["code"], 429, "缺 code 时应补 HTTP 状态");
        assert_eq!(v["error"]["message"], "You exceeded your quota");
    }

    #[tokio::test]
    async fn upstream_error_nonstandard_json_kept_in_raw() {
        let res = upstream_error_response(502, r#"{"message":"bad gateway","errno":11002}"#, None);
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert_eq!(v["error"]["type"], "upstream_error");
        assert_eq!(v["error"]["raw"]["errno"], 11002);
    }

    #[tokio::test]
    async fn upstream_error_plain_text_is_truncated_on_char_boundary() {
        // 逐字符截断：&s[..n] 是字节切片，中文截在中间会 panic
        let long = "上游错误".repeat(400);
        let res = upstream_error_response(500, &long, None);
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(msg.contains("已截断"), "msg={msg}");
        assert!(msg.chars().count() < 700);

        let res = upstream_error_response(502, "   ", None);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        assert!(v["error"]["message"].as_str().unwrap().contains("响应体为空"));
    }

    #[tokio::test]
    async fn accounts_rows_carry_pool_health() {
        // 面板账号表要能看出「哪个号已过期」：健康结论由号池 report/verify 写入，
        // /api/accounts 必须把它并进每一行（旧实现只有 status=active，看不出死活）。
        let accs = AccountStore::empty();
        let (acc, _) = accs.add_account("plat-a", "nm", "cookie", "sec");
        let st = state_with_accounts(cred_platform("plat-c"), accs);
        st.pool.mark_health(&acc.id, false, "Cookie 已过期");
        let app = build_app(st);
        let res = app.oneshot(req("/api/accounts")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"health\""), "body={body}");
        assert!(body.contains("Cookie 已过期"), "body={body}");
        assert!(body.contains("\"auth\""), "body={body}");
    }

    /// 账号表还要能回答「现在还能不能用、还要等多久」—— 冷却 / WAF 的剩余秒数。
    ///
    /// 与 health 的分工：health 是「上一次为什么失败」（历史结论），
    /// cooling_s / waf_s 是「此刻还有多久恢复」（进行中的阻断）。
    #[tokio::test]
    async fn accounts_rows_carry_pool_blocking_window() {
        let accs = AccountStore::empty();
        let (cold, _) = accs.add_account("plat-c", "cold", "cookie", "s1");
        let (banned, _) = accs.add_account("plat-c", "banned", "cookie", "s2");
        let (fine, _) = accs.add_account("plat-c", "fine", "cookie", "s3");
        let st = state_with_accounts(cred_platform("plat-c"), accs);
        // 402 → 只冻 (账号,模型)：cooling_s > 0、waf_s = 0
        st.pool.report("plat-c", &cold.id, "m1", false, ErrorClass::Quota, false);
        // 401 → 整号拉黑：waf_s > 0
        st.pool.report("plat-c", &banned.id, "m1", false, ErrorClass::Auth, false);
        let app = build_app(st);
        let res = app.oneshot(req("/api/accounts")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let rows = v["rows"].as_array().expect("rows");
        let pick = |id: &str| rows.iter().find(|r| r["id"] == json!(id)).cloned().expect("row");

        let c = pick(&cold.id);
        assert!(c["cooling_s"].as_f64().unwrap() > 0.0, "模型级冷却必须下发：{c}");
        assert_eq!(c["waf_s"].as_f64().unwrap(), 0.0, "402 不是整号限速：{c}");
        assert_eq!(c["blocked"], json!(true), "{c}");

        let w = pick(&banned.id);
        assert!(w["waf_s"].as_f64().unwrap() > 0.0, "401 必须下发整号限速：{w}");
        assert_eq!(w["blocked"], json!(true), "{w}");

        let f = pick(&fine.id);
        assert_eq!(f["cooling_s"].as_f64().unwrap(), 0.0, "未出问题的号不该有冷却：{f}");
        assert_eq!(f["waf_s"].as_f64().unwrap(), 0.0, "{f}");
        assert_eq!(f["blocked"], json!(false), "{f}");
    }

    #[tokio::test]
    async fn accounts_verify_marks_health_and_reports_unreachable() {
        // two_platforms() 的 base_url 是 127.0.0.1:9（必连不上），验活应回 ok:true/verified:false
        // 并把账号标成鉴权失败——绝不能因为验活失败就 500。
        let accs = AccountStore::empty();
        let (acc, _) = accs.add_account("plat-a", "nm", "cookie", "sec");
        let st = state_with_accounts(two_platforms(), accs);
        let app = build_app(st);
        let body = json!({"action": "verify", "id": acc.id}).to_string();
        let res = app
            .oneshot(
                Request::post("/api/accounts").header("host", "127.0.0.1:8800")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let out = body_string(res).await;
        assert!(out.contains("\"verified\":false"), "body={out}");
    }

    #[tokio::test]
    async fn accounts_verify_syncs_the_platform_failure_reason() {
        // 「验活」是用户发现凭据过期时最先点的按钮。旧实现只回一句提示，
        // 平台卡片（/api/platforms 的 last_error）什么都不变 —— 点完看不到任何痕迹，
        // 切回接入源页还以为这平台一切正常，继续拿着过期凭据打请求。
        let accs = AccountStore::empty();
        let (acc, _) = accs.add_account("plat-c", "nm", "cookie", "sec");
        let st = state_with_accounts(cred_platform("plat-c"), accs);
        // 先塞一条旧原因：新的验活结论必须覆盖它，而不是被旧原因盖住。
        st.last_err.note("plat-c", 500, "上一次的老原因");
        let app = build_app(st.clone());
        let body = json!({"action": "verify", "id": acc.id}).to_string();
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/accounts").header("host", "127.0.0.1:8800")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let out = body_string(res).await;
        assert!(out.contains("\"verified\":false"), "body={out}");
        assert!(out.contains("验活未通过"), "body={out}");
        assert!(out.contains("last_error"), "响应里要带上面板卡片会看到的原因：{out}");

        let b = body_string(app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap()).await;
        let v: Value = serde_json::from_str(&b).unwrap();
        let row = v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "plat-c")
            .unwrap();
        // 验活拿不到 HTTP 状态码，状态记 0（前端不会画「HTTP 0」）。
        assert_eq!(row["last_error"]["status"], 0, "{b}");
        let text = row["last_error"]["text"].as_str().unwrap_or("");
        assert!(text.contains("验活未通过"), "平台卡片上看不到验活结论：{b}");
        assert!(!text.contains("上一次的老原因"), "旧原因没被覆盖：{b}");
    }

    #[tokio::test]
    async fn accounts_verify_success_clears_the_platform_failure_reason() {
        // 反面：换好凭据、验活通过之后，卡片上不能继续挂着旧的 401。
        use axum::routing::get as get_route;
        let mock = Router::new().route(
            "/models",
            get_route(|| async { (StatusCode::OK, "{\"data\":[]}") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, mock).await });
        let base = format!("http://{addr}");
        let sp = spec_toml(&format!(
            r#"
id = "plat-ok"
name = "plat-ok"
enabled = true
auth = "cookie"
base_url = "{base}"
format = "openai"
models = ["m1"]
models_policy = "static"
stream_dialect = "sse_delta"
"#
        ));
        let accs = AccountStore::empty();
        let (acc, _) = accs.add_account("plat-ok", "nm", "cookie", "sec");
        let st = state_with_accounts(vec![sp], accs);
        st.last_err.note("plat-ok", 401, "invalid api key");
        let app = build_app(st);
        let body = json!({"action": "verify", "id": acc.id}).to_string();
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/accounts").header("host", "127.0.0.1:8800")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let out = body_string(res).await;
        assert!(out.contains("\"verified\":true"), "body={out}");
        let b = body_string(app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap()).await;
        let v: Value = serde_json::from_str(&b).unwrap();
        assert!(
            v["rows"][0]["last_error"].is_null(),
            "验活通过之后不该再显示旧的失败原因：{b}"
        );
    }

    #[tokio::test]
    async fn auth_save_verify_failure_is_recorded_on_the_platform() {
        // 「保存凭据后再验活」是另一条主路径（粘贴 Cookie 的那个抽屉）。
        // 它失败时同样要写进平台卡片，否则用户会以为「保存成功 = 能用了」。
        let st = state(cred_platform("plat-s"));
        let app = build_app(st.clone());
        let body = json!({"action": "save", "platform": "plat-s", "secret": "cookie=abc"}).to_string();
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/auth").header("host", "127.0.0.1:8800")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let out = body_string(res).await;
        assert!(out.contains("\"verified\":false"), "body={out}");
        assert!(out.contains("验活未通过"), "body={out}");
        let b = body_string(app.clone().oneshot(get_with("/api/platforms", &[])).await.unwrap()).await;
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["rows"][0]["last_error"]["status"], 0, "{b}");
        assert!(
            v["rows"][0]["last_error"]["text"]
                .as_str()
                .unwrap_or("")
                .contains("验活未通过"),
            "保存凭据时的验活失败也要落到卡片上：{b}"
        );
    }

    #[tokio::test]
    async fn anon_platform_reports_zero_accounts_and_anon_flag() {
        // 真实事故：账号池里留着历史占位行（name="匿名公共池" secret="public"）挂在
        // anon-zen / kilo 上，/api/platforms 照实报 accounts_n=1 → 面板显示「1 账号」
        // 并算进「已接入 / 账号凭据」，而免密车道转发时根本不读账号池。
        // 免密车道的账号数必须恒为 0，另用 anon=true 让前端显示「免凭据」。
        let anon_spec = spec_toml(
            r#"
id = "anon-plat"
name = "anon-plat"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["m1"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let key_spec = spec_toml(
            r#"
id = "key-plat"
name = "key-plat"
enabled = true
auth = "api_key"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["m2"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let accs = AccountStore::empty();
        accs.add_account("anon-plat", "匿名公共池", "token", "public");
        let anon_acc = accs.secrets_for("anon-plat");
        assert_eq!(anon_acc.len(), 1, "占位行确实在池里");
        accs.add_account("key-plat", "真凭据", "token", "sk-x");
        let st = state_with_accounts(vec![anon_spec, key_spec], accs);
        let app = build_app(st);
        let res = app.oneshot(req("/api/platforms")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let rows = v["rows"].as_array().unwrap();
        let anon_row = rows.iter().find(|r| r["id"] == "anon-plat").unwrap();
        assert_eq!(anon_row["accounts_n"], 0, "免密车道不该报账号数：{anon_row}");
        assert_eq!(anon_row["anon"], true);
        let key_row = rows.iter().find(|r| r["id"] == "key-plat").unwrap();
        assert_eq!(key_row["accounts_n"], 1, "普通平台照实报账号数");
        assert_eq!(key_row["anon"], false);
    }

    #[tokio::test]
    async fn accounts_rows_flag_anon_platform_placeholders() {
        // /api/accounts 也要标出「这行属于免密车道」，前端才能提示「占位行·可删」，
        // 而不是把它当一份真凭据去「检查 / 停用」。
        let anon_spec = spec_toml(
            r#"
id = "anon-plat"
name = "anon-plat"
enabled = true
auth = "anon"
base_url = "http://127.0.0.1:9"
format = "openai"
models = ["m1"]
models_policy = "static"
stream_dialect = "sse_delta"
"#,
        );
        let accs = AccountStore::empty();
        let (anon_acc, _) = accs.add_account("anon-plat", "匿名公共池", "token", "public");
        let (real_acc, _) = accs.add_account("key-plat", "真凭据", "token", "sk-x");
        let st = state_with_accounts(vec![anon_spec], accs);
        // 免密车道被上游封锁时，池里的键是 `{平台}:anon`（转发时现造账号），
        // 不是占位行的随机 id。面板要按车道键查，否则这条车道被冻 120 秒
        // 在账号表上完全看不出来。
        st.pool.report("anon-plat", "anon-plat:anon", "m1", false, ErrorClass::LaneLocked, false);
        let app = build_app(st);
        let res = app.oneshot(req("/api/accounts")).await.unwrap();
        let v: Value = serde_json::from_str(&body_string(res).await).unwrap();
        let rows = v["rows"].as_array().unwrap();
        let a = rows.iter().find(|r| r["id"] == anon_acc.id).unwrap();
        assert_eq!(a["anon"], true, "免密车道的占位行要被标出来：{a}");
        assert_eq!(a["pool_key"], json!("anon-plat:anon"), "免密车道要按车道键查池：{a}");
        assert!(a["cooling_s"].as_f64().unwrap() > 0.0, "车道被封的剩余时间必须下发：{a}");
        assert_eq!(a["blocked"], json!(true), "{a}");
        assert_eq!(a["health"]["status"], json!("lane"), "健康结论也要按车道键查：{a}");
        let b = rows.iter().find(|r| r["id"] == real_acc.id).unwrap();
        assert_eq!(b["anon"], false);
    }

    /// 自愈续期只能对「这条凭据真的能续」的平台开：对每次失败都刷一遍等于白送
    /// 上游压力，还会把限流/额度问题伪装成凭据问题。
    #[test]
    fn refreshable_kind_only_covers_credential_refreshable_platforms() {
        let mk = |kind: &str, id: &str| -> ProviderSpec {
            let raw = format!(
                r#"id = "{id}"
name = "x"
auth = "cookie"
base_url = "https://example.com"
format = "openai"
models = ["m"]
models_policy = "static"
upstream_kind = "{kind}"
stream_dialect = "sse_delta"
"#
            );
            spec_toml(&raw)
        };
        assert!(refreshable_kind(&mk("trae", "apk-trae"), "jwt").is_some());
        assert!(refreshable_kind(&mk("kiro", "sub-kiro"), "{}").is_some());
        assert!(refreshable_kind(&mk("qoder", "apk-qoder"), "dt-1").is_some());
        // 免密车道与纯 Key 平台没有 refreshToken 可续
        assert!(refreshable_kind(&mk("openai", "api-openai"), "sk-1").is_none());
        assert!(refreshable_kind(&mk("opencode", "anon-zen"), "").is_none());
        // cline 认的是 secret 里的 workos:{at}:{rt} 三元组，不是平台名
        let cline = mk("openai", "cline");
        assert!(refreshable_kind(&cline, "workos:at:rt").is_some());
        assert!(refreshable_kind(&cline, "plain-token").is_none());
    }

    /// 单平台兜底只能是「清单拿不到」这一种情形：静态清单平台拼错模型名必须
    /// 404（与「未知模型 model_not_found」语义一致），不能静默转发出去让上游
    /// 回一个面板看不懂的错误。
    #[tokio::test]
    async fn single_platform_fallback_only_when_listing_is_unavailable() {
        let raw = r#"id = "solo"
name = "solo"
auth = "anon"
base_url = "https://solo.example"
format = "openai"
models = ["known-model"]
models_policy = "static"
upstream_kind = "openai"
stream_dialect = "sse_delta"
"#;
        let st = state(vec![spec_toml(raw)]);
        // 静态清单 + 只声明 known-model：拼错的名字不再被兜底
        assert_eq!(st.spec_for_model("known-model").map(|s| s.id), Some("solo".into()));
        assert!(st.spec_for_model("typo-model-xyz").is_none());
    }

    #[tokio::test]
    async fn live_platform_with_empty_listing_still_accepts_unknown_model() {
        let raw = r#"id = "live1"
name = "live1"
auth = "anon"
base_url = "https://live1.example"
format = "openai"
models = []
models_policy = "live"
upstream_kind = "openai"
stream_dialect = "sse_delta"
"#;
        let st = state(vec![spec_toml(raw)]);
        // live 清单为空 = 我们客观上没有名册，放行由上游裁决
        assert_eq!(
            st.spec_for_model("whatever-model").map(|s| s.id),
            Some("live1".into())
        );
        // 一旦拉到了非空清单，就开始按名册判（不再是任意名字都收）
        st.models_cache_per
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                "live1".to_string(),
                (std::time::Instant::now(), vec!["real-model".to_string()], true),
            );
        assert_eq!(st.spec_for_model("real-model").map(|s| s.id), Some("live1".into()));
        assert!(st.spec_for_model("whatever-model").is_none());
    }
    #[tokio::test]
    async fn accounts_action_verify_unknown_id_is_404() {
        let app = build_app(state(vec![]));
        let body = json!({"action": "verify", "id": "nope"}).to_string();
        let res = app
            .oneshot(
                Request::post("/api/accounts").header("host", "127.0.0.1:8800")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }


    // ---------- /v1/messages（Anthropic Messages 兼容端点） ----------

    #[tokio::test]
    async fn messages_nonstream_via_mock_upstream() {
        // Anthropic 请求 → OpenAI 上游 → Anthropic 信封：
        // 断言 role/content/stop_reason/usage 都被正确转换。
        let mock_resp = r#"{"id":"cmpl-1","choices":[{"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let app = build_app(state(vec![spec_for(&base)]));
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","messages":[{"role":"user","content":"hi"}],"max_tokens":16}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["type"], "message", "body={body}");
        assert_eq!(v["role"], "assistant", "body={body}");
        assert_eq!(v["content"][0]["type"], "text", "body={body}");
        assert_eq!(v["content"][0]["text"], "OK", "body={body}");
        assert_eq!(v["stop_reason"], "end_turn", "body={body}");
        assert_eq!(v["model"], "mock-1", "body={body}");
        assert_eq!(v["usage"]["input_tokens"], 2, "body={body}");
        assert_eq!(v["usage"]["output_tokens"], 1, "body={body}");
        assert!(v["id"].as_str().unwrap_or("").starts_with("msg_"), "body={body}");
    }

    /// ★ 回归：上游 200 + HTML（网页版逆向失效的典型形态）在 /v1/messages 上
    /// 必须变成非 2xx。旧实现直接透传 relay 的 status(200)，于是回了
    /// **HTTP 200 + 错误 JSON** —— Claude Code 把它当 assistant 消息解析，
    /// 用户只看到「响应格式错误」，看不到「上游给了登录页」。
    #[tokio::test]
    async fn messages_upstream_200_html_is_not_200() {
        let base = spawn_mock("<html><body>please login</body></html>", "text/html").await;
        let app = build_app(state(vec![spec_for(&base)]));
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","messages":[{"role":"user","content":"hi"}],"max_tokens":16}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::BAD_GATEWAY,
            "上游 200+HTML 必须映射成 502，不能原样回 200"
        );
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).expect("必须是 JSON 错误信封");
        assert_eq!(v["type"], "error", "body={body}");
        assert!(
            v["error"]["message"].as_str().unwrap_or("").contains("please login"),
            "应带回上游片段：{body}"
        );
    }

    #[tokio::test]
    async fn messages_stream_via_mock_upstream() {
        // OpenAI SSE → Anthropic SSE 事件序列：
        // message_start → content_block_start → content_block_delta(Hi) → content_block_stop
        // → message_delta(stop_reason+usage) → message_stop
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
        let base = spawn_mock(Box::leak(sse.to_string().into_boxed_str()), "text/event-stream").await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let app = build_app(AppState::new(
            vec![spec_for(&base)],
            store.clone(),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        ));
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"), "ct={ct}");
        let body = body_string(res).await;
        assert!(body.contains("event: message_start"), "body={body}");
        assert!(body.contains("\"type\":\"content_block_delta\""), "body={body}");
        assert!(body.contains("text_delta"), "body={body}");
        assert!(body.contains("Hi"), "body={body}");
        // 顺序：message_start 在最前，message_stop 在最后
        let i_start = body.find("message_start").unwrap();
        let i_stop = body.find("message_stop").unwrap();
        assert!(i_start < i_stop, "body={body}");
        assert!(body.contains("\"stop_reason\":\"end_turn\""), "body={body}");
        assert!(body.contains("\"output_tokens\":2"), "body={body}");
        // 记账（spawn 异步）
        for _ in 0..20 {
            if store.usage_totals()["requests"].as_u64().unwrap_or(0) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let totals = store.usage_totals();
        assert_eq!(totals["requests"], 1, "totals={totals}");
        assert_eq!(totals["tokensIn"], 4, "totals={totals}");
        assert_eq!(totals["tokensOut"], 2, "totals={totals}");
    }


    #[tokio::test]
    async fn messages_stream_without_upstream_usage_estimates_output_tokens() {
        // 上游不带 usage 帧（如 apk-doubao）时，output_tokens 按已收集文本估算，不为 0。
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let base = spawn_mock(Box::leak(sse.to_string().into_boxed_str()), "text/event-stream").await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let app = build_app(AppState::new(
            vec![spec_for(&base)],
            store.clone(),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        ));
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","stream":true,"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("Hi"), "body={body}");
        assert!(body.contains("message_stop"), "body={body}");
        // Hi = 2 字符 → (2/2).max(1) = 1
        assert!(body.contains("\"output_tokens\":1"), "body={body}");
    }

    // ---------- /v1/messages 工具调用（Claude Code 的核心路径） ----------

    #[test]
    fn anthropic_tools_are_mapped_to_openai_functions() {
        let v = json!({
            "model": "m",
            "max_tokens": 64,
            "system": "sys",
            "tools": [
                {"name": "get_weather", "description": "weather", "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}},
                {"type": "web_search_20250305", "name": "web_search"}
            ],
            "tool_choice": {"type": "any"},
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "18C"}]},
                    {"type": "text", "text": "thanks"}
                ]}
            ]
        });
        let oa = anthropic_to_openai(&v).expect("convert");
        let tools = oa["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "服务端工具（无 input_schema）应被跳过：{tools:?}");
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(tools[0]["function"]["description"], "weather");
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["city"]["type"],
            "string"
        );
        assert_eq!(oa["tool_choice"], "required");
        let msgs = oa["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 5, "system+user+assistant+tool+user：{msgs:?}");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "checking");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "get_weather");
        let args: Value = serde_json::from_str(
            msgs[2]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(args["city"], "Paris");
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "toolu_1");
        assert_eq!(msgs[3]["content"], "18C");
        assert_eq!(msgs[4]["role"], "user");
        assert_eq!(msgs[4]["content"], "thanks");
        // tool_choice 的 tool 形态
        let v2 = json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "tool_choice": {"type": "tool", "name": "foo"}});
        let oa2 = anthropic_to_openai(&v2).unwrap();
        assert_eq!(oa2["tool_choice"]["function"]["name"], "foo");
    }

    #[tokio::test]
    async fn messages_nonstream_tool_calls_become_tool_use_blocks() {
        let mock_resp = r#"{"id":"cmpl-t","choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_9","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let app = build_app(state(vec![spec_for(&base)]));
        let req_body = r#"{"model":"mock-1","max_tokens":64,"messages":[{"role":"user","content":"weather in Paris?"}],"tools":[{"name":"get_weather","input_schema":{"type":"object"}}]}"#;
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["stop_reason"], "tool_use", "body={body}");
        let blocks = v["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1, "body={body}");
        assert_eq!(blocks[0]["type"], "tool_use", "body={body}");
        assert_eq!(blocks[0]["id"], "call_9", "body={body}");
        assert_eq!(blocks[0]["name"], "get_weather", "body={body}");
        assert_eq!(blocks[0]["input"]["city"], "Paris", "body={body}");
        assert_eq!(v["usage"]["output_tokens"], 3, "body={body}");
    }

    #[tokio::test]
    async fn messages_stream_tool_calls_become_tool_use_blocks() {
        let lines = vec![
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_s1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "",
            r#"data: {"usage":{"prompt_tokens":9,"completion_tokens":4}}"#,
            "",
            "data: [DONE]",
            "",
        ];
        let sse = format!("{}\n\n", lines.join("\n"));
        let base = spawn_mock(Box::leak(sse.into_boxed_str()), "text/event-stream").await;
        let app = build_app(state(vec![spec_for(&base)]));
        let req_body = r#"{"model":"mock-1","stream":true,"messages":[{"role":"user","content":"weather?"}],"tools":[{"name":"get_weather","input_schema":{"type":"object"}}]}"#;
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(req_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("\"type\":\"tool_use\""), "body={body}");
        assert!(body.contains("input_json_delta"), "body={body}");
        assert!(body.contains("call_s1"), "body={body}");
        assert!(body.contains("get_weather"), "body={body}");
        assert!(body.contains("\"stop_reason\":\"tool_use\""), "body={body}");
        // 工具块的 index 从 0 开始，且必须有 content_block_stop
        assert!(body.contains("\"index\":0"), "body={body}");
        assert!(body.contains("content_block_stop"), "body={body}");
        assert!(body.contains("message_stop"), "body={body}");
    }
    #[tokio::test]
    async fn count_tokens_estimates_positive_input_tokens() {
        let app = build_app(state(vec![]));
        let body = r#"{"model":"mock-1","system":"you are helpful","messages":[{"role":"user","content":"hello world, this is a longer sentence to count"}],"tools":[{"name":"get_weather","input_schema":{"type":"object"}}]}"#;
        let res = app
            .oneshot(
                Request::post("/v1/messages/count_tokens").header("host", "127.0.0.1:8800")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let text = body_string(res).await;
        let v: Value = serde_json::from_str(&text).unwrap();
        let n = v["input_tokens"].as_i64().unwrap_or(0);
        assert!(n >= 10, "input_tokens={n} body={text}");
        // 中文按 1 字 1 token
        assert!(estimate_tokens("你好世界") >= 4);
        assert!(estimate_tokens("") >= 1);
    }

    #[tokio::test]
    async fn count_tokens_respects_the_keys_allow_list() {
        // ★ 回归：count_tokens 是四条协议入口里唯一不碰上游、却照样认 model 字段的旁路。
        //   旧实现收 State(_st) 不收 AccessKeyCtx，白名单之外的模型名也能拿到 200，
        //   客户端据此以为可用（正式请求再过 /v1/messages 就是 403）。
        //   Claude Code 发正式请求前必先调它做预算，口径必须与 messages 一致。
        let keys = aigw_store::KeyStore::empty();
        let (_k, secret) = keys.add("窄口", vec!["alpha".into()], "");
        let mut st = state(vec![]);
        st.keys = std::sync::Arc::new(keys);
        let app = build_app(st);
        let call = |model: &'static str| {
            let body = format!(
                r#"{{"model":"{model}","messages":[{{"role":"user","content":"hello"}}]}}"#
            );
            Request::post("/v1/messages/count_tokens")
                .header("host", "127.0.0.1:8800")
                .body(Body::from(body))
                .unwrap()
        };
        // 白名单内的模型：正常估算
        let mut r = call("alpha");
        r.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {secret}").parse().unwrap(),
        );
        let res = app.clone().oneshot(r).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // 白名单外的模型：必须 403，且文案与 messages 一致
        let mut r = call("beta");
        r.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {secret}").parse().unwrap(),
        );
        let res = app.clone().oneshot(r).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::FORBIDDEN,
            "白名单外的模型名不该在这个接口上拿到 200"
        );
        let body = body_string(res).await;
        assert!(body.contains("这把访问密钥不能调用模型 beta"), "body={body}");
        // 不带 key 的调用方不受影响
        let res = app.oneshot(call("beta")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
    #[tokio::test]
    async fn messages_missing_messages_field_is_400() {
        let app = build_app(state(vec![]));
        let res = app
            .oneshot(
                Request::post("/v1/messages").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"mock-1","max_tokens":8}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["type"], "error", "body={body}");
        assert_eq!(v["error"]["type"], "invalid_request_error", "body={body}");
        assert!(v["error"]["message"].as_str().unwrap_or("").contains("messages"), "body={body}");
    }

    // ---------- /v1/responses（OpenAI Responses API） ----------

    #[tokio::test]
    async fn responses_nonstream_via_mock_upstream() {
        let mock_resp = r#"{"id":"cmpl-1","choices":[{"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}"#;
        let base = spawn_mock(
            Box::leak(mock_resp.to_string().into_boxed_str()),
            "application/json",
        )
        .await;
        let app = build_app(state(vec![spec_for(&base)]));
        let res = app
            .oneshot(
                Request::post("/v1/responses").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"mock-1","input":"hi","max_output_tokens":16}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["object"], "response", "body={body}");
        assert_eq!(v["status"], "completed", "body={body}");
        assert_eq!(v["output_text"], "OK", "body={body}");
        assert_eq!(v["output"][0]["type"], "message", "body={body}");
        assert_eq!(v["output"][0]["content"][0]["type"], "output_text", "body={body}");
        assert_eq!(v["output"][0]["content"][0]["text"], "OK", "body={body}");
        assert_eq!(v["usage"]["input_tokens"], 2, "body={body}");
        assert_eq!(v["usage"]["output_tokens"], 1, "body={body}");
        assert_eq!(v["usage"]["total_tokens"], 3, "body={body}");
        assert!(v["id"].as_str().unwrap_or("").starts_with("resp_"), "body={body}");
    }

    /// ★ 回归：同 /v1/messages —— 上游 200+HTML 在 /v1/responses 上也必须 502。
    /// Codex CLI 拿到 200 会当「成功的空响应」，而不是「上游要登录」。
    #[tokio::test]
    async fn responses_upstream_200_html_is_not_200() {
        let base = spawn_mock("<html><body>please login</body></html>", "text/html").await;
        let app = build_app(state(vec![spec_for(&base)]));
        let res = app
            .oneshot(
                Request::post("/v1/responses").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"mock-1","input":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).expect("必须是 JSON 错误信封");
        assert!(v["error"]["message"].as_str().unwrap_or("").contains("please login"), "body={body}");
    }

    /// 协议转换面：上游没给 completion_tokens 时不能把 output_tokens 恒写成 0。
    #[test]
    fn converted_usage_estimates_output_tokens() {
        let no_usage = json!({
            "choices": [{"message": {"role": "assistant", "content": "你好世界"}}]
        });
        assert_eq!(conv_usage(&no_usage), (0, 2));
        let with_tools = json!({
            "choices": [{"message": {"role": "assistant", "content": null,
                "tool_calls": [{"id": "c1", "type": "function",
                    "function": {"name": "f", "arguments": "{\"city\":\"Paris\"}"}}]}}]
        });
        let (_, tout) = conv_usage(&with_tools);
        assert!(tout > 0, "工具调用参数也要计入出参估算");
        // 上游给了真实值就绝不覆盖
        let real = json!({
            "choices": [{"message": {"content": "hi"}}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 9}
        });
        assert_eq!(conv_usage(&real), (7, 9));
    }

    /// 上游非流式响应没带 usage 时，记账不能恒为 0（自建/部分免费站常见）。
    #[test]
    fn nonstream_usage_falls_back_to_estimate() {
        let no_usage = json!({
            "choices": [{"message": {"role": "assistant", "content": "你好世界"}, "finish_reason": "stop"}]
        });
        let (tin, tout) = usage_or_estimate(&no_usage, 1000);
        assert_eq!(tin, 250, "入参按字节 / 4 估算");
        assert_eq!(tout, 2, "出参按字符 / 2 估算");
        // 上游真给了 usage 时不得被估算覆盖
        let real = json!({
            "choices": [{"message": {"role": "assistant", "content": "你好世界"}}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 22}
        });
        assert_eq!(usage_or_estimate(&real, 1000), (11, 22));
        // 完全空响应：出参给 0 而不是编造 1
        let empty = json!({"choices": [{"message": {"content": ""}}]});
        assert_eq!(usage_or_estimate(&empty, 8), (2, 0));
    }

    #[test]
    fn responses_to_openai_converts_tools_and_tool_history() {
        // ★ 回归：Codex CLI 的工具是多轮的 —— tools 定义（扁平）要换成 chat 的嵌套形状，
        //   input 里的 function_call / function_call_output 要变成 assistant.tool_calls
        //   与 role=tool 消息。旧实现两样都丢，上游看到的是一串没有工具的普通对话。
        let req = json!({
            "model": "mock-1",
            "instructions": "be nice",
            "tools": [
                {"type": "function", "name": "shell", "description": "run",
                 "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}},
                {"type": "web_search"}
            ],
            "tool_choice": {"type": "function", "name": "shell"},
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "list files"}]},
                {"type": "function_call", "call_id": "call_1", "name": "shell",
                 "arguments": "{\"command\":\"ls\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "a.txt"},
            ],
        });
        let oa = responses_to_openai(&req).expect("convert");
        let msgs = oa["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        // 工具调用变成 assistant 的 tool_calls
        assert_eq!(msgs[2]["role"], "assistant");
        assert!(msgs[2]["content"].is_null(), "纯工具调用的 content 必须是 null");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "shell");
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"ls\"}"
        );
        // 工具结果变成 role=tool
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_1");
        assert_eq!(msgs[3]["content"], "a.txt");
        // tools 换成嵌套形状，托管工具（web_search）被剔除
        let tools = oa["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "托管工具没有 chat 等价物，必须剔除：{tools:?}");
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "shell");
        assert_eq!(tools[0]["function"]["description"], "run");
        assert!(tools[0]["function"]["parameters"]["properties"]["command"].is_object());
        // tool_choice 也换形状
        assert_eq!(oa["tool_choice"]["type"], "function");
        assert_eq!(oa["tool_choice"]["function"]["name"], "shell");
    }

    #[test]
    fn openai_to_responses_emits_function_call_items() {
        // ★ 反向：上游回了 tool_calls，Responses 必须给 function_call item，
        //   否则客户端等不到要执行的动作，agent 循环卡死。
        let v = json!({
            "id": "cmpl-1",
            "choices": [{
                "message": {"role": "assistant", "content": null,
                    "tool_calls": [{"id": "call_9", "type": "function",
                        "function": {"name": "shell", "arguments": "{\"command\":\"ls\"}"}}]},
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4}
        });
        let out = openai_to_responses(&v, "mock-1");
        let items = out["output"].as_array().unwrap();
        assert_eq!(items.len(), 1, "纯工具调用不应造空 message item：{out}");
        assert_eq!(items[0]["type"], "function_call");
        assert_eq!(items[0]["call_id"], "call_9");
        assert_eq!(items[0]["name"], "shell");
        assert_eq!(items[0]["arguments"], "{\"command\":\"ls\"}");
    }

    #[tokio::test]
    async fn responses_stream_via_mock_upstream() {
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
        let base = spawn_mock(Box::leak(sse.to_string().into_boxed_str()), "text/event-stream").await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()));
        let app = build_app(AppState::new(
            vec![spec_for(&base)],
            store.clone(),
            Pool::new(),
            Arc::new(OutClient::new()),
            Arc::new(AccountStore::empty()),
            Arc::new(CustomStore::empty()),
            Arc::new(FreeStore::empty()),
        ));
        let res = app
            .oneshot(
                Request::post("/v1/responses").header("host", "127.0.0.1:8800")
                    .body(Body::from(
                        r#"{"model":"mock-1","stream":true,"input":"hi"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_string(res).await;
        assert!(body.contains("event: response.created"), "body={body}");
        assert!(body.contains("response.output_text.delta"), "body={body}");
        assert!(body.contains("event: response.completed"), "body={body}");
        assert!(body.contains("Hi"), "body={body}");
        let i_created = body.find("response.created").unwrap();
        let i_done = body.find("response.completed").unwrap();
        assert!(i_created < i_done, "body={body}");
        assert!(body.contains("\"output_tokens\":2"), "body={body}");
        for _ in 0..20 {
            if store.usage_totals()["requests"].as_u64().unwrap_or(0) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let totals = store.usage_totals();
        assert_eq!(totals["requests"], 1, "totals={totals}");
        assert_eq!(totals["tokensIn"], 4, "totals={totals}");
        assert_eq!(totals["tokensOut"], 2, "totals={totals}");
    }

    #[tokio::test]
    async fn responses_missing_input_is_400() {
        let app = build_app(state(vec![]));
        let res = app
            .oneshot(
                Request::post("/v1/responses").header("host", "127.0.0.1:8800")
                    .body(Body::from(r#"{"model":"mock-1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_string(res).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["type"], "invalid_request_error", "body={body}");
        assert!(v["error"]["message"].as_str().unwrap_or("").contains("input"), "body={body}");
    }

    #[test]
    fn responses_to_openai_maps_instructions_and_blocks() {
        let v = json!({
            "model": "m",
            "instructions": "be nice",
            "max_output_tokens": 32,
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "hello"}]},
                {"role": "assistant", "content": "hi there"},
                {"role": "user", "content": "go on"}
            ]
        });
        let oa = responses_to_openai(&v).expect("convert");
        assert_eq!(oa["model"], "m");
        assert_eq!(oa["max_tokens"], 32);
        let msgs = oa["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4, "msgs={msgs:?}");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "be nice");
        assert_eq!(msgs[1]["content"], "hello");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[3]["content"], "go on");
        // 只有 instructions 没有 input → 400 路径
        let bad = json!({"model": "m", "instructions": "x"});
        assert!(responses_to_openai(&bad).is_err());
    }
    #[tokio::test]
    async fn same_model_prefers_chat_capable_platform() {
        // 实测：glm-5.3 同时被 web-lobster（base_url="@lobster" 占位符，有账号）
        // 和别的平台声明。旧实现按 id 顺序取第一个 routable 的，于是打开就 503
        // 「上游地址未配置」。路由应优先选真的能转发的平台。
        let toml_specs = [
            r#"
            id = "aaa-placeholder"
            name = "占位平台"
            base_url = "@lobster"
            auth = "api_key"
            format = "openai"
            models_policy = "static"
            stream_dialect = "sse_delta"
            models = ["dup-model"]
            "#,
            r#"
            id = "zzz-real"
            name = "真平台"
            base_url = "http://127.0.0.1:9"
            auth = "api_key"
            format = "openai"
            models_policy = "static"
            stream_dialect = "sse_delta"
            models = ["dup-model"]
            "#,
        ];
        let specs: Vec<ProviderSpec> = toml_specs.iter().map(|s| spec_toml(s)).collect();
        let accs = AccountStore::empty();
        accs.add_account("aaa-placeholder", "n", "token", "s1");
        accs.add_account("zzz-real", "n", "token", "s2");
        let st = state_with_accounts(specs, accs);
        let got = st.spec_for_model("dup-model").expect("应能路由");
        assert_eq!(got.id, "zzz-real", "应跳过占位地址平台");
    }
}
