# -*- coding: utf-8 -*-
"""
原生对话中继（native relay）—— 从 APK/EXE 字节码静态提取的各平台对话端点。

为什么需要
----------
用户质疑："难道我提供的APP和EXE文件里面，没有相关代码吗？那他们的网关是怎么
处理这些东西的？" —— 对。每个 2api APK / 网关 EXE 内部就是一段「OpenAI 入站 →
平台上游出站」的转发代码，端点、头、鉴权全部在字节码里。本模块把它们结构化，
让面板**不启动任何 EXE、不做登录抓包**也能按凭据直连对话。

端点证据（全部逐条来自字节码，非猜测；标注 ★ 的待真 token 联调）
----------------------------------------------------------------
apk-codebuddy 系（base(3).apk classes3.dex + workbuddy-gateway EXE）:
    POST https://copilot.tencent.com/v2/chat/completions            ★
    Authorization: Bearer <accessToken>
    X-Domain / X-Tenant-Id / X-User-Id / X-Enterprise-Id /
    X-No-Department-Info / X-No-Enterprise-Id / X-Client-ID / X-Env-ID
apk-trae（base.apk classes.dex）:
    POST https://api.trae.cn/api/v1/chat/completions                ★
    （备选 /v2/chat/completions、/v1/chat/completions）
    Authorization: Bearer + X-Ide-Token / X-Cloudide-Token / X-Tenant-Id / X-Domain
apk-doubao（base(1).apk dex，gwextra 已登记 verified）:
    POST https://www.doubao.com/samantha/chat/completion
    Cookie 鉴权
apk-yuanbao（base(5).apk dex，bundled_yuanbao.ENDPOINTS）:
    POST https://yuanbao.tencent.com/api/chat/completions
    hy_token / hy_user / uskey（localStorage 会话）
apk-raccoon（base(4).apk classes6.dex）:
    POST https://xiaohuanxiong.com/api/web/llm/v2/chat/completions   ★
    Cookie 鉴权
"""

import json
import ssl
import sys
import time
import os
import urllib.error
import urllib.request

