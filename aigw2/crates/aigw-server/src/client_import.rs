//! 从**本机已安装的桌面客户端**里读凭据。
//!
//! ── 为什么需要 ────────────────────────────────────────────────
//! 有些平台没有可用的登录接口（要么要验证码、要么干脆没开 OAuth），但它们的
//! 桌面客户端把 token **明文**存在本机。让用户去翻 `%APPDATA%` 再复制粘贴，
//! 是一道完全可以省掉的手工活 —— 而且很容易粘错（多一个空格、少一段）。
//!
//! 本项目此前对 OrcaTerm 就是这么要求的（`data.bin` 里取 `oauth_access_token`），
//! 实测可行后收敛成这个模块。
//!
//! ── 安全边界（这个模块最容易写歪的地方）──────────────────────
//! 1. **只读，不写**：绝不修改客户端文件。客户端可能正在运行，写它会破坏它的状态。
//! 2. **路径白名单，不接受任意路径**：调用方只能指定「平台 id」，具体读哪个文件
//!    由本模块的常量表决定。否则这就是一个「读服务器任意文件」的洞。
//! 3. **只提取已知字段**：不在响应里回显文件其它内容（客户端配置里可能有别的东西）。
//! 4. **大小上限**：读到超限直接放弃，避免被一个巨大的文件拖死。
//! 5. 读到的凭据**只交给账号池**，不写日志（日志里出现 token 等于泄漏）。
//!
//! ── 已知局限 ──────────────────────────────────────────────────
//! 凭据会过期。读出来的东西能不能用，取决于客户端**最近有没有登录过**。
//! 所以导入成功后仍然要走过既有的验活流程，结论如实反馈给用户。

use serde_json::Value;
use std::path::PathBuf;

/// 单个客户端文件的读取上限。Tauri store / cookie JSON 都是几 KB 级，
/// 1 MiB 足够且能挡住「误把日志或缓存当数据文件」的情形。
const MAX_CLIENT_FILE: u64 = 1024 * 1024;

/// 凭据在客户端文件里长什么样 —— 决定怎么提取。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// 一个 JSON 对象，凭据是其中某个字符串字段（Tauri store 的典型形态）。
    JsonField,
    /// 一个 **cookie 数组**（`[{name,value,domain,expires}, …]`），要拼成
    /// `Cookie:` 头。Playwright/Electron 导出的 cookie 都是这个形状。
    ///
    /// ★ 为什么需要单独一种：这类文件里没有「一个 token」，而是多条 cookie
    ///   各有名字/域/过期时间。拼错一条上游就当未登录，所以拼装规则要写死在这里。
    CookieArray,
}

/// 一个「可自动导入」的平台：去哪儿读、怎么提取。
pub(crate) struct ClientSource {
    /// 平台 id（与 providers/*.toml 的 id 一致）。
    pub platform: &'static str,
    /// 相对 `%APPDATA%`（Windows）或 `~/.config`（其它）的路径。
    pub rel_path: &'static str,
    /// 提取形态。
    pub shape: Shape,
    /// `JsonField` 时取哪个键；`CookieArray` 时留空。
    pub field: &'static str,
    /// `CookieArray` 时只保留这些名字的 cookie（空 = 全保留）。
    ///
    /// ★ 为什么要白名单：客户端会把**整站**的 cookie 都存进来（埋点、语言偏好、
    ///   广告 id……）。全拼进 `Cookie:` 头既没必要，也可能因为带了不该带的项
    ///   让上游觉得异常。只留登录态那一条最稳。
    pub cookie_names: &'static [&'static str],
    /// 给人看的来源说明（面板上展示，让用户知道读的是哪个文件）。
    pub label: &'static str,
}

