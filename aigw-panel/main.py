# -*- coding: utf-8 -*-
"""
AI 资源整合网关面板 — 主程序 / EXE 入口
=======================================
功能：
  1. 网关进程托管（自动探测 + 拉起 workbuddy-gateway）
  2. 账号池管理（读取 gateway credentials + 外部中转站账号）
  3. 每日签到中心（本地账号池 + NewAPI 系中转站 + 补签/抽奖/旅行）
  4. 任务中心（自定义任务 + 内置任务一键执行）
  5. 用量看板（tokens / 请求数 / 免费付费分布 / 时序曲线 / 模型价格）
  6. 免费资源导航（公益站 + 官方免费额度平台，含注册链接与签到能力）
  7. 通知中心（企微/钉钉/飞书/PushPlus/Server酱/Bark/自定义）
  8. 本面板自身也暴露 OpenAI 兼容聚合端点，供客户端直接调用

单文件运行：aigw-panel.exe
"""

import json
import mimetypes
import os
import socket
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid        # ★ _sse_text_chunks / _proxy_messages 生成 msg_/chatcmpl- id 依赖
import webbrowser
from datetime import datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

from app import catalog  # noqa: E402
from app import upstreams  # noqa: E402
from app.store import Store, now_str, now_ts  # noqa: E402
from app.gwclient import GatewayClient, GatewayError  # noqa: E402
from app import sitecheck  # noqa: E402
from app.scheduler import Scheduler  # noqa: E402
from app.router import Router, Target  # noqa: E402

CST = timedelta(hours=8)
LOG_LINES = []


def log(msg):
    line = "[%s] %s" % (now_str(), msg)
    LOG_LINES.append(line)
    if len(LOG_LINES) > 800:
        del LOG_LINES[:-800]
    try:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()
    except Exception:
        pass


def base_dir():
    """打包成 exe 后数据目录放在 exe 同级，便于用户直接编辑配置"""
    if getattr(sys, "frozen", False):
        return os.path.dirname(sys.executable)
    return os.path.dirname(os.path.abspath(__file__))


APP = {}


def init_app():
    root = base_dir()
    data_dir = os.path.join(root, "data")
    # 静态资源目录：源码运行时在 <项目>/app/static；打包后优先 _MEIPASS/app/static
    cands = [os.path.join(HERE, "app", "static")]
    if hasattr(sys, "_MEIPASS"):
        cands.insert(0, os.path.join(sys._MEIPASS, "app", "static"))
        cands.insert(1, os.path.join(sys._MEIPASS, "static"))
    static_dir = None
    for c in cands:
        if os.path.isdir(c):
            static_dir = c
            break
    if static_dir is None:
        static_dir = cands[0]
        log("警告：未找到静态资源目录 %s" % static_dir)

    # 网关可执行文件：从 exe 所在目录逐级向上找，最多 4 层
    gw_path = None
    cands = []
    d = root
    for _ in range(5):
        cands.append(os.path.join(d, "workbuddy-gateway-windows-1.29.6.exe"))
        cands.append(os.path.join(d, "_probe", "gw.exe"))
        cands.append(os.path.join(d, "workbuddy-gateway.exe"))
        nd = os.path.dirname(d)
        if nd == d:
            break
        d = nd
    for c in cands:
        if os.path.exists(c):
            gw_path = c
            break
    if gw_path is None:
        # 环境变量显式指定
        env = os.environ.get("AIGW_GATEWAY_PATH")
        if env and os.path.exists(env):
            gw_path = env
    if gw_path is None:
        # 兜底：在工作区内做一次浅层搜索
        try:
            for dirpath, dirnames, filenames in os.walk(root):
                if dirpath.count(os.sep) - root.count(os.sep) > 3:
                    dirnames[:] = []
                    continue
                for fn in filenames:
                    if fn.startswith("workbuddy-gateway") and fn.endswith(".exe"):
                        gw_path = os.path.join(dirpath, fn)
                        break
                if gw_path:
                    break
        except Exception:
            pass

    store = Store(data_dir)
    s = store.get("settings")

    client = GatewayClient(
        addr=s.get("gateway_addr", "127.0.0.1"),
        port=int(s.get("gateway_port", 8317)),
        api_key=s.get("gateway_api_key", "admin"),
        admin_user=s.get("gateway_admin_user", "admin"),
        admin_password=s.get("gateway_admin_password", ""))

    APP.update({
        "root": root,
        "data_dir": data_dir,
        "static_dir": static_dir,
        "store": store,
        "client": client,
        "gateway_path": gw_path,
        "started": now_ts(),
        "version": "1.0.0",
    })

    if s.get("gateway_admin_password"):
        try:
            client.login()
        except Exception:
            pass

    sched = Scheduler(store, gw_path, log)
    APP["scheduler"] = sched
    # ★ 网关 EXE 已彻底移除（2026-10-05）：调度器只管 APP 平台签到等原生任务，
    #   不再有任何"托管网关"路径。

    # ---- APP 平台自动签到：独立线程，不依赖网关是否被托管 ----
    # 之前它藏在 scheduler.run() 里，一旦「不自动拉起网关」就跟着停，
    # 定时签到会静默失效。这里拆开。
    try:
        from app import autocheckin as AC
        from app.accounts import Accounts as _Acc
        _acc = _Acc(store)
        _auto = AC.AutoCheckin(store, lambda: _acc, log, sched.notifier)
        _auto.start()
        APP["auto_checkin"] = _auto
        log("APP 自动签到线程已启动（独立于网关）")
    except Exception as e:
        log("自动签到线程启动失败：%s" % e)

    # ---- copilot token 保活（对齐社区 2api 标配：到期前主动刷新）----
    # ★ 2026-10-06 扩展：原仅覆盖 wb-gateway/wb-gateway-intl，导致 CodeBuddy
    #   国内·国际版、Trae 的 accessToken 过期后静默 401、必须重扫。
    #   现把整个 copilot 家族 + Trae 一并纳入（同一套腾讯接口、同一 refresh 机制）。
    KEEPALIVE_PLATFORMS = ("wb-gateway", "wb-gateway-intl",
                           "apk-codebuddy", "apk-codebuddy-cn", "apk-trae")

    def _copilot_keepalive():
        import base64
        while True:
            try:
                now = time.time()
                accs = store.get("accounts") or []
                changed = False
                for a in accs:
                    if a.get("platform") not in KEEPALIVE_PLATFORMS:
                        continue
                    rt = a.get("refresh_token") or ""
                    sec = a.get("secret") or ""
                    if not rt or not sec.count(".") == 2:
                        continue
                    try:
                        payload = json.loads(base64.urlsafe_b64decode(
                            sec.split(".")[1] + "=" * (-len(sec.split(".")[1]) % 4)))
                        exp = int(payload.get("exp") or 0)
                    except Exception:
                        continue
                    if exp and exp - now < 1800:   # 30 分钟内到期 → 主动刷新
                        try:
                            from app import tlogin as TL
                            d = TL.refresh(rt)
                            if d.get("accessToken"):
                                a["secret"] = d["accessToken"]
                                a["refresh_token"] = d.get("refreshToken") or rt
                                changed = True
                                log("copilot token 已自动刷新（%s）" % a.get("name", ""))
                        except Exception as e:
                            log("copilot token 刷新失败（%s）：%s" % (a.get("name", ""), e))
                if changed:
                    store.put("accounts", accs)
            except Exception as e:
                log("token 保活异常：%s" % e)
            time.sleep(1800)

    threading.Thread(target=_copilot_keepalive, daemon=True).start()
    log("copilot token 保活线程已启动（每 30 分钟检查，到期前 30 分钟刷新）")

    # ---- 账号登录：面板自己跑完 7 个网关/平台的登录，凭据落账号池 ----
    from app.accounts import Accounts
    from app.gwlogin import LoginManager
    accounts = Accounts(store)
    APP["accounts"] = accounts

    def client_factory():
        """
        每次取客户端都用**最新** settings 重新配置。
        否则用户在设置页改了控制台密码/端口后，登录功能会静默失效
        （client 仍持有启动时那一份空密码）。
        """
        c = APP["client"]
        st = APP["store"].get("settings")
        c.reconfigure(
            addr=st.get("gateway_addr", "127.0.0.1"),
            port=int(st.get("gateway_port", 8317)),
            api_key=st.get("gateway_api_key", "admin"),
            admin_user=st.get("gateway_admin_user", "admin"),
            admin_password=st.get("gateway_admin_password", ""))
        if st.get("gateway_admin_password"):
            try:
                c.login()
            except Exception:
                pass
        return c

    APP["login_manager"] = LoginManager(client_factory, accounts)

    # 恢复线上模型倍率缓存（登录后拉取过的）
    APP["model_rate_cache"] = store.get("model_rates") or {}

    # 路由引擎
    router = Router(store=store, log=log,
                    breaker_fail=int(s.get("breaker_fail", 3)),
                    breaker_cooldown=int(s.get("breaker_cooldown", 60)))
    APP["router"] = router
    router.accounts = accounts        # 内部中继目标（native:）需要账号池
    load_route_config(router, store, client)
    if s.get("router_auto_probe", True):
        router.start_auto_probe(int(s.get("router_probe_interval", 60)))

    log("面板启动完成，数据目录=%s，网关=%s" % (data_dir, gw_path or "未找到"))
    return APP


# ★ 面板固定默认端口：客户端（ZCode/Cherry/浏览器）配一次即可，
#   不必每次启动重改 base URL。8800 被占用时**报错退出**（不静默顺延，
#   见 _bind_port_or_die）。可用环境变量 AIGW_PANEL_PORT 覆盖。
DEFAULT_PANEL_PORT = 8800

# ------------------------------------------------------------------ 路由配置
DEFAULT_AUTO_MODELS = [
    {"name": "auto", "desc": "自动路由：优先选响应最快的上游",
     "strategy": "fastest"},
    {"name": "free", "desc": "免费优先：优先选免费模型（anon-zen / 豆包等）",
     "strategy": "free"},
]


def load_route_config(router, store, client):
    """从 settings/sites 重建路由表"""
    cfg = store.get("settings").get("routes") or {}
    models_cfg = cfg.get("models") or DEFAULT_AUTO_MODELS
    s = store.get("settings")
    gw_addr = "%s:%s" % (s.get("gateway_addr", "127.0.0.1"), s.get("gateway_port", 8317))
    gw_key = s.get("gateway_api_key", "admin")

    for m in models_cfg:
        targets = []
        # 1) 本地网关作为常驻上游 —— EXE 已彻底移除，默认**不再挂**
        #    （旧行为 use_gateway 默认 True，给每个 auto-* 塞一条永远连不通的
        #    8317 目标，熔断后整组不可用；现在只有用户显式配置才启用）
        if m.get("use_gateway", False):
            targets.append(Target(
                model=m.get("gateway_model", "default"),
                upstream_id="wb-gateway",
                name="WorkBuddy 本地网关",
                endpoint="http://%s/v1" % gw_addr,
                api_key=gw_key,
                weight=m.get("gateway_weight", 100),
                priority=0,
            ))
        # 2) 外部中转站 / 官方平台（取自 sites）
        for site in store.get("sites"):
            if not site.get("in_route"):
                continue
            ep = site.get("route_endpoint") or site.get("url")
            if not ep:
                continue
            key = site.get("token") or site.get("api_key") or ""
            if ep.rstrip("/").endswith("/v1") is False and "/v1" not in ep:
                ep = ep.rstrip("/") + "/v1"
            targets.append(Target(
                model=site.get("route_model") or "default",
                upstream_id=site.get("id"),
                name=site.get("name"),
                endpoint=ep,
                api_key=key,
                weight=site.get("route_weight", 100),
                priority=site.get("route_priority", 10),
                free=site.get("free", False),
            ))
        # 2.5) ★ OpenCode Zen 匿名车道（anon-zen）：免登录、公共凭据、
        #     永远可用、免费。作为 auto / free 的常驻第二候选，保证豆包
        #     挂掉时仍有免费退路（解决「auto 单上游一挂就 503」）。
        try:
            _NR = __import__("app.native_relay", fromlist=["zen_models"])
            _ok, _zm = _NR.zen_models()
            _zen_default = (_zm[0]["id"] if _ok and _zm else "gpt-4o-mini")
        except Exception:
            _zen_default = "gpt-4o-mini"
        targets.append(Target(
            model=_zen_default,
            upstream_id="native:anon-zen",
            name="OpenCode Zen（匿名免费车道）",
            endpoint="native://anon-zen",
            free=True,
        ))
        # 3) ★ 面板内部中继（豆包 samantha）：进程内直调，不出站、不依赖
        #    任何外部进程/端口；账号池有可用豆包 Cookie 时自动挂进 auto-*
        if m.get("use_native", True):
            try:
                _acc = APP.get("accounts")
                if _acc and _acc.usable("apk-doubao"):
                    targets.append(Target(
                        model="doubao-pro",
                        upstream_id="native:apk-doubao",
                        name="豆包（面板原生中继）",
                        endpoint="native://apk-doubao",
                        # 豆包中继不支持 tools 透传与图片输入，如实声明，
                        # 让带工具/图片的请求走真正支持的上游而不是瞎降级
                        capabilities={"tools": False, "vision": False},
                        free=True,
                    ))
            except Exception:
                pass
    if targets:
        router.add_model(m["name"], targets)
    router.config = {m["name"]: m.get("strategy", "fastest") for m in models_cfg}
    log("路由表已加载：%d 个自动模型" % len(router.list_models()))
    _load_merged_routes(router, store, keep={m["name"] for m in models_cfg}
                        | {"auto", "free"})


