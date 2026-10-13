# -*- coding: utf-8 -*-
"""本地持久化：站点账号、签到记录、任务记录、通知配置"""

import json
import os
import threading
import time
from datetime import datetime, timezone, timedelta

CST = timezone(timedelta(hours=8))
_LOCK = threading.RLock()


def now_ts():
    return int(time.time())


def today_str():
    return datetime.now(CST).strftime("%Y-%m-%d")


def now_str():
    return datetime.now(CST).strftime("%Y-%m-%d %H:%M:%S")


class Store:
    def __init__(self, data_dir):
        self.dir = data_dir
        os.makedirs(self.dir, exist_ok=True)
        self.paths = {
            "sites": os.path.join(data_dir, "sites.json"),
            "checkins": os.path.join(data_dir, "checkins.json"),
            "tasks": os.path.join(data_dir, "tasks.json"),
            "settings": os.path.join(data_dir, "settings.json"),
            "notify": os.path.join(data_dir, "notify.json"),
            # 账号池：面板自己跑完登录后落库的凭据（7 网关 + 上游平台）
            "accounts": os.path.join(data_dir, "accounts.json"),
            # 线上模型倍率缓存（登录后从 CodeBuddy 目录接口拉的）
            "model_rates": os.path.join(data_dir, "model_rates.json"),
            # 用户在模型清单里勾选的模型（用于生成客户端配置）
            "selected_models": os.path.join(data_dir, "selected_models.json"),
            # APP 平台定时自动签到配置
            "autocheckin": os.path.join(data_dir, "autocheckin.json"),
            # 工具调用（function calling）的执行选项
            "toolcall": os.path.join(data_dir, "toolcall.json"),
            # ★ 2026-10-13：接入源页「拉取清单」结果需要跨重启保留。
            #   此前 platform_models 由 put() 动态注册路径，但 Store.__init__
            #   只从硬编码 paths 加载，导致重启后动态注册丢失、文件存在却读不到，
            #   用户看到的永远是内置/静态老数据。这里显式注册并给默认值。
            "platform_models": os.path.join(data_dir, "platform_models.json"),
            # 接入源抽屉「账号实时积分余额」缓存（account_id → {value, ts}，
            # 15 分钟 TTL）。与 platform_models 同理：必须注册进 paths，
            # 否则重启后读不到。
            "account_balances": os.path.join(data_dir, "account_balances.json"),
            # 面板侧用量事件（原生中继/路由调用，保留 90 天）
            "usage_events": os.path.join(data_dir, "usage_events.json"),
        }
        self.defaults = {
            "sites": [],
            "checkins": {},      # {"2026-10-03": [{"site":..., "result":...}]}
            "tasks": [],          # [{"id","name","site","kind","done","created","last_run"}]
            "settings": {
                "gateway_port": 8317,
                "gateway_addr": "127.0.0.1",
                "gateway_api_key": "admin",
                "gateway_admin_user": "admin",
                "gateway_admin_password": "",
                "auto_checkin": True,
                "checkin_hour": 9,
                "auto_growth": True,
                "growth_hour": 10,
                # 默认**不**自动拉起 workbuddy-gateway。
                # 用户明确要求「不要一次性启动两个软件」—— 面板不依赖它也能工作
                # （内置倍率表、其它源、工具调用、定时签到都不依赖网关进程）。
                # 网关只在你手动开、或路由里真的用到它时才需要。
                "auto_start_gateway": False,
                "theme": "light",
                "refresh_interval": 15,
                # ★ 面板访问令牌：留空=本机回环不校验；填写或对外监听(AIGW_PANEL_HOST
                #   非回环)时，/api/* 必须带 X-AIGW-Token（首页会自动下发 Cookie）
                "panel_token": "",
            },
            "notify": {
                "webhooks": [],     # [{type, url, enabled, events}]
                "history": [],      # [{ts, ok, target, message}]
                "notify_checkin": True,
                "notify_quota": True,
                "notify_error": True,
            },
            "accounts": [],
            "model_rates": {},
            "selected_models": [],
            # ★ 用户在模型总表里手动关掉的模型（空 = 全部启用）
            "disabled_models": [],
            # 接入源页各平台「拉取清单」的缓存（pid → {models, live, ts}）
            "platform_models": {},
            # 接入源抽屉各账号「实时积分余额」缓存（account_id → {value, ts}）
            "account_balances": {},
            "autocheckin": {},   # 实际默认值在 app/autocheckin.DEFAULT_CFG
            "toolcall": {},
            "usage_events": [],  # 面板侧用量事件（原生中继/路由调用，90 天剪枝）
        }
        self._cache = {}
        for k, p in self.paths.items():
            self._cache[k] = self._load(p, self.defaults[k])

    # ------------------------------------------------------------ 基础
    def _load(self, path, default):
        if not os.path.exists(path):
            return json.loads(json.dumps(default))
        try:
            with open(path, "r", encoding="utf-8") as f:
                data = json.load(f)
            if isinstance(default, dict):
                merged = json.loads(json.dumps(default))
                merged.update(data)
                return merged
            return data
        except Exception:
            # 文件损坏时备份后重建，不静默吞掉
            try:
                os.rename(path, path + ".corrupt-" + str(now_ts()))
            except OSError:
                pass
            return json.loads(json.dumps(default))

    def _flush(self, key):
        path = self.paths[key]
        tmp = path + ".tmp"
        with open(tmp, "w", encoding="utf-8", newline="\n") as f:
            json.dump(self._cache[key], f, ensure_ascii=False, indent=2)
        os.replace(tmp, path)

    def get(self, key):
        with _LOCK:
            # ★ 2026-10-07：新增的键（如 disabled_models）在**老用户的目录**里
            #   没有对应文件 —— _cache 里就没有这个键，直接 KeyError 会让
            #   整个面板起不来。兜底返回默认值深拷贝，并顺手把键补进缓存。
            if key not in self._cache:
                dft = self.defaults.get(key)
                if dft is None:
                    raise KeyError(key)
                self._cache[key] = json.loads(json.dumps(dft))
            return json.loads(json.dumps(self._cache[key]))

    def put(self, key, value):
        with _LOCK:
            self._cache[key] = value
            # ★ 2026-10-07：没注册进 paths 的新键（如 disabled_models）也
            #   要能落盘，否则重启就丢。落到 data/<key>.json。
            if key not in self.paths:
                self.paths[key] = os.path.join(self.dir, key + ".json")
            self._flush(key)
            return json.loads(json.dumps(value))

    def update(self, key, mutator, flush=True):
        """读-改-写（持锁）。flush=False 只改内存，由调用方限频落盘。"""
        with _LOCK:
            cur = json.loads(json.dumps(self._cache[key]))
            out = mutator(cur) or cur
            self._cache[key] = out
            if flush:
                self._flush(key)
            return json.loads(json.dumps(out))

    def flush(self, key):
        with _LOCK:
            return self._flush(key)

    # ------------------------------------------------------------ 站点
    def upsert_site(self, site):
        def mut(cur):
            found = False
            for i, s in enumerate(cur):
                if s.get("id") == site.get("id"):
                    cur[i] = site
                    found = True
                    break
            if not found:
                site.setdefault("created", now_str())
                cur.append(site)
            return cur
        return self.update("sites", mut)

    def delete_site(self, site_id):
        return self.update("sites", lambda cur: [s for s in cur if s.get("id") != site_id])

    # ------------------------------------------------------------ 签到
    def checkin_today(self):
        return self._cache["checkins"].get(today_str(), [])

    def add_checkin(self, entry):
        d = today_str()

        def mut(cur):
            day = cur.setdefault(d, [])
            day.append(entry)
            return cur
        self.update("checkins", mut)

    def checkin_history(self, site_id, days=30):
        out = []
        with _LOCK:
            for d, entries in sorted(self._cache["checkins"].items()):
                for e in entries:
                    if e.get("site") == site_id:
                        out.append({"date": d, **e})
        return out[-days:]

    def checkin_summary(self):
        """全站签到总览：今日 x/y、历史连续天数"""
        with _LOCK:
            hist = self._cache["checkins"]
        today = today_str()
        done_today = set()
        all_dates = {}
        for d, entries in hist.items():
            for e in entries:
                sid = e.get("site")
                if e.get("ok"):
                    all_dates.setdefault(sid, set()).add(d)
                    if d == today:
                        done_today.add(sid)
        sites = [s for s in self._cache["sites"] if s.get("checkin")]
        total = len(sites)
        done = len([s for s in sites if s.get("id") in done_today])
        # 计算每个站点的连续天数
        streaks = {}
        for sid, dates in all_dates.items():
            streak = 0
            cur = datetime.now(CST).date()
            if cur.isoformat() not in dates:
                cur = cur - timedelta(days=1)
            while cur.isoformat() in dates:
                streak += 1
                cur = cur - timedelta(days=1)
            streaks[sid] = streak
        return {
            "date": today,
            "total_checkin_sites": total,
            "done_today": done,
            "streaks": streaks,
            "best_streak": max(streaks.values()) if streaks else 0,
            "total_history": sum(len(v) for v in all_dates.values()),
        }

    # ------------------------------------------------------------ 任务
    def add_task(self, task):
        task.setdefault("id", "t%d%d" % (now_ts(), len(self._cache["tasks"])))
        task.setdefault("created", now_str())
        task.setdefault("done", False)
        task.setdefault("runs", 0)
        return self.update("tasks", lambda cur: cur + [task])

    def toggle_task(self, task_id):
        def mut(cur):
            for t in cur:
                if t.get("id") == task_id:
                    t["done"] = not t.get("done")
                    break
            return cur
        return self.update("tasks", mut)

    def run_task(self, task_id, ok, note=""):
        def mut(cur):
            for t in cur:
                if t.get("id") == task_id:
                    t["runs"] = t.get("runs", 0) + 1
                    t["last_run"] = now_str()
                    t["last_ok"] = ok
                    t["last_note"] = note
                    break
            return cur
        return self.update("tasks", mut)

    def delete_task(self, task_id):
        return self.update("tasks", lambda cur: [t for t in cur if t.get("id") != task_id])

    def task_summary(self):
        with _LOCK:
            tasks = json.loads(json.dumps(self._cache["tasks"]))
        return {
            "total": len(tasks),
            "done": len([t for t in tasks if t.get("done")]),
            "pending": len([t for t in tasks if not t.get("done")]),
            "tasks": tasks,
        }
