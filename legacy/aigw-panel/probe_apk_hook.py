# -*- coding: utf-8 -*-
"""扫 6 个 APK 里的「登录凭据抓取」特征"""
import os
import re
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

PATS = [
    (r"__yb2apiHooked", "y2api hook"),
    (r"addJavascriptInterface", "JS 桥接"),
    (r"evaluateJavascript", "注入 JS"),
    (r"x-uskey", "抓 x-uskey"),
    (r"x-bus-params", "抓 bus-params"),
    (r"x-timestamp", "抓 x-timestamp"),
    (r"x-tt-", "字节头 x-tt"),
    (r"x-[a-z]*sign", "签名头"),
    (r"sessionid", "sessionid"),
    (r"passport_csrf_token", "passport_csrf"),
    (r"msToken", "msToken"),
    (r"webview", "webview"),
    (r"loadUrl\(", "loadUrl"),
    (r"shouldOverrideUrlLoading", "URL 拦截"),
    (r"onPageFinished", "页面完成回调"),
    (r"getCookie", "读 Cookie"),
    (r"CookieManager", "CookieManager"),
    (r"document.cookie", "document.cookie"),
]

for name in ["base.apk", "base(1).apk", "base(2).apk", "base(3).apk",
             "base(4).apk", "base(5).apk"]:
    p = os.path.join(ROOT, name)
    if not os.path.exists(p):
        continue
    z = zipfile.ZipFile(p)
    blob = b""
    for n in z.namelist():
        if n.endswith(".dex"):
            blob += z.read(n)
    txt = blob.decode("utf-8", "replace")
    hits = []
    for pat, label in PATS:
        n = len(re.findall(pat, txt, re.I))
        if n:
            hits.append("%s x%d" % (label, n))
    print("%-14s %s" % (name, "  ".join(hits) if hits else "无特征"))
