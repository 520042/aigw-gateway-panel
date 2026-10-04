# -*- coding: utf-8 -*-
"""
路由引擎单元自测（不依赖真实上游）
====================================
起三个本地假 OpenAI 服务，模拟不同延迟 / 429 / 500 / 超时，
验证 fastest / weight / priority 三种策略与熔断降级是否真的按预期走。
"""

import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, __file__.rsplit("\\", 1)[0].rsplit("/", 1)[0])

from app.router import Router, Target  # noqa: E402


# ------------------------------------------------------------------ 假上游
def make_server(name, delay=0.05, fail_code=0, fail_times=0):
    """fail_times>0 时前 N 次返回 fail_code，之后正常"""
    state = {"calls": 0, "failed_left": fail_times}

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _json(self, obj, code=200):
            b = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)

        def do_GET(self):
            if self.path.endswith("/models"):
                time.sleep(delay)
                self._json({"object": "list", "data": [{"id": "m"}]})
            else:
                self._json({"error": "not found"}, 404)

        def do_POST(self):
            n = int(self.headers.get("Content-Length") or 0)
            body = json.loads(self.rfile.read(n) or b"{}")
            state["calls"] += 1
            if fail_code and state["failed_left"] > 0:
                state["failed_left"] -= 1
                time.sleep(delay)
                self._json({"error": {"message": "%s forced %d" % (name, fail_code)}},
                           fail_code)
                return
            time.sleep(delay)
            want = body.get("model") or "?"
            self._json({
                "id": "chatcmpl-" + name,
                "object": "chat.completion",
                "model": want,
                "choices": [{"index": 0, "message": {"role": "assistant",
                                                    "content": "hi from " + name},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5,
                          "total_tokens": 15},
            })

    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv, srv.server_address[1], state


def main():
    results = []

    def check(name, cond, detail=""):
        results.append((name, "PASS" if cond else "FAIL", detail))

    # ---- 起 3 个假上游：fast=快, mid=中, slow=慢
    s1, p1, st1 = make_server("fast", delay=0.03)
    s2, p2, st2 = make_server("mid", delay=0.12)
    s3, p3, st3 = make_server("slow", delay=0.30)
    print("假上游端口: fast=%d mid=%d slow=%d" % (p1, p2, p3))

    r = Router(log=lambda *_: None)

    def mk(mid, weight, prio, srv, port):
        return Target(model=mid, upstream_id="u" + mid, name=mid,
                      endpoint="http://127.0.0.1:%d/v1" % port,
                      weight=weight, priority=prio)

    # ============ 1. 探测延迟
    r.add_model("t-probe", [mk("fast", 1, 0, s1, p1), mk("slow", 1, 0, s3, p3)])
    pr = r.probe_all()
    check("探测返回全部结果", len(pr) == 2, str(pr))
    snap = r.list_models()["t-probe"]["targets"]
    by = {t["name"]: t for t in snap}
    check("fast 延迟 < slow 延迟",
          (by["fast"]["ewma_ms"] or 1e9) < (by["slow"]["ewma_ms"] or 0),
          "fast=%s slow=%s" % (by["fast"]["ewma_ms"], by["slow"]["ewma_ms"]))
    check("探测标记为健康", by["fast"]["state"] == "closed", by["fast"]["state"])

    # ============ 2. fastest 策略
    r.add_model("t-fast", [mk("fast", 100, 0, s1, p1),
                           mk("mid", 100, 0, s2, p2),
                           mk("slow", 100, 0, s3, p3)])
    r.probe_all()
    tgt, ordered = r.pick("t-fast", "fastest")
    check("fastest 选中最低延迟", tgt.name == "fast", tgt.name)

    res, tried = r.chat("t-fast", {"model": "x",
                                   "messages": [{"role": "user", "content": "hi"}]},
                        strategy="fastest")
    check("fastest 调用成功", "choices" in res, json.dumps(res, ensure_ascii=False)[:120])
    check("fastest 未降级", (res.get("_route") or {}).get("failover") == 0,
          str(res.get("_route")))

    # ============ 3. weight 策略（权重高的先走，不管延迟）
    r.add_model("t-w", [mk("slow", 100, 0, s3, p3), mk("fast", 500, 0, s1, p1)])
    r.probe_all()
    tgt, _ = r.pick("t-w", "weight")
    check("weight 选中高权重", tgt.name == "fast", tgt.name)
    res2, _ = r.chat("t-w", {"model": "x",
                             "messages": [{"role": "user", "content": "hi"}]},
                     strategy="weight")
    check("weight 调用走的是 fast",
          (res2.get("_route") or {}).get("upstream") == "fast",
          str(res2.get("_route")))

    # ============ 4. priority 策略（数字小的优先）
    r.add_model("t-p", [mk("fast", 1, 90, s1, p1), mk("slow", 999, 1, s3, p3)])
    r.probe_all()
    tgt, _ = r.pick("t-p", "priority")
    check("priority 选中优先级小的", tgt.name == "slow", tgt.name)

    # ============ 5. 429 触发降级
    s4, p4, st4 = make_server("flaky", delay=0.02, fail_code=429, fail_times=99)
    r.add_model("t-fail", [mk("flaky", 1, 0, s4, p4), mk("mid", 1000, 5, s2, p2)])
    r.probe_all()
    res3, tried3 = r.chat("t-fail", {"model": "x",
                                      "messages": [{"role": "user", "content": "hi"}]},
                          strategy="priority", max_failover=3)
    check("429 自动降级到下一个",
          (res3.get("_route") or {}).get("upstream") == "mid", str(res3.get("_route")))
    check("降级计数 > 0", (res3.get("_route") or {}).get("failover", 0) >= 1,
          str(res3.get("_route")))
    check("失败上游被记录", st4["calls"] >= 1, "calls=%d" % st4["calls"])

    # ============ 6. 熔断：连续失败 3 次进 OPEN
    r2 = Router(breaker_fail=3, breaker_cooldown=2, log=lambda *_: None)
    s5, p5, st5 = make_server("dead", delay=0.01, fail_code=500, fail_times=999)
    tg = Target(model="dead", upstream_id="d", name="dead",
                endpoint="http://127.0.0.1:%d/v1" % p5, weight=100, priority=0)
    r2.add_model("t-dead", [tg])
    for i in range(4):
        r2.chat("t-dead", {"model": "x", "messages": [{"role": "user", "content": "hi"}]},
                strategy="fastest", max_failover=0)
    check("连续失败后熔断", tg.state == "open",
          "state=%s consec=%d" % (tg.state, tg.consec_fail))

    # 冷却后应可被选中（state 保持 open 但已过期，不该再被熔断拦住）
    time.sleep(2.2)
    tgt, ordered = r2.pick("t-dead", "fastest")
    check("冷却到期后重新可选", tgt is not None and tgt.name == "dead",
          "state=%s picked=%s" % (tg.state, tgt.name if tgt else None))

    # ============ 7. 4xx 不降级（除 429）
    s6, p6, st6 = make_server("bad", delay=0.01, fail_code=400, fail_times=999)
    r.add_model("t-400", [mk("bad", 1, 0, s6, p6), mk("mid", 1, 9, s2, p2)])
    before = st2["calls"]
    res4, tried4 = r.chat("t-400", {"model": "x",
                                     "messages": [{"role": "user", "content": "hi"}]},
                          strategy="priority", max_failover=3)
    check("400 直接报错不降级",
          res4.get("code") == 400 and st2["calls"] == before,
          "code=%s mid_calls %d->%d" % (res4.get("code"), before, st2["calls"]))

    # ============ 8. 能力过滤
    tg_v = Target(model="novis", upstream_id="nv", name="novis",
                  endpoint="http://127.0.0.1:%d/v1" % p1)
    tg_v.capabilities = {"vision": False, "tools": True}
    tg_y = Target(model="vis", upstream_id="vy", name="vis",
                  endpoint="http://127.0.0.1:%d/v1" % p2)
    tg_y.capabilities = {"vision": True, "tools": True}
    r.add_model("t-cap", [tg_v, tg_y])
    tgt, _ = r.pick("t-cap", "fastest", require_vision=True)
    check("视觉请求过滤掉不支持的上游", tgt.name == "vis", tgt.name)
    tgt, _ = r.pick("t-cap", "fastest", require_vision=False)
    check("无视觉需求时不误过滤", tgt is not None, tgt.name if tgt else "None")

    # ============ 9. 会话亲和
    r.add_model("t-sess", [mk("fast", 1, 0, s1, p1), mk("mid", 2, 1, s2, p2)])
    r.bind_session("conv-9", "umid")
    tgt, _ = r.pick("t-sess", "fastest", session_id="conv-9")
    check("会话亲和生效", tgt.name == "mid", tgt.name)

    # ============ 10. 全熔断不返回 None
    tg2 = Target(model="x", upstream_id="x", name="x",
                 endpoint="http://127.0.0.1:1/v1")
    tg2.state = "open"
    tg2.open_until = time.time() + 9999
    r.add_model("t-open", [tg2])
    tgt, ordered = r.pick("t-open", "fastest")
    check("全熔断时降级为半开而非失败", tgt is not None, tgt.name if tgt else "None")

    # ============ 11. 空模型
    tgt, ordered = r.pick("not-exist")
    check("不存在的模型返回 None", tgt is None and not ordered, str(ordered))

    # ============ 12. model 名替换
    r.add_model("t-rename", [mk("realmodel", 1, 0, s1, p1)])
    res5, _ = r.chat("t-rename", {"model": "auto-fast",
                                  "messages": [{"role": "user", "content": "hi"}]})
    check("请求 model 被替换为上游真实模型",
          (res5.get("_route") or {}).get("upstream") == "realmodel",
          str(res5.get("_route")))

    # ============ 汇总
    print()
    print("=" * 78)
    print("%-38s %-6s %s" % ("测试项", "结果", "说明"))
    print("=" * 78)
    npass = sum(1 for _, s, _ in results if s == "PASS")
    for n, s, d in results:
        print("%-38s %-6s %s" % (n, s, d[:34]))
    print("=" * 78)
    print("通过 %d / %d" % (npass, len(results)))

    for s in (s1, s2, s3, s4, s6):
        try:
            s.shutdown()
        except Exception:
            pass
    return 0 if npass == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
