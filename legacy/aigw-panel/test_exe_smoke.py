# -*- coding: utf-8 -*-
"""
打包后的 EXE 冒烟测试
=====================
验证 app/tlogin.py、app/cdp.py 这些**函数内动态导入**的模块
真的被打进了 EXE（build.spec 的 hiddenimports 漏了就会在这里暴露）。

顺带验证 PS 里的坑：Git Bash 下 `taskkill /F` 会被 MSYS 当成路径转换，
必须用 PowerShell 的 Stop-Process。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
EXE = os.path.join(HERE, "dist", "aigw-panel.exe")

OK, BAD = [], []


def check(name, cond, extra=""):
    (OK if cond else BAD).append(name)
    print("  %s %s %s" % ("PASS" if cond else "FAIL", name, extra))


def kill_all():
    """
    杀残留实例。用 taskkill 走 PowerShell（从 Python subprocess 调，
    不受 Git Bash 的 MSYS 路径转换影响；直接在 bash 里敲会被当成 `/F` 路径）。

    ⚠ 光杀进程不够 —— 互斥体 `Local\\WorkBuddyGatewayMutex` 可能要等句柄释放，
    所以杀完轮询确认，别只 sleep 完就往前走。
    """
    subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         "Get-Process aigw-panel -ErrorAction SilentlyContinue "
         "| Stop-Process -Force"],
        capture_output=True)
    for _ in range(15):
        time.sleep(1)
        r = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "(Get-Process aigw-panel -ErrorAction SilentlyContinue).Count"],
            capture_output=True, text=True, encoding="utf-8", errors="replace")
        n = (r.stdout or "0").strip()
        if n in ("", "0", "None"):
            return True
    return False


def find_port(proc, tries=30):
    """
    端口从 **exe 同级** 的 data/panel.url 读。
    ⚠ EXE 的 base_dir() 是 exe 所在目录（dist/），不是源码目录 ——
      一开始错读源码 data/panel.url，探测不到端口，白等 4 分钟。
    ⚠ 也不要自己扫端口：端口固定 8800（被占 EXE 直接报错退出不顺延），
      实际地址以 exe 同级 data/panel.url 为准。
    ⚠ **别用 /api/overview 探活**：它要聚合网关/签到/用量，实测 >3s 会 TimeoutError，
      导致「端口明明活着却探不到」。用 /api/tencent?action=status（纯读本地凭据，最快）。
    """
    url_file = os.path.join(HERE, "dist", "data", "panel.url")
    for _ in range(tries):
        time.sleep(1)
        try:
            with open(url_file, "r", encoding="utf-8") as f:
                u = f.read().strip()
            if "127.0.0.1:" not in u:
                continue
            port = int(u.split("127.0.0.1:")[1].split("/")[0])
            req = urllib.request.Request(
                "http://127.0.0.1:%d/api/tencent?action=status" % port,
                data=b"{}", headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=8) as x:
                if x.status == 200:
                    return port
        except Exception:
            continue
    return None


def post(port, path, body=None, timeout=60):
    url = "http://127.0.0.1:%d%s" % (port, path)
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
    if isinstance(d, dict) and "result" in d:
        return d["result"]
    return d if isinstance(d, dict) else {}


def main():
    if not os.path.exists(EXE):
        print("EXE 不存在，先打包")
        return 2
    check("EXE 存在", True, "%.1f MB" % (os.path.getsize(EXE) / 1048576.0))
    kill_all()

    # 凭据要放在 exe 同级的 data/（base_dir() = exe 目录）。
    # 一开始误以为 EXE 读源码 data/，结果 logged=False 白排查半天 ——
    # tlogin.ROOT 也踩过同一个坑（frozen 时必须用 sys.executable 的 dirname）。
    src_cred = os.path.join(HERE, "data", "tencent_cred.json")
    dst_dir = os.path.join(HERE, "dist", "data")
    os.makedirs(dst_dir, exist_ok=True)
    if os.path.exists(src_cred):
        import shutil
        shutil.copy2(src_cred, os.path.join(dst_dir, "tencent_cred.json"))
        check("凭据已就位到 dist/data", True)
    else:
        check("凭据已就位到 dist/data", False, "源码 data/ 里没有，先登录一次")
    url_file = os.path.join(dst_dir, "panel.url")
    if os.path.exists(url_file):
        os.remove(url_file)      # kill_all 之后再删，避免删到上一轮残留

    env = dict(os.environ)
    env["AIGW_NO_BROWSER"] = "1"
    p = subprocess.Popen([EXE], env=env, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, text=True,
                         encoding="gbk", errors="replace")
    try:
        port = find_port(p)
        check("EXE 启动并响应", bool(port), "port=%s" % port)
        if not port:
            p.terminate()
            # EXE 是 GUI 子系统，terminate 后不一定立刻退出，
            # communicate 会 TimeoutExpired —— 这里不追它的日志了
            try:
                out = p.communicate(timeout=5)[0]
                print("  日志尾部：\n", (out or "")[-1200:])
            except subprocess.TimeoutExpired:
                print("  （无日志：EXE 未退出属正常，直接看下面的排查建议）")
            return 1

        print("\n--- 动态导入模块可用性（hiddenimports 漏了就会 500）---")
        for act in ("status", "catalog", "quota", "checkin"):
            s, d = post(port, "/api/tencent?action=" + act)
            r = result_of(d)
            err = r.get("error") or r.get("message") or ""
            check("action=%s" % act, s == 200 and not err,
                  "HTTP %s %s" % (s, str(err)[:80]))

        print("\n--- 数据正确性 ---")
        s, d = post(port, "/api/tencent?action=status")
        r = result_of(d)
        check("识别到原生登录凭据", r.get("logged") is True,
              "account=%s tokenLen=%s" % (r.get("account"), r.get("tokenLen")))
        v = r.get("verify") or {}
        check("倍率接口验活 200", v.get("rates") == 200, str(v))

        s, d = post(port, "/api/tencent?action=quota")
        r = result_of(d)
        q = r.get("quota") or {}
        check("拿到额度（POST 修正生效）", q.get("totalCount", 0) > 0,
              "条数=%s 剩余=%s" % (q.get("totalCount"), r.get("totalRemain")))
        check("聚合出资源包", len(r.get("summary") or []) > 0,
              "包数=%d" % len(r.get("summary") or []))

        s, d = post(port, "/api/tencent?action=catalog")
        r = result_of(d)
        check("拿到在线模型与倍率", (r.get("count") or 0) > 20,
              "count=%s" % r.get("count"))

        s, d = post(port, "/api/tencent?action=checkin")
        r = result_of(d)
        check("签到幂等识别", r.get("already") is True
              or r.get("checkedIn") is True, "msg=%s" % r.get("msg"))

        print("\n--- 前端静态资源 ---")
        with urllib.request.urlopen("http://127.0.0.1:%d/app.js" % port,
                                    timeout=10) as x:
            js = x.read().decode("utf-8", "replace")
        check("app.js 可下载", len(js) > 50000, "%d 字节" % len(js))
        check("含腾讯面板函数", "viewTencentPanel" in js and "tencentLogin" in js)
        print("\n通过 %d / 失败 %d" % (len(OK), len(BAD)))
        return 1 if BAD else 0
    finally:
        p.terminate()
        time.sleep(1)
        try:
            p.kill()
        except Exception:
            pass


if __name__ == "__main__":
    sys.exit(main())