def _load_merged_routes(router, store, keep=()):
    """
    同模型跨源聚合路由（metapi 思路）：把同一模型 id 在所有可用源里的实例
    聚合成一个「裸名模型」，Router 按实测延迟择快 + 熔断 + 自动降级。
    例：glm-4-flash 同时存在于 web-glm 与 anon-zen → model=glm-4-flash
    会自动在两个源之间择快，一个挂了换下一个。

    只聚合能返回 OpenAI 形状的源（fmt=openai 透传 / web_relay 私有协议适配）；
    与既有模型名（auto/free/doubao-*）重名的跳过，不抢占专门优化过的路径。
    """
    from app import native_relay as NR
    from app import bundled_trae as _BT, bundled_go as _BG, \
        bundled_models as _BM, bundled_yuanbao as _BY
    _BUNDLED = {"apk-trae": _BT.MODELS, "apk-go": _BG.MODELS,
                "apk-codebuddy": _BM.MODELS, "apk-codebuddy-cn": _BM.MODELS,
                "apk-yuanbao": _BY.MODELS}
    # 豆包裸名走专门中继（doubao_relay），不进聚合。
    # 注意 reserved 不能含上一轮聚合出的模型名 —— 它们的 target 全是
    # native://，会在下面被清掉重建；算进保留名就会出现「先清后不再注册」。
    from app.doubao_relay import MODE_FLAGS as _DB_FLAGS
    reserved = set(keep) | set(_DB_FLAGS)

    index = {}   # mid -> [(pid, spec)]
    for pid, spec in NR.RELAY_SPECS.items():
        if not (spec.get("fmt") == "openai" or spec.get("web_relay")):
            continue
        if spec.get("base") == "@lobster" and not (
                (store.get("settings") or {}).get("lobster_server")):
            continue    # 未配置上游地址，挂进去也只会 502
        rows = spec.get("models_static") or _BUNDLED.get(pid) or []
        mids = []
        for m in rows:
            mid = (m.get("id") if isinstance(m, dict) else str(m)) or ""
            if mid and mid != "auto":
                mids.append(mid)
        if pid == "anon-zen" and spec.get("models_anon"):
            # 匿名车道以动态清单为准（zen_models 内置 TTL 缓存，不额外出站）
            try:
                _ok, _zm = NR.zen_models()
                if _ok and isinstance(_zm, list):
                    mids = [m["id"] for m in _zm if m.get("id")]
            except Exception:
                pass
        for mid in mids:
            if mid in reserved:
                continue
            index.setdefault(mid, []).append((pid, spec))

    # 重建语义：先清掉上一轮聚合出来的纯 native 模型（keep 名单——auto/free
    # 等自动组——不参与聚合，即使 target 全是 native:// 也不清），再注册本轮
    for name, info in list(router.list_models().items()):
        if name in reserved:
            continue
        tgts = info.get("targets") or []
        if tgts and all(str(t.get("endpoint", "")).startswith("native://")
                        for t in tgts):
            router.remove_model(name)

    merged_n = 0
    for mid, entries in sorted(index.items()):
        targets = []
        for pid, spec in entries:
            targets.append(Target(
                model=mid,
                upstream_id="native:%s" % pid,
                name=spec.get("name") or pid,
                endpoint="native://%s" % pid,
                free=(pid == "anon-zen"),
            ))
        if targets:
            router.add_model(mid, targets)
            router.config[mid] = "fastest"
            merged_n += 1
    log("同模型聚合路由：%d 个裸名模型跨源可用（保留名除外）" % merged_n)


def new_client():
    """按最新设置重建客户端"""
    s = APP["store"].get("settings")
    c = APP.get("client")
    if c is None:
        c = GatewayClient()
        APP["client"] = c
    c.reconfigure(
        addr=s.get("gateway_addr", "127.0.0.1"),
        port=int(s.get("gateway_port", 8317)),
        api_key=s.get("gateway_api_key", "admin"),
        admin_user=s.get("gateway_admin_user", "admin"),
        admin_password=s.get("gateway_admin_password", ""))
    if c.admin_password and not getattr(c, "_logged_in", False):
        # ★ 网关不活时别硬 login（本机连接拒绝 ~2s/次，拖慢所有接口）
        alive = (_gw_alive() or {}).get("alive")
        if alive:
            try:
                c.login()
            except Exception:
                pass
    return c


# ------------------------------------------------------------------ 工具
def ok(data, **kw):
    out = {"ok": True}
    out.update(data if isinstance(data, dict) else {"data": data})
    out.update(kw)
    return out


def fail(msg, code="error", status=400, **kw):
    out = {"ok": False, "code": code, "message": msg}
    out.update(kw)
    return out, status


def safe(fn, default=None):
    """把 GatewayError 转成结构化错误；返回 (dict, status) 元组，保持与路由一致"""
    try:
        return fn(), 200
    except GatewayError as e:
        return fail(e.message, code=e.code, status=502)
    except Exception as e:
        return fail(str(e), code="internal", status=500)


def safe_dict(fn):
    """只要 body 的版本：出错时返回 {"ok":False,...}"""
    obj, _ = safe(fn)
    return obj


# ------------------------------------------------------------------ API 处理
def _lobster_base():
    """Lobster AI 上游地址（设置→Lobster 上游地址，即 lobsterai2api 的
    LB2A_UPSTREAM_BASE）。web-lobster 的动作与对话中继都用它。"""
    try:
        return (APP["store"].get("settings") or {}).get("lobster_server") or ""
    except Exception:
        return ""


def _gw_alive():
    """网关存活缓存（5s TTL / 2s 短超时）。

    ★ EXE 关闭后，Windows 本机连接拒绝要拖 ~2 秒才返回——
    接入源页 /api/models + /api/overview 各打一次就慢一倍。
    这里统一走缓存，避免每请求都硬探。
    ⚠ 不能经 new_client()（它会调 _gw_alive 判断是否 login → 递归），
    这里直接构建裸客户端、只配参数不登录。"""
    now = time.time()
    ca = APP.get("gw_alive_cache") or {}
    if now - ca.get("ts", 0) < 5:
        return ca
    c = APP.get("client")
    if c is None:
        c = GatewayClient()
        APP["client"] = c
    s = APP["store"].get("settings")
    c.reconfigure(
        addr=s.get("gateway_addr", "127.0.0.1"),
        port=int(s.get("gateway_port", 8317)),
        api_key=s.get("gateway_api_key", "admin"),
        admin_user=s.get("gateway_admin_user", "admin"),
        admin_password=s.get("gateway_admin_password", ""))
    try:
        r = c._request("/v1/models", auth="bearer", retries=0, timeout=2)
        ca = {"ts": now, "alive": True,
              "models": len((r or {}).get("data") or [])}
    except GatewayError as e:
        ca = {"ts": now, "alive": False, "error": e.message}
    except Exception as e:
        ca = {"ts": now, "alive": False, "error": "%s: %s" % (type(e).__name__, e)}
    APP["gw_alive_cache"] = ca
    return ca


def api_overview():
    """总览：面板原生状态 + 签到总览 + 用量总计 + 站点健康（EXE 已彻底移除）"""
    store = APP["store"]
    from app.native_relay import RELAY_SPECS
    out = {
        "version": APP["version"],
        "uptime": now_ts() - APP["started"],
        "native": {
            "accounts": len(store.get("accounts") or []),
            "relays": len(RELAY_SPECS),
            "note": "登录/倍率/签到/对话/用量/通知全部面板原生，无 EXE 依赖",
        },
        "checkin": store.checkin_summary(),
        "tasks": store.task_summary(),
        "sites": {"total": len(store.get("sites"))},
        "scheduler": {
            "ticks": APP["scheduler"].ticks,
            "last_tick": APP["scheduler"].last_tick,
            "last_checkin_date": APP["scheduler"].last_checkin_date,
            "last_growth_date": APP["scheduler"].last_growth_date,
        },
    }
    # 面板侧用量（原生中继/路由调用，保留 90 天）
    ev = store.get("usage_events") or []
    out["usage"] = {"total_tokens": sum(x.get("in", 0) + x.get("out", 0) for x in ev)}
    return ok(out)


def api_usage(rng="all"):
    # ★ 网关不在（常态）就别硬调 c.usage/usage_series——每次白吃 4-10s 死连接，
    #   前端 loadRoute 的 Promise.all 会被它拖住，整页数据迟迟出不来。
    if (_gw_alive() or {}).get("alive"):
        c = new_client()
        u, st = safe(lambda: c.usage(rng))
        s = safe_dict(lambda: c.usage_series("24h")) if rng in ("all", "24h") else None
    else:
        u, st = None, 502
        s = None
    # ★ 面板侧用量（原生中继/路由调用，保留 90 天）—— 网关 EXE 不在也有报表
    panel = {"events": 0, "in": 0, "out": 0, "by_platform": {}, "by_model": {},
             "days": [], "today": None, "recent": []}
    try:
        ev = APP["store"].get("usage_events") or []
        panel["events"] = len(ev)
        panel["in"] = sum(x.get("in", 0) for x in ev)
        panel["out"] = sum(x.get("out", 0) for x in ev)
        today = time.strftime("%Y-%m-%d")
        day_map = {}
        for x in ev:
            bp = panel["by_platform"].setdefault(
                x.get("platform", "?"), {"n": 0, "in": 0, "out": 0, "fail": 0})
            bp["n"] += 1
            bp["in"] += x.get("in", 0)
            bp["out"] += x.get("out", 0)
            bp["fail"] += 0 if x.get("ok") else 1
            bm = panel["by_model"].setdefault(
                x.get("model", "?"), {"n": 0, "in": 0, "out": 0, "fail": 0})
            bm["n"] += 1
            bm["in"] += x.get("in", 0)
            bm["out"] += x.get("out", 0)
            bm["fail"] += 0 if x.get("ok") else 1
            day = time.strftime("%Y-%m-%d", time.localtime(x.get("ts", 0)))
            dd = day_map.setdefault(day, {"date": day, "requests": 0, "success": 0,
                                          "failed": 0, "tokensIn": 0, "tokensOut": 0})
            dd["requests"] += 1
            dd["success"] += 1 if x.get("ok") else 0
            dd["failed"] += 0 if x.get("ok") else 1
            dd["tokensIn"] += x.get("in", 0)
            dd["tokensOut"] += x.get("out", 0)
        panel["days"] = [day_map[k] for k in sorted(day_map)][-14:]
        panel["today"] = day_map.get(today) or {
            "date": today, "requests": 0, "success": 0, "failed": 0,
            "tokensIn": 0, "tokensOut": 0}
        panel["recent"] = sorted(ev, key=lambda x: -x.get("ts", 0))[:20]
    except Exception:
        pass
    if st != 200:
        return ok({"usage": {"gateway_offline": True}, "series": None,
                   "panel": panel})
    return ok({"usage": u, "series": s, "panel": panel})


def api_tencent(action=None, body=None):
    """
    腾讯 CodeBuddy / WorkBuddy 直连（免 workbuddy-gateway 进程）
      probe   探一遍所有端点，看哪些免登录可用、哪些只差凭据
      rates   官方倍率（/v3/config）
      login   原生登录：生成 state + 打开浏览器
      poll    轮询扫码结果，成功后落盘 token
      status  当前凭据状态（有没有 token、能不能验活）
      catalog 在线模型目录（带 credits 倍率）
    """
    from app.tencent import Tencent, probe_all, ENDPOINTS
    b = body or {}
    acc = APP.get("accounts")
    token = b.get("token") or ""
    acct_name = ""
    # 优先用原生登录落盘的凭据
    if not token:
        try:
            from app import tlogin
            token = (tlogin.load() or {}).get("accessToken") or ""
            if token:
                acct_name = "原生登录"
        except Exception:
            pass
    if not token and acc:
        for pid in ("wb-gateway", "wb-gateway-intl", "apk-codebuddy",
                    "apk-codebuddy-cn"):
            try:
                a = acc.get(pid)
            except Exception:
                a = None
            if a and a.get("secret"):
                token = a["secret"]
                acct_name = a.get("name") or pid
                break
    if action == "probe":
        return ok({"probes": probe_all(token), "token": bool(token),
                   "account": acct_name,
                   "endpoints": {k: {"method": v[0], "path": v[1]}
                                for k, v in ENDPOINTS.items()}})

    if action == "login":
        # 起浏览器让用户扫码。注意 state 必须本地先生成，
        # 少了它登录页会直接报「登录链接不完整」。
        state = b.get("state") or Tencent.new_state()
        url = Tencent.login_url(state, b.get("platform") or "CLI")
        try:
            Tencent.open_login(state)
        except Exception as e:
            return fail("起浏览器失败：%s" % e, code="browser_failed")
        APP.setdefault("tencent_login", {})["state"] = state
        return ok({"state": state, "url": url,
                   "note": "在弹出的浏览器里完成扫码/账密登录"})

    if action == "poll":
        state = b.get("state") or (APP.get("tencent_login") or {}).get("state")
        if not state:
            return fail("没有 state，请先发起登录", code="no_state")
        from app import tlogin
        # 已经在浏览器里完成登录的话这一步立刻就出 token
        d = tlogin.exchange_in_page(state=state)
        if d.get("accessToken"):
            path = tlogin.save(d)
            return ok({"state": state, "saved": path,
                       "userId": d.get("userId", ""),
                       "account": "原生登录"})
        # 否则开一轮轮询等扫码
        d = tlogin.poll_token(state=state, timeout=int(b.get("timeout") or 300))
        if d.get("accessToken"):
            path = tlogin.save(d)
            return ok({"state": state, "saved": path,
                       "userId": d.get("userId", ""),
                       "account": "原生登录"})
        return ok({"state": state, "pending": True})

    if action == "status":
        from app import tlogin
        c = tlogin.load()
        t = c.get("accessToken") or ""
        if not t:
            return ok({"logged": False, "account": ""})
        v = tlogin.verify(t)
        return ok({"logged": True, "account": "原生登录",
                   "tokenLen": len(t),
                   "verify": {k: v[k][0] for k in v}})

    if action == "refresh":
        # 刷新 token：refreshToken 走 header X-Refresh-Token（实测唯一可行位置，
        # 放 body / query 都报 400「refreshToken is empty」）
        from app import tlogin
        okk, msg = tlogin.refresh_saved()
        if not okk:
            return fail(str(msg)[:300], code="refresh_failed")
        c = tlogin.load()
        v = tlogin.verify(c.get("accessToken") or "")
        return ok({"refreshed": True, "note": msg,
                   "refreshedAt": c.get("refreshed_at"),
                   "verify": {k: v[k][0] for k in v}})

    if action == "catalog":
        tc = Tencent(token)
        if not token:
            return fail("没有 token，请先登录", code="no_token")
        ms = tc.catalog()
        return ok({"models": ms, "count": len(ms), "account": acct_name})

    if action == "quota":
        tc = Tencent(token)
        if not token:
            return fail("没有 token，请先登录", code="no_token")
        okk, q = tc.quota()
        if not okk:
            return fail(str(q)[:300], code="quota_failed")
        ok2, summary = tc.quota_summary()
        return ok({"quota": q, "summary": summary if ok2 else [],
                   "totalRemain": sum(x["remain"] for x in summary) if ok2 else 0,
                   "totalSize": sum(x["size"] for x in summary) if ok2 else 0,
                   "totalUsed": sum(x["used"] for x in summary) if ok2 else 0})

    if action == "checkin":
        # 400「今天已签到」不是失败，按已签处理
        tc = Tencent(token)
        okk, d = tc.daily_checkin()
        msg = ""
        code = None
        if isinstance(d, dict):
            msg = str(d.get("msg") or "")
            code = d.get("code")
            if d.get("body"):
                try:
                    b = json.loads(d["body"])
                    msg = str(b.get("msg") or msg)
                    code = b.get("code", code)
                except Exception:
                    pass
        already = "已签到" in msg
        return ok({"checkedIn": bool(okk) or already, "already": already,
                   "code": code, "msg": msg or ("签到成功" if okk else "失败")})

    if action == "trae_local":
        # ★ 2026-10-06 新增：自动读取**本机**已登录 Trae 的 token，落库为 web-trae
        #   账号，解决 web-trae「手动从 leveldb / LocalStorage 抠 x-ide-token」的痛点
        #   （对齐 trae-minimax-client / Trae-Account-Manager 的本地登录态提取思路）。
        #   仅在运行面板的那台电脑上有效（Trae 必须本机装过并登录过）。
        from app import trae_local as _TL
        okk, info = _TL.read_trae_token()
        if not okk:
            return ok({"found": False, "candidates": info.get("candidates", []),
                       "error": info.get("error", ""),
                       "hint": info.get("hint", "")})
        secret = info.get("token") or ""
        if not secret or len(secret) < 16:
            return fail("读到的 token 异常（长度不足）", code="bad_token")
        name = info.get("account") or "Trae 本机登录态"
        try:
            if acc:
                acc.add({"platform": "web-trae", "name": name, "type": "token",
                         "secret": secret,
                         "source": info.get("source", "本机 Trae"), "edition": "cn"})
                return ok({"found": True, "account": name,
                           "source": info.get("source"),
                           "note": "已写入 web-trae 账号，刷新账号页即可见"})
        except Exception as e:
            return fail("落库失败：%s" % e, code="save_failed")
        return fail("账号池不可用，无法落库", code="no_accounts")

    tc = Tencent(token)
    if action == "rates":
        okk, d = tc.rates()
        return ok(d) if okk else fail(str(d)[:300], code="rates_failed")
    return ok({"endpoints": list(ENDPOINTS)})


