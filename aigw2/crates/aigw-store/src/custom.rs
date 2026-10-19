//! 自定义 API 存储（v2 接入源页「自定义 API」）。
//!
//! 参考 CherryStudio 的自定义供应商思路：用户自填 名称/协议/URL/Key，
//! 面板把每条自定义 API 动态并进 specs，走统一 OpenAI/Anthropic 透传。
//! 持久化到 `data/custom.json`（与 accounts.json 并列）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    /// 用户自己的 API Key（明文，仅后台使用）。
    pub api_key: String,
    /// 同步到的模型清单（live 拉取失败时回退）。
    #[serde(default)]
    pub models: Vec<String>,
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

impl CustomStore {
    /// 从 data 目录加载（不存在则空表；路径为 data/custom.json）。
    pub fn open(data_dir: &Path) -> CustomStore {
        let p = data_dir.join("custom.json");
        let rows: Vec<CustomApi> = match std::fs::read_to_string(&p) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
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

    fn save_locked(&self, rows: &[CustomApi]) {
        if let Some(p) = &self.path {
            if let Ok(mut f) = std::fs::File::create(p) {
                use std::io::Write;
                let _ = f.write_all(
                    serde_json::to_string_pretty(rows)
                        .unwrap_or_default()
                        .as_bytes(),
                );
            }
        }
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
        let id = format!(
            "custom-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let c = CustomApi {
            id: id.clone(),
            name: name.to_string(),
            format: format.to_string(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            models: Vec::new(),
            enabled: true,
        };
        rows.push(c.clone());
        self.save_locked(&rows);
        Ok(c)
    }

    /// 更新模型清单（前端「同步模型」写入）。
    pub fn set_models(&self, id: &str, models: Vec<String>) -> Result<(), String> {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(c) = rows.iter_mut().find(|c| c.id == id) else {
            return Err("自定义 API 不存在".into());
        };
        c.models = models;
        self.save_locked(&rows);
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
        if base_url.trim().starts_with("http") {
            c.base_url = base_url.trim().trim_end_matches('/').to_string();
        }
        if !api_key.trim().is_empty() {
            c.api_key = api_key.trim().to_string();
        }
        let clone = c.clone();
        self.save_locked(&rows);
        Ok(clone)
    }

    /// 删除；返回是否真的删了。
    pub fn remove(&self, id: &str) -> bool {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = rows.len();
        rows.retain(|c| c.id != id);
        let changed = rows.len() != before;
        if changed {
            self.save_locked(&rows);
        }
        changed
    }

    /// 前端快照：api_key 掩码（仅显示末 4 位）。
    pub fn snapshot(&self) -> serde_json::Value {
        let rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|c| {
                let mask = if c.api_key.len() >= 8 {
                    format!("{}…{}", &c.api_key[..4], &c.api_key[c.api_key.len() - 4..])
                } else {
                    "••••".to_string()
                };
                serde_json::json!({
                    "id": c.id, "name": c.name, "format": c.format,
                    "base_url": c.base_url, "api_key_mask": mask,
                    "models": c.models, "enabled": c.enabled,
                })
            })
            .collect();
        serde_json::json!({"total": list.len(), "rows": list})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
