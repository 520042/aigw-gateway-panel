//! aigw-registry：`providers/*.toml` 声明式平台注册表。
//!
//! 核心思想（借鉴 dsh-our-free-model 的 76 家 registry.json + cherry-studio
//! provider-registry 抽象，落到 Rust + TOML）：**一份 spec = 接入一个平台**。
//! 引擎读 spec 执行；只有真正特殊的平台才写 Rust adapter。
//! 加平台 = 加一份 toml + 一组契约测试，不改引擎代码。
//!
//! schema 演进纪律：M0 落地骨架字段（id/auth/base_url/format/models/headers）；
//! M2 扩签名器、模型清单策略细则、池策略、防风控头清单（详见 重构计划-v2.md §2）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// 认证方式（归纳自 autumn/Trae2api-cn 五认证源 + AIClient2API OAuth 流 + qoder 设备流）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// 免密/匿名车道（anon-zen、pollinations、kilo）。
    Anon,
    /// 静态 API Key / Bearer。
    ApiKey,
    /// OAuth 设备码 / PKCE（codebuddy、grok、claude 系）。
    OAuthDevice,
    /// 扫码登录（copilot 原生、豆包）。
    QrScan,
    /// 手机号 + 短信验证码（讯飞 Loomy、Trae 手机登录）。与扫码同级：
    /// 都是「用户在抽屉里做一步交互、上游发一个短期凭据回来」，但短信要多一次
    /// 往返（先发码拿 msgid，再交验证码换 session），前端需要两步表单。
    SmsCode,
    /// Cookie / 会话（元宝、web 反代系）。
    Cookie,
    /// 本地客户端凭据解密（Trae storage.json、ZCode credentials.json 信封）。
    ClientFile,
}

/// 流式方言：上游 SSE 的形状。归一到 aigw-core::StreamChunk 再转出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamDialect {
    /// 标准 OpenAI SSE delta。
    SseDelta,
    /// 事件状态机（豆包 2001 补全/2002 ACK/2003 FIN）。
    EventMachine,
    /// Anthropic SSE。
    AnthropicSse,
    /// Responses 事件流。
    Responses,
}

/// 模型清单策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelsPolicy {
    /// 实时拉取，失败回退 spec 内静态清单（静态不许覆盖 live，v1 沉淀规则）。
    Live,
    /// 仅静态清单。
    Static,
    /// 无清单：模型路由完全依赖 `模型id@平台` 后缀（元宝/小浣熊/Trae 等无 /models 上游）。
    None,
}

