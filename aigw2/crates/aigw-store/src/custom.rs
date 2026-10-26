//! 自定义 API 存储（v2 接入源页「自定义 API」）。
//!
//! 参考 CherryStudio 的自定义供应商思路：用户自填 名称/协议/URL/Key，
//! 面板把每条自定义 API 动态并进 specs，走统一 OpenAI/Anthropic 透传。
//! 持久化到 `data/custom.json`（与 accounts.json 并列）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 自定义 API 的 Key 走这条 DPAPI 附加熵（与账号池、面板令牌互不相通）。
pub const SECRET_PURPOSE: &str = "custom";

/// 自定义 API 的字段长度上界（名字 / URL / Key）。
///
/// 没有上界时实测 100KB 的名字、5MB 的 Key 都能 200 落盘，custom.json 每次读写
/// 与面板每次回吐都要过一遍这些字节。真实取值远小于这些上限。
pub const MAX_CUSTOM_NAME_LEN: usize = 120;
pub const MAX_CUSTOM_URL_LEN: usize = 2048;
pub const MAX_CUSTOM_KEY_LEN: usize = 8 * 1024;
/// 单个自定义 API 记下的模型条数上限（上游 /models 可能回几千条）。
pub const MAX_CUSTOM_MODELS: usize = 2000;

/// 一条自定义 API。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomApi {
    /// 唯一 id（custom-{hex}）。
    pub id: String,
    /// 用户起的名称（如 "我的中转"）。
    pub name: String,
    /// 协议：openai | anthropic。
    pub format: String,
    /// 上游 base_url（不带尾斜杠）。
    pub base_url: String,
    /// 用户自己的 API Key。
    ///
    /// 内存里是**明文**（中继要拼 Authorization）；落盘时由 `persist_now` 换成
    /// `dpapi:<b64>`，装载时再解开 —— 与账号池同一套（见 `crate::secret`）。
    pub api_key: String,
    /// 密文解不开（换过机器 / 换过 Windows 用户 / 文件被改过）。
    ///
    /// 这种条目**保留原密文、不参与转发**：把密文当 Key 发出去只会换来上游 401 谜团。
    /// 用户重新填一次 Key 就会清掉这个标记。
    #[serde(default)]
    pub key_broken: bool,
    /// 同步到的模型清单（live 拉取失败时回退；models_pinned 时是唯一权威清单）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 清单已「固定」：true 时以 models 为准（/v1/models 只发布这份），不再跟随
    /// 上游 live 清单，更新条目后的自动同步也不覆盖。
    ///
    /// 为什么需要：有的聚合上游（例如本机 our-free-model 桥）清单里宣称上百个
    /// 模型，实测一半以上路由已死（404 / EOL / 挂起 60s 才报错）。跟随上游 =
    /// 面板替它宣传明知坏掉的模型。用户手动收敛一次清单后必须能保住成果，
    /// 否则下一次自动同步又冲回去了。
    #[serde(default)]
    pub models_pinned: bool,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

pub struct CustomStore {
    inner: Mutex<Vec<CustomApi>>,
    path: Option<PathBuf>,
}

/// Key 掩码：前 4 + … + 后 4。
///
/// **必须按字符切**：`&key[..4]` 是字节切片，Key 含非 ASCII（中文备注、
/// 误粘的 Cookie）时会落在字符中间 panic。短于 8 个字符一律全掩。
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() >= 8 {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    } else {
        "••••".to_string()
    }
}

