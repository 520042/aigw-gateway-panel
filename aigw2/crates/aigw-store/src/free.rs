//! 公益站聚合存储（v2 接入源页「公益站」）。
//!
//! 从聚合导航站（bubblevv.github.io/ai-api-gongyi-nav 等）手动/每日刷新获取
//! 免费中转站列表，持久化到 `data/free_sites.json`。
//! 合并规则：按 id 去重——已存在的站点保留（含 enabled 状态，账号不丢），
//! 新站点追加；刷新不会删掉已添加的站点。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 一个公益中转站条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeSite {
    /// 唯一 id（free-{host-slug}）。
    pub id: String,
    /// 站点名（聚合站给的名称）。
    pub name: String,
    /// 上游根地址（如 https://api.aisz.mom，spec 转换时拼 /v1）。
    pub base_url: String,
    /// 注册/访问链接（聚合站原样，含 aff 参数）。
    #[serde(default)]
    pub url: String,
    /// 简介（聚合站标签/描述，选填）。
    #[serde(default)]
    pub desc: String,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

pub struct FreeStore {
    inner: Mutex<Vec<FreeSite>>,
    path: Option<PathBuf>,
}

impl FreeStore {
    /// 从 data 目录加载（不存在则空表；路径为 data/free_sites.json）。
    pub fn open(data_dir: &Path) -> FreeStore {
        let p = data_dir.join("free_sites.json");
        let rows: Vec<FreeSite> = match std::fs::read_to_string(&p) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        FreeStore {
            inner: Mutex::new(rows),
            path: Some(p),
        }
    }

    /// 空表（测试用）。
    pub fn empty() -> FreeStore {
        FreeStore {
            inner: Mutex::new(Vec::new()),
            path: None,
        }
    }

    fn save_locked(&self, rows: &[FreeSite]) {
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

    /// 全部条目。
    pub fn rows(&self) -> Vec<FreeSite> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 合并一批新站点（聚合刷新入口）：
    /// - 已存在（同 id）：保留原条目（enabled/账号状态不动），仅刷新 url/desc/name；
    /// - 新增：追加。
    /// 返回 (新增数, 保留数)。
    pub fn upsert_batch(&self, sites: Vec<FreeSite>) -> (usize, usize) {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut added = 0usize;
        let mut kept = 0usize;
        for s in sites {
            if s.id.is_empty() || s.base_url.is_empty() {
                continue;
            }
            if let Some(existing) = rows.iter_mut().find(|r| r.id == s.id) {
                // 保留原条目，仅刷新外部信息
                existing.name = s.name;
                existing.base_url = s.base_url;
                if !s.url.is_empty() {
                    existing.url = s.url;
                }
                if !s.desc.is_empty() {
                    existing.desc = s.desc;
                }
                kept += 1;
            } else {
                rows.push(s);
                added += 1;
            }
        }
        self.save_locked(&rows);
        (added, kept)
    }

    /// 删除站点；返回是否真的删了。
    pub fn remove(&self, id: &str) -> bool {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = rows.len();
        rows.retain(|r| r.id != id);
        let changed = rows.len() != before;
        if changed {
            self.save_locked(&rows);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_keeps_existing() {
        let st = FreeStore::empty();
        let batch = vec![
            FreeSite {
                id: "free-aaa".into(),
                name: "A站".into(),
                base_url: "https://aaa.example".into(),
                url: "https://aaa.example/sign-up".into(),
                desc: "首次".into(),
                enabled: true,
            },
            FreeSite {
                id: "free-bbb".into(),
                name: "B站".into(),
                base_url: "https://bbb.example".into(),
                url: "".into(),
                desc: "".into(),
                enabled: true,
            },
        ];
        let (a1, k1) = st.upsert_batch(batch.clone());
        assert_eq!((a1, k1), (2, 0));
        // 二刷：A 已存在，只新增 C
        let batch2 = vec![
            FreeSite {
                id: "free-aaa".into(),
                name: "A站改名".into(),
                base_url: "https://aaa.example".into(),
                url: "https://aaa.example/sign-up?aff=x".into(),
                desc: "更新".into(),
                enabled: true,
            },
            FreeSite {
                id: "free-ccc".into(),
                name: "C站".into(),
                base_url: "https://ccc.example".into(),
                url: "".into(),
                desc: "".into(),
                enabled: true,
            },
        ];
        let (a2, k2) = st.upsert_batch(batch2);
        assert_eq!((a2, k2), (1, 1));
        let rows = st.rows();
        assert_eq!(rows.len(), 3);
        let a = rows.iter().find(|r| r.id == "free-aaa").unwrap();
        assert_eq!(a.name, "A站改名");
        assert_eq!(a.desc, "更新");
        assert!(st.remove("free-aaa"));
        assert_eq!(st.rows().len(), 2);
    }
}
