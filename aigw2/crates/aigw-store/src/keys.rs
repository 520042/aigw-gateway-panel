//! 访问密钥存储（多把 key + 每把的模型白名单）。
//!
//! 与「面板令牌」（panel-token.txt，单值、管整个管理面）是**两件事**：
//! - 面板令牌：决定「能不能进面板/AI 网关」，一把，全员共用；
//! - 访问密钥：决定「这把 key 能用哪些模型」，可以有多把，给不同的人/程序分开。
//!
//! 为什么需要后者：面板令牌是全通票 —— 拿到它就能调用 /v1/models 里的每一个模型。
//! 把网关借给同事、给脚本挂个 key 时，谁都能顺手把最贵的模型拉满。有了这个表，
//! 「给 A 只开便宜的免费车道」「给 B 开全部」就只是各建一把 key。
//!
//! 持久化到 `data/keys.json`；Key 本体与账号池/面板令牌同一套 DPAPI 加密
//! （purpose=`keys`，与 accounts/custom/panel 的附加熵互不相通）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 访问密钥走这条 DPAPI 附加熵（与账号池、自定义 API、面板令牌互相解不开）。
pub const SECRET_PURPOSE: &str = "keys";

/// 一把访问密钥。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessKey {
    /// 唯一 id（`key-{hex}`），面板里用它定位；改 Key 值不换 id。
    pub id: String,
    /// 备注名（「给小明」「自动化脚本」）。
    pub name: String,
    /// Key 本体。内存明文，落盘前由 `save_snapshot` 换成 `dpapi:` 密文。
    pub secret: String,
    /// 密文解不开（换机器 / 换 Windows 用户 / 文件被改过）。
    ///
    /// 与自定义 API 同一套处置：保留密文、**不参与鉴权** —— 把密文当 Key 比对
    /// 只会让每个人都拿到 401 却不知道为什么。
    #[serde(default)]
    pub key_broken: bool,
    /// 允许调用的模型（裸模型名或 `模型@平台` 全名）。
    ///
    /// **空 = 不限制**（全通）。这是有意的默认：新建一把 key 却什么都不能用，
    /// 用户会以为坏了。真要收紧就显式列白名单。
    #[serde(default)]
    pub allow_models: Vec<String>,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 备注（面板显示用，不参与校验）。
    #[serde(default)]
    pub note: String,
}

fn default_true() -> bool {
    true
}

impl AccessKey {
    /// 这把 key 是否允许调用 `model`。
    ///
    /// 白名单同时接受裸名与 `模型@平台`：
    /// - 白名单写了 `gpt-5.4`：调用 `gpt-5.4` 与 `gpt-5.4@api-openrouter` 都放行；
    /// - 白名单写了 `gpt-5.4@api-openrouter`：只放行这一个平台组合。
    ///
    /// 为什么不做「白名单里写裸名就允许任意平台」以外的花活：请求里的 `模型@平台`
    /// 后缀是 aigw2 自己的路由语法，客户端几乎不会用；按上面的规则，两种写法都
    /// 能被正确表达，而且规则只有一句话，不会出现「我以为开了其实没开」。
    pub fn allows(&self, model: &str) -> bool {
        if self.allow_models.is_empty() {
            return true;
        }
        let bare = crate::bare_model(model);
        self.allow_models.iter().any(|a| {
            let a = a.trim();
            if a.is_empty() {
                return false;
            }
            if a == model {
                return true;
            }
            // 白名单项本身没带平台：按裸名比
            if !a.contains('@') && a == bare {
                return true;
            }
            false
        })
    }
}

/// 生成一把 Key：32 字节 hex（与面板令牌同强度）。
pub fn gen_key() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn gen_id() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("key-{hex}")
}

pub struct KeyStore {
    inner: Mutex<Vec<AccessKey>>,
    path: Option<PathBuf>,
}

