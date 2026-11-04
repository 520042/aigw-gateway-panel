# -*- coding: utf-8 -*-
"""
用 Bearer token 探测（第二轮）
=============================
上一轮用 Cookie 在页面里 fetch，基线是 403 not_authorized，
结果 `/console/*` 下**所有**路径都返 403 —— 包括不存在的，
说明 APISIX 对 console 路由统一要 token，Cookie 不算。

这一轮改成带 Authorization: Bearer，基线应该变成 404 才说明有区分度。
"""
import json
import os
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from app import tlogin                     # noqa: E402
from app.cdp import Session, Browser       # noqa: E402

CANDIDATES = [
    # 额度 / 用量
    "/console/quota", "/console/billing/quota",
    "/console/billing/meter/get-user-resource",
    "/console/billing/meter/get-user-resource-summary",
    "/console/usage", "/console/usage/summary", "/console/billing/usage",
    "/console/account/quota", "/console/account/usage",
    "/console/enterprises/personal/resource",
    "/console/enterprises/personal/quota",
    "/console/enterprises/personal/usage",
    # 签到
    "/console/billing/meter/daily-checkin",
    "/console/checkin", "/console/checkin/status",
    "/console/billing/meter/daily-checkin/status",
    # 成长
    "/console/activity/growth", "/console/growth",
    "/console/activity/growth/lottery",
    "/console/activity/growth/lottery/draw",
    "/console/activity/growth/heatmap",
    "/console/activity/growth/buddy",
    "/console/activity/growth/makeup-cards",
    # 账号 / 权益
    "/console/account", "/console/accounts", "/console/login/type",
    "/console/billing/ide/trial", "/console/billing/ide",
    "/console/enterprises/personal/models",
    "/console/enterprises/personal/settings",
    # 邀请
    "/console/activity/workbuddy/invitation",
    "/console/invitation", "/console/invite",
    # v3/config 的同族
    "/v3/config", "/v3/models", "/v3/quota", "/v3/user",
    # 文档里见过的
    "/console/account/resource", "/console/account/credit",
]

JS = r"""
(async function(){
  var token = %s;
  var paths = %s;
  var out = {};
  async function hit(p){
    try{
      var r = await fetch(p, {credentials:'omit',
        headers:{'Authorization':'Bearer '+token, 'Accept':'application/json'}});
      var t = await r.text();
      out[p] = [r.status, t.slice(0,400)];
    }catch(e){ out[p] = [-1, String(e).slice(0,120)]; }
  }
  for (var i=0;i<paths.length;i++) await hit(paths[i]);
  return JSON.stringify(out);
})()
"""


def main():
    cred = tlogin.load()
    token = cred.get("accessToken") or ""
    if not token:
        print("没有 token")
        return 2
    br = Browser(port=9333)
    if not br.is_up():
        print("浏览器没起（用页面上下文发请求，需要已登录的浏览器）")
        return 2
    tabs = [t for t in br.targets() if t.get("type") == "page"
            and ("codebuddy" in (t.get("url") or "")
                 or "tencent" in (t.get("url") or ""))]
    tab = (tabs or [t for t in br.targets() if t.get("type") == "page"])[0]
    s = Session(tab["webSocketDebuggerUrl"])
    baseline = "/console/__nope_%s" % uuid.uuid4().hex[:10]
    try:
        r = s.call("Runtime.evaluate",
                   {"expression": JS % (json.dumps(token),
                                        json.dumps([baseline] + CANDIDATES)),
                    "awaitPromise": True, "returnByValue": True}, timeout=120)
        val = (r.get("result") or {}).get("value")
        if not val:
            print("执行失败:", json.dumps(r, ensure_ascii=False)[:400])
            return 2
        res = json.loads(val)
    finally:
        s.close()

    bst, bbody = (res.get(baseline) or [None, ""])[:2]
    bbody = (bbody or "").replace("\n", " ")
    print("基线 %s -> %s  %s\n" % (baseline, bst, bbody[:80]))
    print("=" * 100)
    hits = []
    for p in CANDIDATES:
        st, body = (res.get(p) or [None, ""])[:2]
        body = (body or "").replace("\n", " ")
        same = (st == bst and body[:40] == bbody[:40])
        if same:
            continue
        hits.append((p, st, body))
        print("★ %-54s %-4s %s" % (p[:54], st, body[:130]))
    print("\n有区分度的 %d / %d 条（其余与基线一致 = 不存在）"
          % (len(hits), len(CANDIDATES)))
    out = os.path.join(HERE, "data", "endpoint_probe2.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump({"baseline": [bst, bbody], "hits": hits},
                  f, ensure_ascii=False, indent=1)
    print("已存 %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
