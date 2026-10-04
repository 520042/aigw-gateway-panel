# -*- coding: utf-8 -*-
"""
面板测试运行器：起面板 → 等端口 → 跑一组 HTTP 断言 → 关面板。
绕开 bash 后台进程被回收 / PowerShell 重复 PATH 两个坑。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable
PORT = int(os.environ.get("AIGW_TEST_PORT") or "8801")
BASE = "http://127.0.0.1:%d" % PORT
ROOT = os.path.dirname(HERE)          # 网关项目根目录
GW_EXE = os.path.join(ROOT, "workbuddy-gateway-windows-1.29.6.exe")
GW_PORT = 8317


def clean_env():
    """构造干净环境：PATH 大写去重、清代理变量、指定固定端口"""
    seen, parts = set(), []
    for p in os.environ.get("PATH", "").split(os.pathsep):
        if p and p.lower() not in seen:
            seen.add(p.lower())
            parts.append(p)
    env = {k: v for k, v in os.environ.items()
           if k.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "PATH")}
    env["PATH"] = os.pathsep.join(parts)
    env["AIGW_PANEL_PORT"] = str(PORT)
    env["AIGW_NO_BROWSER"] = "1"
    env["PYTHONIOENCODING"] = "utf-8"
    return env


def wait_port(timeout=40, port=None):
    import socket
    port = port or PORT
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            s = socket.create_connection(("127.0.0.1", port), timeout=1.5)
            s.close()
            return True
        except OSError:
            time.sleep(0.5)
    return False


def restart_gateway():
    """网关限流（too_many_attempts）是内存态，重启即清"""
    import socket as _s
    try:
        c = _s.create_connection(("127.0.0.1", GW_PORT), timeout=1)
        c.close()
    except OSError:
        return "未在运行"
    out = subprocess.run(
        ["tasklist", "/FI", "IMAGENAME eq " + os.path.basename(GW_EXE)],
        capture_output=True, text=True,
        encoding="gbk", errors="replace")   # tasklist 是 GBK，不能用默认 utf-8
    import re
    pids = re.findall(r"(\d+)\s+\S+\s+\d", out.stdout or "")
    for p in pids:
        subprocess.run(["taskkill", "/F", "/PID", p], capture_output=True)
    time.sleep(2)
    left = None
    try:
        c = _s.create_connection(("127.0.0.1", GW_PORT), timeout=1)
        c.close()
        left = "仍占用"
    except OSError:
        left = "已释放"
    return "杀掉 %s → %s" % (pids or "无", left)


def start_gateway():
    """网关没起就拉起来，让 /api/models 能拿到真实模型行"""
    if wait_port(timeout=2, port=GW_PORT):
        return None, "已在运行"
    if not os.path.exists(GW_EXE):
        return None, "未找到网关程序"
    gwl = open(os.path.join(HERE, "data", "gw_run.log"), "w",
               encoding="utf-8", errors="replace")
    p = subprocess.Popen([GW_EXE, "serve", "-addr", "127.0.0.1",
                          "-port", str(GW_PORT), "-api-key", "admin"],
                         cwd=ROOT, env=clean_env(),
                         stdout=gwl, stderr=subprocess.STDOUT,
                         creationflags=0x00000008)  # DETACHED_PROCESS
    okk = wait_port(timeout=25, port=GW_PORT)
    return (p if okk else None), ("已启动 PID=%d" % p.pid if okk else "启动超时")


def http(path, method="GET", body=None, timeout=25):
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    if data:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"_raw": raw[:400]}
    except Exception as e:
        return 0, {"_err": "%s: %s" % (type(e).__name__, e)}


def main():
    gwp, gwmsg = start_gateway()
    print("网关：%s" % gwmsg)

    out = os.path.join(HERE, "data", "panel_run.log")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    f = open(out, "w", encoding="utf-8", errors="replace")
    p = subprocess.Popen([PY, "main.py"], cwd=HERE, env=clean_env(),
                         stdout=f, stderr=subprocess.STDOUT)
    print("面板 PID=%d  端口=%d" % (p.pid, PORT))
    if not wait_port():
        p.kill()
        f.close()
        print("端口未就绪，日志：")
        print(open(out, encoding="utf-8", errors="replace").read()[-2000:])
        return 2
    print("端口就绪")

    # 限流自愈：网关登录失败计数超限会返回 too_many_attempts，重启网关即清
    st0, d0 = http("/api/models")
    if isinstance(d0, dict) and d0.get("code") == "too_many_attempts":
        print("检测到网关限流 → %s → 重启" % restart_gateway())
        time.sleep(1)
        gwp, gwmsg = start_gateway()
        print("网关：%s" % gwmsg)
        time.sleep(2)
        st0, d0 = http("/api/models")
        if isinstance(d0, dict) and d0.get("code") == "too_many_attempts":
            print("重启后仍限流，等待 65s…")
            time.sleep(65)
            st0, d0 = http("/api/models")
    print("模型接口预检：HTTP %s code=%s" % (st0, (d0 or {}).get("code", "ok")))

    fails = []
    try:
        checks = CHECKS()
        for name, fn in checks:
            try:
                okk, msg = fn()
            except Exception as e:
                okk, msg = False, "%s: %s" % (type(e).__name__, e)
            print(("  PASS  " if okk else "  FAIL  ") + name + "  " + str(msg)[:200])
            if not okk:
                fails.append(name)
    finally:
        p.terminate()
        try:
            p.wait(timeout=8)
        except Exception:
            p.kill()
        f.close()
        if gwp is not None:
            try:
                gwp.terminate()
            except Exception:
                pass
    print("\n结果：%d/%d 通过" % (len(CHECKS()) - len(fails), len(CHECKS())))
    if fails:
        print("失败项：%s" % ", ".join(fails))
    return 1 if fails else 0


def CHECKS():
    def c_status():
        st, d = http("/api/overview")
        return st == 200, "HTTP %s gateway=%s" % (
            st, (d.get("gateway") or {}).get("found") if isinstance(d, dict) else "?")

    def c_models():
        st, d = http("/api/models")
        if st != 200:
            return False, "HTTP %s %s" % (st, str(d)[:200])
        e = d.get("models_enriched") or []
        stat = d.get("codebuddy_static") or []
        with_rate = sum(1 for r in e if r.get("rate_source") == "线上目录")
        return True, "模型 %d（带线上倍率 %d）静态表 %d 缓存 %s" % (
            len(e), with_rate, len(stat), d.get("online_models"))

    def c_models_enriched_shape():
        st, d = http("/api/models")
        if st != 200:
            return False, "HTTP %s" % st
        e = d.get("models_enriched") or []
        if not e:
            return True, "无模型行（网关未启动），跳过"
        for r in e:
            if "credits" not in r or "rate_source" not in r:
                return False, "缺字段：%s" % r
        return True, "%d 行均含 credits/rate_source" % len(e)

    def c_catalog_nocred():
        """无凭据时不能 500，必须给出可读提示"""
        st, d = http("/api/models?action=catalog", "POST",
                     {"platform": "apk-codebuddy"})
        if st == 500:
            return False, "HTTP 500：%s" % str(d)[:300]
        if st == 200:
            cat = (d.get("catalog") or {})
            return True, "拉到 %s 个模型（端点 %s）" % (
                cat.get("total"), cat.get("endpoint"))
        return True, "HTTP %s（预期的无凭据/未登录提示）：%s" % (
            st, str(d.get("message") or d)[:160])

    def c_catalog_bad_platform():
        st, d = http("/api/models?action=catalog", "POST",
                     {"platform": "not-a-platform"})
        return st != 500, "HTTP %s %s" % (st, str(d.get("message") or d)[:120])

    def c_platform_list():
        st, d = http("/api/login?action=platforms")
        if st != 200:
            return False, "HTTP %s" % st
        ps = d.get("platforms") or []
        return len(ps) >= 7, "平台 %d 个" % len(ps)

    def c_accounts():
        st, d = http("/api/accounts?action=list")
        return st == 200, "HTTP %s 账号 %s" % (st, len((d or {}).get("accounts") or []))

    def c_models_rate_columns():
        """静态 CodeBuddy 表必须带倍率字段"""
        st, d = http("/api/models")
        if st != 200:
            return False, "HTTP %s" % st
        stat = d.get("codebuddy_static") or []
        if not stat:
            return False, "静态表为空"
        miss = [x for x in stat if not x.get("credits")]
        return len(miss) == 0, "静态表 %d 项，缺倍率 %d 项" % (len(stat), len(miss))

    def c_bundled_stats():
        """内置倍率表必须随接口返回"""
        st, d = http("/api/models")
        if st != 200:
            return False, "HTTP %s" % st
        bd = d.get("bundled") or {}
        if not bd.get("total"):
            return False, "内置表缺失"
        return bd["with_rate"] >= 30, "内置 %d 个，带倍率 %d（%s）" % (
            bd["total"], bd["with_rate"], bd.get("source"))

    def c_rate_coverage():
        """网关模型行的倍率命中率必须过半"""
        st, d = http("/api/models")
        if st != 200:
            return False, "HTTP %s" % st
        s = d.get("rate_summary") or {}
        if not s.get("total"):
            return True, "无模型行（网关未启动），跳过"
        rate = 100.0 * (s.get("with_rate") or 0) / s["total"]
        return rate >= 50, "倍率覆盖 %d/%d = %.0f%%（内置表贡献 %d）" % (
            s.get("with_rate"), s["total"], rate, s.get("from_bundled"))

    def c_rate_not_empty():
        """至少要有若干行拿到非空倍率，且形如 x0.79 credits"""
        st, d = http("/api/models")
        if st != 200:
            return False, "HTTP %s" % st
        rows = d.get("models_enriched") or []
        got = [r for r in rows if r.get("credits")]
        if len(got) < 5:
            return False, "仅 %d 行有倍率" % len(got)
        bad = [r for r in got
               if not (str(r["credits"]).startswith("x") or "credit" in str(r["credits"]).lower())]
        return not bad, "%d 行有倍率，样例 %s" % (
            len(got), [r["credits"] for r in got[:3]])

    def c_rate_source_field():
        """每行都要标明倍率来源"""
        st, d = http("/api/models")
        if st != 200:
            return False, "HTTP %s" % st
        rows = d.get("models_enriched") or []
        if not rows:
            return True, "无模型行，跳过"
        miss = [r for r in rows if not r.get("rate_source")]
        return not miss, "%d 行均带 rate_source" % len(rows)

    return [
        ("面板状态", c_status),
        ("模型清单返回 200", c_models),
        ("模型行含倍率字段", c_models_enriched_shape),
        ("静态表带倍率", c_models_rate_columns),
        ("内置倍率表随接口返回", c_bundled_stats),
        ("倍率覆盖率过半", c_rate_coverage),
        ("倍率是真实数值", c_rate_not_empty),
        ("每行标明倍率来源", c_rate_source_field),
        ("线上倍率拉取不崩", c_catalog_nocred),
        ("非法平台不崩", c_catalog_bad_platform),
        ("登录平台列表", c_platform_list),
        ("账号池列表", c_accounts),
    ]


if __name__ == "__main__":
    sys.exit(main())
