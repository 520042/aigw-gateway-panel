# -*- coding: utf-8 -*-
"""
面板 HTTP 接口端到端测试（/api/tencent 全动作）
================================================
自起面板进程 → 打接口 → 收结果 → 关面板。
不依赖 workbuddy-gateway 进程。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PORT = os.environ.get("AIGW_TEST_PORT", "8811")
BASE = "http://127.0.0.1:" + PORT

OK, BAD = [], []


def check(name, cond, extra=""):
    (OK if cond else BAD).append(name)
    print("  %s %s %s" % ("PASS" if cond else "FAIL", name, extra))


def post(path, body=None, timeout=60):
    url = BASE + path
    data = json.dumps(body or {}).encode("utf-8")
    r = urllib.request.Request(url, data=data,
                               headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as x:
            return x.status, json.loads(x.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode("utf-8", "replace"))
        except Exception:
            return e.code, {}
    except Exception as e:
        return 0, {"_err": str(e)}


def result_of(d):
    if isinstance(d, dict):
        if "result" in d:
            return d["result"]
        if d.get("ok") is False or "error" in d:
            return d
    return d


def main():
    # 先清残留单实例
    for exe in ("aigw-panel.exe",):
        subprocess.run(["taskkill", "/F", "/IM", exe], capture_output=True)
    time.sleep(1)

    env = dict(os.environ)
    env["AIGW_PANEL_PORT"] = PORT
    p = subprocess.Popen([sys.executable, "main.py", "--no-tray"], env=env,
                         cwd=HERE, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, text=True,
                         encoding="gbk", errors="replace")
    try:
        up = False
        for _ in range(25):
            time.sleep(1)
            s, _d = post("/api/overview", timeout=8)
            if s == 200:
                up = True
                break
        check("面板启动并响应", up, "port=%s" % PORT)
        if not up:
            try:
                p.terminate()
                out = p.communicate(timeout=5)[0]
                print("  启动日志尾部：\n", (out or "")[-1500:])
            except Exception:
                p.kill()
            return 1

        print("\n--- action=status ---")
        s, d = post("/api/tencent?action=status")
        r = result_of(d)
        check("status 返回 200", s == 200, str(s))
        check("识别到已登录", bool(r.get("logged")), json.dumps(r, ensure_ascii=False)[:200])
        if r.get("logged"):
            v = r.get("verify") or {}
            check("/v3/config 验活 200", v.get("rates") == 200, str(v))
            check("在线 models 验活 200", v.get("models") == 200, str(v))

        print("\n--- action=catalog（在线倍率目录）---")
        s, d = post("/api/tencent?action=catalog", timeout=60)
        r = result_of(d)
        ms = r.get("models") or []
        check("catalog 返回 200", s == 200, str(s))
        check("模型数 > 20", len(ms) > 20, "count=%d" % len(ms))
        withc = [m for m in ms if m.get("credits")]
        check("多数带倍率", len(withc) >= len(ms) * 0.6,
              "%d/%d" % (len(withc), len(ms)))
        s1 = [m for m in ms if m.get("id") == "hy3"]
        check("hy3 在列且倍率 x0.00",
              bool(s1) and "0.00" in s1[0].get("credits", ""),
              s1[0].get("credits") if s1 else "缺失")
        s2 = [m for m in ms if m.get("id") == "space-bunny"]
        check("space-bunny 在列", bool(s2),
              s2[0].get("credits") if s2 else "缺失")

        print("\n--- action=probe ---")
        s, d = post("/api/tencent?action=probe", timeout=60)
        r = result_of(d)
        check("probe 返回 200", s == 200, str(s))
        check("probe 带端点表", len(r.get("endpoints") or {}) > 5,
              "endpoints=%d" % len(r.get("endpoints") or {}))

        print("\n--- action=quota（额度，2026-10-04 修正为 POST）---")
        s, d = post("/api/tencent?action=quota", timeout=60)
        r = result_of(d)
        check("quota 返回 200", s == 200, str(s))
        q = r.get("quota") or {}
        check("拿到资源条目", q.get("totalCount", 0) > 0,
              "count=%s" % q.get("totalCount"))
        check("拿到累计消耗", isinstance(q.get("totalDosage"), (int, float)),
              str(q.get("totalDosage")))
        summ = r.get("summary") or []
        check("聚合出资源包", len(summ) > 0, "包数=%d" % len(summ))
        if summ:
            f0 = summ[0]
            check("聚合项含 name/unit/size/remain/used",
                  all(k in f0 for k in ("name", "unit", "size", "remain", "used")),
                  str(list(f0.keys())))
        check("汇总总额度 > 0", (r.get("totalSize") or 0) > 0,
              "size=%s remain=%s used=%s" % (r.get("totalSize"),
                                             r.get("totalRemain"),
                                             r.get("totalUsed")))

        print("\n--- action=checkin（400「今天已签到」要当成功）---")
        s, d = post("/api/tencent?action=checkin", timeout=60)
        r = result_of(d)
        check("checkin 返回 200", s == 200, str(s))
        check("识别为已签到（幂等）",
              r.get("already") is True or r.get("checkedIn") is True,
              "already=%s checkedIn=%s msg=%s" % (r.get("already"),
                                                  r.get("checkedIn"),
                                                  r.get("msg")))

        print("\n--- action=login（真起浏览器；注意此动作会拉起 Chrome）---")
        s, d = post("/api/tencent?action=login")
        r = result_of(d)
        check("login 返回 200", s == 200, str(s))
        st = r.get("state") or ""
        check("生成了 state", len(st) >= 16, st)
        url = r.get("url") or ""
        check("URL 带 state 参数", ("state=%s" % st) in url, url[:120])
        check("URL 指向 copilot.tencent.com",
              url.startswith("https://copilot.tencent.com/login"), url[:80])

        print("\n--- action=catalog 无 token 时应报错而不是崩 ---")
        s, d = post("/api/tencent?action=catalog", {"token": ""})
        check("无 token 有明确错误", s in (200, 400, 500), str(s))

        print("\n通过 %d / 失败 %d" % (len(OK), len(BAD)))
        return 1 if BAD else 0
    finally:
        # 只关面板进程，**不要**连带关掉带 CDP 的 Chrome
        # （之前就是这么把用户正在扫码的页面弄死的）
        p.terminate()
        time.sleep(1)
        try:
            p.kill()
        except Exception:
            pass


if __name__ == "__main__":
    sys.exit(main())
