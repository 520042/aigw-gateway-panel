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
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
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

    /// 是/否二次确认，焦点默认在「否」。删除整目录这类不可逆操作必须问一句。
    fn confirm(title: &str, text: &str) -> bool {
        let wtitle = to_wide(title);
        let wtext = to_wide(text);
        let rc = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wtext.as_ptr(),
                wtitle.as_ptr(),
                MB_YESNO | MB_ICONQUESTION,
            )
        };
        // IDYES == 6（windows-sys 各版本里常量类型不一致，用字面量最稳）
        rc == 6
    }

    fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
        copy_dir_inner(src, dst, 0)
    }

    /// 递归复制，带深度上限。
    ///
    /// ★ 旧实现没有环检测：providers/ 里一个指向父目录的 junction 就会无限递归，
    ///   直到路径长度超限才报错。providers/ 实际是扁平目录，8 层足够。
    fn copy_dir_inner(src: &Path, dst: &Path, depth: u32) -> std::io::Result<()> {
        if depth > 8 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("目录层级过深（疑似 junction 循环）：{}", src.display()),
            ));
        }
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let target = dst.join(entry.file_name());
            if ty.is_dir() {
                copy_dir_inner(&entry.path(), &target, depth + 1)?;
            } else {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }

    fn desktop_lnk() -> PathBuf {
        // ★ 不能硬编码 USERPROFILE\Desktop：开了 OneDrive 桌面重定向的机器上真实桌面
        //   在 %USERPROFILE%\OneDrive\Desktop，写错位置 = 装完没有桌面图标（且失败被忽略）。
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
        let plain = PathBuf::from(&home).join("Desktop");
        let desk = if plain.is_dir() {
            plain
        } else {
            let od = PathBuf::from(&home).join("OneDrive").join("Desktop");
            if od.is_dir() { od } else { plain }
        };
        desk.join("aigw v2 网关.lnk")
    }

    fn startmenu_lnk() -> PathBuf {
        let appd = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(appd).join(r"Microsoft\Windows\Start Menu\Programs\aigw v2 网关.lnk")
    }

    /// PowerShell 单引号字符串内的转义（单引号翻倍）。
    ///
    /// ★ 旧实现直接把路径插进 `'{...}'`：安装路径里有 `'`（用户名如 O'Brien）就会
    ///   破坏脚本文法，轻则快捷方式建不出来，重则执行到被截断/拼接后的语句。
    fn ps_quote(s: &str) -> String {
        s.replace('\'', "''")
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
            lnk = ps_quote(&lnk.display().to_string()),
            target = ps_quote(&target.display().to_string()),
            args = ps_quote(args),
            workdir = ps_quote(
                &target
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            desc = ps_quote(APP_NAME),
        );
        let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
        bytes.extend_from_slice(ps.as_bytes());
        // ★ 旧实现临时脚本名可预测（aigw2_install_<pid>.ps1）且 fs::write 会覆盖：
        //   本机另一个进程可以预先放一个同名文件，安装时就被以 ExecutionPolicy Bypass
        //   执行。这里用 create_new + 纳秒后缀，抢不到就放弃（快捷方式不是必需功能）。
        let tmp = std::env::temp_dir().join(format!(
            "aigw2_install_{}_{}.ps1",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("无法创建快捷方式脚本（{e}），跳过：{}", lnk.display());
                return;
            }
        };
        use std::io::Write;
        if f.write_all(&bytes).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        drop(f);
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &tmp.to_string_lossy()])
            .status();
        let _ = std::fs::remove_file(&tmp);
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
                // ★ 必须带引号：安装路径在用户名带空格时会含空格，Run 命令行会被拆成多段，
                //   Windows 于是去找前面那一截 exe（可被同名文件劫持），或直接启动失败，
                //   表现为「自启静默失效」。
                let q = char::from(34u8);
                let quoted = format!("{q}{exe_path}{q}");
                let mut data: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = (data.len() * 2) as u32;
                let rc = RegSetValueExW(
                    hkey,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_mut_ptr().cast(),
                    bytes,
                );
                if rc != 0 {
                    // 旧实现忽略返回值：写注册表失败（权限/杀软拦截）时用户完全看不到，
                    // 唯一表现是「下次开机没起来」。
                    tracing::warn!("写自启注册表失败（rc={rc}）：{quoted}");
                }
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
        // ★ 直接比较 PathBuf 是逐字节比较：大小写不敏感的文件系统上同一目录换个大小写
        //   就会被判成两个不同目录，于是 exe 被复制到它自己身上（fs::copy 报错且返回值
        //   被忽略），装完还是旧版。
        let already = match (src_dir.canonicalize(), inst.canonicalize()) {
            (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
            _ => src_dir == inst,
        };

        // 安装过程中的「非致命但用户必须知道」的问题累积到这里，最后一起弹窗。
        let mut warn_note = String::new();

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
                // ★ 不能静默忽略：providers/ 复制失败时 exe 装好了、服务也起得来，
                //   但平台清单是空的 —— 面板上「一个平台都没有」，用户看到的却是
                //   「安装完成」，只能自己猜哪里错了。
                if let Err(e) = copy_dir_all(&src_providers, &inst.join("providers")) {
                    tracing::warn!("安装：复制 providers 失败（{e}）");
                    warn_note.push_str(&format!(
                        "\n\n⚠ 平台配置（providers/）复制失败：{e}\n服务能启动，但平台清单会是空的。请手动把源目录的 providers/ 拷到 {} 后重启。",
                        inst.join("providers").display()
                    ));
                }
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
        // 非致命问题单独弹一次：安装完成但平台清单为空这类事必须让用户看见。
        if !warn_note.is_empty() {
            msgbox(APP_NAME, &format!("安装过程中有以下问题：{warn_note}"), true);
        }
        // ★ 启动失败要告诉用户：旧实现 `let _ = ...spawn()` 把错误吞掉，安装完
        //   窗口一关什么都没有发生，用户以为程序坏了（实际可能是杀软拦截）。
        if let Err(e) = std::process::Command::new(&installed_exe).spawn() {
            tracing::warn!("安装：启动服务失败（{e}）");
            msgbox(
                APP_NAME,
                &format!(
                    "安装已完成，但自动启动失败：{e}\n\n请手动双击桌面的「aigw v2 网关」图标（或运行 {}）。",
                    installed_exe.display()
                ),
                true,
            );
        }
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
        // ★ 旧实现把路径拼进 cmd.exe /c rmdir /s /q：只做了 replace('"', "") 这一层
        //   自制过滤，路径里的 %VAR% 还会被 cmd 二次展开；更糟的是 install_dir() 在
        //   LOCALAPPDATA 缺失时会退化成相对路径 "aigw2"，rmdir 就会删掉「当前工作目录」
        //   下的同名目录。这里改为纯 Rust 删除：先断言目标确实是绝对路径 + 目录名 aigw2
        //   + 确实是目录，再弹一次确认，最后 remove_dir_all（不经 cmd，无二次展开）。
        let inst = match inst.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                msgbox(
                    "清理已取消",
                    &format!("无法解析安装目录 {}：{e}", inst.display()),
                    true,
                );
                return 1;
            }
        };
        let name_ok = inst
            .file_name()
            .map(|n| n.to_string_lossy().eq_ignore_ascii_case("aigw2"))
            .unwrap_or(false);
        if !inst.is_absolute() || !name_ok || !inst.is_dir() {
            msgbox(
                "清理已取消",
                &format!(
                    "目标目录不像 aigw2 安装目录，已取消删除：{}{}{}（期望：绝对路径，且目录名就是 aigw2）",
                    inst.display(),
                    char::from(10),
                    char::from(10)
                ),
                true,
            );
            return 1;
        }
        let q = char::from(34u8);
        let nl = char::from(10);
        if !confirm(
            "彻底卸载确认",
            &format!("{q}彻底卸载{q}会删除整个目录（含数据/凭据）：{nl}{nl}{}{nl}{nl}继续？", inst.display()),
        ) {
            msgbox(APP_NAME, "已取消清理，没有删除任何文件。", false);
            return 0;
        }
        match std::fs::remove_dir_all(&inst) {
            Ok(()) => msgbox(
                APP_NAME,
                &format!("已彻底删除：{nl}{}", inst.display()),
                false,
            ),
            Err(e) => msgbox(
                "部分文件未能删除",
                &format!(
                    "{}{}{}（服务可能还在运行：先退出 aigw.exe 再手动删除该目录）",
                    e,
                    nl,
                    nl
                ),
                true,
            ),
        }
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
