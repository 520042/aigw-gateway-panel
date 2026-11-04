# -*- coding: utf-8 -*-
"""
本机反代上游管理器（把 CLIProxyAPI 集成进面板）
================================================
把 71 MB 的 `cli-proxy-api.exe` 打进面板 EXE，用户不必手动装服务、改 YAML、
敲命令行登录。面板负责：

  1. 释放内置二进制到持久目录（带校验，避免每次启动解压 71 MB）
  2. 生成 / 维护 config.yaml
  3. 启停 + 状态探活（端口 + /v1/models）
  4. 触发 OAuth 登录并把授权链接抓出来给用户点

设计约束（沿用项目既有约束）：
  - 纯标准库，不引依赖
  - 端口与网关 8317 错开（默认 8318），占用时自动顺延
  - 子进程用 CREATE_NO_WINDOW，别在用户面前弹黑框
  - 登录凭据落在 data/cliproxy/auths/，跟着 data 目录走（不进 Git）
"""

import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
PROJECT = os.path.dirname(HERE)              # aigw-panel/
ROOT = os.path.dirname(PROJECT)              # 网关项目/

# 内置二进制在 EXE 里的位置（build.spec 用 datas 收进来）
BUNDLE_NAME = "cliproxy/cli-proxy-api.exe"
# 开发期用户自己放的（不想用内置的可以放这儿）
LOCAL_CANDIDATES = [
    os.path.join(ROOT, "cli-proxy", "cli-proxy-api.exe"),
    os.path.join(PROJECT, "cli-proxy", "cli-proxy-api.exe"),
]

VERSION = "8.0.13"
DEFAULT_PORT = 8318
DEFAULT_KEY = "aigw-local-key"
STAMP = ".binary.stamp"

# 支持的 OAuth 登录（--help 实测，v8.0.13；没有 CodeBuddy / Qoder）
PROVIDERS = [
    {"id": "kimi", "flag": "-kimi-login", "name": "Kimi",
     "note": "kimi.com 的 Kimi Code 订阅，设备码流程"},
    {"id": "kimi-ai", "flag": "-kimi-ai-login", "name": "Kimi.ai",
     "note": "Kimi 国际站"},
    {"id": "codex", "flag": "-codex-login", "name": "Codex（OpenAI）",
     "note": "ChatGPT Plus/Pro 订阅里的 Codex"},
    {"id": "codex-device", "flag": "-codex-device-login", "name": "Codex（设备码）",
     "note": "没有回调端口时用这个"},
    {"id": "claude", "flag": "-claude-login", "name": "Claude Code",
     "note": "Anthropic 订阅（Pro/Max）"},
    {"id": "antigravity", "flag": "-antigravity-login", "name": "Antigravity",
     "note": "Google Antigravity（Gemini 系）"},
    {"id": "xai", "flag": "-xai-login", "name": "xAI Grok",
     "note": "Grok Build 订阅"},
    {"id": "devin", "flag": "-devin-login", "name": "Devin",
     "note": "Devin 订阅"},
    {"id": "meta", "flag": "-meta-login", "name": "Meta Muse",
     "note": "Meta 登录接入 Muse Code"},
]

NO_WINDOW = 0x08000000 if os.name == "nt" else 0


# ------------------------------------------------------------------ 路径
def data_dir():
    """面板数据目录（EXE 同级 data/，源码期就是项目下的 data/）"""
    if getattr(sys, "frozen", False):
        base = os.path.dirname(sys.executable)
    else:
        base = PROJECT
    d = os.path.join(base, "data", "cliproxy")
    os.makedirs(d, exist_ok=True)
    return d


def bin_dir():
    d = os.path.join(data_dir(), "bin")
    os.makedirs(d, exist_ok=True)
    return d


def auth_dir():
    d = os.path.join(data_dir(), "auths")
    os.makedirs(d, exist_ok=True)
    return d


def config_path():
    return os.path.join(data_dir(), "config.yaml")


def log_path():
    return os.path.join(data_dir(), "service.log")


# ------------------------------------------------------------------ 二进制
def _bundle_src():
    """内置二进制在 PyInstaller 包里的位置"""
    if hasattr(sys, "_MEIPASS"):
        return os.path.join(sys._MEIPASS, "cliproxy", "cli-proxy-api.exe")
    return None


