# -*- coding: utf-8 -*-
"""
把元宝/豆包页面的完整 HTML + JS 全局状态 dump 下来，
从里面系统性提取「网页版模型选项」的真实定义（key/名称/描述）。
"""
import os
import sys
import time
import json
import re

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp

PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "data", "chrome-capture-profile")
OUTDIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")

SITES = [
    ("yuanbao", "https://yuanbao.tencent.com/chat/",
     ["Hy4", "Hy3", "DeepSeek", "hunyuan", "model"]),
    ("doubao", "https://www.doubao.com/chat/",
     ["Seed", "DeepSeek", "thinking", "model"]),
]


def ev(s, expr, timeout=25):
    try:
        r = s.call("Runtime.evaluate",
                   {"expression": expr, "returnByValue": True}, timeout=timeout)
        return ((r or {}).get("result") or {}).get("value")
    except Exception as e:
        return "ERR:%s" % type(e).__name__


def main():
    b = cdp.Browser(port=9499, user_data_dir=PROFILE)
    b.start(url=SITES[0][1])
    print("浏览器已启动")
    time.sleep(4)
    summary = {}
    for name, url, kws in SITES:
        print("\n" + "=" * 78)
        print(name, url)
        print("=" * 78)
        sessions = b.attach_all_pages()
        if not sessions:
            print("无标签")
            continue
        s = list(sessions.values())[0]
        try:
            s.call("Page.enable", {}, timeout=10)
            s.call("Runtime.enable", {}, timeout=10)
            s.call("Page.navigate", {"url": url}, timeout=15)
        except Exception as e:
            print("nav:", e)
        time.sleep(10)

        html = ev(s, "document.documentElement.outerHTML") or ""
        p = os.path.join(OUTDIR, "page_%s.html" % name)
        open(p, "w", encoding="utf-8").write(html)
        print("HTML %d 字节 -> %s" % (len(html), p))

        # 全局状态键
        gk = ev(s, "JSON.stringify(Object.keys(window).filter("
                   "k=>/state|initial|config|model|g_i18n|__/i.test(k)).slice(0,40))")
        print("可疑全局键:", gk)

        hits = {}
        for kw in kws:
            n = html.count(kw)
            if n:
                hits[kw] = n
        print("HTML 内关键词命中:", hits)
        summary[name] = {"file": p, "hits": hits, "size": len(html)}
        cdp.Browser.close_all(sessions)
    b.close()
    print("\n" + json.dumps(summary, ensure_ascii=False, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
