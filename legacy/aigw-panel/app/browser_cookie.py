# -*- coding: utf-8 -*-
"""
本机浏览器 Cookie 读取（Chrome / Edge / 360 / QQ 浏览器）
========================================================
用途：让「Cookie 类网关」（豆包 / 元宝 / 小浣熊 / Trae 网页版）做到
      **一键登录** —— 用户在浏览器里登过，面板直接把 Cookie 取出来用，
      不需要手工复制粘贴。

加密结构（Chromium 系）：
  Local State (JSON) → os_crypt.encrypted_key → base64 → 去掉前 5 字节 "DPAPI"
                     → CryptUnprotectData（DPAPI，绑定当前 Windows 用户）
                     → AES-256-GCM 主密钥（32 字节）
  Cookies (SQLite)   → cookies.encrypted_value → "v10"/"v11" + 12B nonce
                     + 密文 + 16B tag → AES-256-GCM 解密

全程标准库 + ctypes，不引入第三方依赖（AES-GCM 见 app/aesgcm.py）。

已知限制：
  Chrome 127+ 引入 app-bound 加密（"v20" 前缀），密钥不再能被普通进程
  导出。遇到时返回明确错误码 app_bound，前端降级为「手动粘贴 Cookie」。
"""

import base64
import ctypes
import json
import os
import shutil
import sqlite3
import tempfile
import time
from ctypes import wintypes

from .aesgcm import aes_gcm_decrypt

# ------------------------------------------------------------------ DPAPI

class _DATA_BLOB(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD),
                ("pbData", ctypes.POINTER(ctypes.c_char))]


def _dpapi_decrypt(blob):
    """Windows DPAPI 解密，只能在加密时的同一 Windows 账号下成功"""
    if not blob:
        raise ValueError("空的 DPAPI 数据")
    buf = ctypes.create_string_buffer(blob, len(blob))
    indata = _DATA_BLOB(len(blob), buf)
    outdata = _DATA_BLOB()
    ok = ctypes.windll.crypt32.CryptUnprotectData(
        ctypes.byref(indata), None, None, None, None, 0, ctypes.byref(outdata))
    if not ok:
        raise OSError("CryptUnprotectData 失败（errno=%d）" % ctypes.GetLastError())
    try:
        return ctypes.string_at(outdata.pbData, outdata.cbData)
    finally:
        ctypes.windll.kernel32.LocalFree(outdata.pbData)


# ------------------------------------------------------------------ 浏览器定位

BROWSERS = [
    ("chrome", "Google/Chrome/User Data"),
    ("edge", "Microsoft/Edge/User Data"),
    ("360chrome", "360Chrome/Chrome/User Data"),
    ("360speed", "360ChromeX/Chrome/User Data"),
    ("qqbrowser", "Tencent/QQBrowser/User Data"),
    ("brave", "BraveSoftware/Brave-Browser/User Data"),
    ("chromium", "Chromium/User Data"),
]


def _user_data_roots():
    roots = []
    local = os.path.expandvars(r"%LOCALAPPDATA%")
    roaming = os.path.expandvars(r"%APPDATA%")
    for base in (local, roaming):
        if os.path.isdir(base):
            roots.append(base)
    return roots


def list_profiles(browser=None):
    """列出本机可用的浏览器配置（含 Cookie 文件与 Local State）"""
    out = []
    for name, rel in BROWSERS:
        if browser and name != browser:
            continue
        for root in _user_data_roots():
            ud = os.path.join(root, rel)
            ls = os.path.join(ud, "Local State")
            if not (os.path.isdir(ud) and os.path.isfile(ls)):
                continue
            for sub in sorted(os.listdir(ud)):
                if not (sub == "Default" or sub.startswith("Profile ")):
                    continue
                ck = os.path.join(ud, sub, "Network", "Cookies")
                if not os.path.isfile(ck):
                    ck2 = os.path.join(ud, sub, "Cookies")
                    ck = ck2 if os.path.isfile(ck2) else ""
                if not ck:
                    continue
                out.append({"browser": name, "profile": sub,
                            "cookies": ck, "local_state": ls})
    return out


def master_key(local_state_path):
    """从 Local State 取出并解密 AES-GCM 主密钥"""
    with open(local_state_path, "r", encoding="utf-8", errors="replace") as f:
        data = json.load(f)
    enc_b64 = ((data.get("os_crypt") or {}).get("encrypted_key") or "")
    if not enc_b64:
        raise ValueError("Local State 中没有 os_crypt.encrypted_key")
    raw = base64.b64decode(enc_b64)
    if raw[:5] != b"DPAPI":
        raise ValueError("未知的密钥封装格式（前 5 字节不是 DPAPI）")
    return _dpapi_decrypt(raw[5:])


# ------------------------------------------------------------------ Cookie 解密

def decrypt_value(encrypted_value, key):
    """解密单条 Cookie 值，返回 str"""
    if not encrypted_value:
        return ""
    # 明文存储（老版本 / 非加密）
    if encrypted_value[:1] not in (b"v",):
        try:
            return encrypted_value.decode("utf-8")
        except UnicodeDecodeError:
            return ""
    prefix = encrypted_value[:3]
    if prefix in (b"v10", b"v11"):
        body = encrypted_value[3:]
        if len(body) < 28:
            return ""
        nonce, ct, tag = body[:12], body[12:-16], body[-16:]
        try:
            return aes_gcm_decrypt(key, nonce, ct, tag).decode("utf-8", "replace")
        except ValueError:
            return ""
    if prefix == b"v20":
        # Chrome 127+ app-bound 加密，普通进程无法解密
        return ""
    # 更早的版本：整块走 DPAPI
    try:
        return _dpapi_decrypt(encrypted_value).decode("utf-8", "replace")
    except Exception:
        return ""


