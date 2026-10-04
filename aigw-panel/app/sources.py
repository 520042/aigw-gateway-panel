# -*- coding: utf-8 -*-
"""
接入源分类
==========
用户定的规则（2026-10-04）：

    「对于那种既有网页端也有桌面端的，统一按照本地 AI 来处理。」

所以分类**按是否有桌面客户端**划，而不是按「网页 / API」划：

  LOCAL  本地 AI   —— 有桌面客户端的。一律归这里，不管它有没有网页版。
  API    平台 API  —— 只有正规 OpenAI 兼容接口的云厂商。
  WEB    网页对话  —— 只有网页版、没有客户端的。

同一平台可以有多条接入记录（例如元宝既是 LOCAL 又是 WEB），
但**分类字段本身是唯一的**，由下表的 CATEGORY 决定。

分类依据（`HAS_DESKTOP` / `HAS_WEB` / `HAS_API` 三个事实列）：
  - `kind`  是排序用的主分类
  - `note`  说明为什么这么归，方便你复核
"""

# kind: LOCAL / API / WEB
# desktop: 有官方桌面客户端
# web:     有网页版
# api:     有正规 OpenAI 兼容 API（可填 API Key 直连）
# note:    归类理由
# sources: 面板里对应的平台 id 列表
CATEGORIES = [
    {
        "kind": "LOCAL",
        "name": "本地 AI",
        "icon": "◈",
        "desc": "有桌面客户端的 AI 应用。登录后用客户端的额度，"
                "既有网页版也有桌面版的统一归这里。",
        "color": "acc",
    },
    {
        "kind": "API",
        "name": "平台 API",
        "icon": "⇄",
        "desc": "云厂商的正规 OpenAI 兼容接口，有 API Key 和配额说明，"
                "适合程序化调用与批量化。",
        "color": "info",
    },
    {
        "kind": "WEB",
        "name": "网页对话",
        "icon": "☁",
        "desc": "只有网页版、没有客户端的对话产品。"
                "靠浏览器登录态取凭据，稳定性相对弱。",
        "color": "warn",
    },
]

# 平台 → 分类事实表
# 说明：这里只记**客观事实**（有没有客户端/网页/API），分类由规则推导。
PLATFORM_FACTS = {
    # ---------------- 有桌面客户端 → LOCAL ----------------
    "wb-gateway":        dict(desktop=True,  web=False, api=True,  provider="腾讯"),
    "wb-gateway-intl":   dict(desktop=True,  web=True,  api=True,  provider="腾讯"),
    "apk-codebuddy":     dict(desktop=True,  web=True,  api=True,  provider="腾讯"),
    "apk-codebuddy-cn":  dict(desktop=True,  web=True,  api=True,  provider="腾讯"),
    "apk-trae":          dict(desktop=True,  web=True,  api=False, provider="字节"),
    "apk-doubao":        dict(desktop=True,  web=True,  api=True,  provider="字节"),
    "apk-yuanbao":       dict(desktop=True,  web=True,  api=False, provider="腾讯"),
    "apk-go":            dict(desktop=True,  web=False, api=False, provider="腾讯"),
    "apk-nano":          dict(desktop=True,  web=True,  api=False, provider="360"),
    "apk-metaso":        dict(desktop=True,  web=True,  api=True,  provider="秘塔"),
    "apk-kuku":          dict(desktop=False, web=True,  api=False, provider="百度"),
    "apk-wps":           dict(desktop=True,  web=True,  api=False, provider="金山"),
    "apk-qwenwork":      dict(desktop=True,  web=True,  api=False, provider="阿里"),
    "apk-qoder":         dict(desktop=True,  web=True,  api=False, provider="阿里"),
    "apk-kimi":          dict(desktop=True,  web=True,  api=True,  provider="月之暗面"),
    "apk-raccoon":       dict(desktop=False, web=True,  api=False, provider="小浣熊"),
    "apk-loomy":         dict(desktop=True,  web=True,  api=False, provider="讯飞"),
    "apk-antigravity":   dict(desktop=True,  web=True,  api=True,  provider="Google"),
    "apk-coze":          dict(desktop=True,  web=True,  api=True,  provider="字节"),

    # ---------------- 纯 API ----------------
    "api-deepseek":      dict(desktop=False, web=True,  api=True,  provider="DeepSeek"),
    "api-zhipu":         dict(desktop=False, web=True,  api=True,  provider="智谱"),
    "api-dashscope":     dict(desktop=False, web=True,  api=True,  provider="阿里云"),
    "api-moonshot":      dict(desktop=False, web=True,  api=True,  provider="月之暗面"),
    "api-volcengine":    dict(desktop=False, web=True,  api=True,  provider="字节火山"),
    "api-minimax":       dict(desktop=False, web=True,  api=True,  provider="MiniMax"),
    "api-stepfun":       dict(desktop=False, web=True,  api=True,  provider="阶跃"),
    "api-baichuan":      dict(desktop=False, web=True,  api=True,  provider="百川"),
    "api-siliconflow":  dict(desktop=False, web=True,  api=True,  provider="硅基流动"),
    "api-groq":          dict(desktop=False, web=True,  api=True,  provider="Groq"),
    "api-openrouter":    dict(desktop=False, web=True,  api=True,  provider="OpenRouter"),
    "api-modelscope":    dict(desktop=False, web=True,  api=True,  provider="魔搭"),
    "api-bailian":       dict(desktop=False, web=True,  api=True,  provider="阿里百炼"),
    "api-xfyun":         dict(desktop=False, web=True,  api=True,  provider="讯飞星火"),
    "api-qianfan":       dict(desktop=False, web=True,  api=True,  provider="百度千帆"),
}

