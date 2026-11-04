# -*- coding: utf-8 -*-
"""
APP 平台自动签到
================
原有的 Scheduler.run_all_checkins 只做两件事：
  1) 本地网关账号池（wb-gateway）
  2) 外部 NewAPI/OneAPI 中转站（sites 里有 checkin 的）

**APP 平台（Trae / CodeBuddy / 小浣熊 / 千问办公 / 库库…）完全没进这个循环**，
只能手动点「立即签到」。这里补上：

  - 按平台独立开关 + 独立时间（默认错峰，避免同一分钟打满）
  - 失败自动重试（可配次数与间隔）
  - 每次执行落库，可查历史
  - 严格幂等：同一天同一平台只跑一次（除非 force）
  - 无凭据 / 平台未开放签到 的明确区分，不算失败

配置落在 store 的 `autocheckin` 键：
{
  "enabled": true,
  "default_time": "09:10",
  "stagger_sec": 45,          # 平台之间的错峰间隔
  "retry_times": 2,
  "retry_delay_min": 10,
  "notify": true,
  "platforms": {
     "apk-trae":      {"on": true, "time": "09:05"},
     "apk-codebuddy": {"on": true, "time": "09:10"},
     ...
  }
}
"""

import threading
import time
from datetime import datetime, timedelta

from .store import now_str, now_ts
from . import gwextra

CST = timedelta(hours=8)

# 平台 → (显示名, 签到方式)
#   "flow"   = 需要先查状态再领（Trae）
#   "direct" = 单接口直接领
#   "bonus"  = 登录即送积分型（小浣熊）
#   "none"   = 公开接口里没有签到端点，只能靠 probe 发现
PLATFORM_CHECKIN = {
    # 2026-10-08 整理：只保留真正有签到功能的平台
    "wb-gateway": {"name": "WorkBuddy 国内", "mode": "direct", "hour": 9, "minute": 5},
    "wb-gateway-intl": {"name": "WorkBuddy 国际", "mode": "direct", "hour": 9, "minute": 10},
    "apk-trae": {"name": "Trae", "mode": "flow", "hour": 9, "minute": 15},
    "apk-raccoon": {"name": "小浣熊", "mode": "bonus", "hour": 9, "minute": 20},
    "apk-kuku": {"name": "百度文库", "mode": "kuku", "hour": 9, "minute": 25},
    "apk-wps": {"name": "WPS AI", "mode": "wps", "hour": 9, "minute": 30},
}

DEFAULT_CFG = {
    "enabled": True,
    "default_time": "09:10",
    "stagger_sec": 45,
    "retry_times": 2,
    "retry_delay_min": 10,
    "notify": True,
    "platforms": {},
}


def load_cfg(store):
    cfg = dict(DEFAULT_CFG)
    try:
        saved = store.get("autocheckin") or {}
        if isinstance(saved, dict):
            cfg.update(saved)
    except Exception:
        pass
    if not isinstance(cfg.get("platforms"), dict):
        cfg["platforms"] = {}
    return cfg


def save_cfg(store, cfg):
    store.put("autocheckin", cfg)
    return cfg


def platform_list(store):
    """给前端用：每个平台的开关与时间（缺省用默认值）"""
    cfg = load_cfg(store)
    out = []
    for pid, meta in PLATFORM_CHECKIN.items():
        p = (cfg.get("platforms") or {}).get(pid) or {}
        out.append({
            "platform": pid,
            "name": meta["name"],
            "mode": meta["mode"],
            "has_public_checkin": meta["mode"] != "none",
            "on": bool(p.get("on", True)),
            "time": p.get("time") or "%02d:%02d" % (meta["hour"], meta["minute"]),
            "hint": {
                "flow": "先查状态再领（Trae）",
                "direct": "单接口直接领（WorkBuddy）",
                "bonus": "登录即送积分（小浣熊）",
                "kuku": "两段式：先取 bdstoken 会话参数再领积分",
                "wps": "先查任务状态，未签则领（已签幂等）",
                "none": "公开接口里没有签到端点，需先用「探测端点」扫出来",
            }[meta["mode"]],
        })
    return out


def _hm(s, dft=(9, 10)):
    try:
        a, b = str(s).split(":")
        return int(a), int(b)
    except Exception:
        return dft


