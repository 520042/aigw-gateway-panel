//! 服务运行态：`AppState` 及其「某个平台/模型能不能用」的判定。
//!
//! **为什么独立成模块**：`AppState` 是 28 个字段的运行态容器，自带 12 个
//! `with_*` 构造器与 5 个 `platform_*` 判定 —— 后者是路由与面板共同的判据
//! （「有凭据吗」「协议支持吗」「地址是不是占位符」），最容易出现「面板说能用、
//! 请求却 401」这种口径漂移。
//!
//! 这里的每个判定都**以凭据为中心**：没有 Key 的平台即使声明了模型也不该出现在
//! /v1/models 里，否则客户端会拿到一份它永远调不通的清单。

use aigw_pool::Pool;
use aigw_registry::{AuthKind, ProviderSpec};
use aigw_relay::auth_login::LoginEngine;
use aigw_relay::OutClient;
use aigw_store::{AccountStore, CustomStore, FlagStore, FreeStore, Store};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::livecfg;
use crate::{assemble_models, sync_custom_models, TaskHealth, LastErrors};
use crate::ratelimit::RateLimiter;
use crate::models::{custom_to_spec, free_to_spec, ModelsEntry, MODELS_FETCH_CONCURRENCY};

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
    /// 整表模型清单缓存：(生成时刻, (平台, 模型) 索引, 响应体)。
    ///
    /// ★ 响应体用 `Arc<Value>` 而不是 `Value`：这个缓存的读路径是「每个客户端
    ///   拉一次 /v1/models」，而写入之后内容就不再变。用裸 `Value` 时命中一次就要
    ///   深拷贝整棵 JSON 树（148 个模型对象 + 数组 + 对象包装），且**拷贝发生在持锁
    ///   期间** —— 并发拉清单会在这把锁上排队。换成 Arc 后命中只是引用计数 +1。
    pub models_cache: Arc<Mutex<Option<(Instant, Vec<(String, String)>, std::sync::Arc<Value>)>>>,
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
    /// ★ 2026-10-26（P3）：在线配置（并发/超时/重试/限流），面板改完立即生效。
    pub cfg: Arc<livecfg::RuntimeCfg>,
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
pub fn loopback_hosts() -> Vec<String> {
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
pub fn norm_model_id(raw: &str) -> String {
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
pub fn same_model(a: &str, b: &str) -> bool {
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
            // 默认从 data_dir 读；测试/未注入时用临时默认（不落盘）
            cfg: livecfg::RuntimeCfg::load(
                &std::env::temp_dir().join("aigw-livecfg-default"),
            ),
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
        // 运行时配置的真源在 data_dir/runtime.json —— 与 data_dir 同时注入，
        // 否则面板改的参数会写到临时目录、重启就丢。
        self.cfg = livecfg::RuntimeCfg::load(&dir);
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
        let lim = RateLimiter::new(per_ip, global);
        lim.set_rates(per_ip, global, key_rpm);
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
        self.canonical_platform_id(candidate).is_some()
    }

    /// 把「后缀里写的平台名」归一成真实的 spec id（大小写不敏感）。
    ///
    /// ★ 为什么要有它：模型名那边（`canonical_model_for` → `same_model`）本来就
    ///   大小写不敏感，平台名却曾是**精确匹配** —— 用户在客户端里手写
    ///   `kilo-auto/free@KILO` 会失败，而且报的是「未知模型 kilo-auto/free@KILO」，
    ///   把整串当成模型名，**完全没提「平台名写错了」**，用户会去查模型名而不是平台。
    ///   两边口径必须一致。
    ///
    /// 先精确命中（最常见、也最快），再退到大小写不敏感；仍找不到才算「不是平台」。
    pub fn canonical_platform_id(&self, candidate: &str) -> Option<String> {
        let c = candidate.trim();
        if c.is_empty() {
            return None;
        }
        let specs = self.all_specs();
        if specs.iter().any(|s| s.id == c) {
            return Some(c.to_string());
        }
        specs
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(c))
            .map(|s| s.id.clone())
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
    /// 在「真正能聊天」的平台里先找，找不到才回落到「凭据可达」的平台。
    ///
    /// 这个双层顺序在 `spec_for_model` 里重复了六次（静态清单精确 / 整表缓存 /
    /// 每平台 live 缓存 / 别名归一化 …）。每处都各写一遍 `preferred.iter().find()`
    /// 加一段 almost-identical 的 `usable.iter().find()`，改口径时极易漏改其中一处，
    /// 表现为「某些模型永远绕开 preferred」。抽出来后只有一个地方定义优先级。
    ///
    /// `pick` 收到的是候选切片，返回命中的平台引用。
    pub(crate) fn pick_preferred<'a, F>(
        preferred: &[&'a ProviderSpec],
        usable: &[&'a ProviderSpec],
        pick: F,
    ) -> Option<&'a ProviderSpec>
    where
        F: Fn(&ProviderSpec) -> bool,
    {
        preferred
            .iter()
            .copied()
            .find(|s| pick(s))
            .or_else(|| usable.iter().copied().find(|s| pick(s)))
    }

    pub(crate) fn spec_for_model(&self, model: &str) -> Option<ProviderSpec> {
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
        if let Some(s) = Self::pick_preferred(&preferred, &usable, |s| {
            s.models.iter().any(|m| m == model)
        }) {
            return Some(s.clone());
        }
        {
            let g = self.models_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, index, _)) = g.as_ref() {
                // 先把命中该模型的平台 id 全收集起来，再统一走优先级判定：
                // 旧实现是「先扫一遍 preferred、再扫一遍 usable」，两个循环体内容相同。
                let sids: Vec<&String> = index
                    .iter()
                    .filter(|(_, m)| m == model)
                    .map(|(sid, _)| sid)
                    .collect();
                if let Some(s) = Self::pick_preferred(&preferred, &usable, |s| {
                    sids.iter().any(|sid| **sid == s.id)
                }) {
                    return Some(s.clone());
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
            let sids: Vec<String> = g
                .iter()
                .filter(|(_, (_, ids, _))| ids.iter().any(|m| m == model))
                .map(|(sid, _)| sid.clone())
                .collect();
            if let Some(s) = Self::pick_preferred(&preferred, &usable, |s| {
                sids.iter().any(|sid| *sid == s.id)
            }) {
                return Some(s.clone());
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
        if let Some(s) = Self::pick_preferred(&preferred, &usable, alias_hit) {
            return Some(s.clone());
        }
        {
            let g = self.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
            let live_hit = |ids: &Vec<String>| {
                ids.iter()
                    .any(|m| norm_model_id(m) == want && !self.flags.is_model_disabled(m))
            };
            let sids: Vec<String> = g
                .iter()
                .filter(|(_, (_, ids, _))| live_hit(ids))
                .map(|(sid, _)| sid.clone())
                .collect();
            if let Some(s) = Self::pick_preferred(&preferred, &usable, |s| {
                sids.iter().any(|sid| *sid == s.id)
            }) {
                return Some(s.clone());
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
        // ★ 2026-10-26（审计优化）：路由别名（auto / free）在指定平台（模型@平台）
        // 的清单里匹配不到时，回落到该平台**第一个未停用的模型**——而不是把
        // "auto" 裸名发给上游（实测 OpenCode 回 400 Model is unavailable，用户只看到
        // 一句看不懂的上游报错）。免凭据车道（anon-zen / kilo）清一色免费，取第一个
        // 就是合理默认；普通平台同样受益（"auto@平台" 成了"随便给一个能用"）。
        if requested == "auto" || requested == "free" {
            let disabled = |m: &str| self.flags.is_model_disabled(m);
            if let Some(m) = spec.models.iter().find(|m| !disabled(m)) {
                return m.clone();
            }
            let g = self.models_cache_per.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, ids, _)) = g.get(&spec.id) {
                if let Some(m) = ids.iter().find(|m| !disabled(m)) {
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
    pub(crate) async fn resolve_spec(&self, model: &str) -> Option<ProviderSpec> {
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
    pub(crate) async fn fetch_live_models(&self, specs: Vec<ProviderSpec>) {
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
    pub fn warm_models(self) -> impl std::future::Future<Output = ()> + Send + 'static {
        // ★ 收 `self`（按值）而不是 `&self`：返回类型声明了 `'static`，用 `&self`
        //   时异步块会捕获那个引用，直接撑破 `'static`（报 “lifetime may not live
        //   long enough”，但根因是签名与实现不一致）。`AppState` 全是 `Arc`，
        //   clone 一次没有实际开销。
        let st = self;
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
            *g = Some((Instant::now(), index, std::sync::Arc::new(body)));
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
