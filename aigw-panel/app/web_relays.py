# -*- coding: utf-8 -*-
"""
网页版 AI 反代中继（非 OpenAI 原生格式的上游）。

本轮（build38）已把三个平台的「推断请求体」全部对齐到社区逆向**一手源码**，
不再是猜测。来源逐条标注（evidence），改版时按来源复核：

  - web-glm     智谱清言 chatglm.cn
      来源：glm2api（yxc0915）src/glm2api/services/glm_auth.py::build_sign
            + glm_client.py（X-Sign 头组）+ config.py（端点常量）
      ★ 关键补全（build33 遗留 403 根因）：
        - X-Sign / X-Timestamp / X-Nonce 三个签名头，缺一即 403
        - SIGN_SECRET = "8a1317a7468aa3ad86e997d08f3f31cb"
        - timestamp 不是普通毫秒：倒数第二位替换为 (各位和-该位)%10 的校验位
        - 拿到的 chatglm_refresh_token 是 refresh_token，要先经
          /user-api/user/refresh 换 access_token 才能调 stream（build33 直发 refresh 会 401）
  - web-trae    Trae 国内版直连
      来源：dsh-trae-connect（lament-z）src/upstream.ts
      ★ 关键修正（build33 用错了 base 与 appId）：
        - 真实 base = https://trae-api-cn.mchost.guru（不是 a0ai-api-sg.byteintlapi.com）
        - 真实 CN appId = 6eefa01c-1036-4c7e-9ca5-d891f63bfcd8
          （boot-config 里的 932204 会被网关直接拒：record not found）
        - IDE_VERSION_CODE = 20260401（build33 用的 20250325 已过期）
        - 请求体是 Trae 私有 chat 协议：user_input/intent_name/variables/
          chat_history/session_id/model_name/function='chat'，缺 function 会
          报 "function is empty, cannot resolve model"
        - 响应是 event: response 的**增量 delta**（build33 当成全量快照覆盖，会丢字）
  - web-deepseek  DeepSeek 网页版
      来源：deepseek-free-api（qiaojinxia）deepseek_client.py + pow_worker.js
      ★ 关键补全（build33 完全没处理，必然失败）：
        - 必须先 POST /api/v0/chat_session/create 拿 chat_session_id
        - 必须先 POST /api/v0/chat/create_pow_challenge 拿挑战，用随包的
          Node+WASM（app/webpow/）解出 answer，base64 编码后放进
          x-ds-pow-response 头，缺这个头请求会被拒
        - 响应是 p/v/o 分片协议（response/fragments APPEND 等），不是标准 choices

⚠ 仍需用户在本机提供各平台真实凭据才能端到端验证（data/accounts.json 目前
   尚无 web-glm/web-trae/web-deepseek 条目）。协议本身已按一手源码实现。
"""
import base64
import hashlib
import io
import json
import os
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

CTX = ssl.create_default_context()
CTX.check_hostname = False
CTX.verify_mode = ssl.CERT_NONE

UA_CHROME = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
             "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")


def _no_proxy():
    """清掉 *_PROXY 环境变量：这些网页上游直连更稳，也避免 TSD 代理注入。"""
    for k in list(os.environ):
        if k.upper().endswith("_PROXY"):
            os.environ.pop(k, None)


def _open(url, headers, timeout, data=None, method=None):
    _no_proxy()
    req = urllib.request.Request(url, data=data, method=method or ("POST" if data else "GET"),
                                 headers=headers)
    return urllib.request.urlopen(req, timeout=timeout, context=CTX)


def _read_sse(resp, on_line):
    """逐行读 SSE，回调 (event_name, data_str)。"""
    ev = None
    for raw in resp:
        line = raw.decode("utf-8", "replace").strip()
        if not line:
            continue
        if line.startswith("event:"):
            ev = line[6:].strip()
        elif line.startswith("data:"):
            on_line(ev, line[5:].strip())


