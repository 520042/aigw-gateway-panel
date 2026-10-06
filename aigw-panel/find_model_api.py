# -*- coding: utf-8 -*-
"""
抓「模型列表」的真接口：打开页面，把所有 API 响应体都翻一遍，
哪个响应里含模型关键词（doubao-seed / DeepSeek / Hy4 / Hy3…），哪个就是模型接口。
抓到后把 URL + 响应样例存 data/model_api_found.json。
"""
import os
import sys
import time
import json
import base64

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp

PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "data", "chrome-capture-profile")
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                   "data", "model_api_found.json")

KEYWORDS = ["doubao-seed", "DeepSeek", "deepseek", "bot_id", "model_name",
            "Hy4", "hy4", "Hy3", "hy3", "default_model", "modelList",
            "model_list", "modelTag"]


def drain_models(s, seconds=10, page_url=None):
    """抓 seconds 秒，返回 [(url, status, body_snippet)] 中含关键词的"""
    found = {}
    seen_bodies = {}
    t0 = time.time()
    while time.time() - t0 < seconds:
        try:
            evs = s.drain(timeout=0.8) or []
        except Exception:
            continue
        for e in evs:
            m = e.get("method") or ""
            p = e.get("params") or {}
            if m == "Network.responseReceived":
                r = p.get("response") or {}
                rid, url = r.get("requestId"), r.get("url") or ""
                st = r.get("status")
                if not rid or not url:
                    continue
                if "/obj/" in url or url.endswith((".js", ".css", ".png", ".woff")):
                    continue
                seen_bodies[rid] = (url, st)
            elif m == "Network.requestWillBeSent":
                pass
        # 对已收到的响应取 body
        for rid in list(seen_bodies.keys())[:60]:
            url, st = seen_bodies.pop(rid)
            try:
                rb = s.call("Network.getResponseBody", {"requestId": rid}, timeout=8)
                body = rb.get("body") or ""
                if rb.get("base64Encoded"):
                    body = base64.b64decode(body).decode("utf-8", "replace")
            except Exception:
                continue
            hits = [k for k in KEYWORDS if k in body]
            if hits:
                found[url] = {"status": st, "keywords": hits,
                              "len": len(body), "snippet": body[:600]}
    return found


def run_site(b, url, label, wait=8):
    print("\n" + "=" * 78)
    print(label, "->", url)
    print("=" * 78)
    sessions = b.attach_all_pages()
    if not sessions:
        print("无标签")
        return {}
    s = list(sessions.values())[0]
    try:
        s.call("Network.enable", {"maxPostDataSize": 200000}, timeout=12)
        s.call("Page.enable", {}, timeout=10)
        s.call("Page.navigate", {"url": url}, timeout=15)
    except Exception as e:
        print("enable 失败:", e)
    time.sleep(wait)
    found = drain_models(s, seconds=14)
    if not found:
        print("（未抓到含模型关键词的响应）")
    for u, info in found.items():
        print("★ %s  [%s] 命中=%s" % (u[:120], info["status"],
                                      ",".join(info["keywords"])[:60]))
        print("    ", info["snippet"][:300].replace("\n", " "))
    cdp.Browser.close_all(sessions)
    return found


def main():
    b = cdp.Browser(port=9477, user_data_dir=PROFILE)
    b.start(url="https://www.doubao.com/chat/")
    print("浏览器已启动")
    time.sleep(4)
    allf = {}
    allf.update(run_site(b, "https://www.doubao.com/chat/", "豆包"))
    allf.update(run_site(b, "https://yuanbao.tencent.com/chat/", "元宝"))
    b.close()
    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(allf, f, ensure_ascii=False, indent=1)
    print("\n结果已存", OUT, "（%d 个命中接口）" % len(allf))
    return 0


if __name__ == "__main__":
    sys.exit(main())