def _copy_db(path):
    """SQLite 文件可能被浏览器锁住，先复制到临时文件再读"""
    tmp = os.path.join(tempfile.gettempdir(),
                       "aigw_cookies_%d.db" % (int(time.time() * 1000) % 10 ** 9))
    shutil.copy2(path, tmp)
    return tmp


def read_cookies(cookies_path, key, host_filter=None):
    """
    读取 Cookie 库。host_filter 为域名后缀列表（如 ["doubao.com"]），
    为 None 时返回全部。返回 [{"host","name","value","path","expires",...}]
    """
    tmp = _copy_db(cookies_path)
    rows = []
    try:
        con = sqlite3.connect(tmp)
        try:
            cur = con.cursor()
            sql = ("SELECT host_key, name, value, encrypted_value, path, "
                   "expires_utc, is_secure, is_httponly FROM cookies")
            cur.execute(sql)
            for host, name, plain, enc, path, exp, sec, http in cur.fetchall():
                if host_filter and not any(
                        host == h or host.endswith("." + h) for h in host_filter):
                    continue
                val = decrypt_value(enc, key) if enc else (plain or "")
                if not val:
                    val = plain or ""
                rows.append({"host": host, "name": name, "value": val,
                             "path": path or "/", "expires": exp or 0,
                             "secure": bool(sec), "httponly": bool(http)})
        finally:
            con.close()
    finally:
        try:
            os.remove(tmp)
        except OSError:
            pass
    return rows


def cookie_header(rows):
    """把 cookie 列表拼成请求头可用的字符串"""
    return "; ".join("%s=%s" % (r["name"], r["value"])
                     for r in rows if r.get("value"))


def diagnose():
    """
    只读诊断：本机浏览器 Cookie 到底能不能自动取出。
    不开浏览器、不启动任何外部进程，供面板告知用户该走哪条路。
    """
    out = {"profiles": [], "usable": False, "auto_supported": False, "reason": ""}
    try:
        profs = list_profiles()
    except Exception as e:
        out["reason"] = "枚举浏览器失败：%s" % e
        return out
    if not profs:
        out["reason"] = "本机未找到 Chromium 系浏览器"
        return out

    for p in profs:
        item = {"browser": p["browser"], "profile": p["profile"],
                "key_ok": False, "total": 0, "v20": 0, "v11": 0,
                "v10": 0, "plain": 0, "error": ""}
        try:
            key = master_key(p["local_state"])
            item["key_ok"] = True
        except Exception as e:
            item["error"] = "主密钥解密失败：%s" % e
            out["profiles"].append(item)
            continue
        tmp = None
        try:
            tmp = _copy_db(p["cookies"])
            con = sqlite3.connect(tmp)
            cur = con.cursor()
            cur.execute("SELECT encrypted_value FROM cookies")
            for (ev,) in cur.fetchall():
                item["total"] += 1
                if not ev:
                    item["plain"] += 1
                elif ev[:3] == b"v20":
                    item["v20"] += 1
                elif ev[:3] == b"v11":
                    item["v11"] += 1
                elif ev[:3] == b"v10":
                    item["v10"] += 1
                else:
                    item["plain"] += 1
            con.close()
        except Exception as e:
            item["error"] = "读取失败：%s" % e
        finally:
            if tmp:
                try:
                    os.remove(tmp)
                except OSError:
                    pass
        out["profiles"].append(item)

    decryptable = sum(x["v10"] + x["v11"] + x["plain"] for x in out["profiles"])
    out["auto_supported"] = decryptable > 0
    if out["auto_supported"]:
        out["usable"] = True
        out["reason"] = "可直读 %d 条（老版本加密格式）" % decryptable
    else:
        out["reason"] = ("浏览器使用了 app-bound 加密（v20），普通进程无法解密；"
                         "请用「CDP 一键获取」或手动粘贴 Cookie")
    return out


def fetch(hosts, browser=None, profile=None):
    """
    高层接口：按域名取 Cookie。
    返回 {"ok":bool, "source":str, "cookie":str, "count":int,
          "values":{name:value}, "error":str}
    """
    hosts = [h.strip().lower() for h in (hosts or []) if h.strip()]
    if not hosts:
        return {"ok": False, "error": "未指定域名"}
    try:
        profs = list_profiles(browser)
    except Exception as e:
        return {"ok": False, "error": "枚举浏览器失败：%s" % e}
    if profile:
        profs = [p for p in profs if p["profile"] == profile] or profs

    last_err = "本机未找到可用的 Chromium 系浏览器配置"
    for p in profs:
        try:
            key = master_key(p["local_state"])
        except Exception as e:
            last_err = "%s/%s 取主密钥失败：%s" % (p["browser"], p["profile"], e)
            continue
        try:
            rows = read_cookies(p["cookies"], key, hosts)
        except Exception as e:
            last_err = "%s/%s 读 Cookie 失败：%s" % (p["browser"], p["profile"], e)
            continue
        rows = [r for r in rows if r.get("value")]
        if not rows:
            last_err = ("%s/%s 中未找到 %s 的登录 Cookie"
                        % (p["browser"], p["profile"], "/".join(hosts)))
            continue
        return {
            "ok": True,
            "source": "%s / %s" % (p["browser"], p["profile"]),
            "cookie": cookie_header(rows),
            "count": len(rows),
            "values": {r["name"]: r["value"] for r in rows},
        }
    return {"ok": False, "error": last_err}


if __name__ == "__main__":
    import sys
    hosts = sys.argv[1:] or ["doubao.com"]
    print(fetch(hosts))
