//! 面板开关存储（逐模型启用/停用）。
//!
//! 对齐 Python 版 `store.disabled_models`：**按裸模型名记账**（`模型@平台` 只取前半段），
//! 否则「总表里关掉了，路由里 `模型@平台` 还能用」的不一致会一直存在。
//! 持久化到 `data/flags.json`。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Flags {
    /// 被停用的模型名（裸名）。
    #[serde(default)]
    pub disabled_models: Vec<String>,
    /// 被停用的平台 id（文件 spec 无法就地改写，停用状态记在这里）。
    #[serde(default)]
    pub disabled_platforms: Vec<String>,
    /// wb 族每日自动签到开关。默认关：自动去上游领积分是主动写操作
    /// （第三方服务可见、可能触发风控），必须由用户在面板里显式开启。
    #[serde(default)]
    pub daily_checkin: bool,
    /// wb 族每日成长任务自动完成开关（对齐 workbuddy2api 17 任务闭环）。
    /// 同样默认关：事件链上报也是对上游的主动写操作。
    #[serde(default)]
    pub daily_growth: bool,
}

pub struct FlagStore {
    inner: Mutex<Flags>,
    path: Option<PathBuf>,
}

/// `模型id@平台` → `模型id`（裸模型名）。
pub fn bare_model(model: &str) -> &str {
    match model.rsplit_once('@') {
        Some((m, _)) => m,
        None => model,
    }
}

impl FlagStore {
    pub fn open(data_dir: &Path) -> FlagStore {
        let p = data_dir.join("flags.json");
        // 损坏不再静默重置：过去 read_to_string().ok() 一旦解析失败就 unwrap_or_default()，
        // 用户辛苦关掉的几十个模型会在一次半截写入后全部"复活"，且日志里一个字都没有。
        // 现在先把坏文件留档成 flags.json.bad-<ts>，再空表启动并 warn。
        let flags: Flags = match std::fs::read_to_string(&p) {
            Ok(raw) => match serde_json::from_str::<Flags>(&raw) {
                Ok(f) => f,
                Err(e) => {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let bad = data_dir.join(format!("flags.json.bad-{ts}"));
                    let _ = std::fs::rename(&p, &bad);
                    tracing::warn!(
                        "flags.json 解析失败（{e}），已留档为 {} 并以空表启动",
                        bad.display()
                    );
                    Flags::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Flags::default(),
            Err(e) => {
                tracing::warn!("flags.json 读取失败（{e}），以空表启动");
                Flags::default()
            }
        };
        FlagStore {
            inner: Mutex::new(flags),
            path: Some(p),
        }
    }

    /// 无持久化（测试用）。
    pub fn empty() -> FlagStore {
        FlagStore {
            inner: Mutex::new(Flags::default()),
            path: None,
        }
    }

    fn save_locked(&self, f: &Flags) {
        if let Some(p) = &self.path {
            if let Err(e) = crate::atomic::write_json_atomic(p, &f) {
                tracing::warn!("flags 落盘失败（{}）：{e}", p.display());
            }
        }
    }

    /// 该模型是否被停用（按裸名判断）。
    pub fn is_model_disabled(&self, model: &str) -> bool {
        let bare = bare_model(model);
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disabled_models
            .iter()
            .any(|m| m == bare)
    }

    /// 停用集合（裸名，供 /v1/models 过滤）。
    pub fn disabled_models(&self) -> BTreeSet<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disabled_models
            .iter()
            .cloned()
            .collect()
    }

    /// 停用的平台集合（供 /api/platforms 下发）。
    pub fn disabled_platforms(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disabled_platforms
            .clone();
        v.sort();
        v
    }

    /// 该平台是否被面板停用。
    pub fn is_platform_disabled(&self, platform: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disabled_platforms
            .iter()
            .any(|p| p == platform)
    }

    /// 设置平台启用状态；返回设置后的启用值。
    pub fn set_platform_enabled(&self, platform: &str, enabled: bool) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut set: BTreeSet<String> = g.disabled_platforms.iter().cloned().collect();
        if enabled {
            set.remove(platform);
        } else {
            set.insert(platform.to_string());
        }
        g.disabled_platforms = set.into_iter().collect();
        // ★ 必须在持锁期间落盘：旧实现 clone → drop(g) → save，两个并发 toggle
        //   （toggle_platform 与 toggle_model 走同一文件）可能以相反顺序覆盖，磁盘丢一次变更，
        //   重启后「刚关掉的模型又活了」。accounts/custom/free 都是持锁落盘，这里对齐。
        self.save_locked(&g);
        enabled
    }

