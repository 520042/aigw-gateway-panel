//! 自检安装模式（移植旧项目 install.rs，路径/名称适配 aigw2）。
//!
//! 用法：
//!   aigw.exe --install     复制 exe+providers 到 %LOCALAPPDATA%\aigw2\ + 快捷方式 + 自启，结束启动服务
//!   aigw.exe --uninstall   删快捷方式 + 自启（保留数据）
//!   aigw.exe --purge       卸载 + 延迟删除整目录（彻底清理）
//!
//! v2 前端 rust-embed 内嵌，无需复制静态资源；providers/ 随安装复制。

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };

    const APP_NAME: &str = "aigw v2 网关";
    const RUN_VALUE: &str = "Aigw2";

    pub fn install_dir() -> PathBuf {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local).join("aigw2")
        } else {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".aigw2")
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn msgbox(title: &str, text: &str, error: bool) {
        let wtitle = to_wide(title);
        let wtext = to_wide(text);
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wtext.as_ptr(),
                wtitle.as_ptr(),
                if error { MB_ICONERROR } else { MB_ICONINFORMATION } | MB_OK,
            );
        }
    }

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
        desk.join("aigw v2 网关.lnk")
    }

    fn startmenu_lnk() -> PathBuf {
        let appd = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(appd).join(r"Microsoft\Windows\Start Menu\Programs\aigw v2 网关.lnk")
    }

    fn create_shortcut(lnk: &Path, target: &Path, args: &str) {
        let ps = format!(
            "$ws = New-Object -ComObject WScript.Shell\n\
             $s = $ws.CreateShortcut('{lnk}')\n\
             $s.TargetPath = '{target}'\n\
             $s.Arguments = '{args}'\n\
             $s.WorkingDirectory = '{workdir}'\n\
             $s.Description = '{desc}'\n\
             $s.Save()\n",
            lnk = lnk.display(),
            target = target.display(),
            args = args,
            workdir = target
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            desc = APP_NAME,
        );
        let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
        bytes.extend_from_slice(ps.as_bytes());
        let tmp = std::env::temp_dir().join(format!("aigw2_install_{}.ps1", std::process::id()));
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &tmp.to_string_lossy()])
                .status();
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// HKCU\...\Run 自启直写注册表（v2 无托盘，直接指 exe）。
    fn set_autostart(exe_path: &str, enable: bool) {
        unsafe {
            let sub = to_wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
            let mut hkey = std::ptr::null_mut();
            if RegCreateKeyExW(
                HKEY_CURRENT_USER,
                sub.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            ) != 0
            {
                return;
            }
            if enable {
                let name = to_wide(RUN_VALUE);
                let mut data: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = (data.len() * 2) as u32;
                RegSetValueExW(
                    hkey,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_mut_ptr().cast(),
                    bytes,
                );
            } else {
                let name = to_wide(RUN_VALUE);
                RegDeleteValueW(hkey, name.as_ptr());
            }
            RegCloseKey(hkey);
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
                msgbox("安装失败", &format!("无法创建 {}：{e}", inst.display()), true);
                return 1;
            }
            let dst_exe = inst.join("aigw.exe");
            if let Err(e) = std::fs::copy(&src_exe, &dst_exe) {
                msgbox(
                    "安装失败",
                    &format!("复制 exe 失败：{e}\n（若已运行安装目录中的程序，请先退出再装）"),
                    true,
                );
                return 1;
            }
            let src_providers = src_dir.join("providers");
            if src_providers.is_dir() {
                let _ = copy_dir_all(&src_providers, &inst.join("providers"));
            }
        }

        let installed_exe = inst.join("aigw.exe");
        create_shortcut(&desktop_lnk(), &installed_exe, "--port 8800");
        create_shortcut(&startmenu_lnk(), &installed_exe, "--port 8800");
        set_autostart(&installed_exe.to_string_lossy(), true);

        msgbox(
            APP_NAME,
            &format!(
                "安装完成：\n{}\n\n· 桌面 / 开始菜单快捷方式已建立\n· 已加入开机自启\n· 数据目录：{}\\data（重装不丢）\n\n点击「确定」启动服务（127.0.0.1:8800）。",
                inst.display(),
                inst.display()
            ),
            false,
        );
        let _ = std::process::Command::new(&installed_exe).spawn();
        0
    }

    pub fn run_uninstall() -> i32 {
        let inst = install_dir();
        let _ = std::fs::remove_file(desktop_lnk());
        let _ = std::fs::remove_file(startmenu_lnk());
        set_autostart(&inst.join("aigw.exe").to_string_lossy(), false);
        let data = inst.join("data");
        let note = if data.exists() {
            format!("\n\n数据已保留：{}（彻底删除用 --purge）", data.display())
        } else {
            String::new()
        };
        msgbox(APP_NAME, &format!("已卸载（快捷方式 / 自启移除）。{note}"), false);
        0
    }

    pub fn run_purge() -> i32 {
        let inst = install_dir();
        let _ = std::fs::remove_file(desktop_lnk());
        let _ = std::fs::remove_file(startmenu_lnk());
        set_autostart(&inst.join("aigw.exe").to_string_lossy(), false);
        if !inst.exists() {
            msgbox(APP_NAME, "未找到安装目录，无需清理。", false);
            return 0;
        }
        let inst_s = inst.to_string_lossy().replace('"', "");
        let cmd = format!("timeout /t 1 /nobreak >nul & rmdir /s /q \"{inst_s}\" >nul 2>&1");
        let _ = std::process::Command::new("cmd.exe").args(["/c", &cmd]).spawn();
        msgbox(
            APP_NAME,
            &format!("正在彻底卸载并删除：\n{inst_s}\n\n（若服务在运行，退出后自动清理）"),
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
