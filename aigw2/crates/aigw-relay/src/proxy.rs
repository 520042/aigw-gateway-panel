//! 出站网络代理配置（用户 2026-10 拍板：设置代理后所有流程都从代理走）。
//!
//! 三模式：
//! - `direct`：全部直连（默认，与旧版行为一致）
//! - `global`：全部走代理（CODEX/Claude/Grok 等海外平台必需）
//! - `bypass_cn`：大陆域名直连 + 其余走代理（"绕过大陆后代理"）
//!
//! 配置持久化 `data/proxy.json`；运行时放全局 `PROXY_CFG`，
//! reqwest `Proxy::custom` 闭包每次请求读最新值 → **配置热更新，无需重建 client**。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

/// 代理配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxyConfig {
    /// direct | global | bypass_cn
    #[serde(default = "default_mode")]
    pub mode: String,
    /// 代理地址，如 http://127.0.0.1:7890（direct 模式忽略）
    #[serde(default)]
    pub url: String,
}

fn default_mode() -> String {
    "direct".to_string()
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            mode: "direct".into(),
            url: String::new(),
        }
    }
}

/// 全局运行时配置（OnceLock+RwLock：构建 client 后仍可热更新）。
static PROXY_CFG: OnceLock<RwLock<ProxyConfig>> = OnceLock::new();

fn global_cfg() -> &'static RwLock<ProxyConfig> {
    PROXY_CFG.get_or_init(|| RwLock::new(ProxyConfig::default()))
}

/// 默认数据目录（与 main.rs 同逻辑）：`AIGW_DATA_DIR` 优先，其次
/// %LOCALAPPDATA%\aigw2\data，缺省 ./data。
pub fn default_data_dir() -> PathBuf {
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

/// 从 data 目录加载（不存在 → direct 默认）。
///
/// 文件存在但解析失败时**必须出声**：静默回退 direct 意味着用户以为流量在走代理，
/// 实际全部直连（海外平台直接失败，敏感请求还会绕过用户特意配的出口）。
pub fn load(data_dir: &Path) -> ProxyConfig {
    let p = data_dir.join("proxy.json");
    match std::fs::read_to_string(&p) {
        Ok(raw) => match serde_json::from_str::<ProxyConfig>(&raw) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "proxy.json 解析失败（{e}），本次按 direct 启动；请检查 {}",
                    p.display()
                );
                ProxyConfig::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ProxyConfig::default(),
        Err(e) => {
            tracing::warn!("proxy.json 读取失败（{e}），本次按 direct 启动");
            ProxyConfig::default()
        }
    }
}

/// 保存到 data/proxy.json 并**热更新全局配置**。
pub fn save_and_apply(data_dir: &Path, cfg: &ProxyConfig) -> std::io::Result<()> {
    let p = data_dir.join("proxy.json");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 原子写（tmp + rename）：避免写一半留下非法 proxy.json 导致启动回退 direct
    aigw_store::write_json_atomic(&p, cfg)?;
    apply(cfg);
    Ok(())
}

/// 应用配置到全局（进程内热更新；main 启动时也调一次对齐磁盘值）。
pub fn apply(cfg: &ProxyConfig) {
    // 锁中毒不能让「保存代理」静默失效：面板回 200、用户以为已切到代理，
    // 实际后续请求仍按旧配置（或 direct）出网。
    let mut g = global_cfg().write().unwrap_or_else(|e| e.into_inner());
    *g = cfg.clone();
}

/// 读当前运行时配置。
///
/// 中毒时旧实现 `.map(..).unwrap_or_default()` 会**静默退回 direct**：
/// 用户配了代理却整段流量直连，是最不该静默失败的一条路径。
pub fn current() -> ProxyConfig {
    global_cfg()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 本机 / 内网地址判定：这些地址**任何模式都不该走代理**。
///
/// 为什么单独成一个函数、且在模式判断之前就短路：
/// - 代理进程拿到 `127.0.0.1`/`192.168.x` 只会连到**它自己**那台机器的同名地址，
///   永远到不了用户本机或用户局域网。配了 global 代理后接一个本机 OpenAI 兼容网关
///   （常见用法），会被静默改成对代理主机的回环请求，报的还是连不上 —— 无从排查。
/// - 把内网主机名丢给远端代理，等于把自己的内网拓扑写进第三方日志。
///
/// 覆盖：IPv4 回环/私网/链路本地/未指定、IPv6 回环/唯一本地/链路本地、IPv4 映射的 IPv6、
/// `localhost` 与 `.localhost`、`.local`、`.lan`、`.internal`、`.home.arpa`。
/// 注意 URL 的 IPv6 主机是带方括号的（`[::1]`），必须先去括号再解析。
fn host_is_local_or_lan(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let h = h.trim_end_matches('.'); // FQDN 尾点（"localhost."）与不带点等价
    if h.is_empty() {
        return true;
    }
    let bare = h.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            std::net::IpAddr::V6(v6) => {
                if v6.is_loopback() || v6.is_unspecified() {
                    return true;
                }
                // ::ffff:a.b.c.d 走 IPv4 的私网判断（否则 127.0.0.1 的映射写法会漏网）
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified();
                }
                let seg = v6.segments()[0];
                (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
            }
        };
    }
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    const LAN_SUFFIX: &[&str] = &[".local", ".lan", ".internal", ".home.arpa"];
    LAN_SUFFIX.iter().any(|s| h.ends_with(s))
}

