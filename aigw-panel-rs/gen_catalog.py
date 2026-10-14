#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
生成 catalog_bundled.json —— Rust 面板「模型总表(grouped)」与「接入源清单」的
单一数据源。所有数据均从 Python 一手模块导出，**不手抄**：
  - app/bundled_models.py · MODELS        (WorkBuddy/CodeBuddy 国内/国际内置目录)
  - app/bundled_trae.py  · MODELS         (Trae 国内)
  - app/bundled_go.py    · MODELS         (Go 网关)
  - app/bundled_yuanbao.py · MODELS       (元宝)
  - app/doubao_relay.py · MODE_FLAGS       (豆包 samantha 原生中继裸名)
  - app/native_relay.py · RELAY_SPECS     (网页反代 + 免凭据车道的静态清单)
  - src/platforms.json                    (gwlogin.PLATFORMS 导出快照)
导出后 Rust 用 include_str! 内嵌，ca 与 Python 同源、零手写漂移。
"""
import json, os, sys

ROOT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "aigw-panel")
sys.path.insert(0, ROOT)
HERE = os.path.dirname(os.path.abspath(__file__))


def dump(path, obj):
    with open(os.path.join(HERE, path), "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False, indent=1)
    print("wrote", path, "(", len(obj) if hasattr(obj, "__len__") else "?", ")")


# 1) 平台目录（直接从 gwlogin.PLATFORMS 一手导出，含 login_url/hosts/placeholder 等全部字段）
from app.gwlogin import PLATFORMS as _PLATFORMS
platforms = _PLATFORMS

# 1.5) 接入源分类事实表（app/sources.py 的 CATEGORIES / PLATFORM_FACTS / LOCAL_PROXIES）
sources_meta = {"categories": [], "platform_facts": {}, "local_proxies": []}
try:
    from app import sources as _SRC
    sources_meta = {
        "categories": _SRC.CATEGORIES,
        "platform_facts": _SRC.PLATFORM_FACTS,
        "local_proxies": _SRC.LOCAL_PROXIES,
    }
except Exception as e:
    print("sources import failed:", e)

# 1.6) 公益站 / 站点清单（app/catalog.all_sites，前端「公益中转站」卡片数据源）
catalog_sites = []
try:
    from app.catalog import all_sites as _all_sites
    catalog_sites = _all_sites()
except Exception as e:
    print("all_sites import failed:", e)

# 2) bundled 模型清单
import app.bundled_models as BM
import app.bundled_trae as BT
import app.bundled_go as BG
import app.bundled_yuanbao as BY
import app.doubao_relay as DR

bundled = {
    "wb-gateway": BM.MODELS,          # 国内站内置目录（ fallback；在线优先）
    "wb-gateway-intl": BM.MODELS,     # 国际站内置目录
    "apk-trae": BT.MODELS,
    "apk-go": BG.MODELS,
    "apk-yuanbao": BY.MODELS,
    "apk-doubao": [{"id": m, "name": "豆包 · " + m, "desc": "原生中继"}
                   for m in DR.MODE_FLAGS if m != "auto"],
}

# 3) 免凭据车道 + 网页反代静态清单（native_relay.RELAY_SPECS 的静态字段）
relay_specs = {}
free_lanes = {}
try:
    import app.native_relay as NR
    for pid, spec in NR.RELAY_SPECS.items():
        relay_specs[pid] = {
            "name": spec.get("name") or pid,
            "base": spec.get("base"),
            "path": spec.get("path"),
            "auth": spec.get("auth"),
            "secret_fixed": spec.get("secret_fixed"),
            "web_relay": bool(spec.get("web_relay")),
            "free": bool(spec.get("free")),
            "models_static": spec.get("models_static") or [],
        }
        if spec.get("models_anon"):
            free_lanes[pid] = {
                "name": spec.get("name") or pid,
                "models_anon": spec.get("models_anon"),
                # 状态说明（如 anon-zen 2026-10-14 已被上游按账号级收紧）
                "status_note": spec.get("status_note") or "",
                "pending_live": bool(spec.get("pending_live")),
            }
except Exception as e:
    print("native_relay import failed:", e)

# 4) Pollinations 免密钥池（已在 Phase-2 实测可用，作为常驻兜底）
if "pollinations" not in free_lanes:
    free_lanes["pollinations"] = {
        "name": "Pollinations 免费池",
        "models_anon": [
            {"id": "gpt-oss-20b", "name": "gpt-oss-20b", "desc": "OpenCode 开源权重 · 免密钥"},
            {"id": "openai", "name": "openai", "desc": "OpenAI 兼容 · 免密钥"},
        ],
    }

catalog = {
    "platforms": platforms,
    "sources_meta": sources_meta,
    "catalog_sites": catalog_sites,
    "bundled": bundled,
    "relay_specs": relay_specs,
    "free_lanes": free_lanes,
}

dump("catalog_bundled.json", catalog)
print("platforms:", len(platforms))
print("platform_facts:", len(sources_meta.get("platform_facts", {})))
print("catalog_sites:", len(catalog_sites))
print("bundled keys:", list(bundled.keys()))
print("relay_specs:", len(relay_specs))
print("free_lanes:", list(free_lanes.keys()))
