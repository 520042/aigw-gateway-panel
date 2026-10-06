# -*- coding: utf-8 -*-
"""
自动模型路由引擎
================
把 N 个上游（本地网关 / 公益中转站 / 官方平台）抽象成若干「虚拟模型」，
客户端只要填一个 base_url + 模型名，剩下的交给路由。

三种选路策略
------------
  fastest   谁的实测延迟最低用谁（默认）
  weight    按用户配的权重排序，权重高的优先
  priority  严格按优先级列表从上到下，失败才降级

核心机制
--------
  1. 延迟探测：并发发 HEAD/GET 探 `/models`，EWMA 平滑，多次采样
  2. 健康熔断：连续失败 N 次进 OPEN，冷却后半开试探
  3. 自动降级：主上游失败（429/5xx/超时）自动切下一个，不给用户感知
  4. 亲和保持：同一会话尽量粘在同一上游，避免上下文抖动

只用标准库。
"""

import json
import random
import ssl
import threading
import time
import urllib.error
import urllib.request

PROBE_TIMEOUT = 6
CHAT_TIMEOUT = 300


# ============================================================ 健康与延迟
class Target:
    """一个上游目标在某个模型下的运行时状态"""

    __slots__ = ("model", "upstream_id", "name", "endpoint", "api_key",
                 "protocol", "weight", "priority", "extra_paths", "timeout",
                 "latency_ms", "ewma", "ok_count", "fail_count", "consec_fail",
                 "state", "open_until", "last_error", "last_ok_ts",
                 "total_calls", "total_fail", "headers", "capabilities", "free")

    def __init__(self, model, upstream_id, name, endpoint, api_key="",
                 protocol="openai", weight=100, priority=0,
                 extra_paths=None, timeout=CHAT_TIMEOUT, headers=None,
                 capabilities=None, free=False):
        self.model = model
        self.upstream_id = upstream_id
        self.name = name
        self.endpoint = endpoint.rstrip("/")
        self.api_key = api_key
        self.protocol = protocol
        self.weight = weight
        self.priority = priority
        self.extra_paths = extra_paths or []
        self.timeout = timeout
        self.headers = headers or {}
        self.capabilities = capabilities or {}
        self.free = free

        self.latency_ms = None       # 最近一次实测
        self.ewma = None             # 平滑后的延迟
        self.ok_count = 0
        self.fail_count = 0
        self.consec_fail = 0
        self.state = "closed"        # closed / open / half-open
        self.open_until = 0.0
        self.last_error = ""
        self.last_ok_ts = 0.0
        self.total_calls = 0
        self.total_fail = 0

    def snapshot(self):
        return {
            "model": self.model, "upstream": self.upstream_id, "name": self.name,
            "endpoint": self.endpoint, "protocol": self.protocol,
            "weight": self.weight, "priority": self.priority,
            "capabilities": self.capabilities or None,
            "free": self.free,
            "latency_ms": self.latency_ms,
            "ewma_ms": round(self.ewma, 1) if self.ewma else None,
            "state": self.state, "ok": self.ok_count, "fail": self.fail_count,
            "total_calls": self.total_calls, "total_fail": self.total_fail,
            "success_rate": round(
                100.0 * self.ok_count / (self.ok_count + self.fail_count), 1)
            if (self.ok_count + self.fail_count) else None,
            "last_error": self.last_error[:160],
            "last_ok": round(self.last_ok_ts) if self.last_ok_ts else None,
            "cooldown_left": max(0, round(self.open_until - time.time()))
            if self.state == "open" else 0,
        }


