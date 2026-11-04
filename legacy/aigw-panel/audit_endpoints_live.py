# -*- coding: utf-8 -*-
"""
端点全量审计（带真 token）
==========================
把 app/tencent.py 里登记的每个端点用真实 accessToken 打一遍，
输出：状态码 / 是不是 HTML 拦截 / 有没有业务 code。

判定规则（和之前 audit_endpoints.py 同一套，但这次带真凭据）：
  2xx + JSON + code=0        → 真存在且可用
  401/403 + JSON             → 存在，要凭据（但我们已有 token，说明凭据类型不对）
  404                        → 不存在（规格是错的）
  HTML                       → 被 Nginx/APISIX 拦截，不代表端点存在
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "app"))

from app.tencent import Tencent, ENDPOINTS, BASE_CN   # noqa: E402
from app import tlogin                                  # noqa: E402


def classify(status, body):
    s = (body or "").strip()
    is_html = s[:200].lower().lstrip().startswith(("<!doctype", "<html"))
    code = None
    msg = ""
    if not is_html and s.startswith("{"):
        try:
            d = json.loads(s)
            code = d.get("code")
            msg = str(d.get("msg") or "")[:60]
        except Exception:
            pass
    if is_html:
        return "HTML拦截", None, s[:60].replace("\n", " ")
    if status == 404:
        return "不存在", code, msg or s[:60]
    if status in (401, 403):
        return "要凭据", code, msg or s[:60]
    if 200 <= status < 300:
        if code == 0:
            return "✓可用", code, msg
        return "✓存在", code, msg or s[:60]
    return "?其他", code, msg or s[:60]


def main():
    cred = tlogin.load()
    token = cred.get("accessToken") or ""
    if not token:
        print("没有 token，先跑：python app/tlogin.py --token")
        return 2
    print("token 长度 %d，UA=%s\n" % (len(token), tlogin.UA))

    tc = Tencent(token)
    rows = []
    for name, (method, path) in ENDPOINTS.items():
        # 把 path 里的占位符填上
        p = path
        for k, v in (("state", "auditstate"), ("platform", "CLI"),
                     ("token", "x")):
            p = p.replace("{%s}" % k, str(v))
        ok, d = tc.call(name, method=method)
        if isinstance(d, dict):
            body = d.get("_raw") or ""
            if d.get("_html"):
                body = "<html interception page>"
            status = d.get("http") or (200 if d.get("code") is not None else 0)
            verdict, code, note = classify(status, body)
            if d.get("code") is not None and status == 200:
                verdict, code, note = classify(200, json.dumps(d))
        else:
            verdict, code, note = "异常", 0, str(d)[:60]
        rows.append((name, method, path, status, verdict, code, note))
        print("%-18s %-4s %-52s %3s %-8s %s"
              % (name, method, path[:52], status, verdict, note))

    print("\n" + "=" * 100)
    good = [r for r in rows if r[4] in ("✓可用", "✓存在")]
    need = [r for r in rows if r[4] == "要凭据"]
    bad = [r for r in rows if r[4] in ("不存在", "HTML拦截")]
    print("可用 %d | 要凭据 %d | 不可用 %d" % (len(good), len(need), len(bad)))
    if bad:
        print("\n需要重新定位的端点：")
        for r in bad:
            print("   %-18s %-4s %-50s %s" % (r[0], r[1], r[2][:50], r[3]))

    out = os.path.join(HERE, "data", "endpoint_audit.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump([{"name": r[0], "method": r[1], "path": r[2],
                    "status": r[3], "verdict": r[4], "code": r[5],
                    "note": r[6]} for r in rows], f,
                  ensure_ascii=False, indent=1)
    print("\n已存 %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