# ==================================================================== 智谱清言
GLM_BASE = "https://chatglm.cn/chatglm"
GLM_SIGN_SECRET = "8a1317a7468aa3ad86e997d08f3f31cb"   # ← glm_auth.py:19 一手常量
GLM_ASSISTANT_ID = "65940acff94777010aa6b796"          # glm2api 默认助手
# refresh_token -> access_token 缓存（按 refresh_token 键，带过期）
_GLM_TOK = {}


def _glm_build_sign():
    """复刻 glm2api build_sign()：timestamp 倒数第二位是校验位。

    now = 毫秒时间戳字符串；checksum = (各位数字和 - 倒数第二位) % 10；
    timestamp = now[:-2] + str(checksum) + now[-1]
    sign = md5(f"{timestamp}-{nonce}-{SECRET}")
    """
    now = str(int(time.time() * 1000))
    digits = [int(c) for c in now]
    checksum = (sum(digits) - digits[-2]) % 10
    timestamp = now[:-2] + str(checksum) + now[-1]
    nonce = uuid.uuid4().hex
    sign = hashlib.md5(
        ("%s-%s-%s" % (timestamp, nonce, GLM_SIGN_SECRET)).encode("utf-8")).hexdigest()
    return timestamp, nonce, sign


def _glm_access_token(refresh_token):
    """refresh_token → access_token（带 30min 缓存）。

    来源：glm2api glm_auth.py::_refresh_access_token
    POST {base}/user-api/user/refresh  Authorization: Bearer <refresh_token>
    → {"code":0,"data":{"access_token":..., "refresh_token":...}}
    """
    hit = _GLM_TOK.get(refresh_token)
    if hit and time.time() < hit[1]:
        return hit[0]
    headers = {
        "Authorization": "Bearer " + refresh_token,
        "Content-Type": "application/json",
        "User-Agent": UA_CHROME,
    }
    try:
        with _open(GLM_BASE + "/user-api/user/refresh", headers, 30,
                   data=b"{}", method="POST") as r:
            d = json.loads(r.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as e:
        return None, "刷新 token 失败 HTTP %s：%s" % (
            e.code, e.read().decode("utf-8", "replace")[:200])
    except Exception as e:
        return None, "刷新 token 异常：%s: %s" % (type(e).__name__, e)
    data = d.get("data") or {}
    at = data.get("access_token")
    if not at:
        return None, "刷新 token 未返回 access_token：%s" % str(d)[:200]
    _GLM_TOK[refresh_token] = (at, time.time() + 1800)
    return at, None


def glm_chat(messages, secret, model="glm-4-flash", timeout=180):
    """智谱清言网页版（chatglm.cn/chatglm/backend-api/assistant/stream）。"""
    if not secret:
        return False, "缺少 chatglm_refresh_token"
    at, err = _glm_access_token(secret)
    if not at:
        return False, err

    url = GLM_BASE + "/backend-api/assistant/stream"
    conv = [m for m in messages if m.get("role") in ("user", "assistant")]
    body = {
        "assistant_id": GLM_ASSISTANT_ID,
        "conversation_id": "",
        "messages": [{"role": m.get("role"), "content": m.get("content", "")}
                     for m in conv],
        "model": model,
        "stream": True,
        "prompt": "",
        "tools": [],
    }
    timestamp, nonce, sign = _glm_build_sign()
    headers = {
        "Authorization": "Bearer " + at,
        "Content-Type": "application/json",
        "Accept": "text/event-stream",
        "User-Agent": UA_CHROME,
        "X-Nonce": nonce,
        "X-Sign": sign,
        "X-Timestamp": timestamp,
    }
    try:
        with _open(url, headers, timeout,
                   data=json.dumps(body, ensure_ascii=False).encode("utf-8"),
                   method="POST") as r:
            parts = []

            def _on(_ev, data):
                if data in ("[DONE]", ""):
                    return
                try:
                    d = json.loads(data)
                except Exception:
                    return
                # chatglm assistant/stream：parts 是全量快照，最后一个有
                # content 的 part 即当前完整输出
                for p in (d.get("parts") or []):
                    if isinstance(p, dict) and p.get("content"):
                        parts.append(p["content"])
                    elif isinstance(p, str):
                        parts.append(p)
            _read_sse(r, _on)
            last = parts[-1] if parts else ""
            if not last:
                return False, "上游未返回可解析内容（签名或请求体不符）"
            return True, {"choices": [{"message": {"role": "assistant",
                                                   "content": last}}]}
    except urllib.error.HTTPError as e:
        return False, "HTTP %s: %s" % (e.code, e.read().decode("utf-8", "replace")[:300])
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, e)


