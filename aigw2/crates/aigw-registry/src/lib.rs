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
    /// 上游 API 根地址（不带尾斜杠），如 `https://opencode.ai/zen/v1`。
    pub base_url: String,
    /// 线协议：openai | anthropic | responses。
    pub format: String,
    /// 静态模型清单（models_policy=live 时可为空）。
    #[serde(default)]
    pub models: Vec<String>,
    pub models_policy: ModelsPolicy,
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
    /// 请求签名器：""（无）| glm（X-Sign 头组，aigw-crypto::glm_build_sign）。
    #[serde(default)]
    pub signer: String,
    /// 假成功指纹关键词：HTTP 2xx 且 body 命中任一关键词 → Auth 类错误（loomy/星火系）。
    #[serde(default)]
    pub biz_err: Vec<String>,
    /// CDP 抓 Cookie 的域过滤清单（host 后缀命中，如 ["doubao.com","www.doubao.com"]）。
    #[serde(default)]
    pub hosts: Vec<String>,
    /// 登录页 URL（CDP 受控浏览器打开用；空 = 无自动登录）。
    #[serde(default)]
    pub login_url: String,
    #[serde(default)]
    pub notes: String,
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
}

fn default_true() -> bool {
    true
}

fn default_upstream_kind() -> String {
    "openai".into()
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
    match spec.upstream_kind.as_str() {
        "openai" | "chatglm" => {}
        other => return Err(format!("upstream_kind 非法：{other}（openai|chatglm）")),
    }
    match spec.signer.as_str() {
        "" | "glm" => {}
        other => return Err(format!("signer 非法：{other}（空|glm）")),
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

/// 加载目录下全部 *.toml spec；任一非法则报 [`RegistryError::Multi`]（含逐文件原因）。
pub fn load_dir(dir: &Path) -> Result<Vec<ProviderSpec>, RegistryError> {
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
    if !errors.is_empty() {
        return Err(RegistryError::Multi(errors));
    }
    specs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(specs)
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
    fn load_dir_reports_each_bad_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a-good.toml"), VALID).unwrap();
        std::fs::write(
            tmp.path().join("b-good.toml"),
            VALID.replace("id = \"demo\"", "id = \"beta\""),
        )
        .unwrap();
        std::fs::write(tmp.path().join("c-bad.toml"), "id = \"x\"\nname = \"坏\"").unwrap();
        // 非法 spec：缺其余必填字段
        let err = load_dir(tmp.path()).unwrap_err();
        match err {
            RegistryError::Multi(list) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].0, "c-bad.toml");
            }
            other => panic!("期望 Multi，得到 {other:?}"),
        }
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

    #[test]
    fn load_dir_missing_dir_is_io() {
        assert!(matches!(
            load_dir(Path::new("Z:/definitely/not/here")),
            Err(RegistryError::Io(_))
        ));
    }
}
