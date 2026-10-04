# -*- coding: utf-8 -*-
"""
中转站签到引擎
==============
支持 New API / One API 系公益站的每日签到。

实测接口（New API 5.x）：
  GET  /api/user/checkin/status  → {data:{enabled, stats:{checked_in_today,
                                          consecutive_days,total_days},
                                      checkin_history:[...]}}
  POST /api/user/checkin          → {data:{consecutive_days, reward}}
鉴权方式：
  Cookie 直传         → Cookie 头
  Access Token        → new-api-user 头 + Authorization: Bearer <token>
  账号密码自动登录    → POST /api/user/login {username,password} 换 session

部分站点签到路径不是 /api/user/checkin（AnyRouter= /api/user/sign_in 等），
因此每个站点支持自定义 checkin_path。
"""

import json
import ssl
import urllib.error
import urllib.parse
import urllib.request

TIMEOUT = 20
_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36"


class SiteError(Exception):
    pass


def _norm_origin(url):
    u = url.strip().rstrip("/")
    if not u.startswith("http"):
        u = "https://" + u
    return u


def _req(origin, path, method="GET", payload=None, headers=None, timeout=TIMEOUT,
         use_json_body=True):
    url = origin + path
    data = None
    hdrs = {"User-Agent": _UA, "Accept": "application/json, text/plain, */*"}
    if payload is not None:
        if use_json_body:
            data = json.dumps(payload).encode("utf-8")
            hdrs["Content-Type"] = "application/json"
        else:
            data = urllib.parse.urlencode(payload).encode("utf-8")
            hdrs["Content-Type"] = "application/x-www-form-urlencoded"
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    ctx = ssl.create_default_context()
    opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx))
    try:
        with opener.open(req, timeout=timeout) as r:
            raw = r.read().decode("utf-8", "replace")
            code = r.status
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        code = e.code
    except urllib.error.URLError as e:
        raise SiteError("无法连接 %s：%s" % (origin, e.reason))
    try:
        return code, json.loads(raw)
    except json.JSONDecodeError:
        return code, {"_raw": raw[:600]}


def _auth_headers(site):
    h = {}
    token = (site.get("token") or "").strip()
    if token:
        if not token.lower().startswith("bearer "):
            h["Authorization"] = "Bearer " + token
        else:
            h["Authorization"] = token
        if site.get("api_user"):
            h["new-api-user"] = str(site["api_user"])
    cookie = (site.get("cookie") or "").strip()
    if cookie:
        h["Cookie"] = cookie
    return h


def _try_login(origin, site):
    """账号密码登录换 Cookie，返回新 cookie 字符串或 None"""
    user = (site.get("username") or "").strip()
    pwd = (site.get("password") or "").strip()
    if not user or not pwd:
        return None
    code, body = _req(origin, "/api/user/login", "POST",
                       {"username": user, "password": pwd})
    if code != 200 or not isinstance(body, dict) or not body.get("success"):
        raise SiteError("登录失败：%s" % _msg(body, code))
    # 多数 NewAPI 返回 data.session 或直接下发 Set-Cookie
    return (body.get("data") or {}).get("session")


def _msg(body, code=200):
    if isinstance(body, dict):
        m = body.get("message") or body.get("msg") or body.get("error")
        if isinstance(m, str) and m:
            return m
        if isinstance(m, dict):
            return m.get("message") or json.dumps(m, ensure_ascii=False)[:200]
        if body.get("_raw"):
            return body["_raw"][:200]
    return "HTTP %s" % code