/// 支持从本机客户端自动导入的平台。
///
/// ★ 只收**实测验证过**的。加新条目之前必须在真机上确认：
///   · 文件确实是明文可读（不是被系统密钥环加密的）；
///   · 字段确实是可用的凭据（拿它打过上游、能通）；
///   · 格式稳定（客户端升级不会轻易改）。
pub(crate) const SOURCES: &[ClientSource] = &[
    ClientSource {
        platform: "apk-orcaterm",
        rel_path: "com.orcaterm-desktop.app/data.bin",
        shape: Shape::JsonField,
        field: "oauth_access_token",
        cookie_names: &[],
        label: "OrcaTerm 桌面端（data.bin）",
    },
    // MonkeyCode：客户端把登录态存成 cookie 数组，上游认
    // `Cookie: monkeycode_ai_session=<uuid>`。
    ClientSource {
        platform: "web-monkeycode",
        rel_path: "com.chaitin.baizhi.monkeycode/monkeycode-cookies.json",
        shape: Shape::CookieArray,
        field: "",
        cookie_names: &["monkeycode_ai_session"],
        label: "MonkeyCode 客户端（monkeycode-cookies.json）",
    },
    // ★ 同目录下还有 `baizhi-cookies.json`（百智 baizhi.cloud），但本项目
    //   **没有 baizhi 这个 platform**（providers/ 里无对应 toml、relay 里无 adapter）。
    //   把导入项加进白名单只会让面板给一个不存在的平台亮出「可导入」，
    //   用户点了才发现平台根本不存在。等真接了百智（需要先写 adapter 与 toml）
    //   再把这条加回来。
];

/// 本机某个平台的客户端数据目录根。
///
/// Windows 是 `%APPDATA%`（Roaming）—— Tauri 的 `app_data_dir()` 就落在那里；
/// 其它平台取 `~/.config`。取不到就返回 None（导入功能优雅不可用，而不是报错）。
///
/// ★ **不能回退到 `%LOCALAPPDATA%`**：本机实测两个目录下**都有**
///   `com.orcaterm-desktop.app`，但内容完全不同 ——
///     `%APPDATA%/…/data.bin`      ← 真正的凭据（Tauri store）
///     `%LOCALAPPDATA%/…/.cookies` ← WebView 的 cookie 容器（另一种格式）
///   旧实现按 `APPDATA → LOCALAPPDATA` 顺序回退：`APPDATA` 缺失时会去找
///   `%LOCALAPPDATA%/…/data.bin`，那个文件根本不存在，于是报「本机没找到」——
///   而用户的凭据其实好端端在 Roaming 里。这是**归因错误**，会让人以为没登录过。
///   ⇒ 只认 APPDATA，缺失就明确说「环境变量缺失」，不去猜别的位置。
fn appdata_root() -> Option<PathBuf> {
    if cfg!(windows) {
        let v = std::env::var_os("APPDATA")?;
        let p = PathBuf::from(v);
        return (!p.as_os_str().is_empty()).then_some(p);
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
}

/// 从客户端数据文件读 JSON，**容忍「客户端正在写」**。
///
/// ── 为什么需要重试 ────────────────────────────────────────────
/// 客户端保存 `data.bin` 是**截断再写**：`open(w)` 先把文件清成 0 字节，再分块写入。
/// 在中间那一刻读到的是半截内容（实测：半截字符串直接让 `serde_json` 报
/// Unterminated string）。旧实现把这种瞬态说成「客户端版本可能变了」，
/// 把用户引到完全错误的方向（他会去查版本，而实际只要再点一次）。
///
/// 客户端写完通常只需毫秒级，所以短暂重试几次即可；仍失败才如实说
/// 「可能正在写入」或「现在是空的」，而不是武断地归因。
///
/// 参数化 `attempts` / `delay` 是为了测试能用很短的等待跑完。
fn read_json_with_retry(
    path: &std::path::Path,
    attempts: usize,
    delay: std::time::Duration,
) -> Result<Value, String> {
    let mut last_err = String::new();
    for n in 0..attempts.max(1) {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => return Err(format!("读不到 {}：{e}", path.display())),
        };
        match serde_json::from_str::<Value>(&raw) {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e.to_string();
                // 空文件 = 刚被截断，是最典型的「正在写」，值得等下一轮。
                if n + 1 < attempts.max(1) {
                    std::thread::sleep(delay);
                    continue;
                }
                // 最后一轮还失败：区分「空」与「坏」两种可解释的原因。
                if raw.trim().is_empty() {
                    return Err(format!(
                        "{} 现在是空的（客户端可能正在保存），稍等一下再点导入",
                        path.display()
                    ));
                }
                return Err(format!(
                    "{} 解析失败（{last_err}）：可能客户端正在写这个文件，稍后重试；\
                     若一直如此，是客户端换了存储格式",
                    path.display()
                ));
            }
        }
    }
    Err(format!("{}：{last_err}", path.display()))
}

