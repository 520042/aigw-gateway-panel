//! 平台注册表（trait 抽象 + 数据驱动构建）。
//!
//! 从 `catalog_bundled.json` 一次性构造出所有平台的 [`CatalogPlatform`]，每个都实现
//! [`PlatformProvider`]。`@pid` 风格的中继全部委托给对应平台的 `relay()`。新增平台只需在
//! catalog 数据里登记，无需改 Rust 代码。顶层 `resolve()` 还负责 auto/free/upstream 兜底。
use crate::catalog::Catalog;
use crate::core::config::Settings;
use crate::core::model::{LoginMethod, Plan, RelayRequest};
use crate::core::traits::{PlatformProvider, ResolverCtx};
use crate::relay::{POLLINATIONS_BASE, POLLINATIONS_PATH};
use http::HeaderMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// 基于 catalog 数据的平台实现。
pub struct CatalogPlatform {
    pub pid: String,
    pub name: String,
    pub login: LoginMethod,
    /// 该平台的 relay_spec / free_lane 规格（匿名车道可直接打）。
    pub spec: Option<Value>,
}

impl PlatformProvider for CatalogPlatform {
    fn id(&self) -> &str {
        &self.pid
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn login_method(&self) -> LoginMethod {
        self.login.clone()
    }
    fn relay(&self, req: &RelayRequest, _ctx: &ResolverCtx) -> Plan {
        let spec = match &self.spec {
            Some(s) => s,
            None => {
                return Plan::NeedLogin {
                    pid: self.pid.clone(),
                    name: self.name.clone(),
                }
            }
        };
        let base = spec.get("base").and_then(|v| v.as_str()).unwrap_or("");
        let path = spec
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("/chat/completions");
        let auth = spec.get("auth").and_then(|v| v.as_str()).unwrap_or("bearer");
        let secret_fixed = spec.get("secret_fixed").and_then(|v| v.as_str());

        // 占位符 base（如 @lobster）→ 需自定义服务器/登录
        if base.starts_with('@') {
            return Plan::NeedLogin {
                pid: self.pid.clone(),
                name: self.name.clone(),
            };
        }
        // 免凭据公共车道：auth=none 或 secret_fixed 直接打
        if auth == "none" || secret_fixed.is_some() {
            let url = format!("{}{}", base.trim_end_matches('/'), path);
            let override_model = if req.model.is_empty() {
                Some("gpt-oss-20b".to_string())
            } else {
                Some(req.model.clone())
            };
            return Plan::Public {
                url,
                model_override: override_model,
            };
        }
        // 需要 Bearer/登录的中继平台
        Plan::NeedLogin {
            pid: self.pid.clone(),
            name: self.name.clone(),
        }
    }
}

/// 全部已注册平台的集合。
pub struct Registry {
    platforms: HashMap<String, CatalogPlatform>,
    #[allow(dead_code)]
    catalog: Arc<Catalog>,
    data_dir: std::path::PathBuf,
}

impl Registry {
    /// 从 catalog 构建注册表：遍历 platforms / relay_specs / free_lanes。
    pub fn build(catalog: Arc<Catalog>, data_dir: std::path::PathBuf) -> Self {
        let mut platforms: HashMap<String, CatalogPlatform> = HashMap::new();

        // 1) relay_specs（网页反代 + 公共车道）
        if let Some(obj) = catalog.raw_get("relay_specs").and_then(|v| v.as_object()) {
            for (pid, spec) in obj {
                platforms.insert(
                    pid.clone(),
                    CatalogPlatform {
                        pid: pid.clone(),
                        name: spec
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(pid)
                            .to_string(),
                        login: LoginMethod::Unknown,
                        spec: Some(spec.clone()),
                    },
                );
            }
        }

        // 2) free_lanes（免凭据车道）
        if let Some(obj) = catalog.raw_get("free_lanes").and_then(|v| v.as_object()) {
            for (pid, spec) in obj {
                platforms.entry(pid.clone()).or_insert(CatalogPlatform {
                    pid: pid.clone(),
                    name: spec
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(pid)
                        .to_string(),
                    login: LoginMethod::None,
                    spec: Some(spec.clone()),
                });
            }
        }

        // 3) 平台目录（以 login_method 元数据为主）
        if let Some(obj) = catalog.platforms().as_object() {
            for (pid, spec) in obj {
                let method = spec.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let login = if method.contains("oauth") {
                    LoginMethod::OAuth2
                } else if method.contains("qr") || method.contains("qrcode") {
                    LoginMethod::QrCode
                } else if method.contains("cookie") {
                    LoginMethod::Cookie
                } else if method.contains("file") {
                    LoginMethod::File
                } else {
                    LoginMethod::Unknown
                };
                platforms.entry(pid.clone()).or_insert(CatalogPlatform {
                    pid: pid.clone(),
                    name: spec
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(pid)
                        .to_string(),
                    login,
                    spec: None,
                });
            }
        }

        Registry {
            platforms,
            catalog,
            data_dir,
        }
    }