def probe_site(site):
    """探测站点能力：是否 NewAPI 系、是否支持签到、账号是否有效"""
    origin = _norm_origin(site.get("url") or "")
    out = {"url": origin, "reachable": False, "newapi": False,
           "checkin_supported": False, "auth_ok": False, "message": ""}

    # 1) 探活 + 版本
    try:
        code, body = _req(origin, "/api/status", timeout=10)
        if code == 200 and isinstance(body, dict) and body.get("data"):
            out["reachable"] = True
            out["newapi"] = True
            d = body["data"]
            out["version"] = d.get("version") or ""
            out["system_name"] = d.get("system_name") or d.get("name") or ""
            out["quota_per_unit"] = d.get("quota_per_unit")
    except SiteError as e:
        out["message"] = str(e)
        return out

    # 2) 鉴权态探测
    hdrs = _auth_headers(site)
    try:
        code, body = _req(origin, "/api/user/self", headers=hdrs, timeout=10)
        if code == 200 and isinstance(body, dict) and body.get("success"):
            out["auth_ok"] = True
            u = body.get("data") or {}
            out["username"] = u.get("username")
            out["quota"] = u.get("quota")
            out["used_quota"] = u.get("used_quota")
            out["checkin_enabled"] = u.get("checkin_enabled")
    except SiteError as e:
        out["message"] = str(e)

    # 3) 签到能力探测
    cpath = site.get("checkin_path") or "/api/user/checkin"
    try:
        code, body = _req(origin, cpath + "/status", headers=hdrs, timeout=10)
        if code == 200 and isinstance(body, dict) and body.get("success"):
            out["checkin_supported"] = True
            stats = ((body.get("data") or {}).get("stats") or {})
            out["checked_in_today"] = stats.get("checked_in_today")
            out["consecutive_days"] = stats.get("consecutive_days")
            out["total_days"] = stats.get("total_days")
    except SiteError:
        pass

    if not out["message"]:
        if out["auth_ok"]:
            out["message"] = "账号有效"
        elif not (site.get("token") or site.get("cookie") or site.get("password")):
            out["message"] = "站点可达，但未配置凭据"
        else:
            out["message"] = "站点可达，凭据可能已失效"
    return out


def checkin_site(site):
    """执行签到，返回结构化结果（不抛异常，异常收进 ok/message）"""
    origin = _norm_origin(site.get("url") or "")
    name = site.get("name") or origin
    cpath = site.get("checkin_path") or "/api/user/checkin"
    hdrs = _auth_headers(site)
    res = {"site": site.get("id"), "name": name, "ok": False, "message": "",
           "reward": None, "streak": None, "skipped": False}

    if site.get("auto") is False:
        res["skipped"] = True
        res["message"] = "已禁用自动签到"
        return res

    # 已签过就跳过（除非 force）
    if not site.get("force"):
        st = probe_site(site)
        if st.get("checked_in_today"):
            res["skipped"] = True
            res["message"] = "今日已签到（连续 %s 天）" % st.get("consecutive_days")
            res["streak"] = st.get("consecutive_days")
            return res
        if not st.get("auth_ok") and (site.get("username") and site.get("password")):
            sess = _try_login(origin, site)
            if sess:
                hdrs["Cookie"] = sess
                hdrs.pop("new-api-user", None)

    try:
        code, body = _req(origin, cpath, "POST", {}, headers=hdrs)
    except SiteError as e:
        res["message"] = str(e)
        return res

    if code == 200 and isinstance(body, dict):
        if body.get("success"):
            d = body.get("data") or {}
            res["ok"] = True
            res["reward"] = d.get("reward") or d.get("quota") or body.get("reward")
            res["streak"] = d.get("consecutive_days")
            res["message"] = body.get("message") or _msg(body) or "签到成功"
        else:
            res["message"] = _msg(body, code)
    else:
        res["message"] = _msg(body, code) if isinstance(body, dict) else "HTTP %s" % code
    return res


def fetch_balance(site):
    """取额度 / 余额 / 用量，统一换算成 credit"""
    origin = _norm_origin(site.get("url") or "")
    hdrs = _auth_headers(site)
    out = {"site": site.get("id"), "name": site.get("name"), "ok": False}
    try:
        code, body = _req(origin, "/api/user/self", headers=hdrs)
        if code == 200 and isinstance(body, dict) and body.get("success"):
            u = body.get("data") or {}
            quota = u.get("quota")
            used = u.get("used_quota") or 0
            # NewAPI: 500000 quota = $1（quota_per_unit 通常 500000）
            per = 500000.0
            try:
                c, b = _req(origin, "/api/status")
                if b.get("data", {}).get("quota_per_unit"):
                    per = float(b["data"]["quota_per_unit"])
            except SiteError:
                pass
            out.update({
                "ok": True,
                "username": u.get("username"),
                "quota": quota,
                "used_quota": used,
                "credit": round((quota or 0) / per, 4) if per else None,
                "used_credit": round(used / per, 4) if per else None,
                "request_count": u.get("request_count"),
                "group": u.get("group"),
            })
        else:
            out["message"] = _msg(body, code)
    except SiteError as e:
        out["message"] = str(e)
    return out


def fetch_models(site):
    """取站点模型列表（需管理端 token，失败返回空）"""
    origin = _norm_origin(site.get("url") or "")
    hdrs = _auth_headers(site)
    try:
        code, body = _req(origin, "/api/user/self/groups", headers=hdrs)
        if code == 200 and isinstance(body, dict) and body.get("success"):
            return {"ok": True, "groups": body.get("data")}
    except SiteError:
        pass
    return {"ok": False, "groups": {}}