/// 查这个平台能不能自动导入。
pub(crate) fn source_for(platform: &str) -> Option<&'static ClientSource> {
    SOURCES.iter().find(|s| s.platform == platform)
}

/// 把客户端导出的 **cookie 数组**拼成 `Cookie:` 头的值。
///
/// 输入形如：
/// ```json
/// [{"name":"monkeycode_ai_session","value":"…","domain":"monkeycode-ai.com",
///   "path":"/","expires":"2026-10-04T15:02:45Z","host_only":true}]
/// ```
///
/// 规则：
/// - 只取 `names` 里列出的那些（空 = 全取）；
/// - **跳过已过期的**：带 `expires` 且早于现在的直接不用 —— 拿过期 cookie 去请求
///   只会得到 401，而用户会以为是面板坏了。过期时给出明确提示（见调用方）。
/// - 输出 `name=value; name2=value2`（上游 `parse_cred` 认这个形态）。
///
/// 为什么不在这里直接判「是否全过期」：调用方需要区分「没有这条 cookie」与
/// 「有但过期了」，前者是没登录、后者是登录过期 —— 处理方式不同。
fn cookies_to_header(v: &Value, names: &[&str]) -> Result<String, String> {
    let Some(arr) = v.as_array() else {
        return Err("文件内容不是 cookie 数组（期望 [{name,value,…}]）".to_string());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut parts: Vec<String> = Vec::new();
    let mut expired: Vec<String> = Vec::new();
    for c in arr {
        let Some(name) = c.get("name").and_then(|x| x.as_str()) else {
            continue;
        };
        if !names.is_empty() && !names.contains(&name) {
            continue;
        }
        let value = c.get("value").and_then(|x| x.as_str()).unwrap_or("");
        if value.is_empty() {
            continue;
        }
        // expires 可能是 ISO8601 字符串（Playwright 风格）或 unix 时间戳。
        if let Some(exp) = expires_of(c) {
            if exp > 0 && exp < now {
                expired.push(name.to_string());
                continue;
            }
        }
        parts.push(format!("{name}={value}"));
    }
    if parts.is_empty() {
        if !expired.is_empty() {
            return Err(format!(
                "登录态已过期（{} 已失效），请在客户端重新登录后再导入",
                expired.join(" / ")
            ));
        }
        return Err("没有找到需要的 cookie：确认客户端已登录".to_string());
    }
    Ok(parts.join("; "))
}

/// 取一条 cookie 的过期时刻（unix 秒）。没有就返回 None（会话 cookie，长期有效）。
fn expires_of(c: &Value) -> Option<i64> {
    let raw = c.get("expires")?;
    if let Some(n) = raw.as_i64() {
        return Some(n);
    }
    let s = raw.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    // 只认 `YYYY-MM-DDTHH:MM:SSZ`（客户端导出的实际形态），不引第三方日期库。
    parse_iso8601_utc(s)
}

/// 极简 ISO-8601 UTC 解析：`2026-10-04T15:02:45Z`（可带小数秒）。
///
/// 只用到「跟现在比大小」，所以手写足够，不必为这一个用途拉 `chrono`。
/// 解析不了就返回 None —— 当作「不会过期」，避免因为格式微差把可用凭据丢掉。
fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // 公历 → 天数的经典算法（Howard Hinnant 的 days_from_civil），与时区无关。
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + sec)
}

/// 导入结果。
pub(crate) enum ImportOutcome {
    /// 拿到了凭据。
    Found {
        secret: String,
        /// 来源说明（面板展示）。
        label: &'static str,
    },
    /// 这个平台不支持自动导入（附上支持列表，让调用方拼提示）。
    Unsupported,
    /// 支持，但本机找不到文件 / 读不出来 / 缺字段。`why` 是给人看的。
    Missing(String),
}