# ==================================================================== Trae 直连
TRAE_BASE = "https://trae-api-cn.mchost.guru"          # ← 一手（dsh-trae-connect）
TRAE_CHAT_PATH = "/api/ide/v1/chat"
TRAE_APP_ID = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8"  # CN 网关要 UUID，932204 被拒
TRAE_IDE_VERSION = "1.2.10"
TRAE_IDE_VERSION_CODE = "20260401"
TRAE_MAP = {
    "claude-3-5-sonnet": "claude3.5",
    "claude-3-7-sonnet": "aws_sdk_claude37_sonnet",
    "gpt-4o": "gpt-4o",
    "gpt-4.1": "gpt-4.1-2025-04-14",
    "deepseek-chat": "deepseek-V3",
    "deepseek-v3": "deepseek-V3",
    "deepseek-r1": "deepseek-R1",
    "deepseek-reasoner": "deepseek-R1",
    "gemini-2.5-pro": "gemini-2.5-pro-preview-03-25",
    "gemini-2.5-flash": "gemini_2.5_flash",
}


def _trae_session_id(messages):
    """FNV-1a 32bit 哈希 → 'dsh-<base36>'，同会话稳定（保多轮上下文）。"""
    seed = "\x00".join("%s:%s" % (m.get("role", ""), m.get("content", ""))
                       for m in messages)
    h = 0x811c9dc5
    for ch in seed:
        h ^= ord(ch) & 0xFF
        h = (h * 0x01000193) & 0xFFFFFFFF
    return "dsh-%s" % _to_base36(h)


def _to_base36(n):
    if n == 0:
        return "0"
    digits = "0123456789abcdefghijklmnopqrstuvwxyz"
    out = []
    while n:
        n, r = divmod(n, 36)
        out.append(digits[r])
    return "".join(reversed(out))


def _trae_headers(secret):
    dev = "unknown"
    return {
        "Content-Type": "application/json",
        "x-app-id": TRAE_APP_ID,
        "x-ide-version": TRAE_IDE_VERSION,
        "x-ide-version-code": TRAE_IDE_VERSION_CODE,
        "x-ide-version-type": "stable",
        "x-device-cpu": "0",
        "x-device-id": dev,
        "x-machine-id": dev,
        "x-device-brand": "0",
        "x-device-type": "0",
        "x-ide-token": secret,
        "accept": "*/*",
        "Connection": "keep-alive",
        "User-Agent": "",
    }


