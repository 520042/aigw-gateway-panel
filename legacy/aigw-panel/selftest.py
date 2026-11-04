# -*- coding: utf-8 -*-
"""面板全接口自测"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8801"
HERE = os.path.dirname(os.path.abspath(__file__))


def shutil_which_node():
    for p in (r"C:\Users\liang.zhao\.workbuddy\binaries\node\versions\22.22.2-3\node.exe",
              "node"):
        try:
            subprocess.run([p, "--version"], capture_output=True, timeout=10)
            return p
        except Exception:
            continue
    return None


NODE = os.environ.get("AIGW_NODE") or shutil_which_node()


def check_js():
    """前端 JS 语法检查 —— 浏览器静默拒绝整个文件的致命问题"""
    js = os.path.join(HERE, "app", "static", "app.js")
    if not NODE:
        return {"name": "前端 JS 语法", "code": "SKIP", "res": "SKIP",
                "body": "未找到 node，跳过", "dt": 0.0}
    t0 = time.time()
    r = subprocess.run([NODE, "--check", js], capture_output=True, text=True)
    err = (r.stdout + r.stderr).strip().split("\n")[0:3]
    return {"name": "前端 JS 语法", "code": 200 if r.returncode == 0 else 500,
            "res": "PASS" if r.returncode == 0 else "FAIL",
            "body": "语法正确" if r.returncode == 0 else " / ".join(err),
            "dt": time.time() - t0}


def call(path, body=None, timeout=40):
    url = BASE + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method="POST" if body is not None else "GET",
                                 headers={"Content-Type": "application/json"})
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read().decode("utf-8", "replace")
            code = r.status
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        code = e.code
    except Exception as e:
        return {"_err": str(e)}, 0, time.time() - t0
    try:
        return json.loads(raw), code, time.time() - t0
    except json.JSONDecodeError:
        return {"_raw": raw[:300]}, code, time.time() - t0


def brief(o, n=150):
    s = json.dumps(o, ensure_ascii=False, default=str)
    return s if len(s) <= n else s[:n] + "…"


CASES = [
    ("健康检查",        "/healthz",                          None),
    ("总览",            "/api/overview",                     None),
    ("用量看板",        "/api/usage?range=all",              None),
    ("用量曲线",        "/api/usage?range=24h",              None),
    ("模型列表",        "/api/models",                       None),
    ("签到中心",        "/api/checkins",                     None),
    # 「成长任务 /api/growth」「凭据文件 /api/credentials」是 EXE 时代的网关
    # dev 基建（build26 后无 UI 触达、EXE 不在必 502）——不再进自测。
    ("任务中心",        "/api/tasks",                        None),
    ("登录平台列表",    "/api/login?action=platforms",       None),
    ("账号池列表",      "/api/accounts",                     None),
    ("平台能力表",      "/api/platform?action=actions",      None),
    ("资源站点",        "/api/sites",                        None),
    ("免费资源导航",    "/api/catalog",                      None),
    ("上游档案",        "/api/upstreams",                    None),
    ("智能路由总览",    "/api/route",                        None),
    ("路由选路测试",    "/api/route?action=pick",            {"model": "auto"}),
    ("通知中心",        "/api/notify",                       None),
    ("日志",            "/api/logs",                         None),
    ("设置",            "/api/settings",                     None),
    ("聚合模型端点",    "/v1/models",                        None),
    ("首页 HTML",       "/",                                 None),
    ("样式表",          "/style.css",                        None),
    ("前端脚本",        "/app.js",                           None),
    ("不存在路由",      "/api/nope",                         None),
    ("新增任务",        "/api/tasks?action=add",             {"name": "自测任务", "kind": "manual"}),
    ("切换任务",        "/api/tasks?action=toggle",          {"id": "__FIRST__"}),
    ("执行任务",        "/api/tasks?action=run",             {"id": "__FIRST__"}),
    ("删除任务",        "/api/tasks?action=delete",          {"id": "__FIRST__"}),
    ("新增站点",        "/api/sites?action=add",             {"name": "自测站", "url": "https://example.com", "checkin": False}),
    ("探测站点",        "/api/sites?action=probe",           {"id": "__FIRSTSITE__"}),
    ("导出站点",        "/api/sites?action=export",          None),
    ("删除站点",        "/api/sites?action=delete",          {"id": "__FIRSTSITE__"}),
    ("添加Webhook",     "/api/notify?action=add",            {"type": "custom", "url": "http://127.0.0.1:9/x", "name": "自测钩子"}),
    ("删除Webhook",     "/api/notify?action=delete",         {"index": 0}),
    ("保存通知开关",    "/api/notify?action=save_flags",     {"notify": {"notify_checkin": True, "notify_quota": True, "notify_error": True}}),
    ("保存设置",        "/api/settings?action=save",         {"refresh_interval": 15}),
]

rows = []
first_task = None
first_site = None
# 期望 HTTP 非 200 但语义正确的用例（如 404）
EXPECT_ERR = {"/api/nope": 404}

# 前置：面板必须处于原生模式（build26 后 EXE 已彻底移除，overview 返回 native 块）
r, code, dt = call("/api/overview")
nat = r.get("native") or {}
rows.append(("自测前置·原生模式", code, "PASS" if nat.get("relays", 0) >= 7 else "FAIL",
             "账号池=%s 中继=%s 平台" % (nat.get("accounts"), nat.get("relays")), 0.0))

for name, path, body in CASES:
    b = dict(body) if body else None
    if b and b.get("id") == "__FIRST__":
        b["id"] = first_task or ""
    if b and b.get("id") == "__FIRSTSITE__":
        b["id"] = first_site or ""
    r, code, dt = call(path, b)
    if path in EXPECT_ERR:
        good = code == EXPECT_ERR[path]
    else:
        good = code == 200 and (r.get("ok") is not False)
    rows.append((name, code, "PASS" if good else "FAIL", brief(r), dt))
    # 抓新建的 id 供后续用例使用
    if name == "新增任务" and r.get("ok"):
        t = call("/api/tasks")[0]
        for x in (t.get("tasks") or []):
            if x.get("name") == "自测任务":
                first_task = x["id"]
    if name == "新增站点" and r.get("ok"):
        sl = call("/api/sites")[0]
        for x in (sl.get("sites") or []):
            if x.get("name") == "自测站":
                first_site = x["id"]

# ---------------------------------------------------------------- 对外 /v1 端点
# 这是 CodeDesk 等外部工具的接入面：models 必须能列，chat 空请求必须给
# OpenAI 形状的结构化错误（而不是 HTML/连接拒绝）
r, code, dt = call("/v1/models")
_v1n = len((r.get("data") or []))
rows.append(("/v1/models 可列出", code,
             "PASS" if code == 200 and _v1n > 0 else "FAIL", "模型数=%d" % _v1n, dt))
# 同模型跨源聚合：web-trae 的静态清单里 gpt-4o 是裸名注册的聚合模型
# （owned_by=aigw-router、targets≥1）。没有它说明聚合路由没建起来。
_merged = [m for m in (r.get("data") or [])
           if m.get("owned_by") == "aigw-router"
           and not str(m.get("id", "")).startswith(("auto", "free"))]
_mg4o = next((m for m in _merged if m.get("id") == "gpt-4o"), None)
rows.append(("同模型聚合路由已注册", code,
             "PASS" if _mg4o and (_mg4o.get("aigw") or {}).get("targets", 0) >= 1
             else "FAIL",
             "聚合模型=%d gpt-4o=%s" % (len(_merged),
                                       "targets=%d" % (_mg4o["aigw"]["targets"])
                                       if _mg4o else "缺"), dt))
r, code, dt = call("/v1/chat/completions", {"model": "selftest-no-such-model",
                                            "messages": []})
_is_json = isinstance(r, dict)
rows.append(("/v1/chat 空请求结构化报错", code,
             "PASS" if _is_json and code in (400, 404, 422, 500, 502) else "FAIL",
             ("HTTP %s error=%s" % (code, (r.get("error") or {}).get("message", "")[:48])
              if _is_json else "非 JSON 响应"), dt))
# Anthropic 兼容端点（EXE 同名能力已原生化）
r, code, dt = call("/v1/messages", {"model": "claude-sonnet-4", "max_tokens": 1,
                                    "messages": [{"role": "user", "content": "hi"}]})
_is_json = isinstance(r, dict)
rows.append(("/v1/messages 端点活着", code,
             "PASS" if _is_json and code != 404 else "FAIL",
             ("HTTP %s" % code) if _is_json else "非 JSON 响应", dt))

r, code, dt = call("/api/models")
_mc = len(r.get("models_enriched") or [])
_bd = (r.get("bundled") or {}).get("total", 0)
# EXE 移除后模型清单走降级表（内置倍率表 + rate_cache），admin.models 是
# 网关基建（EXE 不在恒空）——断言对象改为 models_enriched/bundled
rows.append(("模型清单(降级表)", code,
             "PASS" if code == 200 and (_mc > 0 or _bd > 0) else "FAIL",
             "enriched=%d 内置=%d" % (_mc, _bd), dt))

# ---------------------------------------------------------------- 倍率信息
# 用户的硬要求：「不仅要获取到模型，还要获取到倍率信息」
r, code, dt = call("/api/models")
bd = r.get("bundled") or {}
rows.append(("内置倍率表随接口返回", code,
             "PASS" if bd.get("total", 0) >= 30 else "FAIL",
             "内置 %d 个 / 带倍率 %d / %s" % (bd.get("total", 0),
                                             bd.get("with_rate", 0),
                                             bd.get("source", "")[:36]), dt))
sm = r.get("rate_summary") or {}
tot = sm.get("total", 0)
wr = sm.get("with_rate", 0)
pct = (100.0 * wr / tot) if tot else 0
rows.append(("模型行倍率覆盖率", code,
             "PASS" if (tot == 0 or pct >= 50) else "FAIL",
             "%d/%d = %.0f%%（内置表贡献 %s）" % (wr, tot, pct, sm.get("from_bundled")), dt))
_rows = r.get("models_enriched") or []
_bad = [x for x in _rows if x.get("credits")
        and not (str(x["credits"]).startswith("x")
                 or "credit" in str(x["credits"]).lower())]
rows.append(("倍率是真实数值", code, "PASS" if not _bad and wr >= 5 else "FAIL",
             "样例 %s" % ([x["credits"] for x in _rows if x.get("credits")][:3]), dt))
_nosrc = [x for x in _rows if not x.get("rate_source")]
rows.append(("每行标明倍率来源", code, "PASS" if not _nosrc and _rows else "FAIL",
             "来源集合 %s" % (sorted(set(x.get("rate_source") for x in _rows if x.get("rate_source"))),), dt))
_ctxt = [x for x in _rows if x.get("credits") and x.get("ctx")]
rows.append(("倍率行带上下文", code, "PASS" if _ctxt else "SKIP",
             "%d/%d 行有 ctx" % (len(_ctxt), wr), dt))
# 在线目录拉取不能 500
r, code, dt = call("/api/models?action=catalog", {"platform": "apk-codebuddy"})
rows.append(("在线倍率拉取不崩", code, "PASS" if code != 500 else "FAIL",
             "HTTP %s %s" % (code, (r.get("message") or "")[:60]), dt))

# 静态产物：前端 JS 语法（浏览器静默失败的最常见原因）
j = check_js()
rows.append((j["name"], j["code"], j["res"], j["body"], j["dt"]))

# ---------------------------------------------------------------- 账号登录
# 1) 平台列表必须覆盖 7 个网关（含国际站变体）
r, code, dt = call("/api/login?action=platforms")
pls = r.get("platforms") or []
ids = {p["id"] for p in pls}
need = {"wb-gateway", "apk-trae", "apk-codebuddy", "apk-doubao",
        "apk-yuanbao", "apk-raccoon", "apk-go"}
rows.append(("登录平台覆盖7网关", code, "PASS" if need <= ids else "FAIL",
             "共%d个，缺=%s" % (len(pls), ",".join(sorted(need - ids)) or "无"), dt))

# 2) 二维码登录：必须拿到二维码与授权链接，且轮询不报错
#    ⚠ start 返回的是启动瞬间快照：面板在后台线程里调网关拿 QR（实测约 2s），
#    所以这里要轮询等待，不能 start 后立刻断言（曾经因此误报 qr=0B）。
qr_ok = False
sess_id = ""
try:
    r, code, dt = call("/api/login?action=start", {"platform": "wb-gateway", "edition": "cn"})
    s = r.get("session") or {}
    sess_id = s.get("id", "")
    qr, au = s.get("qr") or "", s.get("auth_url") or ""
    for _ in range(6):                       # 最多等 ~6s 让后台线程拿到二维码
        if qr.startswith("data:image/png;base64,") and au.startswith("http"):
            break
        time.sleep(1.0)
        r, code, _ = call("/api/login?action=poll", {"id": sess_id})
        s = r.get("session") or {}
        qr, au = s.get("qr") or "", s.get("auth_url") or ""
    # EXE 移除后（build19/26）：copilot 系扫码走面板原生回退——auth_url 由
    # 前端 vendor/qrcode.min.js 现场出码，session.qr 为空串是**正常形态**；
    # 只有网关 EXE 在跑时才回 data:image PNG。两种都算通过。
    qr_ok = bool(sess_id and au.startswith("http")
                 and (qr.startswith("data:image/png;base64,") or qr == ""))
    rows.append(("发起二维码登录", code, "PASS" if qr_ok else "FAIL",
                 "id=%s qr=%dB auth=%s" % (sess_id[:8], len(qr), au[:52]), dt))
    r2, code2, dt2 = call("/api/login?action=poll", {"id": sess_id})
    s2 = r2.get("session") or {}
    rows.append(("轮询登录状态", code2, "PASS" if s2.get("status") else "FAIL",
                 "status=%s %s" % (s2.get("status"), (s2.get("message") or "")[:28]), dt2))
    r3, code3, dt3 = call("/api/login?action=cancel", {"id": sess_id})
    rows.append(("取消登录会话", code3, "PASS" if r3.get("ok") else "FAIL",
                 brief(r3, 40), dt3))
except Exception as e:
    rows.append(("发起二维码登录", 0, "FAIL", str(e)[:70], 0.0))

# 3) 账号池增删（Cookie 手动提交路径）
r, code, dt = call("/api/accounts?action=add",
                   {"platform": "apk-doubao", "name": "自测账号",
                    "type": "cookie", "secret": "sessionid=selftest1234567890"})
aid = (r.get("account") or {}).get("id", "")
rows.append(("账号池新增", code, "PASS" if aid else "FAIL",
             "id=%s" % (aid or "无"), dt))
r, code, dt = call("/api/accounts")
found = any(a.get("id") == aid for a in (r.get("accounts") or []))
rows.append(("账号池回读", code, "PASS" if found else "FAIL",
             "找到=%s" % found, dt))
# 密钥必须掩码，不能明文回传
plain_leak = any("selftest1234567890" in json.dumps(a, ensure_ascii=False)
                 for a in (r.get("accounts") or []))
rows.append(("密钥不外泄", code, "PASS" if not plain_leak else "FAIL",
             "明文泄露=%s" % plain_leak, dt))
r, code, dt = call("/api/accounts?action=delete", {"id": aid})
rows.append(("账号池删除", code, "PASS" if r.get("ok") else "FAIL", brief(r, 40), dt))

# 4) 平台动作：未登录时应给出明确提示，而不是 500
r, code, dt = call("/api/platform?action=call",
                   {"platform": "apk-trae", "action": "checkin_status", "params": {}})
msg = json.dumps(r, ensure_ascii=False)
rows.append(("平台动作无凭据提示", code,
             "PASS" if ("没有" in msg or "请先" in msg or r.get("ok")) else "FAIL",
             brief(r, 60), dt))

print("=" * 104)
print("%-20s %-6s %-6s %-8s %s" % ("测试项", "HTTP", "结果", "耗时", "响应摘要"))
print("=" * 104)
npass = sum(1 for x in rows if x[2] == "PASS")
for n, c, g, b, d in rows:
    print("%-20s %-6s %-6s %-8s %s" % (n, c, g, "%.2fs" % d, b[:70]))
print("=" * 104)
print("通过 %d / %d" % (npass, len(rows)))
if npass != len(rows):
    print("\n失败详情：")
    for n, c, g, b, d in rows:
        if g == "FAIL":
            print("  [%s] HTTP=%s %s" % (n, c, b))
sys.exit(0 if npass == len(rows) else 1)