def api_autocheckin(action=None, body=None):
    """
    APP 平台定时自动签到
      status   状态 + 每个平台的开关/时间
      save     保存配置
      run      立刻跑一轮（force 忽略「今天已跑过」）
      platforms 可签到平台清单
    """
    from app import autocheckin as AC
    store = APP["store"]
    acc = APP.get("accounts")
    b = body or {}

    if action == "platforms":
        return ok({"platforms": AC.platform_list(store)})

    if action == "status":
        st = AC.status(store, acc)
        auto = APP.get("auto_checkin")
        st["thread_alive"] = bool(auto and auto.is_alive())
        st["running"] = bool(auto and auto.running)
        st["last_result"] = getattr(auto, "last_result", None) if auto else None
        return ok(st)

    if action == "save":
        cfg = AC.load_cfg(store)
        for k in ("enabled", "stagger_sec", "retry_times", "retry_delay_min",
                  "notify", "default_time"):
            if k in b:
                cfg[k] = b[k]
        if "platforms" in b and isinstance(b["platforms"], dict):
            cur = cfg.get("platforms") or {}
            for pid, v in (b["platforms"] or {}).items():
                if isinstance(v, dict):
                    cur.setdefault(pid, {}).update(
                        {"on": bool(v.get("on", True)),
                         "time": str(v.get("time") or "09:10")})
            cfg["platforms"] = cur
        AC.save_cfg(store, cfg)
        return ok(AC.status(store, acc))

    if action == "run":
        # 手动触发不依赖后台线程 —— 面板刚启动的静默观察期里也要能用。
        auto = APP.get("auto_checkin")
        only = b.get("platforms")
        force = bool(b.get("force"))
        result = {}

        def _work():
            try:
                if auto is not None:
                    result["r"] = auto.run_round(force=force, only=only,
                                                options=(b.get("options") or {}))
                else:
                    # 线程还没起来（静默观察期），临时造一个跑
                    from app import autocheckin as AC
                    tmp = AC.AutoCheckin(store, lambda: acc, APP.get("log"))
                    result["r"] = tmp.run_round(force=force, only=only,
                                                options=(b.get("options") or {}))
            except Exception as e:
                result["err"] = "%s: %s" % (type(e).__name__, str(e)[:120])

        t = threading.Thread(target=_work, daemon=True)
        t.start()
        t.join(0.4)                 # 给一点点时间让它起来
        if "r" in result:
            r = result["r"]
            return ok({"finished": True, "summary": r.get("summary"),
                       "results": r.get("results")})
        return ok({"started": True,
                   "message": "已触发，稍后刷新看结果"})

    return ok({"platforms": AC.platform_list(store)})


def api_toolcall(action=None, body=None):
    """
    工具调用（function calling）测试台
      list     列出可用工具（OpenAI tools 格式）
      chat     带 tools 发一次对话，返回上游的 tool_calls
      exec     本地执行一批 tool_calls，回传 role=tool 消息
      sandbox  沙箱目录信息
    """
    from app import tools as T
    b = body or {}

    def _opts():
        cfg = {}
        try:
            s = APP["store"].get("toolcall") or {}
            cfg = s.get("options") or {}
        except Exception:
            pass
        return cfg

    if action == "list":
        names = b.get("names")
        return ok({"tools": T.list_tools(names),
                   "names": T.tool_names(),
                   "sandbox": T.SANDBOX,
                   "options": _opts()})

    if action == "sandbox":
        return ok(T.sandbox_info())

    if action == "exec":
        calls = b.get("tool_calls") or []
        results, messages = T.execute_tool_calls(calls, _opts())
        return ok({"results": results, "tool_messages": messages})

    if action == "chat":
        messages = b.get("messages") or []
        names = b.get("names")
        if b.get("all_tools"):
            names = None
        tool_defs = b.get("tools") or T.list_tools(names)
        if not tool_defs:
            return fail("没有启用任何工具")
        payload = {
            "model": b.get("model") or "default",
            "messages": messages,
            "tools": tool_defs,
            "stream": False,
        }
        tc = b.get("tool_choice")
        if tc:
            payload["tool_choice"] = tc
        if b.get("parallel_tool_calls") is not None:
            payload["parallel_tool_calls"] = bool(b["parallel_tool_calls"])
        c = new_client()
        res, st = safe(lambda: c.raw_chat(payload))   # raw_chat 返回 (data, status)
        if st != 200:
            return res, st
        msg = ((res.get("choices") or [{}])[0] or {}).get("message") or {}
        return ok({
            "content": msg.get("content") or "",
            "tool_calls": msg.get("tool_calls") or [],
            "finish_reason": (res.get("choices") or [{}])[0].get("finish_reason"),
            "usage": res.get("usage") or {},
            "model": res.get("model"),
            "raw_keys": sorted(res.keys()),
        })

    return ok({"tools": T.list_tools(), "names": T.tool_names()})


