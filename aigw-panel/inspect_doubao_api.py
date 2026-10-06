# -*- coding: utf-8 -*-
"""
抓豆包网页版的真实接口清单（重点是模型列表接口）。
保存完整 URL（含 query）到 data/doubao_urls.txt，便于原样复现。
"""
import os
import sys
import time
import json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp

PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "data", "chrome-capture-profile")
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "data", "doubao_urls.txt")


def main():
    b = cdp.Browser(port=9466, user_data_dir=PROFILE)
    b.start(url="https://www.doubao.com/chat/")
    print("浏览器已启动，等待加载…")
    time.sleep(6)

    sessions = b.attach_all_pages()
    if not sessions:
        print("无标签")
        b.close()
        return 1
    s = list(sessions.values())[0]
    try:
        s.call("Network.enable", {"maxPostDataSize": 65536}, timeout=12)
        s.call("Page.enable", {}, timeout=10)
        # 重新导航，确保抓到完整生命周期
        s.call("Page.navigate", {"url": "https://www.doubao.com/chat/"}, timeout=15)
    except Exception as e:
        print("enable 失败:", e)

    urls = []
    for _ in range(8):
        time.sleep(2)
        for e in (s.drain(timeout=1.0) or []):
            m = e.get("method") or ""
            if m in ("Network.requestWillBeSent", "Network.responseReceived",
                     "Network.webSocketCreated"):
                p = e.get("params") or {}
                u = ((p.get("request") or {}).get("url")
                     or (p.get("response") or {}).get("url")
                     or p.get("url") or "")
                if u:
                    urls.append(u)

    uniq = sorted(set(urls))
    with open(OUT, "w", encoding="utf-8") as f:
        for u in uniq:
            f.write(u + "\n")
    print("共 %d 个去重 URL -> %s" % (len(uniq), OUT))

    # 只看 doubao 自己的接口（排除 CDN 静态资源）
    api = [u for u in uniq
           if "doubao.com" in u and "/obj/" not in u and not u.endswith((".js", ".css"))]
    print("\n== doubao.com 接口（%d 个，去 query）==" % len(api))
    seen = set()
    for u in api:
        path = u.split("?")[0]
        if path in seen:
            continue
        seen.add(path)
        print("  ", path.replace("https://", ""))

    print("\n== 含 model/agent/bot/launch 关键字 ==")
    for u in uniq:
        lu = u.lower()
        if any(k in lu for k in ("model", "/agent", "/bot", "launch")) \
                and "/obj/" not in u:
            print("  ", u[:170])

    cdp.Browser.close_all(sessions)
    b.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