def trae_chat(messages, secret, model="gpt-4o", timeout=180):
    """Trae 国内版直连（私有 chat 协议，非 OpenAI 格式）。"""
    if not secret:
        return False, "缺少 Trae x-ide-token"
    url = TRAE_BASE + TRAE_CHAT_PATH
    conv = [m for m in messages if m.get("role") in ("user", "assistant")]
    last = conv[-1].get("content", "") if conv else ""
    sid = _trae_session_id(conv)
    variables = {
        "language": "", "locale": "zh-cn", "input": last,
        "version_code": int(TRAE_IDE_VERSION_CODE),
        "is_inline_chat": False, "is_command": False,
        "raw_input": last, "problem": "", "current_filename": "",
        "is_select_code_before_chat": False,
        "last_select_time": int(time.time() * 1000),
        "last_turn_session": "", "hash_workspace": False,
        "hash_file": 0, "hash_code": 0, "use_filepath": True,
        "current_time": time.strftime("%Y/%m/%d %H:%M:%S"),
        "badge_clickable": True, "workspace_path": "",
        "brand": "Trae", "system_type": "Windows",
    }
    # 上一轮助手回复摘要（非空历史必填，否则轮次账本错乱）
    last_info = None
    for i in range(len(conv) - 2, -1, -1):
        if conv[i].get("role") == "assistant":
            last_info = {"turn": i, "is_error": False,
                         "response": conv[i].get("content", "")}
            break
    body = {
        "user_input": last,
        "intent_name": "general_qa_intent",
        "variables": json.dumps(variables, ensure_ascii=False),
        "context_resolvers": [
            {"resolver_id": "project-labels", "variables": '{"labels":""}'},
            {"resolver_id": "terminal_context", "variables": '{"terminal_context":[]}'},
        ],
        "generate_suggested_questions": False,
        "chat_history": [
            {"role": m.get("role"), "session_id": sid,
             "locale": "zh-cn" if m.get("role") == "assistant" else "",
             "content": m.get("content", ""), "status": "success"}
            for m in conv[:-1]
        ],
        "session_id": sid,
        "conversation_id": sid,
        "current_turn": max(0, len(conv) - 1),
        "valid_turns": list(range(max(0, len(conv) - 1))),
        "multi_media": [],
        "model_name": TRAE_MAP.get(model, model),
        "last_llm_response_info": last_info,
        "is_preset": True,
        "provider": "",
        "function": "chat",
    }
    try:
        with _open(url, _trae_headers(secret), timeout,
                   data=json.dumps(body, ensure_ascii=False).encode("utf-8"),
                   method="POST") as r:
            acc = []          # event: response 的 delta 是增量，必须累加
            reasoning = []
            state = {"type": "text"}

            def _on(ev, data):
                if data in ("[DONE]", ""):
                    return
                try:
                    d = json.loads(data)
                except Exception:
                    return
                if isinstance(d.get("response"), str) and d["response"]:
                    acc.append(d["response"])
                if isinstance(d.get("reasoning_content"), str) and d["reasoning_content"]:
                    reasoning.append(d["reasoning_content"])
            _read_sse(r, _on)
            last_text = "".join(acc)
            if not last_text:
                return False, "上游未返回可解析内容（请求体/签名不符，需抓包校准）"
            msg = {"role": "assistant", "content": last_text}
            if reasoning:
                msg["reasoning_content"] = "".join(reasoning)
            return True, {"choices": [{"message": msg}]}
    except urllib.error.HTTPError as e:
        return False, "HTTP %s: %s" % (e.code, e.read().decode("utf-8", "replace")[:300])
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, e)


# ==================================================================== DeepSeek 网页
DS_BASE = "https://chat.deepseek.com"
DS_UA = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
         "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")


def _ds_headers(secret):
    return {
        "accept": "*/*",
        "accept-language": "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7",
        "authorization": "Bearer " + secret,
        "content-type": "application/json",
        "origin": DS_BASE,
        "user-agent": DS_UA,
        "x-app-version": "20241129.1",
        "x-client-locale": "en_US",
        "x-client-platform": "web",
        "x-client-timezone-offset": "28800",
        "x-client-version": "1.7.0",
    }


def _ds_create_session(secret):
    """POST /api/v0/chat_session/create → data.biz_data.id；失败回退随机 uuid。"""
    try:
        with _open(DS_BASE + "/api/v0/chat_session/create", _ds_headers(secret), 20,
                   data=json.dumps({"character_id": None}).encode(), method="POST") as r:
            d = json.loads(r.read().decode("utf-8", "replace"))
            sid = ((d.get("data") or {}).get("biz_data") or {}).get("id")
            if sid:
                return sid
    except Exception:
        pass
    return str(uuid.uuid4())


