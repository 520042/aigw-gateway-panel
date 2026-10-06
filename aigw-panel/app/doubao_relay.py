# -*- coding: utf-8 -*-
"""
豆包原生对话中继（面板内置，免 APK/EXE）

协议来源
--------
1. base(1).apk dex（dev.doubao2api 的模型 id / 鉴权方式）
2. 社区完整逆向实现 doubao-free-api（Vita0519 版 sse.py/models.py，
   2026-10-05 经 WebFetch 核对全文后移植为纯标准库）

协议要点
--------
- POST https://www.doubao.com/samantha/chat/completion?aid=497858&...
- Cookie: sessionid=xxx; sessionid_ss=xxx（面板 CDP 抓的就是这个）
- 体：bot_id（各模式同值）+ completion_option（use_deep_think/use_auto_cot 区分
  pro/think/expert）+ messages[{content:JSON({"text":..}),content_type:2001}]
- 响应：SSE，event_type=2001 时 content_type 10000/2001/2008/2071 出文本/思考
- 验活：/passport/account/info/v2 返回 user_id 即有效

多账号池：账号池里该平台所有可用凭据轮转；429/限流 → 该账号冷却 60s。
"""

import json
import time
import uuid
import urllib.request
import urllib.error
import ssl

API = "https://www.doubao.com/samantha/chat/completion"
BOT_ID = "7338286299411103781"
UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")

# APK 网关模型 id（bundled_doubao.APK_GATEWAY_MODELS）→ samantha 模式开关
MODE_FLAGS = {
    "doubao-pro":    {},
    "doubao-fast":   {},
    "doubao-lite":   {},
    "doubao-think":  {"use_deep_think": True},
    "doubao-expert": {"use_deep_think": True, "use_auto_cot": True, "use_search": True},
    "doubao-coding": {"use_auto_cot": True},
    "auto":          {},
    # 网页版菜单键也兼容进来
    "doubao-2.1-turbo": {}, "doubao-2.1-turbo-alt": {},
    "doubao-2.1-pro":   {"use_deep_think": True},
    "doubao-2.1-lite":  {},
    "0": {}, "3": {}, "4": {}, "5": {}, "9": {}, "seed-lite-7b": {},
}

# ---------------------------------------------------------------- 账号池轮转
# ---------------------------------------------------------------- 账号池轮转
# ★ 2026-10-05 起委托通用池 app/acct_pool.py（对齐社区 2api 标配：
#   轮询 + 429 软冷却 60s + 失效硬冷却 12h + 请求级换号 3 次）。
#   以下保留旧 API 兼容（test_login / main.py 既有调用）。
from app import acct_pool as _POOL


def _fp(secret):
    return _POOL._fp(secret)


def pick_secret(accounts, pid):
    return _POOL.pick_secret(accounts, pid)


def mark_cooldown(pid, secret, level="soft"):
    _POOL.mark_cooldown(pid, secret, level)


def pool_status(accounts, pid):
    return _POOL.pool_status(accounts, pid)


def try_accounts(accounts, pid, fn):
    return _POOL.try_accounts(accounts, pid, fn)


# ---------------------------------------------------------------- 会话续接
# 同一凭据 30 分钟内复用 conversation_id（多轮上下文），超时则开新会话
_conv = {}   # fp(secret) -> {"cid": str, "ts": float}
_CONV_TTL = 1800.0


def _device_ids(secret):
    """由凭据派生稳定 19 位 device_id/web_id（doubao 要求 19 位数字）"""
    import hashlib
    h = hashlib.md5(("doubao" + (secret or "")).encode("utf-8")).hexdigest()
    n = str(int(h[:14], 16))[:19].ljust(19, "7")
    return n, n[:19]


def _url_params(secret):
    dev, web = _device_ids(secret)
    return "&".join([
        "aid=497858", f"device_id={dev}", "device_platform=web", "language=zh",
        "pc_version=3.17.3", "pkg_type=release_version", "real_aid=497858",
        "region=CN", "samantha_web=1", "sys_region=CN", f"tea_uuid={dev}",
        "use-olympus-account=1", "version_code=20800", f"web_id={web}",
    ])


def _headers(secret):
    return {
        "content-type": "application/json",
        "accept": "text/event-stream",
        "agw-js-conv": "str",
        "cookie": secret,
        "origin": "https://www.doubao.com",
        "referer": "https://www.doubao.com/chat/",
        "user-agent": UA,
        "x-flow-trace": json.dumps(
            {"trace_id": uuid.uuid4().hex, "span_id": uuid.uuid4().hex}),
    }