def api_models(action=None, body=None):
    """
    模型清单。除网关自带模型外，还合并 CodeBuddy 静态倍率表；
    action=catalog          用账号池凭据在线拉取模型目录（含线上倍率）
    action=sources          列出可作为「模型来源」的平台
    action=platform_models  取某个平台自己的模型列表（按平台筛选用）
    action=select           读写用户勾选的模型
    """
    b = body or {}                      # 各分支共用，别在分支里重复定义
    c = new_client()
    # ★ 网关不活就别硬调 c.models()（本机拒绝 ~2s/次，拖慢每个 action）
    if (_gw_alive() or {}).get("alive"):
        m, st = safe(lambda: c.models())    # 网关自己的模型（platform_models 用）
    else:
        m, st = None, 502
    if action == "catalog":
        from app import gwextra
        platform = b.get("platform", "apk-codebuddy")
        # 顺带返回「可拉取倍率的平台清单」，前端下拉框用，不再硬编码
        sources = gwextra.mark_catalog_accounts(
            gwextra.catalog_sources(), APP.get("accounts"))
        okk, data = gwextra.fetch_model_catalog(
            APP.get("accounts"), platform, b.get("account_id"))
        if okk:
            # 缓存倍率（按模型 id），让 /api/models 的表直接带上
            cache = APP.setdefault("model_rate_cache", {})
            for x in (data or {}).get("models", []):
                cache[str(x.get("id"))] = x
            # 持久化，重启不丢
            try:
                APP["store"].put("model_rates", cache)
            except Exception:
                pass
        return ok({"ok": okk, "catalog": data, "sources": sources}) if okk else fail(
            data if isinstance(data, str) else json.dumps(
                data, ensure_ascii=False)[:500], code="catalog_failed")

    if action == "sources":
        # 一次给全：倍率来源清单 + 接入源分类（见 app/sources.py 的分类规则）
        from app import gwextra
        from app import sources as SRC
        return ok({
            # 倍率可拉取的平台
            "sources": gwextra.mark_catalog_accounts(
                gwextra.catalog_sources(), APP.get("accounts")),
            # 接入源分类：有桌面客户端 → LOCAL，否则有 API → API，否则 WEB
            "categories": SRC.CATEGORIES,
            "platform_kinds": {p: {"kind": SRC.kind_of(p),
                                   "facts": SRC.facts_of(p),
                                   "meta": SRC.category_meta(SRC.kind_of(p))}
                               for p in SRC.PLATFORM_FACTS},
            "local_proxies": SRC.LOCAL_PROXIES,
            "summary": SRC.summary(),
        })

    if action == "select":
        # 持久化用户勾选的模型（前端用来生成客户端配置）
        store = APP["store"]
        if body is not None and body.get("models") is not None:
            models = [str(x) for x in (body.get("models") or []) if str(x).strip()]
            store.put("selected_models", sorted(set(models))[:500])
            return ok({"saved": len(set(models))})
        saved = store.get("selected_models") or []
        return ok({"models": saved})

    if action == "relays":
        # 原生对话中继能力清单（端点全部静态提取自 APK/EXE 字节码）
        from app import native_relay as NR
        acc = APP.get("accounts")
        out = []
        for r in NR.relay_list():
            pid = r["platform"]
            n_acc = len(acc.usable(pid)) if acc else 0
            out.append(dict(r, usable_credentials=n_acc))
        return ok({"relays": out,
                   "usage": "对话请求 model 带 @平台id 后缀即走原生中继，"
                            "如 \"doubao-pro@apk-doubao\""})

    if action == "grouped":
        # ★ 模型总表（按来源分组）：所有可用模型一张表，前端不再按平台切换
        from app import native_relay as NR
        from app.doubao_relay import MODE_FLAGS as _DBF
        from app import bundled_trae as BT, bundled_go as BG, \
            bundled_models as BM, bundled_yuanbao as BY, bundled_doubao as BD
        acc = APP.get("accounts")
        groups = []

        def _g(pid, name, models, hint="", live=False):
            n = len(acc.usable(pid)) if (acc and pid not in ("auto", "anon-zen")) \
                else (len(acc.usable("anon-zen")) if acc and pid == "anon-zen" else 0)
            groups.append({"pid": pid, "name": name, "hint": hint,
                           "live": live, "usable": n, "models": models})

        # ① 自动路由（统一接口的推荐入口）
        router = APP.get("router")
        autos = []
        _stg = {"fastest": "最快优先", "weight": "权重优先", "priority": "优先级",
                "free": "免费优先"}
        if router:
            for name, info in sorted(router.list_models().items()):
                autos.append({"id": name,
                              "name": "自动路由 · " + _stg.get(
                                  router.config.get(name, "fastest"), name),
                              "desc": "%d 个上游候选 · 当前健康 %d"
                                      % (len(info["targets"]), info["healthy"])})
        _g("auto", "自动路由（推荐：一个模型名自动挑最快上游）", autos,
           "对话 model 填 auto（最快优先）/ free（免费优先）")

        # ② 豆包（samantha 原生中继，裸名直出）
        db = [{"id": mid, "name": "豆包 · " + mid, "desc": "原生中继"}
              for mid in _DBF if mid != "auto"]
        _g("apk-doubao", "豆包（Cookie 登录 · 原生中继）", db,
           "model 直接写 doubao-pro 等，无需 @后缀", live=False)

        # ③★ WorkBuddy 账号（wb-gateway）：模型总表原本**漏了这个分组**
        #   （之前只有 apk-codebuddy 代表 copilot 系）→ 前端总表看不到 WorkBuddy。
        #   有 token → 原生直连在线目录（带倍率）；否则回退内置表，永不空组。
        _wb_rows, _wb_live = [], False
        if acc:
            try:
                from app import tlogin as TL
                from app.acct_pool import pick_secret
                _sec, _ = pick_secret(acc, "wb-gateway") or ("", "")
                if _sec and _sec.startswith("eyJ"):
                    _wb_online = TL.models_merged(_sec) or []
                    if _wb_online:
                        _wb_rows = [{"id": "%s@wb-gateway" % r.get("id"),
                                     "name": r.get("name") or r.get("id"),
                                     "credits": r.get("credits") or "",
                                     "ctx": r.get("maxInputTokens"),
                                     "out": r.get("maxOutputTokens"),
                                     "desc": "在线目录（原生直连）"}
                                    for r in _wb_online if r.get("id")]
                        _wb_live = True
            except Exception as e:
                log("总表 wb-gateway 在线目录失败：%s: %s" % (type(e).__name__, e))
        if not _wb_rows:
            _wb_rows = [{"id": "%s@wb-gateway" % m.get("id"),
                         "name": m.get("name") or m.get("id"),
                         "credits": m.get("credits") or "",
                         "ctx": m.get("maxInputTokens"),
                         "out": m.get("maxOutputTokens"),
                         "desc": "内置清单"}
                        for m in BM.MODELS if (m.get("id") or "").lower() != "auto"]
        # ★ 站点口径（2026-10-06 搜索证实，勿再按"产品"拆）：
        #   WorkBuddy 与 CodeBuddy **同族同契约**（同一套 /v2/chat/completions、
        #   同一 OAuth 轮询、同一积分），只是分**国内站 / 国际站**：
        #     国内站 copilot.tencent.com（品牌：WorkBuddy 国内 / CodeBuddy 国内）
        #     国际站 www.workbuddy.ai（品牌：WorkBuddy 国际 / CodeBuddy 国际，
        #       workbuddy.ai 与 codebuddy.ai 两个域名都属国际版）
        #   凭据按站点隔离（edition），两站账号独立但积分共享。
        _g("wb-gateway", "copilot 接口 · 国内站（WorkBuddy/CodeBuddy 国内 · 实时）",
           _wb_rows,
           "★ 国内站 copilot.tencent.com；与下方「国际站」是同一套接口的两个站点，"
           "凭据隔离、积分共享。已接入则显示实时清单（带倍率）",
           live=_wb_live)

        # ③ 各中继平台的静态/内置清单
        _SRC = [
            ("apk-trae", "Trae 国内版（aigw.app 网关）", BT.MODELS),
            ("apk-go", "Go 网关（libgojni 内嵌清单）", BG.MODELS),
            #   ↓ 国际站分组：www.workbuddy.ai（workbuddy.ai / codebuddy.ai 两个品牌
            #     域名都属国际版）。未接入国际站账号 → 暂用内置清单，标注清楚。
            ("apk-codebuddy", "copilot 接口 · 国际站（WorkBuddy/CodeBuddy 国际 · 内置清单）",
             BM.MODELS),
            ("apk-yuanbao", "元宝（APK 内置清单）", BY.MODELS),
        ]
        for pid, name, models in _SRC:
            rows = [{"id": "%s@%s" % (m.get("id"), pid),
                     "name": m.get("name") or m.get("id"),
                     "credits": m.get("credits"),
                     "ctx": m.get("ctx") or m.get("maxInputTokens")
                            or m.get("context_length"),
                     "out": m.get("out") or m.get("maxOutputTokens"),
                     "desc": m.get("desc") or ""}
                    for m in models if (m.get("id") or "").lower() != "auto"]
            _g(pid, name, rows,
               "对话 model 带 @%s 后缀即走该平台原生中继" % pid)

        # ④ 中继平台上运行时才组装清单的（如实说明）
        #   ★ 补入 build38 三个网页版反代（此前漏列 → 总表看不到它们）
        for pid, name in (("web-qoder", "Qoder（直连模型端点）"),
                          ("web-lobster", "网易 Lobster AI"),
                          ("apk-raccoon", "小浣熊"),
                          ("web-glm", "智谱清言网页版"),
                          ("web-trae", "Trae 国内版（直连）"),
                          ("web-deepseek", "DeepSeek 网页版")):
            spec = NR.relay_of(pid) or {}
            rows = [{"id": "%s@%s" % (m.get("id"), pid),
                     "name": m.get("name") or m.get("id"),
                     "desc": m.get("desc") or ""}
                    for m in (spec.get("models_static") or [])]
            _g(pid, name, rows,
               rows and "静态清单（社区逆向）" or
               "清单由上游运行时下发：登录后「查看该源模型」动态拉取")

        # ⑤ anon-zen 免登录车道（动态 86 个）
        zen_rows = []
        if NR.RELAY_SPECS.get("anon-zen"):
            okz, zm = NR.zen_models()
            if okz:
                zen_rows = [{"id": "%s@anon-zen" % m["id"],
                             "name": m.get("name") or m["id"],
                             "ctx": m.get("ctx"), "desc": m.get("desc") or ""}
                            for m in zm]
        _g("anon-zen", "Our Free Model（OpenCode Zen 匿名车道 · 免登录）",
           zen_rows, "无需任何账号；2026-10-05 实测上游 chat 已开始要求真实 Key"
                     "（401 Missing API key），模型清单仍免登录可拉",
           live=okz if zen_rows else False)

        total = sum(len(g["models"]) for g in groups)
        return ok({"groups": groups, "total": total})

    if action == "test_model":
        # ★ 连接测试：对单个模型发一条极小请求，回 {ok, latency_ms, reply/error}
        #   这是真实调用（消耗极少量额度），就是用户要的「手动检测」。
        model = str(b.get("model") or "").strip()
        if not model:
            return fail("model 必填")
        msgs = [{"role": "user", "content": "ping"}]
        t0 = time.time()

        def _fin(okk, **kw):
            return ok(dict(ok=okk, model=model,
                           latency_ms=round((time.time() - t0) * 1000), **kw))

        router = APP.get("router")
        if router and model in router.list_models():
            body2 = {"model": model, "messages": msgs, "max_tokens": 16}
            result, tried = router.chat(model, body2,
                                        strategy=router.config.get(model, "fastest"))
            good = not (result.get("ok") is False and "choices" not in result)
            content = ""
            try:
                content = ((result.get("choices") or [{}])[0]
                           .get("message") or {}).get("content", "")
            except Exception:
                pass
            return _fin(good, via="router",
                        upstream=(result.get("_route") or {}).get("upstream"),
                        reply=str(content)[:120],
                        error=None if good else str(result)[:300])
        base_model, _, pid = model.rpartition("@")
        if "@" in model and pid:
            from app import native_relay as NR, acct_pool
            if not NR.relay_of(pid):
                return fail("未知中继平台：%s" % pid)
            # ★ anon-zen：chat 端点已要求真实 Key（401 Missing API key），
            #   连接测试改探清单接口（force 活体 GET，不读 TTL 缓存，避免假绿）
            if pid == "anon-zen":
                _okz, _zm = NR.zen_models(timeout=10, ttl=0)
                if _okz:
                    return _fin(True, via="anon-zen",
                                reply="清单接口可达（免登录 %d 个模型）；对话需真实 Key"
                                      % len(_zm or []),
                                error=None)
                return _fin(False, via="anon-zen",
                            error="清单接口不可达：%s" % str(_zm)[:200])
            rbody = {"model": base_model or "default", "messages": msgs,
                     "max_tokens": 16, "stream": False}

            def _call(secret, name, row=None):
                return NR.relay_once(pid, rbody, secret, timeout=45,
                                     base_override=_lobster_base(), account=row)

            okk, payload, acct = acct_pool.try_accounts(
                APP.get("accounts"), pid, _call)
            if not okk and "没有" in str(payload) and "凭据" in str(payload):
                # 免登录车道（anon-zen secret_fixed=public）：无池凭据也直接试
                spec = (NR.RELAY_SPECS.get(pid) or {})
                if spec.get("secret_fixed"):
                    okk, payload = NR.relay_once(pid, rbody, "", timeout=45,
                                                 base_override=_lobster_base())
                    acct = "公共凭据"
            reply = ""
            if okk and isinstance(payload, dict):
                try:
                    reply = ((payload.get("choices") or [{}])[0]
                             .get("message") or {}).get("content", "")
                except Exception:
                    pass
            return _fin(bool(okk), via=pid, account=acct,
                        reply=str(reply)[:120],
                        error=None if okk else str(payload)[:300])
        from app.doubao_relay import MODE_FLAGS as _DBF
        if model in _DBF:
            from app import doubao_relay as DR, acct_pool

            def _db(secret, name):
                return DR.chat(msgs, secret, model=model, timeout=45)

            okk, payload, acct = acct_pool.try_accounts(
                APP.get("accounts"), "apk-doubao", _db)
            reply = ""
            if okk and isinstance(payload, dict):
                try:
                    ch = payload.get("choices") or []
                    reply = ((ch[0] if ch else {}).get("message") or {}).get("content", "")
                except Exception:
                    pass
            return _fin(bool(okk), via="apk-doubao", account=acct,
                        reply=str(reply)[:120],
                        error=None if okk else str(payload)[:300])
        return fail("未知模型：%s（可用清单见「路由与模型 → 模型与倍率」）" % model)

    if action == "platform_models":
        # 按平台取该平台自己的模型列表（用户反馈「选了平台但表里还是网关的模型」）
        from app import gwextra
        pid = b.get("platform")
        if not pid:
            return fail("platform 必填")
        acc = APP.get("accounts")

        # 元宝 / 豆包：策略一致 —— 已登录优先拉云端实时清单，失败回退内置。
        if pid in ("apk-yuanbao", "apk-doubao"):
            from app import model_extract as ME
            from app import gwextra
            real, live, note = ME.live_models(
                acc, pid, gwextra.call, gwextra.action_spec)
            if live and real:
                return ok({"platform": pid, "name": pid,
                           "models": real, "live": True, "note": note})
            # 回退内置
            if pid == "apk-yuanbao":
                from app import bundled_yuanbao as BY
                return ok({"platform": pid, "name": "元宝（内置清单）",
                           "models": BY.MODELS, "live": False,
                           "note": "元宝不提供公开模型接口，这 %d 个来自 APK 内置；"
                                   "登录后可拉 /api/agent/model/list 实时刷新（%s）"
                                   % (len(BY.MODELS), note),
                           "source": BY.GENERATED_NOTE})
            from app import bundled_doubao as BD
            models = list(BD.MODELS) + [
                dict(m, desc="APK 网关 id：" + (m.get("desc") or ""))
                for m in getattr(BD, "APK_GATEWAY_MODELS", [])]
            return ok({"platform": pid, "name": "豆包（网页版菜单 + APK 网关）",
                       "models": models, "live": False,
                       "note": "网页版「模型选择」%d 个 + dev.doubao2api 本地网关"
                               "自有 id %d 个；登录后可拉 /alice/basic/launch"
                               " 实时刷新（%s）"
                               % (len(BD.MODELS),
                                  len(getattr(BD, "APK_GATEWAY_MODELS", [])),
                                  note),
                       "source": BD.GENERATED_NOTE})

        # Trae / Go 网关：APK 本身就是网关，模型清单打在 dex/.so 里（已逐条验证）
        if pid == "apk-trae":
            from app import bundled_trae as BT
            return ok({"platform": pid, "name": "Trae aigw.app（APK 网关清单）",
                       "models": BT.MODELS, "live": False,
                       "note": "这 %d 个模型 id 逐条验证自 base.apk dex；"
                               "aigw.app 本地网关暴露 /v1/models 与"
                               " /v1/chat/completions（OAuth 回调 51120/51121）"
                               % len(BT.MODELS),
                       "source": BT.GENERATED_NOTE,
                       "custom_slots": getattr(BT, "CUSTOM_SLOTS", [])})
        if pid == "apk-go":
            from app import bundled_go as BG
            return ok({"platform": pid, "name": "Go 原生网关（APK 内嵌能力表）",
                       "models": BG.MODELS, "live": False,
                       "note": "这 %d 个模型含上下文/输出上限，提取自"
                               " libgojni.so 内嵌 JSON（自带 /panel 管理页）"
                               % len(BG.MODELS),
                       "source": BG.GENERATED_NOTE})

        # ★ copilot 系（wb-gateway / wb-gateway-intl）：原生直连 copilot.tencent.com
        #   （EXE 已彻底移除，旧「网关型分支」去问 8317 的 /v1/models 永远为空 ——
        #    这就是前端 WorkBuddy 卡片看不到模型的根因）。
        #   有 token → 在线目录 /console/enterprises/personal/models（带倍率）；
        #   拉不到/未登录 → 回退内置 bundled_models（36 个），界面永不空表。
        if pid in ("gateway", "wb-gateway", "wb-gateway-intl"):
            nm = {"wb-gateway": "WorkBuddy 账号 · 国内站",
                  "wb-gateway-intl": "WorkBuddy 账号 · 国际站",
                  "gateway": "本地网关（全部）"}[pid]
            if pid != "gateway" and acc:
                try:
                    from app import tlogin as TL
                    from app.acct_pool import pick_secret
                    sec, _ = pick_secret(acc, pid) or ("", "")
                    if sec and not sec.startswith("eyJ"):
                        sec = ""   # 只认 JWT 型 accessToken
                    if sec:
                        rows = TL.models_merged(sec) or []
                        if rows:
                            return ok({
                                "platform": pid, "name": nm + "（在线目录）",
                                "models": [{
                                    "id": r.get("id"),
                                    "name": r.get("name") or r.get("id"),
                                    "credits": r.get("credits") or "",
                                    "ctx": r.get("maxInputTokens"),
                                    "out": r.get("maxOutputTokens"),
                                } for r in rows if r.get("id")],
                                "live": True,
                                "note": "面板原生直连在线目录（%d 个，带倍率），不依赖网关 EXE"
                                        % len(rows)})
                except Exception as e:
                    log("wb-gateway 在线目录拉取失败：%s: %s" % (type(e).__name__, e))
            # 回退内置倍率表（与 codebuddy 同款，保证界面有内容）
            from app import bundled_models as BM
            fallback = []
            for m in getattr(BM, "MODELS", []):
                if isinstance(m, dict) and m.get("id"):
                    fallback.append({"id": m["id"], "name": m.get("name") or m["id"],
                                     "credits": m.get("credits") or ""})
                elif isinstance(m, str):
                    fallback.append({"id": m, "name": m, "credits": ""})
            return ok({"platform": pid, "name": nm + "（内置清单）",
                       "models": fallback, "live": False,
                       "note": "在线目录未取到（未登录或 token 过期），以下 %d 个来自"
                               "内置倍率表；扫码接入后可拉实时清单（带倍率）"
                               % len(fallback)})

        # 官方 API 平台（api-*）：账号池配了 Key → Bearer 拉实时 /models；
        # 没配 Key → 回退官方常见模型提示（不报错，界面永远有内容）
        if pid.startswith("api-") and pid in gwextra.ACTIONS:
            from app import model_extract as ME
            real, live, note = ME.live_models(
                acc, pid, gwextra.call, gwextra.action_spec)
            if live and real:
                return ok({"platform": pid, "name": gwextra.api_official_name(pid),
                           "models": real, "live": True, "note": note})
            hints = gwextra.api_hint_models(pid)
            if hints:
                return ok({"platform": pid, "name": gwextra.api_official_name(pid),
                           "models": hints, "live": False,
                           "note": "账号池未配置 API Key，以下是官方常见模型提示；"
                                   "在「账号登录」给该平台添加 Key 后自动拉全量清单（%s）"
                                   % note})

        # anon-zen：匿名车道动态清单（免登录，OpenCode Zen）
        if pid == "anon-zen":
            from app import native_relay as NRZ
            okk, models = NRZ.zen_models()
            if okk and models:
                return ok({"platform": pid, "name": "Our Free Model（匿名车道）",
                           "models": models, "live": True,
                           "note": "免登录动态清单（%d 个，OpenCode Zen）；"
                                   "免费额度按会话计" % len(models),
                           "source": "dsh-our-free-model 逆向"})
            return fail("匿名清单拉取失败：%s" % models, code="zen_models_failed")

        # web-qoder / web-lobster 等静态内置清单（社区逆向端点，免登录可展示）
        from app import native_relay as NR0
        _nspec = NR0.relay_of(pid)
        if _nspec and _nspec.get("models_static"):
            return ok({"platform": pid,
                       "name": _nspec.get("name") or pid,
                       "models": _nspec["models_static"], "live": False,
                       "note": "静态清单（社区逆向端点）；登录后可拉动态清单",
                       "source": "社区项目逆向"})

        spec = gwextra.action_spec(pid, "models")
        if not spec:
            return fail("平台 %s 的模型清单由它 APK 内的本地网关（运行在手机 "
                        "127.0.0.1）在运行时提供，面板无法直连手机端点；"
                        "静态提取结果已内置的会直接显示，未内置的可在"
                        "「拉取线上倍率」或登录后探测" % pid,
                        code="no_models_action")
        okk, data = gwextra.call(acc, pid, "models", {},
                                 base_override=_lobster_base())
        if not okk:
            # CodeBuddy 国际站：未登录也有内置倍率表可回退（36 模型，离线可用）
            if pid == "apk-codebuddy":
                from app import bundled_models as BM
                fallback = [{"id": x.get("id"),
                             "name": x.get("name") or x.get("id"),
                             "credits": x.get("credits")}
                            for x in BM.MODELS if isinstance(x, dict)]
                if fallback:
                    return ok({"platform": pid, "name": "CodeBuddy 国际版（内置倍率表）",
                               "models": fallback, "live": False,
                               "note": "未登录：展示 APK 内置倍率表 %d 个模型"
                                       "（来源 %s）；登录后可拉在线目录（%s）"
                                       % (len(fallback), BM.SOURCE,
                                          str(data)[:80])})
            return fail(str(data)[:300], code="platform_models_failed")
        # 统一用 model_extract 解析（兼容 data/model_list/models 等多种响应形状）
        from app import model_extract as ME
        models = ME.extract_generic(data if isinstance(data, dict) else {})
        return ok({"platform": pid, "name": pid, "models": models, "live": True,
                   "note": "该平台返回 %d 个模型" % len(models)})

    if st != 200:
        # ★ 网关 EXE 不在 → 不再 502，用内置倍率表 + 在线缓存降级出模型表
        #   （接入源页靠这个接口渲染，502 会让整页"加载失败"）
        from app import bundled_models as BM
        cache = APP.get("model_rate_cache") or {}
        rows = []
        for x in BM.MODELS:
            if not isinstance(x, dict):
                continue
            mid = str(x.get("id"))
            hit = cache.get(mid) or {}
            rows.append({
                "id": mid, "name": x.get("name") or mid,
                "credits": hit.get("rate") or x.get("credits") or "",
                "ctx": x.get("maxInputTokens", ""),
                "out": x.get("maxOutputTokens", ""),
                "rate_source": "线上目录" if hit.get("rate") else "内置表",
                "availableAccounts": 0, "cnAccounts": 0, "intlAccounts": 0,
                "cost": "未观测",
            })
        return ok({
            "admin": {"models": [], "gateway_offline": True},
            "v1": {"data": []},
            "models_enriched": rows,
            "online_models": len(cache),
            "codebuddy_static": upstreams.CODEBUDDY_MODELS,
            "bundled": BM.stats(),
            "rate_summary": {"total": len(rows),
                             "with_rate": sum(1 for r in rows if r.get("credits")),
                             "from_bundled": BM.stats().get("with_credits"),
                             "from_online": len(cache),
                             "gateway_offline": True},
            "note": "网关 EXE 未运行：展示内置倍率表（登录/原生中继不受影响）",
        })
    v1 = safe_dict(lambda: c.v1_models())

    admin = (m or {}).get("models") if isinstance(m, dict) else None
    return ok(_merge_model_rates(admin, m, v1))