# ---------------------------------------------------------------- 中继规格
# fmt: openai = 请求/响应直接透传；custom = 上游私有格式，原样转发由调用方适配
RELAY_SPECS = {
    "apk-codebuddy": {
        "name": "CodeBuddy 国际（copilot 直连）",
        "base": "https://copilot.tencent.com",
        "path": "/v2/chat/completions",
        "method": "POST",
        "auth": "bearer",
        "fmt": "openai",
        # ★ 实测（2026-10-06，wb-gateway 真token）：上游只收流式 ——
        #   非流式 400 code 11101 "Non-stream chat request is currently
        #   not supported"。relay_once 会自动转流式并聚合回完整响应。
        "stream_only": True,
        "extra_headers": {
            "X-Domain": "codebuddy",
            "X-Client-ID": "codebuddy-cli",
        },
        "extra_headers_fn": "copilot",
        "evidence": "base(3).apk classes3.dex + EXE upstreamProfile + "
                    "workbuddy2api converter.py _build_headers_from（风控头组）"
                    "+ live 400/11101",
        "pending_live": False,
    },
    "apk-codebuddy-cn": {
        "name": "CodeBuddy 国内（copilot 直连）",
        "base": "https://www.codebuddy.cn",
        "path": "/v2/chat/completions",
        "method": "POST",
        "auth": "bearer",
        "fmt": "openai",
        "stream_only": True,       # 同国际站（同一套网关）
        "extra_headers": {"X-Domain": "codebuddy"},
        "extra_headers_fn": "copilot",
        "evidence": "base(3).apk classes3.dex（国内域变体）+ workbuddy2api 头组规格",
        "pending_live": True,
    },
    "apk-trae": {
        "name": "Trae（api.trae.cn 直连）",
        "base": "https://api.trae.cn",
        "path": "/api/v1/chat/completions",
        "method": "POST",
        "auth": "bearer",
        "fmt": "openai",
        "extra_headers": {"X-Domain": "trae"},
        "evidence": "base.apk classes.dex（/api/v1 为主，/v2 备选）",
        "pending_live": True,
    },
    "apk-doubao": {
        "name": "豆包（samantha 直连）",
        "base": "https://www.doubao.com",
        "path": "/samantha/chat/completion",
        "method": "POST",
        "auth": "cookie",
        "fmt": "custom",
        "extra_headers": {},
        "evidence": "base(1).apk dex + gwextra verified",
        "pending_live": False,
    },
    "apk-yuanbao": {
        "name": "元宝（hy 直连）",
        "base": "https://yuanbao.tencent.com",
        "path": "/api/chat/completions",
        "method": "POST",
        "auth": "cookie",
        "fmt": "custom",
        "extra_headers": {},
        # 鉴权键来自 dex 静态提取（bundled_yuanbao.AUTH_FIELDS）；用户整浏览器
        # 复制的 Cookie 里其它键一律不外发（见 _filter_cookie）
        "cookie_keys": ["hy_token", "hy_user", "hyUserId", "uskey",
                        "uskeyMd5", "deviceId"],
        "evidence": "base(5).apk dex（AUTH_FIELDS: hy_token/uskey）",
        "pending_live": True,
    },
    "web-lobster": {
        "name": "网易 Lobster AI（有道龙虾）",
        "base": "@lobster",   # 运行时由 设置→Lobster 上游地址 提供
        "path": "/api/proxy/v1/chat/completions",
        "method": "POST",
        "auth": "bearer",
        "fmt": "openai",
        "extra_headers": {},
        "evidence": "社区项目 lobsterai2api internal/upstream/client.go（OpenAI 兼容透传）",
        "pending_live": True,
    },
    "web-glm": {
        "name": "智谱清言网页版（glm2api 逆向）",
        "base": "https://chatglm.cn",
        "path": "/chatglm/backend-api/assistant/stream",
        "method": "POST", "auth": "bearer", "fmt": "custom",
        "web_relay": True,   # ★ 走 app/web_relays.py（非 OpenAI 原生格式）
        "evidence": "glm2api(yxc0915) services/glm_auth.py::build_sign + "
                    "glm_client.py（X-Sign 头组）+ /user-api/user/refresh 换 access_token",
        "pending_live": True,
        "models_static": [
            {"id": "glm-4-flash", "name": "GLM-4 Flash", "desc": "智谱清言网页版（免费快）"},
            {"id": "glm-4", "name": "GLM-4", "desc": "智谱清言网页版"},
            {"id": "glm-4-plus", "name": "GLM-4 Plus", "desc": "智谱清言网页版（增强）"},
            {"id": "glm-4-air", "name": "GLM-4 Air", "desc": "智谱清言网页版"},
            {"id": "glm-4-all", "name": "GLM-4 All", "desc": "智谱清言网页版（All 工具）"},
            {"id": "glm-zero-preview", "name": "GLM Zero Preview", "desc": "智谱清言网页版（深度思考）"},
        ],
    },
    "web-trae": {
        "name": "Trae 直连（Trae2api-cn 逆向）",
        "base": "https://trae-api-cn.mchost.guru",
        "path": "/api/ide/v1/chat",
        "method": "POST", "auth": "bearer", "fmt": "custom",
        "web_relay": True,
        "evidence": "dsh-trae-connect(lament-z) src/upstream.ts（CN base + 私有 chat 协议 + "
                    "CN_APP_ID 6eefa01c-…，响应为 event:response 增量 delta）",
        "pending_live": True,
        "models_static": [
            {"id": "gpt-4o", "name": "GPT-4o", "desc": "Trae 直连"},
            {"id": "claude-3-7-sonnet", "name": "Claude 3.7 Sonnet", "desc": "Trae 直连"},
            {"id": "claude-3-5-sonnet", "name": "Claude 3.5 Sonnet", "desc": "Trae 直连"},
            {"id": "deepseek-v3", "name": "DeepSeek V3", "desc": "Trae 直连"},
            {"id": "deepseek-r1", "name": "DeepSeek R1", "desc": "Trae 直连（推理）"},
        ],
    },
    "web-deepseek": {
        "name": "DeepSeek 网页版（deepseek-free-api 逆向）",
        "base": "https://chat.deepseek.com",
        "path": "/api/v0/chat/completion",
        "method": "POST", "auth": "bearer", "fmt": "custom",
        "web_relay": True,
        "evidence": "deepseek-free-api(qiaojinxia) deepseek_client.py + pow_worker.js"
                    "（session create + create_pow_challenge + Node/WASM 解 POW + "
                    "p/v/o 分片响应）",
        "pending_live": True,
        "models_static": [
            {"id": "deepseek-chat", "name": "DeepSeek Chat", "desc": "DeepSeek 网页版"},
            {"id": "deepseek-reasoner", "name": "DeepSeek Reasoner", "desc": "DeepSeek 网页版（R1 深度思考）"},
        ],
    },
    "anon-zen": {
        "name": "Our Free Model（OpenCode Zen 匿名车道）",
        "base": "https://opencode.ai",
        "path": "/zen/v1/chat/completions",
        "method": "POST",
        "auth": "bearer",
        "fmt": "openai",
        # ★ 指纹头与会话 mint 逻辑照抄 dsh-our-free-model src/upstream.js：
        #   UA >= 1.17 是网关硬检查；session/request 是网关形状 id
        #   （ses_/msg_ + 12hex + 14base62）；免费额度按 session 计。
        "extra_headers_fn": "zen",
        "secret_fixed": "public",
        "evidence": "dsh-our-free-model src/upstream.js（2026-09-24 逐条实测）",
        "pending_live": True,
        "models_anon": "https://opencode.ai/zen/v1/models",
    },
    "web-qoder": {
        "name": "Qoder（直连模型端点，社区 Qoder2Api 逆向）",
        "base": "https://api2-v2.qoder.sh",
        "path": "/model/v1/chat/completions",
        "method": "POST",
        "auth": "bearer",
        "fmt": "openai",
        "extra_headers": {},
        "evidence": "github.com/ErfanBagheri404/Qoder2Api（桌面端直连模型端点，"
                    "绕开 COSY 签名的 agent 端点；qwen3.8-flash 免费）",
        "pending_live": True,
        "models_static": [
            {"id": "qwen3.8-flash", "name": "Qwen3.8 Flash", "desc": "免费，不扣积分"},
            {"id": "qwen3.7-plus", "name": "Qwen3.7 Plus", "desc": "trial credits"},
            {"id": "kimi-k2.7-code", "name": "Kimi K2.7 Code", "desc": "trial credits"},
            {"id": "deepseek-v4-pro", "name": "DeepSeek V4 Pro", "desc": "trial credits"},
            {"id": "minimax-m2.5", "name": "MiniMax M2.5", "desc": "trial credits"},
        ],
    },
    "apk-raccoon": {
        "name": "小浣熊（llm v2 直连）",
        "base": "https://xiaohuanxiong.com",
        "path": "/api/web/llm/v2/chat/completions",
        "method": "POST",
        "auth": "cookie",
        "fmt": "openai",
        "extra_headers": {},
        "evidence": "base(4).apk classes6.dex",
        "pending_live": True,
    },
}


