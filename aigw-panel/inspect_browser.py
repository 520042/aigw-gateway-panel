# -*- coding: utf-8 -*-
"""
真的开浏览器查：用面板抓包用的 profile 起 Chrome，
读「Network.getAllCookies」明文 Cookie + 页面 localStorage，
看元宝/豆包的会话凭据到底存哪儿（Cookie 还是 localStorage）。
"""
import os
import sys
import time
import json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp

PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "data", "chrome-capture-profile")

SITES = [
    ("apk-yuanbao", "https://yuanbao.tencent.com/", "yuanbao.tencent.com"),
    ("apk-doubao", "https://www.doubao.com/", "doubao.com"),
]


def main():
    print("profile:", PROFILE)
    print("存在:", os.path.isdir(PROFILE))
    b = cdp.Browser(port=9444, user_data_dir=PROFILE)
    try:
        b.start(url="https://yuanbao.tencent.com/")
        print("浏览器已启动")
    except Exception as e:
        print("启动失败:", e)
        return 1

    time.sleep(4)

    # ---------- 1. 全部明文 Cookie ----------
    print("\n" + "=" * 78)
    print("1. Network.getAllCookies（明文，绕开 v20 磁盘加密）")
    print("=" * 78)
    try:
        all_c = b.cookies(None)
    except Exception as e:
        print("读取失败:", e)
        all_c = []
    print("共 %d 条 Cookie" % len(all_c))
    for host in ("yuanbao", "doubao"):
        hits = [c for c in all_c if host in (c.get("domain") or "")]
        print("\n-- %s 域 (%d 条) --" % (host, len(hits)))
        for c in hits[:40]:
            print("   %-40s %-22s %s" % (
                c.get("name"), c.get("domain"),
                (c.get("value") or "")[:26]))

    # ---------- 2. localStorage / sessionStorage ----------
    print("\n" + "=" * 78)
    print("2. 页面 localStorage / sessionStorage（元宝 token 可能在这）")
    print("=" * 78)
    try:
        sessions = b.attach_all_pages()
        print("挂了 %d 个 page 标签" % len(sessions))
        for tid, s in list(sessions.items())[:3]:
            try:
                r = s.call("Runtime.evaluate", {
                    "expression": "JSON.stringify({url:location.href, "
                                  "ls:Object.keys(localStorage||{}), "
                                  "ss:Object.keys(sessionStorage||{})})",
                    "returnByValue": True, "awaitPromise": False,
                }, timeout=15)
                val = ((r or {}).get("result") or {}).get("value")
                if val:
                    d = json.loads(val)
                    print("\n  url:", d.get("url"))
                    print("  localStorage 键:", d.get("ls"))
                    print("  sessionStorage 键:", d.get("ss"))
            except Exception as e:
                print("  评估失败:", type(e).__name__, str(e)[:80])
        cdp.Browser.close_all(sessions)
    except Exception as e:
        print("attach 失败:", type(e).__name__, str(e)[:120])

    b.close()
    print("\n完成")
    return 0


if __name__ == "__main__":
    sys.exit(main())
