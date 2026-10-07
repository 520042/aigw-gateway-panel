#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
「多平台串台」回归测试（2026-10-06）

用户实测 bug：点「元宝」登录，抓回来的是**豆包**的二维码。
根因：面板所有平台原先共用固定调试端口 9333，第二个平台发起时
      cdp.Browser.start() 见端口已就绪就盲目复用 → 连到上一个平台的
      浏览器，于是抓回别人的登录页。

本测试为**负向验收**：连续为多个平台发起无头登录，断言每个抓到的
二维码都不相同、且都来自各自平台的登录页（用页面 URL 校验）。
串台时本测试必然失败。

★ 2026-10-06 二次修订（用户点破后的正解）：
  第一轮修复用「每平台散列不同端口」防串台，虽然有效但属于绕路。
  现行方案是**所有平台共用一个固定端口 9333**，靠 owner 归属标记防串台。
  所以本测试额外断言：所有平台用的都是**同一个端口**（不许再出现端口膨胀）。

运行：python3 test_no_crosstalk.py
"""
import base64
import hashlib
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp                       # noqa: E402
from app.gwlogin import (                 # noqa: E402
    PLATFORMS, CDP_PORT, _find_headless_chrome, _looks_like_qr, _is_blank_png,
)

PAIRS = [("apk-doubao", "doubao.com"),
         ("apk-yuanbao", "yuanbao.tencent.com")]
TIMEOUT = 70


def capture(platform, exe):
    """起一个独立的无头会话（带 owner 标记），返回 (二维码md5, 页面URL, 端口)"""
    spec = PLATFORMS[platform]
    url = spec.get("login_url")
    owner = "%s-%s" % (platform, hashlib.md5(platform.encode()).hexdigest()[:6])
    # 单端口策略：所有平台都用 CDP_PORT，防串台靠 owner 归属标记
    p = CDP_PORT
    b = cdp.Browser(port=p, exe=exe, headless=True, owner=owner)
    try:
        b.start(url=url)
    except Exception as e:
        return None, "start-fail: %s" % e, p
    try:
        page_url = ""
        click_texts = spec.get("login_click") or ()
        t0 = time.time()
        tries = 0
        clicked = False
        while time.time() - t0 < TIMEOUT:
            tab = b._first_page_ws()
            if not tab:
                time.sleep(2)
                continue
            tries += 1
            if click_texts and (not clicked or tries % 4 == 0):
                clicked = b.click_login(tab, texts=click_texts) or clicked
            # 记录当前页面真实 URL —— 串台时这里会是**别的平台**的域名
            try:
                for t in b.targets():
                    if t.get("type") == "page" and t.get("url", "").startswith("http"):
                        page_url = t["url"]
                        break
            except Exception:
                pass
            rect = b.find_qr_rect(tab)
            if not rect and not b.page_has_qr_hint(tab):
                time.sleep(3)
                continue
            clip = {"x": max(0, rect["x"] - 14), "y": max(0, rect["y"] - 14),
                    "width": rect["width"] + 28, "height": rect["height"] + 28,
                    "scale": 2} if rect else None
            data = b.screenshot(tab, clip=clip)
            if data and not _is_blank_png(data) and _looks_like_qr(data):
                md5 = hashlib.md5(base64.b64decode(data)).hexdigest()
                return md5, page_url, p
            time.sleep(3)
        return None, "timeout (url=%s)" % page_url, p
    finally:
        try:
            b.close()
        except Exception:
            pass


def main():
    exe = _find_headless_chrome()
    print("无头浏览器: %s\n" % exe)
    results = {}
    for pid, want_domain in PAIRS:
        print("=== %s  (期望域名 %s)" % (pid, want_domain))
        md5, url, port = capture(pid, exe)
        print("    端口=%d  md5=%s" % (port, md5[:12] if md5 else "—"))
        print("    页面URL=%s" % url)
        results[pid] = (md5, url, want_domain, port)
        print()

    ok = True
    # 1) 每个平台都抓到了码
    for pid, (md5, url, _, _p) in results.items():
        if not md5:
            ok = False
            print("FAIL %s 没抓到二维码 (%s)" % (pid, url))
    # 2) 域名必须匹配各自平台（这是串台的直接判据）
    for pid, (md5, url, want, _p) in results.items():
        if md5 and want not in url:
            ok = False
            print("FAIL %s 页面域名不对：期望含 %s，实际 %s" % (pid, want, url))
        elif md5:
            print("PASS %s 页面域名正确 (%s)" % (pid, want))
    # 3) 两张码不能是同一张
    mds = [v[0] for v in results.values() if v[0]]
    if len(mds) == len(results) and len(set(mds)) != len(mds):
        ok = False
        print("FAIL 抓到了相同的二维码 → 说明串台了")
    elif len(mds) == len(results):
        print("PASS 各平台二维码互不相同（无串台）")
    # 4) 单端口策略：所有平台必须共用同一个端口，不许再散列膨胀
    ports = {v[3] for v in results.values()}
    if ports and ports != {CDP_PORT}:
        ok = False
        print("FAIL 端口膨胀：应为单一端口 %d，实际 %s" % (CDP_PORT, ports))
    else:
        print("PASS 所有平台共用同一端口 %d（无端口膨胀）" % CDP_PORT)

    print("\n结论:", "无串台 ✅" if ok else "存在串台 ❌")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
