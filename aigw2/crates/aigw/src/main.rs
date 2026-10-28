//! aigw v2 入口：CLI 解析 → registry 加载 → 数据目录 + 旧数据导入 → axum 起服。
//!
//! 里程碑路线见 重构计划-v2.md：M0 骨架 → M1 引擎核（本文件含数据迁移接线）
//! → M2 声明式接入 → M3 平台全量 → M4 前端 → M5 交付链 → M6 可观测。
//! 端口最终目标 8800；与旧版并行期用 `--port 8801`。

mod install;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "aigw",
    version,
    about = "aigw v2 —— 免费 AI 额度 → 本地 OpenAI 兼容端点（Rust 全量重写）"
)]
struct Cli {
    /// HTTP 监听地址（仅本机回环）
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// HTTP 监听端口；与旧版并行期用 --port 8801
    #[arg(long, default_value_t = 8800)]
    port: u16,
    /// providers 目录（声明式平台 spec）。缺省依次找：
    /// cwd/providers → exe 同级 providers → 源码树 providers（本地开发）。
    #[arg(long)]
    providers: Option<String>,
    /// 跳过旧版数据导入
    #[arg(long, default_value_t = false)]
    no_import: bool,
    /// 安装到 %LOCALAPPDATA%\aigw2（快捷方式 + 自启）后退出
    #[arg(long, default_value_t = false)]
    install: bool,
    /// 卸载（保留数据）
    #[arg(long, default_value_t = false)]
    uninstall: bool,
    /// 彻底卸载（删除数据）
    #[arg(long, default_value_t = false)]
    purge: bool,
    /// /v1/* 每分钟请求上限（全局；0 = 关闭）。超出返回 429 + Retry-After。
    #[arg(long, default_value_t = 1200)]
    rpm: u32,
    /// /v1/* 每分钟请求上限（每个客户端 IP；0 = 关闭）。
    #[arg(long, default_value_t = 300)]
    ip_rpm: u32,
    /// /v1/* 每分钟请求上限（**每把访问密钥**；0 = 关闭）。
    ///
    /// 为什么需要：本机部署下「每 IP 限流」其实等同于全局限流（面板、脚本、各个客户端
    /// 全在 127.0.0.1）。给同事或者脚本发一把 key，他那边挂个死循环就把所有人的
    /// 额度吃光 —— 而 key 的用途本来就是「把不同用途分开」，额度也该跟着分开。
    #[arg(long, default_value_t = 120)]
    key_rpm: u32,
    /// 面板访问令牌：设置后 /api/* 必须携带（X-AIGW-Token / Bearer / aigw_token Cookie）。
    /// 缺省读环境变量 AIGW_PANEL_TOKEN；对外监听（非回环）时未设置会自动生成并打印一次。
    #[arg(long)]
    token: Option<String>,
    /// 接入源体检：逐个用真实代码去拉模型清单，打印「谁通、谁挂、为什么」后退出。
    ///
    /// 用途：上游改版/反代失效时，面板上只能看到「近24h 失败 N 次」，说不出是哪个
    /// 端点变了。这个模式把每个源的结论直接打出来，是排查「接入源是不是废了」的
    /// 第一手段。不启动服务、不写数据。
    #[arg(long, default_value_t = false)]
    doctor: bool,
    /// 配合 --doctor：只体检 id 含该子串的源（可重复传）。
    #[arg(long)]
    only: Vec<String>,
}