def relay_of(pid):
    return RELAY_SPECS.get(pid)


def relay_list():
    return [{"platform": pid, **{k: v for k, v in spec.items()}}
            for pid, spec in RELAY_SPECS.items()]


# ---------------------------------------------------------------- 请求
def _ssl_ctx():
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


# X-Device-Token（腾讯图灵盾设备风控头）提供器。
# ★ 该 token 由 WorkBuddy/CodeBuddy 桌面端内置的 Turing Shield SDK 生成，
#   第三方无法凭空计算（workbuddy2api 也只是读桌面端产物：turing_helper.js
#   桥接 SDK / 或宿主落盘 device_token 文件）。这里走文件兜底：
#   data/device_token.txt —— 用户从桌面端拿到后放进去即生效。
#   成功缓存 600s、失败负缓存 300s（照抄社区 turing_token.py 的教训：
#   失败不缓存会每请求都读盘）。取不到就优雅跳过该头，不影响主流程。
_DT_CACHE = {"tok": None, "ts": 0.0, "fail_ts": 0.0}


def _device_token():
    import time as _t
    now = _t.time()
    if _DT_CACHE["tok"] and now - _DT_CACHE["ts"] < 600:
        return _DT_CACHE["tok"]
    if _DT_CACHE["fail_ts"] and now - _DT_CACHE["fail_ts"] < 300:
        return None
    root = (os.path.dirname(sys.executable) if getattr(sys, "frozen", False)
            else os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    tok = ""
    try:
        fp = os.path.join(root, "data", "device_token.txt")
        if os.path.isfile(fp):
            with open(fp, encoding="utf-8") as f:
                tok = (f.read() or "").strip()
            if len(tok) > 1024:          # 与 workbuddy2api 同限：>1KB 视为异常
                tok = ""
    except Exception:
        tok = ""
    if tok:
        _DT_CACHE.update(tok=tok, ts=now, fail_ts=0.0)
        return tok
    _DT_CACHE.update(tok=None, ts=0.0, fail_ts=now)
    return None


def _filter_cookie(spec, cookie):
    """
    有 cookie_keys 规格时只外发鉴权需要的键：用户从浏览器整串复制的
    Cookie 常混着其它站点的键，全量转发既是隐私泄漏也是风控信号。
    一个键都没匹配上（规格清单过期 / 用户粘贴的是别的东西）则原样透传，
    宁可多送不送错。
    """
    keys = spec.get("cookie_keys")
    if not keys or not cookie:
        return cookie
    keep = [p.strip() for p in cookie.split(";")
            if p.strip().split("=", 1)[0].strip() in keys]
    return "; ".join(keep) if keep else cookie


# X-User-Id 懒取缓存：copilot 风控要求 uid 与 token 同账号（错配反是风控信号）。
# 登录时已随响应落库（accounts.uid）；存量账号用 /console/account 懒取，
# 成功缓存 24h、失败负缓存 600s（两域同路径同响应，2026-10-06 实测）。
_UID_CACHE = {"uid": {}, "fail": {}}


def copilot_uid(secret, base="https://www.codebuddy.cn", timeout=15):
    import hashlib
    fp = hashlib.md5((secret or "").encode("utf-8")).hexdigest()[:16]
    now = time.time()
    uid = _UID_CACHE["uid"].get(fp)
    if uid:
        return uid
    if _UID_CACHE["fail"].get(fp, 0) > now:
        return ""
    req = urllib.request.Request(
        base.rstrip("/") + "/console/account",
        headers={"Authorization": "Bearer " + (secret or ""),
                 "Accept": "application/json",
                 "User-Agent": "CLI/2.143.1 CodeBuddy/2.143.1"})
    for k in list(__import__("os").environ):
        if k.upper().endswith("_PROXY"):
            __import__("os").environ.pop(k, None)
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_ctx()) as r:
            d = json.loads(r.read().decode("utf-8", "replace"))
        uid = ((d.get("data") or {}).get("uid")) or ""
    except Exception:
        uid = ""
    if uid:
        _UID_CACHE["uid"][fp] = uid
    else:
        _UID_CACHE["fail"][fp] = now + 600
    return uid