/// 一个平台的声明式接入规格。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpec {
    /// 平台标识（同目录内唯一），小写字母/数字/`-`/`_`。
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub auth: AuthKind,
    /// 这个平台**实际支持的全部登录方式**（qr / sms / pwd / device / paste / cookie），
    /// 空表示「按 auth 推导」（见 `login_methods()`）。
    ///
    /// 为什么需要它：一个平台常常同时有多种通路 —— Loomy 既有微信扫码又有手机号短信/密码；
    /// 只用 `auth` 单值表达会强迫二选一，于是「relay 实现了扫码、toml 却写 sms_code」
    /// 这种状态就会出现，前端抽屉只画手填框，扫码入口根本到不了（实测踩过）。
    /// 这里显式列出来，前端和测试都有据可依。
    #[serde(default)]
    pub login_methods: Vec<String>,
    /// 上游 API 根地址（不带尾斜杠），如 `https://opencode.ai/zen/v1`。
    pub base_url: String,
    /// 线协议：openai | anthropic | responses。
    pub format: String,
    /// 静态模型清单（models_policy=live 时可为空）。
    #[serde(default)]
    pub models: Vec<String>,
    pub models_policy: ModelsPolicy,
    /// 声明流的方言（sse_delta | json_full | …）。
    ///
    /// ★ 目前**仅用于登记/展示**：relay 走的是「字节原样透传」，不做按方言的
    /// 事件归一（`crates/aigw-relay/src/lib.rs` 里 0 处引用本字段）。写进 toml
    /// 不会改变行为，换成 anthropic 方言也不会被翻译——真正决定能不能用的是
    /// `format`（只有 openai 被接受）。保留字段是为了将来接 Responses/Anthropic 归一。
    pub stream_dialect: StreamDialect,
    /// 固定请求头（如 anon-zen 的 `Authorization: Bearer public` + UA）。
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// chat 端点路径（拼在 base_url 后）。OpenAI 标准 /chat/completions；
    /// pollinations 等特殊上游可覆盖（如 /openai）。
    #[serde(default)]
    pub chat_path: String,
    /// live 模型拉取路径（默认 /models）。
    #[serde(default)]
    pub models_path: String,
    /// live 清单过滤："" 全量 | "isFree" 只取 isFree==true 切片（kilo）。
    #[serde(default)]
    pub models_filter: String,
    /// 上游形态：openai（OpenAI 兼容，字节透传）| chatglm（智谱私有协议，走 adapter）。
    #[serde(default = "default_upstream_kind")]
    pub upstream_kind: String,
    /// 请求签名器：""（无）| glm（X-Sign 头组，aigw-crypto::glm_build_sign）
    /// | codearts（SDK-HMAC-SHA256，账号 secret 存 `AK:SK`，aigw-crypto::codearts）。
    #[serde(default)]
    pub signer: String,
    /// 假成功指纹关键词：HTTP 2xx 且 body 命中任一关键词 → Auth 类错误（loomy/星火系）。
    #[serde(default)]
    pub biz_err: Vec<String>,
    /// 凭据所属域（host 后缀说明，如 ["doubao.com","www.doubao.com"]，纯展示用，不启动浏览器）。
    #[serde(default)]
    pub hosts: Vec<String>,
    /// 登录页 URL（前端「打开登录页」链接，引导用户在自己的浏览器登录后手动粘贴 Cookie；空 = 无）。
    #[serde(default)]
    pub login_url: String,
    #[serde(default)]
    pub notes: String,
    /// 是否支持对话反代（默认 true）。经全网调研确无纯 HTTP 对话端点的平台设 false：
    /// 目前仅 WPS 灵犀、360 纳米（仅登录/验活，官方逆向需浏览器或只有付费 API）。
    #[serde(default = "default_true")]
    pub chat_supported: bool,
    /// 站点区域：`cn`（国内站）| `intl`（国际站/海外，国内直连通常要代理）| `global`（同一入口、全球可达）。
    ///
    /// ★ 有国内/国际**双站点**的平台（Kimi、MiniMax、智谱 ↔ Z.AI、Qoder、Trae、WorkBuddy）
    /// 必须分别声明：两边的**账号与 Key 不通用**、登录入口也不同。前端据此打「国内 / 国际」
    /// 徽章、给出「该挂代理 / 该直连」的提示，并让上游连不上时**直接说清是代理问题**而不是
    /// 让人去怀疑凭据。
    #[serde(default = "default_region")]
    pub region: String,
}

impl ProviderSpec {
    /// chat 端点路径（带默认值语义：缺省 = OpenAI 标准）。
    pub fn chat_path(&self) -> &str {
        if self.chat_path.is_empty() {
            "/chat/completions"
        } else {
            &self.chat_path
        }
    }

    /// live 模型拉取路径（缺省 = /models）。
    pub fn models_path(&self) -> &str {
        if self.models_path.is_empty() {
            "/models"
        } else {
            &self.models_path
        }
    }

    /// 这个平台支持的登录方式（前端抽屉据此决定画哪些选项卡、测试据此核对 relay 覆盖）。
    ///
    /// 显式声明优先；没写就按 `auth` 推导出唯一一种 —— 老 toml 不用改。
    pub fn login_methods(&self) -> Vec<&str> {
        if !self.login_methods.is_empty() {
            return self.login_methods.iter().map(|s| s.as_str()).collect();
        }
        match self.auth {
            AuthKind::QrScan => vec!["qr"],
            AuthKind::SmsCode => vec!["sms"],
            AuthKind::OAuthDevice => vec!["device"],
            AuthKind::ApiKey | AuthKind::Cookie | AuthKind::ClientFile => vec!["paste"],
            AuthKind::Anon => vec![],
        }
    }

