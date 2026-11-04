# -*- coding: utf-8 -*-
"""
Trae 本机登录态自动读取（2026-10-06 新增，解决 web-trae「手动抠 x-ide-token」痛点）

背景
----
原 web-trae 登录是 file 方式：要求用户手动从 Trae 的 leveldb / Electron
LocalStorage 抠出 x-ide-token 再粘贴。对普通用户极不友好，也是「登录不好用」的
典型。本模块把这一步自动化：**在用户本机（运行面板的那台电脑）直接定位 Trae 的
本地登录态并提取 token**，点一下即可，无需 F12 / leveldb。

实现要点
--------
1. 纯标准库（与项目一致，零第三方依赖）。
2. 跨平台定位 Trae 的 userData 目录（Windows / macOS / Linux 多候选名）。
3. 优先读 Trae 自带的 storage.json（trae-minimax-client 实证位置），再回退扫描
   Electron Local Storage 的 leveldb 日志，提取 x-ide-token / iCubeAuthInfo /
   authInfo / ideToken 等已知键。
4. 只「读」不「写」，不触碰任何加密存储（Chrome127+ app-bound 那种解密一律不做）。

⚠ 真机验证提示
--------------
不同 Trae 版本存储位置/字段可能变化。若你的 Trae 把 token 存在别的路径，把路径
告诉我即可补进 CANDIDATE_DIRS / AUTH_KEYS。本模块失败时会把「找到的候选目录」
一并返回，方便你反馈。

用法
----
    from app import trae_local as TL
    okk, info = TL.read_trae_token()
    # info = {"token": "...", "account": "...", "source": "...", "note": "..."}
"""
import json
import os
import re

# Trae 在不同系统/版本下的 userData 目录候选（按常见度排序）。
# Windows: %APPDATA% 通常是 C:\\Users\\<你>\\AppData\\Roaming
# macOS:   ~/Library/Application Support
# Linux:   ~/.config 或 ~/.local/share 或 ~/.trae
_CANDIDATE_REL = [
    "Trae CN", "Trae", "Trae Global", "Trae Studio", "Trae CN/User",
    "Trae/User", ".trae", "Trae Global/User",
]

# storage.json / LocalStorage 里可能出现的鉴权字段（按优先级）。
AUTH_KEYS = (
    "x-ide-token", "ideToken", "authToken", "token", "accessToken",
    "iCubeAuthInfo", "authInfo", "auth_info", "Authorization",
)

# Local Storage leveldb 日志里 token 的常见前缀（Electron 以 <key>\t<value> 落盘）。
_LDB_KEY_RE = re.compile(
    r"(?:x-ide-token|ideToken|authToken|iCubeAuthInfo|authInfo)\s*[\t=]\s*([^\n\r\t]{16,4000})")


def _candidate_dirs():
    dirs = []
    env = {
        "win": os.environ.get("APPDATA"),
        "mac": os.path.expanduser("~/Library/Application Support"),
        "linux_config": os.path.expanduser("~/.config"),
        "linux_share": os.path.expanduser("~/.local/share"),
        "home": os.path.expanduser("~"),
    }
    for base in env.values():
        if not base or not os.path.isdir(base):
            continue
        for rel in _CANDIDATE_REL:
            d = os.path.join(base, rel)
            if os.path.isdir(d):
                dirs.append(d)
    # 去重保序
    seen, out = set(), []
    for d in dirs:
        if d not in seen:
            seen.add(d)
            out.append(d)
    return out


def _parse_storage_json(path):
    """读 Trae 的 storage.json，返回 (token, account) 或 (None, None)。"""
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            data = json.load(f)
    except Exception:
        return None, None
    token, account = _search_auth(data)
    return token, account


