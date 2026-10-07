# -*- coding: utf-8 -*-
"""
Windows 托盘
============
纯 ctypes 实现系统托盘图标，不依赖 pystray/Pillow（打包体积敏感）。

提供：
  TrayIcon        系统托盘图标 + 右键菜单
  SingleInstance  单实例互斥，第二次启动唤醒第一个
  TaskbarAppId    设置任务栏 AppUserModelID（图标正常显示的关键）

菜单项：
  打开面板        浏览器打开
  ─────
  启动网关
  停止网关
  ─────
  开机自启动  ✓   写入/删除 Run 注册表项
  ─────
  退出

托盘图标可双击打开面板。
"""

import ctypes
import os
import sys
import threading
import webbrowser

# 顶层常量：SingleInstance 的默认互斥名在类定义时即求值，
# 必须跨平台可见，否则非 Windows 启动直接 NameError。
APP_ID = "AigwPanel.AIResourceHub"

IS_WINDOWS = sys.platform == "win32"

if IS_WINDOWS:
    import ctypes.wintypes as wt

    # ---------------------------------------------------------------- 常量
    WM_DESTROY = 0x0002
    WM_NULL = 0x0000
    WM_COMMAND = 0x0111
    WM_APP = 0x8000
    WM_TRAY = WM_APP + 1
    WM_TASKBARCREATED = WM_APP + 2
    WM_LBUTTONDBLCLK = 0x0203
    WM_RBUTTONUP = 0x0205
    WM_CONTEXTMENU = 0x007B
    WM_SETICON = 0x0080

    NIM_ADD = 0x00000000
    NIM_MODIFY = 0x00000001
    NIM_DELETE = 0x00000002
    NIM_SETVERSION = 0x00000004

    NIF_MESSAGE = 0x00000001
    NIF_ICON = 0x00000002
    NIF_TIP = 0x00000004
    NIF_INFO = 0x00000010

    NOTIFYICON_VERSION_4 = 4

    TPM_RIGHTBUTTON = 0x0002
    MF_STRING = 0x00000000
    MF_SEPARATOR = 0x00000800
    MF_CHECKED = 0x00000008

    HICON = ctypes.c_void_p
    HINSTANCE = ctypes.c_void_p
    HBRUSH = ctypes.c_void_p
    WPARAM = ctypes.c_size_t
    LPARAM = ctypes.c_ssize_t
    LRESULT = ctypes.c_ssize_t
    WNDPROC = ctypes.WINFUNCTYPE(LRESULT, wt.HWND, ctypes.c_uint,
                                 WPARAM, LPARAM)

    class WNDCLASSW(ctypes.Structure):
        _fields_ = [
            ("style", ctypes.c_uint),
            ("lpfnWndProc", WNDPROC),
            ("cbClsExtra", ctypes.c_int),
            ("cbWndExtra", ctypes.c_int),
            ("hInstance", HINSTANCE),
            ("hIcon", HICON),
            ("hCursor", HICON),
            ("hbrBackground", HBRUSH),
            ("lpszMenuName", ctypes.c_wchar_p),
            ("lpszClassName", ctypes.c_wchar_p),
        ]

    user32 = ctypes.windll.user32
    kernel32 = ctypes.windll.kernel32
    shell32 = ctypes.windll.shell32

    CreateWindowExW = user32.CreateWindowExW
    CreateWindowExW.restype = wt.HWND
    CreateWindowExW.argtypes = [ctypes.c_ulong, ctypes.c_wchar_p,
                                ctypes.c_wchar_p, ctypes.c_ulong,
                                ctypes.c_int, ctypes.c_int,
                                ctypes.c_int, ctypes.c_int,
                                wt.HWND, wt.HMENU, HINSTANCE, ctypes.c_void_p]
    DestroyWindow = user32.DestroyWindow
    DefWindowProcW = user32.DefWindowProcW
    DefWindowProcW.restype = ctypes.c_ssize_t
    DefWindowProcW.argtypes = [wt.HWND, ctypes.c_uint, WPARAM, LPARAM]
    PostQuitMessage = user32.PostQuitMessage
    LoadIconW = user32.LoadIconW
    LoadIconW.restype = HICON
    LoadIconW.argtypes = [HINSTANCE, ctypes.c_wchar_p]
    LoadImageW = user32.LoadImageW
    LoadImageW.restype = wt.HANDLE
    RegisterClassW = user32.RegisterClassW
    RegisterClassW.argtypes = [ctypes.POINTER(WNDCLASSW)]
    UnregisterClassW = user32.UnregisterClassW
    GetModuleHandleW = kernel32.GetModuleHandleW
    GetModuleHandleW.restype = wt.HMODULE
    GetModuleHandleW.argtypes = [ctypes.c_wchar_p]
    GetMessageW = user32.GetMessageW
    TranslateMessage = user32.TranslateMessage
    DispatchMessageW = user32.DispatchMessageW
    FindWindowW = user32.FindWindowW
    FindWindowW.restype = wt.HWND
    FindWindowW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p]
    UpdateWindow = user32.UpdateWindow
    ShowWindow = user32.ShowWindow
    SW_HIDE = 0
    WM_CLOSE = 0x0010

    class NOTIFYICONDATAW(ctypes.Structure):
        _fields_ = [
            ("cbSize", ctypes.c_ulong),
            ("hWnd", wt.HWND),
            ("uID", ctypes.c_uint),
            ("uFlags", ctypes.c_uint),
            ("uCallbackMessage", ctypes.c_uint),
            ("hIcon", HICON),
            ("szTip", ctypes.c_wchar * 128),
            ("dwState", ctypes.c_ulong),
            ("dwStateMask", ctypes.c_ulong),
            ("szInfo", ctypes.c_wchar * 256),
            ("uVersion", ctypes.c_uint),
            ("szInfoTitle", ctypes.c_wchar * 64),
            ("dwInfoFlags", ctypes.c_ulong),
            ("guidItem", ctypes.c_byte * 16),
            ("hBalloonIcon", HICON),
        ]

    Shell_NotifyIconW = shell32.Shell_NotifyIconW
    Shell_NotifyIconW.argtypes = [ctypes.c_ulong,
                                  ctypes.POINTER(NOTIFYICONDATAW)]
    Shell_NotifyIconW.restype = wt.BOOL

    CreatePopupMenu = user32.CreatePopupMenu
    CreatePopupMenu.restype = wt.HMENU
    AppendMenuW = user32.AppendMenuW
    AppendMenuW.argtypes = [wt.HMENU, ctypes.c_uint, ctypes.c_size_t,
                            ctypes.c_wchar_p]
    TrackPopupMenu = user32.TrackPopupMenu
    TrackPopupMenu.argtypes = [wt.HMENU, ctypes.c_uint, ctypes.c_int,
                               ctypes.c_int, ctypes.c_int, wt.HWND, ctypes.c_void_p]
    DestroyMenu = user32.DestroyMenu
    SetForegroundWindow = user32.SetForegroundWindow
    PostMessageW = user32.PostMessageW
    PostMessageW.argtypes = [wt.HWND, ctypes.c_uint, WPARAM, LPARAM]
    SetWindowTextW = user32.SetWindowTextW
    GetCursorPos = user32.GetCursorPos

    IMAGE_ICON = 1
    LR_LOADFROMFILE = 0x00000010
    LR_DEFAULTSIZE = 0x00000040

    # 菜单命令 ID
    ID_OPEN = 1001
    ID_GW_START = 1002
    ID_GW_STOP = 1003
    ID_AUTOSTART = 1004
    ID_EXIT = 1099
    ID_TRAY = 1

    RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"