    /// 是否支持扫码登录。
    pub fn supports_qr(&self) -> bool {
        self.login_methods().contains(&"qr")
    }
}

fn default_true() -> bool {
    true
}

fn default_upstream_kind() -> String {
    "openai".into()
}

fn default_region() -> String {
    "global".into()
}

/// 校验 spec 合法性；返回第一条不合法原因。
pub fn validate(spec: &ProviderSpec) -> Result<(), String> {
    let id_ok = !spec.id.is_empty()
        && spec.id.len() <= 64
        && spec
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !id_ok {
        return Err(format!("id 非法：{:?}（须为 1-64 位小写字母/数字/-/_）", spec.id));
    }
    if spec.name.trim().is_empty() {
        return Err("name 不能为空".into());
    }
    if !spec.base_url.starts_with("http://")
        && !spec.base_url.starts_with("https://")
        && !spec.base_url.starts_with('@')
    {
        return Err(format!(
            "base_url 必须以 http(s):// 或 @ 占位符开头：{}",
            spec.base_url
        ));
    }
    if spec.base_url.starts_with("http") && spec.base_url.ends_with('/') {
        return Err("base_url 不带尾斜杠".into());
    }
    match spec.format.as_str() {
        "openai" | "anthropic" | "responses" => {}
        other => return Err(format!("format 非法：{other}（openai|anthropic|responses）")),
    }
    match spec.region.as_str() {
        "cn" | "intl" | "global" => {}
        other => return Err(format!("region 非法：{other}（cn|intl|global）")),
    }
    if !KNOWN_UPSTREAM_KINDS.contains(&spec.upstream_kind.as_str()) {
        return Err(format!(
            "upstream_kind 非法：{}（可选：{}）",
            spec.upstream_kind,
            KNOWN_UPSTREAM_KINDS.join("|")
        ));
    }
    match spec.signer.as_str() {
        "" | "glm" | "codearts" | "loomy" | "wb" => {}
        other => return Err(format!("signer 非法：{other}（空|glm|codearts|loomy|wb）")),
    }
    if spec.upstream_kind == "chatglm" && spec.signer != "glm" {
        return Err("upstream_kind=chatglm 要求 signer=glm".into());
    }
    if spec.models_filter != "" && spec.models_filter != "isFree" {
        return Err(format!("models_filter 非法：{}（空|isFree）", spec.models_filter));
    }
    if spec.models_policy == ModelsPolicy::Static && spec.models.is_empty() {
        return Err("models_policy=static 时 models 不能为空".into());
    }
    Ok(())
}

/// 所有已实现的 upstream_kind。
///
/// **新增 adapter 必须同时改两处**：这里的白名单 + `aigw-relay` 的 `has_adapter`。
/// 只改一边的后果分别是「toml 被静默跳过」和「静默走 OpenAI 透传」，
/// 后者更隐蔽，所以 `aigw-relay` 有一条测试反向校验本表。
pub const KNOWN_UPSTREAM_KINDS: &[&str] = &[
    "openai",
    "chatglm",
    "doubao",
    "yuanbao",
    "kuku",
    "metaso",
    "qwen-web",
    "mimo",
    "kiro",
    "deepseek-web",
    "opencode",
    "cloudcode",
    "qoder",
    "trae",
    "coze",
    "devin",
];

/// 从字符串解析并校验（测试与加载器共用）。
pub fn parse_spec(raw: &str) -> Result<ProviderSpec, String> {
    let spec: ProviderSpec = toml::from_str(raw).map_err(|e| format!("toml 解析：{e}"))?;
    validate(&spec)?;
    Ok(spec)
}