/// 落盘串行化闸门（见 accounts.rs 的同一说明）：内存锁管内存，闸门管磁盘写序。
static SAVE_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_save_gate<R>(f: impl FnOnce() -> R) -> R {
    let _g = SAVE_GATE.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

impl CustomStore {
    /// 从 data 目录加载（不存在则空表；路径为 data/custom.json）。
    pub fn open(data_dir: &Path) -> CustomStore {
        let p = data_dir.join("custom.json");
        let rows: Vec<CustomApi> = match std::fs::read_to_string(&p) {
            // 解析失败必须留档再置空：否则下一次 add/update 落盘就会把用户配的
            // 自定义 API（含 Key）永久覆盖掉——这正是 accounts.rs 犯过的错。
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(rows) => rows,
                Err(e) => {
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let bad = data_dir.join(format!("custom.json.bad-{stamp}"));
                    let _ = std::fs::rename(&p, &bad);
                    tracing::error!(
                        "custom.json 解析失败（{e}），已留档到 {}，本次以空表启动",
                        bad.display()
                    );
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        // 落盘是 dpapi: 密文，内存要明文。解不开的（换机器/换用户）保留密文原文并标记，
        // 交给转发层跳过 —— 绝不能把密文当凭据发出去。
        let mut rows = rows;
        for r in rows.iter_mut() {
            if !crate::secret::is_wrapped(&r.api_key) {
                continue;
            }
            match crate::secret::unwrap_with(&r.api_key, SECRET_PURPOSE) {
                Ok(plain) => r.api_key = plain,
                Err(e) => {
                    r.key_broken = true;
                    tracing::error!(
                        "custom.json 里 {}（{}）的 API Key 解不开（{e}）；该条目已停用，重新填一次 Key 即可恢复",
                        r.id,
                        r.name
                    );
                }
            }
        }
        CustomStore {
            inner: Mutex::new(rows),
            path: Some(p),
        }
    }

    /// 空表（测试用）。
    pub fn empty() -> CustomStore {
        CustomStore {
            inner: Mutex::new(Vec::new()),
            path: None,
        }
    }

    /// 原子落盘（tmp + fsync + rename），Key 加密后再写。
    ///
    /// 内存里是明文、磁盘上是 `dpapi:`。解不开的条目（`key_broken`）**原样保留密文**：
    /// 既不二次加密（见 `secret::wrap_with` 的幂等）也不清空（否则用户只能靠自己记得重填）。
    /// 落盘：逐条 DPAPI 加密（每条一次系统调用）+ 写临时文件 + fsync + rename。
    ///
    /// ★ **调用方必须已经释放 `inner` 锁**：DPAPI + fsync 是毫秒到几十毫秒的阻塞操作，
    ///   持锁做会让面板、`all_specs()`、转发时解析 Key 这些读路径一起排队。

    /// 在写闸门内取快照并落盘（各写方法的统一出口）。
    ///
    /// ★ 闸门必须同时盖住「取快照」与「写盘」：只盖写盘时，A 取到旧快照后被抢占、
    ///   B 取新快照先写完，A 再把旧快照写回去 —— 磁盘留下旧数据（内存对、重启后回退）。
    fn persist_now(&self) {
        with_save_gate(|| {
            let snapshot = self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(p) = &self.path {
                let disk: Vec<CustomApi> = snapshot
                    .iter()
                    .map(|c| {
                        let mut d = c.clone();
                        if !d.key_broken {
                            d.api_key = crate::secret::wrap_with(&d.api_key, SECRET_PURPOSE);
                        }
                        d
                    })
                    .collect();
                if let Err(e) = crate::atomic::write_json_atomic(p, &disk) {
                    tracing::warn!("custom 落盘失败（{}）：{e}", p.display());
                }
            }
        });
    }

    /// 全部条目（前端展示；api_key 掩码处理由快照层完成）。
    pub fn rows(&self) -> Vec<CustomApi> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 新增自定义 API；返回新条目。
    pub fn add(
        &self,
        name: &str,
        format: &str,
        base_url: &str,
        api_key: &str,
    ) -> Result<CustomApi, String> {
        let name = name.trim();
        let base_url = base_url.trim().trim_end_matches('/');
        let api_key = api_key.trim();
        // ★ 长度上界：实测 100KB 的名字、5MB 的 api_key、3KB 的 base_url 都能 200 落盘。
        //   custom.json 每次读写都要过一遍这些字段，面板每刷一次也会把它们整份回吐。
        //   真实取值：名字几十字节、Key 几百字节、URL 至多数百字节。
        if name.chars().count() > MAX_CUSTOM_NAME_LEN {
            return Err(format!("名称太长（上限 {} 字符）", MAX_CUSTOM_NAME_LEN));
        }
        if base_url.chars().count() > MAX_CUSTOM_URL_LEN {
            return Err(format!("URL 太长（上限 {} 字符）", MAX_CUSTOM_URL_LEN));
        }
        if api_key.chars().count() > MAX_CUSTOM_KEY_LEN {
            return Err(format!("API Key 太长（上限 {} 字符）", MAX_CUSTOM_KEY_LEN));
        }
        if name.is_empty() {
            return Err("名称不能为空".into());
        }
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("URL 必须以 http(s):// 开头".into());
        }
        if api_key.is_empty() {
            return Err("API Key 不能为空".into());
        }
        if format != "openai" && format != "anthropic" {
            return Err("协议只支持 openai / anthropic".into());
        }
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // 纳秒时间戳在同一纳秒内批量新增会撞 id；加进程内自增序号兜底。
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = format!(
            "custom-{:x}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let c = CustomApi {
            id: id.clone(),
            name: name.to_string(),
            format: format.to_string(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            key_broken: false,
            models: Vec::new(),
            models_pinned: false,
            enabled: true,
        };
        rows.push(c.clone());
        drop(rows);
        self.persist_now();
        Ok(c)
    }

    /// 更新模型清单（前端「同步模型」写入）。
    pub fn set_models(&self, id: &str, models: Vec<String>) -> Result<(), String> {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(c) = rows.iter_mut().find(|c| c.id == id) else {
            return Err("自定义 API 不存在".into());
        };
        c.models = models;
        drop(rows);
        self.persist_now();
        Ok(())
    }

    /// 写入模型清单并设置「固定」标记。
    /// pinned=true：以这份清单为准（sync_models 显式给清单的路径）；
    /// pinned=false：恢复跟随上游（add/update 后的自动同步走这里）。
    pub fn set_models_pinned(
        &self,
        id: &str,
        models: Vec<String>,
        pinned: bool,
    ) -> Result<(), String> {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(c) = rows.iter_mut().find(|c| c.id == id) else {
            return Err("自定义 API 不存在".into());
        };
        // 上游 /models 可以回一份极大的清单（有的聚合站回几千条），落盘前截一下。
        let mut models = models;
        models.truncate(MAX_CUSTOM_MODELS);
        c.models = models;
        c.models_pinned = pinned;
        drop(rows);
        self.persist_now();
        Ok(())
    }

    /// 更新条目（名称/协议/URL/Key 可改）。
    pub fn update(
        &self,
        id: &str,
        name: &str,
        format: &str,
        base_url: &str,
        api_key: &str,
    ) -> Result<CustomApi, String> {
        // 与 add 同一道闸：只堵 add 不堵 update 等于留后门。
        if name.trim().chars().count() > MAX_CUSTOM_NAME_LEN {
            return Err(format!("名称太长（上限 {} 字符）", MAX_CUSTOM_NAME_LEN));
        }
        if base_url.trim().chars().count() > MAX_CUSTOM_URL_LEN {
            return Err(format!("URL 太长（上限 {} 字符）", MAX_CUSTOM_URL_LEN));
        }
        if api_key.trim().chars().count() > MAX_CUSTOM_KEY_LEN {
            return Err(format!("API Key 太长（上限 {} 字符）", MAX_CUSTOM_KEY_LEN));
        }
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(c) = rows.iter_mut().find(|c| c.id == id) else {
            return Err("自定义 API 不存在".into());
        };
        if !name.trim().is_empty() {
            c.name = name.trim().to_string();
        }
        if format == "openai" || format == "anthropic" {
            c.format = format.to_string();
        }
        // ★ 不能只 starts_with("http")：那会放行 "httpx://…" 这种非法 scheme，
        // 之后 reqwest 构造 URL 失败，用户只会看到 "builder error"。
        let bu = base_url.trim().trim_end_matches('/');
        if bu.starts_with("http://") || bu.starts_with("https://") {
            c.base_url = bu.to_string();
        }
        if !api_key.trim().is_empty() {
            c.api_key = api_key.trim().to_string();
            // 用户重新给了 Key：之前的「解不开」就不是问题了，清掉标记恢复转发。
            c.key_broken = false;
        }
        let clone = c.clone();
        drop(rows);
        self.persist_now();
        Ok(clone)
    }

    /// 删除；返回是否真的删了。
    /// 启用/停用一条自定义 API（停用后 all_specs 会把它置 enabled=false，模型从 /v1/models 消失）。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut hit = false;
        for r in rows.iter_mut() {
            if r.id == id {
                r.enabled = enabled;
                hit = true;
                break;
            }
        }
        if hit {
            drop(rows);
            self.persist_now();
        }
        hit
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = rows.len();
        rows.retain(|c| c.id != id);
        let changed = rows.len() != before;
        if changed {
            drop(rows);
            self.persist_now();
        }
        changed
    }

    /// 前端快照：api_key 掩码（仅显示末 4 位）。
    pub fn snapshot(&self) -> serde_json::Value {
        let rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|c| {
                let mask = if c.key_broken {
                    // 密文解不开时掩码没有意义：直接说清楚要重填，别显示一串像 Key 的东西。
                    "需重新填写".to_string()
                } else {
                    mask_key(&c.api_key)
                };
                serde_json::json!({
                    "id": c.id, "name": c.name, "format": c.format,
                    "base_url": c.base_url, "api_key_mask": mask,
                    "models": c.models, "enabled": c.enabled,
                    "models_pinned": c.models_pinned,
                    "key_broken": c.key_broken,
                })
            })
            .collect();
        serde_json::json!({"total": list.len(), "rows": list})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 字段没有上界时实测 100KB 名字 / 5MB Key / 3KB URL 都能 200 落盘。
    #[test]
    fn add_and_update_reject_overlong_fields() {
        let dir = tempfile::tempdir().unwrap();
        let cs = CustomStore::open(dir.path());
        let long_name = "n".repeat(MAX_CUSTOM_NAME_LEN + 1);
        let e = cs.add(&long_name, "openai", "https://x.example", "k").unwrap_err();
        assert!(e.contains("名称太长"), "{e}");
        let long_key = "k".repeat(MAX_CUSTOM_KEY_LEN + 1);
        let e = cs.add("ok", "openai", "https://x.example", &long_key).unwrap_err();
        assert!(e.contains("API Key 太长"), "{e}");
        let long_url = format!("https://x.example/{}", "p".repeat(MAX_CUSTOM_URL_LEN));
        let e = cs.add("ok", "openai", &long_url, "k").unwrap_err();
        assert!(e.contains("URL 太长"), "{e}");
        // update 走同一道闸（只堵 add 等于留后门）
        let c = cs.add("ok", "openai", "https://x.example", "k").unwrap();
        let e = cs.update(&c.id, &long_name, "", "https://x.example", "").unwrap_err();
        assert!(e.contains("名称太长"), "{e}");
        // 正常长度必须放行
        assert!(cs.add("正常名字", "openai", "https://x.example/v1", "sk-123").is_ok());
    }

    #[test]
    fn mask_key_never_splits_multibyte() {
        // 按字节切第 4 位会落在汉字中间 panic：Key 里出现中文时必须按字符切
        assert_eq!(mask_key("中文密钥中文密钥"), "中文密钥…中文密钥");
        assert_eq!(mask_key("sk-abcdef123456"), "sk-a…3456");
        assert_eq!(mask_key("短"), "••••");
        assert_eq!(mask_key(""), "••••");
        assert_eq!(mask_key("中文一二三四五六七八"), "中文一二…五六七八");
    }

    #[test]
    fn add_and_snapshot_masks_key() {
        let st = CustomStore::empty();
        let c = st.add("测试中转", "openai", "https://x.example/", "sk-abcdef123456").unwrap();
        assert!(c.id.starts_with("custom-"));
        let snap = st.snapshot();
        let rows = snap["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]["api_key_mask"].as_str().unwrap().ends_with("3456"));
        assert!(rows[0]["base_url"].as_str().unwrap().ends_with("x.example"));
    }

    #[test]
    fn reject_bad_input() {
        let st = CustomStore::empty();
        assert!(st.add("", "openai", "https://x", "k").is_err());
        assert!(st.add("n", "graphql", "https://x", "k").is_err());
        assert!(st.add("n", "openai", "ftp://x", "k").is_err());
        assert!(st.add("n", "openai", "https://x", "").is_err());
    }

    #[test]
    fn pinned_flag_persists_and_old_files_default_to_false() {
        // 固定标记必须落盘：不然重启一次，用户收敛好的白名单又被上游全量清单冲掉。
        let dir = tempfile::tempdir().unwrap();
        let cs = CustomStore::open(dir.path());
        let c = cs.add("桥", "openai", "http://127.0.0.1:9/v1", "sk-x").unwrap();
        cs.set_models_pinned(&c.id, vec!["m1".into()], true).unwrap();
        let rows = CustomStore::open(dir.path()).rows();
        assert!(rows[0].models_pinned, "固定标记要能重开读出");
        assert_eq!(rows[0].models, vec!["m1".to_string()]);
        // 老 custom.json（没有 models_pinned 字段）→ 默认不固定
        std::fs::write(
            dir.path().join("custom.json"),
            r#"[{"id":"c-old","name":"n","format":"openai","base_url":"http://x.example","api_key":"sk-plain","models":[],"enabled":true}]"#,
        )
        .unwrap();
        let old = CustomStore::open(dir.path()).rows();
        assert!(!old[0].models_pinned, "老数据缺字段必须默认不固定");
    }

    #[cfg(windows)]
    #[test]
    fn keys_are_encrypted_on_disk_and_plaintext_in_memory() {
        // 目标：custom.json 里不能出现明文 Key（以前 api_key 是直接明文落盘的）。
        let dir = tempfile::tempdir().unwrap();
        let st = CustomStore::open(dir.path());
        let c = st.add("中转", "openai", "https://x.example", "sk-plain-secret-123456").unwrap();

        let raw = std::fs::read_to_string(dir.path().join("custom.json")).unwrap();
        assert!(!raw.contains("sk-plain-secret-123456"), "磁盘上不能有明文 Key：{raw}");
        assert!(raw.contains(crate::secret::PREFIX), "应当是 dpapi: 密文：{raw}");

        // 重新装载：内存里必须恢复明文（否则转发会带着密文去 401）。
        let st2 = CustomStore::open(dir.path());
        let rows = st2.rows();
        let got = rows.iter().find(|r| r.id == c.id).unwrap();
        assert_eq!(got.api_key, "sk-plain-secret-123456");
        assert!(!got.key_broken, "能解开就不该标记坏 Key");
        assert!(st2.snapshot()["rows"][0]["api_key_mask"]
            .as_str()
            .unwrap()
            .ends_with("3456"));
    }

    #[test]
    fn undecryptable_key_disables_entry_but_keeps_the_blob() {
        // 换机器/换用户后：密文解不开。要 (1) 不把密文当 Key 用，(2) 不把密文弄丢或二次加密。
        let dir = tempfile::tempdir().unwrap();
        // "dpapi:AAAA" 是合法 base64 但不是我们的 blob（熵不匹配/别的机器）→ 必然解不开。
        let seed = serde_json::json!([{
            "id": "custom-dead-0", "name": "别处的中转", "format": "openai",
            "base_url": "https://x.example", "api_key": "dpapi:AAAA",
            "models": [], "enabled": true
        }]);
        std::fs::write(dir.path().join("custom.json"), seed.to_string()).unwrap();

        let st = CustomStore::open(dir.path());
        let rows = st.rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].key_broken, "解不开必须标记：{rows:?}");
        assert_eq!(rows[0].api_key, "dpapi:AAAA", "密文原样保留，不清空");
        assert_eq!(st.snapshot()["rows"][0]["api_key_mask"], "需重新填写");

        // 触发一次不相干的落盘（同步模型）：坏 Key 必须原样写回，不能变成密文的密文。
        st.set_models("custom-dead-0", vec!["m1".into()]).unwrap();
        let raw = std::fs::read_to_string(dir.path().join("custom.json")).unwrap();
        assert!(raw.contains("dpapi:AAAA"), "坏 Key 要原样留着：{raw}");
        assert!(!raw.contains("dpapi:ZGFwaTpB"), "不能二次加密：{raw}");

        // 重新填一次 Key：标记清掉，可以恢复转发。
        st.update("custom-dead-0", "", "openai", "", "sk-new-key-987654321").unwrap();
        let rows = st.rows();
        assert!(!rows[0].key_broken);
        assert_eq!(rows[0].api_key, "sk-new-key-987654321");
    }

    #[test]
    fn update_and_remove() {
        let st = CustomStore::empty();
        let c = st.add("n", "openai", "https://a.example", "k1").unwrap();
        st.update(&c.id, "改名", "anthropic", "https://b.example", "k2").unwrap();
        st.set_models(&c.id, vec!["m1".into(), "m2".into()]).unwrap();
        let rows = st.rows();
        let c2 = rows.iter().find(|x| x.id == c.id).unwrap();
        assert_eq!(c2.name, "改名");
        assert_eq!(c2.format, "anthropic");
        assert_eq!(c2.models.len(), 2);
        assert!(st.remove(&c.id));
        assert!(!st.remove(&c.id));
    }
}
