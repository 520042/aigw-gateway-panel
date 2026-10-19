//! 账号池存储：只读导入旧版 `accounts.json`（顶层 list，secret 明文）。
//!
//! 铁律对齐旧项目：`list_accounts()` 展示层脱敏，**后台/中继必须用明文 secret**
//! ——本模块即明文源；[`AccountStore::snapshot`] 面向前端，不含 secret。

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub status: String,
}

pub struct AccountStore {
    inner: Mutex<Vec<Account>>,
    /// v2 权威存储路径（登录新增凭据持久化到这里）
    path: Option<PathBuf>,
}

impl AccountStore {
    /// 双源装载：v2 own 文件存在则以其为权威；否则从 legacy 只读导入并**落 own**
    /// （之后 own 为权威，legacy 只读不动）。
    pub fn load_dual(legacy: &Path, own: &Path) -> AccountStore {
        let rows: Vec<Account> = if own.exists() {
            std::fs::read_to_string(own)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default()
        } else {
            let from_legacy: Vec<Account> = std::fs::read_to_string(legacy)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            // 写 own（初次权威化）
            if !from_legacy.is_empty() {
                if let Ok(mut f) = std::fs::File::create(own) {
                    use std::io::Write;
                    let _ = f.write_all(
                        serde_json::to_string_pretty(&from_legacy)
                            .unwrap_or_default()
                            .as_bytes(),
                    );
                }
            }
            from_legacy
        };
        tracing::info!(
            "accounts：装载 {} 个账号（own={}，legacy 只读={})",
            rows.len(),
            own.display(),
            legacy.display()
        );
        AccountStore {
            inner: Mutex::new(rows),
            path: Some(own.to_path_buf()),
        }
    }

    /// 测试用：无持久化
    pub fn empty() -> AccountStore {
        AccountStore {
            inner: Mutex::new(vec![]),
            path: None,
        }
    }

    /// 直接以行构造（测试/程序化注入）。
    pub fn from_rows(rows: Vec<Account>) -> AccountStore {
        AccountStore {
            inner: Mutex::new(rows),
            path: None,
        }
    }

    fn save_locked(&self, accs: &[Account]) {
        if let Some(p) = &self.path {
            if let Ok(mut f) = std::fs::File::create(p) {
                use std::io::Write;
                let _ = f.write_all(
                    serde_json::to_string_pretty(accs)
                        .unwrap_or_default()
                        .as_bytes(),
                );
            }
        }
    }

    /// 新增/更新账号（同 platform+name 覆盖，防重复落池）并持久化。
    pub fn add_account(&self, platform: &str, name: &str, kind: &str, secret: &str) -> Account {
        let mut accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = accs
            .iter_mut()
            .find(|a| a.platform == platform && a.name == name)
        {
            existing.secret = secret.to_string();
            existing.enabled = true;
            let clone = existing.clone();
            self.save_locked(&accs);
            return clone;
        }
        let a = Account {
            id: format!("{:x}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)),
            platform: platform.to_string(),
            name: name.to_string(),
            secret: secret.to_string(),
            kind: kind.to_string(),
            enabled: true,
            status: "active".into(),
        };
        accs.push(a.clone());
        self.save_locked(&accs);
        a
    }

    /// 平台的启用账号 `(id, secret)` 列表（明文，仅供中继）。
    pub fn secrets_for(&self, platform: &str) -> Vec<(String, String)> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|a| a.platform == platform && a.enabled)
            .map(|a| (a.id.clone(), a.secret.clone()))
            .collect()
    }

    /// 前端账号明细：**不含 secret**（掩码原则），只给管理字段。
    pub fn rows_snapshot(&self) -> serde_json::Value {
        let accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let rows: Vec<serde_json::Value> = accs
            .iter()
            .map(|a| {
                json!({
                    "id": a.id, "platform": a.platform, "name": a.name,
                    "kind": a.kind, "enabled": a.enabled, "status": a.status,
                    "secret_len": a.secret.len(),
                })
            })
            .collect();
        json!({ "total": accs.len(), "rows": rows })
    }

    /// 前端快照：**不含 secret**（掩码原则）。
    pub fn snapshot(&self) -> serde_json::Value {
        let accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut by_platform: std::collections::BTreeMap<String, u64> = Default::default();
        for a in accs.iter().filter(|a| a.enabled) {
            *by_platform.entry(a.platform.clone()).or_insert(0) += 1;
        }
        json!({
            "total": accs.len(),
            "enabled_by_platform": by_platform,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_secrets_for() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("accounts.json");
        std::fs::write(
            &p,
            r#"[
                {"id":"a1","platform":"web-glm","name":"GLM 账号","secret":"rt-123","type":"cookie","enabled":true},
                {"id":"a2","platform":"web-glm","name":"禁用","secret":"rt-456","type":"cookie","enabled":false},
                {"id":"a3","platform":"apk-doubao","name":"豆包","secret":"ck-789","type":"cookie","enabled":true}
            ]"#,
        )
        .unwrap();
        let st = AccountStore::load_dual(&p, &tmp.path().join("own-accounts.json"));
        let glm = st.secrets_for("web-glm");
        assert_eq!(glm.len(), 1, "禁用账号不入池");
        assert_eq!(glm[0], ("a1".into(), "rt-123".into()));
        assert_eq!(st.secrets_for("nonexistent").len(), 0);
        // 快照不含 secret
        let snap = st.snapshot().to_string();
        assert!(!snap.contains("rt-123"), "snapshot 不得泄漏 secret");
        assert!(snap.contains("web-glm"));
    }

    #[test]
    fn missing_file_is_empty_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let st = AccountStore::load_dual(
            Path::new("Z:/no/such/legacy.json"),
            &tmp.path().join("own-accounts.json"),
        );
        assert_eq!(st.secrets_for("web-glm").len(), 0);
        assert_eq!(st.snapshot()["total"], 0);
    }
}
