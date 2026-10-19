//! 自检安装模式（对齐 dsh-our-free-model 的 NSIS setup.exe 体验）。
//!
//! 项目本就是单 EXE 自洽（exe + app/static + 可选 webpow），因此「安装」=
//! 把当前 exe 连同静态资源复制到 %LOCALAPPDATA%\aigw-panel-rs\，再建
//! 开始菜单 / 桌面快捷方式 + 写 HKCU 自启。数据目录天然落在
//! <install>/data（config::data_dir 取 exe 同级），重装/升级不丢数据。
//!
//! 用法：
//!   aigw-panel.exe --install     复制 + 快捷方式 + 自启，结束后启动面板
//!   aigw-panel.exe --uninstall   删快捷方式 + 自启（保留数据）
//!   aigw-panel.exe --purge       卸载 + 删除数据目录（彻底清理）
//!
//! 设计取舍：不引入 NSIS/makensis 依赖（本机无），纯 Rust + 少量 PowerShell
//! 建 .lnk（COM WScript.Shell）。可选 NSIS 脚本见 installer/aigw-panel.nsi。

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };

    const APP_NAME: &str = "AI 资源网关面板";

    /// 安装根目录：%LOCALAPPDATA%\aigw-panel-rs
    pub fn install_dir() -> PathBuf {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local).join("aigw-panel-rs")
        } else {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".aigw-panel-rs")
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 二进制是 windows_subsystem（无控制台），用 MessageBox 给用户可见反馈。
    fn msgbox(title: &str, text: &str, error: bool) {
        let wtitle = to_wide(title);
        let wtext = to_wide(text);
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wtext.as_ptr(),
                wtitle.as_ptr(),
                if error {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                } | MB_OK,
            );
        }
    }

    /// 递归复制目录（覆盖式）。
    fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let target = dst.join(entry.file_name());
            if ty.is_dir() {
                copy_dir_all(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }

    fn desktop_lnk() -> PathBuf {
        let desk = std::env::var("USERPROFILE")
            .map(|h| PathBuf::from(h).join("Desktop"))
            .unwrap_or_else(|_| PathBuf::from("."));
        desk.join("AI 资源网关面板.lnk")
    }

    fn startmenu_lnk() -> PathBuf {
        let appd = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(appd)
            .join(r"Microsoft\Windows\Start Menu\Programs\AI 资源网关面板.lnk")
    }

    /// 用 PowerShell COM WScript.Shell 建 .lnk（带 BOM 避免中文乱码）。
    fn create_shortcut(lnk: &Path, target: &Path, args: &str, icon: &Path) {
        let ps = format!(
            "$ws = New-Object -ComObject WScript.Shell\n\
             $s = $ws.CreateShortcut('{lnk}')\n\
             $s.TargetPath = '{target}'\n\
             $s.Arguments = '{args}'\n\
             $s.WorkingDirectory = '{workdir}'\n\
             $s.IconLocation = '{icon}'\n\
             $s.Description = '{desc}'\n\
             $s.Save()\n",
            lnk = lnk.display(),
            target = target.display(),
            args = args,
            workdir = target
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            icon = icon.display(),
            desc = APP_NAME,
        );
        let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
        bytes.extend_from_slice(ps.as_bytes());
        let tmp = std::env::temp_dir().join(format!("aigw_install_{}.ps1", std::process::id()));
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    &tmp.to_string_lossy(),
                ])
                .status();
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn run_install() -> i32 {
        let src_exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                msgbox("安装失败", &format!("无法定位当前 exe：{e}"), true);
                return 1;
            }
        };
        let src_dir = match src_exe.parent() {
            Some(d) => d.to_path_buf(),
            None => {
                msgbox("安装失败", "无法定位当前 exe 所在目录", true);
                return 1;
            }
        };
        let inst = install_dir();
        let already = src_dir == inst;

        if !already {
            if let Err(e) = std::fs::create_dir_all(&inst) {
                msgbox(
                    "安装失败",
                    &format!("无法创建安装目录 {}：{e}", inst.display()),
                    true,
                );
                return 1;
            }
            let dst_exe = inst.join("aigw-panel.exe");
            if let Err(e) = std::fs::copy(&src_exe, &dst_exe) {
                msgbox(
                    "安装失败",
                    &format!(
                        "复制 exe 失败：{e}\n（若已运行安装目录中的程序，请先退出再装）"
                    ),
                    true,
                );
                return 1;
            }
            // 同步前端静态资源（三处来源，复制整目录最稳）
            let src_static = src_dir.join("app/static");
            if src_static.is_dir() {
                let _ = copy_dir_all(&src_static, &inst.join("app/static"));
            }
            // 可选：deepseek POW（webpow）
            let src_webpow = src_dir.join("webpow");
            if src_webpow.is_dir() {
                let _ = copy_dir_all(&src_webpow, &inst.join("webpow"));
            }
        }

        let installed_exe = inst.join("aigw-panel.exe");
        let icon = inst.join("app/static/aigw.ico");
        let icon_loc = if icon.is_file() {
            icon
        } else {
            installed_exe.clone()
        };
        // 桌面 / 开始菜单：双击直接开面板+托盘（不带 --tray）
        create_shortcut(&desktop_lnk(), &installed_exe, "", &icon_loc);
        create_shortcut(&startmenu_lnk(), &installed_exe, "", &icon_loc);
        // 开机自启：指向安装位 --tray（登录不弹浏览器，只挂托盘）
        let inst_exe_s = installed_exe.to_string_lossy().into_owned();
        crate::tray::set_autostart_for(&inst_exe_s, true);

        let msg = format!(
            "安装完成：\n{}\n\n· 桌面 / 开始菜单 已建立快捷方式\n· 已加入开机自启动（托盘右键可关闭）\n· 数据目录：{}\\data（重装不丢）\n\n点击「确定」启动面板。",
            inst.display(),
            inst.display()
        );
        msgbox(APP_NAME, &msg, false);

        // 启动已安装的 exe（安装在自身进程里，让新进程接管）
        let _ = std::process::Command::new(&installed_exe).spawn();
        0
    }

    pub fn run_uninstall() -> i32 {
        let inst = install_dir();
        let _ = std::fs::remove_file(desktop_lnk());
        let _ = std::fs::remove_file(startmenu_lnk());
        let inst_exe_s = inst.join("aigw-panel.exe").to_string_lossy().into_owned();
        crate::tray::set_autostart_for(&inst_exe_s, false);
        let data = inst.join("data");
        let data_note = if data.exists() {
            format!(
                "\n\n数据目录已保留：{}\n（如需彻底删除，请用 --purge）",
                data.display()
            )
        } else {
            String::new()
        };
        msgbox(
            APP_NAME,
            &format!("已卸载（快捷方式 / 自启已移除）。{}", data_note),
            false,
        );
        0
    }

    pub fn run_purge() -> i32 {
        let inst = install_dir();
        let _ = std::fs::remove_file(desktop_lnk());
        let _ = std::fs::remove_file(startmenu_lnk());
        let inst_exe_s = inst.join("aigw-panel.exe").to_string_lossy().into_owned();
        crate::tray::set_autostart_for(&inst_exe_s, false);
        if !inst.exists() {
            msgbox(APP_NAME, "未找到安装目录，无需清理。", false);
            return 0;
        }
        // 当前进程可能正从安装目录运行 → 直接删会因占用失败，交给延迟 cmd 删除。
        let inst_s = inst.to_string_lossy().replace('"', "");
        let cmd = format!(
            "timeout /t 1 /nobreak >nul & rmdir /s /q \"{}\" >nul 2>&1",
            inst_s
        );
        let _ = std::process::Command::new("cmd.exe")
            .args(["/c", &cmd])
            .spawn();
        msgbox(
            APP_NAME,
            &format!(
                "正在彻底卸载并删除：\n{}\n\n（若面板正在运行，将于退出后自动清理）",
                inst.display()
            ),
            false,
        );
        0
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
pub fn run_install() -> i32 {
    eprintln!("仅 Windows 支持安装模式");
    1
}
#[cfg(not(windows))]
pub fn run_uninstall() -> i32 {
    eprintln!("仅 Windows 支持安装模式");
    1
}
#[cfg(not(windows))]
pub fn run_purge() -> i32 {
    eprintln!("仅 Windows 支持安装模式");
    1
}
