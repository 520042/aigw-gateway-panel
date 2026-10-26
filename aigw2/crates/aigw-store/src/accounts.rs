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

/// 落盘视图：secret 换成 `dpapi:` 密文（DPAPI 不可用时 fail-open 退回明文），
/// 并清掉只在内存里成立的 `unreadable` 标记。
///
/// 单独一个函数是因为写入口不止 `save_locked` 一处（`load_dual` 初次权威化
/// 也直接调 `write_json_atomic`）——漏掉任何一处都会让明文重新落到磁盘上。
fn wrapped(accs: &[Account]) -> Vec<Account> {
    accs.iter()
        .map(|a| {
            let mut c = a.clone();
            // unreadable 的账号 secret 里是解不开的**密文**，原样写回，别二次加密。
            c.secret = if a.unreadable {
                a.secret.clone()
            } else {
                crate::secret::wrap(&a.secret)
            };
            c.unreadable = false;
            c
        })
        .collect()
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
    /// 上游在登录时告诉我们的归属域（WorkBuddy / CodeBuddy 族用）。
    ///
    /// 同一个 token 端点在 cn 与 intl 两套域上都存在，上游按 `X-Domain` 判定请求来自
    /// 哪个产品线。留空表示这个平台不区分域（绝大多数平台都是这样）。
    #[serde(default)]
    pub domain: String,
    /// 磁盘上是 `dpapi:` 密文但**本机解不开**（换了 Windows 用户 / 换了机器 / 文件被改）。
    ///
    /// 这不是持久状态，所以 `#[serde(skip)]`：每次装载都由
    /// `secret::unwrap` 现算。置位时 `secret` 里留的是**原始密文**（不要覆盖掉，
    /// 用户把文件搬回原机器还能救），并且`secrets_for`/`secret_of` 跳过它 ——
    /// 绝不把密文当凭据发给上游（那只会变成一堆看不懂的上游 401）。
    #[serde(default, skip)]
    pub unreadable: bool,
}

/// 单条凭据的长度上限（64KB）。
///
/// 真实 Cookie / Token 序列化后最长也就几十 KB；没有闸门时实测能贴进 5MB 并落盘，
/// accounts.json 从 17KB 涨到 7MB。server 的 /api/auth 已回 400，这里再守一道 ——
/// 恢复备份与凭据刷新链路同样会写 secret。
pub const MAX_CREDENTIAL_LEN: usize = 64 * 1024;

pub struct AccountStore {
    inner: Mutex<Vec<Account>>,
    /// v2 权威存储路径（登录新增凭据持久化到这里）
    path: Option<PathBuf>,
}

