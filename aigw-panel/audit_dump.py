# -*- coding: utf-8 -*-
"""盘点面板侧四大维度定义：模型 / 登录 / 模型获取 / 签到"""
import sys, os, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from app import gwextra, gwlogin, autocheckin, upstreams

out = {}

# ---- 1. 动作表 ----
acts = {}
for plat, a in (gwextra.ACTIONS or {}).items():
    acts[plat] = [{
        "id": x.get("id"), "name": x.get("name"),
        "method": x.get("method", "GET"), "path": x.get("path"),
        "base": x.get("base"), "verified": x.get("verified"),
        "unverified": x.get("unverified"), "local": x.get("local"),
        "need": x.get("need"), "need_query": x.get("need_query"),
        "query": x.get("query"), "body_form": x.get("body_form"),
        "ok_keys": x.get("ok_keys"),
    } for x in (a or [])]
out["actions"] = acts
out["base"] = dict(gwextra.BASE or {})

# ---- 2. 登录平台 ----
plats = {}
for k, v in (gwlogin.PLATFORMS or {}).items():
    plats[k] = {
        "name": v.get("name"), "method": v.get("method"),
        "hosts": v.get("hosts"), "login_url": v.get("login_url"),
        "gateway": v.get("gateway"), "upstream": v.get("upstream"),
        "verify": v.get("verify"), "verify_pending": v.get("verify_pending"),
    }
out["platforms"] = plats

# ---- 3. 自动签到 ----
out["checkin"] = dict(getattr(autocheckin, "PLATFORM_CHECKIN", None) or {})

# ---- 4. 模型目录 ----
out["catalogs"] = dict(getattr(gwextra, "MODEL_CATALOGS", {}) or {})

print(json.dumps(out, ensure_ascii=False, indent=1, default=str))