def _headers(pid, spec, secret, body=None, account=None):
    h = {
        "User-Agent": "aigw-panel-native-relay/1.0",
        "Accept": "application/json",
        "Content-Type": "application/json",
    }
    if spec.get("extra_headers_fn") == "zen":
        # OpenCode Zen 匿名车道：UA 指纹 + x-opencode-* 会话头（照抄插件实现）。
        # 会话按对话稳定复用（额度按 session 计，fresh-per-request 会烧光额度）。
        h["User-Agent"] = "opencode/1.18.31"
        h["Accept"] = "*/*"
        h["Authorization"] = "Bearer public"
        _, reqid = _zen_ids()
        h["x-opencode-client"] = "desktop"
        h["x-opencode-session"] = _zen_session_for(body)
        h["x-opencode-request"] = reqid
        h["x-opencode-project"] = "global"
        return h
    if spec.get("extra_headers_fn") == "copilot":
        # copilot 直连风控头组（规格逆向自 workbuddy2api converter.py，
        # 与桌面端真实出站请求对齐；缺失会被上游识别为异常客户端）：
        #   X-Agent-Purpose / X-IDE-Name / X-IDE-Type / X-Product (+ X-Device-Token)
        # UA 三段式：CLI/<v> <产品>/<v>（tlogin 实测 12403 check ua 通过的形状）。
        _nm = "CodeBuddy" if "codebuddy" in (pid or "") else "WorkBuddy"
        h["User-Agent"] = "CLI/2.143.1 %s/2.143.1" % _nm
        h["X-Agent-Purpose"] = "conversation"
        h["X-IDE-Name"] = _nm
        h["X-IDE-Type"] = _nm
        h["X-Product"] = "SaaS"
        _tok = _device_token()
        if _tok:
            h["X-Device-Token"] = _tok
        # X-User-Id：uid 与 token 必须同账号（错配反而是风控信号）。
        # 优先用账号落库的 uid（登录响应 / /console/account），存量账号懒取。
        _uid = (account or {}).get("uid") or copilot_uid(secret)
        if _uid:
            h["X-User-Id"] = str(_uid)
    if spec["auth"] == "bearer":
        h["Authorization"] = "Bearer " + (secret or "")
    else:
        h["Cookie"] = _filter_cookie(spec, secret or "")
    for k, v in (spec.get("extra_headers") or {}).items():
        h[k] = v
    return h