# 本机跑的反代也算 LOCAL（它是本机服务，不是云 API）
LOCAL_PROXIES = [
    {"id": "gateway", "name": "workbuddy-gateway",
     "desc": "CodeBuddy / WorkBuddy 的反代（腾讯，Go 1.24.5）",
     "endpoint": "http://127.0.0.1:8317/v1"},
    {"id": "localproxy", "name": "CLIProxyAPI",
     "desc": "Kimi / Codex / Claude / Antigravity 等 CLI 订阅反代",
     "endpoint": "http://127.0.0.1:8318/v1"},
]


def kind_of(pid):
    """
    按用户定的规则推导分类：
      有桌面客户端 → LOCAL（哪怕它同时有网页版）
      否则有正规 API → API
      否则 → WEB
    """
    if pid in ("gateway", "localproxy"):
        return "LOCAL"
    f = PLATFORM_FACTS.get(pid)
    if not f:
        return "WEB"
    if f.get("desktop"):
        return "LOCAL"
    if f.get("api"):
        return "API"
    return "WEB"


def facts_of(pid):
    return dict(PLATFORM_FACTS.get(pid) or {})


def bucket(pids):
    """把平台 id 按三类分桶，返回 {kind: [pid,...]}"""
    out = {"LOCAL": [], "API": [], "WEB": []}
    for p in pids or []:
        out[kind_of(p)].append(p)
    return out


def category_meta(kind):
    for c in CATEGORIES:
        if c["kind"] == kind:
            return c
    return {"kind": kind, "name": kind, "icon": "?", "desc": "", "color": ""}


def summary():
    """给前端 KPI 用：每类各有多少个平台"""
    allp = sorted(set(list(PLATFORM_FACTS.keys())
                      + [x["id"] for x in LOCAL_PROXIES]))
    b = bucket(allp)
    return {
        "LOCAL": len(b["LOCAL"]),
        "API": len(b["API"]),
        "WEB": len(b["WEB"]),
        "total": len(allp),
    }


if __name__ == "__main__":
    import json
    import sys
    sys.path.insert(0, __import__("os").path.dirname(
        __import__("os").path.abspath(__file__)))
    from app import gwlogin as GL

    live = sorted(GL.PLATFORMS.keys())
    print("=" * 76)
    print("按「有桌面客户端 → LOCAL」的规则给现有 %d 个登录平台分类：" % len(live))
    b = bucket(live)
    for kind in ("LOCAL", "API", "WEB"):
        meta = category_meta(kind)
        print()
        print("【%s】%s —— %d 个" % (kind, meta["name"], len(b[kind])))
        for p in b[kind]:
            f = facts_of(p)
            marks = "".join([
                "桌" if f.get("desktop") else "  ",
                "网" if f.get("web") else "  ",
                "API" if f.get("api") else "   "])
            print("   %-20s [%s] %s" % (p, marks, f.get("provider", "")))
    print()
    print("汇总:", json.dumps(summary(), ensure_ascii=False))