def _pow_dir():
    """定位随包分发的 Node POW worker（源码目录 or _MEIPASS）。"""
    if getattr(sys, "frozen", False) and hasattr(sys, "_MEIPASS"):
        cands = [os.path.join(sys._MEIPASS, "app", "webpow"),
                 os.path.join(sys._MEIPASS, "webpow")]
    else:
        here = os.path.dirname(os.path.abspath(__file__))
        cands = [os.path.join(here, "webpow")]
    for c in cands:
        if os.path.isfile(os.path.join(c, "pow_worker.js")):
            return c
    return None


def _node_exe():
    """定位 node：先 PATH，再常见安装位置（打包后用户机 node 未必在 PATH）。"""
    for name in ("node", "node.exe"):
        p = shutil_which(name)
        if p:
            return p
    guesses = [
        r"C:\Program Files\nodejs\node.exe",
        r"C:\Program Files (x86)\nodejs\node.exe",
        os.path.expandvars(r"%LOCALAPPDATA%\Programs\nodejs\node.exe"),
        os.path.expandvars(r"%APPDATA%\nvm\current\node.exe"),
        os.path.expandvars(r"%ProgramFiles%\nodejs\node.exe"),
    ]
    for g in guesses:
        if g and os.path.isfile(g):
            return g
    return None


def shutil_which(name):
    for d in os.environ.get("PATH", "").split(os.pathsep):
        cand = os.path.join(d, name)
        if os.path.isfile(cand) and os.access(cand, os.X_OK):
            return cand
    return None


def _ds_solve_pow(challenge_cfg):
    """调随包 Node+WASM 解 POW challenge → answer(int)。找不到 node/wasm 返回 None。"""
    d = _pow_dir()
    node = _node_exe()
    if not d or not node:
        return None
    payload = {
        "challenge": challenge_cfg.get("challenge"),
        "salt": challenge_cfg.get("salt"),
        "difficulty": challenge_cfg.get("difficulty"),
        "expire_at": challenge_cfg.get("expire_at"),
    }
    try:
        proc = subprocess.run(
            [node, os.path.join(d, "pow_worker.js")],
            input=json.dumps(payload).encode(), stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, timeout=60)
        if proc.returncode != 0:
            return None
        res = json.loads(proc.stdout.decode("utf-8", "replace").strip())
        if "error" in res:
            return None
        return int(res["answer"])
    except Exception:
        return None


def _ds_fetch_pow(secret):
    """POST /api/v0/chat/create_pow_challenge → 挑战 dict（解不出返回 None）。"""
    try:
        with _open(DS_BASE + "/api/v0/chat/create_pow_challenge", _ds_headers(secret), 20,
                   data=json.dumps({"target_path": "/api/v0/chat/completion"}).encode(),
                   method="POST") as r:
            d = json.loads(r.read().decode("utf-8", "replace"))
            ch = (((d.get("data") or {}).get("biz_data") or {}).get("challenge") or {})
            if not ch.get("challenge"):
                return None
            ans = _ds_solve_pow(ch)
            if ans is None:
                return None
            return {
                "algorithm": ch.get("algorithm"),
                "challenge": ch.get("challenge"),
                "salt": ch.get("salt"),
                "answer": ans,
                "signature": ch.get("signature"),
                "target_path": ch.get("target_path"),
            }
    except Exception:
        return None