def relay_once(pid, body, secret, timeout=180, base_override=None, account=None):
    """
    单发对话（非流式聚合）：pid 平台 + OpenAI 请求体 + 该平台凭据。
    base_override：动态基址（web-lobster 的上游地址来自设置）。
    account：账号池行 dict（可带 uid 等账号级字段，用于风控头注入）。
    返回 (ok, payload)。ok=True 时 payload 是上游 JSON；
    False 时 payload 是错误说明字符串。
    上游私有格式（custom）原样返回 —— 调用方按平台适配字段。
    """
    spec = relay_of(pid)
    if not spec:
        return False, "平台 %s 没有原生中继规格" % pid
    if not secret:
        if spec.get("secret_fixed"):
            secret = spec["secret_fixed"]       # 匿名车道（Zen）：公共凭据
        else:
            return False, "账号池里没有 %s 的可用凭据" % pid
    base = spec["base"]
    if base == "@lobster":
        base = (base_override or "").rstrip("/")
        if not base:
            return False, ("未配置 Lobster 上游地址：设置→「Lobster 上游地址」"
                           "（lobsterai2api 的 LB2A_UPSTREAM_BASE）")
    url = base.rstrip("/") + spec["path"]
    renames = None
    if spec.get("extra_headers_fn") == "zen":
        # 免费层工具指纹门：四件套缺槽补自禁 decoy（403 FreeTierError 不补就挂）
        renames = _zen_fingerprint(body)
    stream_only = bool(spec.get("stream_only"))
    if stream_only:
        # 上游只收流式（copilot 系实测 400/11101）：转流式发、聚合回完整响应
        body = dict(body or {}, stream=True)
    data = json.dumps(body or {}).encode("utf-8")
    req = urllib.request.Request(url, data=data, method="POST",
                                 headers=_headers(pid, spec, secret, body,
                                                  account=account))
    if stream_only:
        req.add_header("Accept", "text/event-stream")
    for k in list(__import__("os").environ):
        if k.upper().endswith("_PROXY"):
            __import__("os").environ.pop(k, None)
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_ctx()) as r:
            raw = r.read().decode("utf-8", "replace")
            status = r.status
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        return False, "上游 %d: %s" % (e.code, raw[:300])
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, e)
    if stream_only:
        payload = _aggregate_sse(raw)
        if payload is None:
            return False, "上游流式响应解析为空: %s" % raw[:200]
    else:
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError:
            return status < 400, {"status": status, "raw": raw[:2000]}
    if renames:
        payload = _zen_restore_tools(payload, renames)
    return (status < 400), payload