/// 接入源体检：用**真实转发代码**逐个去拉模型清单，把结论打出来。
///
/// 为什么不用 curl 手工试：每个源的鉴权方式、头组、签名、私有协议都不一样
/// （qoder 要 COSY 签名、trae 要 Cloud-IDE-JWT、doubao 走 samantha 私有 SSE）。
/// 手工拼的请求与代码发出的请求不是一回事，手测「通」不能证明代码可用。
///
/// 判定口径与运行时一致：
/// - Ok(n)     → 端点通（n=0 表示上游给了合法空清单，不是失败）
/// - 401/403   → **端点活着**，只是本机没凭据（不算失效）
/// - 其它 Err  → 真失败（超时 / 4xx / 5xx / 解析不了），需要人工跟进
/// 把 URL 里的 host 解析成 IP。返回 `Some(false)` = **DNS 查不到这个域名**。
///
/// ★ 为什么要单独查 DNS：体检把「连不上」统一归成 [NET]，但里面混着三类完全
///   不同的事 —— ① 域名已经不存在（ENOTFOUND，源真的没了）② 域名在但被 WAF 拦
///   ③ 海外源本机没配代理。三类都显示「连不上」时，用户只能一个个手工 dig。
///   先查一次 DNS 就能把 ① 单独摘出来，那才是真正需要处理的。
fn host_resolves(base_url: &str) -> Option<bool> {
    let rest = base_url.split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    use std::net::ToSocketAddrs;
    Some((host, 443u16).to_socket_addrs().map(|mut it| it.next().is_some()).unwrap_or(false))
}
/// 从代理 URL 抠出 (host, port)：转调 relay 的唯一实现（面板「测试连通」共用）。
fn proxy_host_port(url: &str) -> Option<(String, u16)> {
    aigw_relay::proxy::parse_host_port(url)
}
async fn run_doctor(specs: Vec<aigw_registry::ProviderSpec>, only: &[String]) -> i32 {
    use aigw_relay::OutClient;
    let targets: Vec<_> = specs
        .into_iter()
        // ★ 只体检声明了 live 清单的源。Static/None 策略的源**本来就没有**
        //   /models 上游（秘塔、豆包、元宝、Trae 都是私有协议，清单靠静态表或后缀路由），
        //   拿 /models 去探必然 404 —— 那不是失效，是这边探错了地方。
        // static 源里只有显式声明了 models_path 的才值得核对静态清单
        // （其余的私有协议源本来就没有清单端点，拿 /models 去探必然 404）。
        // ★ 现在**所有**启用源都参与体检。做法分两档（见下面的探测分支）：
        //   ① 有清单端点的（live / static+models_path）→ 拉清单，顺带核对静态表；
        //   ② 没有清单端点的（none / static 无 models_path）→ 探**根路径**判可达。
        //   之前只覆盖 ①，导致 31 个源（大量逆向协议源 + 4 家自定义 adapter）
        //   从来没人探过，端点是死是活只能等用户配好凭据才发现。
        // 已停用的源不体检：它们多半就是「上游已废」才被停掉的，报出来只是噪音。
        .filter(|s| s.enabled)
        // 占位符源（base_url 以 @ 开头 = 用户还没填地址）没有可探测的端点：
        // 硬探会得到一个非法 URL，然后被归进 [NET]「连不上」—— 那是假故障。
        .filter(|s| !s.base_url.starts_with('@'))
        .filter(|s| {
            if only.is_empty() {
                return true;
            }
            only.iter().any(|k| s.id.contains(k.as_str()))
        })
        .collect();
    if targets.is_empty() {
        eprintln!("没有匹配的接入源（--only 过滤后为空）");
        return 2;
    }
    let client = std::sync::Arc::new(OutClient::new());
    println!(
        "接入源体检：{} 个 live 源（并发 8，单源 12s 超时）\n",
        targets.len()
    );
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
    let mut handles = Vec::new();
    for spec in targets {
        let c = client.clone();
        let s2 = sem.clone();
        handles.push(tokio::spawn(async move {
            let _p = s2.acquire_owned().await;
            // 无凭据拉清单：有 Key 的源会回 401/403，那也是「端点活着」的证据。
            // ★ none 策略的源**没有清单端点**（清单靠静态表或 模型@平台 后缀），
            //   拿 /models 去探必然 404 —— 那是探错了地方，不是端点失效。
            //   对它们改探**根路径**：只要拿到任何 HTTP 状态（而非连接失败/超时），
            //   就说明「域名与网关还在」。这条覆盖的价值在实测中体现过：
            //   orcaterm / monkeycode / accio 这类自定义 adapter 之前完全没被探过。
            let has_models_endpoint = spec.models_policy == aigw_registry::ModelsPolicy::Live
                || (spec.models_policy == aigw_registry::ModelsPolicy::Static
                    && !spec.models_path.is_empty());
            let r = if !has_models_endpoint {
                let (st, err) = aigw_relay::OutClient::probe_reachable(c.http(), &spec.base_url).await;
                match err {
                    Some(e) => Err((0u16, e)),
                    None => Ok(vec![format!("http-{st}")]),
                }
            } else {
                c.fetch_models_with_secret(&spec, "")
                    .await
                    .map_err(|e| (e.status, e.body))
            };
            (
                spec.id.clone(),
                spec.base_url.clone(),
                // 有静态表就参与比对（不只 static 源）：live 源的静态表同样是
                // 「实时拉不到时的兜底」，里面躺着已下线的模型一样会让用户选到 404。
                !spec.models.is_empty(),
                spec.models.clone(),
                spec.models_filter.clone(),
                r,
            )
        }));
    }
    let mut rows = Vec::new();
    for h in handles {
        if let Ok(x) = h.await {
            rows.push(x);
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let mut alive = 0usize;
    let mut nokey = 0usize;
    let mut dead = Vec::new();
    let mut unreachable_net = Vec::new();
    let mut throttled = Vec::new();
    let mut drift = Vec::new();
    for (id, base, has_static, declared, mfilter, r) in &rows {
        match r {
            Ok(ids) => {
                alive += 1;
                // ★ 静态清单漂移检测：static 策略的源，toml 里的模型表就是唯一真相，
                //   上游下架了模型它不会自己知道。anon-zen 里曾躺着一个已下架的
                //   minimax-m3.1-flash-free（上游清单里早没了），用户选它只会拿到 404。
                //   这类「配置与上游脱节」唯有实时比对能发现。
                    // ★ `!ids.is_empty()` 这条**不能省**：拉不到清单时 ids 是空的，
                    //   拿空集去比会让静态表里的每一个模型都被判成「已下架」。
                    //   实测 loomy 的 /models 回 HTTP 200 + {"code":"100002","desc":"缺少 token"}
                    //   （无 data），于是它整张静态表被误报成 6 个模型全下架。
                    //   空清单的含义是「这次没拿到」，不是「上游一个模型都没有」。
                // ★ 根路径探测的「ids」是 http-<状态码> 标记，**不是模型清单**：
                //   拿它去比静态表会把每一个模型都判成「已下架」（实测一次冒出 28 个假漂移）。
                let probed_root = ids.first().map(|s| s.starts_with("http-")).unwrap_or(false);
                if *has_static
                    && !declared.is_empty()
                    && mfilter.is_empty()
                    && !ids.is_empty()
                    && !probed_root
                {
                    // ★ 有 models_filter 的源**跳过比对**（如 kilo 的 isFree）。
                    //   原因：fetch_models_with_secret 返回的本来就是**过滤后**的清单，
                    //   拿它去比静态表，「静态表列了非免费模型」会被误报成「上游已下架」
                    //   —— 实测 kilo 的 kilo-auto/frontier / balanced 在上游确实存在，
                    //   只是 isFree 为假。要准确比对必须先拿到未过滤的原始清单，
                    //   而当前接口拿不到；与其误报，不如不判。
                    let gone: Vec<&String> =
                        declared.iter().filter(|m| !ids.contains(m)).collect();
                    if gone.is_empty() {
                        // 静态表是上游的子集：不报警。刻意只暴露免费档 / 某个切片的源
                        // 本来就列不全，而「少列」不会让用户拿到 404。
                        // 真正会造成失败的是下面那条（列了上游已经没有的）。
                        println!("  [OK ]  {:<22} 静态表 {} 个模型均在上游", id, declared.len());
                    } else {
                        println!("  [DRIFT] {:<22} 静态表里有上游已下架的模型", id);
                        println!("          已下架：{:?}", gone);
                        println!(
                            "          影响：这些模型会出现在 /v1/models 里，选了就是 404"
                        );
                        drift.push(id.clone());
                    }
                } else if ids.first().map(|s| s.starts_with("http-")).unwrap_or(false) {
                    // none 策略源：探的是根路径，拿到的「条目」是 http-<状态码> 标记，
                    // 不是模型清单 —— 文案要说清楚是「可达」而不是「有 N 个模型」。
                    let code = ids.first().map(|s| s.trim_start_matches("http-").to_string()).unwrap_or_default();
                    println!("  [OK ]  {:<22} 端点可达（根路径 HTTP {code}；该源无清单端点，模型靠静态表/后缀路由）", id);
                } else {
                    println!("  [OK ]  {:<22} 清单 {} 条", id, ids.len());
                }
            }
            Err((st, body)) if *st == 401 || *st == 403 => {
                nokey += 1;
                println!("  [KEY]  {:<22} HTTP {}（端点活着，本机无凭据）", id, st);
                // ★ 401 也要看一眼 body：它能区分「这个 401 是 API 端点给的」
                //   （典型是 {"error":{"code":"invalid_api_key"...}}）还是「某个
                //   网站/WAF 给的」。后者说明 base_url 可能打到了网页而不是 API ——
                //   只看状态码的话，两种都长得一样。
                let head = body.chars().take(80).collect::<String>().replace('\n', " ");
                if !head.trim().is_empty() {
                    println!("           {}", head);
                }
            }
            Err((st, body)) => {
                let why = body.chars().take(90).collect::<String>().replace('\n', " ");
                // ★ HTTP 0 = 连接层失败（超时/拒绝连接）。海外源在本机没配代理时必然
                //   落到这一类，把它和「端点真的变了」分开报，否则一次体检会冒出几十
                //   条假故障，真问题反而被淹没。
                if *st == 0 {
                    // 先分「域名没了」与「连不上」：前者是真要处理的，后者多半是没配代理。
                    if host_resolves(base) == Some(false) {
                        dead.push(id.clone());
                        println!("  [DNS]  {:<22} 域名解析失败（ENOTFOUND，源可能已下线）", id);
                        println!("           base_url = {}", base);
                    } else {
                        unreachable_net.push(id.clone());
                        // ★ 把原因摘要打出来：「证书过期」「代理拒连」「超时」在 user
                        //   眼里是同一行「连不上」，但处置完全不同（等上游续期 /
                        //   检查代理 / 重试）。旧实现只打一句固定的「超时/拒连」，
                        //   实测 api-tokenrouter 证书过期时用户会去查自己的网络。
                        let brief: String = why.chars().take(110).collect();
                        if brief.trim().is_empty() {
                            println!("  [NET]  {:<22} 连不上（超时/拒连）", id);
                        } else {
                            println!("  [NET]  {:<22} 连不上：{}", id, brief);
                        }
                    }
                } else if *st == 429 {
                    // 429 = 上游限流，是**临时状态**而不是「端点变了」。
                    // 混进「需跟进」里会让每次体检都挂着几条待办，真问题反而被稀释；
                    // 单独一类，稍后重跑即可。
                    throttled.push(id.clone());
                    println!("  [429]  {:<22} 被上游限流（稍后重跑即可）", id);
                } else {
                    dead.push(id.clone());
                    println!("  [FAIL] {:<22} HTTP {} {}", id, st, why);
                    println!("           base_url = {}", base);
                }
            }
        }
    }
    println!(
        "\n小结：通 {} / 需凭据 {} / 连不上 {} / 被限流 {} / 失败 {} / 清单漂移 {}",
        alive,
        nokey,
        unreachable_net.len(),
        throttled.len(),
        dead.len(),
        drift.len()
    );
    if !throttled.is_empty() {
        println!("被上游限流（临时状态，稍后重跑）：{}", throttled.join("，"));
    }
    if !drift.is_empty() {
        println!("静态表与上游不一致：{}", drift.join("，"));
    }
    if !unreachable_net.is_empty() {
        println!("连不上（多为海外源，需配代理后复检）：{}", unreachable_net.join("，"));
    }
    if !dead.is_empty() {
        println!("失败（端点可能已变，需跟进）：{}", dead.join("，"));
    }
    println!("注：[KEY] 不是故障（官方 API 源要粘贴 Key）；[NET] 多为本机无代理，也不是故障。");
    println!("只有 [FAIL] 是需要跟进的。");
    0
}
/// 本机回环判定（`127.0.0.0/8` / `localhost` / `::1`）。
fn is_loopback(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // ★ 不能用 `starts_with("127.")`：`127.0.0.1.evil.com` 这类域名也会命中前缀，
    //   于是「对外监听」被误判成回环 —— 令牌不会自动生成，面板裸奔在公网上。
    //   先按 IP 字面量解析，只用 `IpAddr::is_loopback`（127.0.0.0/8、::1 都在内）。
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    // 简写形式（127.1 / 127.0.1）标准库的 IpAddr 解析不接受（实测 parse 直接报错），
    // 但系统名字解析认。这里只接受「127. 开头且剩余全是数字或点」的字面量，
    // 因此 127.0.0.1.evil.com 仍会落到域名分支被拒（fail-closed：宁可多要一个令牌）。
    if let Some(rest) = h.strip_prefix("127.") {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return true;
        }
    }
    false
}

/// 32 字节随机令牌（hex），对齐 Python 版 secrets.token_urlsafe 的强度。
fn gen_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从 RUST_LOG 解析**全局**日志级别。
///
/// ★ 旧实现写死 `Level::INFO`，`RUST_LOG=debug` 完全无效，排障时什么 debug 日志都没有。
///   这里不引入 tracing-subscriber 的 env-filter feature（会拉
///   matchers/regex 等依赖），只支持全局级别：`RUST_LOG=debug` 生效，
///   `RUST_LOG=aigw_server=debug` 这种 per-target 写法按「含 debug」当全局 debug 处理。
fn log_level_from_env() -> tracing::Level {
    level_from_rust_log(&std::env::var("RUST_LOG").unwrap_or_default())
}

fn level_from_rust_log(raw: &str) -> tracing::Level {
    let raw = raw.to_ascii_lowercase();
    if raw.contains("trace") {
        tracing::Level::TRACE
    } else if raw.contains("debug") {
        tracing::Level::DEBUG
    } else if raw.contains("warn") {
        tracing::Level::WARN
    } else if raw.contains("error") {
        tracing::Level::ERROR
    } else {
        tracing::Level::INFO
    }
}

fn resolve_providers(cli: &Cli) -> Option<PathBuf> {
    if let Some(p) = &cli.providers {
        return Some(PathBuf::from(p));
    }
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("providers")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("providers"));
        }
    }
    // ★ 源码树里的 providers/ 只对本地开发有意义。不能无条件塞进去：那会把构建机的
    //   绝对路径（含用户名）编进发布二进制，在用户机器上还多一次无意义探测。
    #[cfg(debug_assertions)]
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers"));
    candidates.into_iter().find(|p| p.is_dir())
}