class Router:
    def __init__(self, store=None, breaker_fail=3, breaker_cooldown=60,
                 ewma_alpha=0.3, log=None):
        self.store = store
        self.breaker_fail = breaker_fail
        self.breaker_cooldown = breaker_cooldown
        self.alpha = ewma_alpha
        self.log = log or (lambda *_: None)
        self.accounts = None       # 账号池引用（main 注入，内部中继目标用）
        self._lock = threading.RLock()
        self._models = {}          # {"auto-fast": {"targets":[Target,...]}}
        self._session = {}         # 会话亲和: session_id -> (upstream_id, ts)
        self._stop = threading.Event()
        self._probe_thread = None

    # --------------------------------------------------------- 配置
    def add_model(self, name, targets):
        """targets: [Target, ...]"""
        with self._lock:
            self._models[name] = {"targets": list(targets), "updated": time.time()}
        return len(targets)

    def remove_model(self, name):
        with self._lock:
            self._models.pop(name, None)

    def list_models(self):
        with self._lock:
            return {
                k: {
                    "targets": [t.snapshot() for t in v["targets"]],
                    "healthy": len([t for t in v["targets"] if t.state != "open"]),
                    "total": len(v["targets"]),
                    "updated": v["updated"],
                } for k, v in self._models.items()
            }

    def get_targets(self, model):
        with self._lock:
            m = self._models.get(model)
            return list(m["targets"]) if m else []

    # --------------------------------------------------------- 选路
    def pick(self, model, strategy="fastest", session_id=None,
             exclude=None, require_tools=False, require_vision=False):
        """返回 (target, 候选列表)"""
        cands = self.get_targets(model)
        if not cands:
            return None, []
        exclude = set(exclude or [])
        pool = [t for t in cands if t.upstream_id not in exclude]
        if not pool:
            pool = cands

        # 熔断过滤
        now = time.time()
        alive = [t for t in pool if not (t.state == "open" and now < t.open_until)]
        if not alive:
            # 全熔断 → 半开第一个
            for t in pool:
                if t.state == "open":
                    t.state = "half-open"
                    alive = [t]
                    break
        if not alive:
            alive = pool

        # 能力过滤
        if require_tools or require_vision:
            filtered = [t for t in alive if _cap_ok(t, require_tools, require_vision)]
            if filtered:
                alive = filtered

        if strategy == "priority":
            ordered = sorted(alive, key=lambda t: (t.priority, t.weight, -(t.ewma or 1e9)))
            # 会话亲和优先
            sid = self._affinity(session_id)
            if sid:
                for t in ordered:
                    if t.upstream_id == sid:
                        ordered.remove(t)
                        ordered.insert(0, t)
                        break
            return ordered[0], ordered

        if strategy == "weight":
            # 权重越高越优先；同权重按延迟
            ordered = sorted(alive, key=lambda t: (-t.weight, (t.ewma if t.ewma else 1e9)))
            return ordered[0], ordered

        if strategy == "free":
            # 免费优先：free 候选排前（组内按延迟），非 free 作兜底排后
            ordered = sorted(
                alive,
                key=lambda t: (0 if t.free else 1, t.ewma if t.ewma else 1e9))
            sid = self._affinity(session_id)
            if sid:
                for t in ordered:
                    if t.upstream_id == sid and len(ordered) > 1:
                        ordered.remove(t)
                        ordered.insert(0, t)
                        break
            return ordered[0], ordered

        # fastest：EWMA 延迟最低优先。**无延迟数据的排最后**
        # （不能排最前：否则刚配置、还没探测过的上游会抢跑）
        with_lat = [t for t in alive if t.ewma]
        without = [t for t in alive if not t.ewma]
        ordered = sorted(with_lat, key=lambda t: t.ewma) + without
        sid = self._affinity(session_id)
        if sid:
            for t in ordered:
                if t.upstream_id == sid and len(ordered) > 1:
                    ordered.remove(t)
                    ordered.insert(0, t)
                    break
        return ordered[0], ordered

    def bind_session(self, session_id, upstream_id):
        if not session_id:
            return
        now = time.time()
        with self._lock:
            # 防膨胀：超限时先清过期条目，仍超限就丢最早写入的
            # （长驻面板的内存保护；亲和超过 TTL 本来也已失效）
            if len(self._session) >= self._AFFINITY_MAX:
                cutoff = now - self._AFFINITY_TTL
                for k in [k for k, (_, ts) in self._session.items() if ts < cutoff]:
                    del self._session[k]
            while len(self._session) >= self._AFFINITY_MAX:
                self._session.pop(next(iter(self._session)))
            self._session[session_id] = (upstream_id, now)

    # 会话亲和的有效期与容量：超过后绑定视为不存在
    _AFFINITY_TTL = 3600
    _AFFINITY_MAX = 4096

    def _affinity(self, session_id):
        """读会话亲和（带 TTL）；返回 upstream_id 或 None"""
        if not session_id:
            return None
        ent = self._session.get(session_id)
        if not ent:
            return None
        uid, ts = ent
        if time.time() - ts > self._AFFINITY_TTL:
            return None
        return uid

    # --------------------------------------------------------- 健康
    def mark_ok(self, t, latency_ms=None):
        with self._lock:
            t.ok_count += 1
            t.consec_fail = 0
            t.fail_count = max(0, t.fail_count - 1)
            t.total_calls += 1
            t.state = "closed"
            t.last_ok_ts = time.time()
            if latency_ms is not None:
                t.latency_ms = round(latency_ms)
                t.ewma = latency_ms if t.ewma is None \
                    else (1 - self.alpha) * t.ewma + self.alpha * latency_ms

    def mark_fail(self, t, err, cooldown=None):
        with self._lock:
            t.fail_count += 1
            t.consec_fail += 1
            t.total_calls += 1
            t.total_fail += 1
            t.last_error = str(err)[:200]
            if t.consec_fail >= self.breaker_fail:
                t.state = "open"
                t.open_until = time.time() + (cooldown or self.breaker_cooldown)

    # --------------------------------------------------------- 探测
    def probe_target(self, t):
        """探 /models 测延迟；内部中继目标（native:）不发包，凭据可用即视为活"""
        if t.upstream_id.startswith("native:"):
            pid = t.upstream_id.split(":", 1)[1]
            if pid == "anon-zen":
                # 匿名车道：真实探活 GET /zen/v1/models。不可达/超时→判死，
                # 避免盲目标 alive 后被 router 首选却长时间挂起。
                try:
                    req = urllib.request.Request(
                        "https://opencode.ai/zen/v1/models",
                        headers={"User-Agent": "opencode/1.18.31",
                                 "Authorization": "Bearer public",
                                 "Accept": "application/json"})
                    with urllib.request.urlopen(
                            req, timeout=PROBE_TIMEOUT,
                            context=ssl.create_default_context()) as r:
                        r.read(256)
                    self.mark_ok(t, 1.0)
                    return True, 1, "匿名车道（免登录免费，常驻）"
                except Exception as e:
                    self.mark_fail(t, "zen 不可达: %s" % str(e)[:60],
                                   cooldown=20)
                    return False, None, "匿名车道不可达（网络/限流）"
            try:
                n = len(self.accounts.usable(pid)) if self.accounts else 0
            except Exception:
                n = 0
            if n:
                self.mark_ok(t, 1.0)
                return True, 1, "内部中继（可用凭据 %d）" % n
            self.mark_fail(t, "无可用水凭据", cooldown=20)
            return False, None, "内部中继无可用凭据（先去接入源登录豆包）"
        urls = [t.endpoint + "/models"] + \
               [t.endpoint + p for p in t.extra_paths]
        best = None
        last_err = ""
        for url in urls:
            req = urllib.request.Request(url, method="GET", headers=self._headers(t))
            t0 = time.time()
            try:
                opener = urllib.request.build_opener(
                    urllib.request.HTTPSHandler(context=ssl.create_default_context()))
                with opener.open(req, timeout=PROBE_TIMEOUT) as r:
                    r.read(2048)
                    dt = (time.time() - t0) * 1000
                    self.mark_ok(t, dt)
                    return True, round(dt), url
            except urllib.error.HTTPError as e:
                last_err = "HTTP %d" % e.code
                if e.code in (401, 403):
                    # 鉴权失败也算「活的」，只是凭据不对
                    dt = (time.time() - t0) * 1000
                    self.mark_ok(t, dt)
                    return True, round(dt), url + " (鉴权失败)"
            except Exception as e:
                last_err = str(e)[:80]
        self.mark_fail(t, last_err, cooldown=20)
        return False, None, last_err

    def probe_all(self, parallel=True):
        tgts = []
        with self._lock:
            for m in self._models.values():
                tgts.extend(m["targets"])
        # 同一 endpoint 只探一次，但要把结果**回填到所有引用该 endpoint 的 Target**
        groups = {}
        for t in tgts:
            groups.setdefault((t.upstream_id, t.endpoint), []).append(t)
        items = list(groups.items())
        results = []

        def work(key_t):
            (uid, ep), members = key_t
            probe_t = members[0]
            try:
                ok, dt, info = self.probe_target(probe_t)
            except Exception as e:
                for t in members:
                    results.append({"name": t.name, "endpoint": t.endpoint,
                                    "ok": False, "latency_ms": None,
                                    "info": "探测异常 %s" % str(e)[:100]})
                return
            for t in members:
                # 同步探测结果（mark_ok 已在 probe_t 上做过，这里只复制指标）
                t.latency_ms = probe_t.latency_ms
                t.ewma = probe_t.ewma
                t.state = probe_t.state
                t.last_error = probe_t.last_error
                t.consec_fail = probe_t.consec_fail
                results.append({"name": t.name, "endpoint": t.endpoint,
                                "ok": ok, "latency_ms": dt, "info": str(info)[:120]})
        if parallel:
            ths = [threading.Thread(target=work, args=(it,), daemon=True) for it in items]
            for th in ths:
                th.start()
            for th in ths:
                th.join(timeout=PROBE_TIMEOUT + 6)
            alive_n = len([th for th in ths if th.is_alive()])
            if alive_n:
                self.log("探测线程超时 %d 个" % alive_n)
        else:
            for it in items:
                work(it)
        return results

    def start_auto_probe(self, interval=60):
        if self._probe_thread:
            return
        def loop():
            while not self._stop.is_set():
                try:
                    self.probe_all()
                except Exception as e:
                    self.log("探测循环异常 %s" % e)
                self._stop.wait(interval)
        self._probe_thread = threading.Thread(target=loop, daemon=True,
                                             name="router-probe")
        self._probe_thread.start()
        self.log("路由延迟探测已启动，间隔 %ss" % interval)

    def stop(self):
        self._stop.set()

    # --------------------------------------------------------- 调用
    def _headers(self, t, stream=False):
        h = {"Content-Type": "application/json", "Accept": "application/json"}
        if t.api_key:
            h["Authorization"] = "Bearer " + t.api_key
        h.update(t.headers)
        return h

    def chat(self, model, payload, strategy="fastest", session_id=None,
             max_failover=4, on_route=None):
        """
        带自动降级的对话调用。
        返回 (结果dict, 路由轨迹list)
        """
        body = dict(payload)
        need_tools = bool(body.get("tools"))
        reqs = body.get("messages") or []
        has_img = any(_has_image(m) for m in reqs)

        tried = []
        t0_all = time.time()
        for attempt in range(max_failover + 1):
            target, ordered = self.pick(
                model, strategy=strategy, session_id=session_id,
                exclude=[x[0] for x in tried],
                require_tools=need_tools, require_vision=has_img)
            if target is None:
                break

            body["model"] = target.model

            # ★ 内部中继目标（native:平台id）：进程内直调原生中继，不出站
            if target.upstream_id.startswith("native:"):
                t0 = time.time()
                res = self._native_chat(target, body)
                dt = (time.time() - t0) * 1000
                if res.get("ok") is not False:
                    self.mark_ok(target, dt)
                    self.bind_session(session_id, target.upstream_id)
                    if on_route:
                        on_route(target, dt, True, "")
                    tried.append((target.upstream_id, dt))
                    res["_route"] = {
                        "upstream": target.name, "endpoint": target.endpoint,
                        "upstream_id": target.upstream_id,
                        "latency_ms": round(dt), "attempt": attempt + 1,
                        "failover": attempt, "candidates": len(ordered)}
                    return res, tried
                self.mark_fail(target, str(res.get("error") or "内部中继失败"))
                tried.append((target.upstream_id, None))
                if on_route:
                    on_route(target, dt, False,
                             str(res.get("error") or "内部中继失败")[:100])
                continue

            url = target.endpoint + "/chat/completions"
            data = json.dumps(body).encode("utf-8")
            req = urllib.request.Request(url, data=data, method="POST",
                                         headers=self._headers(target))
            t0 = time.time()
            try:
                opener = urllib.request.build_opener(
                    urllib.request.HTTPSHandler(context=ssl.create_default_context()))
                with opener.open(req, timeout=target.timeout) as r:
                    raw = r.read().decode("utf-8", "replace")
                dt = (time.time() - t0) * 1000
                self.mark_ok(target, dt)
                self.bind_session(session_id, target.upstream_id)
                result = json.loads(raw)
                if on_route:
                    on_route(target, dt, True, "")
                tried.append((target.upstream_id, dt))
                result["_route"] = {
                    "upstream": target.name, "endpoint": target.endpoint,
                    "upstream_id": target.upstream_id,
                    "latency_ms": round(dt), "attempt": attempt + 1,
                    "failover": attempt, "candidates": len(ordered),
                }
                return result, tried
            except urllib.error.HTTPError as e:
                body_txt = e.read().decode("utf-8", "replace")[:300]
                code = e.code
                # 4xx（除 429）通常是请求本身问题，不降级
                if 400 <= code < 500 and code != 429:
                    self.mark_fail(target, "HTTP %d" % code, cooldown=15)
                    err = {"ok": False, "code": code,
                           "message": "上游拒绝请求：%s" % _errmsg(body_txt),
                           "_route": {"upstream": target.name, "endpoint": target.endpoint,
                                      "attempt": attempt + 1}}
                    if on_route:
                        on_route(target, (time.time() - t0) * 1000, False, "HTTP %d" % code)
                    return err, tried
                self.mark_fail(target, "HTTP %d %s" % (code, body_txt))
                tried.append((target.upstream_id, None))
                if on_route:
                    on_route(target, (time.time() - t0) * 1000, False, "HTTP %d" % code)
            except Exception as e:
                self.mark_fail(target, e)
                tried.append((target.upstream_id, None))
                if on_route:
                    on_route(target, (time.time() - t0) * 1000, False, str(e)[:100])

        return {"ok": False, "code": 503,
                "message": "所有上游均不可用（已尝试 %d 次）" % len(tried),
                "_elapsed_ms": round((time.time() - t0_all) * 1000),
                "_tried": tried}, tried

    def _native_relay_chat(self, pid, target, body):
        """通用原生中继（RELAY_SPECS 静态规格直连）——同模型跨源聚合的执行层。

        fmt=openai 走 NR.relay_once 透传；web_relay 走 web_relays.chat
        （私有协议 → OpenAI 形状）。换号/冷却由 acct_pool.try_accounts 负责，
        401/403 硬冷却、429 软冷却，与豆包路径同一套池。
        """
        from app import native_relay as NR, acct_pool
        spec = NR.relay_of(pid)
        if not spec:
            return {"ok": False, "error": "平台 %s 没有原生中继规格" % pid}
        msgs = body.get("messages") or []
        base_override = None
        if spec.get("base") == "@lobster" and self.store:
            base_override = ((self.store.get("settings") or {})
                             .get("lobster_server") or "")
        if spec.get("web_relay"):
            from app import web_relays as WR

            def _call(secret, name):
                okk, p = WR.chat(pid, msgs, secret, model=target.model)
                return okk, _relay_cooldown(p, okk)
        else:
            def _call(secret, name, row=None):
                okk, p = NR.relay_once(pid, body, secret, timeout=target.timeout,
                                       base_override=base_override, account=row)
                return okk, _relay_cooldown(p, okk)
        if spec.get("secret_fixed"):
            # 匿名车道（anon-zen 等）：无账号池概念，公共凭据直接打
            okk, payload = NR.relay_once(pid, body, "", timeout=target.timeout,
                                         base_override=base_override)
            if okk and isinstance(payload, dict):
                payload.setdefault("model", target.model)
                payload["_native_account"] = spec["secret_fixed"]
                return payload
            return {"ok": False, "error": str(payload)[:300]}
        okk, payload, acct = acct_pool.try_accounts(self.accounts, pid, _call)
        if okk and isinstance(payload, dict):
            payload.setdefault("model", target.model)
            payload["_native_account"] = acct
            return payload
        return {"ok": False, "error": str(payload)[:300]}

    def _native_chat(self, target, body):
        """内部中继调用（upstream_id = native:apk-doubao），进程内直调不出站。

        返回 OpenAI 形状 dict（含 choices/usage）或 {"ok": False, "error": …}。
        换号/冷却由 acct_pool.try_accounts 负责，与 /v1 直连路径同一套池。
        """
        pid = target.upstream_id.split(":", 1)[1]
        if pid == "anon-zen":
            from app import native_relay as NR
            okk, payload = NR.relay_once("anon-zen", body, "public", timeout=25)
            if okk:
                if isinstance(payload, dict):
                    payload.setdefault("model", target.model)
                    payload["_native_account"] = "anon-zen(public)"
                return payload
            return {"ok": False, "error": str(payload)[:300]}
        if pid != "apk-doubao":
            return self._native_relay_chat(pid, target, body)
        from app import doubao_relay as DR, acct_pool
        msgs = body.get("messages") or []

        def _call(secret, name, row=None):
            okk, p = DR.chat(msgs, secret, model=target.model,
                             timeout=target.timeout)
            if not okk and isinstance(p, dict):
                st = p.get("status") or 0
                if st in (401, 403):
                    p = {"message": p, "cooldown": "hard"}
                elif st == 429:
                    p = {"message": p, "cooldown": "soft"}
            return okk, p

        okk, payload, acct = acct_pool.try_accounts(self.accounts, "apk-doubao", _call)
        if okk:
            if isinstance(payload, dict):
                payload.setdefault("model", target.model)
                payload["_native_account"] = acct
            return payload
        return {"ok": False, "error": str(payload)[:300]}