def _aggregate_sse(raw):
    """把上游 SSE 聚合回一个 OpenAI chat.completion 形状（stream_only 上游用）。
    content 增量拼接、tool_calls 增量按 index 拼接、usage/finish_reason 取末帧。"""
    txt_parts, tool_calls, usage, model, cid, finish = [], {}, None, None, None, None
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        chunk = line[5:].strip()
        if chunk == "[DONE]":
            break
        try:
            d = json.loads(chunk)
        except json.JSONDecodeError:
            continue
        model = d.get("model") or model
        cid = d.get("id") or cid
        if d.get("usage"):
            usage = d["usage"]
        for ch in d.get("choices") or []:
            delta = ch.get("delta") or ch.get("message") or {}
            if isinstance(delta.get("content"), str):
                txt_parts.append(delta["content"])
            for tc in delta.get("tool_calls") or []:
                idx = tc.get("index", 0)
                slot = tool_calls.setdefault(idx, {"id": tc.get("id") or "",
                                                   "type": "function",
                                                   "function": {"name": "", "arguments": ""}})
                if tc.get("id"):
                    slot["id"] = tc["id"]
                f = tc.get("function") or {}
                if f.get("name"):
                    slot["function"]["name"] += f["name"]
                if f.get("arguments"):
                    slot["function"]["arguments"] += f["arguments"]
            if ch.get("finish_reason"):
                finish = ch["finish_reason"]
    if model is None and cid is None and not txt_parts and not tool_calls:
        return None
    msg = {"role": "assistant",
           "content": "".join(txt_parts) if txt_parts else None}
    if tool_calls:
        msg["tool_calls"] = [tool_calls[k] for k in sorted(tool_calls)]
    out = {"id": cid or ("chatcmpl-" + os.urandom(12).hex()),
           "object": "chat.completion", "created": int(time.time()),
           "model": model or "", "choices": [
               {"index": 0, "message": msg, "finish_reason": finish or "stop"}]}
    if usage:
        out["usage"] = usage
    return out


def relay_stream_meta(pid):
    """给前端/调用方的说明：该平台中继的端点与证据（不含凭据）"""
    spec = relay_of(pid)
    if not spec:
        return None
    return {"platform": pid, "url": spec["base"] + spec["path"],
            "auth": spec["auth"], "fmt": spec["fmt"],
            "evidence": spec["evidence"],
            "pending_live": spec.get("pending_live", False)}