    /// 设置某模型启用状态；返回设置后的启用值。
    pub fn set_model_enabled(&self, model: &str, enabled: bool) -> bool {
        let bare = bare_model(model).to_string();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut set: BTreeSet<String> = g.disabled_models.iter().cloned().collect();
        if enabled {
            set.remove(&bare);
        } else {
            set.insert(bare);
        }
        g.disabled_models = set.into_iter().collect();
        self.save_locked(&g); // 同上：持锁落盘，防并发丢更新
        enabled
    }

    /// wb 族每日自动签到是否开启。
    pub fn daily_checkin(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daily_checkin
    }

    /// 开/关每日自动签到；返回设置后的值。持锁落盘（与 set_platform_enabled 同一纪律）。
    pub fn set_daily_checkin(&self, on: bool) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.daily_checkin = on;
        self.save_locked(&g);
        on
    }

    /// 每日成长任务自动完成开关。
    pub fn daily_growth(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .daily_growth
    }

    pub fn set_daily_growth(&self, on: bool) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.daily_growth = on;
        self.save_locked(&g);
        on
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_model_strips_platform_suffix() {
        assert_eq!(bare_model("glm-4-flash"), "glm-4-flash");
        assert_eq!(bare_model("glm-4-flash@web-glm"), "glm-4-flash");
        // 模型名自带 @ 但无平台语义时也只剥最后一段（与路由语义一致）
        assert_eq!(bare_model("a@b@c"), "a@b");
    }

    #[test]
    fn toggle_by_bare_name_covers_platform_suffix() {
        let st = FlagStore::empty();
        assert!(!st.is_model_disabled("m1@plat"));
        st.set_model_enabled("m1@plat", false);
        assert!(st.is_model_disabled("m1"));
        assert!(st.is_model_disabled("m1@other"));
        st.set_model_enabled("m1", true);
        assert!(!st.is_model_disabled("m1@plat"));
    }

    #[test]
    fn platform_toggle_persists() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let st = FlagStore::open(tmp.path());
            assert!(!st.is_platform_disabled("web-glm"));
            st.set_platform_enabled("web-glm", false);
            assert!(st.is_platform_disabled("web-glm"));
        }
        let st2 = FlagStore::open(tmp.path());
        assert!(st2.is_platform_disabled("web-glm"));
        st2.set_platform_enabled("web-glm", true);
        assert!(!st2.is_platform_disabled("web-glm"));
    }

    #[test]
    fn daily_checkin_flag_persists() {
        // 默认必须是关的：没人显式同意前，网关不许自己去上游领积分。
        let tmp = tempfile::tempdir().unwrap();
        {
            let st = FlagStore::open(tmp.path());
            assert!(!st.daily_checkin());
            assert!(st.set_daily_checkin(true));
            assert!(st.daily_checkin());
        }
        let st2 = FlagStore::open(tmp.path());
        assert!(st2.daily_checkin(), "开关必须落盘，重启后保持");
        st2.set_daily_checkin(false);
        let st3 = FlagStore::open(tmp.path());
        assert!(!st3.daily_checkin());
    }

    #[test]
    fn persists_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let st = FlagStore::open(tmp.path());
            st.set_model_enabled("gone", false);
        }
        let st2 = FlagStore::open(tmp.path());
        assert!(st2.is_model_disabled("gone"));
        assert_eq!(st2.disabled_models().len(), 1);
    }
}