def _search_auth(obj, _depth=0):
    """递归在 dict/list/str 里找 AUTH_KEYS 对应的值，顺带抓账号名。"""
    if _depth > 12:
        return None, None
    account = None
    if isinstance(obj, dict):
        # 账号名线索
        for k in ("account", "email", "username", "nick", "userId", "user",
                  "name", "loginName"):
            v = obj.get(k)
            if isinstance(v, str) and v and "@" in v or (isinstance(v, str) and len(v) < 60):
                if not account:
                    account = v
        for k, v in obj.items():
            kl = str(k).lower()
            if kl in AUTH_KEYS and isinstance(v, str) and len(v) >= 16:
                # 若是 JSON 字符串（iCubeAuthInfo 常是 JSON），尝试再解一层
                tok = _maybe_json_token(v)
                if tok:
                    return tok, account
            if isinstance(v, (dict, list)):
                t, a = _search_auth(v, _depth + 1)
                if t:
                    return t, a or account
    elif isinstance(obj, list):
        for v in obj:
            t, a = _search_auth(v, _depth + 1)
            if t:
                return t, a
    return None, account


def _maybe_json_token(v):
    """v 可能是裸 token，也可能是含 token 字段的 JSON 字符串。"""
    if isinstance(v, str) and len(v) >= 16:
        try:
            d = json.loads(v)
            if isinstance(d, dict):
                for k in ("token", "accessToken", "x-ide-token", "ideToken",
                          "authToken", "access_token"):
                    tv = d.get(k)
                    if isinstance(tv, str) and len(tv) >= 16:
                        return tv
        except Exception:
            pass
        return v
    return None


def _scan_leveldb(dir_path):
    """扫描 Electron Local Storage 的 leveldb 日志，提取 token。"""
    ldb = os.path.join(dir_path, "Local Storage", "leveldb")
    if not os.path.isdir(ldb):
        return None, None
    token, account = None, None
    try:
        for fn in os.listdir(ldb):
            if not fn.endswith(".log"):
                continue
            with open(os.path.join(ldb, fn), "r", encoding="utf-8",
                      errors="replace") as f:
                for line in f:
                    if "x-ide-token" in line or "iCubeAuthInfo" in line \
                            or "authInfo" in line or "ideToken" in line:
                        m = _LDB_KEY_RE.search(line)
                        if m:
                            token = m.group(1).strip().strip('"').strip("'")
                            if token and len(token) >= 16:
                                return token, account
    except Exception:
        pass
    return token, account


def read_trae_token():
    """自动读取本机 Trae 登录态。

    返回 (ok, info)。
      ok=True  : info={"token","account","source"}
      ok=False : info={"error","candidates":[...]"}（把找到的候选目录一并返回，
                 方便在真机上定位存储位置）
    """
    dirs = _candidate_dirs()
    if not dirs:
        return False, {
            "error": "未在当前系统找到 Trae 的用户数据目录（已排查 Windows "
                     "AppData / macOS Application Support / Linux ~/.config、"
                     "~/.local/share、~/.trae 下的 Trae/Trae CN 等候选）。",
            "candidates": [],
            "hint": "请确认本机已安装并至少登录过一次 Trae；或告诉我它的安装路径。",
        }

    for d in dirs:
        # 1) storage.json（Trae 自身持久化，trae-minimax-client 实证）
        sj = os.path.join(d, "storage.json")
        if os.path.isfile(sj):
            tok, acc = _parse_storage_json(sj)
            if tok:
                return True, {"token": tok, "account": acc or "Trae 本机登录态",
                              "source": "storage.json @ %s" % sj}
        # 2) User/storage.json（部分版本在 User 子目录）
        sj2 = os.path.join(d, "User", "storage.json")
        if os.path.isfile(sj2):
            tok, acc = _parse_storage_json(sj2)
            if tok:
                return True, {"token": tok, "account": acc or "Trae 本机登录态",
                              "source": "User/storage.json @ %s" % sj2}
        # 3) Electron Local Storage leveldb 日志兜底
        tok, acc = _scan_leveldb(d)
        if tok:
            return True, {"token": tok, "account": acc or "Trae 本机登录态",
                          "source": "Local Storage leveldb @ %s" % d}

    return False, {
        "error": "在已找到的 Trae 目录里没能提取到登录 token（storage.json / "
                 "Local Storage 都试过）。可能你的版本改了存储位置或字段名。",
        "candidates": dirs,
        "hint": "把下面某个目录路径发我，我帮你确认 token 存在哪个键。",
    }


if __name__ == "__main__":
    okk, info = read_trae_token()
    print(json.dumps({"ok": okk, **info}, ensure_ascii=False, indent=2))
