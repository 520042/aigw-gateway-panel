# -*- coding: utf-8 -*-
"""
单端口 + owner 归属校验 + 生命周期收口 —— 回归测试。

背景（用户 2026-10-06 点破）：
  上一版为防串台把各平台散列到 9333~9992，属于绕路。
  正解是「一个固定端口 + 归属校验 + 前端离开即收口 + 空闲超时」。

本测试验证四件事：
  1. 同一个端口，第二个不同 owner 的实例 **不能复用** 第一个的浏览器
     （这正是「点元宝出豆包码」的根因，必须继续挡住）；
  2. 同 owner 重复 start 可以复用（不重复起进程）；
  3. close() 后端口真正释放，另一个 owner 能立刻在同一端口起来；
  4. LoginManager.cancel() 会关掉浏览器并释放端口（前端"返回源列表"走的就是它）。

运行：python3 test_single_port.py
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp  # noqa: E402

PORT = 9333
PASS = []
FAIL = []


def check(name, ok, detail=""):
    (PASS if ok else FAIL).append(name)
    print("%s %s%s" % ("PASS" if ok else "FAIL", name,
                       ("  " + detail) if detail else ""))


def main():
    # 服务端无头场景用同一个查找函数（find_browser 只认 Windows 路径）
    from app.gwlogin import _find_headless_chrome
    exe = _find_headless_chrome()
    if not exe:
        print("跳过：未找到 Chromium/Chrome")
        return 0
    print("浏览器: %s\n端口: %d\n" % (exe, PORT))

    a = cdp.Browser(port=PORT, exe=exe, headless=True, owner="platA-sessA")
    b = cdp.Browser(port=PORT, exe=exe, headless=True, owner="platB-sessB")

    # 1) A 先起
    ok = a.start(url="about:blank")
    check("A 在 9333 上启动成功", ok, "port=%d" % a.port)
    time.sleep(1)

    # 2) B（不同 owner）在同一端口 start 必须失败 —— 不许复用 A 的浏览器
    reused = False
    try:
        b.start(url="about:blank")
        reused = True
    except RuntimeError as e:
        check("B 不同 owner 被拒绝（防串台）", True, str(e)[:60])
    if reused:
        # 若没抛错，至少要保证 B 连到的是自己的浏览器而非 A 的
        v = b.version()
        title = str(v.get("Browser") or "")
        check("B 不同 owner 被拒绝（防串台）", "aigw:platB-sessB" in title,
              "实际 title=%s" % title)

    # 3) A 自己重复 start 应复用（不重启进程）
    okt = a._owner_ok()
    check("A 的 owner 校验通过（自己的实例可复用）", okt)
    up_before = a.is_up()
    a.start(url="about:blank")
    check("A 重复 start 复用同一浏览器", up_before and a.is_up())

    # 4) 关掉 A → 端口释放 → B 立刻能在同一端口起来
    a.close()
    time.sleep(1)
    probe = cdp.Browser(port=PORT)
    check("A close() 后端口已释放", not probe.is_up())
    okb = b.start(url="about:blank")
    check("B 在同一端口 9333 启动成功（无端口膨胀）", okb and b.port == PORT,
          "port=%d" % b.port)
    check("B 的 owner 归属正确", b._owner_ok())
    b.close()

    # 5) LoginManager.cancel() 释放浏览器（前端"返回源列表"调的就是它）
    try:
        from app.gwlogin import LoginManager, PLATFORMS
        mgr = LoginManager(lambda: None, None)
        pid = "apk-doubao" if "apk-doubao" in PLATFORMS else list(PLATFORMS)[0]
        sess = mgr.start(pid, headless=1)
        for _ in range(60):
            time.sleep(0.5)
            if sess._browser and sess._browser.is_up():
                break
        has_b = bool(sess._browser and sess._browser.is_up())
        port_used = sess.opts.get("_cdp_port")
        mgr.cancel(sess.id)
        time.sleep(1.5)
        released = not cdp.Browser(port=port_used or PORT).is_up()
        check("cancel() 关掉浏览器并释放端口（返回源列表即收口）",
              has_b and released,
              "起浏览器=%s 端口=%s 已释放=%s" % (has_b, port_used, released))
    except Exception as e:
        check("cancel() 关掉浏览器并释放端口（返回源列表即收口）", False,
              "%s: %s" % (type(e).__name__, e))

    print("\n" + "=" * 56)
    print("通过 %d / 失败 %d" % (len(PASS), len(FAIL)))
    if FAIL:
        for f in FAIL:
            print("  ✗ " + f)
    print("=" * 56)
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
