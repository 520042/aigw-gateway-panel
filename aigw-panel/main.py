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

    # 预置资源导航（首次启动写入，之后用户可自行增删）
    sites = store.get("sites")
    if not any(s.get("builtin") for s in sites):
        pass  # 不自动写入，避免污染用户数据；由 /api/catalog 提供

    if s.get("gateway_admin_password"):
        try:
            client.login()
        except Exception:
            pass

    sched = Scheduler(store, gw_path, log)
    APP["scheduler"] = sched
    if s.get("auto_start_gateway", True):
        sched.start()

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
    load_route_config(router, store, client)
    if s.get("router_auto_probe", True):
        router.start_auto_probe(int(s.get("router_probe_interval", 60)))

    log("面板启动完成，数据目录=%s，网关=%s" % (data_dir, gw_path or "未找到"))
    return APP


# ------------------------------------------------------------------ 路由配置
DEFAULT_AUTO_MODELS = [
    {"name": "auto-fast", "desc": "自动最快：优先实测延迟最低的上游",
     "strategy": "fastest"},
    {"name": "auto-weight", "desc": "自动权重：按你配的权重排序",
     "strategy": "weight"},
    {"name": "auto-priority", "desc": "自动优先级：严格按优先级列表，失败才降级",
     "strategy": "priority"},
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
        # 1) 本地网关作为一条常驻上游（如果用户勾选）
        if m.get("use_gateway", True):
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
            ))
        if targets:
            router.add_model(m["name"], targets)
    router.config = {m["name"]: m.get("strategy", "fastest") for m in models_cfg}
    log("路由表已加载：%d 个自动模型" % len(router.list_models()))


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
def api_overview():
    """总览：网关状态 + 签到总览 + 用量总计 + 站点健康"""
    c = new_client()
    store = APP["store"]
    ping = c.ping()
    out = {
        "version": APP["version"],
        "uptime": now_ts() - APP["started"],
        "gateway": {
            "path": APP["gateway_path"] or "",
            "addr": "%s:%s" % (c.addr, c.port),
            "alive": ping.get("alive"),
            "error": ping.get("error"),
            "models": ping.get("models", 0),
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
    if ping.get("alive"):
        u, _ = safe(lambda: c.usage())
        if u.get("ok") is not False:
            out["usage"] = u.get("totals")
        m, _ = safe(lambda: c.models())
        if m.get("ok") is not False:
            out["model_count"] = len(m.get("models") or [])
    return ok(out)


def api_usage(rng="all"):
    c = new_client()
    u, st = safe(lambda: c.usage(rng))
    if st != 200:
        return u, st
    s = safe_dict(lambda: c.usage_series("24h")) if rng in ("all", "24h") else None
    return ok({"usage": u, "series": s})


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
        sch = APP.get("scheduler")
        auto = getattr(sch, "auto", None) if sch else None
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
        # 手动触发不依赖后台线程 —— 面板刚启动的 45 秒静默观察期里也要能用。
        sch = APP.get("scheduler")
        auto = getattr(sch, "auto", None) if sch else None
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
    action=catalog 用账号池凭据在线拉取模型目录（含线上倍率）。
    """
    if action == "catalog":
        from app import gwextra
        b = body or {}
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
        from app import gwextra
        return ok({"sources": gwextra.mark_catalog_accounts(
            gwextra.catalog_sources(), APP.get("accounts"))})

    if action == "select":
        # 持久化用户勾选的模型（前端用来生成客户端配置）
        store = APP["store"]
        if body is not None and body.get("models") is not None:
            models = [str(x) for x in (body.get("models") or []) if str(x).strip()]
            store.put("selected_models", sorted(set(models))[:500])
            return ok({"saved": len(set(models))})
        saved = store.get("selected_models") or []
        return ok({"models": saved})

    c = new_client()
    m, st = safe(lambda: c.models())
    if st != 200:
        return m, st
    v1 = safe_dict(lambda: c.v1_models())

    admin = (m or {}).get("models") if isinstance(m, dict) else None
    return ok(_merge_model_rates(admin, m, v1))


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
    c = new_client()
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

    if action == "gateway_restart":
        okk, msg = APP["scheduler"].ensure_gateway(c)
        return ok({"alive": okk, "message": msg})

    if action == "gateway_stop":
        okk, msg = APP["scheduler"].stop_gateway()
        return ok({"stopped": okk, "message": msg})

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
        r = safe_dict(lambda: c.probe_models((body or {}).get("models")))
        return r

    gw = safe_dict(lambda: c.settings())
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
                                 b.get("account_id"))
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
    c = new_client()
    g = safe_dict(lambda: c.logs(200))
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
        model = body.get("model") or "auto-fast"
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
        model = body.get("model") or "auto-fast"
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
        self._send({"ok": False, "message": "not found"}, 404)

    def _agg_models(self):
        """聚合模型列表：自动路由模型 + 本地网关 + 已配置站点"""
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
        model 命中自动模型（auto-*）→ 走智能路由；
        否则透传到本地网关。
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
            if result.get("ok") is False and "choices" not in result:
                self._send(result, 503 if result.get("code") == 503 else 502)
                return
            result["_aigw_route"] = tracks
            self._send(result, 200)
            return

        # 透传：自动模型但网关未配置时兜底走网关
        c = new_client()
        url = "http://%s:%s/v1/chat/completions" % (c.addr, c.port)
        data = json.dumps(body).encode("utf-8")
        req = urllib.request.Request(url, data=data, method="POST", headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer " + c.api_key})
        try:
            with urllib.request.urlopen(req, timeout=180) as r:
                raw = r.read().decode("utf-8", "replace")
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw.encode("utf-8"))
        except urllib.error.HTTPError as e:
            self._send({"ok": False, "message": "上游返回 %s" % e.code,
                        "raw": e.read().decode("utf-8", "replace")[:800]}, 502)
        except Exception as e:
            self._send({"ok": False, "message": str(e)}, 502)


def free_port(start=8790):
    for p in range(start, start + 60):
        s = socket.socket()
        try:
            s.bind(("127.0.0.1", p))
            return p
        except OSError:
            continue
        finally:
            s.close()
    return start


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
    port = int(os.environ.get("AIGW_PANEL_PORT") or 0) or free_port()
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    APP["server"] = srv
    url = "http://127.0.0.1:%d/" % port
    APP["url"] = url
    _save_url(port)

    state = {"exit": False}

    def do_open():
        webbrowser.open(url)

    def do_start_gw():
        threading.Thread(target=_gw_start, daemon=True).start()

    def do_stop_gw():
        try:
            okk, msg = APP["scheduler"].stop_gateway()
            _tray_notify("网关", msg)
        except Exception as e:
            _tray_notify("网关", "停止失败：%s" % e)

    def do_exit():
        state["exit"] = True
        threading.Thread(target=_shutdown, args=(srv,), daemon=True).start()

    # 3) 托盘图标
    tr = None
    if os.name == "nt":
        tr = traymod.TrayIcon(
            title="AI 资源整合网关面板",
            icon_path=_icon_path(),
            on_open=do_open, on_start_gateway=do_start_gw,
            on_stop_gateway=do_stop_gw, on_exit=do_exit)
        if tr.start():
            log("托盘图标已就绪（双击打开面板，右键菜单可退出）")
        else:
            tr = None
            log("托盘图标创建失败，退出请用任务管理器或 Ctrl+C")
    APP["tray"] = tr

    print("=" * 64)
    print("  AI 资源整合网关面板 v%s" % APP["version"])
    print("  面板地址:   %s" % url)
    print("  数据目录:   %s" % APP["data_dir"])
    print("  网关程序:   %s" % (APP["gateway_path"] or "未找到（仅面板功能可用）"))
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


def _gw_start():
    c = new_client()
    okk, msg = APP["scheduler"].ensure_gateway(c)
    _tray_notify("网关", msg)


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


def _save_url(port):
    try:
        with open(os.path.join(APP["data_dir"], "panel.url"), "w",
                  encoding="utf-8") as f:
            f.write("http://127.0.0.1:%d/" % port)
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