def _record_usage(platform, model, tin, tout, secs, ok, upstream=""):
    """面板侧用量记录（保留 90 天 / 5000 条，等价网关 usageRetentionDays）"""
    try:
        store = APP["store"]
        ev = store.get("usage_events") or []
        ev.append({"ts": time.time(), "platform": platform, "model": model,
                   "in": int(tin or 0), "out": int(tout or 0),
                   "ms": int((secs or 0) * 1000), "ok": bool(ok),
                   "upstream": str(upstream or "")[:60]})
        cutoff = time.time() - 90 * 86400
        store.put("usage_events", [x for x in ev if x.get("ts", 0) >= cutoff][-5000:])
    except Exception:
        pass


def _usage_from_payload(payload):
    """从上游/OpenAI 形状响应里取 usage（缺省 0）"""
    try:
        u = (payload or {}).get("usage") or {}
        return u.get("prompt_tokens"), u.get("completion_tokens")
    except Exception:
        return 0, 0


def _merge_model_rates(admin, m, v1):
    """
    倍率三源合并，优先级从高到低：
      1. 网关实测 cost（跑过真实流量才会有数值，否则是「未观测」）
      2. 在线目录缓存（action=catalog 登录后拉到的）
      3. 内置倍率表（APK 自带 codebuddy-code 目录，36 个模型 35 带倍率）
    网关上游名与内置表槽位名不同，靠 bundled_models.merge() 做
    精确 / 别名 / 归一化 / 同族前缀四级匹配。
    """
    from app import bundled_models

    online = APP.get("model_rate_cache") or {}
    rows, bundled_hit = bundled_models.merge(admin or [])

    for item in rows:
        mid = str(item.get("id"))
        # 网关只在部分情况返回账号计数字段，缺失时补 0 ——
        # 否则前端会渲染出 "undefined / 0"（用户截图里就是这个）
        for k in ("availableAccounts", "cnAccounts", "intlAccounts"):
            try:
                item[k] = int(item.get(k) or 0)
            except (TypeError, ValueError):
                item[k] = 0
        for k in ("cnFree", "intlFree"):
            if item.get(k) in (None, ""):
                item[k] = "-"
        if item.get("cost") in (None, ""):
            item["cost"] = "未观测"
        # 1) 网关实测：只有真跑过流量才是数字，其余是「未观测」/「-」
        cost = item.get("cost")
        if isinstance(cost, (int, float)) and cost > 0:
            item["credits"] = "x%s" % cost
            item["rate_source"] = "网关实测"
            continue
        # 2) 在线目录缓存
        hit = online.get(mid)
        if hit and hit.get("rate"):
            item["credits"] = hit.get("rate")
            item["ctx"] = hit.get("ctx", item.get("ctx", ""))
            item["out"] = hit.get("out", item.get("out", ""))
            item["rate_source"] = "线上目录"
            continue
        # 3) 内置表（merge 已填，未匹配时留空）

    with_rate = sum(1 for r in rows if r.get("credits"))
    return {
        "admin": m, "v1": v1, "models_enriched": rows,
        "online_models": len(online),
        "codebuddy_static": upstreams.CODEBUDDY_MODELS,
        "bundled": bundled_models.stats(),
        "rate_summary": {
            "total": len(rows), "with_rate": with_rate,
            "from_bundled": bundled_hit,
            "from_online": len(online),
        },
    }


def api_checkins(trigger=False, force=False):
    store = APP["store"]
    if not trigger:
        return ok({"today": store.checkin_today(), "summary": store.checkin_summary()})
    c = new_client()
    sched = APP["scheduler"]
    t = threading.Thread(target=sched.run_all_checkins, args=(c, force), daemon=True)
    t.start()
    return ok({"started": True, "message": "签到已触发，结果稍后刷新"})


def api_growth(trigger=False):
    c = new_client()
    if not trigger:
        g, st = safe(lambda: c.growth())
        if st != 200:
            return g, st
        return ok({"growth": g})
    sched = APP["scheduler"]
    threading.Thread(target=sched.run_growth_tasks, args=(c,), daemon=True).start()
    return ok({"started": True, "message": "成长任务已触发"})


def api_sites(action=None, body=None):
    store = APP["store"]
    if action == "add" or action == "update":
        s = body or {}
        if not s.get("name") or not s.get("url"):
            return fail("name 与 url 必填")
        if not s.get("id"):
            s["id"] = "s%d%d" % (now_ts(), len(store.get("sites")))
        s.setdefault("checkin", True)
        s.setdefault("auto", True)
        s.setdefault("category", "relay")
        s["url"] = s["url"].strip().rstrip("/")
        store.upsert_site(s)
        if s.get("in_route") or s.get("route_endpoint"):
            load_route_config(APP["router"], store, new_client())
        return ok({"site": s, "message": "已保存"})

    if action == "delete":
        sid = (body or {}).get("id")
        store.delete_site(sid)
        load_route_config(APP["router"], store, new_client())
        return ok({"message": "已删除"})

    if action == "import":
        items = (body or {}).get("sites") or []
        n = 0
        for s in items:
            if s.get("url") and s.get("name"):
                if not s.get("id"):
                    s["id"] = "s%d_%d" % (now_ts(), n)
                store.upsert_site(s)
                n += 1
        return ok({"imported": n})

    if action == "export":
        return ok({"sites": store.get("sites")})

    if action == "probe":
        sid = (body or {}).get("id")
        allsites = store.get("sites")
        site = next((x for x in allsites if x.get("id") == sid), None)
        if not site:
            return fail("站点不存在", code="not_found", status=404)
        return ok({"probe": sitecheck.probe_site(site)})

    if action == "probe_all":
        allsites = [s for s in store.get("sites")]
        results = []
        lock = threading.Lock()

        def work(s):
            r = sitecheck.probe_site(s)
            with lock:
                results.append(r)
        ts = [threading.Thread(target=work, args=(s,)) for s in allsites[:20]]
        for t in ts:
            t.start()
        for t in ts:
            t.join(timeout=40)
        return ok({"probed": len(results), "results": results})

    if action == "balance_all":
        allsites = [s for s in store.get("sites") if s.get("token") or s.get("cookie")]
        results = []
        lock = threading.Lock()

        def workb(s):
            r = sitecheck.fetch_balance(s)
            with lock:
                results.append(r)
        ts = [threading.Thread(target=workb, args=(s,)) for s in allsites[:20]]
        for t in ts:
            t.start()
        for t in ts:
            t.join(timeout=40)
        return ok({"results": results})

    if action == "run_one":
        sid = (body or {}).get("id")
        allsites = store.get("sites")
        site = next((x for x in allsites if x.get("id") == sid), None)
        if not site:
            return fail("站点不存在", code="not_found", status=404)
        r = sitecheck.checkin_site(dict(site, force=bool((body or {}).get("force"))))
        APP["store"].add_checkin({"site": site.get("id"), "name": site.get("name"),
                                  "ok": bool(r.get("ok")), "message": r.get("message"),
                                  "reward": r.get("reward"), "streak": r.get("streak"),
                                  "at": now_str()})
        return ok({"result": r})

    # 默认：站点 + 目录
    return ok({
        "sites": store.get("sites"),
        "catalog": catalog.all_sites(),
        "catalog_verified_at": catalog.VERIFIED_AT,
    })


def api_tasks(action=None, body=None):
    store = APP["store"]
    if action == "add":
        t = body or {}
        if not t.get("name"):
            return fail("任务名必填")
        store.add_task({"name": t["name"], "site": t.get("site", ""),
                        "kind": t.get("kind", "manual"), "note": t.get("note", "")})
        return ok({"message": "任务已添加"})
    if action == "toggle":
        store.toggle_task((body or {}).get("id"))
        return ok({"message": "已切换"})
    if action == "delete":
        store.delete_task((body or {}).get("id"))
        return ok({"message": "已删除"})
    if action == "run":
        tid = (body or {}).get("id")
        tasks = store.get("tasks")
        task = next((x for x in tasks if x.get("id") == tid), None)
        if not task:
            return fail("任务不存在", code="not_found", status=404)
        # 关联站点的任务：真实调用签到
        if task.get("site"):
            site = next((x for x in store.get("sites") if x.get("id") == task["site"]), None)
            if site:
                r = sitecheck.checkin_site(dict(site, force=True))
                store.run_task(tid, bool(r.get("ok")), r.get("message", ""))
                store.add_checkin({"site": site.get("id"), "name": site.get("name"),
                                   "ok": bool(r.get("ok")), "message": r.get("message"),
                                   "at": now_str()})
                return ok({"result": r, "task": task})
        store.run_task(tid, True, "手动标记完成")
        return ok({"message": "已执行", "task": task})
    return ok(store.task_summary())


def api_settings(action=None, body=None):
    store = APP["store"]
    # EXE 已移除：设置页不再无条件探网关（每次省 2-4s 死连接）。
    # 仅网关 dev 基建动作需要客户端。
    c = None

    def _client():
        return c or new_client()

    if action == "save":
        patch = body or {}
        cur = store.get("settings")
        cur.update(patch)
        store.put("settings", cur)
        c = new_client()
        if c.admin_password:
            try:
                c.login()
            except Exception:
                pass
        return ok({"settings": cur, "message": "已保存"})

    if action == "gateway_setup":
        u, p = (body or {}).get("username"), (body or {}).get("password")
        if not u or not p:
            return fail("username 与 password 必填")
        c = _client()
        r = safe_dict(lambda: c.setup(u, p))
        cur = store.get("settings")
        cur["gateway_admin_user"] = u
        cur["gateway_admin_password"] = p
        store.put("settings", cur)
        c.admin_user, c.admin_password = u, p
        try:
            c.login()
        except GatewayError as e:
            return ok({"setup": r, "logged_in": False,
                       "message": "控制台初始化返回 %s，但登录失败：%s（可能之前已用其他密码初始化过，"
                                  "请填对原密码）" % (r.get("code") or "?", e.message)}, status=200)
        return ok({"setup": r, "message": "网关控制台账号已初始化并登录"})

    if action == "gateway_login":
        c = _client()
        u = (body or {}).get("username") or c.admin_user
        p = (body or {}).get("password") or ""
        c.admin_user, c.admin_password = u, p
        try:
            good = c.login()
        except GatewayError as e:
            return ok({"logged_in": False, "message": "登录失败：%s" % e.message}, status=200)
        if good:
            cur = store.get("settings")
            cur["gateway_admin_user"] = u
            cur["gateway_admin_password"] = p
            store.put("settings", cur)
        return ok({"logged_in": good, "message": "登录成功" if good else "登录失败"})

    if action == "shutdown":
        def _bye():
            time.sleep(0.4)
            tr = APP.get("tray")
            if tr:
                tr.notify("AI 资源网关面板", "正在退出…", timeout=2000)
            time.sleep(0.6)
            srv = APP.get("server")
            if srv:
                try:
                    _shutdown(srv, quiet=True)
                except Exception:
                    pass
            os._exit(0)
        threading.Thread(target=_bye, daemon=True).start()
        return ok({"message": "面板将在 1 秒内退出"})

    if action == "probe_models":
        r = safe_dict(lambda: _client().probe_models((body or {}).get("models")))
        return r

    # 网关状态块：EXE 不在（常态）就不打 8317，直接如实标注
    gw = ({"ok": False, "code": "unreachable",
           "message": "网关 EXE 未运行（面板原生模式，无网关侧设置）"}
          if not (_gw_alive() or {}).get("alive")
          else safe_dict(lambda: _client().settings()))
    return ok({"settings": store.get("settings"), "gateway": gw,
               "gateway_path": APP["gateway_path"] or ""})