/// 域名决策：给定 host，返回是否**直连**（bypass_cn 模式下大陆域名 → true）。
fn should_direct(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    if host_is_local_or_lan(&h) {
        return true;
    }
    if h.ends_with(".cn") {
        return true;
    }
    const CN_DOMAINS: &[&str] = &[
        "qq.com", "baidu.com", "taobao.com", "tmall.com", "aliyun.com", "jd.com",
        "weibo.com", "douyin.com", "bytedance.com", "toutiao.com", "xiaohongshu.com",
        "bilibili.com", "zhihu.com", "163.com", "126.com", "sina.com", "sohu.com",
        "tencent.com", "woa.com", "doubao.com", "volcengine.com", "volces.com",
        "feishu.cn", "larkoffice.com", "larksuite.com", "kuaishou.com", "meituan.com",
        "dianping.com", "ele.me", "gitee.com", "csdn.net", "zhipin.com", "51job.com",
        "moonshot.cn", "minimaxi.com", "minimax.chat", "chatglm.cn", "zhipuai.cn",
        "bigmodel.cn", "siliconflow.cn", "trae.cn", "dingtalk.com", "alipay.com",
        "kimi.moonshot.cn", "maas.bytedance.com", "ark.cn-beijing.volces.com",
        // ★ 2026-10 补齐：这些是「只有国内出口才连得通」的厂商域名，旧列表漏了
        //   aliyuncs / baidubce / myhuaweicloud / xf-yun 四大云 —— 它们在 bypass_cn
        //   模式下会被送到代理，结果 dashscope、千帆、华为云 MaaS、讯飞星火**整条平台全废**，
        //   而用户以为自己配的代理是"绕过大陆"。反过来少判一个只会慢一点，多判一个会不可用，
        //   所以这里按厂商根域整段收。
        "aliyuncs.com", "alicdn.com", "baidubce.com", "huaweicloud.com",
        "myhuaweicloud.com", "tencentcloudapi.com", "xf-yun.com", "iflytek.com",
        "baichuan-ai.com", "stepfun.com", "xiaomimimo.com", "youdao.com",
        "deepseek.com", "kimi.com", "01.ai", "longcat.chat", "sensetime.com",
    ];
    CN_DOMAINS
        .iter()
        .any(|d| h == *d || h.ends_with(&format!(".{}", d)))
}

/// 单个 URL 的代理决策（纯函数，便于测试）：Some(代理地址) / None(直连)。
///
/// 每次请求都会调用，所以读的是**当前** `PROXY_CFG`。
pub fn proxy_url_for(u: &reqwest::Url) -> Option<reqwest::Url> {
    decide(&current(), u)
}

/// 纯决策函数：给定配置与 URL 出代理地址。抽出来是为了**可测**——
/// 走全局 `PROXY_CFG` 的测试并行跑时会互相踩（实测过一个用例被另一个用例
/// 切成 direct 而误报失败），配置作为参数传进来就没有共享状态。
pub(crate) fn decide(cfg: &ProxyConfig, u: &reqwest::Url) -> Option<reqwest::Url> {
    if cfg.mode == "direct" || cfg.url.is_empty() {
        return None;
    }
    let host = u.host_str().unwrap_or("");
    // 本机/内网在任何模式下都直连：代理够不到它，硬走代理只会得到连不上的假故障。
    if host_is_local_or_lan(host) {
        return None;
    }
    if cfg.mode == "bypass_cn" && should_direct(host) {
        return None;
    }
    reqwest::Url::parse(&cfg.url).ok()
}