def set_taskbar_appid():
    """让 Windows 把 exe 正确归组到任务栏（图标能显示的关键）"""
    if not IS_WINDOWS:
        return False
    try:
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(APP_ID)
        return True
    except Exception:
        return False


class SingleInstance:
    """命名互斥体：已存在时把信号发给先前的实例并退出"""

    def __init__(self, name=APP_ID + ".Mutex"):
        self.name = name
        self.handle = None
        self.acquired = False

    def acquire(self):
        if not IS_WINDOWS:
            self.acquired = True
            return True
        kernel32.CreateMutexW.restype = wt.HANDLE
        kernel32.CreateMutexW.argtypes = [ctypes.c_void_p, wt.BOOL,
                                          ctypes.c_wchar_p]
        kernel32.CloseHandle.argtypes = [wt.HANDLE]
        kernel32.CloseHandle.restype = wt.BOOL
        h = kernel32.CreateMutexW(None, False, self.name)
        if not h:
            return True
        self.handle = wt.HANDLE(h)
        err = kernel32.GetLastError()
        # ERROR_ALREADY_EXISTS = 183
        self.acquired = (err != 183)
        if not self.acquired:
            # 没拿到锁，句柄要立刻关掉，否则会一直占着
            kernel32.CloseHandle(self.handle)
            self.handle = None
        return self.acquired

    def find_existing_window(self):
        """找到已运行实例的托盘窗口并通知它打开面板"""
        if not IS_WINDOWS:
            return False
        hwnd = FindWindowW(APP_ID + ".TrayWnd", None)
        if hwnd:
            PostMessageW(hwnd, WM_COMMAND, ID_OPEN, 0)
            return True
        return False

    def release(self):
        if self.handle and IS_WINDOWS:
            kernel32.CloseHandle(self.handle)
            self.handle = None


