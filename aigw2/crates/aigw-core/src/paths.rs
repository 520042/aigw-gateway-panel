//! 跨平台的数据/配置目录解析。
//!
//! ★ 一切以「能不能在 Linux/macOS 上正常跑」为准。
//!
//! 旧实现把数据目录写死成 `%LOCALAPPDATA%\aigw2\data`，并且在环境变量缺失时
//! 退到**相对路径** `./data`。在 Linux/macOS 上那是**真正的故障**：相对路径意味着
//! 数据目录跟着「当前工作目录」变 —— 从 systemd 启动是 `/`、从 shell 启动是 `$HOME`、
//! 从别处启动又是三三：两次启动看不到同一套账号与台账。
//!
//! 本模块是**唯一来源**：`aigw`（bin）与 `aigw-relay`（proxy 配置）都从这里取。
//! 两处各写一份是这个项目反复跨到的坑（参见 MEMORY：「同一个事实在多处手写」）。
//!
//! ## ★ Windows 上保留 `%LOCALAPPDATA%`（不是 `%APPDATA%`）
//!
//! 这里曾经踩过一个「跨平台改造顺手把已有部署改坏」的坑：改成
//! `%APPDATA%\aigw2\data` 看起来更「标准」（Roaming 跟用户走域漫游），
//! 但**已部署的机器上真实数据全在 `%LOCALAPPDATA%`**（accounts.json / keys.json /
//! panel.db + -wal / pool-state.json …）。换根目录等于让升级后的服务读到一个空目录：
//! 面板上账号全没了、/v1 全部 403、台账清零，而旧数据还好端端躺在原处 ——
//! 用户只会以为「升级把数据搞丢了」。
//!
//! 结论：**目录迁移只能在「必须先赢一次迁移」的前提下做**，不能靠改常量静默完成。
//! 现在 Windows 恒用 `%LOCALAPPDATA%`；跨平台只需保证「非 Windows 也有稳定的绝对路径」。
//! 真要迁到 Roaming，那要单独做一次带迁移代码的版本（见 [`migrate_legacy_data_dir`]）。

use std::path::PathBuf;

/// 应用名（数据目录/配置目录下的子目录名）。
pub const APP: &str = "aigw2";

/// 用户配置根目录（平台默认）。
///
/// | 平台 | 来源 | 典型值 |
/// |---|---|---|
/// | Windows | `%LOCALAPPDATA%` → `%APPDATA%` | `C:\Users\me\AppData\Local` |
/// | Linux/macOS | `$XDG_CONFIG_HOME` → `$HOME/.config` | `/home/me/.config` |
/// | 最后手段 | 当前目录 | 不得已而（至少是绝对路径） |
///
/// Windows 上优先 `%LOCALAPPDATA%`：那是历史数据真正所在的位置（见模块头注释）。
/// 只有它缺失时才退 `%APPDATA%`，免得在奇怪的环境里给出相对路径。
pub fn config_root() -> PathBuf {
    if cfg!(windows) {
        for key in ["LOCALAPPDATA", "APPDATA"] {
            if let Some(v) = std::env::var_os(key) {
                let p = PathBuf::from(v);
                if !p.as_os_str().is_empty() {
                    return p;
                }
            }
        }
    } else {
        if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() {
                return p;
            }
        }
        if let Some(h) = std::env::var_os("HOME") {
            let p = PathBuf::from(h);
            if !p.as_os_str().is_empty() {
                return p.join(".config");
            }
        }
    }
    // 剥底：用绝对路径而不是 `./x`。`current_dir` 基本总能成功，
    // 真失败时才退到 `"."`。
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 默认数据目录：`<config_root>/aigw2/data`。
///
/// ★ `AIGW_DATA_DIR` 的处理**不在这里**：调用方（main / proxy）先查环境变量，
///   本函数只答「默认在哪」。这样「用户显式指定」与「平台默认」两件事
///   不会在多个地方各自判一遍。
pub fn data_dir() -> PathBuf {
    config_root().join(APP).join("data")
}

/// 历史上曾经用过的数据目录（**只读**，用于迁移/导入，不要往这里写）。
///
/// 目前只有一条：Windows 上一版短暂用过 `%APPDATA%\aigw2\data`。
/// 把它列出来是为了让「已经用新版本跑过一次的机器」能把数据搬回来，
/// 而不是在 `/api/health` 里看着平台数 0 无从下手。
pub fn legacy_data_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        if let Some(v) = std::env::var_os("APPDATA") {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() {
                let d = p.join(APP).join("data");
                if d != data_dir() {
                    out.push(d);
                }
            }
        }
    }
    out
}

