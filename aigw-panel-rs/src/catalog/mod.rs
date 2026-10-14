//! 平台目录（单一数据源）。
//!
//! 数据来自 `catalog_bundled.json`（由 `gen_catalog.py` 从 Python 一手模块导出，零手写）。
//! 提供：平台清单 / 模型总表分组(grouped) / relay_spec 查询 / 免凭据车道查询。
//! 注意 include 路径：本文件位于 `src/catalog/mod.rs`，故回退两层到 crate 根。
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Clone)]
pub struct Catalog {
    raw: Value,
}

impl Catalog {
    pub fn load() -> Self {
        let raw: Value = serde_json::from_str(include_str!("../../catalog_bundled.json"))
            .expect("catalog_bundled.json 解析失败");
        Catalog { raw }
    }

    pub fn platforms(&self) -> &Value {
        self.raw.get("platforms").unwrap_or(&Value::Null)
    }

    /// 原始字段访问（registry 构建时需要 relay_specs / free_lanes / bundled）。
    pub fn raw_get(&self, key: &str) -> Option<&Value> {
        self.raw.get(key)
    }

    // ── 接入源页契约（对齐 Python gwlogin.platform_list / app.sources） ──────

    /// 对齐 `gwlogin.platform_list()`：平台**数组**，抽屉卡片必需字段齐全。
    /// 前端 `for(const p of loginPlatforms)` 直接迭代，绝不能返回对象。
    pub fn platform_list(&self) -> Value {
        let mut out: Vec<Value> = Vec::new();
        if let Some(obj) = self.platforms().as_object() {
            for (pid, p) in obj {
                out.push(json!({
                    "id": pid,
                    "name": p.get("name").and_then(|v| v.as_str()).unwrap_or(pid),
                    "method": p.get("method").and_then(|v| v.as_str()).unwrap_or(""),
                    "edition": p.get("edition").and_then(|v| v.as_str()).unwrap_or(""),
                    "hosts": p.get("hosts").cloned().unwrap_or(json!([])),
                    "login_url": p.get("login_url").and_then(|v| v.as_str()).unwrap_or(""),
                    "upstream": p.get("upstream").and_then(|v| v.as_str()).unwrap_or(""),
                    "hint": p.get("hint").and_then(|v| v.as_str()).unwrap_or(""),
                    "gateway": p.get("gateway").and_then(|v| v.as_bool()).unwrap_or(false),
                    "no_remote_scan": p.get("no_remote_scan").and_then(|v| v.as_bool()).unwrap_or(false),
                    "browser_capture": p.get("browser_capture").and_then(|v| v.as_bool()).unwrap_or(false),
                    "paste_type": p.get("paste_type").and_then(|v| v.as_str()).unwrap_or(""),
                    "placeholder": p.get("placeholder").and_then(|v| v.as_str()).unwrap_or(""),
                    "qr": false, // 远程扫码支持清单待 qrlogin 模块接入；当前如实标不支持
                }));
            }
        }
        Value::Array(out)
    }

    fn sources_meta(&self) -> &Value {
        self.raw_get("sources_meta").unwrap_or(&Value::Null)
    }