def binary_info():
    """返回 {path, exists, size, bundled, version, needs_extract}"""
    out = {"path": "", "exists": False, "size": 0, "bundled": False,
           "version": VERSION, "needs_extract": False}
    target = os.path.join(bin_dir(), "cli-proxy-api.exe")
    src = _bundle_src()
    if src and os.path.exists(src):
        out["bundled"] = True
        out["size"] = os.path.getsize(src)
    for cand in LOCAL_CANDIDATES:
        if os.path.exists(cand):
            out["size"] = out["size"] or os.path.getsize(cand)
            break
    if os.path.exists(target):
        out["path"] = target
        out["exists"] = True
        out["size"] = os.path.getsize(target)
        return out
    if src and os.path.exists(src):
        out["path"] = src
        out["exists"] = True
        out["needs_extract"] = True       # 还没释放到持久目录
    else:
        for cand in LOCAL_CANDIDATES:
            if os.path.exists(cand):
                out["path"] = cand
                out["exists"] = True
                break
    return out


def ensure_binary():
    """
    确保二进制在持久目录里。
    - 优先用内置包（PyInstaller 解出来的临时文件）释放一份到 data/cliproxy/bin
    - 用 size+stamp 双重判断，已经释放过就直接返回，不重复拷 71 MB
    """
    info = binary_info()
    target = os.path.join(bin_dir(), "cli-proxy-api.exe")
    stamp = os.path.join(bin_dir(), STAMP)
    if os.path.exists(target) and os.path.exists(stamp):
        try:
            if open(stamp).read().strip() == str(os.path.getsize(target)):
                return target, False               # 已在位，不用动
        except OSError:
            pass
    src = _bundle_src() or (LOCAL_CANDIDATES[0] if LOCAL_CANDIDATES
                            and os.path.exists(LOCAL_CANDIDATES[0]) else None)
    if not src or not os.path.exists(src):
        return "", False
    if os.path.abspath(src) == os.path.abspath(target):
        # 开发期直接用仓库里的
        with open(stamp, "w") as f:
            f.write(str(os.path.getsize(target)))
        return target, False
    tmp = target + ".part"
    shutil.copyfile(src, tmp)
    os.replace(tmp, target)                        # 原子替换，避免半截文件
    with open(stamp, "w") as f:
        f.write(str(os.path.getsize(target)))
    return target, True


def sha256(path, limit=None):
    h = hashlib.sha256()
    n = 0
    with open(path, "rb") as f:
        while True:
            b = f.read(262144)
            if not b:
                break
            h.update(b)
            n += len(b)
            if limit and n >= limit:
                break
    return h.hexdigest()


# ------------------------------------------------------------------ 配置
DEFAULT_CONFIG = """# 由 AI 资源网关面板自动生成，可手工编辑
host: "127.0.0.1"
port: {port}
auth-dir: "auths"
api-keys:
  - "{key}"
debug: false
logging-to-file: false
usage-statistics-enabled: false
proxy-url: ""
request-retry: 2
"""