def deepseek_chat(messages, secret, model="deepseek-chat", timeout=180):
    """DeepSeek 网页版（需 POW + session，响应为 p/v/o 分片协议）。"""
    if not secret:
        return False, "缺少 DeepSeek userToken"
    prompt = messages[-1].get("content", "") if messages else ""
    session_id = _ds_create_session(secret)

    headers = _ds_headers(secret)
    pow_resp = _ds_fetch_pow(secret)
    if pow_resp:
        headers["x-ds-pow-response"] = base64.b64encode(
            json.dumps(pow_resp).encode()).decode()
    else:
        return False, ("POW 挑战求解失败：需随包 Node+wasm（app/webpow）。"
                       "确认系统有 node 且未被沙箱禁用。")

    body = {
        "chat_session_id": session_id,
        "parent_message_id": None,
        "prompt": prompt,
        "ref_file_ids": [],
        "thinking_enabled": ("reasoner" in model or "r1" in model.lower()
                             or "think" in model.lower()),
        "search_enabled": "search" in model.lower(),
        "preempt": False,
    }
    try:
        with _open(DS_BASE + "/api/v0/chat/completion", headers, timeout,
                   data=json.dumps(body, ensure_ascii=False).encode(), method="POST") as r:
            text = []
            cur = {"type": "text"}

            def _on(_ev, data):
                if data in ("[DONE]", ""):
                    return
                try:
                    d = json.loads(data)
                except Exception:
                    return
                # p/v/o 分片协议：{p, o, v}
                p = d.get("p")
                v = d.get("v")
                op = d.get("o")
                if p == "response/fragments" and op == "APPEND" and isinstance(v, list):
                    for frag in v:
                        if isinstance(frag, dict):
                            if frag.get("type") == "THINKING":
                                cur["type"] = "thinking"
                            else:
                                cur["type"] = "text"
                            c = frag.get("content")
                            if c and cur["type"] == "text":
                                text.append(c)
                    return
                if p == "response/fragments/-1/content" and isinstance(v, str):
                    if cur["type"] == "text":
                        text.append(v)
                    return
                # 兜底：标准 choices 增量
                for ch in (d.get("choices") or []):
                    c = (ch.get("delta") or {}).get("content")
                    if c:
                        text.append(c)
            _read_sse(r, _on)
            last = "".join(text)
            if not last:
                return False, "上游未返回可解析内容（POW/凭据可能失效）"
            return True, {"choices": [{"message": {"role": "assistant",
                                                   "content": last}}]}
    except urllib.error.HTTPError as e:
        return False, "HTTP %s: %s" % (e.code, e.read().decode("utf-8", "replace")[:300])
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, e)


# ==================================================================== 调度入口
WEB_RELAY_PIDS = {"web-glm", "web-trae", "web-deepseek"}


def chat(pid, messages, secret, model=None):
    if pid == "web-glm":
        return glm_chat(messages, secret, model or "glm-4-flash")
    if pid == "web-trae":
        return trae_chat(messages, secret, model or "gpt-4o")
    if pid == "web-deepseek":
        return deepseek_chat(messages, secret, model or "deepseek-chat")
    return False, "未知网页反代平台 %s" % pid


def model_list(pid):
    """静态模型清单（对话写 模型id@pid）。"""
    if pid == "web-glm":
        return [
            {"id": "glm-4-flash", "name": "GLM-4 Flash", "desc": "智谱清言网页版（免费快）"},
            {"id": "glm-4", "name": "GLM-4", "desc": "智谱清言网页版"},
            {"id": "glm-4-plus", "name": "GLM-4 Plus", "desc": "智谱清言网页版（增强）"},
            {"id": "glm-4-air", "name": "GLM-4 Air", "desc": "智谱清言网页版"},
            {"id": "glm-4-all", "name": "GLM-4 All", "desc": "智谱清言网页版（All 工具）"},
            {"id": "glm-zero-preview", "name": "GLM Zero Preview", "desc": "智谱清言网页版（深度思考）"},
        ]
    if pid == "web-trae":
        return [
            {"id": "gpt-4o", "name": "GPT-4o", "desc": "Trae 直连"},
            {"id": "claude-3-7-sonnet", "name": "Claude 3.7 Sonnet", "desc": "Trae 直连"},
            {"id": "claude-3-5-sonnet", "name": "Claude 3.5 Sonnet", "desc": "Trae 直连"},
            {"id": "deepseek-v3", "name": "DeepSeek V3", "desc": "Trae 直连"},
            {"id": "deepseek-r1", "name": "DeepSeek R1", "desc": "Trae 直连（推理）"},
        ]
    if pid == "web-deepseek":
        return [
            {"id": "deepseek-chat", "name": "DeepSeek Chat", "desc": "DeepSeek 网页版"},
            {"id": "deepseek-reasoner", "name": "DeepSeek Reasoner", "desc": "DeepSeek 网页版（R1 深度思考）"},
        ]
    return []