impl AccountStore {
    /// 双源装载：v2 own 文件存在则以其为权威；否则从 legacy 只读导入并**落 own**
    /// （之后 own 为权威，legacy 只读不动）。
    pub fn load_dual(legacy: &Path, own: &Path) -> AccountStore {
        // 解析 + 解密。加密是可为空的加固层：历史明文（旧版本写的 / legacy 导入的）
        // 原样通过，所以这里不需要「是否已加密」的版本开关。
        let read_rows = |p: &Path| -> Option<Vec<Account>> {
            let mut rows: Vec<Account> = std::fs::read_to_string(p)
                .ok()
                .and_then(|raw| serde_json::from_str::<Vec<Account>>(&raw).ok())?;
            let mut broken = 0usize;
            for a in rows.iter_mut() {
                match crate::secret::unwrap(&a.secret) {
                    Ok(plain) => a.secret = plain,
                    Err(why) => {
                        // 保留密文原样（别覆盖！），只打标记：既让人知道要重新登录，
                        // 又留下「把文件搬回原机器还能解」的余地。
                        a.unreadable = true;
                        broken += 1;
                        tracing::error!(
                            "账号 {}/{}（{}）的凭据解不开：{why}",
                            a.platform, a.name, a.id
                        );
                    }
                }
            }
            if broken > 0 {
                tracing::warn!("accounts：{broken} 个账号的凭据解不开，已标记「需重新登录」");
            }
            Some(rows)
        };
        let rows: Vec<Account> = if own.exists() {
            match read_rows(own) {
                Some(v) => v,
                None => {
                    // ★ 旧实现 `unwrap_or_default()` 直接空表启动：任何后续 add/update/remove 的
                    //   save_locked 都会用内存里的少量条目**覆盖**原文件 → 全部凭据永久丢失。
                    //   与 flags.rs 对齐：先把坏文件留档，再尝试回退 legacy。
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let bad = own.with_file_name(format!("accounts.json.bad-{ts}"));
                    let _ = std::fs::rename(own, &bad);
                    let fallback = read_rows(legacy).unwrap_or_default();
                    tracing::error!(
                        "accounts.json 解析失败，已留档为 {} 并回退 legacy（{} 个账号）；请检查该文件",
                        bad.display(),
                        fallback.len()
                    );
                    fallback
                }
            }
        } else {
            let from_legacy: Vec<Account> = read_rows(legacy).unwrap_or_default();
            // 写 own（初次权威化；原子写，避免半截文件）
            if !from_legacy.is_empty() {
                if let Err(e) = crate::atomic::write_json_atomic(own, &wrapped(&from_legacy)) {
                    tracing::warn!("accounts 初次权威化落盘失败（{}）：{e}", own.display());
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
        let store = AccountStore {
            inner: Mutex::new(rows),
            path: Some(own.to_path_buf()),
        };
        // ★ 升级即加固：历史版本把明文写在磁盘上，如果只在「下次登录/改账号」时才加密，
        //   那份明文文件在第一次保存之前一直躺在那里 —— 而这正是要防的东西。
        //   所以装载完立刻把明文换成密文（只在确实有明文时才写盘，DPAPI 不可用的机器
        //   不会每次启动都重写一遍文件）。
        let plain: Vec<String> = {
            let accs = store.inner.lock().unwrap_or_else(|e| e.into_inner());
            accs.iter()
                .filter(|a| !a.unreadable && !a.secret.is_empty() && !crate::secret::is_wrapped(&a.secret))
                .map(|a| a.secret.clone())
                .collect()
        };
        if !plain.is_empty() {
            let encrypted = crate::secret::wrap(&plain[0]) != plain[0];
            let accs = store.inner.lock().unwrap_or_else(|e| e.into_inner());
            store.save_locked(&accs);
            if encrypted {
                tracing::info!("accounts：启动时把 {} 个明文凭据换成 DPAPI 密文落盘", plain.len());
            } else {
                tracing::warn!(
                    "accounts：{} 个凭据仍是明文（本机 DPAPI 不可用，fail-open 保持可用）",
                    plain.len()
                );
            }
        }
        store
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

    /// 原子落盘（tmp + fsync + rename）：进程中途崩溃不会留下半截 accounts.json。
    fn save_locked(&self, accs: &[Account]) {
        if let Some(p) = &self.path {
            if let Err(e) = crate::atomic::write_json_atomic(p, &wrapped(accs)) {
                tracing::warn!("accounts 落盘失败（{}）：{e}", p.display());
            }
        }
    }

    /// 新增/更新账号（同 platform+name 覆盖，防重复落池）并持久化。
    ///
    /// 返回 `(Account, existed)`：`existed` 为 true 表示这次是**覆盖已有行**
    /// （同一个 platform+name 又登录了一次 / 重新粘贴了凭据）。调用方据此决定是否要
    /// 清掉该账号在号池里的治理状态与会话粘性绑定 —— 换过凭据却还留着上一次失败
    /// 留下的冷却、鉴权失败隔离与粘性路由，会让「重新登录后仍然 401」看起来像登录没生效。
    pub fn add_account(&self, platform: &str, name: &str, kind: &str, secret: &str) -> (Account, bool) {
        self.add_account_with_domain(platform, name, kind, secret, "")
    }

    /// 带归属域的 add_account：WorkBuddy 族登录后必须把上游给的 domain 一起存下来，
    /// 否则转发时拼不出 `X-Domain`（国内账号发到国际域会被上游当成跨域请求拒掉）。
    pub fn add_account_with_domain(
        &self,
        platform: &str,
        name: &str,
        kind: &str,
        secret: &str,
        domain: &str,
    ) -> (Account, bool) {
        if secret.len() > MAX_CREDENTIAL_LEN {
            // 超限不落盘也不覆盖已有凭据：截断会造出一个「看起来存了、实际用不了」的账号，
            // 比直接拒绝更难排查。返回的 Account 只用于告知调用方，不会进入账号表。
            tracing::warn!(
                "凭据超长被拒收（{} 字节，上限 {}）：platform={platform}",
                secret.len(),
                MAX_CREDENTIAL_LEN
            );
            return (
                Account {
                    id: String::new(),
                    platform: platform.to_string(),
                    name: name.to_string(),
                    secret: String::new(),
                    kind: kind.to_string(),
                    domain: domain.to_string(),
                    enabled: false,
                    status: "rejected".into(),
                    unreadable: false,
                },
                false,
            );
        }
        let mut accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = accs
            .iter_mut()
            .find(|a| a.platform == platform && a.name == name)
        {
            existing.secret = secret.to_string();
            if !domain.trim().is_empty() {
                existing.domain = domain.trim().to_string();
            }
            existing.enabled = true;
            // 重新登录/重新粘贴凭据 = 换了一把能用的钥匙，之前「解不开」的标签必须摘掉。
            existing.unreadable = false;
            let clone = existing.clone();
            self.save_locked(&accs);
            return (clone, true);
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
            domain: domain.trim().to_string(),
            enabled: true,
            status: "active".into(),
            unreadable: false,
        };
        accs.push(a.clone());
        self.save_locked(&accs);
        (a, false)
    }

    /// 按账号 id 更新 secret（Cline WorkOS 刷新后写回）并持久化。
    ///
    /// 返回 `(hit, changed)`：`hit` 为 false 表示 id 不存在；`changed` 为 true 表示
    /// secret 真的变了。换过凭据的账号不该继续背着上一次失败留下的冷却 / 鉴权隔离，
    /// 调用方据此决定要不要 `pool.forget_account`。
    pub fn update_secret(&self, id: &str, secret: &str) -> (bool, bool) {
        let mut accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = accs.iter_mut().find(|a| a.id == id) {
            let changed = existing.secret != secret;
            existing.secret = secret.to_string();
            existing.status = "active".into();
            // 刷新链路拿到的是**可用的**新凭据，解不开的标记一并清掉。
            existing.unreadable = false;
            self.save_locked(&accs);
            (true, changed)
        } else {
            (false, false)
        }
    }

    /// 删除账号；返回是否命中。调用方应同时 `pool.forget_account` 清治理状态。
    pub fn remove(&self, id: &str) -> bool {
        let mut accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = accs.len();
        accs.retain(|a| a.id != id);
        let changed = accs.len() != before;
        if changed {
            self.save_locked(&accs);
        }
        changed
    }

    /// 改账号名（给账号起个看得懂的名字：这台机器 / 这个人 / 这条线路）。
    ///
    /// 返回 `(hit, err, changed)`。名字按 (platform, name) 唯一
    /// （`add_account_with_domain` 会**覆盖同平台同名行**），所以改名撞上同平台已有行时
    /// 必须拒绝而不是合并 —— 合并会让下一次登录静默覆盖掉另一份凭据。
    pub fn rename(&self, id: &str, name: &str) -> (bool, String, bool) {
        let name = name.trim();
        if name.is_empty() {
            return (false, "名字不能为空".into(), false);
        }
        let mut accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(cur) = accs.iter().find(|a| a.id == id) else {
            return (false, "账号不存在".into(), false);
        };
        if cur.name == name {
            return (true, String::new(), false);
        }
        let plat = cur.platform.clone();
        if accs
            .iter()
            .any(|a| a.id != id && a.platform == plat && a.name == name)
        {
            return (
                true,
                format!(
                    "同一个平台下已经有叫「{name}」的账号了，换个名字（重名会让下一次登录覆盖掉那一份凭据）"
                ),
                false,
            );
        }
        if let Some(a) = accs.iter_mut().find(|a| a.id == id) {
            a.name = name.to_string();
            self.save_locked(&accs);
            return (true, String::new(), true);
        }
        (false, "账号不存在".into(), false)
    }

    /// 启用/停用账号（停用后 `secrets_for` 不再返回它）；返回是否命中。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        let mut accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = accs.iter_mut().find(|a| a.id == id) {
            a.enabled = enabled;
            a.status = if enabled { "active".into() } else { "disabled".into() };
            self.save_locked(&accs);
            true
        } else {
            false
        }
    }

    /// 平台名 → 启用账号 id 列表（号池 forget 用）。
    pub fn ids_for(&self, platform: &str) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|a| a.platform == platform)
            .map(|a| a.id.clone())
            .collect()
    }