def due_list(cfg, dt=None):
    """当前时刻应该跑的平台列表"""
    dt = dt or (datetime.utcnow() + CST)
    now_min = dt.hour * 60 + dt.minute
    out = []
    for pid, meta in PLATFORM_CHECKIN.items():
        p = (cfg.get("platforms") or {}).get(pid) or {}
        if not p.get("on", True):
            continue
        h, m = _hm(p.get("time") or "%02d:%02d" % (meta["hour"], meta["minute"]))
        if now_min >= h * 60 + m:
            out.append((pid, meta))
    out.sort(key=lambda x: _hm(
        ((cfg.get("platforms") or {}).get(x[0]) or {}).get("time")
        or "%02d:%02d" % (x[1]["hour"], x[1]["minute"])))
    return out


def run_platform(accounts, platform, mode, log=None, options=None):
    """
    跑一个平台的签到。
    返回 (ok, 说明, 是否跳过)
    """
    def _log(m):
        if log:
            try:
                log(m)
            except Exception:
                pass

    try:
        usable = accounts.usable(platform) if accounts else []
    except Exception:
        usable = []
    if not usable:
        return False, "未登录（账号池里没有 %s 的凭据）" % platform, True

    try:
        if mode == "flow":
            okk, data = gwextra.trae_checkin_flow(accounts)
        elif mode == "bonus":
            okk, data = gwextra.raccoon_login_bonus(accounts)
        elif mode == "kuku":
            okk, data = gwextra.kuku_checkin_flow(accounts)
        elif mode == "wps":
            okk, data = gwextra.wps_checkin_flow(accounts)
        elif mode == "direct":
            act = gwextra.action_spec(platform, "checkin")
            if not act:
                return False, "该平台没有 checkin 动作", True
            okk, data = gwextra.call(accounts, platform, "checkin", {})
        else:
            return False, "公开接口里没有签到端点（需先「探测端点」）", True
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, str(e)[:120]), False

    if okk:
        msg = _brief(data)
        _log("  %s 签到成功：%s" % (platform, msg))
        return True, msg, False
    msg = _brief(data)
    if _looks_like_auth_error(msg):
        return False, msg, True
    return False, msg, False


def _brief(data):
    if isinstance(data, dict):
        for k in ("summary", "message", "msg", "desc", "error", "data", "already"):
            if k in data and data[k] not in (None, ""):
                v = data[k]
                if isinstance(v, dict):
                    for kk in ("message", "already", "checked_in", "checked_in_today"):
                        if kk in v:
                            return str(v[kk])
                    return str(list(v.keys())[:6])
                return str(v)[:160]
    return str(data)[:160]


def _looks_like_auth_error(msg):
    m = str(msg).lower()
    return any(k in m for k in (
        "401", "403", "未登录", "登录", "凭据", "cookie", "token", "账号",
        "unauthorized", "forbidden", "缺少", "no_credential", "无效",
        "authenticate", "接口拒绝", "未认证", "1001",
    ))