/// v2 数据目录：`AIGW_DATA_DIR` 优先，其次 %LOCALAPPDATA%\aigw2\data（独立于旧版，导入只读）。
///
/// 环境变量覆盖让「第二个干净实例」成为可能：想在 8801 上试一份空配置，
/// 或做便携模式（数据放 U 盘），不必动真实凭据目录。
fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("AIGW_DATA_DIR") {
        let d = d.trim();
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    std::env::var("LOCALAPPDATA")
        .map(|l| PathBuf::from(l).join("aigw2").join("data"))
        .unwrap_or_else(|_| PathBuf::from("./data"))
}

/// 旧版数据目录：%LOCALAPPDATA%\aigw-panel-rs\data
///
/// ★ 指定了 `AIGW_DATA_DIR` 就当作「独立档案」，**不做任何旧数据导入**：
///   否则想开一个干净实例（便携版/U 盘/测试实例）时，会把另一套安装的
///   账号凭据悄悄复制进新目录 —— 既违背隔离意图，也让明文 secret 多了一份拷贝。
fn legacy_dir() -> Option<PathBuf> {
    if std::env::var("AIGW_DATA_DIR")
        .map(|d| !d.trim().is_empty())
        .unwrap_or(false)
    {
        return None;
    }
    std::env::var("LOCALAPPDATA")
        .ok()
        .map(|l| PathBuf::from(l).join("aigw-panel-rs").join("data"))
        .filter(|p| p.exists())
}

