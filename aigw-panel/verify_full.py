# -*- coding: utf-8 -*-
"""
实机全流程验证（2026-10-05）：拉起面板 → 逐平台打 platform_models →
验登录页平台清单 → 验倍率合并 → 落盘结果。
"""
import sys, io, json, time
sys.path.insert(0, '.')
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')
import devboot

PORT = 8823
base = "http://127.0.0.1:%d" % PORT

print("== 清残留 + 拉起面板 ==")
devboot.kill_all()
info = devboot.boot(port=PORT, gateway=False, log="panel_verify.log")
if not info:
    print("!! 面板启动失败")
    sys.exit(1)
time.sleep(1.0)

results = {"platform_models": {}, "issues": []}

def get(path, body=None, **q):
    if q:
        path = path + "?" + "&".join("%s=%s" % kv for kv in q.items())
    st, obj = devboot.http(base, path, method="GET" if body is None else "POST",
                           body=body)
    return obj if isinstance(obj, dict) else {"ok": False, "message": str(obj)}

# ---------- 1) 登录页平台清单 ----------
r = get("/api/login?action=platforms")
plats = r.get("platforms") or []
print("登录页平台数:", len(plats))
results["login_platforms"] = len(plats)

# ---------- 2) 逐平台 platform_models（action 是查询参数！）----------
ALL = ["gateway", "wb-gateway", "wb-gateway-intl"] + \
      [p["id"] for p in plats if p["id"].startswith(("apk-", "api-"))]
for pid in ALL:
    try:
        r = get("/api/models", {"platform": pid}, action="platform_models")
    except Exception as e:
        results["platform_models"][pid] = {"error": str(e)}
        continue
    if r.get("ok") is False:
        results["platform_models"][pid] = {
            "fail": (r.get("message") or "")[:120],
            "code": r.get("code")}
        continue
    ms = r.get("models") or []
    results["platform_models"][pid] = {
        "n": len(ms), "live": r.get("live"),
        "name": r.get("name"), "note": (r.get("note") or "")[:80],
        "sample": [m.get("id") for m in ms[:4]]}

print("\n===== platform_models 实测 =====")
for pid, r in results["platform_models"].items():
    if "error" in r:
        print("%-22s 异常: %s" % (pid, r["error"]))
    elif "fail" in r:
        print("%-22s FAIL[%s] %s" % (pid, r["code"], r["fail"]))
    else:
        print("%-22s %s %2d 个  live=%s  %s" % (
            pid, r["name"][:18], r["n"], r["live"], r["sample"]))

# ---------- 3) 倍率三源合并（api/models 默认表） ----------
r = get("/api/models")
rows = r.get("models") or []
withrate = [x for x in rows if isinstance(x, dict)
            and (x.get("credits") or x.get("cost"))]
print("\n默认模型表: %d 个，带倍率 %d 个" % (len(rows), len(withrate)))
results["default_models"] = {"n": len(rows), "with_rate": len(withrate)}

# ---------- 4) 账号池（应为空/或含历史） ----------
r = get("/api/accounts")
accs = (r.get("accounts") or [])
print("账号池条数:", len(accs))
results["accounts"] = len(accs)

with open("data/verify_full.json", "w", encoding="utf-8") as f:
    json.dump(results, f, ensure_ascii=False, indent=1)
print("\n落盘 data/verify_full.json")
