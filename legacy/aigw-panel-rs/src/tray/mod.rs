//! 系统托盘（Windows）。
//!
//! 移植自 Python `app/tray.py`（纯 Win32，无第三方 GUI 依赖）：
//!   - 托盘图标 + 右键菜单：打开面板 / 开机自启动(✓) / 退出
//!   - 双击图标打开面板
//!   - 开机自启写 HKCU `...\CurrentVersion\Run` 的 `AigwPanel` = `"<exe>" --tray`
//!   - explorer 重启（WM_TASKBARCREATED）后自动重挂图标
//!
//! 非 Windows 目标走空实现（服务端逻辑与平台无关）。
//! ⚠ 关键坑（照抄 Python 的经验）：**不要设 NOTIFYICON_VERSION_4**，否则
//!   lParam 变成 NIN_* 通知码、真实鼠标消息被挪到 uParam 低位，右键判定失效。
//!   v1 语义下 lParam 就是 WM_RBUTTONUP，行为最直接。

/// 启动托盘（Windows 显示托盘图标）。`url` 为「打开面板」跳转地址。
/// 返回是否成功挂上图标。
#[cfg(windows)]
pub fn start(url: &str) -> bool {
    win::start(url)
}

#[cfg(not(windows))]
pub fn start(_url: &str) -> bool {
    false
}

/// 开机自启是否已开。
#[cfg(windows)]
pub fn is_autostart_enabled() -> bool {
    win::is_autostart_enabled()
}

#[cfg(not(windows))]
pub fn is_autostart_enabled() -> bool {
    false
}

/// 设置开机自启（写/删 HKCU Run 项）。返回 (ok, 说明)。
#[cfg(windows)]
pub fn set_autostart(enable: bool) -> (bool, String) {
    win::set_autostart(enable)
}

#[cfg(not(windows))]
pub fn set_autostart(_enable: bool) -> (bool, String) {
    (false, "仅 Windows 支持".to_string())
}

/// 设置开机自启（指定 exe 路径，安装模式用：指向安装位而非当前 exe）。
#[cfg(windows)]
pub fn set_autostart_for(exe_path: &str, enable: bool) -> (bool, String) {
    win::set_autostart_for(exe_path, enable)
}

#[cfg(not(windows))]
pub fn set_autostart_for(_exe_path: &str, _enable: bool) -> (bool, String) {
    (false, "仅 Windows 支持".to_string())
}