def api_creds(action=None, body=None):
    c = new_client()
    if action == "delete":
        r = safe_dict(lambda: c.delete_credential((body or {}).get("name")))
        return r
    if action == "start_login":
        r = safe_dict(lambda: c._request("/admin/api/login/start", "POST", {}))
        return r
    if action == "poll_login":
        r = safe_dict(lambda: c._request("/admin/api/login/poll", "GET"))
        return r
    r = safe_dict(lambda: c.credentials())
    return r


# ------------------------------------------------------------------ 账号登录
def api_login(action=None, body=None):
    """
    7 个本地网关 / 上游平台的账号登录。
      platforms          列出可登录的平台与方式
      start              发起登录，返回会话（二维码或待登录提示）
      poll               轮询会话状态
      submit             提交手动 Cookie / Key
      cancel             取消会话
    """
    from app.gwlogin import PLATFORMS, platform_list
    mgr = APP.get("login_manager")
    if mgr is None:
        return fail("登录管理器未初始化", code="not_ready")

    if action == "platforms":
        return ok({"platforms": platform_list()})

    if action == "browser":
        """
        只读诊断：本机浏览器 Cookie 能否自动取出。
        Chrome 127+ 用 app-bound 加密（v20），普通进程解不开，
        这时要告诉用户改走 CDP 或手动粘贴，而不是让他干等。
        """
        try:
            from app import browser_cookie
            return ok({"browser": browser_cookie.diagnose()})
        except Exception as e:
            return ok({"browser": {"profiles": [], "usable": False,
                                   "auto_supported": False,
                                   "reason": "诊断失败：%s" % e}})

    if action == "start":
        b = body or {}
        pid = b.get("platform")
        if not pid:
            return fail("platform 必填")
        if pid not in PLATFORMS:
            return fail("未知平台：%s" % pid)
        try:
            sess = mgr.start(pid, **{k: v for k, v in b.items() if k != "platform"})
        except Exception as e:
            return fail("启动登录失败：%s" % e, code="start_failed")
        return ok({"session": sess.to_dict()})

    if action == "qrscan":
        """服务端远程扫码：直接打平台 Web 二维码接口，无需本机浏览器。"""
        b = body or {}
        pid = b.get("platform")
        if not pid:
            return fail("platform 必填")
        if pid not in PLATFORMS:
            return fail("未知平台：%s" % pid)
        try:
            sess = mgr.start_remote_qr(pid)
        except Exception as e:
            return fail("发起远程扫码失败：%s" % e, code="qrscan_failed")
        return ok({"session": sess.to_dict()})

    if action == "poll":
        b = body or {}
        sid = b.get("id") or b.get("session_id")
        if not sid:
            return fail("会话 id 必填")
        sess = mgr.poll(sid)
        if sess is None:
            return fail("会话不存在或已过期", code="no_session", status=404)
        return ok({"session": sess.to_dict()})

    if action == "submit":
        b = body or {}
        sid = b.get("id") or b.get("session_id")
        sess = mgr.get(sid) if sid else None
        if sess is None:
            # 没有会话时直接落库（手动添加的凭据）
            acc = APP["accounts"].add({
                "platform": b.get("platform", ""),
                "name": b.get("name") or b.get("account") or b.get("platform", ""),
                "type": b.get("type", "cookie"),
                "secret": b.get("secret") or b.get("cookie") or "",
                "source": "手动添加",
            })
            return ok({"account": APP["accounts"]._mask(acc), "message": "已保存"})
        sess = mgr.submit(sid, **{k: v for k, v in b.items()
                                  if k not in ("id", "session_id")})
        return ok({"session": sess.to_dict()})

    if action == "cancel":
        b = body or {}
        mgr.cancel(b.get("id") or b.get("session_id"))
        return ok({"message": "已取消"})

    # 默认：列出平台 + 进行中的会话
    return ok({
        "platforms": platform_list(),
        "sessions": [s.to_dict() for s in mgr.sessions.values()],
    })


# ------------------------------------------------------------------ 账号池
def api_accounts(action=None, body=None):
    acc = APP.get("accounts")
    if acc is None:
        return fail("账号池未初始化", code="not_ready")
    b = body or {}

    if action == "add":
        if not b.get("platform"):
            return fail("platform 必填")
        item = acc.add({
            "platform": b["platform"],
            "name": b.get("name") or b.get("account") or b["platform"],
            "type": b.get("type", "cookie"),
            "secret": b.get("secret") or b.get("cookie") or b.get("api_key") or "",
            "source": b.get("source", "手动添加"),
            "edition": b.get("edition", ""),
            "note": b.get("note", ""),
        })
        return ok({"account": acc._mask(item), "message": "已保存"})

    if action == "delete":
        okk = acc.delete(b.get("id"))
        return ok({"deleted": okk}) if okk else fail("账号不存在", status=404)

    if action == "update":
        item = acc.update(b.get("id"), b)
        if not item:
            return fail("账号不存在", status=404)
        return ok({"account": acc._mask(item)})

    return ok({
        "accounts": acc.list(b.get("platform")),
        "stats": acc.stats(),
    })


# ------------------------------------------------------------------ 平台能力
def api_platform(action=None, body=None):
    """
    调用各 APP 网关内部的接口（签到 / 任务 / 积分 / 兑换 / 送积分 …）。
      actions   列出某平台支持的动作
      call      执行一个动作
    """
    from app import gwextra
    acc = APP.get("accounts")
    b = body or {}

    if action == "actions":
        pid = b.get("platform")
        if pid:
            return ok({"platform": pid, "actions": gwextra.actions_of(pid)})
        return ok({p: gwextra.actions_of(p) for p in gwextra.ACTIONS})

    if action == "call":
        pid = b.get("platform")
        act = b.get("action")
        if not pid or not act:
            return fail("platform 与 action 必填")
        okk, data = gwextra.call(acc, pid, act, b.get("params") or {},
                                 b.get("account_id"),
                                 base_override=_lobster_base())
        return ok({"ok": okk, "result": data}) if okk else fail(
            data if isinstance(data, str) else json.dumps(
                data, ensure_ascii=False)[:500], code="platform_call_failed")

    if action == "probe":
        # 动态探测平台真实端点（应对「整站 401、404 探测法失效」的平台）
        pid = b.get("platform")
        if not pid:
            return fail("platform 必填")
        r = gwextra.probe_endpoints(acc, pid, b.get("account_id"),
                                     b.get("paths"))
        return ok(r) if r.get("ok") else fail(
            r.get("message") or "探测失败", code=r.get("code") or "probe_failed")

    if action == "checkin":
        pid = b.get("platform", "apk-trae")
        if pid == "apk-trae":
            okk, data = gwextra.trae_checkin_flow(acc, b.get("account_id"))
        elif pid == "apk-raccoon":
            okk, data = gwextra.raccoon_login_bonus(acc, b.get("account_id"))
        elif pid == "apk-codebuddy":
            # CodeBuddy 是单接口直接领（/v2/billing/meter/daily-checkin）
            okk, data = gwextra.codebuddy_checkin(acc, b.get("account_id"))
        else:
            return fail("平台 %s 暂无一键签到" % pid)
        return ok({"ok": okk, "result": data}) if okk else fail(
            data if isinstance(data, str) else json.dumps(
                data, ensure_ascii=False)[:500], code="checkin_failed")

    return ok({"platforms": list(gwextra.ACTIONS)})


def api_notify(action=None, body=None):
    store = APP["store"]
    from app.notify import WEBHOOK_TYPES
    if action == "add":
        h = body or {}
        if not h.get("url"):
            return fail("url 必填")
        cfg = store.get("notify")
        cfg.setdefault("webhooks", []).append(h)
        store.put("notify", cfg)
        return ok({"message": "已添加"})
    if action == "delete":
        idx = (body or {}).get("index")
        cfg = store.get("notify")
        hooks = cfg.get("webhooks") or []
        if 0 <= idx < len(hooks):
            hooks.pop(idx)
            cfg["webhooks"] = hooks
            store.put("notify", cfg)
        return ok({"message": "已删除"})
    if action == "test":
        from app.notify import send_one
        okk, msg = send_one(body or {}, "AI 资源网关面板", "通知测试成功 ✅\n" + now_str())
        return ok({"sent": okk, "message": msg})
    if action == "save_flags":
        patch = (body or {}).get("notify") or {}
        cfg = store.get("notify")
        for k in ("notify_checkin", "notify_quota", "notify_error"):
            if k in patch:
                cfg[k] = bool(patch[k])
        store.put("notify", cfg)
        return ok({"notify": cfg, "message": "已保存"})
    if action == "send":
        from app.notify import Notifier
        n = Notifier(store)
        res = n.notify((body or {}).get("event", "checkin"),
                       (body or {}).get("title", "测试通知"),
                       (body or {}).get("body", "来自整合面板的手动通知"))
        return ok({"results": res})
    cfg = store.get("notify")
    return ok({"notify": cfg, "types": WEBHOOK_TYPES})


def api_logs():
    # EXE 已移除：网关不在就不再硬探 8317（每次省 ~2s）
    if (_gw_alive() or {}).get("alive"):
        g = safe_dict(lambda: new_client().logs(200))
    else:
        g = {"ok": False, "code": "unreachable",
             "message": "网关 EXE 未运行（面板原生模式，仅面板侧日志）"}
    return ok({"gateway": g, "panel": LOG_LINES[-200:]})


def api_catalog():
    return ok({
        "verified_at": catalog.VERIFIED_AT,
        "local_gateways": catalog.LOCAL_GATEWAYS,
        "relay_sites": catalog.RELAY_SITES,
        "official": catalog.OFFICIAL_PLATFORMS,
        "open_source": catalog.OPEN_SOURCE,
        "apk_analysis": catalog.APK_ANALYSIS,
    })


def api_localproxy(action=None, body=None):
    """
    本机反代上游管理（内置 CLIProxyAPI）
      status  二进制/服务/凭据/模型 状态
      start   启动（端口占用自动顺延）
      stop    停止
      restart 重启
      config  改端口 / API Key
      login   触发 OAuth 登录，返回授权链接
      log     看服务日志
    """
    from app import localproxy as LP
    b = body or {}

    if action == "start":
        okk, msg = LP.start(port=b.get("port"), key=b.get("key"))
        return ok({"message": msg}) if okk else fail(msg, code="start_failed")
    if action == "stop":
        killed = LP.stop()
        return ok({"killed": killed,
                   "message": "已停止（%d 个进程）" % len(killed) if killed
                   else "本来就没在运行"})
    if action == "restart":
        LP.stop()
        okk, msg = LP.start(port=b.get("port"), key=b.get("key"))
        return ok({"message": msg}) if okk else fail(msg, code="restart_failed")
    if action == "config":
        okk, msg = LP.write_config(port=b.get("port"), key=b.get("key"),
                                   host=b.get("host"))
        if not okk:
            return fail(msg, code="config_failed")
        if b.get("apply") and LP.status()["online"]:
            LP.restart(port=b.get("port"), key=b.get("key"))
        return ok({"message": msg, "status": LP.status()})
    if action == "login":
        okk, d = LP.start_login(b.get("provider") or "",
                                no_browser=b.get("no_browser", True))
        return ok(d) if okk else fail(d.get("message", "登录失败"),
                                     code="login_failed")
    if action == "log":
        return ok({"log": LP.tail_log(int(b.get("lines") or 80))})
    return ok(LP.status())


def api_upstreams():
    """全量上游档案（7 本地网关 + 17 公益站 + 22 官方 + 12 Vibe Coding 反代）"""
    st = upstreams.stats()
    # 本机部署的反代探活（哪些真在跑）
    live = {}
    try:
        from app import gwextra
        for it in gwextra.probe_local_upstreams():
            live[it["id"]] = it
    except Exception as e:
        live = {"_error": str(e)}
    return ok({
        "stats": st,
        "gateways": upstreams.GATEWAYS,
        "relays": upstreams.RELAYS,
        "official": upstreams.OFFICIAL,
        "open_source": upstreams.OPEN_SOURCE,
        "vibe_proxy": upstreams.VIBE_PROXY,
        "vibe_dead": upstreams.VIBE_DEAD,
        "codebuddy_models": upstreams.CODEBUDDY_MODELS,
        "flat": upstreams.all_upstreams(),
        "local_probes": live,
        # 兼容旧字段
        "all": upstreams.all_upstreams(),
    })


def api_route(action=None, body=None):
    """智能路由管理"""
    router = APP["router"]
    store = APP["store"]
    s = store.get("settings")
    body = body or {}

    if action == "test_chat":
        model = body.get("model") or "auto"
        msg = body.get("message") or "ping"
        strategy = body.get("strategy") or router.config.get(model, "fastest")
        payload = {"model": model, "messages": [{"role": "user", "content": msg}],
                   "stream": False}
        tracks = []

        def on_route(t, dt, ok, err):
            tracks.append({"upstream": t.name, "endpoint": t.endpoint,
                           "latency_ms": round(dt), "ok": ok, "error": err})
        result, tried = router.chat(model, payload, strategy=strategy,
                                    session_id=body.get("session"), on_route=on_route)
        return ok({"result": result, "track": tracks,
                   "tried": [{"upstream": a, "ms": b} for a, b in tried],
                   "strategy": strategy})

    if action == "probe":
        res = router.probe_all()
        return ok({"results": res, "models": router.list_models()})

    if action == "pick":
        model = body.get("model") or "auto"
        strategy = body.get("strategy") or router.config.get(model, "fastest")
        t, ordered = router.pick(model, strategy=strategy,
                                 require_tools=bool(body.get("tools")),
                                 require_vision=bool(body.get("vision")))
        return ok({"picked": t.snapshot() if t else None,
                   "ordered": [x.snapshot() for x in ordered],
                   "strategy": strategy})

    if action == "save_models":
        cfg = store.get("settings")
        cur = cfg.get("routes") or {}
        cur["models"] = body.get("models") or DEFAULT_AUTO_MODELS
        cfg["routes"] = cur
        store.put("settings", cfg)
        load_route_config(router, store, new_client())
        return ok({"models": router.list_models(), "message": "路由配置已保存并生效"})

    if action == "reset":
        cfg = store.get("settings")
        cur = cfg.get("routes") or {}
        cur["models"] = DEFAULT_AUTO_MODELS
        cfg["routes"] = cur
        store.put("settings", cfg)
        load_route_config(router, store, new_client())
        return ok({"models": router.list_models(), "message": "已恢复默认自动模型"})

    # 默认：路由总览
    cfg = s.get("routes") or {}
    return ok({
        "models": router.list_models(),
        "config": router.config,
        "defaults": DEFAULT_AUTO_MODELS,
        "auto_probe": s.get("router_auto_probe", True),
        "probe_interval": s.get("router_probe_interval", 60),
        "breaker_fail": s.get("breaker_fail", 3),
        "breaker_cooldown": s.get("breaker_cooldown", 60),
        "in_route_sites": [{"id": x.get("id"), "name": x.get("name"),
                            "weight": x.get("route_weight", 100),
                            "priority": x.get("route_priority", 10)}
                           for x in store.get("sites") if x.get("in_route")],
    })