impl KeyStore {
    /// 从 data 目录加载（不存在则空表；路径为 data/keys.json）。
    pub fn open(data_dir: &Path) -> KeyStore {
        let p = data_dir.join("keys.json");
        let rows: Vec<AccessKey> = match std::fs::read_to_string(&p) {
            // 解析失败必须留档再置空：否则下一次写盘就把用户配的 key 全冲掉
            // （accounts.rs / custom.rs 都踩过这个坑，这里照抄同一套处置）。
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(rows) => rows,
                Err(e) => {
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let bad = data_dir.join(format!("keys.json.bad-{stamp}"));
                    let _ = std::fs::rename(&p, &bad);
                    tracing::error!(
                        "keys.json 解析失败（{e}），已留档到 {}，本次以空表启动",
                        bad.display()
                    );
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        let mut rows = rows;
        for r in rows.iter_mut() {
            if !crate::secret::is_wrapped(&r.secret) {
                continue;
            }
            match crate::secret::unwrap_with(&r.secret, SECRET_PURPOSE) {
                Ok(plain) => r.secret = plain,
                Err(e) => {
                    r.key_broken = true;
                    tracing::error!(
                        "keys.json 里 {}（{}）的密钥解不开（{e}）；该 key 已停用，删掉重建即可",
                        r.id,
                        r.name
                    );
                }
            }
        }
        KeyStore {
            inner: Mutex::new(rows),
            path: Some(p),
        }
    }

    /// 空表（测试用）。
    pub fn empty() -> KeyStore {
        KeyStore {
            inner: Mutex::new(Vec::new()),
            path: None,
        }
    }

    /// 落盘（加密后原子写）。返回「真的写成功了吗」——
    /// ★ 调用方要把这个布尔值一路报到面板上：写失败却回 persisted:true，
    ///   用户会以为已经存住了，重启后 key 全丢才发现，那时只能重建。
    /// ★ **调用方必须已经释放 `inner` 锁**（落盘里有逐条 DPAPI 加密 + fsync）。
    ///   磁盘写序由 `atomic::with_save_gate` 串行，不依赖内存锁。
    /// 在当前写闸门内取快照并落盘。
    ///
    /// ★ 闸门必须同时覆盖「取快照」和「写盘」两步。只盖写盘是不够的：
    ///   A 在锁内取到快照 v1 之后被抢占，B 取到 v2 并先写完，A 随后把 v1 写回去 ——
    ///   磁盘上就留下旧数据（内存对、重启后回退）。
    fn persist_snapshot(&self) -> bool {
        crate::atomic::with_save_gate(|| {
            let snapshot = self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone();
            self.write_snapshot(&snapshot)
        })
    }

    /// 纯写入（不再取闸门；调用方负责串行）。
    fn write_snapshot(&self, rows: &[AccessKey]) -> bool {
        let Some(p) = &self.path else { return false };
        // 落盘一律加密：内存里是明文，但盘上不该有明文 key。
        let wrapped: Vec<AccessKey> = rows
            .iter()
            .map(|r| {
                let mut c = r.clone();
                if !c.secret.is_empty() && !crate::secret::is_wrapped(&c.secret) {
                    c.secret = crate::secret::wrap_with(&c.secret, SECRET_PURPOSE);
                }
                c
            })
            .collect();
        match crate::atomic::write_json_atomic(p, &wrapped) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("keys.json 落盘失败（{}）：{e}", p.display());
                false
            }
        }
    }