def read_config():
    """读 config.yaml，返回 dict（只解析我们关心的字段，不引 yaml 库）"""
    p = config_path()
    cfg = {"host": "127.0.0.1", "port": DEFAULT_PORT, "keys": []}
    if not os.path.exists(p):
        return cfg
    try:
        with open(p, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                s = line.strip()
                if s.startswith("host:"):
                    cfg["host"] = s[5:].strip().strip('"').strip("'")
                elif s.startswith("port:"):
                    try:
                        cfg["port"] = int(s[5:].strip().split("#")[0])
                    except ValueError:
                        pass
                elif s.startswith("- ") and cfg["keys"] is not None \
                        and "api-keys" in line:
                    pass
        # api-keys 是列表，单独抓
        txt = io_read(p)
        m = re.search(r"api-keys:\s*\n((?:\s*-\s*.+\n)+)", txt)
        if m:
            cfg["keys"] = [x.strip().lstrip("- ").strip().strip('"').strip("'")
                           for x in m.group(1).splitlines() if x.strip()]
    except OSError:
        pass
    return cfg


def io_read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        return f.read()


def write_config(port=None, key=None, host=None, force=False):
    """生成/更新 config.yaml。返回 (ok, 信息)"""
    p = config_path()
    cur = read_config()
    port = int(port or cur.get("port") or DEFAULT_PORT)
    host = host or cur.get("host") or "127.0.0.1"
    keys = cur.get("keys") or []
    key = key or (keys[0] if keys else DEFAULT_KEY)
    if not force and os.path.exists(p):
        # 只改端口/key，不动用户手改过的其它字段
        txt = io_read(p)
        txt = re.sub(r'^port:\s*\d+', 'port: %d' % port, txt, flags=re.M)
        txt = re.sub(r'^host:\s*".*?"', 'host: "%s"' % host, txt, flags=re.M)
        txt = re.sub(r'^\s*-\s*".*?"\s*$', '  - "%s"' % key, txt,
                     count=1, flags=re.M)
        with open(p, "w", encoding="utf-8", newline="\n") as f:
            f.write(txt)
        return True, "已更新端口 %d / key" % port
    with open(p, "w", encoding="utf-8", newline="\n") as f:
        f.write(DEFAULT_CONFIG.format(port=port, key=key))
    return True, "已生成配置（端口 %d）" % port


# ------------------------------------------------------------------ 端口
def port_busy(port, host="127.0.0.1"):
    try:
        s = socket.create_connection((host, port), timeout=1)
        s.close()
        return True
    except OSError:
        return False


def pick_port(prefer=DEFAULT_PORT, tries=20):
    """prefer 被占就顺延"""
    for i in range(tries):
        p = prefer + i
        if not port_busy(p):
            return p
    return prefer


# ------------------------------------------------------------------ 启停
_proc = None
_login_proc = None


def _http(url, key="", timeout=6):
    req = urllib.request.Request(url)
    req.add_header("Accept", "application/json")
    if key:
        req.add_header("Authorization", "Bearer " + key)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, ""
    except Exception:
        return 0, ""


def status():
    """返回给前端的状态"""
    exe, _ = ensure_binary()
    cfg = read_config()
    port = int(cfg.get("port") or DEFAULT_PORT)
    key = (cfg.get("keys") or [DEFAULT_KEY])[0]
    url = "http://127.0.0.1:%d/v1/models" % port
    code, raw = _http(url, key)
    info = {
        "installed": bool(exe and os.path.exists(exe)),
        "exe": exe,
        "bundled": binary_info().get("bundled", False),
        "size_mb": round(binary_info().get("size", 0) / 1048576, 1),
        "version": VERSION,
        "host": cfg.get("host", "127.0.0.1"),
        "port": port,
        "api_key": key,
        "base_url": "http://127.0.0.1:%d/v1" % port,
        "config_path": config_path(),
        "auth_dir": auth_dir(),
        "log_path": log_path(),
        "online": code == 200,
        "code": code,
        "models": 0,
        "note": "",
        "providers": PROVIDERS,
        "auth_files": [],
    }
    try:
        for n in sorted(os.listdir(auth_dir())):
            fp = os.path.join(auth_dir(), n)
            if os.path.isfile(fp):
                info["auth_files"].append({
                    "name": n, "size": os.path.getsize(fp),
                    "mtime": time.strftime("%Y-%m-%d %H:%M",
                                           time.localtime(os.path.getmtime(fp)))})
    except OSError:
        pass
    if code == 200:
        try:
            d = json.loads(raw or "{}")
            data = d.get("data")
            if isinstance(data, list):
                info["models"] = len(data)
                info["model_ids"] = [str(x.get("id")) for x in data[:50]]
                info["note"] = ("在线，%d 个模型" % len(data)) if data \
                    else "在线，但还没登录任何账号"
            else:
                info["note"] = "在线（响应不是标准模型列表）"
        except json.JSONDecodeError:
            info["note"] = "在线（非 JSON）"
    elif code == 401:
        info["note"] = "服务在跑，但 API Key 不对"
    elif code == 404:
        info["note"] = "端口有响应但没有 /v1/models，可能不是这个服务"
    elif code:
        info["note"] = "HTTP %d" % code
    else:
        info["note"] = "未运行"
    return info


def start(port=None, key=None, autostart=True):
    """启动服务。返回 (ok, 信息)"""
    global _proc
    exe, _extracted = ensure_binary()
    if not exe or not os.path.exists(exe):
        return False, "没有找到 cli-proxy-api.exe（内置包缺失或没释放成功）"
    st = status()
    if st["online"]:
        return True, "已经在运行（%s）" % st["base_url"]
    cfg_port = int(port or st["port"] or DEFAULT_PORT)
    if port_busy(cfg_port):
        cfg_port = pick_port(cfg_port)
    ok, msg = write_config(port=cfg_port, key=key, force=(port is not None))
    if not ok:
        return False, msg
    logf = open(log_path(), "a", encoding="utf-8", errors="replace")
    try:
        _proc = subprocess.Popen(
            [exe, "--config", config_path()],
            cwd=data_dir(), stdout=logf, stderr=subprocess.STDOUT,
            creationflags=NO_WINDOW)
    except Exception as e:
        return False, "启动失败：%s" % str(e)[:120]
    for _ in range(30):
        time.sleep(0.5)
        if status()["code"] == 200 or port_busy(cfg_port):
            return True, "已启动 %s（PID %d）" % (
                "http://127.0.0.1:%d/v1" % cfg_port, _proc.pid)
    return False, "启动了但没监听端口，看 %s" % log_path()


def stop():
    """停止服务"""
    global _proc
    killed = []
    if _proc and _proc.poll() is None:
        try:
            _proc.terminate()
            killed.append(_proc.pid)
        except Exception:
            pass
        _proc = None
    # 兜底：按镜像名杀
    if os.name == "nt":
        try:
            r = subprocess.run(["tasklist", "/FI",
                                "IMAGENAME eq cli-proxy-api.exe"],
                               capture_output=True, text=True,
                               encoding="gbk", errors="replace")
            for pid in re.findall(r"(\d+)\s+\S+\s+\d", r.stdout or ""):
                if pid not in killed:
                    subprocess.run(["taskkill", "/F", "/PID", pid],
                                   capture_output=True)
                    killed.append(pid)
        except Exception:
            pass
    time.sleep(1.2)
    return killed


def restart(**kw):
    stop()
    return start(**kw)


# ------------------------------------------------------------------ 登录
def start_login(provider_id, no_browser=True, timeout=1800):
    """
    触发 OAuth 登录，返回 (ok, {provider, flag, url, user_code, raw, pid})
    授权链接从子进程 stdout 里抓 —— 各家格式不同，用正则兜底。
    """
    global _login_proc
    exe, _ = ensure_binary()
    if not exe or not os.path.exists(exe):
        return False, {"message": "没有找到 cli-proxy-api.exe"}
    spec = next((p for p in PROVIDERS if p["id"] == provider_id), None)
    if not spec:
        return False, {"message": "未知的登录方式：%s" % provider_id}
    st = status()
    if not st["online"]:
        ok, msg = start()
        if not ok:
            return False, {"message": "服务没起来，登录不了：%s" % msg}
    cmd = [exe, spec["flag"], "--config", config_path()]
    if no_browser:
        cmd.append("--no-browser")
    try:
        _login_proc = subprocess.Popen(
            cmd, cwd=data_dir(), stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, creationflags=NO_WINDOW)
    except Exception as e:
        return False, {"message": "启动登录失败：%s" % str(e)[:120]}

    url = ""
    code = ""
    raw = ""
    t0 = time.time()
    # 抓到 URL 后再多读几行 —— user code 通常紧跟在 URL 后面一行
    while time.time() - t0 < 25:
        if _login_proc.poll() is not None:
            break
        try:
            line = _login_proc.stdout.readline()
            if not line:
                break
            s = line.decode("utf-8", "replace").strip()
            raw += s + "\n"
            if not url:
                m = re.search(r"https?://\S+", s)
                # 排除日志里的版本号/文档链接，只认授权链接那一行
                if m and "visit" not in s.lower() and "github" not in m.group(0):
                    url = m.group(0).rstrip(".,;)")
            if not code:
                m = re.search(r"user code:\s*(\S+)", s, re.I)
                if m:
                    code = m.group(1)
                elif re.fullmatch(r"[A-Z0-9]{4}-[A-Z0-9]{4}", s):
                    code = s
            elif not url and re.search(r"https?://\S+", s):
                m = re.search(r"https?://\S+", s)
                if m:
                    url = m.group(0).rstrip(".,;)")
        except Exception:
            break
        if url and code:
            break
    return True, {
        "provider": spec["id"], "name": spec["name"],
        "flag": spec["flag"], "url": url, "user_code": code,
        "raw": raw[-2000:], "pid": _login_proc.pid,
        "note": "授权完成后凭据会自动落到 auths 目录，服务热加载不用重启",
    }


def login_status():
    """登录子进程是否还在跑 + 已产生的 auth 文件数"""
    running = bool(_login_proc and _login_proc.poll() is None)
    n = 0
    try:
        n = len([x for x in os.listdir(auth_dir())
                 if os.path.isfile(os.path.join(auth_dir(), x))])
    except OSError:
        pass
    return {"running": running, "auth_files": n}


def tail_log(n=80):
    p = log_path()
    if not os.path.exists(p):
        return ""
    try:
        with open(p, "r", encoding="utf-8", errors="replace") as f:
            return "".join(f.readlines()[-n:])
    except OSError:
        return ""
