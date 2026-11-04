# -*- coding: utf-8 -*-
"""通知：企业微信 / 钉钉 / 飞书 / PushPlus / Server酱 / 自定义 Webhook"""

import json
import ssl
import urllib.error
import urllib.parse
import urllib.request

TIMEOUT = 12

WEBHOOK_TYPES = [
    {"type": "wecom",      "label": "企业微信机器人", "url_hint": "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=..."},
    {"type": "dingtalk",   "label": "钉钉机器人",     "url_hint": "https://oapi.dingtalk.com/robot/send?access_token=..."},
    {"type": "feishu",     "label": "飞书机器人",     "url_hint": "https://open.feishu.cn/open-apis/bot/v2/hook/..."},
    {"type": "pushplus",   "label": "PushPlus",       "url_hint": "https://www.pushplus.plus/send"},
    {"type": "sctp",       "label": "Server酱",       "url_hint": "https://sctapi.ftqq.com/<key>.send"},
    {"type": "bark",       "label": "Bark (iOS)",     "url_hint": "https://api.day.app/<key>"},
    {"type": "custom",     "label": "自定义 POST",    "url_hint": "任意 http(s) 地址"},
]


def _post(url, payload, headers=None, method="POST"):
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, data=json.dumps(payload).encode("utf-8"),
                                 headers=hdrs, method=method)
    opener = urllib.request.build_opener(
        urllib.request.HTTPSHandler(context=ssl.create_default_context()))
    try:
        with opener.open(req, timeout=TIMEOUT) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except urllib.error.URLError as e:
        return 0, str(e.reason)


def send_one(hook, title, body, fmt="text"):
    t = hook.get("type")
    url = (hook.get("url") or "").strip()
    if not url:
        return False, "缺少 webhook 地址"
    try:
        if t == "wecom":
            payload = {"msgtype": "text", "text": {"content": "%s\n%s" % (title, body)}}
            code, raw = _post(url, payload)
        elif t == "dingtalk":
            payload = {"msgtype": "text", "text": {"content": "%s\n%s" % (title, body)}}
            code, raw = _post(url, payload)
        elif t == "feishu":
            payload = {"msg_type": "text", "content": {"text": "%s\n%s" % (title, body)}}
            code, raw = _post(url, payload)
        elif t == "pushplus":
            payload = {"title": title, "content": body, "template": "txt"}
            code, raw = _post(url, payload)
        elif t == "sctp":
            payload = {"title": title, "desp": body}
            code, raw = _post(url, payload)
        elif t == "bark":
            payload = {"title": title, "body": body}
            code, raw = _post(url, payload)
        else:  # custom
            payload = {"title": title, "content": body, "msg": "%s\n%s" % (title, body)}
            code, raw = _post(url, payload)
        ok = 200 <= code < 300
        return ok, ("HTTP %d %s" % (code, raw[:180])) if not ok else "发送成功"
    except Exception as e:
        return False, str(e)


class Notifier:
    def __init__(self, store):
        self.store = store

    def _enabled_hooks(self, event):
        cfg = self.store.get("notify")
        out = []
        for h in cfg.get("webhooks", []):
            if not h.get("enabled", True):
                continue
            evs = h.get("events")
            if evs and event not in evs:
                continue
            out.append(h)
        return out

    def notify(self, event, title, body):
        cfg = self.store.get("notify")
        gate = {
            "checkin": cfg.get("notify_checkin", True),
            "quota": cfg.get("notify_quota", True),
            "error": cfg.get("notify_error", True),
            "growth": cfg.get("notify_checkin", True),
            "task": True,
        }.get(event, True)
        results = []
        if not gate:
            return [{"ok": False, "message": "该类通知已在设置中关闭"}]
        hooks = self._enabled_hooks(event)
        if not hooks:
            return [{"ok": False, "message": "未配置启用的 webhook"}]
        for h in hooks:
            ok, msg = send_one(h, title, body)
            results.append({"type": h.get("type"), "target": h.get("name") or h.get("url"),
                            "ok": ok, "message": msg})
        hist = cfg.setdefault("history", [])
        from .store import now_ts, now_str
        for r in results:
            hist.insert(0, {"ts": now_ts(), "time": now_str(), "event": event,
                            "ok": r["ok"], "target": r.get("target"), "message": r["message"]})
        cfg["history"] = hist[:100]
        self.store.put("notify", cfg)
        return results

    def test(self, hook):
        return send_one(hook, "AI 资源网关面板",
                        "通知测试成功。这是一条来自本机整合面板的消息。\n时间：%s" % now_str_local())


def now_str_local():
    from .store import now_str
    return now_str()
