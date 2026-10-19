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
use std::io::Write;
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

/// 默认数据目录（与 main.rs 同逻辑）：%LOCALAPPDATA%\aigw2\data，缺省 ./data。
pub fn default_data_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(|l| PathBuf::from(l).join("aigw2").join("data"))
        .unwrap_or_else(|_| PathBuf::from("./data"))
}

/// 从 data 目录加载（不存在/损坏 → direct 默认）。
pub fn load(data_dir: &Path) -> ProxyConfig {
    let p = data_dir.join("proxy.json");
    match std::fs::read_to_string(&p) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => ProxyConfig::default(),
    }
}

/// 保存到 data/proxy.json 并**热更新全局配置**。
pub fn save_and_apply(data_dir: &Path, cfg: &ProxyConfig) -> std::io::Result<()> {
    let p = data_dir.join("proxy.json");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    {
        let mut f = std::fs::File::create(&p)?;
        let _ = f.write_all(
            serde_json::to_string_pretty(cfg)
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    apply(cfg);
    Ok(())
}

/// 应用配置到全局（进程内热更新；main 启动时也调一次对齐磁盘值）。
pub fn apply(cfg: &ProxyConfig) {
    if let Ok(mut g) = global_cfg().write() {
        *g = cfg.clone();
    }
}

/// 读当前运行时配置。
pub fn current() -> ProxyConfig {
    global_cfg().read().map(|g| g.clone()).unwrap_or_default()
}

/// 域名决策：给定 host，返回是否**直连**（bypass_cn 模式下大陆域名 → true）。
fn should_direct(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    if h.is_empty() {
        return true;
    }
    // 本机回环/内网永不代理
    if h == "localhost" || h == "127.0.0.1" || h == "::1" || h.ends_with(".local") {
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
    ];
    CN_DOMAINS
        .iter()
        .any(|d| h == *d || h.ends_with(&format!(".{}", d)))
}

/// 构建 reqwest Proxy：按当前配置决策（None = 直连）。
/// 供 OutClient 构造时注入 `Client::builder().proxy(...)`。
pub fn proxy_for_current() -> Option<reqwest::Proxy> {
    let cfg = current();
    if cfg.mode == "global" && !cfg.url.is_empty() {
        return reqwest::Proxy::all(cfg.url.as_str()).ok();
    }
    if cfg.mode == "bypass_cn" && !cfg.url.is_empty() {
        let url = cfg.url.clone();
        return Some(reqwest::Proxy::custom(move |u: &reqwest::Url| {
            let host = u.host_str().unwrap_or("");
            if should_direct(host) {
                None
            } else {
                Some(url.clone())
            }
        }));
    }
    None
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