/// 加载单个 toml 文件。
pub fn load_file(path: &Path) -> Result<ProviderSpec, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    parse_spec(&raw)
}

/// 加载目录下全部 *.toml spec，**跳过非法文件并逐条告警**（不再整表失败）。
///
/// 为什么改：旧实现「任一非法即 `RegistryError::Multi`」会让 main 用空表启动，
/// 一个写错的 toml（或新增 upstream_kind/signer 未同步白名单）就能把 122 个平台
/// 静默打成 0 个，且只有一条 warn，排查成本极高。现在坏文件只丢自己。
///
/// 只有目录本身不可读才返回 Err（那是真的环境故障）。
pub fn load_dir(dir: &Path) -> Result<Vec<ProviderSpec>, RegistryError> {
    let (specs, errors) = load_dir_lenient(dir)?;
    if !errors.is_empty() {
        for (file, reason) in &errors {
            tracing::warn!("registry 跳过非法 spec {file}：{reason}");
        }
        tracing::warn!(
            "registry：{} 个 spec 加载成功，{} 个非法被跳过（服务继续启动）",
            specs.len(),
            errors.len()
        );
    }
    Ok(specs)
}

/// 加载目录，返回 (合法 spec, 逐文件错误)。目录不可读才 Err。
///
/// 供 `/api/health` 等面向上层展示「哪些 spec 被跳过了」。
pub fn load_dir_lenient(
    dir: &Path,
) -> Result<(Vec<ProviderSpec>, Vec<(String, String)>), RegistryError> {
    let entries = std::fs::read_dir(dir).map_err(RegistryError::Io)?;
    let mut specs = Vec::new();
    let mut errors: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let fname = p
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("?")
            .to_string();
        match load_file(&p) {
            Ok(s) => specs.push(s),
            Err(reason) => errors.push((fname, reason)),
        }
    }
    specs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((specs, errors))
}

/// 用 settings 表替换 spec 内的 `@word` 占位符（旧版语义：`@lobster` → settings.lobster_server）。
/// 未配置的占位符保留原样（该平台请求时会因 URL 非法而失败，属预期门控）。
pub fn apply_settings(specs: &mut [ProviderSpec], settings: &std::collections::BTreeMap<String, String>) {
    for s in specs.iter_mut() {
        if !s.base_url.starts_with('@') {
            continue;
        }
        let key = s.base_url[1..].to_string();
        if let Some(v) = settings.get(&key) {
            if !v.is_empty() {
                s.base_url = v.trim_end_matches('/').to_string();
            }
        }
    }
}