def _body(messages, model, secret):
    flags = MODE_FLAGS.get(model, {})
    fp = _fp(secret)
    prev = _conv.get(fp)
    reuse = bool(prev and time.time() - prev["ts"] < _CONV_TTL and prev.get("cid"))
    cid = prev["cid"] if reuse else "0"

    def text_of(c):
        if isinstance(c, str):
            return c
        if isinstance(c, list):
            return "\n".join(i.get("text", "") for i in c
                             if isinstance(i, dict) and i.get("type") == "text")
        return str(c)

    msgs = []
    for m in messages or []:
        t = text_of(m.get("content"))
        if not t:
            continue
        msgs.append({"content": json.dumps({"text": t}, ensure_ascii=False),
                     "content_type": 2001, "attachments": [], "references": []})
    ext = {}
    if flags.get("use_deep_think"):
        ext["use_deep_think"] = "1"
    if flags.get("use_search"):
        ext["use_search"] = "1"
    return {
        "bot_id": BOT_ID,
        "completion_option": {
            "is_regen": False, "with_suggest": False,
            "need_create_conversation": not reuse, "launch_stage": 1,
            "use_auto_cot": bool(flags.get("use_auto_cot")),
            "use_deep_think": bool(flags.get("use_deep_think")),
        },
        "conversation_id": cid,
        "local_conversation_id": "local_%d" % (uuid.uuid4().int % 10 ** 16),
        "local_message_id": str(uuid.uuid4()),
        # 新会话只发最后一条（与 free-api 一致）；续接时发最后一条用户消息
        "messages": msgs[-1:],
        "ext": ext,
    }


def _sse_parse(line):
    if not line.startswith("data:"):
        return None
    ds = line[5:].strip()
    if ds == "[DONE]":
        return None
    try:
        outer = json.loads(ds)
    except json.JSONDecodeError:
        return None
    inner = outer.get("event_data")
    if isinstance(inner, str) and inner:
        try:
            inner = json.loads(inner)
        except json.JSONDecodeError:
            inner = {}
    return {"event_type": outer.get("event_type"), "data": inner or {}}


