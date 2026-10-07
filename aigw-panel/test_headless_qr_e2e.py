#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
服务端无头浏览器扫码 —— 真实端到端验证（2026-10-06）

与 test_login_e2e.py 的区别：那个测的是「凭据落库」的单元链路（有桩），
本文件测的是**真·无头浏览器**打开真实登录页 → 点登录按钮 → 抓到二维码
→ 过真伪鉴别这一整条链路。这是用户最关心的能力：
    「面板跑在服务器上（没有本机 Chrome）也能扫码登录」。

运行：python3 test_headless_qr_e2e.py [平台id ...]
      不传平台则跑默认 5 个（豆包/元宝/小浣熊/WPS/秘塔）
产出：抓到码的截图存 /tmp/e2e_hq_<平台>.png，便于人工复核能不能扫。
"""
import base64
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp                      # noqa: E402
from app.gwlogin import (                # noqa: E402
    PLATFORMS, _find_headless_chrome, _looks_like_qr, _is_blank_png,
)

DEFAULT = ["apk-doubao", "apk-yuanbao", "apk-raccoon", "apk-wps", "apk-metaso"]
PORT = 9455
TIMEOUT = 75


def try_capture(platform, exe):
    """
    起无头浏览器打开该平台登录页，模仿 gwlogin._watch_cookie 的核心动作：
    点 login_click → find_qr_rect → screenshot → 真伪鉴别。
    返回 (ok, 说明, 截图b64)
    """
    spec = PLATFORMS.get(platform)
    if not spec:
        return False, "未知平台", ""
    url = spec.get("login_url") or ("https://" + (spec.get("hosts") or [""])[0])
    b = cdp.Browser(port=PORT, exe=exe, headless=True)
    try:
        b.start(url=url)
    except Exception as e:
        return False, "浏览器启动失败 %s" % e, ""

    try:
        click_texts = spec.get("login_click") or ()
        t0 = time.time()
        clicked = False
        tries = 0
        while time.time() - t0 < TIMEOUT:
            tab = b._first_page_ws()
            if not tab:
                time.sleep(2)
                continue
            tries += 1
            if click_texts and (not clicked or tries % 4 == 0):
                clicked = b.click_login(tab, texts=click_texts) or clicked
            rect = b.find_qr_rect(tab)
            hint = b.page_has_qr_hint(tab)
            if not rect and not hint:
                time.sleep(3)
                continue
            clip = None
            if rect:
                pad = 14
                clip = {"x": max(0, rect["x"] - pad), "y": max(0, rect["y"] - pad),
                        "width": rect["width"] + pad * 2,
                        "height": rect["height"] + pad * 2, "scale": 2}
            data = b.screenshot(tab, clip=clip)
            if data and not _is_blank_png(data) and _looks_like_qr(data):
                return True, "抓到二维码（rect=%s，第 %d 轮）" % (
                    rect is not None, tries), data
            time.sleep(3)
        return False, "超时未抓到可用二维码（tries=%d）" % tries, ""
    finally:
        try:
            b.close()
        except Exception:
            pass


def main():
    targets = sys.argv[1:] or DEFAULT
    exe = _find_headless_chrome()
    print("无头浏览器: %s\n" % exe)
    if not exe:
        print("!! 未找到 Chromium，测试无法进行")
        return 2
    ok_n = 0
    for pid in targets:
        print("=== %-16s %s" % (pid, PLATFORMS.get(pid, {}).get("name", "")))
        ok, why, data = try_capture(pid, exe)
        if ok:
            ok_n += 1
            out = "/tmp/e2e_hq_%s.png" % pid
            with open(out, "wb") as f:
                f.write(base64.b64decode(data))
            print("    PASS  %s  → %s" % (why, out))
        else:
            print("    FAIL  %s" % why)
        print()
    print("通过 %d/%d" % (ok_n, len(targets)))
    return 0 if ok_n == len(targets) else 1


if __name__ == "__main__":
    sys.exit(main())
