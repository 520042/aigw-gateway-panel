#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
面板 UI 端到端渲染验证（2026-10-06）
用真无头 Chromium 打开面板，验证：
  1. 首页可加载、无 JS 报错
  2. 源列表 → 源详情：sticky 返回栏存在且吸顶（滚动后仍可见）
  3. 手机视口(390x844)下返回按钮尺寸够大、可点
产出截图到 /tmp/ui_verify_*.png

注：这是**测试脚本**，用 Playwright 只是为了截图/断言方便；
    生产运行时面板自身不依赖 Playwright（走项目自研 app/cdp.py）。
"""
import os
import sys

from playwright.sync_api import sync_playwright

BASE = os.environ.get("PANEL_BASE", "http://127.0.0.1:8099")


def probe(page, tag):
    """进入某个源的详情页并量返回栏"""
    page.evaluate("""() => {
        try { localStorage.setItem('aigw_tab','sources'); } catch(e){}
    }""")
    page.goto(BASE)
    page.wait_for_timeout(2500)
    # 点「源列表」入口，再点第一个源
    clicked = page.evaluate("""() => {
        const cands = [...document.querySelectorAll('a,button,div,span')]
          .filter(e => (e.innerText||'').trim() === '源列表');
        if (cands.length) { cands[0].click(); return 'nav'; }
        return 'no-nav';
    }""")
    page.wait_for_timeout(1200)
    res = page.evaluate("""() => {
        // 尝试点开第一个源卡片
        const btns = [...document.querySelectorAll('[onclick]')]
          .filter(e => /openSrc|srcDetail/.test(e.getAttribute('onclick')||''));
        if (btns.length) { btns[0].click(); return 'opened'; }
        return 'no-src';
    }""")
    page.wait_for_timeout(1500)
    info = page.evaluate("""() => {
        const bb = document.querySelector('.backbar');
        if (!bb) return {found:false};
        const r = bb.getBoundingClientRect();
        const btn = bb.querySelector('.btn');
        const br = btn ? btn.getBoundingClientRect() : null;
        const st = getComputedStyle(bb);
        return {found:true, top:Math.round(r.top), sticky:st.position,
                btnH: br?Math.round(br.height):0,
                btnW: br?Math.round(br.width):0,
                btnTxt: btn?btn.innerText.trim():''};
    }""")
    print("[%s] nav=%s open=%s backbar=%s" % (tag, clicked, res, info))
    page.screenshot(path="/tmp/ui_verify_%s_detail.png" % tag)
    # 滚动再看
    page.evaluate("window.scrollTo(0, 700)")
    page.wait_for_timeout(700)
    top = page.evaluate("""() => {
        const bb = document.querySelector('.backbar');
        return bb ? Math.round(bb.getBoundingClientRect().top) : null;
    }""")
    print("[%s] 滚动后 backbar.top=%s" % (tag, top))
    page.screenshot(path="/tmp/ui_verify_%s_scrolled.png" % tag)
    return info, top


def main():
    errs = []
    with sync_playwright() as p:
        exe = "/root/.cache/ms-playwright/chromium-1208/chrome-linux64/chrome"
        if not os.path.exists(exe):
            print("找不到 chromium:", exe)
            return 2

        # ---- 桌面 ----
        b = p.chromium.launch(executable_path=exe,
                              args=["--no-sandbox", "--disable-gpu"])
        pg = b.new_page(viewport={"width": 1440, "height": 960})
        pg.on("pageerror", lambda e: errs.append("desktop: %s" % e))
        pg.goto(BASE)
        pg.wait_for_timeout(3000)
        pg.screenshot(path="/tmp/ui_verify_home_desktop.png", full_page=False)
        body = pg.evaluate("(document.body.innerText||'').length")
        print("桌面首页 body 文本长度=%s" % body)
        info_d, top_d = probe(pg, "desktop")
        b.close()

        # ---- 手机 ----
        b2 = p.chromium.launch(executable_path=exe,
                               args=["--no-sandbox", "--disable-gpu"])
        pg2 = b2.new_page(viewport={"width": 390, "height": 844},
                          is_mobile=True, has_touch=True,
                          user_agent=("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 "
                                      "like Mac OS X) AppleWebKit/605.1.15 "
                                      "(KHTML, like Gecko) Version/17.0 "
                                      "Mobile/15E148 Safari/604.1"))
        pg2.on("pageerror", lambda e: errs.append("mobile: %s" % e))
        pg2.goto(BASE)
        pg2.wait_for_timeout(3000)
        pg2.screenshot(path="/tmp/ui_verify_home_mobile.png")
        info_m, top_m = probe(pg2, "mobile")
        b2.close()

    print()
    if errs:
        print("JS 报错：")
        for e in errs[:10]:
            print("  ·", e)
    else:
        print("无 JS 报错")

    verdict = []
    verdict.append(("桌面 backbar 存在", info_d.get("found") is True))
    verdict.append(("桌面 backbar 吸顶", info_d.get("sticky") == "sticky"))
    verdict.append(("桌面按钮够大(≥28)", info_d.get("btnH", 0) >= 28))
    verdict.append(("手机 backbar 存在", info_m.get("found") is True))
    verdict.append(("手机按钮够大(≥36)", info_m.get("btnH", 0) >= 36))
    verdict.append(("滚动后仍吸顶", (top_d is not None and top_d <= 2)))
    for k, v in verdict:
        print("  %s %s" % ("PASS" if v else "FAIL", k))
    return 0 if all(v for _, v in verdict) else 1


if __name__ == "__main__":
    sys.exit(main())