def is_autostart_enabled(exe_path=None):
    if not IS_WINDOWS:
        return False
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as k:
            v, _ = winreg.QueryValueEx(k, "AigwPanel")
        return True
    except Exception:
        return False


def set_autostart(enable, exe_path=None):
    """写/删当前用户 Run 注册表项"""
    if not IS_WINDOWS:
        return False, "仅 Windows 支持"
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY,
                            0, winreg.KEY_ALL_ACCESS) as k:
            if enable:
                path = exe_path or sys.executable
                winreg.SetValueEx(k, "AigwPanel", 0, winreg.REG_SZ,
                                  '"%s" --tray' % path)
                return True, "已加入开机自启动"
            else:
                try:
                    winreg.DeleteValue(k, "AigwPanel")
                except FileNotFoundError:
                    return True, "本就未设置"
                return True, "已取消开机自启动"
    except Exception as e:
        return False, str(e)


class TrayIcon:
    """系统托盘图标"""

    def __init__(self, title="AI 资源整合网关面板",
                 icon_path=None,
                 on_open=None, on_start_gateway=None,
                 on_stop_gateway=None, on_exit=None,
                 show_autostart=True):
        self.title = title
        self.icon_path = icon_path
        self.on_open = on_open
        self.on_start_gateway = on_start_gateway
        self.on_stop_gateway = on_stop_gateway
        self.on_exit = on_exit
        self.show_autostart = show_autostart
        self.hwnd = None
        self._nid = None
        self._icon_handle = None
        self._thread = None
        self._proc = None
        self._running = False
        self._ready = threading.Event()

    # ---------------------------------------------------------- 生命周期
    def start(self):
        if not IS_WINDOWS:
            return False
        if self._running:
            return True
        self._running = True
        self._thread = threading.Thread(target=self._loop, daemon=True,
                                        name="tray")
        self._thread.start()
        self._ready.wait(timeout=8)
        return self.hwnd is not None

    def _loop(self):
        hinst = GetModuleHandleW(None)
        cls_name = APP_ID + ".TrayWnd"
        # 回调必须保持引用，否则会被 GC 掉导致窗口过程失效
        proc = WNDPROC(self._wndproc)
        self._proc = proc
        wc = WNDCLASSW()
        wc.lpfnWndProc = proc
        wc.hInstance = hinst
        wc.lpszClassName = cls_name
        wc.hbrBackground = ctypes.cast(0, HBRUSH)
        wc.hCursor = LoadIconW(None, ctypes.c_wchar_p(32512))  # IDC_ARROW
        wc.style = 0
        if not RegisterClassW(ctypes.byref(wc)):
            pass
        hwnd = CreateWindowExW(0, cls_name, self.title, 0,
                               0, 0, 0, 0, None, None, hinst, None)
        if not hwnd:
            self._running = False
            self._ready.set()
            return
        self.hwnd = hwnd
        self._add_icon()
        self._ready.set()
        # 消息循环
        msg = wt.MSG()
        while self._running:
            r = user32.GetMessageW(ctypes.byref(msg), None, 0, 0)
            if r in (0, -1):
                break
            user32.TranslateMessage(ctypes.byref(msg))
            user32.DispatchMessageW(ctypes.byref(msg))
        self._remove_icon()

    def _wndproc(self, hwnd, msg, wparam, lparam):
        if msg == WM_TRAY:
            # 鼠标消息既可能在 lParam（v1），也可能在 uParam 低位（v4）。
            # 两种都判一遍，这样不管系统/图标版本怎么变，右键都能识别。
            mouse = lparam
            if not (0x0200 <= (mouse & 0xFFFF) <= 0x0210
                    or 0x0200 <= (wparam & 0xFFFF) <= 0x0210):
                mouse = wparam & 0xFFFF
            code = mouse & 0xFFFF
            if code == WM_LBUTTONDBLCLK:
                self._open()
            elif code == WM_RBUTTONUP or code == WM_CONTEXTMENU:
                self._menu(hwnd)
            return 0
        if msg == WM_TASKBARCREATED:
            # explorer.exe 重启后要重新挂图标
            self._add_icon()
            return 0
        if msg == WM_COMMAND:
            cmd = wparam & 0xFFFF
            if cmd == ID_OPEN:
                self._open()
            elif cmd == ID_GW_START and self.on_start_gateway:
                self.on_start_gateway()
            elif cmd == ID_GW_STOP and self.on_stop_gateway:
                self.on_stop_gateway()
            elif cmd == ID_AUTOSTART:
                ok, msg2 = set_autostart(not is_autostart_enabled())
                self._toast("开机自启动", msg2)
            elif cmd == ID_EXIT and self.on_exit:
                self.on_exit()
            return 0
        if msg == WM_DESTROY:
            PostQuitMessage(0)
            return 0
        return DefWindowProcW(hwnd, msg, wparam, lparam)

    def _open(self):
        if self.on_open:
            try:
                self.on_open()
            except Exception:
                pass

    def _menu(self, hwnd):
        m = CreatePopupMenu()
        AppendMenuW(m, MF_STRING, ID_OPEN, "打开面板")
        AppendMenuW(m, MF_SEPARATOR, 0, None)
        AppendMenuW(m, MF_STRING, ID_GW_START, "启动网关")
        AppendMenuW(m, MF_STRING, ID_GW_STOP, "停止网关")
        if self.show_autostart:
            AppendMenuW(m, MF_SEPARATOR, 0, None)
            flags = MF_STRING | (MF_CHECKED if is_autostart_enabled() else 0)
            AppendMenuW(m, flags, ID_AUTOSTART, "开机自启动")
        AppendMenuW(m, MF_SEPARATOR, 0, None)
        AppendMenuW(m, MF_STRING, ID_EXIT, "退出")

        pt = wt.POINT()
        user32.GetCursorPos(ctypes.byref(pt))
        SetForegroundWindow(hwnd)
        TrackPopupMenu(m, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None)
        # 关键：TrackPopupMenu 返回后必须给窗口发一条 WM_NULL，
        # 否则系统认为菜单没有前景归属，**菜单会一闪就消失** ——
        # 用户体感就是「图标右键没反应」。这是 ctypes 托盘的经典坑。
        PostMessageW(hwnd, WM_NULL, 0, 0)
        DestroyMenu(m)

    # ---------------------------------------------------------- 图标
    def _load_icon(self):
        if not self.icon_path or not os.path.exists(self.icon_path):
            return None
        h = LoadImageW(None, self.icon_path, IMAGE_ICON, 0, 0, LR_LOADFROMFILE)
        return h or None

    def _nid_obj(self):
        nid = NOTIFYICONDATAW()
        nid.cbSize = ctypes.sizeof(NOTIFYICONDATAW)
        nid.hWnd = self.hwnd
        nid.uID = ID_TRAY
        nid.uCallbackMessage = WM_TRAY
        return nid

    def _add_icon(self):
        if not self.hwnd:
            return False
        self._icon_handle = self._load_icon()
        nid = self._nid_obj()
        if self._icon_handle:
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP
            nid.hIcon = self._icon_handle
        else:
            nid.uFlags = NIF_MESSAGE | NIF_TIP
        nid.szTip = self.title
        if not Shell_NotifyIconW(NIM_ADD, ctypes.byref(nid)):
            # 静默失败是最坑的：图标没挂上，用户完全不知道，只觉得"右键没反应"
            import sys as _sys
            _sys.stderr.write("[tray] Shell_NotifyIconW(NIM_ADD) 失败 err=%d\n"
                             % ctypes.get_last_error())
            _sys.stderr.flush()
            return False
        self._nid = nid
        # 不要设 NOTIFYICON_VERSION_4。
        # v4 会把 lParam 从「鼠标消息」改成「通知码(NIN_*)」，
        # 真实鼠标消息被挪到 uParam 低位 —— 而我们的 wndproc 是按
        # `lparam == WM_RBUTTONUP` 判断的，v4 下永远不成立，右键菜单就废了。
        # v1（不调用 NIM_SETVERSION）下 lParam 就是 WM_RBUTTONUP，行为最直接。
        return True

    def _remove_icon(self):
        if self.hwnd:
            nid = self._nid_obj()
            Shell_NotifyIconW(NIM_DELETE, ctypes.byref(nid))
        self._nid = None

    def notify(self, title, msg, timeout=4000):
        """气泡通知（非阻塞，立即返回）"""
        if not self.hwnd:
            return
        nid = self._nid_obj()
        nid.uFlags = NIF_INFO
        nid.szInfoTitle = title[:63]
        nid.szInfo = msg[:255]
        nid.dwInfoFlags = 0
        if self._icon_handle:
            nid.hBalloonIcon = self._icon_handle
        Shell_NotifyIconW(NIM_MODIFY, ctypes.byref(nid))

    def _toast(self, title, msg):
        threading.Thread(target=self.notify, args=(title, msg), daemon=True).start()

    def set_tip(self, tip):
        self.title = tip
        if self.hwnd and self._nid:
            nid = self._nid_obj()
            nid.uFlags = NIF_TIP
            nid.szTip = tip
            Shell_NotifyIconW(NIM_MODIFY, ctypes.byref(nid))

    def stop(self):
        self._running = False
        if self.hwnd:
            user32.PostMessageW(self.hwnd, WM_CLOSE, 0, 0)
        if self._thread and self._thread.is_alive():
            self._thread.join(timeout=3)