    /// 平台的启用账号 `(id, secret)` 列表（明文，仅供中继）。
    pub fn secrets_for(&self, platform: &str) -> Vec<(String, String)> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            // unreadable 的账号 secret 是密文，绝不能进中继。
            .filter(|a| a.platform == platform && a.enabled && !a.unreadable)
            .map(|a| (a.id.clone(), a.secret.clone()))
            .collect()
    }

    /// 按账号 id 取 (platform, secret)：面板「立即检查」需要拿平台 spec 去验活。
    /// 与 secrets_for 一样返回明文，调用方只允许服务端内部使用。
    pub fn secret_of(&self, id: &str) -> Option<(String, String)> {
        let accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        accs.iter()
            .find(|a| a.id == id && !a.unreadable)
            .map(|a| (a.platform.clone(), a.secret.clone()))
    }

    /// 账号的归属域（WorkBuddy / CodeBuddy 族用：转发时要带 `X-Domain`）。
    ///
    /// 空串表示「这个平台不区分域」，绝大多数平台都是这样。
    pub fn domain_of(&self, platform: &str, secret: &str) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|a| a.platform == platform && a.secret == secret)
            .map(|a| a.domain.clone())
            .unwrap_or_default()
    }
    /// 这个账号的凭据是不是「本机解不开」（换过 Windows 用户/机器）。
    ///
    /// 面板「立即检查」用它把 404「账号不存在」和「存在但解不开」区分开——
    /// 后者要的是重新登录，不是去别处找账号。
    pub fn needs_relogin(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|a| a.id == id && a.unreadable)
    }

    /// 平台 + secret → 账号 id：`/api/auth save` 落库后要立刻标记健康状态。
    pub fn id_of(&self, platform: &str, secret: &str) -> Option<String> {
        let accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        accs.iter()
            .rev()
            .find(|a| a.platform == platform && a.secret == secret)
            .map(|a| a.id.clone())
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
                    // secret_len 对 unreadable 的账号没有信息量（那是 base64 密文长度），给 0
                    "secret_len": if a.unreadable { 0 } else { a.secret.len() },
                    "needs_relogin": a.unreadable,
                })
            })
            .collect();
        let needs_relogin = accs.iter().filter(|a| a.unreadable).count();
        json!({ "total": accs.len(), "rows": rows, "needs_relogin": needs_relogin })
    }

    /// 前端快照：**不含 secret**（掩码原则）。
    pub fn snapshot(&self) -> serde_json::Value {
        let accs = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut by_platform: std::collections::BTreeMap<String, u64> = Default::default();
        // unreadable 的账号虽然 enabled=true，但取不到凭据 = 事实上不可用，
        // 算进「可用账号数」会让人以为这个平台还有号能用。
        for a in accs.iter().filter(|a| a.enabled && !a.unreadable) {
            *by_platform.entry(a.platform.clone()).or_insert(0) += 1;
        }
        let needs_relogin = accs.iter().filter(|a| a.unreadable).count();
        json!({
            "total": accs.len(),
            "enabled_by_platform": by_platform,
            "needs_relogin": needs_relogin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_rejects_blank_and_duplicate_names() {
        let tmp = tempfile::tempdir().unwrap();
        let st = AccountStore::load_dual(&tmp.path().join("legacy.json"), &tmp.path().join("accounts.json"));
        let (a, _) = st.add_account("plat-x", "老名字", "token", "s1");
        let (b, _) = st.add_account("plat-x", "另一个号", "token", "s2");
        // 空 / 全空白：拒绝（面板上会出现无名行，只能靠 id 认人）
        assert_eq!(st.rename(&a.id, "   "), (false, "名字不能为空".into(), false));
        // 同平台重名：拒绝而不是合并 —— add_account 对 (platform, name) 是覆盖语义，
        // 合并会让下一次登录静默顶掉另一份凭据。
        let (hit, err, changed) = st.rename(&b.id, "老名字");
        assert!(hit);
        assert!(changed == false);
        assert!(err.contains("已经有叫"), "{err}");
        // 正常改名：落盘且能读回
        let (hit, err, changed) = st.rename(&a.id, "测试号 A");
        assert_eq!((hit, err.is_empty(), changed), (true, true, true));
        let loaded = AccountStore::load_dual(&tmp.path().join("legacy.json"), &tmp.path().join("accounts.json"));
        let names: Vec<String> = loaded.rows_snapshot()["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(names.contains(&"测试号 A".to_string()), "{names:?}");
        // 改成同一个名字：不算失败，但也不算改动
        assert_eq!(st.rename(&a.id, "测试号 A"), (true, String::new(), false));
        // 不存在的 id
        assert_eq!(st.rename("nope", "x").0, false);
    }

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

    #[test]
    fn secret_of_and_id_of_roundtrip() {
        let st = AccountStore::empty();
        let (acc, _) = st.add_account("plat", "nm", "cookie", "s3cret");
        assert_eq!(st.secret_of(&acc.id), Some(("plat".to_string(), "s3cret".to_string())));
        assert_eq!(st.id_of("plat", "s3cret"), Some(acc.id.clone()));
        assert_eq!(st.id_of("plat", "other"), None);
        assert_eq!(st.secret_of("nope"), None);
    }

    /// ★ 加固：落盘必须是 DPAPI 密文，装载后内存里必须是明文。
    /// 这条同时守住两个方向的回归：①某天有人把 `wrapped()` 从写路径里摘掉 →
    /// 文件里又出现明文（断言失败）；②某天有人忘了在装载时 `unwrap` →
    /// 中继拿到 `dpapi:...` 去发上游（secrets_for 断言失败）。
    #[test]
    fn secrets_are_encrypted_on_disk_and_plain_in_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let own = tmp.path().join("accounts.json");
        let legacy = tmp.path().join("legacy.json");
        let st = AccountStore::load_dual(&legacy, &own);
        let (acc, existed) = st.add_account("web-glm", "GLM 账号", "cookie", "rt-super-secret-123");
        assert!(!existed);
        let on_disk = std::fs::read_to_string(&own).unwrap();
        assert!(
            !on_disk.contains("rt-super-secret-123"),
            "落盘文件里不能出现明文：{on_disk}"
        );
        #[cfg(windows)]
        assert!(on_disk.contains("dpapi:"), "Windows 上应写成 dpapi: 密文：{on_disk}");

        // 重新装载（模拟重启）：仍能拿到明文，且账号状态正常
        let st2 = AccountStore::load_dual(&legacy, &own);
        assert_eq!(
            st2.secrets_for("web-glm"),
            vec![(acc.id.clone(), "rt-super-secret-123".to_string())]
        );
        assert_eq!(st2.rows_snapshot()["needs_relogin"], 0);
        assert_eq!(st2.rows_snapshot()["rows"][0]["secret_len"], 19);
        // 再存一次不能把明文写回去（wrapped 是幂等的：内存里始终是明文）
        st2.set_enabled(&acc.id, true);
        let again = std::fs::read_to_string(&own).unwrap();
        assert!(!again.contains("rt-super-secret-123"), "二次落盘又漏了明文：{again}");
    }

    /// ★ 解不开的密文（换机器 / 换 Windows 用户 / 文件被改）必须变成「需重新登录」，
    /// 而不是被当成凭据发上游 —— 也不能把密文覆盖掉（搬回原机器还能救）。
    #[test]
    fn undecryptable_secret_is_flagged_and_never_used() {
        let tmp = tempfile::tempdir().unwrap();
        let own = tmp.path().join("accounts.json");
        let legacy = tmp.path().join("legacy.json");
        std::fs::write(
            &own,
            r#"[{"id":"x1","platform":"web-glm","name":"外机账号","secret":"dpapi:AAAA","type":"cookie","enabled":true}]"#,
        )
        .unwrap();
        let st = AccountStore::load_dual(&legacy, &own);
        assert_eq!(st.secrets_for("web-glm").len(), 0, "密文绝不能进中继");
        assert_eq!(st.secret_of("x1"), None);
        let snap = st.rows_snapshot();
        assert_eq!(snap["rows"][0]["needs_relogin"], true, "{snap}");
        assert_eq!(snap["needs_relogin"], 1);
        assert_eq!(snap["total"], 1);
        // 快照/可用账号数都不能把它算成可用
        assert_eq!(st.snapshot()["enabled_by_platform"]["web-glm"], serde_json::Value::Null);
        // 触发一次落盘：原始密文必须被原样保留（不能变成空的 wrap("")）
        st.set_enabled("x1", true);
        let on_disk = std::fs::read_to_string(&own).unwrap();
        assert!(on_disk.contains("dpapi:AAAA"), "密文被覆盖了：{on_disk}");

        // 重新登录同一 platform+name → 标记摘掉，凭据可用
        let (acc, existed) = st.add_account("web-glm", "外机账号", "cookie", "rt-new");
        assert!(existed);
        assert!(!acc.unreadable);
        assert_eq!(st.secrets_for("web-glm"), vec![("x1".to_string(), "rt-new".to_string())]);
        assert_eq!(st.rows_snapshot()["needs_relogin"], 0);
    }

    /// ★ 升级即加固：装载一个**历史明文** accounts.json 后，磁盘上必须已经变成密文，
    /// 不需要等到下一次登录/改账号。
    #[test]
    fn legacy_plaintext_file_is_encrypted_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let own = tmp.path().join("accounts.json");
        let legacy = tmp.path().join("legacy.json");
        std::fs::write(
            &own,
            r#"[{"id":"p1","platform":"web-glm","name":"老账号","secret":"rt-legacy-plain","type":"cookie","enabled":true}]"#,
        )
        .unwrap();
        let st = AccountStore::load_dual(&legacy, &own);
        // 内存里仍是明文（中继要用）
        assert_eq!(st.secrets_for("web-glm"), vec![("p1".to_string(), "rt-legacy-plain".to_string())]);
        let on_disk = std::fs::read_to_string(&own).unwrap();
        assert!(!on_disk.contains("rt-legacy-plain"), "装载后磁盘上不该再有明文：{on_disk}");
        #[cfg(windows)]
        assert!(on_disk.contains("dpapi:"), "装载时应该已经加密：{on_disk}");
        // 再装载一次仍然可用（幂等）
        let st2 = AccountStore::load_dual(&legacy, &own);
        assert_eq!(st2.secrets_for("web-glm").len(), 1);
    }
}