def relay_stream(pid, body, secret, base_override=None, timeout=180,
                 account=None):
    """
    流式透传（对齐社区 2api 标配的 SSE 输出）：仅支持 fmt="openai" 的平台
    （上游本身就是 OpenAI 格式 SSE，逐帧原样转发）。yield 字符串帧。
    出错时 yield 一帧 {"aigw_error": true, ...}。
    """
    spec = relay_of(pid)
    if not spec:
        yield 'data: ' + json.dumps({"aigw_error": True,
              "message": "平台 %s 没有原生中继规格" % pid}) + "\n\n"
        return
    if spec.get("fmt") != "openai":
        yield 'data: ' + json.dumps({"aigw_error": True,
              "message": "平台 %s 上游非 OpenAI 格式，暂不支持流式" % pid}) + "\n\n"
        return
    if not secret:
        if spec.get("secret_fixed"):
            secret = spec["secret_fixed"]
        else:
            yield 'data: ' + json.dumps({"aigw_error": True,
                  "message": "账号池里没有 %s 的可用凭据" % pid}) + "\n\n"
            return
    base = spec["base"]
    if base == "@lobster":
        base = (base_override or "").rstrip("/")
        if not base:
            yield 'data: ' + json.dumps({"aigw_error": True,
                  "message": "未配置 Lobster 上游地址（设置→Lobster 上游地址）"}) + "\n\n"
            return
    url = base.rstrip("/") + spec["path"]
    sbody = dict(body or {}, stream=True)
    if spec.get("extra_headers_fn") == "zen":
        _zen_fingerprint(sbody)      # 免费层工具指纹门
    data = json.dumps(sbody).encode("utf-8")
    req = urllib.request.Request(url, data=data, method="POST",
                                 headers=_headers(pid, spec, secret, sbody,
                                                  account=account))
    req.add_header("Accept", "text/event-stream")
    for k in list(__import__("os").environ):
        if k.upper().endswith("_PROXY"):
            __import__("os").environ.pop(k, None)
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_ctx()) as r:
            for raw in r:
                line = raw.decode("utf-8", "replace").rstrip("\n")
                if line:
                    yield line + "\n\n"
    except Exception as e:
        yield 'data: ' + json.dumps({"aigw_error": True,
              "message": "%s: %s" % (type(e).__name__, e)}) + "\n\n"


def _zen_ids():
    """
    网关形状的会话/请求 id（照抄 dsh-our-free-model mintSessionId/mintRequestId）：
    ses_/msg_ + 12 位 hex（时间反码）+ 14 位 base62。
    免费额度按 session 计——同一会话复用同一 id 才不浪费额度。
    """
    ts = int(time.time() * 1000)
    val = ~(ts * 0x1000 + 1) & 0xFFFFFFFFFF
    hex6 = ("%010x" % val)
    hex6 = (hex6 + "00")[:12]
    B62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
    rnd = os.urandom(14)
    b62 = "".join(B62[b % 62] for b in rnd)
    return "ses_%s%s" % (hex6, b62), "msg_%s%s" % (hex6, b62)


def _zen_session_for(body):
    """免费额度按 session 计，fresh id 每请求 = 自己把额度烧光（429 收场）。
    照抄 sessionForConversation 的 sha256 方案：同一会话稳定映射到同一
    canonical session。面板侧用「首条 user 消息」做会话种子（多轮对话里
    它不变，天然按会话分桶）；无消息回落 'global'。"""
    import hashlib
    seed = "global"
    for m in (body or {}).get("messages") or []:
        if isinstance(m, dict) and m.get("role") == "user":
            c = m.get("content")
            if isinstance(c, str) and c.strip():
                seed = c
            elif c is not None:
                seed = json.dumps(c, ensure_ascii=False)[:512]
            break
    d = hashlib.sha256(("our-free-model\x00" + seed).encode("utf-8")).digest()
    B62 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
    return "ses_%s%s" % (d[:6].hex(), "".join(B62[b % 62] for b in d[6:20]))


# 免费层工具指纹门（upstream.js：403 FreeTierError without it）：
# 请求必须声明 bash/glob/grep/read 四件套，缺的槽位补「自禁 decoy」
# （描述写明不可用 + tool_choice=none，模型不会真去调）。
_ZEN_QUARTET = ("bash", "glob", "grep", "read")
_ZEN_DECOY_DESC = "This tool is currently unavailable and must not be used."