#[cfg(windows)]
mod win {
    use std::sync::atomic::{AtomicIsize, AtomicBool, Ordering};
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM, HINSTANCE};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_QUERY_VALUE, REG_SZ,
    };
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW, LoadImageW, PostMessageW,
        PostQuitMessage, RegisterClassW, SetForegroundWindow, TranslateMessage, TrackPopupMenu,
        MSG, WNDCLASSW, WNDPROC,
    };

    const WM_DESTROY: u32 = 0x0002;
    const WM_NULL: u32 = 0x0000;
    const WM_COMMAND: u32 = 0x0111;
    const WM_APP: u32 = 0x8000;
    const WM_TRAY: u32 = WM_APP + 1;
    const WM_TASKBARCREATED: u32 = WM_APP + 2;
    const WM_LBUTTONDBLCLK: u32 = 0x0203;
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_CONTEXTMENU: u32 = 0x007B;

    const NIF_MESSAGE_V: u32 = NIF_MESSAGE;
    const NIF_ICON_V: u32 = NIF_ICON;
    const NIF_TIP_V: u32 = NIF_TIP;

    const TPM_RIGHTBUTTON: u32 = 0x0002;
    const MF_STRING: u32 = 0x0000;
    const MF_SEPARATOR: u32 = 0x0008_00;
    const MF_CHECKED: u32 = 0x0000_0008;

    const ID_OPEN: usize = 1001;
    const ID_AUTOSTART: usize = 1004;
    const ID_EXIT: usize = 1099;
    const ID_TRAY: u32 = 1;

    const APP_ID: &str = "AigwPanel.AIResourceHub";

    const IMAGE_ICON: u32 = 1;
    const LR_LOADFROMFILE: u32 = 0x0000_0010;
    const LR_DEFAULTSIZE: u32 = 0x0000_0040;

    static URL: OnceLock<String> = OnceLock::new();
    static HWND_STORE: AtomicIsize = AtomicIsize::new(0);
    static RUNNING: AtomicBool = AtomicBool::new(false);

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_arr<const N: usize>(s: &str) -> [u16; N] {
        let mut a = [0u16; N];
        let w: Vec<u16> = s.encode_utf16().take(N - 1).collect();
        a[..w.len()].copy_from_slice(&w);
        a
    }

    fn current_exe() -> String {
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    fn ico_candidates() -> Vec<std::path::PathBuf> {
        let mut v = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                v.push(dir.join("aigw.ico"));
                v.push(dir.join("app/static/aigw.ico"));
                v.push(dir.join("../app/static/aigw.ico"));
            }
        }
        v.push(std::path::PathBuf::from("app/static/aigw.ico"));
        v
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            m if m == WM_TRAY => {
                let mut mouse = lparam as u32;
                let code = mouse & 0xFFFF;
                if !(0x0200..=0x0210).contains(&code) {
                    mouse = (wparam & 0xFFFF) as u32;
                }
                let c = mouse & 0xFFFF;
                if c == WM_LBUTTONDBLCLK {
                    open_panel();
                } else if c == WM_RBUTTONUP || c == WM_CONTEXTMENU {
                    show_menu(hwnd);
                }
                0
            }
            m if m == WM_TASKBARCREATED => {
                add_icon(hwnd);
                0
            }
            m if m == WM_COMMAND => {
                let cmd = (wparam & 0xFFFF) as usize;
                if cmd == ID_OPEN {
                    open_panel();
                } else if cmd == ID_AUTOSTART {
                    let (ok, msg) = set_autostart(!is_autostart_enabled());
                    tracing::info!(ok, msg, "托盘：切换开机自启动");
                    let _ = msg;
                } else if cmd == ID_EXIT {
                    tracing::info!("托盘：退出面板");
                    std::process::exit(0);
                }
                0
            }
            m if m == WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    fn open_panel() {
        if let Some(url) = URL.get() {
            crate::server::open_browser(url);
        }
    }

    unsafe fn show_menu(hwnd: HWND) {
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING, ID_OPEN, to_wide("打开面板").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        let flags = MF_STRING | if is_autostart_enabled() { MF_CHECKED } else { 0 };
        AppendMenuW(menu, flags, ID_AUTOSTART, to_wide("开机自启动").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_EXIT, to_wide("退出").as_ptr());

        let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
        // 关键：TrackPopupMenu 返回后必须发 WM_NULL，否则菜单一闪就消失（经典坑）。
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }

    unsafe fn load_icon() -> windows_sys::Win32::Foundation::HANDLE {
        for p in ico_candidates() {
            if p.is_file() {
                let w = to_wide(&p.display().to_string());
                let h = LoadImageW(
                    std::ptr::null_mut(),
                    w.as_ptr(),
                    IMAGE_ICON,
                    0,
                    0,
                    LR_LOADFROMFILE | LR_DEFAULTSIZE,
                );
                if !h.is_null() {
                    return h;
                }
            }
        }
        // 兜底：系统默认应用图标 IDI_APPLICATION=32512
        LoadIconW(std::ptr::null_mut(), 32512 as *const u16) as windows_sys::Win32::Foundation::HANDLE
    }

    unsafe fn add_icon(hwnd: HWND) {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = ID_TRAY;
        nid.uCallbackMessage = WM_TRAY;
        nid.uFlags = NIF_MESSAGE_V | NIF_ICON_V | NIF_TIP_V;
        nid.hIcon = load_icon();
        nid.szTip = wide_arr::<128>("AI 资源整合网关面板");
        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
            tracing::warn!("托盘：Shell_NotifyIconW(NIM_ADD) 失败");
        }
    }

    fn loop_body(url: String) {
        URL.set(url).ok();
        unsafe {
            let hinst: HINSTANCE = GetModuleHandleW(std::ptr::null());
            let cls = to_wide(&format!("{}.TrayWnd", APP_ID));
            let title = to_wide("AI 资源整合网关面板");

            let proc: WNDPROC = Some(wndproc);
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = proc;
            wc.hInstance = hinst;
            wc.lpszClassName = cls.as_ptr();
            wc.hbrBackground = std::ptr::null_mut();
            wc.hCursor = LoadIconW(std::ptr::null_mut(), 32512 as *const u16);
            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                0,
                cls.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                tracing::warn!("托盘：CreateWindowExW 失败");
                RUNNING.store(false, Ordering::SeqCst);
                return;
            }
            HWND_STORE.store(hwnd as isize, Ordering::SeqCst);
            add_icon(hwnd);
            tracing::info!("系统托盘已启动（右键：打开面板 / 开机自启动 / 退出）");

            let mut msg: MSG = std::mem::zeroed();
            loop {
                let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if r == 0 || r == -1 {
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // 退出清理
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = ID_TRAY;
            Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    pub fn start(url: &str) -> bool {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return true;
        }
        let url = url.to_string();
        std::thread::Builder::new()
            .name("aigw-tray".to_string())
            .spawn(move || loop_body(url))
            .is_ok()
    }

    // ------------------------------------------------------------------ 注册表自启
    const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "AigwPanel";

    pub fn is_autostart_enabled() -> bool {
        unsafe {
            let sub = to_wide(RUN_SUBKEY);
            let name = to_wide(VALUE_NAME);
            let mut hkey = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey) != 0 {
                return false;
            }
            let mut ty = 0u32;
            let mut size = 0u32;
            let r = RegQueryValueExW(hkey, name.as_ptr(), std::ptr::null(), &mut ty, std::ptr::null_mut(), &mut size);
            RegCloseKey(hkey);
            r == 0
        }
    }

    pub fn set_autostart(enable: bool) -> (bool, String) {
        set_autostart_for(&current_exe(), enable)
    }

    /// 与 set_autostart 同逻辑，但自启项指向调用方指定的 exe 路径
    /// （安装模式：指向 %LOCALAPPDATA%\aigw-panel-rs\aigw-panel.exe，而非源 exe）。
    pub fn set_autostart_for(exe_path: &str, enable: bool) -> (bool, String) {
        unsafe {
            let sub = to_wide(RUN_SUBKEY);
            let name = to_wide(VALUE_NAME);
            let mut hkey = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_ALL_ACCESS, &mut hkey) != 0 {
                return (false, "打开注册表失败".to_string());
            }
            if enable {
                let val = to_wide(&format!("\"{}\" --tray", exe_path));
                let bytes = std::slice::from_raw_parts(
                    val.as_ptr() as *const u8,
                    val.len() * 2,
                );
                let r = RegSetValueExW(hkey, name.as_ptr(), 0, REG_SZ, bytes.as_ptr(), bytes.len() as u32);
                RegCloseKey(hkey);
                if r == 0 {
                    (true, "已加入开机自启动".to_string())
                } else {
                    (false, format!("写注册表失败（code {}）", r))
                }
            } else {
                let r = RegDeleteValueW(hkey, name.as_ptr());
                RegCloseKey(hkey);
                if r == 0 {
                    (true, "已取消开机自启动".to_string())
                } else {
                    (true, "本就未设置".to_string())
                }
            }
        }
    }
}