    pub fn categories(&self) -> Vec<Value> {
        self.sources_meta()
            .get("categories")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    pub fn platform_facts(&self) -> serde_json::Map<String, Value> {
        self.sources_meta()
            .get("platform_facts")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default()
    }

    pub fn local_proxies(&self) -> Vec<Value> {
        self.sources_meta()
            .get("local_proxies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    /// 公益站 / 站点清单（app/catalog.all_sites）。
    pub fn catalog_sites(&self) -> Vec<Value> {
        self.raw_get("catalog_sites")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    /// 对齐 `sources.kind_of`：有桌面客户端 → LOCAL；有 API → API；否则 WEB。
    pub fn kind_of(&self, pid: &str) -> &'static str {
        if pid == "gateway" || pid == "localproxy" {
            return "LOCAL";
        }
        match self.platform_facts().get(pid) {
            None => "WEB",
            Some(f) => {
                if f.get("desktop").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "LOCAL"
                } else if f.get("api").and_then(|v| v.as_bool()).unwrap_or(false) {
                    "API"
                } else {
                    "WEB"
                }
            }
        }
    }

    /// 对齐 `sources.category_meta`：按 kind 找 CATEGORIES 条目。
    pub fn category_meta(&self, kind: &str) -> Value {
        for c in self.categories() {
            if c.get("kind").and_then(|v| v.as_str()) == Some(kind) {
                return c;
            }
        }
        json!({"kind": kind, "name": kind, "icon": "?", "desc": "", "color": ""})
    }

    /// 对齐 `/api/models?action=sources` 的 platform_kinds。
    pub fn platform_kinds(&self) -> Value {
        let facts = self.platform_facts();
        let mut out = serde_json::Map::new();
        for pid in facts.keys() {
            let kind = self.kind_of(pid);
            out.insert(
                pid.clone(),
                json!({
                    "kind": kind,
                    "facts": facts.get(pid).cloned().unwrap_or(Value::Null),
                    "meta": self.category_meta(kind),
                }),
            );
        }
        Value::Object(out)
    }

    /// 对齐 `sources.summary`：每类各有多少平台（KPI 用）。
    pub fn sources_summary(&self) -> Value {
        let mut all: Vec<String> = self.platform_facts().keys().cloned().collect();
        for lp in self.local_proxies() {
            if let Some(id) = lp.get("id").and_then(|v| v.as_str()) {
                all.push(id.to_string());
            }
        }
        all.sort();
        all.dedup();
        let (mut l, mut a, mut w) = (0u64, 0u64, 0u64);
        for p in &all {
            match self.kind_of(p) {
                "LOCAL" => l += 1,
                "API" => a += 1,
                _ => w += 1,
            }
        }
        json!({"LOCAL": l, "API": a, "WEB": w, "total": all.len()})
    }

    /// 对齐 `gwextra.catalog_sources`：只列有模型能力的平台（has_models/has_catalog/can_list）。
    pub fn catalog_sources(&self) -> Value {
        let mut out: Vec<Value> = Vec::new();
        let bundled = self.raw_get("bundled");
        if let Some(obj) = self.platforms().as_object() {
            for (pid, spec) in obj {
                let has_models = self.relay_spec(pid).is_some()
                    || bundled.and_then(|b| b.get(pid)).is_some();
                if !has_models {
                    continue; // 既没 models 也没目录，别列出来凑数（对齐 Python）
                }
                out.push(json!({
                    "platform": pid,
                    "name": spec.get("name").and_then(|v| v.as_str()).unwrap_or(pid),
                    "has_models": true,
                    "has_catalog": false,
                    "can_list": true,
                    "urls": [],
                    "has_account": false,
                }));
            }
        }
        Value::Array(out)
    }

    pub fn relay_spec(&self, pid: &str) -> Option<&Value> {
        self.raw.get("relay_specs")?.get(pid)
    }

    pub fn free_lane(&self, pid: &str) -> Option<&Value> {
        self.raw.get("free_lanes")?.get(pid)
    }

    /// 模型总表（对齐 Python `/api/models?action=grouped`）。
    /// 分组顺序：自动路由 → 豆包 → WorkBuddy 国内 → Trae/Go/WorkBuddy国际/元宝 → 网页反代 → 免凭据车道。
    pub fn grouped(&self, disabled: &HashSet<String>) -> Value {
        let mut groups: Vec<Value> = Vec::new();
        let bundled = self.raw.get("bundled").cloned().unwrap_or(Value::Null);
        let relay = self.raw.get("relay_specs").cloned().unwrap_or(Value::Null);
        let free = self.raw.get("free_lanes").cloned().unwrap_or(Value::Null);

        // ① 自动路由（统一接口推荐入口）
        let autos = vec![
            json!({"id":"auto","name":"自动路由 · 最快优先","desc":"所有健康上游里延迟最低的优先"}),
            json!({"id":"free","name":"自动路由 · 免费优先","desc":"免费车道优先，免登录即可用"}),
        ];
        groups.push(self.mk_group(
            "auto", "自动路由",
            "对话 model 填 auto（最快优先）/ free（免费优先）",
            &autos, false, false, 0, disabled,
        ));

        // ② 豆包（samantha 原生中继，裸名直出）
        if let Some(rows) = bundled.get("apk-doubao").and_then(|v| v.as_array()) {
            groups.push(self.mk_group(
                "apk-doubao", "豆包",
                "model 直接写 doubao-pro 等，无需 @后缀",
                rows, false, false, 0, disabled,
            ));
        }

        // ③ WorkBuddy 国内（带 @wb-gateway 后缀）
        groups.push(self.suffixed_group(
            "wb-gateway", "WorkBuddy 国内",
            "国内站 copilot.tencent.com；凭据隔离、积分共享",
            bundled.get("wb-gateway"), disabled,
        ));

        // ④ 各中继平台内置清单
        for (pid, name, hint) in [
            ("apk-trae", "Trae 国内", "对话 model 带 @apk-trae 后缀即走原生中继"),
            ("apk-go", "Go 网关", "对话 model 带 @apk-go 后缀即走原生中继"),
            ("wb-gateway-intl", "WorkBuddy 国际", "国际站 www.workbuddy.ai；凭据隔离、积分共享"),
            ("apk-yuanbao", "元宝", "对话 model 带 @apk-yuanbao 后缀即走原生中继"),
        ] {
            groups.push(self.suffixed_group(pid, name, hint, bundled.get(pid), disabled));
        }

        // ⑤ 网页反代静态清单（固定列表）
        for pid in [
            "web-qoder", "web-lobster", "apk-raccoon",
            "web-glm", "web-trae", "web-deepseek", "pollinations",
        ] {
            let (name, rows): (String, Value) = if pid == "pollinations" {
                let fl = free.get(pid);
                let rows = fl.and_then(|v| v.get("models_anon")).cloned().unwrap_or(Value::Null);
                (
                    fl.and_then(|v| v.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(pid)
                        .to_string(),
                    rows,
                )
            } else {
                let sp = relay.get(pid);
                let rows = sp.and_then(|v| v.get("models_static")).cloned().unwrap_or(Value::Null);
                (
                    sp.and_then(|v| v.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(pid)
                        .to_string(),
                    rows,
                )
            };
            groups.push(self.suffixed_group(pid, &name, "静态清单（社区逆向）", Some(&rows), disabled));
        }

        // ⑥ 免凭据车道（anon-zen / kilo / pollinations…）
        //   models_anon 有两种形态：
        //   - 数组：静态清单（pollinations），直接用
        //   - URL 字符串：需联网拉动态清单（anon-zen/kilo），读 free_lane_models 缓存
        let lane_cache = crate::server::free_lane_models::get_cached();
        if let Some(obj) = free.as_object() {
            for (pid, spec) in obj {
                let name = spec
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(pid)
                    .to_string();
                let rows = match spec.get("models_anon") {
                    Some(Value::Array(a)) => Value::Array(a.clone()),
                    _ => {
                        // 动态车道：从缓存取（无缓存回空，前端显示"点刷新拉取"）
                        lane_cache
                            .as_ref()
                            .and_then(|c| c.get(pid.as_str()))
                            .filter(|v| v.is_array())
                            .cloned()
                            .unwrap_or_else(|| Value::Array(vec![]))
                    }
                };
                // 状态提示：被上游收紧的车道（如 anon-zen 2026-10-14 FreeTierError）
                // 在描述里如实标注，避免用户看到模型清单以为能对话。
                let status_note = spec.get("status_note").and_then(|v| v.as_str()).unwrap_or("");
                let hint = if status_note.is_empty() {
                    "免凭据直连：无需登录/注册/填 Key（点「接入」只是登记进路由）".to_string()
                } else {
                    format!("免凭据直连（无需登录/注册/填 Key）· ⚠ {}", status_note)
                };
                let g = self.suffixed_group(
                    pid,
                    &name,
                    &hint,
                    Some(&rows),
                    disabled,
                );
                groups.push(mark_free_lane(g));
            }
        }

        let total: usize = groups
            .iter()
            .map(|g| g.get("models_total").and_then(|v| v.as_u64()).unwrap_or(0) as usize)
            .sum();
        let enabled: usize = groups
            .iter()
            .map(|g| g.get("models_on").and_then(|v| v.as_u64()).unwrap_or(0) as usize)
            .sum();
        json!({
            "groups": groups,
            "total": total,
            "enabled_total": enabled,
            "disabled": Vec::<String>::new(),
        })
    }

    fn mk_group(
        &self,
        pid: &str,
        name: &str,
        hint: &str,
        rows: &[Value],
        free_lane: bool,
        live: bool,
        usable: usize,
        disabled: &HashSet<String>,
    ) -> Value {
        let models: Vec<Value> = rows
            .iter()
            .map(|m| {
                let mut r = m.clone();
                if let Value::Object(ref mut o) = r {
                    let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    // disabled 记账用裸名（对齐 Python：id@pid → 剥后缀），
                    // 自动路由/豆包本身是裸名，两者都查
                    let bare = match id.rsplit_once('@') {
                        Some((b, _)) => b.to_string(),
                        None => id.clone(),
                    };
                    let on = !disabled.contains(&bare) && !disabled.contains(&id);
                    o.insert("on".to_string(), Value::Bool(on));
                }
                r
            })
            .collect();
        let on = models
            .iter()
            .filter(|m| m.get("on") == Some(&Value::Bool(true)))
            .count();
        json!({
            "pid": pid,
            "name": name,
            "hint": hint,
            "live": live,
            "usable": usable,
            "models": models,
            "free_lane": free_lane,
            "models_total": models.len(),
            "models_on": on,
        })
    }

    fn suffixed_group(
        &self,
        pid: &str,
        name: &str,
        hint: &str,
        rows: Option<&Value>,
        disabled: &HashSet<String>,
    ) -> Value {
        let arr = rows.and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let suffixed: Vec<Value> = arr
            .iter()
            .map(|m| {
                let mut r = m.clone();
                if let Value::Object(ref mut o) = r {
                    let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let new_id = if id.is_empty() || id.contains('@') {
                        id.clone()
                    } else {
                        format!("{}@{}", id, pid)
                    };
                    o.insert("id".to_string(), Value::String(new_id));
                    if o.get("name").is_none() || o.get("name") == Some(&Value::Null) {
                        o.insert("name".to_string(), Value::String(id));
                    }
                }
                r
            })
            .collect();
        self.mk_group(pid, name, hint, &suffixed, false, false, 0, disabled)
    }
}

fn mark_free_lane(mut g: Value) -> Value {
    if let Value::Object(ref mut o) = g {
        o.insert("free_lane".to_string(), Value::Bool(true));
    }
    g
}
