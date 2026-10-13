# -*- coding: utf-8 -*-
"""每日免费模型榜单（数据源：ClawLabsAI/free-ai-models，每天 04:00 UTC 自动重建）。

拉取仓库的 data/models.json（机器可读、带排名 / 是否训练 / 提供商 / base_url），
过滤出对面板有用的聊天类免费模型，缓存到 data/free_models_feed.json；并提供
非阻塞读取与后台每日刷新线程。面板「路由与模型」页展示该榜单，让用户随时看到
当前最优、且明确标注「是否会被用于训练」的免费模型。

★ 仅做聚合与展示，不替用户决策；模型 id 可直接用于对应提供商的 OpenAI SDK。
"""
import os
import time
import json
import threading
import traceback
import urllib.request

FEED_URL = ("https://raw.githubusercontent.com/ClawLabsAI/free-ai-models/"
            "main/data/models.json")
CACHE_FILE = "free_models_feed.json"
_TTL = 3600 * 20          # 20h：源每日只更新一次
_FAIL_TTL = 3600 * 3       # 失败负缓存
_lock = threading.Lock()
_cache = {"ts": 0.0, "ok": False, "data": None, "fail_ts": 0.0, "err": None}
_DATA_DIR = None          # 由 main.py 在启动时 set_data_dir() 注入，避免循环导入


def set_data_dir(d):
    global _DATA_DIR
    _DATA_DIR = d


def _data_dir():
    if _DATA_DIR:
        return _DATA_DIR
    return os.path.join(os.getcwd(), "data")


def _opener():
    try:
        from app import native_relay as NR
        return NR._opener()
    except Exception:
        return urllib.request.build_opener()


def fetch_free_feed(force=False):
    """拉取并解析榜单；返回 payload dict 或 None（失败）。"""
    now = time.time()
    if not force:
        with _lock:
            if _cache["ok"] and _cache["data"] and now - _cache["ts"] < _TTL:
                return _cache["data"]
            if (not _cache["ok"]) and _cache["fail_ts"] and \
                    now - _cache["fail_ts"] < _FAIL_TTL:
                return _cache["data"]
    try:
        req = urllib.request.Request(
            FEED_URL,
            headers={"User-Agent": "aigw-panel/1.0",
                     "Accept": "application/json"})
        with _opener().open(req, timeout=30) as r:
            d = json.loads(r.read().decode("utf-8", "replace"))
        out = []
        for m in (d.get("models") or []):
            if m.get("kind") not in (None, "chat"):
                continue
            mid = m.get("id")
            if not mid:
                continue
            out.append({
                "rank": m.get("rank"),
                "id": mid,
                "name": m.get("name") or mid,
                "provider": m.get("provider"),
                "data_use": m.get("data_use") or "unknown",
                "served_by": m.get("served_by") or [],
                "context_window": m.get("context_window"),
                "supports_tools": bool(m.get("supports_tools")),
                "rate_limit": m.get("rate_limit") or "",
                "health": m.get("health"),
                "expires": m.get("expires"),
                "source": m.get("source") or "",
            })
        out.sort(key=lambda x: (x["rank"] is None,
                                x["rank"] if x["rank"] is not None else 999))
        payload = {"updated_at": d.get("updated_at"),
                   "total": len(out), "models": out,
                   "fetched_at": now}
        try:
            fp = os.path.join(_data_dir(), CACHE_FILE)
            with open(fp, "w", encoding="utf-8") as f:
                json.dump(payload, f, ensure_ascii=False)
        except Exception:
            pass
        with _lock:
            _cache.update(ts=now, ok=True, data=payload, fail_ts=0.0, err=None)
        return payload
    except Exception as e:
        err = "%s: %s" % (type(e).__name__, e)
        with _lock:
            _cache.update(ts=now, ok=False, fail_ts=now, err=err)
        return None


def get_free_feed():
    """非阻塞读取：先回内存缓存，失败再回磁盘文件。"""
    with _lock:
        if _cache["ok"] and _cache["data"]:
            return _cache["data"]
    try:
        fp = os.path.join(_data_dir(), CACHE_FILE)
        if os.path.exists(fp):
            with open(fp, encoding="utf-8") as f:
                d = json.load(f)
            with _lock:
                _cache.update(ts=d.get("fetched_at", time.time()),
                              ok=True, data=d, fail_ts=0.0)
            return d
    except Exception:
        pass
    return None


def start_daily_refresh(interval_hours=24, first_delay=90):
    """后台每日刷新；首刷延迟 first_delay 秒，避免启动即抢占网络。"""
    def _loop():
        time.sleep(first_delay)
        while True:
            try:
                fetch_free_feed(force=True)
            except Exception:
                traceback.print_exc()
            time.sleep(interval_hours * 3600)
    t = threading.Thread(name="aigw-free-feed", target=_loop, daemon=True)
    t.start()
    return t