ROUTES = {
    "/api/overview": lambda q, b: api_overview(),
    "/api/usage": lambda q, b: api_usage(q.get("range", ["all"])[0]),
    "/api/models": lambda q, b: api_models(q.get("action", [None])[0], b),
    "/api/checkins": lambda q, b: api_checkins(q.get("trigger", ["0"])[0] == "1",
                                               q.get("force", ["0"])[0] == "1"),
    "/api/growth": lambda q, b: api_growth(q.get("trigger", ["0"])[0] == "1"),
    "/api/sites": lambda q, b: api_sites(q.get("action", [None])[0], b),
    "/api/tasks": lambda q, b: api_tasks(q.get("action", [None])[0], b),
    "/api/settings": lambda q, b: api_settings(q.get("action", [None])[0], b),
    "/api/credentials": lambda q, b: api_creds(q.get("action", [None])[0], b),
    "/api/login": lambda q, b: api_login(q.get("action", [None])[0], b),
    "/api/accounts": lambda q, b: api_accounts(q.get("action", [None])[0], b),
    "/api/platform": lambda q, b: api_platform(q.get("action", [None])[0], b),
    "/api/notify": lambda q, b: api_notify(q.get("action", [None])[0], b),
    "/api/logs": lambda q, b: api_logs(),
    "/api/toolcall": lambda q, b: api_toolcall(q.get("action", ["list"])[0], b),
    "/api/localproxy": lambda q, b: api_localproxy(q.get("action", ["status"])[0], b),
    "/api/tencent": lambda q, b: api_tencent(q.get("action", ["probe"])[0], b),
    "/api/autocheckin": lambda q, b: api_autocheckin(q.get("action", ["status"])[0], b),
    "/api/catalog": lambda q, b: api_catalog(),
    "/api/upstreams": lambda q, b: api_upstreams(),
    "/api/route": lambda q, b: api_route(q.get("action", [None])[0], b),
}


# ------------------------------------------------------------------ HTTP
def dispatch(path, q, body):
    """统一路由分发：任何单接口异常都不允许打挂连接"""
    fn = ROUTES.get(path)
    if fn is None:
        return fail("not found: %s" % path, code="not_found", status=404)
    try:
        res = fn(q, body)
    except GatewayError as e:
        return fail(e.message, code=e.code, status=502)
    except Exception as e:
        import traceback
        log("接口 %s 异常：%s\n%s" % (path, e, traceback.format_exc()))
        return fail("%s: %s" % (type(e).__name__, e), code="internal", status=500)
    if isinstance(res, tuple):
        obj, st = res
        if isinstance(obj, dict) and obj.get("ok") is False and st == 400:
            return obj, st
        return obj, st
    return res, 200


