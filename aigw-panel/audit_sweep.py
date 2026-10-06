# -*- coding: utf-8 -*-
"""
全量扫描面板登记的 88 个动作，无凭据探测每个端点，分类：
  DEAD_404   → 路径不存在（确定错误）
  SPA_HTML   → 200 但返回 HTML 页面（SPA 兜底，不是 JSON API，确定错误）
  NEED_AUTH  → 401/403（端点真实存在，只是没凭据）
  PUBLIC_OK  → 200 JSON（免登录可读）
  ERR/UNREACHABLE → 网络不通（国内连不上等，需人工确认）
"""
import ssl, os, json, urllib.request, urllib.error, urllib.parse
import concurrent.futures as cf

for k in list(os.environ):
    if k.upper().endswith("_PROXY"):
        os.environ.pop(k, None)

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36")

d = json.load(open(os.path.join("data", "panel_defs.json"), encoding="utf-8"))
BASE = d["base"]
ACTIONS = d["actions"]


def build_url(plat, a):
    base = a.get("base") or BASE.get(plat) or ""
    path = a.get("path") or ""
    if not base or not path:
        return None
    return base.rstrip("/") + "/" + path.lstrip("/")


def probe(item):
    plat, a = item
    url = build_url(plat, a)
    if not url:
        return plat, a, None, "NO_URL", ""
    m = (a.get("method") or "GET").upper()
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    hdrs = {"User-Agent": UA, "Accept": "application/json"}
    data = b"{}" if m == "POST" else None
    if data is not None:
        hdrs["Content-Type"] = "application/json"
    try:
        req = urllib.request.Request(url, data=data, headers=hdrs, method=m)
        with urllib.request.urlopen(req, timeout=12, context=ctx) as r:
            body = r.read(400).decode("utf-8", "replace")
            code, status = r.status, r.status
    except urllib.error.HTTPError as e:
        body = e.read(400).decode("utf-8", "replace")
        code, status = e.code, e.code
    except Exception as e:
        return plat, a, url, "UNREACHABLE", "%s" % (str(e)[:60])

    s = body.lstrip()
    # 判定顺序很关键：401/403 的 body 常常是 HTML 错误页，
    # 必须先判状态码再判 HTML，否则会把「真实存在但要鉴权」的端点误判成 SPA 兜底
    if status in (404, 410):
        kind = "DEAD_404"
    elif status in (401, 403):
        kind = "NEED_AUTH"
    elif s.startswith("<") or s.lower().startswith("<!doctype"):
        kind = "SPA_HTML"
    elif status == 200:
        kind = "PUBLIC_OK"
    else:
        kind = "HTTP_%s" % status
    return plat, a, url, kind, s[:100].replace("\n", " ")


items = []
for plat, acts in ACTIONS.items():
    if plat in ("apk-go",):          # copilot 官网，/v1/* 全 404，无动作
        continue
    for a in acts:
        if a.get("local"):           # APP 本地路由，公网必 404，跳过
            continue
        items.append((plat, a))

rows = []
with cf.ThreadPoolExecutor(max_workers=8) as ex:
    for r in ex.map(probe, items):
        rows.append(r)

print("%-16s %-22s %-6s %-12s %s" % ("平台", "动作", "方法", "判定", "URL"))
print("-" * 118)
bad = []
for plat, a, url, kind, snip in rows:
    flag = ""
    if kind in ("DEAD_404", "SPA_HTML"):
        flag = "  <<<< 错误"
        bad.append((plat, a, url, kind))
    print("%-16s %-22s %-6s %-12s %s%s" % (
        plat, a["id"], a.get("method", "GET"), kind, url, flag))

print("\n\n========== 需要修的端点 ==========")
for plat, a, url, kind in bad:
    print("  [%s] %-22s %-6s %s" % (plat, a["id"], kind, url))
print("\n合计 %d 个可疑端点 / 共扫 %d 个" % (len(bad), len(rows)))
json.dump([{"plat": p, "id": a["id"], "url": u, "kind": k} for p, a, u, k in bad],
          open(os.path.join("data", "bad_endpoints.json"), "w", encoding="utf-8"),
          ensure_ascii=False, indent=1)