#[derive(Debug)]
pub enum RegistryError {
    Io(std::io::Error),
    Invalid { file: String, reason: String },
    Multi(Vec<(String, String)>),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Io(e) => write!(f, "IO：{e}"),
            RegistryError::Invalid { file, reason } => write!(f, "{file} 非法：{reason}"),
            RegistryError::Multi(list) => {
                write!(f, "{} 个 spec 非法：", list.len())?;
                for (i, (file, reason)) in list.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{file}（{reason}）")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
id = "demo"
name = "Demo 平台"
enabled = true
auth = "api_key"
base_url = "https://example.com/v1"
format = "openai"
models = ["m-1", "m-2"]
models_policy = "static"
stream_dialect = "sse_delta"
"#;

    #[test]
    fn parse_valid_spec() {
        let s = parse_spec(VALID).unwrap();
        assert_eq!(s.id, "demo");
        assert_eq!(s.auth, AuthKind::ApiKey);
        assert_eq!(s.models.len(), 2);
        assert!(s.enabled);
    }

    #[test]
    fn region_defaults_to_global_and_rejects_unknown() {
        // 没写 region 的老 toml 必须继续能加载（默认 global），否则升级会让 80 个
        // provider 全被 load_dir_lenient 跳过——平台数直接从 127 掉到 0。
        let s = parse_spec(VALID).unwrap();
        assert_eq!(s.region, "global");
        // 写了但拼错要**报错**而不是静默 global：区域错了会给出完全相反的代理建议
        let raw = VALID.replace("stream_dialect", "region = \"jp\"\nstream_dialect");
        let err = parse_spec(&raw).unwrap_err().to_string();
        assert!(err.contains("region 非法"), "err={err}");
    }

    #[test]
    fn parse_anon_zen_style() {
        // 与 providers/anon-zen.toml 同构（headers 表 + 空 models + live 策略）
        let raw = r#"
id = "anon-zen"
name = "Our Free Model（OpenCode Zen 匿名车道）"
enabled = true
auth = "anon"
base_url = "https://opencode.ai/zen/v1"
format = "openai"
models = []
models_policy = "live"
stream_dialect = "sse_delta"

[headers]
Authorization = "Bearer public"
User-Agent = "opencode/1.18.31"
"#;
        let s = parse_spec(raw).unwrap();
        assert_eq!(s.headers.get("Authorization").map(String::as_str), Some("Bearer public"));
        assert_eq!(s.stream_dialect, StreamDialect::SseDelta);
    }

    #[test]
    fn reject_bad_format() {
        let raw = VALID.replace("openai", "graphql");
        let err = parse_spec(&raw).unwrap_err();
        assert!(err.contains("format"), "err={err}");
    }

    #[test]
    fn reject_missing_auth() {
        let raw = VALID.replace("auth = \"api_key\"", "");
        let err = parse_spec(&raw).unwrap_err();
        assert!(err.contains("missing field") || err.contains("auth"), "err={err}");
    }

    #[test]
    fn reject_bad_id_and_url() {
        let raw = VALID.replace("id = \"demo\"", "id = \"DEMO!\"");
        assert!(parse_spec(&raw).unwrap_err().contains("id 非法"));
        let raw = VALID.replace("https://", "ftp://");
        assert!(parse_spec(&raw).unwrap_err().contains("base_url"));
    }

    #[test]
    fn reject_static_with_empty_models() {
        let raw = VALID.replace("models = [\"m-1\", \"m-2\"]", "models = []");
        let err = parse_spec(&raw).unwrap_err();
        assert!(err.contains("static"), "err={err}");
    }

    #[test]
    fn load_dir_skips_bad_file_and_keeps_rest() {
        // 容错语义：坏文件只丢自己，好文件照常加载（旧行为是整表失败 → 空表启动）
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a-good.toml"), VALID).unwrap();
        std::fs::write(
            tmp.path().join("b-good.toml"),
            VALID.replace("id = \"demo\"", "id = \"beta\""),
        )
        .unwrap();
        std::fs::write(tmp.path().join("c-bad.toml"), "id = \"x\"\nname = \"坏\"").unwrap();
        let specs = load_dir(tmp.path()).expect("坏文件不应导致整表失败");
        assert_eq!(specs.len(), 2, "两个好文件应保留：{:?}", specs.iter().map(|s| &s.id).collect::<Vec<_>>());
        assert!(specs.iter().any(|s| s.id == "demo"));
        assert!(specs.iter().any(|s| s.id == "beta"));
    }

    #[test]
    fn load_dir_lenient_reports_each_bad_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a-good.toml"), VALID).unwrap();
        std::fs::write(tmp.path().join("c-bad.toml"), "id = \"x\"\nname = \"坏\"").unwrap();
        std::fs::write(tmp.path().join("d-bad.toml"), "id = \"y\"").unwrap();
        let (specs, errors) = load_dir_lenient(tmp.path()).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(errors.len(), 2);
        let names: Vec<&str> = errors.iter().map(|(f, _)| f.as_str()).collect();
        assert!(names.contains(&"c-bad.toml"));
        assert!(names.contains(&"d-bad.toml"));
    }