/// 一次性数据迁移：把 [`legacy_data_dirs`] 里**存在但当前目录没有**的文件搬过来。
///
/// 语义严格是「补缺」，不覆盖：当前目录已有的文件一律保留（那是更新的真相）。
/// 返回搬过来的文件名清单，供启动日志与 /api/health 展示。
///
/// ★ 只搬不删：源目录原样保留，用户确认无误后自己清理。半途失败也只是少搬几个，
///   不会出现「两边都没有」的状态。
pub fn migrate_legacy_data_dir() -> Vec<String> {
    let dst = data_dir();
    let mut moved: Vec<String> = Vec::new();
    for src in legacy_data_dirs() {
        if !src.is_dir() || src == dst {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&src) else { continue };
        if std::fs::create_dir_all(&dst).is_err() {
            return moved;
        }
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let Some(name) = p.file_name() else { continue };
            let target = dst.join(name);
            if target.exists() {
                continue;
            }
            if std::fs::copy(&p, &target).is_ok() {
                moved.push(name.to_string_lossy().into_owned());
            }
        }
    }
    moved.sort();
    moved
}

/// 跟磁盘上的密钥文件目录（凭据加密用）。
///
/// ★ 必须与 [`data_dir`] 分开：`data/` 会被备份、导出、整包拷走。
pub fn key_dir() -> PathBuf {
    config_root().join(APP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_is_absolute_and_under_config_root() {
        let d = data_dir();
        assert!(d.is_absolute(), "数据目录必须是绝对路径：{}", d.display());
        assert!(d.ends_with("data"), "应以 data 结尾：{}", d.display());
        // 必须是 <config_root>/aigw2/data，不能是 "./data"
        assert_eq!(d, config_root().join(APP).join("data"));
    }

    #[test]
    fn key_dir_is_not_inside_data_dir() {
        // 密钥目录与数据目录必须是兄弟，不能是父子 —— 否则整包拷走 data/ 就把密钥也带走了。
        let k = key_dir();
        let d = data_dir();
        assert!(!k.starts_with(&d), "密钥目录不能在数据目录里：{} vs {}", k.display(), d.display());
        assert_ne!(k, d);
    }

    #[test]
    fn config_root_follows_platform_env() {
        // 只验证「确实用了平台环境变量」这个契约，不锁死具体值（否则换机器就假红）。
        let r = config_root();
        assert!(r.is_absolute(), "config_root 必须是绝对路径：{}", r.display());
        #[cfg(windows)]
        {
            // ★ Windows 必须优先 %LOCALAPPDATA%：已部署机器的数据都在那儿。
            //   改成 %APPDATA% 会让升级后读到一个空目录（账号/密钥/台账全「消失」）。
            if let Some(v) = std::env::var_os("LOCALAPPDATA") {
                assert_eq!(r, PathBuf::from(v), "Windows 上必须优先用 %LOCALAPPDATA%");
            }
        }
        #[cfg(not(windows))]
        {
            assert!(
                std::env::var_os("XDG_CONFIG_HOME").is_none() || r == PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()),
                "Unix 上应优先 $XDG_CONFIG_HOME"
            );
        }
    }

    /// ★ 回归：Windows 数据目录必须还是 `%LOCALAPPDATA%\aigw2\data`。
    ///
    /// 这条是在「跨平台改造把根目录改成了 %APPDATA%」之后补的。那种改动在本机编译
    /// 与测试全绿（两条路径都是绝对路径、都 end_with("data")），只有**真实部署**
    /// 才会暴露：升级后平台数 0、账号全空，而旧数据仍在 Local 下。
    #[test]
    fn windows_data_dir_stays_under_localappdata() {
        #[cfg(windows)]
        {
            let Some(local) = std::env::var_os("LOCALAPPDATA") else {
                return; // 环境缺失时这条断言无意义
            };
            let want = PathBuf::from(local).join(APP).join("data");
            assert_eq!(
                data_dir(),
                want,
                "Windows 数据目录必须留在 %LOCALAPPDATA%\\aigw2\\data（历史数据在这里）"
            );
        }
    }

    /// 迁移只补缺、不覆盖：当前目录已有同名文件时必须以当前为准。
    #[test]
    fn migration_never_overwrites_existing_files() {
        // 这条测试不碰真实目录：它只验证 legacy_data_dirs 不会把当前目录也算进去
        // （那样「迁移」就会变成自己抄自己，甚至覆盖当前值）。
        for d in legacy_data_dirs() {
            assert_ne!(d, data_dir(), "legacy 目录不能等于当前数据目录");
        }
    }
}
