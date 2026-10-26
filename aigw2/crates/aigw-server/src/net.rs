//! 主机名 / 来源头 / 出站目标判定 —— 门禁与 SSRF 防线用到的全部纯函数。
//!
//! **为什么单独成模块**：这块逻辑原先长在 `lib.rs` 里（那个文件已经一万六千行），
//! 而它恰好是安全边界上最该被单独盯着、单独测的部分：Host 白名单、Origin/Referer
//! 白名单、`[::1]:8800` 这类 IPv6 字面量的括号处理、以及「哪些出站目标禁止被当成上游」。
//! 拆出来后每条规则都能直接写单测，不用先拼一个 AppState 再发一个 HTTP 请求。

use aigw_registry::ProviderSpec;

/// Host 头取主机名：`127.0.0.1:8800` → `127.0.0.1`；`[::1]:8800` → `[::1]`。
pub fn host_only(raw: &str) -> String {
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
pub fn origin_host(raw: &str) -> String {
    let s = raw.trim();
    let rest = match s.find("://") {
        Some(i) => &s[i + 3..],
        None => s,
    };
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    host_only(hostport)
}

/// IP 字面量判定（IPv4 / IPv6 / 带方括号的 IPv6）。域名一律返回 false。
pub fn is_ip_literal(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.is_empty() {
        return false;
    }
    // 允许局域网常见写法之外不做任何模糊匹配：主机名一律视为域名
    h.parse::<std::net::Ipv4Addr>().is_ok() || h.parse::<std::net::Ipv6Addr>().is_ok()
}

/// 常量时间比较（令牌校验用，避免逐字节短路泄露长度/前缀）。
pub fn ct_eq(a: &str, b: &str) -> bool {
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

/// Host / Origin 主机名是否放行。
///
/// - 命中白名单（回环名 + 绑定地址）→ 放行；
/// - 通配监听（0.0.0.0 / ::）时，**任意 IP 字面量**放行：局域网用
///   `192.168.1.5:8800` 访问面板是正常用法；
/// - 域名永不因通配而放行：DNS 重绑定的载荷一定是域名。
pub fn host_allowed(allow_hosts: &[String], allow_any_ip: bool, host: &str) -> bool {
    if allow_hosts.iter().any(|h| ct_eq(h, host)) {
        return true;
    }
    allow_any_ip && is_ip_literal(host)
}

/// 出站目标是否属于「不该被本面板当上游」的地址。
///
/// 面板是本机单用户工具，但它有能力带着用户填的 Key 去请求任意地址。真正值得挡的不是
/// 「用户自己填内网地址」——那往往是有意的（本机另一个网关、局域网内的 vLLM）；而是：
/// 1. **云元数据端点**（169.254.169.254 / fd00:ec2::254 / metadata.google.internal 等）：
///    一旦被填进来，上游响应里就是云主机的临时凭据，属典型 SSRF 提权。
///
/// 因此这里只做「元数据」硬拦截，回环与其它内网地址一律放行并在文档里讲清楚取舍
/// —— 一刀切封内网会把「接本机另一个 OpenAI 兼容网关」这种正常用法打死。
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
    META.iter().any(|m| h == *m)
}

/// 从 base_url / 代理 url 里取出主机名（去掉 scheme、端口、路径）。取不到返回空串。
pub fn url_host(raw: &str) -> String {
    origin_host(raw)
}

/// 「同一个站」判定用的 host 规范化：小写 + 去 `www.`（端口与路径 `url_host` 已去掉）。
pub fn site_host(raw: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_only_strips_port_but_keeps_ipv6_brackets() {
        assert_eq!(host_only("127.0.0.1:8800"), "127.0.0.1");
        assert_eq!(host_only("example.com"), "example.com");
        // IPv6 的冒号不能被当成端口分隔符，方括号必须保留（否则再拼 URL 会坏）
        assert_eq!(host_only("[::1]:8800"), "[::1]");
        assert_eq!(host_only("[::1]"), "[::1]");
        // 右方括号缺失时原样返回，由调用方当作「不合法 host」处理
        assert_eq!(host_only("[::1"), "[::1");
        assert_eq!(host_only("  "), "");
    }

    #[test]
    fn origin_host_handles_scheme_port_path_and_garbage() {
        assert_eq!(origin_host("https://evil.example:8443/x?y#z"), "evil.example");
        assert_eq!(origin_host("http://127.0.0.1:8800"), "127.0.0.1");
        // 无 scheme 的裸 host:port（Origin 头理论上不会这样，但别让步）
        assert_eq!(origin_host("example.com:9000"), "example.com");
        assert_eq!(origin_host("null"), "null");
    }

    #[test]
    fn is_ip_literal_accepts_v4_v6_and_rejects_domains() {
        for h in ["127.0.0.1", "192.168.1.5", "[::1]", "::1", "[fe80::1]"] {
            assert!(is_ip_literal(h), "{h} 应判为 IP 字面量");
        }
        for h in ["example.com", "localhost", "", "1.2.3", "[::1x]"] {
            assert!(!is_ip_literal(h), "{h} 不该判为 IP 字面量");
        }
    }

    #[test]
    fn host_allowed_never_whitelists_domains_even_in_wildcard_mode() {
        let hosts = vec!["127.0.0.1".to_string(), "localhost".to_string()];
        assert!(host_allowed(&hosts, false, "127.0.0.1"));
        assert!(host_allowed(&hosts, false, "localhost"));
        assert!(!host_allowed(&hosts, false, "192.168.1.5"));

        // 通配监听：局域网 IP 放行，但域名永不因通配放行（DNS 重绑定载荷必是域名）
        assert!(host_allowed(&hosts, true, "192.168.1.5"));
        assert!(host_allowed(&hosts, true, "10.0.0.7"));
        assert!(!host_allowed(&hosts, true, "attacker.dns-rebind.example"));
        assert!(!host_allowed(&hosts, true, ""));
    }

    #[test]
    fn blocked_upstream_host_blocks_cloud_metadata_only() {
        // 云元数据一律拒绝：被当上游等于把临时凭据送到面板里
        for h in [
            "169.254.169.254",
            "[fd00:ec2::254]",
            "metadata.google.internal",
            "METADATA.GOOG",
        ] {
            assert!(blocked_upstream_host(h), "{h} 必须被拦");
        }
        // 本机 / 局域网是合法上游（接另一个 OpenAI 兼容网关），不能一刀切
        for h in ["127.0.0.1", "192.168.1.5", "localhost", "api.openai.com", ""] {
            assert!(!blocked_upstream_host(h), "{h} 不该被拦");
        }
    }

    #[test]
    fn site_host_normalizes_case_and_www() {
        assert_eq!(site_host("https://WWW.AgentRouter.org/x"), "agentrouter.org");
        assert_eq!(site_host("https://agentrouter.org"), "agentrouter.org");
        assert_eq!(site_host("agentrouter.org:443"), "agentrouter.org");
    }

    #[test]
    fn is_loopback_host_covers_localhost_and_127_range() {
        for h in ["localhost", "LOCALHOST", "127.0.0.1", "127.0.0.8", "[::1]", "::1"] {
            assert!(is_loopback_host(h), "{h} 应判为回环");
        }
        for h in ["10.0.0.1", "example.com", ""] {
            assert!(!is_loopback_host(h), "{h} 不该判为回环");
        }
    }

    #[test]
    fn ct_eq_is_exact() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        // 长度不同也必须返回 false（且不 panic）
        assert!(!ct_eq("abc", "ab"));
        assert!(ct_eq("", ""));
    }

    fn spec(id: &str, base: &str, login: &str) -> ProviderSpec {
        toml::from_str(&format!(
            "id = \"{id}\"\nname = \"{id}\"\nauth = \"anon\"\nbase_url = \"{base}\"\n\
             format = \"openai\"\nmodels_policy = \"live\"\nstream_dialect = \"sse_delta\"\n\
             login_url = \"{login}\"\n"
        ))
        .unwrap()
    }

    #[test]
    fn static_free_hosts_dedups_across_base_and_login_url() {
        let specs = vec![
            spec("free-agentrouter", "https://api.agentrouter.org/v1", "https://www.agentrouter.org"),
            spec("api-openai", "https://api.openai.com/v1", ""), // 非 free- 不参与
        ];
        let hosts = static_free_hosts(&specs);
        assert_eq!(hosts, vec!["api.agentrouter.org".to_string(), "agentrouter.org".to_string()]);
    }
}
