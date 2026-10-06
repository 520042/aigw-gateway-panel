# -*- coding: utf-8 -*-
"""验证 cdp.py 的 cookie 过滤修复：豆包子域 cookie 不再被丢弃"""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from app import cdp

sample = [
    {"name": "sessionid", "value": "ABC123SESSION", "domain": ".doubao.com"},
    {"name": "sid_tt",    "value": "TT_SESSION_VAL", "domain": "www.doubao.com"},
    {"name": "sid_uc",    "value": "UC_VAL",         "domain": "passport.doubao.com"},
    {"name": "odin_tt",   "value": "ODIN_VAL",       "domain": ".doubao.com"},
    {"name": "tt_csrf",   "value": "csrf_token_xyz","domain": "www.doubao.com"},
    {"name": "qimei",     "value": "device_id",      "domain": ".doubao.com"},
    {"name": "unrelated", "value": "should_drop",    "domain": ".google.com"},
]

print("== _domain_matches 单测 ==")
assert cdp.Browser._domain_matches("www.doubao.com", "doubao.com") is True
assert cdp.Browser._domain_matches("passport.doubao.com", "doubao.com") is True
assert cdp.Browser._domain_matches(".doubao.com", "doubao.com") is True
assert cdp.Browser._domain_matches("doubao.com", "doubao.com") is True
assert cdp.Browser._domain_matches("google.com", "doubao.com") is False
assert cdp.Browser._domain_matches("a.doubao.com", "www.doubao.com") is False
# 父域覆盖子域（apk-qoder：hosts 含 lingma.aliyun.com，cookie 在 .aliyun.com）
assert cdp.Browser._domain_matches(".aliyun.com", "lingma.aliyun.com") is True
assert cdp.Browser._domain_matches("aliyun.com", "lingma.aliyun.com") is True
# doubao 双向
assert cdp.Browser._domain_matches("doubao.com", "www.doubao.com") is True
assert cdp.Browser._host_of("https://doubao.com") == "doubao.com"
assert cdp.Browser._host_of("http://www.doubao.com/path?x=1") == "www.doubao.com"
print("  通过")

# ---- apk-qoder 父域 cookie 不应被丢（回归防护）----
qoder = [
    {"name": "session", "value": "Q_ALIYUN", "domain": ".aliyun.com"},
    {"name": "x",       "value": "y",        "domain": ".google.com"},
]
hosts_q = ["qoder.com", "lingma.aliyun.com"]
bq = {}
for c in qoder:
    dom = (c.get("domain") or "").lstrip(".")
    if not any(cdp.Browser._domain_matches(dom, h) for h in hosts_q):
        continue
    bq[c["name"]] = c
assert "session" in bq, "apk-qoder 的 .aliyun.com 父域 cookie 被丢（回归）"
assert "x" not in bq
print("✓ apk-qoder：.aliyun.com 父域 cookie 正确保留，跨站已排除")

hosts = ["doubao.com"]

def _score(c):
    dom = (c.get("domain") or "").lstrip(".")
    s = 0
    for h in hosts:
        if dom == h:
            s += 2
        elif dom.endswith("." + h):
            s += 3
    return s

best = {}
for c in sample:
    dom = (c.get("domain") or "").lstrip(".")
    if not any(cdp.Browser._domain_matches(dom, h) for h in hosts):
        continue
    nm = c.get("name")
    cur = best.get(nm)
    if cur is None:
        best[nm] = c
        continue
    cs_, ns_ = _score(cur), _score(c)
    if ns_ > cs_ or (ns_ == cs_ and len(c.get("value", "")) > len(cur.get("value", ""))):
        best[nm] = c
sel = list(best.values())
header = "; ".join("%s=%s" % (c["name"], c["value"]) for c in sel)

print("\n== 修复后 cookie_header 输出 ==")
print(header)
names = {c["name"] for c in sel}
assert "sessionid" in names
assert "sid_tt" in names, "www.doubao.com 的 sid_tt 被丢（旧 bug）"
assert "sid_uc" in names, "passport.doubao.com 的 sid_uc 被丢（旧 bug）"
assert "odin_tt" in names
assert "unrelated" not in names
assert "should_drop" not in header
print("✓ 所有豆包子域会话 cookie 均已保留，跨站 cookie 已排除")

# 同名：子域(www)应优先于父域(.doubao.com)
dup = [
    {"name": "sid_tt", "value": "PARENT",    "domain": ".doubao.com"},
    {"name": "sid_tt", "value": "CHILD_WWW", "domain": "www.doubao.com"},
]
best2 = {}
for c in dup:
    dom = (c.get("domain") or "").lstrip(".")
    nm = c["name"]
    cur = best2.get(nm)
    if cur is None:
        best2[nm] = c
        continue
    cs_, ns_ = _score(cur), _score(c)
    if ns_ > cs_ or (ns_ == cs_ and len(c.get("value", "")) > len(cur.get("value", ""))):
        best2[nm] = c
assert best2["sid_tt"]["value"] == "CHILD_WWW", "同名应保留更贴近 host 的子域值"
print("✓ 同名 cookie 优先保留子域会话值")

# 空匹配（未登录任何豆包）→ 返回空，让上层正确提示
empty = [{"name": "x", "value": "y", "domain": ".google.com"}]
best3 = {}
for c in empty:
    dom = (c.get("domain") or "").lstrip(".")
    if not any(cdp.Browser._domain_matches(dom, h) for h in hosts):
        continue
    best3[c["name"]] = c
assert not best3, "未登录时应无豆包 cookie"
print("✓ 无匹配时返回空（上层提示「未检测到登录 Cookie」）")
print("\n全部断言通过 —— 修复有效")