class Handler(BaseHTTPRequestHandler):
    server_version = "AIGWPanel/1.0"

    def log_message(self, fmt, *args):
        pass

    def _send(self, obj, status=200):
        body = json.dumps(obj, ensure_ascii=False, default=str).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "*")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionAbortedError):
            pass

    def _send_sse(self, chunk_iter):
        """
        流式 SSE 响应（对齐社区 2api 标配）：逐帧写出并 flush，
        结束即断开（Connection: close，无 Content-Length）。
        """
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.close_connection = True
        try:
            for frame in chunk_iter:
                if frame:
                    self.wfile.write(frame.encode("utf-8"))
                    self.wfile.flush()
        except (BrokenPipeError, ConnectionAbortedError):
            pass

    def _sse_text_chunks(self, text, model):
        """把一段完整文本包成 OpenAI SSE（网页反代非流式聚合后回放用）。"""
        cid = "chatcmpl-" + uuid.uuid4().hex[:24]
        yield "data: " + json.dumps({
            "id": cid, "object": "chat.completion.chunk",
            "created": int(time.time()), "model": model,
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""},
                         "finish_reason": None}]}, ensure_ascii=False) + "\n\n"
        if text:
            yield "data: " + json.dumps({
                "id": cid, "object": "chat.completion.chunk",
                "created": int(time.time()), "model": model,
                "choices": [{"index": 0, "delta": {"content": text},
                             "finish_reason": None}]}, ensure_ascii=False) + "\n\n"
        yield "data: " + json.dumps({
            "id": cid, "object": "chat.completion.chunk",
            "created": int(time.time()), "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ensure_ascii=False) + "\n\n"
        yield "data: [DONE]\n\n"

    def _sse_error(self, msg):
        return "data: " + json.dumps({"aigw_error": True, "message": msg}) + "\n\n"

    def _static(self, path):
        if path in ("/", ""):
            path = "/index.html"
        rel = path.lstrip("/").replace("..", "")
        full = os.path.join(APP["static_dir"], rel)
        if not os.path.isfile(full):
            self._send({"ok": False, "message": "not found: %s" % path}, 404)
            return
        ctype = mimetypes.guess_type(full)[0] or "application/octet-stream"
        with open(full, "rb") as f:
            data = f.read()
        self.send_response(200)
        self.send_header("Content-Type", ctype + ("; charset=utf-8" if "text" in ctype or
                                                  "javascript" in ctype else ""))
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionAbortedError):
            pass

    def do_OPTIONS(self):
        self._send({})

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        q = urllib.parse.parse_qs(u.query)
        if u.path in ROUTES:
            obj, st = dispatch(u.path, q, None)
            self._send(obj, st)
            return
        # 面板自身的 OpenAI 兼容聚合端点
        if u.path in ("/v1/models", "/g/v1/models"):
            self._send(self._agg_models())
            return
        if u.path in ("/healthz",):
            self._send({"ok": True, "uptime": now_ts() - APP["started"]})
            return
        self._static(u.path)

    def do_POST(self):
        import urllib.parse as _up
        u = _up.urlparse(self.path)
        q = _up.parse_qs(u.query)
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        try:
            body = json.loads(raw.decode("utf-8")) if raw else {}
        except Exception:
            body = {}
        if u.path in ROUTES:
            obj, st = dispatch(u.path, q, body)
            self._send(obj, st)
            return
        # 聚合转发：/g/v1/chat/completions → 本地网关
        if u.path in ("/v1/chat/completions", "/g/v1/chat/completions"):
            self._proxy_chat(body)
            return
        # ★ Anthropic 兼容：/v1/messages（等价网关 EXE 的同名能力，免 EXE）
        if u.path in ("/v1/messages", "/g/v1/messages"):
            self._proxy_messages(body)
            return
        self._send({"ok": False, "message": "not found"}, 404)

    def _agg_models(self):
        """聚合模型列表：自动路由 + 原生中继（豆包裸名 / @平台id）+ 网关（可选）

        EXE 移除后对外模型清单以原生能力为主体；网关仅在活着时追加，
        避免每次拉清单都吃 2s 死连接（CodeDesk 等客户端会高频轮询这里）。
        """
        out = []
        router = APP.get("router")
        if router:
            for name, info in sorted(router.list_models().items()):
                out.append({
                    "id": name, "object": "model", "owned_by": "aigw-router",
                    "aigw": {"strategy": router.config.get(name, "fastest"),
                             "targets": len(info["targets"]),
                             "healthy": info["healthy"]},
                })
        # ★ 豆包裸名（samantha 原生中继，model 直接写 doubao-pro 等）
        from app.doubao_relay import MODE_FLAGS as _DB_FLAGS
        for mid in _DB_FLAGS:
            out.append({"id": mid, "object": "model", "owned_by": "apk-doubao",
                        "aigw": {"via": "doubao_relay"}})
        # ★ 原生中继模型：<模型id>@<平台id>。静态清单来自 APK/社区逆向；
        #   anon-zen 动态清单走 zen_models（内置 TTL 缓存）。
        from app import native_relay as NR
        from app import bundled_trae as _BT, bundled_go as _BG, \
            bundled_models as _BM, bundled_yuanbao as _BY
        _BUNDLED = {"apk-trae": _BT.MODELS, "apk-go": _BG.MODELS,
                    "apk-codebuddy": _BM.MODELS, "apk-codebuddy-cn": _BM.MODELS,
                    "apk-yuanbao": _BY.MODELS}
        for pid, spec in NR.RELAY_SPECS.items():
            rows = spec.get("models_static") or _BUNDLED.get(pid) or []
            for m in rows:
                mid = (m.get("id") if isinstance(m, dict) else str(m)) or ""
                if not mid or mid == "auto":
                    continue
                out.append({"id": "%s@%s" % (mid, pid), "object": "model",
                            "owned_by": pid, "aigw": {"via": "relay"}})
            if spec.get("models_anon") and pid == "anon-zen":
                try:
                    _ok, _zm = NR.zen_models()
                    for m in (_zm if _ok and isinstance(_zm, list) else []):
                        out.append({"id": "%s@%s" % (m["id"], pid),
                                    "object": "model", "owned_by": pid,
                                    "aigw": {"via": "relay"}})
                except Exception:
                    pass
        # 网关（可选组件）：活着才追加，死了绝不拖慢清单。
        # ⚠ _gw_alive() 返回缓存 dict（永远真值），必须取 ["alive"]。
        if (_gw_alive() or {}).get("alive"):
            c = new_client()
            try:
                v1 = c.v1_models()
                for m in (v1.get("data") or []):
                    out.append({"id": m.get("id"), "object": "model",
                                "owned_by": "workbuddy-gateway"})
            except Exception:
                pass
        if not out:
            for s in APP["store"].get("sites"):
                out.append({"id": "site/%s" % s.get("name"), "object": "model",
                            "owned_by": s.get("url")})
        return {"object": "list", "data": out}

    def _proxy_chat(self, body):
        """
        面板即 OpenAI 端点。
        model 命中自动模型（auto / free）→ 走智能路由；
        model 带「@平台id」后缀（如 doubao-pro@apk-doubao）→ 原生中继直连上游
          （端点/头组静态提取自各 APK/EXE 字节码，见 app/native_relay.py）；
        否则透传到本地网关（若在运行）。
        """
        model = (body or {}).get("model") or ""
        router = APP.get("router")
        if router and model in router.list_models():
            strategy = (body.pop("aigw_strategy", None)
                        or router.config.get(model, "fastest"))
            session = body.pop("aigw_session", None)
            tracks = []

            def on_route(t, dt, ok, err):
                tracks.append({"upstream": t.name, "latency_ms": round(dt),
                               "ok": ok, "error": err})

            result, tried = router.chat(model, body, strategy=strategy,
                                        session_id=session, on_route=on_route)
            log("路由 %s[%s] → %s（尝试 %d 次）" % (
                model, strategy,
                (result.get("_route") or {}).get("upstream", "失败"), len(tried)))
            # ★ 全链路用量记录（路由路径中央记账；流式回放前落，
            #   聚合结果里的 usage 是上游真实值）
            _rt = result.get("_route") or {}
            _tin, _tout = _usage_from_payload(result)
            _record_usage(
                str(_rt.get("upstream_id") or "route").replace("native:", ""),
                model, _tin, _tout,
                (_rt.get("latency_ms") or 0) / 1000.0,
                "choices" in result, upstream=_rt.get("upstream", ""))
            if result.get("ok") is False and "choices" not in result:
                self._send(result, 503 if result.get("code") == 503 else 502)
                return
            result["_aigw_route"] = tracks
            # ★ 流式支持（ZCode/Cherry 等客户端默认 stream:true）：
            #   router.chat 是聚合结果，这里按 OpenAI SSE 帧回放，
            #   否则客户端收不到 data: 帧 → 报 empty_model_response。
            if (body or {}).get("stream"):
                def _rgen():
                    try:
                        txt = (((result.get("choices") or [{}])[0]
                                .get("message") or {}).get("content", "") or "")
                    except Exception:
                        txt = ""
                    if not txt:
                        yield self._sse_error(
                            "自动路由未返回内容：%s"
                            % (result.get("error") or result.get("message")
                               or "上游返回空"))
                        return
                    for piece in self._sse_text_chunks(
                            txt, result.get("model") or model):
                        yield piece
                self._send_sse(_rgen())
                return
            self._send(result, 200)
            return

        # ★ 豆包原生中继：APK 网关模型 id（doubao-pro/think/expert…）直接面板出话
        #   （samantha 协议移植自社区逆向实现 + dex 证据，见 app/doubao_relay.py）。
        #   请求级换号：失败自动换下一个账号重试（最多 3 次，社区 2api 标配）；
        #   401/403 → 硬冷却 12h，429 → 软冷却 60s。model 可带 @apk-doubao 也可裸写。
        base_model = model.rpartition("@")[0] or model
        if base_model in __import__("app.doubao_relay", fromlist=["MODE_FLAGS"]).MODE_FLAGS:
            from app import doubao_relay as DR
            acc = APP.get("accounts")
            rbody = dict(body or {})
            rbody["model"] = base_model
            msgs = rbody.get("messages") or []

            def _db_call(secret, name, row=None):
                t0 = time.time()
                okk, payload = DR.chat(msgs, secret, model=base_model)
                _record_usage("apk-doubao", base_model,
                              (payload.get("usage") or {}).get("prompt_tokens")
                              if okk else 0,
                              (payload.get("usage") or {}).get("completion_tokens")
                              if okk else 0, time.time() - t0, okk)
                if not okk:
                    st = payload.get("status") if isinstance(payload, dict) else 0
                    if st in (401, 403):
                        payload = {"message": payload, "cooldown": "hard"}
                    elif st == 429:
                        payload = {"message": payload, "cooldown": "soft"}
                return okk, payload

            # ---- 流式：SSE 直通（对齐社区 2api 标配）----
            if (body or {}).get("stream"):
                def _gen():
                    def stream_one(secret, name):
                        return DR.chat_stream(msgs, secret, model=base_model)
                    # 流式换号：先取一个号，流内出错帧时由客户端重试
                    # （SSE 一旦开始无法换号——与 lobsterai2api 同样的限制）
                    secret, name = DR.pick_secret(acc, "apk-doubao")
                    if not secret:
                        yield 'data: ' + json.dumps(
                            {"aigw_error": True, "code": "no_credential",
                             "message": "账号池里没有可用的豆包 Cookie（全部冷却或未登录）",
                             "pool": DR.pool_status(acc, "apk-doubao")}) + "\n\n"
                        return
                    yield from DR.chat_stream(msgs, secret, model=base_model)
                self._send_sse(_gen())
                return

            okk, payload, acct_name = DR.try_accounts(acc, "apk-doubao", _db_call)
            if okk:
                payload["model"] = base_model
                payload["_aigw_route"] = [{"upstream": "豆包原生中继",
                                           "account": acct_name}]
                self._send(payload, 200)
            else:
                try:
                    n = (APP.get("scheduler") and APP["scheduler"].notifier)
                    if n:
                        n.notify("relay_error", "豆包中继失败",
                                 "换号 %d 次后仍失败：%s" % (
                                     3, str(payload)[:160]))
                except Exception:
                    pass
                self._send({"ok": False, "code": "relay_failed",
                            "message": str(payload)[:400],
                            "pool": DR.pool_status(acc, "apk-doubao")}, 502)
            return

        # ★ 原生中继：model 带 @pid 后缀 → 直接按 APK 提取的端点直连上游
        #   请求级换号：失败自动换下一个账号（最多 3 次，社区 2api 标配）；
        #   stream=true 且上游为 OpenAI 格式 → SSE 逐帧透传。
        if "@" in model:
            mid, _, pid = model.rpartition("@")
            from app import native_relay as NR
            spec = NR.relay_of(pid)
            if spec:
                acc = APP.get("accounts")
                rbody = dict(body or {})
                rbody["model"] = mid

                # ★ 网页版反代（chatglm.cn / Trae 直连 / DeepSeek 网页版）：
                #   走 app/web_relays.py（非 OpenAI 原生格式，需协议转换）。
                if spec.get("web_relay"):
                    from app import web_relays as WR
                    secret, _ = (APP.get("accounts") and
                                 __import__("app.acct_pool", fromlist=["pick_secret"]).pick_secret(acc, pid)) or ("", "")
                    if not secret:
                        self._send({"ok": False, "code": "no_credential",
                                    "message": "账号池里没有 %s 的可用凭据（先去接入源页接入）" % pid}, 502)
                        return
                    if (body or {}).get("stream"):
                        def _wgen():
                            okk, payload = WR.chat(pid, rbody.get("messages") or [], secret, mid)
                            _record_usage(pid, mid, 0, 0, 0, okk, upstream=pid)
                            if okk:
                                txt = (payload.get("choices") or [{}])[0].get("message", {}).get("content", "")
                                for piece in self._sse_text_chunks(txt, mid):
                                    yield piece
                            else:
                                yield self._sse_error(str(payload))
                        self._send_sse(_wgen())
                        return
                    _t0 = time.time()
                    okk, payload = WR.chat(pid, rbody.get("messages") or [], secret, mid)
                    _tin, _tout = _usage_from_payload(payload) if okk else (0, 0)
                    _record_usage(pid, mid, _tin, _tout, time.time() - _t0, okk, upstream=pid)
                    if okk:
                        self._send(payload, 200)
                    else:
                        self._send({"ok": False, "code": "relay_failed",
                                    "message": str(payload)[:400]}, 502)
                    return

                if (body or {}).get("stream"):
                    def _gen():
                        secret, _ = (APP.get("accounts") and __import__("app.acct_pool", fromlist=["pick_secret"]).pick_secret(acc, pid)) or ("", "")
                        yield from NR.relay_stream(pid, rbody, secret,
                                                   base_override=_lobster_base())
                    self._send_sse(_gen())
                    return

                def _call(secret, name, row=None):
                    okk, payload = NR.relay_once(pid, rbody, secret,
                                                 base_override=_lobster_base(),
                                                 account=row)
                    if not okk and isinstance(payload, dict) \
                            and payload.get("status") in (401, 403):
                        payload = {"message": payload, "cooldown": "hard"}
                    return okk, payload

                # 匿名车道（anon-zen 等）：公共凭据，无需账号池，直接打
                if spec.get("secret_fixed"):
                    _t0 = time.time()
                    okk, payload = NR.relay_once(pid, rbody, "",
                                                 base_override=_lobster_base())
                    _tin, _tout = _usage_from_payload(payload) if okk else (0, 0)
                    _record_usage(pid, mid, _tin, _tout, time.time() - _t0, okk, upstream=pid)
                    self._send(payload if okk else
                               {"ok": False, "code": "relay_failed",
                                "message": str(payload)[:400]}, 200 if okk else 502)
                    return

                _t0 = time.time()
                okk, payload, _acct = __import__("app.acct_pool", fromlist=["try_accounts"]).try_accounts(acc, pid, _call)
                if okk:
                    _tin, _tout = _usage_from_payload(payload)
                    _record_usage(pid, mid, _tin, _tout, time.time() - _t0, True, upstream=pid)
                    self._send(payload, 200)
                else:
                    _record_usage(pid, mid, 0, 0, time.time() - _t0, False, upstream=pid)
                    self._send({"ok": False, "code": "relay_failed",
                                "message": str(payload)[:400],
                                "relay": NR.relay_stream_meta(pid)}, 502)
                return

        # —— 终态：未知模型。build26 移除 EXE 兜底后这里曾**直落无响应**
        #    （方法走完什么都不发 → 连接被掐，CodeDesk 等客户端表现为
        #    「连接失败」）。必须回结构化 OpenAI 错误 + 可用模型族提示。
        self._send({"error": {
            "message": "未知模型 %r。可用：auto / free（自动路由/免费优先）、"
                       "doubao-*（豆包裸名）、"
                       "<模型id>@<平台id>（原生中继）；完整清单 GET /v1/models"
                       % model,
            "type": "invalid_request_error", "code": "model_not_found"},
            "ok": False}, 404)

    def _proxy_messages(self, body):
        """
        Anthropic /v1/messages 兼容端点（等价网关 EXE 的同名能力，免 EXE）。
        model=claude-* 且账号池有豆包 Cookie → 映射豆包模式原生出话；
        其它模型 → 交给 _proxy_chat 的既有链路（路由/中继/EXE），
        最后把 OpenAI 形状转回 Anthropic 形状。非流式。
        """
        model = (body or {}).get("model") or ""
        msgs_in = (body or {}).get("messages") or []
        system = (body or {}).get("system") or ""

        def blocks_text(c):
            if isinstance(c, str):
                return c
            if isinstance(c, list):
                return chr(10).join(b.get("text", "") for b in c
                                    if isinstance(b, dict) and b.get("type") == "text")
            return str(c or "")

        oa_msgs = []
        if system:
            sys_text = blocks_text(system)
            if sys_text:
                oa_msgs.append({"role": "system", "content": sys_text})
        for m in msgs_in:
            oa_msgs.append({"role": m.get("role", "user"),
                            "content": blocks_text(m.get("content"))})

        def to_anthropic(oa):
            ch = ((oa.get("choices") or [{}])[0])
            msgd = ch.get("message") or {}
            text = msgd.get("content") or ""
            u = oa.get("usage") or {}
            return {
                "id": oa.get("id") or ("msg_" + uuid.uuid4().hex[:24]),
                "type": "message", "role": "assistant",
                "model": model,
                "content": [{"type": "text", "text": text}],
                "stop_reason": "end_turn", "stop_sequence": None,
                "usage": {"input_tokens": u.get("prompt_tokens", 1),
                          "output_tokens": u.get("completion_tokens", 1)},
            }

        # claude-* → 豆包模式映射（等价 free-api 的 ANTHROPIC_MODEL_MAP）
        DB_MAP = {"claude-3-5-sonnet-latest": "doubao-pro",
                  "claude-3-5-haiku-latest": "doubao-fast",
                  "claude-3-opus-latest": "doubao-expert",
                  "claude-sonnet-4": "doubao-pro",
                  "claude-opus-4": "doubao-expert"}
        db_model = None
        ml = model.lower()
        for k, v in DB_MAP.items():
            if ml.startswith(k):
                db_model = v
                break
        if db_model:
            from app import doubao_relay as DR
            acc = APP.get("accounts")
            secret, acct_name = DR.pick_secret(acc, "apk-doubao")
            if not secret:
                self._send({"type": "error", "error": {
                    "type": "authentication_error",
                    "message": "账号池里没有可用的豆包 Cookie（全部冷却或未登录）"}},
                    401)
                return
            t0 = time.time()
            okk, payload = DR.chat(oa_msgs, secret, model=db_model)
            _record_usage("apk-doubao", db_model,
                          (payload.get("usage") or {}).get("prompt_tokens") if okk else 0,
                          (payload.get("usage") or {}).get("completion_tokens") if okk else 0,
                          time.time() - t0, okk)
            if okk:
                out = to_anthropic(payload)
                out["model"] = model
                self._send(out, 200)
            else:
                self._send({"type": "error", "error": {
                    "type": "api_error", "message": str(payload)[:400]}}, 502)
            return

        # 其它模型：转 OpenAI 后走既有链路，再转回 Anthropic
        oa_body = {"model": model, "messages": oa_msgs,
                   "max_tokens": (body or {}).get("max_tokens") or 4096}
        self._proxy_chat(oa_body)
        # _proxy_chat 直接写回响应（未知模型也会回结构化错误，不会直落），
        # 这里必须 return——下面曾残留一段「透传网关」旧代码，会在同一连接
        # 上发第二次响应（响应帧错乱）并白等 2s 死网关。EXE 已移除，整段删除。
        return


def _icon_path():
    cands = [os.path.join(APP["static_dir"], "aigw.ico")]
    if hasattr(sys, "_MEIPASS"):
        cands.insert(0, os.path.join(sys._MEIPASS, "app", "static", "aigw.ico"))
    for c in cands:
        if os.path.exists(c):
            return c
    return None


def main():
    from app import tray as traymod

    # 1) 任务栏分组（不设这条，Windows 不给任务栏位置）
    traymod.set_taskbar_appid()

    # 2) 单实例：已运行时唤醒老实例，不重复开
    guard = traymod.SingleInstance()
    tray_mode = "--tray" in sys.argv
    if not guard.acquire():
        log("检测到已有实例运行，唤醒其面板后退出")
        guard.find_existing_window()
        if os.environ.get("AIGW_NO_BROWSER") != "1":
            webbrowser.open(_last_url_path() or "http://127.0.0.1:8790/")
        guard.release()
        return

    init_app()
    # ★ 固定端口策略（B 案，2026-10-06）：默认 8800，**被占就报错退出**，
    #   不再静默顺延 —— 客户端配置一次永久有效，不会被"悄悄换端口"坑。
    #   AIGW_PANEL_PORT 环境变量仍可指定端口（同样严格，被占即退）。
    try:
        port = int(os.environ.get("AIGW_PANEL_PORT") or DEFAULT_PANEL_PORT)
    except ValueError:
        port = DEFAULT_PANEL_PORT
    bind_host = _bind_host()
    if not _bind_port_or_die(port, bind_host):
        sys.exit(1)
    srv = ThreadingHTTPServer((bind_host, port), Handler)
    APP["server"] = srv
    url = "http://%s:%d/" % (bind_host if bind_host != "0.0.0.0" else "127.0.0.1", port)
    APP["url"] = url
    _save_url(port)

    state = {"exit": False}

    def do_open():
        webbrowser.open(url)

    def do_exit():
        state["exit"] = True
        threading.Thread(target=_shutdown, args=(srv,), daemon=True).start()

    # 3) 托盘图标（★ 网关 EXE 已彻底移除，托盘不再有 启动/停止网关 项）
    tr = None
    if os.name == "nt":
        tr = traymod.TrayIcon(
            title="AI 资源整合网关面板",
            icon_path=_icon_path(),
            on_open=do_open, on_exit=do_exit)
        if tr.start():
            log("托盘图标已就绪（双击打开面板，右键菜单可退出）")
        else:
            tr = None
            log("托盘图标创建失败，退出请用任务管理器或 Ctrl+C")
    APP["tray"] = tr

    print("=" * 64)
    print("  AI 资源整合网关面板 v%s（纯面板模式，无 EXE 依赖）" % APP["version"])
    print("  面板地址:   %s" % url)
    print("  数据目录:   %s" % APP["data_dir"])
    if tr:
        print("  已最小化到系统托盘（任务栏右下角），双击图标打开面板")
        print("  退出：托盘图标右键 → 退出")
    else:
        print("  按 Ctrl+C 退出")
    print("=" * 64)

    if os.environ.get("AIGW_NO_BROWSER") != "1" and not tray_mode:
        threading.Timer(1.0, do_open).start()

    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\n正在退出…")
    finally:
        _shutdown(srv, quiet=True)
        guard.release()


def _tray_notify(title, msg):
    tr = APP.get("tray")
    if tr:
        try:
            tr.notify(title, msg, timeout=3500)
        except Exception:
            pass


def _shutdown(srv, quiet=False):
    try:
        APP["scheduler"].stop_flag.set()
    except Exception:
        pass
    try:
        APP.get("router") and APP["router"].stop()
    except Exception:
        pass
    tr = APP.get("tray")
    if tr:
        try:
            tr.stop()
        except Exception:
            pass
    try:
        srv.shutdown()
    except Exception:
        pass
    if not quiet:
        print("已退出")


def _bind_host():
    """监听地址：默认 127.0.0.1（只本机）；部署对外时设 AIGW_PANEL_HOST=0.0.0.0。"""
    return os.environ.get("AIGW_PANEL_HOST") or "127.0.0.1"


def _bind_port_or_die(port, host=None):
    """固定端口策略：端口被占就日志 + 弹窗后退出（不静默顺延，
    避免客户端写死的地址悄悄失联）。返回 True 表示可继续启动。"""
    if host is None:
        host = _bind_host()
    s = socket.socket()
    try:
        s.bind((host, port))
        return True
    except OSError:
        pass
    finally:
        s.close()
    msg = ("端口 %d 已被其它程序占用，面板无法启动。\n\n"
           "请结束占用该端口的程序（或在任务管理器里结束旧的 "
           "aigw-panel 进程）后重新启动。" % port)
    log("启动失败：%s" % msg.replace("\n\n", " "))
    if os.environ.get("AIGW_NO_BROWSER") != "1":   # 无头/自测模式不弹窗
        try:
            import ctypes
            ctypes.windll.user32.MessageBoxW(
                0, msg, "AI 资源网关面板", 0x10)   # MB_ICONERROR
        except Exception:
            pass
    return False


def _save_url(port):
    host = _bind_host()
    disp = "127.0.0.1" if host == "0.0.0.0" else host
    try:
        with open(os.path.join(APP["data_dir"], "panel.url"), "w",
                  encoding="utf-8") as f:
            f.write("http://%s:%d/" % (disp, port))
    except Exception:
        pass


def _last_url_path():
    try:
        p = os.path.join(APP.get("data_dir", "."), "panel.url")
        if os.path.exists(p):
            return open(p, encoding="utf-8").read().strip()
    except Exception:
        pass
    return None


if __name__ == "__main__":
    main()