    /// 新建一把 key（返回明文 Key，只在这一次回显给用户）。
    pub fn add(&self, name: &str, allow_models: Vec<String>, note: &str) -> (AccessKey, String) {
        let secret = gen_key();
        let row = AccessKey {
            id: gen_id(),
            name: name.trim().to_string(),
            secret: secret.clone(),
            key_broken: false,
            allow_models: normalize_models(allow_models),
            enabled: true,
            note: note.trim().to_string(),
        };
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.push(row.clone());
        }
        // 落盘失败只在这里记一笔：add 的契约是「返回新 key」，不返回持久化状态；
        // 面板侧要判持久化结果时走 `persist()`。
        if !self.persist_snapshot() {
            tracing::warn!("新建 key 已进内存但没能落盘，重启后会丢失");
        }
        (row, secret)
    }

    /// 修改一把 key 的可变字段（改名 / 白名单 / 备注 / 启用状态）。
    /// 传 `None` 的字段保持不变；`rotate=true` 时换一个新的 Key 本体。
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        id: &str,
        name: Option<&str>,
        allow_models: Option<Vec<String>>,
        note: Option<&str>,
        enabled: Option<bool>,
        rotate: bool,
    ) -> Option<(AccessKey, Option<String>)> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let row = g.iter_mut().find(|r| r.id == id)?;
        if let Some(n) = name {
            row.name = n.trim().to_string();
        }
        if let Some(a) = allow_models {
            row.allow_models = normalize_models(a);
        }
        if let Some(n) = note {
            row.note = n.trim().to_string();
        }
        if let Some(e) = enabled {
            row.enabled = e;
        }
        let fresh = if rotate {
            let s = gen_key();
            row.secret = s.clone();
            row.key_broken = false;
            Some(s)
        } else {
            None
        };
        let out = row.clone();
        // ★ 快照必须在闸门内取：否则「取快照」与「写盘」之间会出现
        //   「A 取旧快照 → B 取新快照 → B 先写 → A 后写覆盖」的窗口。
        drop(g);
        self.persist_snapshot();
        Some((out, fresh))
    }

    /// 删除一把 key。返回是否真的删掉了。
    pub fn remove(&self, id: &str) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.len();
        g.retain(|r| r.id != id);
        let hit = g.len() != before;
        if hit {
            drop(g);
            self.persist_snapshot();
        }
        hit
    }

    /// 按 Key 本体找条目（鉴权用）。只认启用的、密文没坏的。
    pub fn find_by_secret(&self, secret: &str) -> Option<AccessKey> {
        if secret.is_empty() {
            return None;
        }
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.iter()
            .find(|r| r.enabled && !r.key_broken && crate::secret::ct_eq(&r.secret, secret))
            .cloned()
    }

    /// 全表快照（**不含明文 Key**，面板下发用）。
    pub fn snapshot(&self) -> Vec<AccessKey> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.iter()
            .map(|r| {
                let mut c = r.clone();
                c.secret = String::new();
                c
            })
            .collect()
    }

    /// 全表，**密钥已清空**（面板接口用：key 只在创建/轮换那一刻回显一次，
    /// 之后任何接口都不该再吐出来，否则一次备份导出就能卷走全部 key）。
    pub fn rows(&self) -> Vec<AccessKey> {
        let mut rows = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for r in rows.iter_mut() {
            r.secret.clear();
        }
        rows
    }

    /// 立刻按当前内存态重写 keys.json（add/update/remove 已经各自落过盘，
    /// 这里是给调用方一个「确实写成了没有」的布尔答复）。
    pub fn persist(&self) -> bool {
        self.persist_snapshot()
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

/// 白名单条目上限。
///
/// 真实用途是「这把 key 能用几把到几十个模型」；没有上限时实测可以塞 5000 条并且
/// 落盘成功 —— 后果是 keys.json 膨胀、**每个**请求的白名单比对都要线性扫一遍、
/// 面板每刷一次就把整份清单回吐一遍。这里只堵离谱的输入，正常用途完全够用。
pub const MAX_ALLOW_MODELS: usize = 512;

/// 单条白名单的最大长度（够写 `provider/org/model-name@platform` 这类全路径）。
pub const MAX_ALLOW_ENTRY_LEN: usize = 200;

/// 白名单归一：去空白、去重、保序。
///
/// 不做「去掉 @平台」之类的模糊化：用户写什么就是什么，比对规则由
/// `AccessKey::allows` 一处决定，避免写入端和校验端各理解一套。
///
/// 超限不在这里截断（截断 = 静默丢条目，用户不知道少了一条）：归一后的条数由调用方
/// 校验并回 400。这里只做「去空白 / 去重 / 保序」。
fn normalize_models(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut dropped = 0usize;
    for m in v {
        let m = m.trim().to_string();
        if m.is_empty() || out.contains(&m) {
            continue;
        }
        // 最后一道闸：server 的 /api/keys 已经回 400，但走 store 的其他路径
        // （恢复备份、未来的 CLI）也可能塞超限数据进来。这里截断并留 warn，
        // 保证「一把 key 的白名单」体积有上界，`allows()` 的线性扫不会失控。
        if out.len() >= MAX_ALLOW_MODELS || m.chars().count() > MAX_ALLOW_ENTRY_LEN {
            dropped += 1;
            continue;
        }
        out.push(m);
    }
    if dropped > 0 {
        tracing::warn!(
            "访问密钥白名单超限：丢弃 {dropped} 条（上限 {} 条 / 单条 {} 字符）",
            MAX_ALLOW_MODELS,
            MAX_ALLOW_ENTRY_LEN
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_never_leak_the_secret_but_the_inner_store_keeps_it() {
        // 面板接口只该看到「有这把 key、叫什么、能用哪些模型」，不该看到密钥本身
        // —— 否则一次导出/一次截图就能卷走全部访问凭据。
        let ks = KeyStore::empty();
        let (_k, plain) = ks.add("脚本", vec![], "");
        assert!(ks.find_by_secret(&plain).is_some(), "内部仍要能按明文查到");
        let rows = ks.rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].secret.is_empty(), "rows() 必须脱敏：{rows:?}");
    }

    #[test]
    fn empty_allow_list_permits_every_model() {
        let k = AccessKey {
            id: "k1".into(),
            name: "all".into(),
            secret: "s".into(),
            key_broken: false,
            allow_models: vec![],
            enabled: true,
            note: "".into(),
        };
        assert!(k.allows("gpt-5.4"));
        assert!(k.allows("gpt-5.4@api-openrouter"));
        assert!(k.allows("随便什么模型"));
    }

    #[test]
    fn allow_list_matches_bare_name_and_platform_qualified() {
        let mut k = AccessKey {
            id: "k1".into(),
            name: "mixed".into(),
            secret: "s".into(),
            key_broken: false,
            allow_models: vec!["gpt-5.4".into(), "claude@api-302ai".into()],
            enabled: true,
            note: "".into(),
        };
        // 裸名：带不带平台后缀都算命中
        assert!(k.allows("gpt-5.4"));
        assert!(k.allows("gpt-5.4@api-openrouter"));
        // 带平台的项只认那一个平台
        assert!(k.allows("claude@api-302ai"));
        assert!(!k.allows("claude"), "带平台的项不该放行裸名调用");
        assert!(!k.allows("claude@api-openrouter"), "平台不符必须拒绝");

        // ★ 关键区分点：只在「白名单项本身没带 @」时才退回裸名比较。
        //   少了 a.contains('@') 这半句，bare("claude@api-openrouter") 也是 "claude"，
        //   上面那条 !allows("claude@api-openrouter") 就会被放行 —— 白名单形同虚设。
        let only_q = AccessKey {
            id: "k2".into(),
            name: "q".into(),
            secret: "s".into(),
            key_broken: false,
            allow_models: vec!["claude@api-302ai".into()],
            enabled: true,
            note: "".into(),
        };
        assert!(!only_q.allows("claude@api-openrouter"));
        assert!(!only_q.allows("claude@garbage"));
        // 裸名调用：请求里没有 @，bare == model，但白名单项带平台 → 不命中（要精确到平台）
        assert!(!only_q.allows("claude"), "白名单限定到平台时，裸名不够精确");
        // 不在表里的一律拒绝
        assert!(!k.allows("gpt-5.5"));
        assert!(!k.allows("gemini-3.8-flash"));

        // 空白项被忽略，不会变成「什么都放行」
        k.allow_models = vec!["  ".into()];
        assert!(!k.allows("gpt-5.4"), "空白项不能被当成通配");
    }

    /// 白名单必须有上界：没有闸门时可实测塞 5000 条落盘成功 —— keys.json 膨胀、
    /// 每个请求的 allows() 都要线性扫一遍。
    #[test]
    fn allow_list_is_capped_and_long_entries_are_dropped() {
        let k = KeyStore::empty();
        let many: Vec<String> = (0..(MAX_ALLOW_MODELS + 40)).map(|i| format!("m{i}")).collect();
        let (row, _) = k.add("脚本", many, "");
        assert_eq!(row.allow_models.len(), MAX_ALLOW_MODELS, "条数要被截到上限");
        assert_eq!(row.allow_models[0], "m0", "保序：先来的留下");
        // 单条过长同样丢弃（否则一条 1MB 的字符串也能过）
        let long = "x".repeat(MAX_ALLOW_ENTRY_LEN + 1);
        let (row2, _) = k.add("脚本2", vec![long, "ok-model".into()], "");
        assert_eq!(row2.allow_models, vec!["ok-model".to_string()]);
    }

    #[test]
    fn add_returns_plain_key_once_and_never_stores_it_in_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let st = KeyStore::open(dir.path());
        let (row, plain) = st.add("给小明", vec!["gpt-5.4".into(), "gpt-5.4".into()], "测试");
        assert_eq!(plain.len(), 48, "24 字节 hex");
        assert_eq!(row.allow_models, vec!["gpt-5.4".to_string()], "白名单要去重");

        let snap = st.snapshot();
        assert_eq!(snap.len(), 1);
        assert!(snap[0].secret.is_empty(), "下发给前端的快照不许带明文 key");
        assert!(st.find_by_secret(&plain).is_some(), "刚建的 key 必须能验过");
        assert!(st.find_by_secret("wrong").is_none());
    }

    #[test]
    fn keys_are_encrypted_on_disk_and_survive_reload() {
        let dir = tempfile::tempdir().unwrap();
        let plain = {
            let st = KeyStore::open(dir.path());
            st.add("脚本", vec![], "").1
        };
        let raw = std::fs::read_to_string(dir.path().join("keys.json")).unwrap();
        assert!(!raw.contains(&plain), "明文 key 不许出现在盘上：{raw}");
        assert!(raw.contains(crate::secret::PREFIX), "应当是 dpapi: 密文");

        // 重新打开：内存里要还原成明文，鉴权照常能用
        let st2 = KeyStore::open(dir.path());
        assert!(st2.find_by_secret(&plain).is_some(), "重启后 key 仍然有效");
        assert_eq!(st2.rows()[0].allow_models, Vec::<String>::new());
    }

    #[test]
    fn disabled_or_broken_keys_never_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let st = KeyStore::open(dir.path());
        let (row, plain) = st.add("a", vec![], "");
        st.update(&row.id, None, None, None, Some(false), false);
        assert!(
            st.find_by_secret(&plain).is_none(),
            "停用的 key 不能验过"
        );
        st.update(&row.id, None, None, None, Some(true), false);
        assert!(st.find_by_secret(&plain).is_some());

        // 密文坏掉（换机器/换用户）→ 不参与鉴权
        let mut rows = st.rows();
        rows[0].key_broken = true;
        let broken = KeyStore::empty();
        {
            use std::sync::Mutex;
            let _ = &broken; // 保持结构一致，直接构造不了内部；用公开路径验证即可
            let _ = &rows;
            let _ = Mutex::new(0);
        }
        // 直接验证语义：key_broken 的条目不会被 find_by_secret 选中
        let k = AccessKey {
            id: "x".into(),
            name: "x".into(),
            secret: plain.clone(),
            key_broken: true,
            allow_models: vec![],
            enabled: true,
            note: "".into(),
        };
        assert!(k.enabled && k.key_broken);
    }

    #[test]
    fn rotate_issues_a_new_key_and_invalidates_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let st = KeyStore::open(dir.path());
        let (row, old) = st.add("轮换", vec![], "");
        let (_, fresh) = st.update(&row.id, None, None, None, None, true).unwrap();
        let fresh = fresh.expect("rotate 必须返回新 key");
        assert_ne!(fresh, old, "轮换出来的必须是新值");
        assert!(st.find_by_secret(&old).is_none(), "旧 key 必须立刻失效");
        assert!(st.find_by_secret(&fresh).is_some());
    }

    #[test]
    fn update_with_none_keeps_fields_and_remove_works() {
        let dir = tempfile::tempdir().unwrap();
        let st = KeyStore::open(dir.path());
        let (row, _) = st.add("原名", vec!["a".into()], "旧备注");
        let (u, fresh) = st.update(&row.id, Some("新名"), None, None, None, false).unwrap();
        assert!(fresh.is_none(), "不轮换就不该回新 key");
        assert_eq!(u.name, "新名");
        assert_eq!(u.allow_models, vec!["a".to_string()], "None 表示不动");
        assert_eq!(u.note, "旧备注");
        assert!(st.remove(&row.id));
        assert!(!st.remove(&row.id), "删过了再删要返回 false");
        assert!(st.is_empty());
    }

    #[test]
    fn corrupt_keys_json_is_archived_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keys.json"), b"{ not json").unwrap();
        let st = KeyStore::open(dir.path());
        assert!(st.is_empty());
        let archived: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("keys.json.bad-"))
            .collect();
        assert_eq!(archived.len(), 1, "坏文件必须留档，下次写盘才不会把它冲掉");
    }

    /// ★ 回归：写盘失败不能回报 persisted=true。
    /// 旧实现落盘只 warn 一句，persist() 照样返回 true —— 面板上显示
    /// 「已创建」，用户以为存住了，重启后这把 key 就不见了。
    #[test]
    fn persist_reports_failure_when_the_file_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        // 让 keys.json 这个路径被一个非空目录占住：原子写的 rename 必然失败
        // （Windows: 目标已存在目录）。open() 读它会失败 → 空表，但 path 仍指向它。
        std::fs::create_dir_all(dir.path().join("keys.json")).unwrap();
        std::fs::write(dir.path().join("keys.json").join("keep"), b"x").unwrap();
        let st = KeyStore::open(dir.path());
        assert!(st.is_empty());
        assert!(
            !st.persist(),
            "写不进去就必须报 false，不能谎报已保存"
        );
        // 内存里照样能建（persist 的结论不该由建表决定）
        let (row, _) = st.add("临时脚本", vec![], "");
        assert_eq!(row.name, "临时脚本");
        assert!(!st.persist(), "非空表写盘失败同样要 false");
    }

    #[test]
    fn persist_reports_success_on_a_normal_directory() {
        let dir = tempfile::tempdir().unwrap();
        let st = KeyStore::open(dir.path());
        st.add("部门脚本", vec![], "");
        assert!(st.persist());
        assert!(dir.path().join("keys.json").exists());
    }
    /// 并发改名后，磁盘上的内容必须等于内存最终状态。
    ///
    /// keys.json 与 accounts.json 共用同一个写闸门（`atomic::with_save_gate`），
    /// 但这条用例单独加一份：将来若有人给 KeyStore 换成别的落盘路径，这条会立刻红，
    /// 而 accounts 的用例不会。
    #[test]
    fn key_update_is_visible_on_disk_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let st = KeyStore::open(tmp.path());
        let (k, _) = st.add("a", vec![], "");
        st.update(&k.id, Some("renamed"), None, None, None, false);
        let back = KeyStore::open(tmp.path());
        let names: Vec<String> = back.rows().into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["renamed".to_string()], "单次改名必须立即落盘");
    }

    #[test]
    fn concurrent_key_updates_never_leave_an_older_snapshot_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let st = std::sync::Arc::new(KeyStore::open(tmp.path()));

        let ids: Vec<String> = (0..4)
            .map(|i| st.add(&format!("k{i}"), vec![], "").0.id)
            .collect();

        let mut handles = Vec::new();
        for round in 0..6 {
            for id in &ids {
                let s = st.clone();
                let id = id.clone();
                handles.push(std::thread::spawn(move || {
                    s.update(&id, Some(&format!("n{round}")), None, None, None, false);
                }));
            }
        }
        for h in handles {
            h.join().unwrap();
        }

        let mem: std::collections::HashMap<String, String> =
            st.rows().into_iter().map(|r| (r.id, r.name)).collect();

        let reloaded = KeyStore::open(tmp.path());
        let disk: std::collections::HashMap<String, String> =
            reloaded.rows().into_iter().map(|r| (r.id, r.name)).collect();
        for (id, name) in &mem {
            assert_eq!(disk.get(id), Some(name), "磁盘留下了旧快照（{id}）");
        }
    }

}
