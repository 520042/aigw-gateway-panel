# -*- coding: utf-8 -*-
"""
干净环境工具：清残留进程 + 起面板 + 提供 HTTP 助手。
之前反复踩的坑都收在这里：
  - 单实例互斥体被残留进程占着 → 新实例直接退出
  - 端口只认 AIGW_PANEL_PORT，不认 --port
  - tasklist 输出是 GBK
  - PowerShell 重复 PATH
  - 网关限流是内存态
"""
import json
import os
import re
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable
GW_EXE = os.path.join(os.path.dirname(HERE),
                       "workbuddy-gateway-windows-1.29.6.exe")
GW_PORT = 8317


def clean_env(port=8801, no_browser=True):
    """PATH 大写去重 + 清代理 + 指定端口"""
    seen, parts = set(), []
    for p in os.environ.get("PATH", "").split(os.pathsep):
        if p and p.lower() not in seen:
            seen.add(p.lower())
            parts.append(p)
    env = {k: v for k, v in os.environ.items()
           if k.upper() not in ("HTTP_PROXY", "HTTPS_PROXY",
                                "ALL_PROXY", "PATH")}
    env["PATH"] = os.pathsep.join(parts)
    env["AIGW_PANEL_PORT"] = str(port)
    if no_browser:
        env["AIGW_NO_BROWSER"] = "1"
    env["PYTHONIOENCODING"] = "utf-8"
    return env


def _tasklist(image):
    r = subprocess.run(["tasklist", "/FI", "IMAGENAME eq " + image],
                       capture_output=True, text=True,
                       encoding="gbk", errors="replace")   # tasklist 是 GBK
    return re.findall(r"(\d+)\s+\S+\s+\d", r.stdout or "")


PID_FILE = os.path.join(HERE, "data", "panel.pid")


def kill_all():
    """杀掉残留面板进程。
    不用 wmic（Win11 已移除），改用 PID 文件 + 镜像名匹配。"""
    killed = []
    for img in ("aigw-panel.exe",):
        for p in _tasklist(img):
            subprocess.run(["taskkill", "/F", "/PID", p], capture_output=True)
            killed.append((img, p))
    # 源码实例：读 PID 文件（devboot 自己写）
    if os.path.exists(PID_FILE):
        try:
            pid = int(open(PID_FILE).read().strip())
            subprocess.run(["taskkill", "/F", "/PID", str(pid)],
                           capture_output=True)
            killed.append(("python main.py", pid))
        except Exception:
            pass
        try:
            os.remove(PID_FILE)
        except Exception:
            pass
    time.sleep(1.5)
    return killed


def port_busy(port):
    try:
        s = socket.create_connection(("127.0.0.1", port), timeout=1)
        s.close()
        return True
    except OSError:
        return False


def wait_port(port, timeout=45):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if port_busy(port):
            return True
        time.sleep(0.4)
    return False


def start_panel(port=8801, log="panel.log"):
    """起面板，返回 (proc, 端口)；PID 写 data/panel.pid 供下次清理"""
    out = os.path.join(HERE, "data", log)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    f = open(out, "w", encoding="utf-8", errors="replace")
    p = subprocess.Popen([PY, "main.py"], cwd=HERE, env=clean_env(port),
                         stdout=f, stderr=subprocess.STDOUT)
    with open(PID_FILE, "w") as pf:
        pf.write(str(p.pid))
    return p, port


def start_gateway():
    """网关限流是内存态，重启即清；没在跑就起"""
    if port_busy(GW_PORT):
        r = subprocess.run(["tasklist", "/FI", "IMAGENAME eq " +
                            os.path.basename(GW_EXE)],
                           capture_output=True, text=True,
                           encoding="gbk", errors="replace")
        for pid in re.findall(r"(\d+)\s+\S+\s+\d", r.stdout or ""):
            subprocess.run(["taskkill", "/F", "/PID", pid], capture_output=True)
        time.sleep(2)
    if not os.path.exists(GW_EXE):
        return None, "未找到网关"
    gwl = open(os.path.join(HERE, "data", "gw_run.log"), "w",
               encoding="utf-8", errors="replace")
    p = subprocess.Popen([GW_EXE, "serve", "-addr", "127.0.0.1",
                          "-port", str(GW_PORT), "-api-key", "admin"],
                         cwd=os.path.dirname(HERE), env=clean_env(),
                         stdout=gwl, stderr=subprocess.STDOUT,
                         creationflags=0x00000008)
    ok = wait_port(GW_PORT, 30)
    return (p if ok else None), ("PID=%d" % p.pid if ok else "启动超时")


def http(base, path, method="GET", body=None, timeout=25):
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(base + path, data=data, method=method)
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


def boot(port=8801, gateway=True, log="panel.log"):
    """一键干净启动：清残留 → （可选）起网关 → 起面板 → 等就绪"""
    killed = kill_all()
    if killed:
        print("清理残留：%s" % killed)
    if gateway:
        gwp, gwmsg = start_gateway()
        print("网关：%s" % gwmsg)
    p, _ = start_panel(port, log)
    ok = wait_port(port)
    print("面板：%s（%s）" % ("就绪" if ok else "超时",
                              "http://127.0.0.1:%d" % port))
    if not ok:
        print(open(os.path.join(HERE, "data", log), encoding="utf-8",
                   errors="replace").read()[-1200:])
    return p, ok


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8801)
    ap.add_argument("--no-gateway", action="store_true")
    ap.add_argument("--keep", action="store_true", help="跑完不退出")
    a = ap.parse_args()
    p, ok = boot(a.port, gateway=not a.no_gateway)
    if ok and a.keep:
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
    p.terminate()
