//! 面板开关存储（逐模型启用/停用）。
//!
//! 对齐 Python 版 `store.disabled_models`：**按裸模型名记账**（`模型@平台` 只取前半段），
//! 否则「总表里关掉了，路由里 `模型@平台` 还能用」的不一致会一直存在。
//! 持久化到 `data/flags.json`。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flags {
    /// 被停用的模型名（裸名）。
    #[serde(default)]
    pub disabled_models: Vec<String>,
    /// 被停用的平台 id（文件 spec 无法就地改写，停用状态记在这里）。
    #[serde(default)]
    pub disabled_platforms: Vec<String>,
    /// 自动签到总开关。默认关：自动去上游领积分是主动写操作
    /// （第三方服务可见、可能触发风控），必须由用户在面板里显式开启。
    #[serde(default)]
    pub daily_checkin: bool,
    /// 每日 WB 任务自动完成开关（对齐 workbuddy2api 17 任务闭环）。
    /// 同样默认关：事件链上报也是对上游的主动写操作。
    #[serde(default)]
    pub daily_growth: bool,
    /// 自动执行的**本地小时**（0-23）。默认 9（早上 9 点）。
    ///
    /// ★ 为什么需要自定义：旧实现是「每 12 小时跑一次」，执行时刻随启动时间漂移，
    ///   用户既不知道今天几点会跑，也没法挑一个上游不拥挤的时段。
    ///   存小时而不是完整 cron：这些任务是一天一次的粒度，分钟级没有意义，
    ///   而且上游对同一账号当天重复签到本来就幂等，几点领不影响结果。
    #[serde(default = "default_run_hour")]
    pub run_hour: u8,
    /// 自动执行的分钟（0-59）。默认 0。
    ///
    /// ★ 2026-11-06：参考项目的「定时签到」用的是 `<input type="time">`，
    ///   「自定义签到时间」不能只让用户选整点。小时与分钟分开存，是为了
    ///   老 flags.json 里只有 run_hour 时仍能按 09:00 正确启动（serde default 0）。
    #[serde(default)]
    pub run_minute: u8,
    /// 参与自动签到的平台 id 集合。空集 = 沿用 `daily_checkin` 总开关（兼容旧配置）。
    ///
    /// ★ 为什么要按平台分别设：WB / Lobster / 小浣熊是三套独立上游，用户可能只想
    ///   自动签其中一家（比如小浣熊活动经常变，不想每天自动打）。
    ///   非空时 `daily_checkin` 不再作为唯一依据，改由这里逐平台判定。
    #[serde(default)]
    pub checkin_platforms: Vec<String>,
}

/// 默认执行时刻：早上 9 点（本地时间）。避开 0 点上游批量任务高峰。
fn default_run_hour() -> u8 { 9 }

impl Default for Flags {
    fn default() -> Self {
        Self {
            disabled_models: Vec::new(),
            disabled_platforms: Vec::new(),
            daily_checkin: false,
            daily_growth: false,
            // ★ 必须手写 Default：derive(Default) 会把 u8 置 0，
            //   与 serde 的 `default = "default_run_hour"`（9）不一致 ——
            //   新建 flags.json 与从磁盘读入会拿到不同的默认时刻。
            run_hour: default_run_hour(),
            run_minute: 0,
            checkin_platforms: Vec::new(),
        }
    }
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

    /// 在写闸门内取快照并落盘（各写方法的统一出口）。
    ///
    /// ★ 旧实现是「持内存锁落盘」防并发丢更新 —— 排序是对的，但面板轮询
    ///   `disabled_models()` / `is_model_disabled()`（每次路由都要问）会陪着等磁盘。
    ///   现在内存锁只改内存，闸门负责磁盘写序，两边都拿到。
    fn persist_now(&self) {
        crate::atomic::with_save_gate(|| {
            let snapshot = self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone();
            self.write_snapshot(&snapshot);
        });
    }

    /// 纯写入（调用方负责串行）。
    fn write_snapshot(&self, f: &Flags) {
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
        // 内存态已改完，放锁后由闸门串行落盘（闸门同时盖住取快照与写盘）

        drop(g);

        self.persist_now();
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
        drop(g);
        self.persist_now();
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
        drop(g);
        self.persist_now();
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
        drop(g);
        self.persist_now();
        on
    }

    /// 自动任务在一天里的执行秒数（北京时间 00:00 起算）。
    /// 非法分钟一律回落默认 9:00，绝不把 60+ 传给调度计算。
    pub fn run_time(&self) -> (u8, u8) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let h = if g.run_hour <= 23 { g.run_hour } else { default_run_hour() };
        let m = if g.run_minute <= 59 { g.run_minute } else { 0 };
        (h, m)
    }

    pub fn run_hour(&self) -> u8 {
        self.run_time().0
    }

    pub fn run_minute(&self) -> u8 {
        self.run_time().1
    }

    /// 一次性写 HH:MM，避免小时、分钟分两次落盘产生「改到一半」的中间态。
    pub fn set_run_time(&self, hour: u8, minute: u8) -> (u8, u8) {
        let h = if hour <= 23 { hour } else { default_run_hour() };
        let m = if minute <= 59 { minute } else { 0 };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.run_hour = h;
        g.run_minute = m;
        drop(g);
        self.persist_now();
        (h, m)
    }

    pub fn set_run_hour(&self, hour: u8) -> u8 {
        let m = self.run_minute();
        self.set_run_time(hour, m).0
    }

    /// 自动签到的平台集合（克隆出来，避免把锁暴露给调用方）。
    pub fn checkin_platforms(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .checkin_platforms
            .clone()
    }

    /// 整组替换自动签到平台（面板一次性提交勾选项）。
    pub fn set_checkin_platforms(&self, ids: Vec<String>) -> Vec<String> {
        let mut v: Vec<String> = ids.into_iter().filter(|s| !s.is_empty()).collect();
        v.sort();
        v.dedup();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.checkin_platforms = v.clone();
        drop(g);
        self.persist_now();
        v
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
    fn run_time_defaults_and_persists_minutes() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let st = FlagStore::open(tmp.path());
            assert_eq!(st.run_time(), (9, 0), "老配置/新装的默认必须是 09:00");
            assert_eq!(st.set_run_time(7, 30), (7, 30));
            assert_eq!(st.run_hour(), 7);
            assert_eq!(st.run_minute(), 30);
        }
        let st2 = FlagStore::open(tmp.path());
        assert_eq!(st2.run_time(), (7, 30), "自定义 HH:MM 必须落盘");

        // 旧配置只有 run_hour、没有 run_minute：按整点执行，不能回退成 0 点。
        std::fs::write(
            tmp.path().join("flags.json"),
            r#"{"disabled_models":[],"disabled_platforms":[],"daily_checkin":false,"daily_growth":false,"run_hour":6,"checkin_platforms":[]}"#,
        )
        .unwrap();
        assert_eq!(FlagStore::open(tmp.path()).run_time(), (6, 0));
    }

    #[test]
    fn run_time_rejects_invalid_values() {
        let st = FlagStore::empty();
        assert_eq!(st.set_run_time(99, 99), (9, 0));
        assert_eq!(st.run_time(), (9, 0));
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
