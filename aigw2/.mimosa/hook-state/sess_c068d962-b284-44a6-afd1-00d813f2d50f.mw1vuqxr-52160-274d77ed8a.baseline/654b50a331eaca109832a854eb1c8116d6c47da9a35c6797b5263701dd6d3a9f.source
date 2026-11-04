//! 公益站聚合：抓取外部导航页 → 解析 JSON-LD → 过滤 → 入库。
//!
//! 从 lib.rs 拆出（2026-10-26 第二十轮）。本模块只做「聚合页 → 站点条目」这一件事，
//! 与面板路由无关的部分（host 准入 / slug / 抓取重试）都是纯函数，可单测。
//!
//! ★ 安全前提：聚合页是**外部不可信输入**，里面每条 url 都会被当成上游站点入库。
//! 因此凡是能变成请求目标的东西（host 与 scheme）都必须先过 gongyi_host_ok 与
//! scheme_ok，尤其要挡掉云元数据端点——面板会带着用户填的 Key 去请求它。
//!
//! ★ 多源回退的可解释性是刚需：聚合站挂掉时，面板要能说清「正式站挂了、已回退到
//! raw 源」还是「全挂了」，所以每个源的尝试结果都留在 GongyiStats::sources 里。

use crate::net::{blocked_upstream_host, scheme_ok, site_host, url_host};
use serde_json::Value;

// ---- 公益站聚合（手动刷新 + 每日自动） ----

/// 聚合源（静态导航页，页面内嵌 JSON-LD itemListElement：name+url）。
///
/// 第一个是正式站（GitHub Pages），第二个是**同一仓库**在 raw.githubusercontent.com
/// 上的原始 index.html（实测同样含 itemListElement）。两个源分属不同域名/不同 CDN，
/// Pages 构建抖动、证书过期、自定义域名改绑时第二个仍能刷新 —— 旧实现只有第一个，
/// 它一挂「公益站刷新」就整轮失败，只能等聚合站自己恢复。
///
/// 顺序即优先级：按顺序尝试，先成功的那份生效（失败明细进任务台账与 /api/free）。
pub(crate) const GONGYI_SOURCES: &[&str] = &[
    "https://bubblevv.github.io/ai-api-gongyi-nav/",
    "https://raw.githubusercontent.com/bubblevv/ai-api-gongyi-nav/main/index.html",
];

/// 追加自定义聚合源：`data/gongyi-sources.json`（`{"urls":["https://…"]}` 或裸数组）。
///
/// 内置源只收验证过含 itemListElement 的地址 —— 往代码里塞未经核对的镜像，
/// 只会在真出故障时一起 404。用户自己的镜像/内网静态页写这个文件即可，追加在内置源之后。
pub(crate) const GONGYI_SOURCES_FILE: &str = "gongyi-sources.json";

/// 聚合源数量上限：串行尝试、每源 30s 超时，源太多会把一次刷新拖成几分钟。
pub(crate) const MAX_GONGYI_SOURCES: usize = 6;

/// 单批公益站上限：聚合页是外部输入，被投毒塞几千条会把 /v1/models 与
/// 路由表撑爆，也让面板变成别人的扫描器。
pub(crate) const MAX_GONGYI_SITES: usize = 200;

/// JSON-LD ListItem（聚合页内嵌）。
#[derive(Debug, serde::Deserialize)]
pub(crate) struct GongyiItem {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) url: String,
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
pub(crate) fn slug_of(host: &str) -> String {
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
pub(crate) fn host_of(u: &str) -> Option<String> {
    let rest = u.split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    Some(host.to_string())
}


/// 聚合源 URL 的合规检查：只收 http(s)、主机可解析、且不是云元数据端点。
///
/// **回环放行**：自建镜像常见于本机 https://127.0.0.1:8443，那是用户自己的静态页，
/// 与「聚合页被投毒塞进来一个站」不是一回事（后者由 `gongyi_host_ok` 单独拦）。
pub(crate) fn gongyi_source_ok(raw: &str) -> Option<String> {
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
pub(crate) fn short_host(u: &str) -> String {
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
pub(crate) fn parse_gongyi_items(html: &str) -> Result<Vec<GongyiItem>, String> {
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
pub(crate) const GONGYI_FETCH_TRIES: usize = 3;


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
pub(crate) async fn fetch_one_source(
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
pub(crate) fn combine_gongyi_attempts(
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
pub(crate) fn gongyi_items_to_sites(items: Vec<GongyiItem>) -> (Vec<aigw_store::FreeSite>, GongyiStats) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
