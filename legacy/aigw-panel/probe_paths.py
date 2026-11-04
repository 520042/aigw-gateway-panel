# -*- coding: utf-8 -*-
"""
为 404 的端点在**已登录的浏览器上下文**里探测正确路径
=====================================================
为什么要浏览器：APISIX 会校验会话，手搓 Cookie 直连一律 401（实测）。
在页面里 fetch 是同源的，浏览器自动带全部 Cookie。

基线对照法：先打一条肯定不存在的随机路径，
如果响应与候选完全一致 → 说明是通用错误页，不能作为「存在」的证据。
"""
import json
import os
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from app import tlogin                     # noqa: E402
from app.cdp import Session, Browser       # noqa: E402

# 候选路径：逆向的错了，从命名规律 + 前端 JS 里可能出现的路径去试
CANDIDATES = {
    "quota": [
        "/console/quota",
        "/console/billing/meter/get-user-resource",
        "/console/account/quota",
        "/console/enterprises/personal/resource",
        "/console/user/resource",
        "/v2/billing/meter/quota",
        "/console/billing/resource",
        "/console/subscription",
        "/console/plan",
    ],
    "usage_summary": [
        "/console/billing/meter/get-user-resource-summary",
        "/console/usage",
        "/console/billing/usage",
        "/console/usage/summary",
        "/console/account/usage",
    ],
    "claim_gift": [
        "/console/billing/meter/claim-gift",
        "/console/gift/claim",
        "/console/activity/gift/claim",
        "/console/billing/gift",
    ],
    "oauth_state": [
        "/console/login/state",
        "/console/auth/state",
        "/v2/plugin/login/state",
    ],
    # 顺带把签到/成长相关的其他可能路径也试一遍
    "checkin_status": [
        "/console/billing/meter/daily-checkin/status",
        "/console/checkin/status",
        "/v2/billing/meter/daily-checkin/status",
        "/console/billing/meter/checkin-status",
    ],
    "growth_info": [
        "/console/activity/growth",
        "/console/growth",
        "/activity/growth",
        "/console/activity/growth/info",
        "/console/activity/growth/status",
    ],
    "account_info": [
        "/console/account",
        "/console/accounts",
        "/console/user",
    ],
    "billing_trial": [
        "/console/billing/ide/trial",
        "/console/billing",
    ],
}


def run_js(state_names, cands, baseline):
    """在页面里批量 fetch，返回 {path: [status, body前300]}"""
    js = r"""
(async function(){
  var out = {};
  var base = %s;
  async function hit(p, method, body){
    try{
      var opt = {method:method, credentials:'include',
                 headers:{'Content-Type':'application/json'}};
      if (body !== null) opt.body = JSON.stringify(body);
      var r = await fetch(p, opt);
      var t = await r.text();
      out[p] = [r.status, t.slice(0,300)];
    }catch(e){ out[p] = [-1, String(e).slice(0,120)]; }
  }
  %s
  await hit(%s, 'GET', null);            // 基线
  var list = %s;
  for (var i=0;i<list.length;i++) await hit(list[i], 'GET', null);
  return JSON.stringify(out);
})()
"""
    filler = "".join("await hit(%s,'GET',null);\n" % json.dumps(p)
                     for ps in cands.values() for p in ps)
    return js % (json.dumps(""), filler,
                 json.dumps(baseline), json.dumps(list(cands.values()))[1:-1]
                 .join(["[", "]"]) if False else
                 json.dumps([p for ps in cands.values() for p in ps]))


def main():
    br = Browser(port=9333)
    if not br.is_up():
        print("浏览器没起，先登录一次")
        return 2
    tabs = [t for t in br.targets()
            if t.get("type") == "page"
            and ("codebuddy" in (t.get("url") or "")
                 or "tencent" in (t.get("url") or ""))]
    if not tabs:
        tabs = [t for t in br.targets() if t.get("type") == "page"]
    if not tabs:
        print("没有可用标签页")
        return 2
    s = Session(tabs[0]["webSocketDebuggerUrl"])
    baseline = "/console/__no_such_path_%s" % uuid.uuid4().hex[:12]
    try:
        r = s.call("Runtime.evaluate",
                   {"expression": run_js(None, CANDIDATES, baseline),
                    "awaitPromise": True, "returnByValue": True}, timeout=90)
        val = (r.get("result") or {}).get("value")
        if not val:
            print("执行失败:", json.dumps(r, ensure_ascii=False)[:500])
            return 2
        res = json.loads(val)
    finally:
        s.close()

    base_st = (res.get(baseline) or [None, ""])[0]
    base_body = (res.get(baseline) or ["", ""])[1]
    print("基线（不存在路径）: %s  %s\n" % (baseline, base_body[:90]))

    # 把候选按逻辑分组打回给调用方
    inv = {}
    for k, ps in CANDIDATES.items():
        inv[k] = ps
    for k, ps in inv.items():
        print("=" * 92)
        print("[%s]" % k)
        for p in ps:
            st, body = (res.get(p) or [None, ""])[:2]
            body = (body or "").replace("\n", " ")
            same = (st == base_st and body[:40] == base_body[:40])
            tag = "同基线(无信息)" if same else "★有区别"
            print("  %-52s %-4s %s  %s" % (p[:52], st, tag, body[:110]))
    out = os.path.join(HERE, "data", "endpoint_probe.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(res, f, ensure_ascii=False, indent=1)
    print("\n已存 %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