    #[test]
    fn load_dir_sorted_and_ignores_non_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("zeta.toml"), VALID.replace("id = \"demo\"", "id = \"zeta\""))
            .unwrap();
        std::fs::write(
            tmp.path().join("alpha.toml"),
            VALID.replace("id = \"demo\"", "id = \"alpha\""),
        )
        .unwrap();
        std::fs::write(tmp.path().join("readme.md"), "ignore me").unwrap();
        let specs = load_dir(tmp.path()).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].id, "alpha");
        assert_eq!(specs[1].id, "zeta");
    }

    /// 仓库里真正随包发布的 `providers/*.toml` 必须全部合法。
    ///
    /// 为什么必须有这条：`load_dir` 是 lenient 的（坏文件只 warn 后跳过），
    /// 所以一个漏写的必填字段（真实事故：anon-zen.toml 少了 `models_policy`）
    /// 会让整个平台**静默消失** —— 服务照常启动、`/api/platforms` 只是少一行，
    /// 直到用户发现某个模型 404 才暴露。这条测试把「静默跳过」变成 CI 红灯。
    #[test]
    fn shipped_providers_are_all_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("providers");
        if !dir.is_dir() {
            // 打包环境（只带编译产物、不带源码树）跳过，不算失败。
            eprintln!("跳过：找不到 {}（非源码树环境）", dir.display());
            return;
        }
        let (specs, errors) = load_dir_lenient(&dir).expect("providers 目录应可读");
        assert!(
            specs.len() >= 50,
            "随包 spec 数量异常偏少（{}），可能整目录解析失败",
            specs.len()
        );
        assert!(
            errors.is_empty(),
            "有 {} 个随包 spec 非法（会被静默跳过）：{:?}",
            errors.len(),
            errors
        );
    }

    /// 扫码平台的两半必须对上：toml 说 `auth = "qr_scan"` 的平台，relay 里必须有真的
    /// 「出码 + 轮询」实现；否则面板会给用户画一个永远扫不出结果的二维码。
    /// 反之 relay 有实现、toml 却写成 cookie 的平台也会掉进「填 Cookie」分支，
    /// 扫码入口根本到不了（2026-10 的 Loomy 就是这个坑，排查花了很久）。
    #[test]
    fn qr_scan_platforms_have_a_matching_relay_implementation() {
        // relay 的 LoginEngine::start 里真正实现了扫码轮询的平台 id。
        // 改动这里必须同时改 aigw-relay/src/auth_login.rs 的分派分支。
        const IMPLEMENTED: &[&str] = &["apk-doubao", "apk-kuku", "apk-loomy", "apk-raccoon"];
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("providers");
        if !dir.is_dir() {
            eprintln!("跳过：找不到 {}（非源码树环境）", dir.display());
            return;
        }
        let (specs, _) = load_dir_lenient(&dir).expect("providers 目录应可读");
        let mut qr: Vec<String> = specs
            .iter()
            .filter(|s| s.supports_qr())
            .map(|s| s.id.clone())
            .collect();
        qr.sort();
        assert!(!qr.is_empty(), "至少要有一个扫码平台，否则前端「仅看扫码」筛选永远是 0");
        for id in &qr {
            assert!(
                IMPLEMENTED.contains(&id.as_str()),
                "{id} 声明了 auth=qr_scan，但 relay 里没有对应的扫码实现（会出一个扫不出结果的码）"
            );
        }
        // 反方向：有实现却没声明 qr 的平台，抽屉里根本走不到扫码分支。
        for id in IMPLEMENTED {
            let s = specs.iter().find(|s| s.id == *id);
            let Some(s) = s else { continue };
            assert!(
                s.supports_qr(),
                "{id} 在 relay 里有扫码实现，toml 却没列 qr（auth={:?}，login_methods={:?}）—— \
                 用户会在抽屉里只看到手填框",
                s.auth,
                s.login_methods
            );
        }
    }

    #[test]
    fn load_dir_missing_dir_is_io() {
        assert!(matches!(
            load_dir(Path::new("Z:/definitely/not/here")),
            Err(RegistryError::Io(_))
        ));
    }
}