# ------------------------------------------------------------------ 工具
def _has_image(msg):
    c = msg.get("content")
    if isinstance(c, list):
        for p in c:
            if isinstance(p, dict) and p.get("type") in ("image_url", "image"):
                return True
    return False


def _cap_ok(t, need_tools, need_vision):
    cap = getattr(t, "capabilities", None) or {}
    if not cap:
        return True
    if need_tools and cap.get("tools") is False:
        return False
    if need_vision and cap.get("vision") is False:
        return False
    return True


def _relay_cooldown(payload, ok=True):
    """把中继错误串里的状态码映射成账号池冷却档位（fn 约定：
    失败 payload 为 dict 且带 "cooldown": "soft"/"hard"）。"""
    if ok or not isinstance(payload, str):
        return payload
    import re
    m = re.search(r"(?:上游 |HTTP )(\d{3})[:,]", payload)
    if m:
        st = int(m.group(1))
        if st in (401, 403):
            return {"message": payload, "cooldown": "hard"}
        if st == 429:
            return {"message": payload, "cooldown": "soft"}
    return payload


def _errmsg(txt):
    try:
        d = json.loads(txt)
        e = d.get("error")
        if isinstance(e, dict):
            return e.get("message") or json.dumps(e, ensure_ascii=False)[:120]
        if isinstance(e, str):
            return e
        return d.get("message") or txt[:120]
    except Exception:
        return txt[:120]