    pub fn get(&self, pid: &str) -> Option<&CatalogPlatform> {
        self.platforms.get(pid)
    }

    pub fn ids(&self) -> Vec<&str> {
        self.platforms.keys().map(|s| s.as_str()).collect()
    }

    /// 顶层解析：auto/free/upstream 兜底 + `@pid` 委托给平台 provider。
    /// `@custom-<id>` 走用户自定义 API（base_url + key → Upstream）。
    pub fn resolve(
        &self,
        model: &str,
        body: &[u8],
        headers: &HeaderMap,
        cfg: &Settings,
    ) -> Plan {
        if let Some((mid, pid)) = model.rsplit_once('@') {
            // 自定义 API 车道：@custom-<id>
            if let Some(cid) = pid.strip_prefix("custom-") {
                return self.resolve_custom(cid, mid, body, headers, cfg);
            }
            // 豆包原生中继（凭据取账号池 apk-doubao）
            if pid == "apk-doubao" {
                return Plan::Native {
                    pid: "apk-doubao".to_string(),
                    model: if mid.is_empty() { "doubao-pro".to_string() } else { mid.to_string() },
                };
            }
            // 通用 OpenAI 兼容原生中继（小浣熊 / Trae 等；凭据取账号池 <pid>）
            if matches!(pid, "apk-raccoon" | "apk-trae") {
                return Plan::Native {
                    pid: pid.to_string(),
                    model: if mid.is_empty() { "default".to_string() } else { mid.to_string() },
                };
            }
            // 元宝多步私有协议原生中继（凭据取账号池 apk-yuanbao）
            if pid == "apk-yuanbao" {
                return Plan::Native {
                    pid: "apk-yuanbao".to_string(),
                    model: if mid.is_empty() { "deepseek".to_string() } else { mid.to_string() },
                };
            }
            // 网页版反代（智谱清言 / Trae 国内 / DeepSeek 网页）：私有协议，
            // 凭据取账号池 <pid>（对接平台账户里粘贴的 refresh_token / x-ide-token / userToken）。
            if crate::relay::web::is_web_pid(pid) {
                return Plan::Web {
                    pid: pid.to_string(),
                    model: if mid.is_empty() {
                        crate::relay::web::default_model(pid).to_string()
                    } else {
                        mid.to_string()
                    },
                };
            }
            match self.get(pid) {
                Some(p) => {
                    let req = RelayRequest {
                        model: mid.to_string(),
                        body: body.to_vec(),
                        headers: headers.clone(),
                    };
                    p.relay(&req, &ResolverCtx { cfg })
                }
                None => Plan::NeedLogin {
                    pid: pid.to_string(),
                    name: pid.to_string(),
                },
            }
        } else if crate::relay::doubao::is_doubao_model(model) {
            // 裸名豆包模型（doubao-pro 等）直走原生中继
            Plan::Native {
                pid: "apk-doubao".to_string(),
                model: model.to_string(),
            }
        } else if model == "auto" || model == "free" {
            Plan::Public {
                url: format!("{}{}", POLLINATIONS_BASE, POLLINATIONS_PATH),
                model_override: Some("gpt-oss-20b".to_string()),
            }
        } else if !cfg.upstream_base.trim().is_empty() {
            Plan::Upstream {
                url: format!("{}/chat/completions", cfg.upstream_base.trim_end_matches('/')),
                key: None,
            }
        } else {
            Plan::Public {
                url: format!("{}{}", POLLINATIONS_BASE, POLLINATIONS_PATH),
                model_override: Some(model.to_string()),
            }
        }
    }

    /// 自定义 API：{base_url}/chat/completions + Bearer key（有 key 才带）。
    fn resolve_custom(
        &self,
        cid: &str,
        mid: &str,
        body: &[u8],
        headers: &HeaderMap,
        _cfg: &Settings,
    ) -> Plan {
        let store = crate::server::store::load("custom_apis");
        let cfg_json = store
            .get("apis")
            .and_then(|a| a.get(&format!("custom-{}", cid)))
            .cloned();
        let Some(cfg_json) = cfg_json else {
            return Plan::NeedLogin {
                pid: format!("custom-{}", cid),
                name: format!("自定义 API custom-{}", cid),
            };
        };
        let base = cfg_json
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        if base.is_empty() {
            return Plan::NeedLogin {
                pid: format!("custom-{}", cid),
                name: cfg_json
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("自定义 API")
                    .to_string(),
            };
        }
        let _ = (mid, body, headers); // model 由请求体携带，透传即可
        Plan::Upstream {
            url: format!("{}/chat/completions", base),
            key: cfg_json
                .get("api_key")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
        }
    }
}