/// 构建 reqwest Proxy：**每次请求**都按当前配置决策（None = 直连）。
///
/// 必须用 `Proxy::custom` 而不是 `Proxy::all(url)`：
/// `reqwest::Client` 在 build 时就把 proxy 固化进去了，`Proxy::all` 拿到的是构建那一刻的
/// 固定 URL，之后改 `proxy.json`（global → direct）**不会生效**，只能重启进程。
/// 走 custom 闭包才能在请求发起时读 `PROXY_CFG`，做到真正的热更新。
pub fn proxy_for_current() -> Option<reqwest::Proxy> {
    Some(reqwest::Proxy::custom(|u: &reqwest::Url| proxy_url_for(u)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_host_detection() {
        assert!(should_direct("api.baidu.com"));
        assert!(should_direct("chatglm.cn"));
        assert!(should_direct("kimi.moonshot.cn"));
        assert!(should_direct("doubao.com"));
        assert!(should_direct("127.0.0.1"));
        assert!(!should_direct("api.openai.com"));
        assert!(!should_direct("api.anthropic.com"));
        assert!(!should_direct("grok.com"));
        assert!(!should_direct("accounts.google.com"));
        assert!(!should_direct("api.x.ai"));
        assert!(!should_direct("github.com"));
    }

    #[test]
    fn load_default_when_missing() {
        let dir = std::env::temp_dir();
        let cfg = load(&dir);
        assert_eq!(cfg.mode, "direct");
    }

    #[test]
    fn proxy_decision_reads_live_config() {
        // 热更新语义：决策函数每次调用都读全局配置，改完立刻生效。
        let url = reqwest::Url::parse("https://api.openai.com/v1/models").unwrap();
        let cn = reqwest::Url::parse("https://api.baidu.com/x").unwrap();
        let mk = |m: &str, u: &str| ProxyConfig { mode: m.into(), url: u.into() };

        apply(&mk("direct", "http://127.0.0.1:7890"));
        assert!(proxy_url_for(&url).is_none(), "direct 模式不应代理");

        apply(&mk("global", "http://127.0.0.1:7890"));
        assert!(proxy_url_for(&url).is_some(), "global 模式应代理");

        apply(&mk("bypass_cn", "http://127.0.0.1:7890"));
        assert!(proxy_url_for(&url).is_some(), "bypass_cn 对海外域名应代理");
        assert!(proxy_url_for(&cn).is_none(), "bypass_cn 对大陆域名应直连");

        apply(&mk("global", ""));
        assert!(proxy_url_for(&url).is_none(), "url 为空不应代理");

        apply(&ProxyConfig::default());
    }

    #[test]
    fn local_and_lan_hosts_are_never_proxied_in_any_mode() {
        // 代理进程拿到 127.0.0.1 / 192.168.x 只会连到自己那台机器，
        // 所以「配了 global 代理 → 本机自定义网关全部 502」是必然结果，必须在决策层短路。
        let lan = [
            "127.0.0.1",
            "127.0.0.5",
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.5",
            "169.254.169.254",
            "localhost",
            "localhost.",
            "api.localhost",
            "nas.local",
            "db.internal",
            "printer.lan",
            "[::1]",
            "::1",
            "[fc00::1]",
            "[fe80::1]",
            "[::ffff:127.0.0.1]",
        ];
        for h in lan {
            assert!(host_is_local_or_lan(h), "{h} 必须判为内网");
            assert!(should_direct(h), "{h} 在 bypass_cn 下必须直连");
        }
        // 公网地址不能被误判成内网（172.32 已出 172.16/12 段）
        for h in ["8.8.8.8", "172.32.0.1", "1.1.1.1", "api.openai.com", "[2606:4700::1111]"] {
            assert!(!host_is_local_or_lan(h), "{h} 是公网，不该被判成内网");
        }
        // 不只是 bypass_cn：global 模式也必须直连，否则本机桥接的自定义 API 直接废掉。
        // 用 decide() 传配置而不是改全局，避免与其它用例并行互相踩。
        let mk = |m: &str| ProxyConfig { mode: m.into(), url: "http://127.0.0.1:7890".into() };
        let local = reqwest::Url::parse("http://127.0.0.1:18899/v1/models").unwrap();
        let lan_ip = reqwest::Url::parse("http://192.168.1.5:8000/v1/models").unwrap();
        let public = reqwest::Url::parse("https://api.openai.com/v1/models").unwrap();
        for mode in ["global", "bypass_cn", "direct"] {
            let cfg = mk(mode);
            assert!(decide(&cfg, &local).is_none(), "{mode} 下本机网关不该走代理");
            assert!(decide(&cfg, &lan_ip).is_none(), "{mode} 下局域网网关不该走代理");
        }
        // 对照：同样的配置对公网地址仍是走代理（证明短路只拦住内网，没有把代理功能关掉）
        assert!(decide(&mk("global"), &public).is_some());
    }

    #[test]
    fn cn_cloud_vendors_are_direct_so_bypass_mode_does_not_break_them() {
        // 这些厂商只认国内出口；被判成"海外"送去代理 → 平台整条不可用。
        for h in [
            "dashscope.aliyuncs.com",
            "qianfan.baidubce.com",
            "snap-access.cn-north-4.myhuaweicloud.com",
            "spark-api-open.xf-yun.com",
            "api.baichuan-ai.com",
            "api.stepfun.com",
            "aistudio.xiaomimimo.com",
            "lobsterai.youdao.com",
            "api.deepseek.com",
            "api.kimi.com",
            "longcat.chat",
        ] {
            assert!(should_direct(h), "{h} 是大陆端点，bypass_cn 下必须直连");
        }
        // 真正的海外端点仍然要走代理
        for h in ["api.openai.com", "api.anthropic.com", "generativelanguage.googleapis.com"] {
            assert!(!should_direct(h), "{h} 必须走代理");
        }
    }

    #[test]
    fn apply_and_current_roundtrip() {
        let cfg = ProxyConfig {
            mode: "global".into(),
            url: "http://127.0.0.1:7890".into(),
        };
        apply(&cfg);
        assert_eq!(current().mode, "global");
        assert_eq!(current().url, "http://127.0.0.1:7890");
        apply(&ProxyConfig::default());
    }
}
