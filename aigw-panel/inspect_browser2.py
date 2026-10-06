# -*- coding: utf-8 -*-
"""
第二轮实查：
  A. 读出元宝 localStorage 里的登录态/模型缓存实际内容
  B. 打开豆包，抓 Network 请求，找「网页版模型选项」背后的真接口
"""
import os
import sys
import time
import json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp

PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "data", "chrome-capture-profile")


def eval_js(s, expr):
    try:
        r = s.call("Runtime.evaluate", {
            "expression": expr, "returnByValue": True, "awaitPromise": False,
        }, timeout=20)
        return ((r or {}).get("result") or {}).get("value")
    except Exception as e:
        return "ERR:%s" % type(e).__name__


def main():
    b = cdp.Browser(port=9455, user_data_dir=PROFILE)
    b.start(url="https://yuanbao.tencent.com/")
    print("浏览器已启动")
    time.sleep(5)

    sessions = b.attach_all_pages()
    print("已挂 %d 个标签" % len(sessions))
    if not sessions:
        b.close()
        return 1
    s = list(sessions.values())[0]

    print("\n" + "=" * 78)
    print("A. 元宝 localStorage 关键值")
    print("=" * 78)
    keys = ["LOCAL_AUTH_INFO_KEY_yuanbao.tencent.com", "__is_guest_mode__",
            "ModelInfoCacheV1", "null_modelList"]
    for k in keys:
        v = eval_js(s, "JSON.stringify(localStorage.getItem(%s))" % json.dumps(k))
        print("\n  [%s]" % k)
        print("   ", str(v)[:400])

    # 列出所有含 model 的 localStorage 键及其内容长度
    v = eval_js(s, """JSON.stringify(Object.keys(localStorage)
        .filter(k=>/model/i.test(k))
        .map(k=>[k, (localStorage.getItem(k)||'').length]))""")
    print("\n  含 model 的键:", v)

    # ---------- B. 豆包：抓网络请求找模型接口 ----------
    print("\n" + "=" * 78)
    print("B. 打开豆包抓 Network，找模型接口")
    print("=" * 78)
    try:
        s.call("Page.enable", {}, timeout=10)
        s.call("Network.enable", {"maxPostDataSize": 65536}, timeout=10)
        r = s.call("Page.navigate", {"url": "https://www.doubao.com/chat/"}, timeout=15)
        print("  导航:", "ok" if r else "?")
        time.sleep(8)
        evs = s.drain(timeout=1.0) or []
        urls = []
        for e in evs:
            m = e.get("method") or ""
            if m in ("Network.requestWillBeSent", "Network.responseReceived"):
                p = (e.get("params") or {})
                u = (p.get("request") or {}).get("url") or \
                    (p.get("response") or {}).get("url") or ""
                if u:
                    urls.append(u)
        # 再去一次，多收集
        for _ in range(3):
            time.sleep(2)
            for e in (s.drain(timeout=1.0) or []):
                m = e.get("method") or ""
                if m in ("Network.requestWillBeSent", "Network.responseReceived"):
                    p = (e.get("params") or {})
                    u = (p.get("request") or {}).get("url") or \
                        (p.get("response") or {}).get("url") or ""
                    if u:
                        urls.append(u)
        uniq = sorted(set(urls))
        print("  共抓到 %d 个 URL（去重 %d）" % (len(urls), len(uniq)))
        print("\n  -- 与 model 相关的 --")
        for u in uniq:
            if "model" in u.lower():
                print("   ", u[:150])
        print("\n  -- doubao.com 域名下的接口（前 30）--")
        n = 0
        for u in uniq:
            if "doubao.com" in u and "/api" in u or "samantha" in u:
                print("   ", u[:150])
                n += 1
                if n > 30:
                    break
    except Exception as e:
        print("  抓包失败:", type(e).__name__, str(e)[:150])

    cdp.Browser.close_all(sessions)
    b.close()
    print("\n完成")
    return 0


if __name__ == "__main__":
    sys.exit(main())