/// 从本机客户端读一个平台的凭据。
///
/// 只读、只提取白名单字段、失败一律返回 `Missing(人话)` —— 不 panic、不吞错。
pub(crate) fn import(platform: &str) -> ImportOutcome {
    let Some(src) = source_for(platform) else {
        return ImportOutcome::Unsupported;
    };
    let Some(root) = appdata_root() else {
        // 明确说「环境变量缺失」，而不是含糊的「找不到」——
        // 后者会让用户以为是自己没登录过，实际是服务进程的环境不完整
        // （例如从计划任务/服务启动时没继承用户环境）。
        return ImportOutcome::Missing(
            "这个进程看不到 %APPDATA% 环境变量（通常是从服务/计划任务启动、没继承用户环境）。\
             改用面板手动粘贴凭据，或用当前用户会话重启面板"
                .to_string(),
        );
    };
    let path = root.join(src.rel_path);
    // 大小闸门：先看元数据，别把大文件整个读进内存再判断。
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > MAX_CLIENT_FILE => {
            return ImportOutcome::Missing(format!(
                "{} 比预期大得多（{} 字节），不像是客户端的数据文件",
                path.display(),
                m.len()
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ImportOutcome::Missing(format!(
                "本机没找到 {}：确认装过 {} 并至少登录过一次",
                path.display(),
                src.label
            ));
        }
        Err(e) => {
            return ImportOutcome::Missing(format!("读不到 {}：{e}", path.display()));
        }
    }
    let v = match read_json_with_retry(&path, 4, std::time::Duration::from_millis(120)) {
        Ok(v) => v,
        Err(why) => return ImportOutcome::Missing(why),
    };
    let secret = match src.shape {
        Shape::JsonField => v
            .get(src.field)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        Shape::CookieArray => match cookies_to_header(&v, src.cookie_names) {
            Ok(s) => s,
            Err(why) => {
                return ImportOutcome::Missing(format!("{}：{why}", path.display()));
            }
        },
    };
    if secret.is_empty() {
        let what = if src.shape == Shape::CookieArray {
            format!("需要的 cookie（{}）", src.cookie_names.join(" / "))
        } else {
            src.field.to_string()
        };
        return ImportOutcome::Missing(format!(
            "{} 里没有 {}（可能还没登录，或客户端换了字段名）",
            path.display(),
            what
        ));
    }
    // ★ 绝不把 secret 打日志。这里只记「从哪个平台导入了多长」。
    tracing::info!(
        platform = %platform,
        len = secret.len(),
        "已从本机客户端读取凭据（内容不入日志）"
    );
    ImportOutcome::Found {
        secret,
        label: src.label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 白名单必须只认已实测的平台，不能变成「任意 id 都能读文件」。
    #[test]
    fn only_whitelisted_platforms_are_importable() {
        assert!(source_for("apk-orcaterm").is_some(), "OrcaTerm 应可导入");
        // 不在表里的一律不支持 —— 这是「不能读任意文件」的核心保证
        assert!(source_for("apk-autoclaw").is_none());
        assert!(source_for("../../etc/passwd").is_none());
        assert!(source_for("").is_none());
        assert!(source_for("apk-orcaterm/../../../secret").is_none());
    }

    /// 不支持的平台要回 Unsupported（而不是 Missing），调用方据此拼不同的提示。
    #[test]
    fn unsupported_platform_reports_unsupported() {
        assert!(matches!(
            import("apk-autoclaw"),
            ImportOutcome::Unsupported
        ));
    }

    /// 白名单里的每个 platform **必须真的存在于 providers/**。
    ///
    /// 这是一次真实事故：我一度把 `baizhi` 加进白名单，但项目里根本没有这个平台
    /// （无 toml、无 adapter）。后果是面板给一个不存在的平台亮出「可导入」，
    /// 用户点下去才发现无处可存。这条测试把「导入白名单」与「平台是否真的存在」
    /// 绑在一起，防止再出现悬空条目。
    #[test]
    fn every_importable_platform_actually_exists() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../providers");
        if !dir.is_dir() {
            // 打包环境没有源码树，跳过（不算失败）
            return;
        }
        let specs = aigw_registry::load_dir_lenient(&dir)
            .map(|(s, _)| s)
            .unwrap_or_default();
        let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
        for s in SOURCES {
            assert!(
                ids.contains(&s.platform),
                "导入白名单里的 `{}` 在 providers/ 里不存在（可用 id 示例：{:?}）",
                s.platform,
                ids.iter().take(5).collect::<Vec<_>>()
            );
        }
    }

    /// 路径必须是**相对**的：绝对路径会让白名单形同虚设。
    #[test]
    fn source_paths_are_relative_and_sane() {
        for s in SOURCES {
            assert!(
                !s.rel_path.starts_with('/') && !s.rel_path.contains(":\\"),
                "{} 的路径是绝对的：{}",
                s.platform,
                s.rel_path
            );
            assert!(
                !s.rel_path.contains(".."),
                "{} 的路径含 ..：{}",
                s.platform,
                s.rel_path
            );
            // 两种形态各自的必填项
            match s.shape {
                Shape::JsonField => {
                    assert!(!s.field.is_empty(), "{} 是 JsonField 但没指定字段", s.platform);
                }
                Shape::CookieArray => {
                    assert!(
                        !s.cookie_names.is_empty(),
                        "{} 是 CookieArray 但没列出要哪些 cookie —— 会把整站 cookie 都拼进去",
                        s.platform
                    );
                }
            }
        }
    }

    /// cookie 数组 → `Cookie:` 头：只取白名单名字、跳过过期项、保持顺序。
    #[test]
    fn cookie_array_becomes_a_cookie_header() {
        let v = serde_json::json!([
            {"name":"_ga","value":"GA1.2","domain":"x.com","expires":"2099-01-01T00:00:00Z"},
            {"name":"sess","value":"abc123","domain":"x.com","expires":"2099-01-01T00:00:00Z"},
            {"name":"other","value":"zzz"}
        ]);
        let got = cookies_to_header(&v, &["sess"]).expect("应拼出 header");
        assert_eq!(got, "sess=abc123", "只该保留白名单里的 cookie");
        // 空名单 = 全取
        let all = cookies_to_header(&v, &[]).expect("全取");
        assert!(all.contains("_ga=GA1.2") && all.contains("sess=abc123"));
        assert!(all.contains("; "), "多条要用 ; 分隔：{all}");
    }

    /// **已过期的 cookie 必须被跳过，并给出「请重新登录」的明确提示。**
    ///
    /// 这条是这次实测踩出来的：本机 MonkeyCode / 百智 的 cookie 都过期了
    /// （分别过期 30 天 / 7 天）。如果不判过期，导入会「成功」但一发请求就 401，
    /// 用户会以为面板坏了。
    #[test]
    fn expired_cookies_are_rejected_with_a_clear_reason() {
        let v = serde_json::json!([
            {"name":"monkeycode_ai_session","value":"old","expires":"2026-10-04T15:02:45Z"}
        ]);
        let err = cookies_to_header(&v, &["monkeycode_ai_session"]).expect_err("过期应报错");
        assert!(err.contains("已过期"), "提示要说明是过期：{err}");
        assert!(
            err.contains("重新登录"),
            "还要告诉用户怎么办：{err}"
        );
    }

    /// 没有这条 cookie ≠ 过期了：两种情况的提示不同。
    #[test]
    fn missing_cookie_is_distinct_from_expired() {
        let v = serde_json::json!([{"name":"_ga","value":"x"}]);
        let err = cookies_to_header(&v, &["monkeycode_ai_session"]).expect_err("缺 cookie 应报错");
        assert!(!err.contains("已过期"), "缺 cookie 不该说成过期：{err}");
        assert!(err.contains("登录") || err.contains("没有找到"), "{err}");
    }

    /// 会话 cookie（没有 expires）视为长期有效，不能因为缺字段就丢掉。
    #[test]
    fn session_cookie_without_expiry_is_kept() {
        let v = serde_json::json!([{"name":"sess","value":"v1"}]);
        assert_eq!(cookies_to_header(&v, &["sess"]).unwrap(), "sess=v1");
    }

    /// 非数组输入要报清楚，而不是 panic。
    #[test]
    fn non_array_input_is_a_clear_error() {
        let v = serde_json::json!({"cookies": []});
        assert!(cookies_to_header(&v, &["x"]).is_err());
    }

    /// ISO-8601 解析：只用于比大小，但必须算对（跨月/闰年各验一个）。
    #[test]
    fn iso8601_parsing_is_correct() {        // 1970-01-01T00:00:00Z = 0
        assert_eq!(parse_iso8601_utc("1970-01-01T00:00:00Z"), Some(0));
        // 2026-10-04T15:02:45Z 实测对应的 unix 秒
        assert_eq!(parse_iso8601_utc("2026-10-04T15:02:45Z"), Some(1791126165));
        // 闰年 2 月 29 日
        assert!(parse_iso8601_utc("2024-02-29T00:00:00Z").is_some());
        // 解析不了 → None（当长期有效，别把可用凭据丢掉）
        assert_eq!(parse_iso8601_utc("not-a-date"), None);
        assert_eq!(parse_iso8601_utc(""), None);
    }

    /// 建一个只给本测试用的临时目录（先删后建，避免上一次运行的残留）。
    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aigw-ci-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    /// 客户端正在写文件时（读到半截 JSON），必须**重试**而不是立刻报错。
    ///
    /// 真实场景：客户端保存 `data.bin` 是「截断再写」，中间那一刻能读到不完整的
    /// JSON，`serde_json` 报 Unterminated string。旧实现把这种瞬态说成
    /// 「客户端版本换了」，用户会去查版本而实际只要再点一次。
    #[test]
    fn partial_write_is_retried_then_succeeds() {
        let dir = tmp("partial");
        let p = dir.join("data.bin");
        // 先写半截（模拟客户端刚截断并写了一部分）
        std::fs::write(&p, r#"{"field_x": "PART"#).expect("write partial");
        let p2 = p.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            std::fs::write(&p2, r#"{"field_x": "PART-COMPLETE"}"#).expect("finish");
        });
        let got = read_json_with_retry(&p, 5, std::time::Duration::from_millis(100));
        h.join().expect("writer");
        let v = got.expect("补全后应能读到完整 JSON");
        assert_eq!(v.get("field_x").and_then(|x| x.as_str()), Some("PART-COMPLETE"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一直是坏内容时，重试耗尽后要说「可能正在写 / 稍后重试」，
    /// 而不是武断地断言「格式变了」。
    #[test]
    fn persistently_invalid_file_reports_write_in_progress() {
        let dir = tmp("broken");
        let p = dir.join("data.bin");
        std::fs::write(&p, "not json at all").expect("write");
        let err = read_json_with_retry(&p, 2, std::time::Duration::from_millis(10))
            .expect_err("坏内容应报错");
        assert!(
            err.contains("正在写") && err.contains("稍后重试"),
            "提示应说明可能与写入并发：{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空文件（客户端刚截断）要说「现在是空的」，不要和「内容坏」混为一谈。
    #[test]
    fn empty_file_reports_being_cleared() {
        let dir = tmp("empty");
        let p = dir.join("data.bin");
        std::fs::write(&p, "").expect("write");
        let err = read_json_with_retry(&p, 2, std::time::Duration::from_millis(10))
            .expect_err("空文件应报错");
        assert!(err.contains("空的"), "提示要点明是空的：{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件不存在要给「没有这个文件」的提示（不是解析失败）。
    #[test]
    fn missing_file_reports_clearly() {
        let dir = tmp("missing");
        let p = dir.join("nope.bin");
        let err = read_json_with_retry(&p, 2, std::time::Duration::from_millis(10))
            .expect_err("不存在应报错");
        assert!(err.contains("读不到"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 在 Windows 上，客户端数据根**只能是 `%APPDATA%`**，不能回退到 `%LOCALAPPDATA%`。
    ///
    /// 本机实测两个目录下都有 `com.orcaterm-desktop.app`，但内容不同：
    ///   `%APPDATA%/…/data.bin`      ← 真凭据（Tauri store）
    ///   `%LOCALAPPDATA%/…/.cookies` ← WebView cookie 容器（另一种格式）
    /// 回退会去找一个不存在的 `data.bin`，然后报「本机没找到」——
    /// 归因错误，用户会以为自己没登录过。
    ///
    /// 这里验证「取到的根必须等于 APPDATA」这个契约（在 Windows 且有该变量的环境下）。
    #[cfg(windows)]
    #[test]
    fn root_is_appdata_not_localappdata() {
        let Some(root) = appdata_root() else {
            return; // 环境里没有 APPDATA 时不算失败
        };
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let expect = PathBuf::from(appdata);
            if !expect.as_os_str().is_empty() {
                assert_eq!(
                    root, expect,
                    "客户端数据根必须是 %APPDATA%，实际 {}",
                    root.display()
                );
            }
        }
        // 若同时存在 LOCALAPPDATA，两者不应相等（确保我们没退到它）
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let l = PathBuf::from(local);
            if !l.as_os_str().is_empty() {
                assert_ne!(root, l, "不能把 %LOCALAPPDATA% 当成客户端数据根");
            }
        }
    }
}
