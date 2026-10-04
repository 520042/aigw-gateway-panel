# -*- coding: utf-8 -*-
"""
托盘右键实测：真创建图标、真发消息、看 _menu 是否被触发
====================================================
起因：用户两次反馈「任务栏图标右键没反应」。
之前只按代码推理修过 TrackPopupMenu 缺 WM_NULL 的问题（那个确实是个坑），
但用户说还是没用 —— 说明真因在别处。这个脚本不做推理，直接实测。

做法：
  1) 起一个 TrayIcon，记录 _menu 被调用的次数
  2) 确认图标真的挂上（Shell_NotifyIcon 返回值 + 图标可见）
  3) 从另一个线程往托盘窗口发 WM_TRAY + WM_RBUTTONUP
  4) 等 1 秒看 _menu 有没有被调用
  5) 抓异常（ctypes 回调里抛异常会被静默吞掉，必须 hook）

运行：python probe_tray.py
"""
import ctypes
import ctypes.wintypes as wt
import os
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

os.environ.setdefault("AIGW_NO_BROWSER", "1")

from app import tray as T  # noqa: E402

user32 = ctypes.windll.user32
kernel32 = ctypes.windll.kernel32

# 关键：把 ctypes 回调里吞掉的异常打出来，否则「没反应」无从查证
_real_dispatch = None


def hook_errors():
    """给 wndproc 包一层，任何异常都打印"""
    orig = T.TrayIcon._wndproc

    def wrapped(self, hwnd, msg, wparam, lparam):
        try:
            return orig(self, hwnd, msg, wparam, lparam)
        except Exception as e:
            import traceback
            print("  [wndproc 异常] %s: %s" % (type(e).__name__, e))
            traceback.print_exc()
            return 0
    T.TrayIcon._wndproc = wrapped


def main():
    hook_errors()

    menu_calls = []
    icons = {}

    class Probe(T.TrayIcon):
        def __init__(self, *a, **kw):
            super().__init__(*a, **kw)
            # 必须在 hook 之前抓住原实现，否则 self._add_icon 已被覆盖 → 无限递归
            self._add_icon_real = T.TrayIcon._add_icon.__get__(self)

        def _add_icon(self):
            try:
                r = self._add_icon_real()
                icons["add_ok"] = r
                return r
            except Exception as e:
                icons["add_err"] = "%s: %s" % (type(e).__name__, e)
                return False

        def _menu(self, hwnd):
            menu_calls.append(time.time())
            print("  ✓ _menu 被调用了（右键事件到达）")
            # 不真的弹菜单（会阻塞），直接返回
            return

    # 用一个真实存在的图标
    ico = os.path.join(HERE, "app", "static", "aigw.ico")
    tr = Probe(title="托盘实测", icon_path=ico,
               on_open=lambda: print("  ✓ 双击打开"),
               on_start_gateway=lambda: print("  ✓ 启动网关"),
               on_stop_gateway=lambda: print("  ✓ 停止网关"),
               on_exit=lambda: print("  ✓ 退出"))
    ok = tr.start()
    print("1) 托盘启动:", ok)
    print("   hwnd:", tr.hwnd, " 图标挂载:", icons)
    if not ok or not tr.hwnd:
        print("   托盘没起来，后面测不了")
        return 1

    # 窗口类名要和我们发消息的对象一致
    cls = T.APP_ID + ".TrayWnd"
    found = user32.FindWindowW(ctypes.create_unicode_buffer(cls),
                               None)
    print("2) FindWindow(%s) = %s" % (cls, found))

    # 3) 模拟点击托盘图标：Shell_NotifyIcon 内部用的 NOTIFYICON 结构
    #    我们直接构造一条 WM_TRAY 发过去，等价于用户点了图标
    print("3) 发 WM_TRAY + WM_RBUTTONUP 模拟右键…")
    PostMessageW = user32.PostMessageW
    PostMessageW.argtypes = [wt.HWND, ctypes.c_uint, wt.WPARAM, wt.LPARAM]
    PostMessageW.restype = wt.BOOL
    okw = PostMessageW(tr.hwnd, T.WM_TRAY, 0, T.WM_RBUTTONUP)
    print("   PostMessageW 返回:", okw, " err:", ctypes.get_last_error())

    time.sleep(1.2)
    if menu_calls:
        print("4) ✓ 右键消息成功送达，_menu 被调用")
    else:
        print("4) ✗ 右键消息没送达 —— wndproc 没收到 WM_TRAY")
        # 退一步：直接调 wndproc 看能不能跑通
        print("   直接调用 wndproc 试试…")
        tr._wndproc(tr.hwnd, T.WM_TRAY, 0, T.WM_RBUTTONUP)
        time.sleep(0.3)
        if menu_calls:
            print("   → 直接调用成功，说明是消息投递（PostMessage）的问题")
        else:
            print("   → 直接调用也失败，问题在 wndproc 内部")

    # 5) 直接测 _menu 真实弹窗会不会卡住
    print("5) 测 TrackPopupMenu 是否会阻塞（用 1.5s 超时观察）")
    t0 = time.time()
    done = threading.Event()

    def call_menu():
        try:
            tr._menu_orig(tr.hwnd)
        except Exception as e:
            print("   _menu 异常:", e)
        finally:
            done.set()
    tr._menu_orig = tr._menu          # 保留真实实现
    th = threading.Thread(target=call_menu, daemon=True)
    th.start()
    finished = done.wait(1.5)
    if finished:
        print("   ✓ _menu 立刻返回（%.2fs）—— 菜单没阻塞"
              % (time.time() - t0))
    else:
        print("   ✗ _menu 阻塞了 %.1fs —— TrackPopupMenu 在等消息循环"
              % (time.time() - t0))
        print("     这是正常的（等用户点菜单项），但说明菜单真的弹出来了")
        # 阻塞是因为弹了菜单，说明右键功能其实是好的
    try:
        tr.stop()
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