def _zen_fingerprint(body):
    """补齐四件套指纹。返回改名表 {规范名: 调用方原名}，响应里的
    tool_calls 名字要用它回填（restoreToolName 等价物）。"""
    tools = body.get("tools")
    tools = [t for t in tools if isinstance(t, dict)] \
        if isinstance(tools, list) else []
    out, seen, renames = [], set(), {}
    for t in tools:
        fn = t.get("function") if isinstance(t.get("function"), dict) else None
        name = (fn or t).get("name")
        name = name.strip() if isinstance(name, str) else ""
        key = name.lower() if name.lower() in _ZEN_QUARTET else ""
        if not key:
            out.append(t)
            continue
        if key in seen:
            continue
        seen.add(key)
        if name != key:
            renames[key] = name
            t = dict(t, function=dict(fn, name=key)) if fn else dict(t, name=key)
        out.append(t)
    for name in _ZEN_QUARTET:
        if name in seen:
            continue
        out.append({"type": "function", "function": {
            "name": name, "description": _ZEN_DECOY_DESC,
            "parameters": {"type": "object", "properties": {}}}})
    body["tools"] = out
    if not body.get("tool_choice"):
        body["tool_choice"] = "auto" if tools else "none"
    return renames


def _zen_restore_tools(payload, renames):
    """响应侧把 decoy 规范名回填成调用方原名（无改名表时原样返回）。"""
    if not renames or not isinstance(payload, dict):
        return payload
    for ch in payload.get("choices") or []:
        for call in ((ch.get("message") or {}).get("tool_calls") or []):
            fn = call.get("function") if isinstance(call, dict) else None
            if fn and fn.get("name") in renames:
                fn["name"] = renames[fn["name"]]
    return payload


_ZEN_CACHE = {"ts": 0.0, "ok": False, "models": []}

# 免密车道判定（照抄 dsh-our-free-model catalog.isFreeLane）：
# /zen/v1/models 混着收费 id —— 收费 id 打 chat 返 401 Missing API key，
# 只有 -free 分段和 ALWAYS_FREE 这两个例外免密。
_ZEN_ALWAYS_FREE = {"union-alpha", "space-bunny-free"}

# 非Chat wire 的免费模型：muse-spark 系走 /zen/v1/responses、union-alpha 走
# /zen/v1/messages。中继目前只透传 chat/completions，下发只会让路由进死胡同。
_ZEN_NON_CHAT_PREFIX = ("muse-spark", "muse_spark")


def _zen_free_lane(mid):
    if mid in _ZEN_ALWAYS_FREE:
        return True
    import re
    return bool(re.search(r"(?:^|[-_])free(?:$|[-_.])", mid))


def zen_models(timeout=20, ttl=600):
    """OpenCode Zen 免密车道模型清单（Bearer public，免登录）。返回 (ok, models|str)

    清单按免密车道过滤（见 _zen_free_lane），剔除非 chat wire 的模型；
    /v1/models 与 CodeDesk 类客户端会高频轮询，做 10 分钟 TTL 缓存，
    失败不缓存（下次重试），成功才落缓存。
    """
    now = time.time()
    if _ZEN_CACHE["ok"] and now - _ZEN_CACHE["ts"] < ttl:
        return True, _ZEN_CACHE["models"]
    req = urllib.request.Request(
        "https://opencode.ai/zen/v1/models",
        headers={"User-Agent": "opencode/1.18.31",
                 "Authorization": "Bearer public",
                 "Accept": "application/json"})
    for k in list(__import__("os").environ):
        if k.upper().endswith("_PROXY"):
            __import__("os").environ.pop(k, None)
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_ctx()) as r:
            d = json.loads(r.read().decode("utf-8", "replace"))
        models = d.get("data") or d.get("models") or []
        out = []
        for m in models:
            if not (isinstance(m, dict) and m.get("id")):
                continue
            mid = str(m["id"]).strip()
            base = mid.split("/")[-1]          # EAC 联营道 id 带 org/ 前缀
            if not _zen_free_lane(base):
                continue
            if base.startswith(_ZEN_NON_CHAT_PREFIX) or mid == "union-alpha":
                continue
            caps = m.get("capabilities")
            out.append({"id": base, "name": m.get("name") or base,
                        "ctx": m.get("context_length"),
                        "desc": (", ".join(caps) if isinstance(caps, list)
                                 else (m.get("description") or ""))
                                or "Zen 免密车道"})
        _ZEN_CACHE.update(ts=now, ok=True, models=out)
        return True, out
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, e)