class AutoCheckin(threading.Thread):
    """后台线程：按配置时间自动跑各平台签到"""

    daemon = True

    def __init__(self, store, accounts_getter, log, notifier=None):
        super().__init__(name="aigw-autocheckin")
        self.store = store
        self._accounts_getter = accounts_getter
        self.log = log
        self.notifier = notifier
        self.stop_flag = threading.Event()
        self.done_today = set()
        self.running = False
        self.last_result = None

    def _log(self, m):
        try:
            self.log(m)
        except Exception:
            pass

    def mark_done(self, platform, date=None):
        d = date or (datetime.utcnow() + CST).strftime("%Y-%m-%d")
        self.done_today.add((d, platform))
        for k in list(self.done_today):
            if k[0] < (datetime.utcnow() + CST - timedelta(days=3)).strftime("%Y-%m-%d"):
                self.done_today.discard(k)

    def is_done(self, platform, date=None):
        d = date or (datetime.utcnow() + CST).strftime("%Y-%m-%d")
        return (d, platform) in self.done_today

    def run_round(self, force=False, only=None, options=None):
        if self.running:
            return {"ok": False, "message": "上一轮签到还在跑"}
        self.running = True
        try:
            cfg = load_cfg(self.store)
            accounts = self._accounts_getter()
            todo = due_list(cfg) if only is None else [
                (p, PLATFORM_CHECKIN[p]) for p in only
                if p in PLATFORM_CHECKIN]
            if force:
                todo = [(p, PLATFORM_CHECKIN[p]) for p in PLATFORM_CHECKIN]
            if not todo:
                return {"ok": True, "message": "没有到点的平台", "results": []}

            rt = int(cfg.get("retry_times", 2) or 0)
            delay = float(cfg.get("retry_delay_min", 10) or 0) * 60
            stagger = float(cfg.get("stagger_sec", 0) or 0)
            results = []
            for i, (pid, meta) in enumerate(todo):
                if not force and self.is_done(pid):
                    results.append({"platform": pid, "name": meta["name"],
                                    "ok": True, "skipped": True,
                                    "message": "今天已执行过"})
                    continue
                self._log("自动签到：%s" % meta["name"])
                okk, msg, skipped = run_platform(
                    accounts, pid, meta["mode"], log=self._log,
                    options=options)
                tries = 0
                while (not okk and not skipped and rt > 0
                       and tries < rt and delay > 0):
                    tries += 1
                    self._log("  %s 第 %d 次重试（等 %.0f 秒）"
                              % (meta["name"], tries, delay))
                    self.stop_flag.wait(delay)
                    if self.stop_flag.is_set():
                        break
                    okk, msg, skipped = run_platform(
                        accounts, pid, meta["mode"], log=self._log,
                        options=options)
                if okk or skipped:
                    self.mark_done(pid)
                results.append({
                    "platform": pid, "name": meta["name"], "ok": bool(okk),
                    "skipped": bool(skipped), "message": msg,
                    "retries": tries, "at": now_str(),
                })
                self._record(results[-1])
                if stagger and i < len(todo) - 1:
                    self.stop_flag.wait(stagger)

            ok_n = len([r for r in results if r.get("ok")])
            sk_n = len([r for r in results if r.get("skipped")])
            fail_n = len([r for r in results
                          if not r.get("ok") and not r.get("skipped")])
            summary = "APP 自动签到：成功 %d / 跳过 %d / 失败 %d" % (
                ok_n, sk_n, fail_n)
            self.last_result = {"ok": fail_n == 0, "summary": summary,
                                "results": results, "at": now_str()}
            self._log(summary)
            if cfg.get("notify", True) and self.notifier:
                try:
                    detail = "\n".join(
                        "· %s：%s%s" % (r["name"], r["message"],
                                        "（重试 %d 次）" % r["retries"]
                                        if r.get("retries") else "")
                        for r in results)
                    self.notifier.notify("checkin", "APP 自动签到", summary + "\n" + detail)
                except Exception:
                    pass
            return self.last_result
        finally:
            self.running = False

    def _record(self, item):
        try:
            self.store.add_checkin({
                "site": "auto:" + item["platform"],
                "name": "自动签到 · " + item["name"],
                "ok": bool(item.get("ok")) or bool(item.get("skipped")),
                "message": item.get("message", ""),
                "at": item.get("at") or now_str(),
            })
        except Exception:
            pass

    def run(self):
        self._log("APP 自动签到线程已启动")
        self.stop_flag.wait(50)
        while not self.stop_flag.is_set():
            try:
                cfg = load_cfg(self.store)
                if cfg.get("enabled", True):
                    self.run_round()
            except Exception as e:
                self._log("自动签到循环异常：%s" % e)
            self.stop_flag.wait(60)
        self._log("APP 自动签到线程已退出")

    def stop(self):
        self.stop_flag.set()


def status(store, accounts=None):
    """给前端的状态卡"""
    cfg = load_cfg(store)
    dt = datetime.utcnow() + CST
    today = dt.strftime("%Y-%m-%d")
    pls = platform_list(store)
    for p in pls:
        p["logged_in"] = False
        try:
            p["logged_in"] = bool(accounts and accounts.usable(p["platform"]))
        except Exception:
            pass
        p["done_today"] = (today, p["platform"]) in _DONE_CACHE
    return {
        "enabled": bool(cfg.get("enabled", True)),
        "default_time": cfg.get("default_time", "09:10"),
        "stagger_sec": cfg.get("stagger_sec", 45),
        "retry_times": cfg.get("retry_times", 2),
        "retry_delay_min": cfg.get("retry_delay_min", 10),
        "notify": bool(cfg.get("notify", True)),
        "platforms": pls,
        "now": dt.strftime("%Y-%m-%d %H:%M:%S"),
    }


_DONE_CACHE = set()


def register_done(platform, date=None):
    """给主进程/手动执行时回填「今天已跑过」"""
    d = date or (datetime.utcnow() + CST).strftime("%Y-%m-%d")
    _DONE_CACHE.add((d, platform))