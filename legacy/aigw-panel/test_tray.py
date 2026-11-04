# -*- coding: utf-8 -*-
"""托盘与单实例自测（需要 Windows 桌面环境）"""

import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import tray as T  # noqa: E402

results = []


def check(name, cond, detail=""):
    results.append((name, "PASS" if cond else "FAIL", str(detail)[:70]))


def main():
    icon = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                        "app", "static", "aigw.ico")
    check("图标文件存在", os.path.exists(icon),
          "%d bytes" % (os.path.getsize(icon) if os.path.exists(icon) else 0))

    check("AppUserModelID 设置", T.set_taskbar_appid(), T.APP_ID)

    # 单实例
    g1 = T.SingleInstance("AigwPanelTest.Mutex")
    a1 = g1.acquire()
    check("首个实例取得锁", a1)
    g2 = T.SingleInstance("AigwPanelTest.Mutex")
    a2 = g2.acquire()
    check("第二个实例被拒", a2 is False, "acquired=%s" % a2)
    g1.release()
    g3 = T.SingleInstance("AigwPanelTest.Mutex")
    a3 = g3.acquire()
    check("释放后可再取得", a3, "acquired=%s" % a3)
    g3.release()

    # 托盘
    got = {}
    tr = T.TrayIcon(title="测试托盘", icon_path=icon,
                    on_open=lambda: got.setdefault("open", True),
                    on_start_gateway=lambda: got.setdefault("start", True),
                    on_stop_gateway=lambda: got.setdefault("stop", True),
                    on_exit=lambda: got.setdefault("exit", True))
    ok = tr.start()
    check("托盘图标创建", ok, "hwnd=%s" % tr.hwnd)
    check("图标句柄已加载", tr._icon_handle is not None)
    check("NOTIFYICONDATA 已挂载", tr._nid is not None)

    time.sleep(0.6)
    # 模拟双击打开面板
    T.PostMessageW(tr.hwnd, T.WM_TRAY, 0, T.WM_LBUTTONDBLCLK)
    time.sleep(0.5)
    check("双击触发打开面板", got.get("open") is True)

    # 模拟菜单命令
    T.PostMessageW(tr.hwnd, T.WM_COMMAND, T.ID_GW_START, 0)
    T.PostMessageW(tr.hwnd, T.WM_COMMAND, T.ID_GW_STOP, 0)
    T.PostMessageW(tr.hwnd, T.WM_COMMAND, T.ID_EXIT, 0)
    time.sleep(0.6)
    check("菜单「启动网关」", got.get("start") is True)
    check("菜单「停止网关」", got.get("stop") is True)
    check("菜单「退出」", got.get("exit") is True)

    # 气泡通知（不阻塞）
    threading_ok = True
    try:
        import threading
        th = threading.Thread(target=tr.notify, args=("标题", "内容"), daemon=True)
        th.start()
        th.join(timeout=3)
        threading_ok = not th.is_alive()
    except Exception as e:
        threading_ok = False
    check("气泡通知不卡死", threading_ok)

    tr.set_tip("新提示文字")
    check("托盘提示可更新", True, tr.title)

    tr.stop()
    time.sleep(0.5)
    check("托盘可停止", not tr._running)

    # 开机自启动读写
    ok1, m1 = T.set_autostart(True)
    check("开启开机自启动", ok1, m1)
    check("读取自启动状态", T.is_autostart_enabled())
    ok2, m2 = T.set_autostart(False)
    check("关闭开机自启动", ok2, m2)
    check("确认已关闭", not T.is_autostart_enabled())

    # ---- 右键菜单回归（用户两次反馈「右键没反应」）----
    # 根因：_add_icon 里设了 NOTIFYICON_VERSION_4，v4 会把 lParam
    # 从「鼠标消息」改成「通知码」，导致 `lparam == WM_RBUTTONUP`
    # 永远不成立，菜单永远不弹。必须保证不再设 v4。
    import inspect
    _src = inspect.getsource(T.TrayIcon._add_icon)
    # 真正让右键失效的动作是「调用 NIM_SETVERSION 设 v4」，
    # 注释里提到版本号常量不算（代码里应该只出现在注释里）
    _code_only = "\n".join(
        l for l in _src.split("\n")
        if not l.strip().startswith("#"))
    check("不再调用 NIM_SETVERSION（v4 会让右键失效）",
          "NIM_SETVERSION" not in _code_only,
          "v4 把 lParam 变成通知码，lparam==WM_RBUTTONUP 永不成立")
    check("_add_icon 检查 Shell_NotifyIconW 返回值",
          "if not Shell_NotifyIconW" in _src, "静默失败会让图标根本没挂上")
    # wndproc 必须同时认 lParam 和 uParam 两种位置
    _w = inspect.getsource(T.TrayIcon._wndproc)
    check("wndproc 同时认 lParam 与 uParam",
          "wparam & 0xFFFF" in _w and "lparam" in _w,
          "v1/v4 两种消息位置都要覆盖")
    check("wndproc 识别 WM_RBUTTONUP", "WM_RBUTTONUP" in _w)
    check("wndproc 识别 WM_LBUTTONDBLCLK", "WM_LBUTTONDBLCLK" in _w)
    check("wndproc 也认 WM_CONTEXTMENU", "WM_CONTEXTMENU" in _w)
    # TrackPopupMenu 之后必须发 WM_NULL，否则菜单一闪就没
    _m = inspect.getsource(T.TrayIcon._menu)
    check("TrackPopupMenu 后发 WM_NULL", "WM_NULL" in _m,
          "缺这条菜单会一闪就消失")
    check("菜单含「退出」项", "ID_EXIT" in _m)

    print()
    print("=" * 66)
    print("%-28s %-6s %s" % ("测试项", "结果", "说明"))
    print("=" * 66)
    np = sum(1 for _, s, _ in results if s == "PASS")
    for n, s, d in results:
        print("%-28s %-6s %s" % (n, s, d))
    print("=" * 66)
    print("通过 %d / %d" % (np, len(results)))
    return 0 if np == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