def _ssl_ctx():
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def chat(messages, secret, model="doubao-pro", timeout=180):
    """
    非流式聚合调用。messages 为 OpenAI 格式 [{role, content}]。
    返回 (ok, payload)。ok=True 时 payload 为 OpenAI chat.completion 形状；
    False 时 payload 为错误说明（含 status/body 供验活判断）。
    """
    if not secret:
        return False, "缺少豆包 Cookie（sessionid）"
    url = API + "?" + _url_params(secret)
    data = json.dumps(_body(messages, model, secret)).encode("utf-8")
    req = urllib.request.Request(url, data=data, method="POST",
                                 headers=_headers(secret))
    for k in list(__import__("os").environ):
        if k.upper().endswith("_PROXY"):
            __import__("os").environ.pop(k, None)
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_ctx()) as r:
            raw = r.read().decode("utf-8", "replace")
            status = r.status
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", "replace")
        return False, {"status": e.code, "body": body[:400]}
    except Exception as e:
        return False, {"status": 0, "body": "%s: %s" % (type(e).__name__, e)}

    text, thinking, cid = [], [], ""
    for line in raw.splitlines():
        p = _sse_parse(line.strip())
        if not p:
            continue
        if p["event_type"] == 2001:
            msg = (p["data"] or {}).get("message", {})
            ct = msg.get("content_type")
            if ct in (10000, 2001, 2008, 2071):
                rc = msg.get("content", "")
                try:
                    cc = json.loads(rc) if isinstance(rc, str) and rc else (rc or {})
                except json.JSONDecodeError:
                    cc = {"text": rc}
                t = cc.get("text", "") if isinstance(cc, dict) else str(cc)
                th = (cc.get("thinking") or cc.get("reasoning_content") or "") \
                    if isinstance(cc, dict) else ""
                if ct == 2008 and not th:
                    th, t = t, ""
                if t:
                    text.append(t)
                if th:
                    thinking.append(th)
        c = (p["data"] or {}).get("conversation_id")
        if c:
            cid = c
    full = "".join(text).strip()
    if not full:
        return False, {"status": status, "body": raw[:400]}
    fp = _fp(secret)
    if cid:
        _conv[fp] = {"cid": cid, "ts": time.time()}
    content = full
    reasoning = "".join(thinking).strip() or None
    msg = {"role": "assistant", "content": content}
    if reasoning:
        msg["reasoning_content"] = reasoning
    approx_in = sum(len(str(m.get("content", ""))) for m in (messages or []))
    return True, {
        "id": "chatcmpl-" + uuid.uuid4().hex[:24],
        "object": "chat.completion",
        "created": int(time.time()),
        "model": model,
        "conversation_id": cid,
        "choices": [{"index": 0, "message": msg, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": approx_in // 2 or 1,
                  "completion_tokens": len(full) // 2 or 1,
                  "total_tokens": (approx_in + len(full)) // 2 or 1},
    }


def verify(secret):
    """验活：/passport/account/info/v2 返回 user_id 即有效（社区实测口径）"""
    url = ("https://www.doubao.com/passport/account/info/v2?"
           "account_sdk_source=web&aid=497858")
    req = urllib.request.Request(url, method="POST", headers={
        "User-Agent": UA, "Cookie": secret or "",
        "Content-Type": "application/json"})
    for k in list(__import__("os").environ):
        if k.upper().endswith("_PROXY"):
            __import__("os").environ.pop(k, None)
    try:
        with urllib.request.urlopen(req, timeout=15, context=_ssl_ctx()) as r:
            d = json.loads(r.read().decode("utf-8", "replace"))
        return bool(d.get("data", {}).get("user_id") or d.get("user_id")), d
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, e)


def chat_stream(messages, secret, model="doubao-pro", timeout=180):
    """
    流式版（对齐社区 2api 标配的 SSE 输出）：逐行解析上游 SSE，
    yield OpenAI chat.completion.chunk 字符串（每帧自带 data: 前缀与
    两个换行，最后 yield `data: [DONE]`）。出错时 yield 一帧
    {"aigw_error": true, ...}，由调用方决定呈现方式。
    """
    if not secret:
        yield 'data: ' + json.dumps({"aigw_error": True,
              "message": "缺少豆包 Cookie（sessionid）"}) + "\n\n"
        return
    url = API + "?" + _url_params(secret)
    data = json.dumps(_body(messages, model, secret)).encode("utf-8")
    req = urllib.request.Request(url, data=data, method="POST",
                                 headers=_headers(secret))
    for k in list(__import__("os").environ):
        if k.upper().endswith("_PROXY"):
            __import__("os").environ.pop(k, None)
    cid = ""
    chat_id = "chatcmpl-" + uuid.uuid4().hex[:24]

    def chunk(delta, finish=None):
        return "data: " + json.dumps({
            "id": chat_id, "object": "chat.completion.chunk",
            "created": int(time.time()), "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        }, ensure_ascii=False) + "\n\n"

    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_ctx()) as r:
            yield chunk({"role": "assistant", "content": ""})
            for raw in r:
                line = raw.decode("utf-8", "replace").strip()
                p = _sse_parse(line)
                if not p:
                    continue
                if p["event_type"] == 2001:
                    msg = (p["data"] or {}).get("message", {})
                    ct = msg.get("content_type")
                    if ct in (10000, 2001, 2008, 2071):
                        rc = msg.get("content", "")
                        try:
                            cc = json.loads(rc) if isinstance(rc, str) and rc else (rc or {})
                        except json.JSONDecodeError:
                            cc = {"text": rc}
                        t = cc.get("text", "") if isinstance(cc, dict) else str(cc)
                        th = (cc.get("thinking") or cc.get("reasoning_content") or "") \
                            if isinstance(cc, dict) else ""
                        if ct == 2008 and not th:
                            th, t = t, ""
                        if th:
                            yield chunk({"reasoning_content": th})
                        if t:
                            yield chunk({"content": t})
                c = (p["data"] or {}).get("conversation_id")
                if c:
                    cid = c
            yield chunk({}, finish="stop")
            yield "data: [DONE]\n\n"
            if cid:
                _conv[_fp(secret)] = {"cid": cid, "ts": time.time()}
    except Exception as e:
        yield 'data: ' + json.dumps({"aigw_error": True,
              "message": "%s: %s" % (type(e).__name__, e)}) + "\n\n"