/// 初始化日志：stdout + <data>/logs 下按大小滚动的文件。
///
/// 文件日志不可用（只读盘、权限不足）时**退回只写 stdout**：日志缺失是降级，
/// 不该让服务起不来。
fn init_tracing(dd: &std::path::Path) {
    let level = log_level_from_env();
    match aigw_store::rotlog::Tee::new(dd) {
        Ok(tee) => {
            // ★ 关掉 ANSI 颜色：Tee 同时写终端与落盘文件，开着颜色时转义码
            //   （\x1b[2m 等）会跟着进 aigw.log —— 面板「日志」页显示的是文件，
            //   用户复制日志贴群里就是乱码，且 grep 关键词会切开转义码
            //   归档产物就该是纯文本；终端失去配色是小事（计划任务跑的进程
            //   本来就没人盯着看）。
            tracing_subscriber::fmt()
                .with_max_level(level)
                .with_ansi(false)
                .with_writer(tee)
                .init();
        }
        Err(e) => {
            tracing_subscriber::fmt().with_max_level(level).init();
            eprintln!(
                "文件日志不可用（{}），继续只写 stdout：{e}",
                aigw_store::rotlog::log_dir(dd).display()
            );
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // ★ 三个安装模式必须互斥：旧实现是三个 if 顺序判断，先命中的直接 exit，于是
    //   `--purge --install` 会静默只做 purge（用户以为「清理并重装」），
    //   `--install --uninstall` 同理。这里明确报错而不是猜用户意图。
    let n_modes = [cli.install, cli.uninstall, cli.purge]
        .iter()
        .filter(|x| **x)
        .count();
    if n_modes > 1 {
        eprintln!("--install / --uninstall / --purge 只能同时使用一个（互斥）");
        std::process::exit(2);
    }

    // 安装模式优先：安装/卸载后退出
    if cli.install {
        std::process::exit(install::run_install());
    }
    if cli.uninstall {
        std::process::exit(install::run_uninstall());
    }
    if cli.purge {
        std::process::exit(install::run_purge());
    }

    // 日志同时落盘（<data>/logs/aigw.log，按 4MiB 滚动、留 5 份）并写 stdout。
    // 之前只有 stdout：用计划任务 Aigw2Service 启动时一行日志都没有，排障全靠猜，
    // 而手动重定向的 _run.out 又是每次启动被截断、长期运行无上限。
    init_tracing(&data_dir());

    let providers_dir = resolve_providers(&cli);
    // 用 load_dir_lenient 而不是 load_dir：要把「哪些 spec 被跳过、为什么」留下来
    // 交给 /api/health 长期展示。load_dir 只把原因写进日志，重启就滚掉了 ——
    // 一个漏写必填字段的 toml 会让整个平台静默消失（真实事故见 AppState.skipped_specs）。
    let mut skipped_specs: Vec<(String, String)> = Vec::new();
    let mut specs = match &providers_dir {
        Some(dir) => match aigw_registry::load_dir_lenient(dir) {
            Ok((v, errors)) => {
                tracing::info!("registry：{} 个平台 spec（{}）", v.len(), dir.display());
                for (file, reason) in &errors {
                    tracing::warn!("registry 跳过非法 spec {file}：{reason}");
                }
                if !errors.is_empty() {
                    tracing::warn!(
                        "registry：{} 个 spec 非法被跳过（服务继续启动，原因见 /api/health）",
                        errors.len()
                    );
                }
                skipped_specs = errors;
                v
            }
            Err(e) => {
                tracing::warn!("registry 加载失败，空表启动：{e}");
                Vec::new()
            }
        },
        None => {
            tracing::warn!("providers 目录未找到，空表启动（可用 --providers 指定）");
            Vec::new()
        }
    };

    // 体检模式：打完报告就返回，不启动服务、不写数据。
    //
    // ★ 这里必须用 return 而不是 std::process::exit：exit 在 tokio 运行时里会
    //   跳过栈上所有析构（Arc/Mutex 守卫/未完成的 spawn 任务），实测全量体检
    //   偶发 0xC0000005 —— 报告明明已打完，崩在收尾阶段。正常返回让运行时
    //   自己收尾就没有这个问题。
    if cli.doctor {
        // ★ 体检也要走代理。旧实现在这里直接 return，而 `proxy::load/apply` 在下面几十行
        //   之后 —— 于是 --doctor **永远走直连**，海外源一律报 [NET]「连不上」。
        //   而体检的首要用途正是「上游改版/反代失效时，逐个源确认谁挂了」，海外源
        //   恰恰是最需要排查的那批。读代理配置是只读操作，不违反「不写数据」的约定。
        let pcfg = aigw_relay::proxy::load(&data_dir());
        aigw_relay::proxy::apply(&pcfg);
        if pcfg.mode != "direct" {
            println!(
                "出站代理：{} {}（来自 data/proxy.json；改它用面板「代理」页或 /api/proxy）",
                pcfg.mode, pcfg.url
            );
            // ★ 预检代理本身活不活。代理进程退了但 proxy.json 还是 global 时，
            //   所有源（连国内站都不例外）都会被引向死端口 —— 实测 2026-10-27：
            //   Clash 退出后国内站 TCP 直连明明正常，体检却报 140/141 [NET]。
            //   用户对着满屏「连不上」只会以为上游全挂了，**永远想不到是自家
            //   代理软件没开** —— 报告必须把最可能的根因放在最前面说。
            if let Some((host, port)) = proxy_host_port(&pcfg.url) {
                let probe = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    tokio::net::TcpStream::connect((host.as_str(), port)),
                )
                .await;
                let dead = match &probe {
                    Ok(Ok(_)) => false,
                    Ok(Err(_)) | Err(_) => true,
                };
                if dead {
                    println!(
                        "\n⚠⚠ 代理 {}:{} 连不上（模式仍是 {}）。下面的 [NET]「连不上」大概率全是**代理软件没运行**\n    造成的，不是上游失效；国内站直连通常本来可用。先启动代理再重跑 --doctor，报告才有诊断价值。\n",
                        host, port, pcfg.mode
                    );
                }
            }
        }
        let _code = run_doctor(specs, &cli.only).await;
        return;
    }

    // settings：旧版 settings.json 只读导入占位符键（@lobster → lobster_server）
    let mut settings: std::collections::BTreeMap<String, String> = Default::default();
    if let Some(legacy) = legacy_dir() {
        let sp = legacy.join("settings.json");
        if sp.exists() {
            if let Ok(raw) = std::fs::read_to_string(&sp) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                    for k in ["lobster_server"] {
                        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
                            settings.insert(k.to_string(), s.to_string());
                        }
                    }
                }
            }
        }
    }
    aigw_registry::apply_settings(&mut specs, &settings);
    let n_placeholder = specs.iter().filter(|s| s.base_url.starts_with('@')).count();
    if n_placeholder > 0 {
        tracing::warn!("{n_placeholder} 个平台 base_url 仍为未配置占位符（对应平台请求将失败）");
    }

    let dd = data_dir();
    // 留档文件（.bak-* / *.bad-* / *.pre-dpapi-*）按族只留最新若干份：
    // 它们是排障救命用的，但恢复备份每做一次就多一组，无限增长后数据目录没法看，
    // 备份导出也会被无谓拖大。放在开库之前跑，避免与面板运行期写入抢同一批文件。
    let pruned = aigw_store::archive::prune(&dd, aigw_store::archive::KEEP_PER_FAMILY);
    if pruned > 0 {
        tracing::info!(
            "数据目录留档清理：删除 {pruned} 个旧备份/坏档（每族保留 {} 份）",
            aigw_store::archive::KEEP_PER_FAMILY
        );
    }
    let store = Arc::new(aigw_store::Store::open(&dd));
    tracing::info!(
        "store：{}（{}）",
        if store.is_db() { "SQLite" } else { "JSON 降级" },
        dd.display()
    );

    if !cli.no_import {
        if let Some(legacy) = legacy_dir() {
            match store.import_legacy(&legacy) {
                Ok(r) => tracing::info!(
                    "旧数据导入：{} 条（跳过 ts ≤ {:.0} 的既有数据）",
                    r.imported,
                    r.from_max_ts
                ),
                Err(e) => tracing::warn!("旧数据导入失败：{e}"),
            }
        }
    }

    // ★ 2026-10-26（P3）：带持久化的号池——冷却/健康/积分快照重启后恢复。
    // 否则每次重启都把「哪些账号被上游限流」归零，一上来就再吃一轮 429/402。
    let pool = aigw_pool::Pool::open(&data_dir());
    // 出站代理：启动时加载 data/proxy.json 到全局（OutClient 构造即生效；/api/proxy 可热更新）
    let pcfg = aigw_relay::proxy::load(&data_dir());
    aigw_relay::proxy::apply(&pcfg);
    if pcfg.mode != "direct" {
        tracing::info!("proxy：mode={} url={}", pcfg.mode, pcfg.url);
    }
    // ★ 2026-10-28（对齐 magpie system-prompt）：启动时加载自定义系统提示词配置。
    {
        let sp = aigw_relay::sysprompt::load(&data_dir());
        if sp.is_enabled() {
            tracing::info!(
                "sysprompt：已启用（{} 模式，注入位置 {}，模型过滤 {} 条）",
                if sp.override_existing { "覆盖" } else { "仅缺省时" },
                if sp.prepend { "前置" } else { "追加" },
                sp.models.len()
            );
        }
        aigw_relay::sysprompt::apply_cfg(&sp);
    }
    // ★ 2026-10-28（对齐 magpie history-trim）：启动时加载历史裁剪配置（默认开启）。
    {
        let ht = aigw_relay::history_trim::load(&data_dir());
        if ht.compress_tool_results {
            tracing::info!(
                "history_trim：已开启（工具结果上限 {} 字符，system 保护 {}）",
                ht.max_tool_len,
                if ht.preserve_system { "开" } else { "关" }
            );
        }
        aigw_relay::history_trim::apply_cfg(&ht, ht.compress_tool_results);
    }
    // ★ 2026-10-28（对齐 magpie model-map）：启动时加载模型重定向规则。
    {
        let (rules, keep) = aigw_relay::model_map::load(&data_dir());
        if !rules.is_empty() {
            tracing::info!("model_map：{} 条重定向规则已加载", rules.len());
        }
        aigw_relay::model_map::apply_cfg(&rules, keep);
    }
    // ★ 2026-10-28（对齐 magpie param-override）：启动时加载参数覆盖。
    {
        let po = aigw_relay::param_override::load(&data_dir());
        if !po.is_empty() {
            tracing::info!("param_override：{} 项直设 + {} 条操作已加载", po.direct.len(), po.ops.len());
        }
        aigw_relay::param_override::apply_cfg(&po, !po.is_empty());
    }
    // ★ 2026-10-28（对齐 magpie think-tags）：启动时加载思考标签模式。
    {
        let tt = aigw_relay::think_tags::load(&data_dir());
        if let Some(m) = &tt {
            tracing::info!("think_tags：模式 {} 已启用", m);
        }
        aigw_relay::think_tags::apply_cfg(tt.as_deref());
    }
    let client = Arc::new(aigw_relay::OutClient::new());
    // 账号池：双源装载（v2 data/accounts.json 权威；首次从 legacy 只读初始化）
    let legacy_accounts = legacy_dir()
        .map(|l| l.join("accounts.json"))
        .unwrap_or_else(|| PathBuf::from("accounts-legacy.json"));
    let own_accounts = dd.join("accounts.json");
    let accounts = Arc::new(aigw_store::AccountStore::load_dual(
        &legacy_accounts,
        &own_accounts,
    ));
    // 自定义 API 存储（data/custom.json）
    let custom = Arc::new(aigw_store::CustomStore::open(&dd));
    // 公益站聚合存储（data/free_sites.json）
    let free = Arc::new(aigw_store::FreeStore::open(&dd));
    // 访问密钥（data/keys.json）：带数据目录打开，否则内存里建出来的 key 重启即丢
    // —— 用户会以为「建好了能用了」，直到重启后全部 401 才发现没落盘。
    let keys = aigw_store::KeyStore::open(&dd);
    // 模型链（data/combos.json）：同样必须带目录打开，否则建好的链重启即丢。
    let combos = aigw_store::ComboStore::open(&dd);
    // OAuth 本地回调监听（Codex 1455 / Claude 54545）——跳转登录自动捕获
    aigw_relay::auth_login::start_callback_listeners();
    // ---- 门禁策略 ----
    // Host/Origin 白名单：回环名 + 绑定地址本身（对外监听 --host 0.0.0.0 时，
    // 用户仍可用本机 IP 访问面板）。
    let mut allow_hosts: Vec<String> = vec![
        "127.0.0.1".into(),
        "localhost".into(),
        "::1".into(),
        "[::1]".into(),
    ];
    let bound_host = cli.host.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    // 通配监听（0.0.0.0 / ::）：浏览器发来的 Host 是真实局域网 IP，把 "0.0.0.0"
    // 塞进白名单没有意义。改为「任意 IP 字面量放行、域名一律拒绝」——
    // 局域网正常可用，同时 DNS 重绑定（域名 → 127.0.0.1）仍被挡住。
    let wildcard = bound_host == "0.0.0.0" || bound_host == "::" || bound_host.is_empty();
    if !wildcard && !allow_hosts.iter().any(|h| h == &bound_host) {
        allow_hosts.push(bound_host.clone());
    }
    // 面板里设置/轮换过的令牌（`/api/token` 写在 data/panel-token.txt）：
    // 优先级低于 --token 与环境变量，但**高于对外监听自动生成** —— 面板里明确设过的
    // 令牌重启后必须还在，否则「设了令牌 → 重启 → 又裸奔」或「换了令牌 → 自己进不去」。
    // 文件里是 DPAPI 密文（purpose=panel）。历史版本写的是明文，unwrap_with 会原样返回，
    // 所以升级不需要迁移。解不开（换机器/换用户）时不启用令牌，并在日志里说清楚怎么办。
    let panel_token: Option<String> = std::fs::read_to_string(dd.join("panel-token.txt"))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .and_then(|t| match aigw_store::secret::unwrap_with(&t, "panel") {
            Ok(p) if !p.trim().is_empty() => Some(p.trim().to_string()),
            Ok(_) => None,
            Err(e) => {
                tracing::error!(
                    "data/panel-token.txt 解不开（{e}）；本次不启用面板令牌。要恢复鉴权：删掉该文件后用 /api/token 重新生成，或直接用 --token 指定"
                );
                None
            }
        });
    // 令牌：显式 --token > 环境变量 AIGW_PANEL_TOKEN > 对外监听时自动生成。
    // 来源单独记下来给面板用：令牌值不外泄，但「要不要令牌 + 从哪来」必须在
    // /api/health 里说清楚，否则用户看到 /v1 401 只能猜。
    let (token, token_src): (Option<String>, &'static str) = match cli
        .token
        .clone()
        .filter(|t| !t.trim().is_empty())
    {
        Some(t) => {
            tracing::info!("面板令牌已启用：/api/* 与 /v1/* 需携带 X-AIGW-Token");
            (Some(t), "cli")
        }
        None => match std::env::var("AIGW_PANEL_TOKEN") {
            Ok(t) if !t.trim().is_empty() => {
                tracing::info!("面板令牌已启用（AIGW_PANEL_TOKEN）：/api/* 与 /v1/* 需携带 X-AIGW-Token");
                (Some(t), "env")
            }
            _ if panel_token.is_some() => {
                tracing::info!(
                    "面板令牌已启用（来自 data/panel-token.txt，可用 /api/token 轮换）：/api/* 与 /v1/* 需携带 X-AIGW-Token"
                );
                (panel_token.clone(), "panel")
            }
            _ if !is_loopback(&cli.host) => {
                let t = gen_token();
                tracing::warn!(
                    "监听 {} 为非回环地址且未设置令牌：已自动生成面板令牌 {}（请用 X-AIGW-Token 携带；下次启动需重新指定）",
                    cli.host,
                    t
                );
                (Some(t), "auto")
            }
            _ => (None, ""),
        },
    };

    let flags = Arc::new(aigw_store::FlagStore::open(&dd));
    // client / free 还要给后台刷新任务留一份，这里交给它 clone（Arc 很便宜）。
    let state = aigw_server::AppState::new(
        specs,
        store,
        pool,
        client.clone(),
        accounts,
        custom,
        free.clone(),
    )
        .with_skipped_specs(skipped_specs)
        .with_flags(flags)
        .with_security(token, allow_hosts)
        .with_token_source(token_src)
        .with_keys(keys)
        .with_combos(combos)
        .with_data_dir(dd.clone())
        .with_limits(cli.ip_rpm, cli.rpm)
        .with_key_rate(cli.key_rpm)
        .with_wildcard_host(wildcard);
    // 后台预热 live 模型清单：冷启动后第一条 chat 若撞上空缓存，路由要现场拉
    // 上游（实测 26s 才出第一条消息）。放到启动后台跑，用户提问时已是热路径。
    // 带监管：预热 panic 不影响进程，结果进 /api/health 的 tasks.warm_models。
    tokio::spawn(state.clone().warm_models_supervised());
    // 公益站每日自动刷新。
    //
    // ★ 旧实现是「interval + loop + 直接 await」：`refresh_free_sites` 里任何一次 panic 都会让
    //   这个 tokio 任务永久消失 —— 进程还活着、/api/health 还是 ok，但公益站再也不会刷新，
    //   界面上没有任何迹象。现在走 `supervise_loop`：panic 被隔离并继续调度，失败按 10 分钟
    //   重试（网络抖动不该让刷新停摆整整一天），最近成功/失败时间进 /api/health 的 tasks。
    {
        let free2 = free.clone();
        let client2 = client.clone();
        let tasks = state.tasks.clone();
        // 聚合源（内置 + data/gongyi-sources.json）：启动时算一次，之后每轮复用。
        let sources = aigw_server::gongyi_source_list(Some(dd.as_path()));
        // 内置 providers/free-*.toml 已覆盖的 host：聚合页里同一个站不再重复入库
        // （否则 id 不同 → 面板上两张一模一样的卡片）。
        let static_hosts = aigw_server::static_free_hosts(&state.specs);
        // ★ 用 supervise_loop_now：先跑一轮再进入 24 小时周期。旧写法先睡 24 小时，
        //   全新部署当天 /api/health 与面板一直是「尚未成功过」，用户只能手动点刷新。
        // 先声明这条台账：错峰 10 秒期间 /api/health 里也不能整条任务消失
        // （panel 会把缺失的任务当成「没这个功能」，而不是「还没开始」）。
        state.tasks.declare("free_refresh");
        // 先订阅再 spawn：接收端在预热还没结束时就已经存在，不会漏掉信号；
        // 而且 `state` 随后还要交给 build_app，不能 move 进 spawn 里。
        let warm_rx0 = state.warm_done_rx();
        tokio::spawn(async move {
            // ★ 冷启动错峰：warm_models 在 0 秒就开始并发拉 80 个平台的模型清单，
            //   公益站刷新再同时打聚合源，会让启动后头十几秒的出站连接被两批占满。
            //   旧实现固定 sleep 10 秒：预热只用 3 秒时白等 7 秒（面板刷新按钮那栏
            //   会在「预热早就完了」之后仍然空白一段时间），预热要 12 秒时又照样撞车。
            //   现在等 `warm_done` 信号：预热一结束就起跑，最坏等 WARM_WAIT_CAP。
            // 注意：这里不能用 `warm_rx.wait_for(|done| *done)` —— 它的 future 持着
            // `watch::Ref`，不是 Send，套进 `tokio::spawn` 会编译失败（「future cannot be
            // sent between threads safely」）。用 changed() 轮询，Ref 不跨 await。
            let mut warm_rx = warm_rx0;
            let waited = tokio::time::timeout(aigw_server::WARM_WAIT_CAP, async {
                loop {
                    if *warm_rx.borrow_and_update() {
                        break;
                    }
                    if warm_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
            if waited.is_err() {
                tracing::warn!(
                    "模型预热 {}s 内没结束，公益站刷新不再等它（避免刷新被无限推迟）",
                    aigw_server::WARM_WAIT_CAP.as_secs()
                );
            }
            aigw_server::supervise_loop_now(
            "free_refresh",
            tasks,
            std::time::Duration::from_secs(24 * 3600),
            std::time::Duration::from_secs(10 * 60),
            move || {
                let (c, f, src, sh) = (
                    client2.clone(),
                    free2.clone(),
                    sources.clone(),
                    static_hosts.clone(),
                );
                async move {
                    // 单飞：手动刷新与定时任务共用一条链路，撞上了就说清楚被跳过，
                    // 不计失败（10 分钟后重试的语义在这里会变成无意义的重抓）。
                    match aigw_server::refresh_free_sites_guarded(&c, &f, &src, &sh).await? {
                        aigw_server::FreeRefresh::Busy => Ok("上一轮刷新仍在进行，本轮跳过".to_string()),
                        aigw_server::FreeRefresh::Done(t, a, k, st) => {
                            let note = st.sources_note();
                            Ok(if note.is_empty() {
                                format!("共 {t} 站，新增 {a}，保留 {k}；{}", st.detail())
                            } else {
                                format!("共 {t} 站，新增 {a}，保留 {k}；{}；{note}", st.detail())
                            })
                        }
                    }
                }
            },
            )
            .await;
        });
    }
    // wb 族每日自动签到：默认关（自动去上游领积分是对第三方的写操作，须用户在
    // 面板「账号 → 自动签到」显式开启）。开启后启动即跑一轮、每 12 小时一轮。
    // 不用 24h：周期从启动时刻起算会漂移，12h 保证每个日历日至少落一次；
    // 当天已领过时上游回 code=2「今日已领取」，多跑无副作用。
    // ★ 单账号失败不报 Err：Err 会触发 30 分钟高频重试，凭据过期时等于拿失效
    //   token 反复敲上游签到接口（风控风险）；失败写进 last_ok 汇总，面板可见。
    {
        let tasks = state.tasks.clone();
        let st2 = state.clone();
        state.tasks.declare("daily_checkin");
        tokio::spawn(async move {
            aigw_server::supervise_loop_now(
                "daily_checkin",
                tasks,
                std::time::Duration::from_secs(12 * 3600),
                std::time::Duration::from_secs(30 * 60),
                move || {
                    let st3 = st2.clone();
                    async move {
                        if !st3.flags.daily_checkin() {
                            return Ok(
                                "自动签到未开启（面板账号页「自动签到」可打开），跳过".to_string()
                            );
                        }
                        let v = aigw_server::checkin_all(&st3, true).await;
                        Ok(aigw_server::summarize_checkin(&v))
                    }
                },
            )
            .await;
        });
    }
    // ★ 回灌在线倍率缓存（data/model-rates.json）：上次刷新成功过的话，
    //   启动即用在线值，不用等第一个 6 小时周期。文件缺/坏静默用离线表。
    aigw_server::model_rates::load_cache(&dd);
    // ★ Token 限制器规则回灌：限制存于 accounts.json（账号字段），号池事实表
    //   在内存——启动必须把规则装回池里，否则「重启一次限制就失效」。
    {
        let snap = state.accounts.rows_snapshot();
        if let Some(rows) = snap.get("rows").and_then(|r| r.as_array()) {
            for row in rows {
                let id = row.get("id").and_then(|x| x.as_str()).unwrap_or("");
                let limit = row.get("token_limit").and_then(|x| x.as_u64());
                let period = row.get("token_period_s").and_then(|x| x.as_i64()).unwrap_or(3600);
                if !id.is_empty() {
                    if let Some(l) = limit {
                        state.pool.set_token_rule(id, Some(l), period);
                    }
                }
            }
        }
    }
    // ★ 2026-10-27：积分快照周期刷新（只读查询，无需开关）。快照 TTL 25h，
    //   6h 一轮 —— 用户不点「积分」不签到时，「快到期优先」与积分总览条也有数。
    {
        let tasks = state.tasks.clone();
        let st7 = state.clone();
        state.tasks.declare("credits_refresh");
        tokio::spawn(async move {
            aigw_server::supervise_loop_now(
                "credits_refresh",
                tasks,
                std::time::Duration::from_secs(6 * 3600),
                std::time::Duration::from_secs(30 * 60),
                move || {
                    let st8 = st7.clone();
                    async move { Ok(aigw_server::refresh_credits_all(&st8).await) }
                },
            )
            .await;
        });
    }
    // ★ 2026-10-26（P3）：号池状态定期落盘。冷却/健康/积分快照每 60 秒存一次，
    // 进程被强杀最多丢 60 秒的治理状态——比「重启全丢」好得多。
    {
        let pool = state.pool.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                pool.persist();
            }
        });
    }
    // ★ 2026-10-26（P0）：wb 族成长任务自动完成（对齐 workbuddy2api 17 任务闭环）。
    // 同样默认关（对上游做写操作），由面板开关显式打开，周期 12h。
    {
        let tasks = state.tasks.clone();
        let st5 = state.clone();
        state.tasks.declare("daily_growth");
        tokio::spawn(async move {
            aigw_server::supervise_loop_now(
                "daily_growth",
                tasks,
                std::time::Duration::from_secs(12 * 3600),
                std::time::Duration::from_secs(30 * 60),
                move || {
                    let st6 = st5.clone();
                    async move {
                        if !st6.flags.daily_growth() {
                            return Ok("自动成长任务未开启（面板账号页可打开），跳过".to_string());
                        }
                        let mut lines: Vec<String> = Vec::new();
                        // ★ 按 **signer == "wb"** 枚举，不写死平台 id：
                        //   实测漏过一次 —— 列表里只有 wb-gateway/codebuddy（后者还是个
                        //   不存在的历史遗留），**国际版 wb-gateway-intl 从来没跑过成长任务**；
                        //   而且 web_base 写死成 workbuddy.cn，就算遍历到 intl 也会打错域。
                        //   spec.domain 是站点域的唯一来源（wb-gateway=workbuddy.cn /
                        //   wb-gateway-intl=workbuddy.ai / codebuddy=codebuddy.cn）。
                        let wb_specs: Vec<aigw_registry::ProviderSpec> = st6
                            .all_specs()
                            .into_iter()
                            .filter(|s| s.signer == "wb")
                            .collect();
                        for spec in wb_specs {
                            let platform = spec.id.clone();
                            let Some(base) = aigw_relay::wb::wb_checkin_base(&platform) else {
                                continue;
                            };
                            let Some(web_base) = aigw_relay::wb::web_base_for(&spec) else {
                                continue;
                            };
                            let web_base = web_base.as_str();
                            let http = aigw_relay::auth_login::auth_http();
                            for (acc_id, secret) in st6.accounts.secrets_for(&platform) {
                                let nick = st6
                                    .accounts
                                    .rows_snapshot()
                                    .get("rows")
                                    .and_then(|r| r.as_array())
                                    .and_then(|arr| {
                                        arr.iter().find(|a| {
                                            a.get("id").and_then(|x| x.as_str()) == Some(acc_id.as_str())
                                        })
                                    })
                                    .and_then(|a| a.get("name"))
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let r = aigw_relay::wb_growth::run_all_growth(
                                    &http, base, web_base, &secret, &acc_id, &nick,
                                )
                                .await;
                                let ok = r.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
                                let credit = r.get("credit").and_then(|x| x.as_i64()).unwrap_or(0);
                                let claimed = r.get("claimed").and_then(|x| x.as_i64()).unwrap_or(0);
                                lines.push(format!(
                                    "{platform}/{acc_id}: {} 领取 {claimed} 项 +{credit} 积分",
                                    if ok { "✓" } else { "✗" }
                                ));
                            }
                        }
                        if lines.is_empty() {
                            return Ok("没有 wb 族账号，跳过".to_string());
                        }
                        Ok(lines.join("；"))
                    }
                },
            )
            .await;
        });
    }
    // 自定义 API 的模型清单定时刷新。
    //
    // 清单原本只在「新建 / 编辑 / 手动同步」时更新，之后一直冻着：本机聚合桥重启后
    // 换了一批模型，面板仍按旧清单路由 —— 模型列表里明明有，一发就 404。实测过
    // 一次 /v1/models 从 148 掉到 67，只有手动点「同步模型」才恢复。
    //
    // 6 小时一轮：桥/中转站的模型改动不是实时需求，但「一整天用着过期清单」必须消除。
    // 失败按 10 分钟重试（上游暂时不可达时不该等 6 小时）。
    // ★ 等预热结束再跑，避免启动当头三批出站连接（预热 / 公益站 / 本任务）互抢。
    {
        let tasks = state.tasks.clone();
        let st4 = state.clone();
        state.tasks.declare("custom_refresh");
        let mut warm_rx = state.warm_done_rx();
        tokio::spawn(async move {
            // 同 free_refresh：`wait_for` 的 future 持着 watch::Ref 不是 Send，改用轮询。
            let _ = tokio::time::timeout(aigw_server::WARM_WAIT_CAP, async {
                loop {
                    if *warm_rx.borrow_and_update() {
                        break;
                    }
                    if warm_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
            aigw_server::supervise_loop_now(
                "custom_refresh",
                tasks,
                std::time::Duration::from_secs(6 * 3600),
                std::time::Duration::from_secs(10 * 60),
                move || {
                    let st5 = st4.clone();
                    async move { st5.refresh_custom_models_now().await }
                },
            )
            .await;
        });
    }
    let app = aigw_server::build_app(state);

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定 {addr} 失败：{e}（已有实例在跑？换 --port 试试）");
            std::process::exit(1);
        }
    };
    tracing::info!("aigw v2 final listening on http://{addr}");
    // `into_make_service_with_connect_info` 让 guard 能拿到对端 IP（限流按 IP 计数）。
    if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await {
        eprintln!("serve error: {e}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    /// 代理 URL 抠 host:port：预检要连的端点得解析对，三种形态 + 两个非法。
    #[test]
    fn parses_common_proxy_urls_for_precheck() {
        assert_eq!(proxy_host_port("http://127.0.0.1:7890"), Some(("127.0.0.1".into(), 7890u16)));
        assert_eq!(proxy_host_port("http://u:p@127.0.0.1:7897"), Some(("127.0.0.1".into(), 7897u16)));
        assert_eq!(proxy_host_port("http://proxy.local"), Some(("proxy.local".into(), 80u16)));
        assert_eq!(proxy_host_port(""), None);
        assert_eq!(proxy_host_port("http://h:notaport"), None);
    }

    use super::*;

    #[test]
    fn rust_log_global_level_is_honored() {
        // 旧实现写死 INFO：RUST_LOG 无论写什么都不生效，debug 日志永远看不到。
        assert_eq!(level_from_rust_log(""), tracing::Level::INFO);
        assert_eq!(level_from_rust_log("info"), tracing::Level::INFO);
        assert_eq!(level_from_rust_log("debug"), tracing::Level::DEBUG);
        assert_eq!(level_from_rust_log("aigw_server=debug"), tracing::Level::DEBUG);
        assert_eq!(level_from_rust_log("trace"), tracing::Level::TRACE);
        assert_eq!(level_from_rust_log("warn"), tracing::Level::WARN);
        assert_eq!(level_from_rust_log("ERROR"), tracing::Level::ERROR);
        // 更详细的级别优先：同时出现 debug 与 warn 时应当按 debug 放行。
        assert_eq!(level_from_rust_log("warn,aigw_debug=debug"), tracing::Level::DEBUG);
    }

    #[test]
    fn loopback_detection_rejects_lookalike_hosts() {
        // 旧实现 starts_with("127.") 会把这类域名判成回环 → 对外监听不生成令牌。
        assert!(!is_loopback("127.0.0.1.evil.com"));
        assert!(!is_loopback("localhost.evil.com"));
        assert!(is_loopback("127.0.0.1"));
        // 简写形式：IpAddr::parse 不接受，但系统名字解析认 127.1，所以单独放行。
        assert!(is_loopback("127.1"));
        assert!(is_loopback("127.0.1"));
        // 纯数字/点但非 127 段的不算回环。
        assert!(!is_loopback("128.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(is_loopback("[::1]"));
    }

    #[test]
    fn generated_token_is_hex_and_long_enough() {
        let t = gen_token();
        assert_eq!(t.len(), 48, "24 字节 hex 应是 48 字符");
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, gen_token());
    }
}
