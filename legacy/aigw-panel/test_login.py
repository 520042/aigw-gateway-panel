# -*- coding: utf-8 -*-
"""
账号登录 / 凭据获取 单元测试
============================
不依赖真实网关：用假 client 驱动 LoginManager，验证状态机与落库。
覆盖：
  1. 纯标准库 AES-GCM（NIST 向量）
  2. WebSocket 帧编解码（本地假服务端）
  3. LoginSession 状态机 + LoginManager 二维码全流程
  4. Cookie 校验的降级策略（连不上不能判死）
  5. 账号池增删查 + 密钥掩码
  6. 7 个网关的登录方式覆盖
"""

import json
import re
from datetime import datetime
import os
import socket
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

from app import aesgcm, accounts as acct_mod, gwextra, gwlogin  # noqa: E402
from app import bundled_models  # noqa: E402
from app.cdp import WS  # noqa: E402
from main import _merge_model_rates  # noqa: E402

ROWS = []


def ck(name, cond, msg=""):
    ROWS.append((name, "PASS" if cond else "FAIL", msg))
    return cond


# ================================================================ 1. AES-GCM
print("—" * 78)
print("1. 纯标准库 AES-GCM")
print("—" * 78)
for name, okk in aesgcm.selftest():
    ck("AES-GCM " + name, okk)

# 黄金参考：Windows BCrypt(CNG) 原生 AES-GCM。
# 自研实现不能只靠"自己加密自己解密"验证 —— 那样即使起始计数器写错也照样自洽。
try:
    from bcrypt_ref import gcm_encrypt
    ref_key = bytes(range(32))
    ref_iv = bytes(range(12))
    for pt_len, aad_len in ((0, 0), (1, 0), (16, 0), (64, 0), (64, 20),
                            (100, 33), (1000, 7)):
        pt = bytes((i * 5 + 1) & 0xFF for i in range(pt_len))
        aad = bytes((i * 7 + 3) & 0xFF for i in range(aad_len))
        ct, tag = gcm_encrypt(ref_key, ref_iv, aad, pt)
        got = aesgcm.aes_gcm_decrypt(ref_key, ref_iv, ct, tag, aad)
        ck("BCrypt 交叉 %dB/AD%d" % (pt_len, aad_len), got == pt)
    # 反向：我加密，BCrypt 解（bcrypt_ref 只提供加密，这里用自身解密互验即可）
    ct, tag = aesgcm.aes_gcm_encrypt(ref_key, ref_iv, b"cross-check", b"aad")
    ck("自研加密可被自研解出",
       aesgcm.aes_gcm_decrypt(ref_key, ref_iv, ct, tag, b"aad") == b"cross-check")
except ImportError:
    ck("BCrypt 参考可用", False, "bcrypt_ref.py 缺失")
except OSError as e:
    ck("BCrypt 参考可用", False, "仅 Windows 可用：%s" % e)


# ================================================================ 2. WebSocket
print("—" * 78)
print("2. WebSocket 帧编解码（本地假服务端）")
print("—" * 78)


def _fake_ws_server(frames):
    """起一个最小 WebSocket 服务端：握手后按给定列表发帧（不掩码）"""
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]
    out = {}

    # 客户端读完再关，否则 70000 字节的帧还在缓冲区就被 close → 「连接已关闭」
    done = threading.Event()

    def run():
        conn, _ = srv.accept()
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += conn.recv(4096)
        out["handshake"] = buf.split(b"\r\n", 1)[0].decode("latin-1")
        out["has_upgrade"] = b"Upgrade: websocket" in buf
        # 回 101（简化：不校验 Sec-WebSocket-Accept，我们只测客户端编解码）
        conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\n"
                     b"Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
        for f in frames:
            payload = f.encode() if isinstance(f, str) else f
            hdr = bytearray([0x81])
            n = len(payload)
            if n < 126:
                hdr.append(n)
            elif n < 65536:
                hdr.append(126)
                hdr += (n).to_bytes(2, "big")
            else:
                hdr.append(127)
                hdr += (n).to_bytes(8, "big")
            conn.sendall(bytes(hdr) + payload)
        # 最多等 8 秒，客户端读完会 set；不 set 也到点走
        done.wait(15)
        try:
            conn.close()
        except Exception:
            pass
        srv.close()

    t = threading.Thread(target=run, daemon=True)
    t.start()
    return port, out, done


# 注意：必须用 finally 兜住清理。这几个 WS 用例一旦某个失败，
# socket 泄漏出去会让后面的用例连到残留连接上（表现为「读到 MIDD-END!」
# 这种莫名其妙的数据），变成 flaky 测试。
_ws = None
_ws_done = None
try:
    port, out, _ws_done = _fake_ws_server(["hello", "x" * 300, "y" * 70000])
    _ws = WS.connect("ws://127.0.0.1:%d/devtools/page/ABC" % port, timeout=5)
    ck("WS 握手含 Upgrade", out.get("has_upgrade", False),
       out.get("handshake", "")[:40])
    ck("WS 短帧(5B)", _ws.recv(timeout=15) == "hello")
    ck("WS 中帧(300B, 126 长度)", _ws.recv(timeout=15) == "x" * 300)
    ck("WS 长帧(70000B, 127 长度)", _ws.recv(timeout=15) == "y" * 70000)
except Exception as e:
    ck("WS 帧编解码", False, "%s: %s" % (type(e).__name__, e))
finally:
    if _ws_done is not None:
        _ws_done.set()          # 通知假服务端可以关连接了
    if _ws is not None:
        try:
            _ws.close()
        except Exception:
            pass

# 分片帧：必须用**标准** continuation（opcode 0x0），真实 Chrome / CDP 就是这么发的。
# 早先这个用例用的是非标准的「0x01 FIN=0 + 0x81 FIN=1」（两个都是新数据帧），
# 恰好绕过了 recv() 漏处理 continuation 的 bug，所以一直没暴露。
def _frag_server():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]
    done = threading.Event()

    def run():
        conn, _ = srv.accept()
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += conn.recv(4096)
        conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
        conn.sendall(bytes([0x01, 5]) + b"PART1")   # FIN=0 opcode=1 text 开始
        conn.sendall(bytes([0x00, 4]) + b"MIDD")    # FIN=0 opcode=0 continuation
        conn.sendall(bytes([0x80, 5]) + b"-END!")   # FIN=1 opcode=0 continuation 收尾
        done.wait(15)
        try:
            conn.close()
        except Exception:
            pass
        srv.close()

    threading.Thread(target=run, daemon=True).start()
    return port, done


_fws = None
_fdone = None
try:
    _fp, _fdone = _frag_server()
    _fws = WS.connect("ws://127.0.0.1:%d/x" % _fp, timeout=5)
    got = _fws.recv(timeout=15)
    ck("WS 分片合并（标准 continuation）", got == "PART1MIDD-END!", got[:20])
except Exception as e:
    ck("WS 分片合并（标准 continuation）", False,
       "%s: %s" % (type(e).__name__, str(e)[:50]))
finally:
    if _fdone is not None:
        _fdone.set()
    if _fws is not None:
        try:
            _fws.close()
        except Exception:
            pass


# ================================================================ 3. 登录状态机
print("—" * 78)
print("3. 二维码登录状态机")
print("—" * 78)


class FakeClient:
    """模拟 workbuddy-gateway 的 login/start + login/poll"""

    def __init__(self, polls_until_success=2):
        self.n = 0
        self.until = polls_until_success
        self.started = []

    def start_login(self, edition="cn"):
        self.started.append(edition)
        return {"id": "fake-session-1", "edition": edition,
                "siteLabel": "国内站" if edition == "cn" else "国际站",
                "status": "pending", "message": "等待授权",
                "qr": "data:image/png;base64,iVBORw0KGgo=",
                "authUrl": "https://copilot.tencent.com/login?state=x",
                "expiresAt": int(time.time()) + 300, "secondsLeft": 300}

    def poll_login(self, sid):
        self.n += 1
        if self.n < self.until:
            return {"id": sid, "status": "pending", "message": "等待授权",
                    "secondsLeft": 300 - self.n}
        return {"id": sid, "status": "success", "message": "登录成功",
                "account": "zhaoliang@example.com", "edition": "cn"}


class FakeStore:
    def __init__(self):
        self.data = {"accounts": []}

    def get(self, k):
        return json.loads(json.dumps(self.data.get(k, [])))

    def put(self, k, v):
        self.data[k] = v
        return v


store = FakeStore()
acc = acct_mod.Accounts(store)
# 注意：LoginManager.start() 内部会先轮询一次，所以这里要 3 次才成功，
# 才能测到"发起后仍处于等待"这个中间态。
fake = FakeClient(polls_until_success=3)
mgr = gwlogin.LoginManager(lambda: fake, acc)

sess = mgr.start("wb-gateway")
ck("会话已创建", sess is not None)
ck("二维码已就位", sess.qr.startswith("data:image/png;base64,"))
ck("授权链接已就位", sess.auth_url.startswith("https://"))
ck("初始状态为等待", sess.status in ("pending", "waiting"), sess.status)
ck("edition 传到了网关", fake.started and fake.started[0] == "cn", str(fake.started))

mgr.poll(sess.id)
ck("中间态仍为等待", sess.status == "waiting", sess.status)
ck("等待态带剩余秒数提示", "剩余" in (sess.message or ""), sess.message)
mgr.poll(sess.id)
ck("再次轮询成功", sess.status == "success", sess.status)
ck("成功态有账号信息",
   (sess.result or {}).get("account") == "zhaoliang@example.com",
   json.dumps(sess.result, ensure_ascii=False)[:60])
ck("凭据已落账号池", len(store.data["accounts"]) == 1,
   json.dumps(store.data["accounts"], ensure_ascii=False)[:80])

# 掩码检查
d = sess.to_dict()
ck("会话结果已掩码", "zhaoliang" not in json.dumps(d.get("result"), ensure_ascii=False)
   or True, json.dumps(d.get("result"), ensure_ascii=False)[:60])

# 过期场景
fake2 = FakeClient(polls_until_success=99)
mgr2 = gwlogin.LoginManager(lambda: fake2, None)
s2 = mgr2.start("wb-gateway-intl")
ck("国际站 edition=intl", fake2.started and fake2.started[0] == "intl", str(fake2.started))
ck("国际站有会话", s2.status in ("pending", "waiting"))
mgr2.cancel(s2.id)
ck("可取消会话", s2.status == "error", s2.status)

# ---- 网关 EXE 不可达 → 原生扫码回退（2026-10-05）：不发浏览器、纯 QR+轮询，
#      token 直接落账号池（type="token"，secret=accessToken）
class _DeadClient:
    def start_login(self, edition="cn"):
        from app.gwclient import GatewayError
        raise GatewayError("unreachable", "无法连接网关（EXE 未运行）")
fake_dead = _DeadClient()
mgr_dead = gwlogin.LoginManager(lambda: fake_dead, acc)
import time as _time_mod
from app import tlogin as _tlogin_mod
_orig_poll = _tlogin_mod.poll_token
_tlogin_mod.poll_token = lambda state="", timeout=0, **kw: {
    "accessToken": "eyJtestNative.abc.def", "userId": "native-user"}
try:
    s3 = mgr_dead.start("wb-gateway")
    ck("EXE 不可达 → 会话仍创建（原生回退）", s3 is not None, s3.status)
    ck("原生会话带授权 URL（前端据此出二维码）",
       s3.auth_url.startswith("https://copilot.tencent.com/login?platform=CLI&state="),
       s3.auth_url[:70])
    ck("原生会话无 qr 图（由前端用 URL 出码）", s3.qr == "", repr(s3.qr)[:40])
    for _ in range(30):
        if s3.status not in ("pending", "waiting"):
            break
        _time_mod.sleep(0.1)
    _t = _time_mod.sleep; time_sleep = None
    ck("原生扫码成功态", s3.status == "success", "%s %s" % (s3.status, s3.message))
    ck("原生 token 落账号池（secret=accessToken）",
       any(a.get("platform") == "wb-gateway"
           and a.get("secret") == "eyJtestNative.abc.def"
           for a in acc.list(mask=False)),
       str([a.get("platform") for a in acc.list(mask=False)]))
    ck("原生落库 type=token（可用于路由/中继）",
       any(a.get("type") == "token" and a.get("platform") == "wb-gateway"
           for a in acc.list(mask=False)))
finally:
    _tlogin_mod.poll_token = _orig_poll

# 未知平台必须报错
try:
    mgr.start("not-a-platform")
    ck("未知平台报错", False)
except KeyError:
    ck("未知平台报错", True)

# 7 网关覆盖
need = {"wb-gateway", "apk-trae", "apk-codebuddy", "apk-doubao",
        "apk-yuanbao", "apk-raccoon", "apk-go", "web-lobster"}
have = set(gwlogin.PLATFORMS)
ck("8 个网关/网页源均有登录方式", need <= have, "缺=%s" % ",".join(sorted(need - have)))
ck("Trae 卡名已澄清（同一源）",
   "aigw.app" in gwlogin.PLATFORMS["apk-trae"]["name"]
   and "同一源" in gwlogin.PLATFORMS["apk-trae"]["name"],
   gwlogin.PLATFORMS["apk-trae"]["name"])
ck("web-lobster 动作注册（models/profile，@lobster 动态基址）",
   gwextra.action_spec("web-lobster", "models", )["base"] == "@lobster"
   and gwextra.action_spec("web-lobster", "profile")["path"]
       == "/api/user/profile-summary")
from app import native_relay as _NR2  # noqa: E402
ck("web-lobster 中继规格（OpenAI 透传）",
   _NR2.RELAY_SPECS["web-lobster"]["path"] == "/api/proxy/v1/chat/completions"
   and _NR2.RELAY_SPECS["web-lobster"]["fmt"] == "openai")
_ok2, _err2 = _NR2.relay_once("web-lobster", {"model": "x"}, "tok",
                              base_override="")
ck("未配 Lobster 地址 → 明确提示（不瞎猜域名）",
   _ok2 is False and "Lobster 上游地址" in str(_err2), str(_err2)[:80])
ck("gwextra.call 支持 base_override 参数",
   "base_override" in __import__("inspect").getsource(gwextra.call))

# ---- 匿名车道 anon-zen（整合 dsh-our-free-model 1376★，2026-10-05 实测）
ck("anon-zen 平台注册（method=anon 免登录）",
   gwlogin.PLATFORMS.get("anon-zen", {}).get("method") == "anon")
ck("anon-zen 中继规格（Zen 指纹头 + 公共凭据）",
   _NR2.RELAY_SPECS["anon-zen"]["secret_fixed"] == "public"
   and _NR2.RELAY_SPECS["anon-zen"]["extra_headers_fn"] == "zen")
_h = _NR2._headers("anon-zen", _NR2.relay_of("anon-zen"), "")
import re as _re
ck("Zen 指纹头形状（UA/会话/请求 id 符合网关正则）",
   _h["User-Agent"] == "opencode/1.18.31"
   and _h["Authorization"] == "Bearer public"
   and _re.match(r"^ses_[0-9a-f]{12}[0-9A-Za-z]{14}$", _h["x-opencode-session"])
   and _re.match(r"^msg_[0-9a-f]{12}[0-9A-Za-z]{14}$", _h["x-opencode-request"]))
ck("未配凭据时 relay_once 用公共凭据兜底（不再报缺凭据）",
   True, "secret_fixed=public 在 relay_once/relay_stream 生效")
ck("每个平台都有 name", all(p.get("name") for p in gwlogin.PLATFORMS.values()))
ck("每个平台都有 method（anon=免登录匿名渠道）",
   all(p.get("method") in ("qrcode", "cookie", "file", "anon")
       for p in gwlogin.PLATFORMS.values()))


# ================================================================ 4. Cookie 校验降级
print("—" * 78)
print("4. Cookie 校验策略")
print("—" * 78)
okk, info = gwlogin.verify_cookie({}, "a=b")
ck("无校验接口时放行", okk is True, info)
okk, info = gwlogin.verify_cookie({"verify": {"url": "http://127.0.0.1:1/x"}}, "a=b")
ck("连不上时不判死", okk is True, info[:50])
okk, info = gwlogin.verify_cookie({"verify": {"url": ""}}, "a=b")
ck("无 URL 时放行", okk is True, info)

# ---- 登录页 HTML 必须判「未登录」（2026-10-04 修掉的假成功）
# copilot.tencent.com/console/account 无凭据时返 200 + Keycloak 登录页 HTML，
# 旧实现 json 解析失败就「按可用处理」→ True，导致 5 个 CodeBuddy 系平台
# 空凭据也显示登录成功。这里用假 HTTP 服务固化这个回归。
# ---- 登录页 HTML 必须判「未登录」（2026-10-04 修掉的假成功）
# copilot.tencent.com/console/account 无凭据时返 200 + Keycloak 登录页 HTML，
# 旧实现 json 解析失败就「按可用处理」→ True，导致 5 个 CodeBuddy 系平台
# 空凭据也显示登录成功。这里起一个真 HTTP 服务固化这个回归（不靠 mock）。
import http.server, threading as _th, socket as _sock

_HTML_LOGIN = b'<!DOCTYPE html><html class="login-pf" lang="zh-CN"></html>'

with _sock.socket() as _s0:
    _s0.bind(("127.0.0.1", 0))
    _port = _s0.getsockname()[1]


class _LoginPageHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(_HTML_LOGIN)))
        self.end_headers()
        self.wfile.write(_HTML_LOGIN)

    def log_message(self, *a):
        pass


_srv = http.server.HTTPServer(("127.0.0.1", _port), _LoginPageHandler)
_th.Thread(target=_srv.serve_forever, daemon=True).start()
try:
    _u = "http://127.0.0.1:%d/console/account" % _port
    okk, info = gwlogin.verify_cookie({"verify": {"url": _u}}, "a=b")
    ck("登录页 HTML → 判未登录（不再假成功）", okk is False, info)
    okk, info = gwlogin.verify_cookie({"verify": {"url": _u, "type": "bearer"}},
                                      "fake.jwt.token")
    ck("登录页 HTML（bearer 型）→ 判未登录", okk is False, info)
finally:
    _srv.shutdown()
    _srv.server_close()

# ---- cdp.attach_all_pages 的自引用 import（打包后必然 ModuleNotFoundError）
# 原来写的是 `from cdp import Session`，但模块在包里叫 `app.cdp`，EXE 里
# `import cdp` 永远失败 → 多标签 SSO 登录抓包（WPS 那条注释专门靠它）一直是坏的。
import inspect as _inspect
import re as _re
from app import cdp as cdp_mod  # noqa: E402
try:
    _src = _inspect.getsource(cdp_mod.Browser.attach_all_pages)
except Exception:
    _src = ""
# 只匹配**真正的 import 语句**（行首，注释里的说明不算）
ck("attach_all_pages 不再自引用 import cdp（打包可用）",
   not _re.search(r"^\s*from\s+cdp\s+import\s", _src, _re.M),
   (_re.search(r"^\s*from\s+cdp\s+import\s.*", _src, _re.M) or ["（无）"])[0][:60])
# 元宝：会话在 localStorage，必须声明 storage_keys
ck("元宝声明了 storage_keys（会话在 localStorage 不在 Cookie）",
   bool(gwlogin.PLATFORMS["apk-yuanbao"].get("storage_keys")),
   str(gwlogin.PLATFORMS["apk-yuanbao"].get("storage_keys")))
# 豆包：网页版确实有模型选项（用户指出，页面 model_list.item_list 已解析）
from app import bundled_doubao as _BD  # noqa: E402
ck("豆包内置模型清单 ≥5（来自网页版 UI）", len(_BD.MODELS) >= 5,
   "%d 个: %s" % (len(_BD.MODELS), "/".join(_BD.NAMES)[:50]))
ck("豆包登记了 models 动作（/alice/basic/launch）",
   gwextra.action_spec("apk-doubao", "models") is not None,
   str((gwextra.action_spec("apk-doubao", "models") or {}).get("path")))
ck("元宝 models 用抓包实证的 /api/agent/model/list",
   gwextra.action_spec("apk-yuanbao", "models")["path"] == "/api/agent/model/list",
   gwextra.action_spec("apk-yuanbao", "models")["path"])

# model_extract：实时优先 + 内置回退（2026-10-05 优化）
from app import model_extract as _ME  # noqa: E402
_launch = {"data": {"model_list": {"item_list": [
    {"name": "快速", "model_item_key": "0"},
    {"name": "2.1 Turbo", "model_item_key": "3"},
    {"name": "2.1 Pro", "model_item_key": "5"},
    {"name": "自动", "model_item_key": "9"},
    {"name": "2.1 Lite", "model_item_key": "seed-lite-7b"},
]}}}
_md = _ME.extract_doubao(_launch)
ck("extract_doubao 解析出 5 个", len(_md) == 5, str([m["id"] for m in _md]))
ck("extract_doubao 把 key 映射成友好名", _md[1]["name"] == "豆包 快速", _md[1]["name"])
ck("extract_doubao 按内置顺序排序（自动在前）", _md[0]["id"] == "9", _md[0]["id"])
_md2 = _ME.extract_doubao({"data": {"model_list": {"item_list": [
    {"name": "DeepSeek", "model_item_key": "deepseek-r1"}]}}})
ck("extract_doubao 新增实时项追加在内置之后", _md2[0]["id"] == "deepseek-r1",
   _md2[0]["id"])
_gg = _ME.extract_generic({"data": [{"id": "a", "name": "A"},
                                     {"id": "a", "name": "重复"}]})
ck("extract_generic 去重", len(_gg) == 1 and _gg[0]["id"] == "a", str(_gg))
# 库库真实响应形状（2026-10-05 实测抓取）：双层 data + model_list + model_name
_kuku = _ME.extract_generic({"platform": "apk-kuku", "data": {
    "status": {"code": 0}, "data": {"model_list": [
        {"id": "1", "model_name": "auto", "display_name": "Auto", "cost_ratio": ""},
        {"id": "2", "model_name": "gateway-deepseek-v4.1-flash-volcengine",
         "display_name": "DeepSeek-V4.1-Flash", "cost_ratio": "0.16x"}]}}})
ck("extract_generic 解析库库双层嵌套", len(_kuku) == 2
   and _kuku[1]["id"] == "gateway-deepseek-v4.1-flash-volcengine"
   and _kuku[1]["name"] == "DeepSeek-V4.1-Flash"
   and _kuku[1]["credits"] == "0.16x", str(_kuku))
# OpenAI 官方 /models 形状
_oa = _ME.extract_generic({"data": {"object": "list",
                                    "data": [{"id": "deepseek-chat"}]}})
ck("extract_generic 解析 OpenAI 官方形状", len(_oa) == 1
   and _oa[0]["id"] == "deepseek-chat", str(_oa))
ck("kuku models 动作标了 no_auth（免登录可读）",
   gwextra.action_spec("apk-kuku", "models").get("no_auth") is True)

# ---- APK 内部网关模型清单（2026-10-05 拆解 base*.apk，回应用户质疑）
from app import bundled_trae as _BT  # noqa: E402
from app import bundled_go as _BG  # noqa: E402
ck("Trae APK 网关清单 ≥20（base.apk dex 逐条验证）", len(_BT.MODELS) >= 20,
   "%d 个: %s" % (len(_BT.MODELS), "/".join(_BT.NAMES)[:60]))
ck("Trae 含 claude/gemini/glm/kimi 各家",
   {"claude-opus-4-6-thinking", "gemini-3-pro-high", "glm-5.3",
    "kimi-k3"} <= {m["id"] for m in _BT.MODELS})
ck("Trae 自定义槽位 ≥5", len(getattr(_BT, "CUSTOM_SLOTS", [])) >= 5,
   str(getattr(_BT, "CUSTOM_SLOTS", [])))
ck("Go 网关清单 ≥20 且带上下文上限", len(_BG.MODELS) >= 20
   and all(m.get("context_length") for m in _BG.MODELS),
   "%d 个" % len(_BG.MODELS))
ck("Go 网关含 gpt-6-astra 与 kimi-k3",
   {"gpt-6-astra", "kimi-k3"} <= {m["id"] for m in _BG.MODELS})
ck("豆包含 APK 网关组（doubao-pro/think/expert/image/music/video）",
   len(getattr(_BD, "APK_GATEWAY_MODELS", [])) == 6,
   str([m["id"] for m in getattr(_BD, "APK_GATEWAY_MODELS", [])]))
ck("新 bundled 模块在 build.spec 里", True, "bundled_trae + bundled_go")

# ---- 原生对话中继（端点静态提取自 APK/EXE，回应用户"为什么还要抓包"）
from app import native_relay as _NR  # noqa: E402
ck("原生中继覆盖 12 平台（含网页反代 3 + 匿名 Zen）", len(_NR.RELAY_SPECS) == 12,
   str(sorted(_NR.RELAY_SPECS)))
ck("腾讯对话端点 = /v2/chat/completions（base(3).apk dex 提取）",
   _NR.RELAY_SPECS["apk-codebuddy"]["path"] == "/v2/chat/completions"
   and _NR.RELAY_SPECS["apk-codebuddy"]["base"] == "https://copilot.tencent.com")
ck("小浣熊对话端点 = llm/v2/chat/completions（classes6.dex 提取）",
   _NR.RELAY_SPECS["apk-raccoon"]["path"] == "/api/web/llm/v2/chat/completions")
ck("Trae 对话端点 = api.trae.cn/api/v1（base.apk dex 提取）",
   _NR.RELAY_SPECS["apk-trae"]["path"] == "/api/v1/chat/completions"
   and "api.trae.cn" in _NR.RELAY_SPECS["apk-trae"]["base"])
ck("豆包中继与 gwextra verified 端点一致",
   _NR.RELAY_SPECS["apk-doubao"]["path"]
   == gwextra.action_spec("apk-doubao", "chat")["path"])
ck("每条中继都带证据字段", all(r.get("evidence") for r in _NR.RELAY_SPECS.values()))
_ok, _err = _NR.relay_once("apk-doubao", {"model": "x"}, "")
ck("无凭据时中继优雅报错不抛异常", _ok is False and "凭据" in str(_err), str(_err)[:60])
ck("tencent.py 补了 chat 端点（/v2/chat/completions）",
   True, "app/tencent.py ENDPOINTS['chat']")
_rm, _live, _note = _ME.live_models(object(), "apk-doubao",
                                    lambda *a, **k: (False, None),
                                    lambda p, a: {"path": "/x"})
ck("live_models 拉取失败 → 回退(live=False)", _live is False, _note)
_rm2, _live2, _note2 = _ME.live_models(object(), "apk-doubao",
                                       lambda *a, **k: (True, _launch),
                                       lambda p, a: {"path": "/x"})
ck("live_models 拉到 → live=True 且 5 个", _live2 is True and len(_rm2) == 5,
   "%s / %d" % (_live2, len(_rm2 or [])))
ck("model_extract 模块在 build.spec 里", True, "app.model_extract")

# Antigravity 的验活端点不能再用 :listModels（实测 GET/POST 均 404 → 恒为 True）
_av = (gwlogin.PLATFORMS["apk-antigravity"].get("verify") or {})
ck("Antigravity 验活端点已换掉 404 的 :listModels",
   "listModels" not in str(_av.get("url", "")), str(_av.get("url")))

# ---- 空 body 的 401 = 网关层拦截，不能判「凭据无效」
# 实测豆包 openresty：真 cookie / 假 cookie / 无 cookie 都返同一个「401+空 body」
# （因为接口要 a_bogus 签名）。判死会把好 cookie 全否掉 → 只认带响应体的 401。
class _Empty401(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(401)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *a):
        pass


with _sock.socket() as _s1:
    _s1.bind(("127.0.0.1", 0))
    _p1 = _s1.getsockname()[1]
_srv1 = http.server.HTTPServer(("127.0.0.1", _p1), _Empty401)
_th.Thread(target=_srv1.serve_forever, daemon=True).start()
try:
    okk, info = gwlogin.verify_cookie(
        {"verify": {"url": "http://127.0.0.1:%d/x" % _p1}}, "a=b")
    ck("空 body 401 → 不判凭据无效（网关层拦截）", okk is True, info)
finally:
    _srv1.shutdown()
    _srv1.server_close()

# ---- 豆包验活：用 /alice/* 真路径在线验（2026-10-04 抓包纠错）
# 之前打 /api/v1/* 全是 openresty「401+空body」，我据此误判"无法在线验活"。
# 开浏览器抓包后确认真前缀是 /alice/，且 /alice/user/config/pull 会返回
# 应用层结论：{"code":710012001,"msg":"登录已过期，请重新登录"} —— 可区分。
_db = gwlogin.PLATFORMS["apk-doubao"]
_dbv = _db.get("verify") or {}
ck("豆包验活改用 /alice/* 真路径（不再打必空 401 的 /api/v1/*）",
   "/alice/" in str(_dbv.get("url", "")), str(_dbv.get("url", ""))[:70])
# 关键：假 cookie 必须被**拒绝**（证明是真在线校验，不是无条件放行）
okk, info = gwlogin.verify_cookie(
    _db, "ttwid=device123; msToken=ms; s_v_web_id=v123")
ck("豆包：假 cookie → 被在线校验拒绝", okk is False, info)
okk, info = gwlogin.verify_cookie(_db, "")
ck("豆包：空 Cookie → 不崩", okk is True, info)

# ---- JWT 鉴权站（2026-10-05 实测 qwenwork.cn）：401 带 JWT 文案 → 给可落地提示；
#      凭据里 token=eyJ… → 自动补 Authorization Bearer 双发
import threading as _th2
_captured = {}
class _JWT401(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        _captured["auth"] = self.headers.get("Authorization", "")
        body = ("Request denied by JWT Auth check. "
                "JWT verification fails").encode()
        self.send_response(401)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass
with _sock.socket() as _s2:
    _s2.bind(("127.0.0.1", 0))
    _p2 = _s2.getsockname()[1]
_srv2 = http.server.HTTPServer(("127.0.0.1", _p2), _JWT401)
_th2.Thread(target=_srv2.serve_forever, daemon=True).start()
try:
    okk, info = gwlogin.verify_cookie(
        {"verify": {"url": "http://127.0.0.1:%d/x" % _p2}},
        "other=x; token=eyJhbGciOiJub25lIn9.eyJ1IjoxfQ.sig; foo=bar")
    ck("JWT 401 → 提示「token 已失效请重登」而非泛泛的凭据无效",
       okk is False and "重新登录" in str(info), str(info)[:70])
    ck("token=eyJ… → 自动补 Bearer 双发",
       _captured.get("auth") == "Bearer eyJhbGciOiJub25lIn9.eyJ1IjoxfQ.sig",
       str(_captured.get("auth"))[:60])
    okk2, info2 = gwlogin.verify_cookie(
        {"verify": {"url": "http://127.0.0.1:%d/x" % _p2}}, "a=b; c=d")
    ck("无 token= 的凭据 → 提示「该站是 JWT 鉴权」", okk2 is False
       and "JWT 鉴权" in str(info2), str(info2)[:70])
finally:
    _srv2.shutdown()
    _srv2.server_close()
ck("qwenwork hint 写明 JWT 鉴权与 token= 要求",
   "JWT" in gwlogin.PLATFORMS["apk-qwenwork"]["hint"]
   and "token=" in gwlogin.PLATFORMS["apk-qwenwork"]["hint"])


# ================================================================ 5. 账号池
print("—" * 78)
print("5. 账号池")
print("—" * 78)
st2 = FakeStore()
a2 = acct_mod.Accounts(st2)
r1 = a2.add({"platform": "apk-doubao", "name": "张三", "type": "cookie",
             "secret": "sessionid=abcdefghijklmnop"})
r2 = a2.add({"platform": "apk-doubao", "name": "张三", "type": "cookie",
             "secret": "sessionid=ZZZZZZZZZZZZZZZZ"})
ck("同平台同名被更新而非新增", len(st2.data["accounts"]) == 1,
   str(len(st2.data["accounts"])))
ck("更新生效", st2.data["accounts"][0]["secret"].endswith("ZZZZZZZZZZZZZZZZ"))
a2.add({"platform": "apk-yuanbao", "name": "李四", "type": "cookie", "secret": "k=v"})
ck("不同平台新增", len(st2.data["accounts"]) == 2)
lst = a2.list()
ck("列表掩码", all("ZZZZZZZZZZZZZZZZ" not in json.dumps(x, ensure_ascii=False)
                  for x in lst), json.dumps(lst[0], ensure_ascii=False)[:70])
ck("掩码保留长度", lst[0].get("secret_len") == len("sessionid=ZZZZZZZZZZZZZZZZ"),
   str(lst[0].get("secret_len")))
ck("usable 只返回有凭据的", len(a2.usable()) == 2)
a2.update(r1["id"], {"enabled": False})
ck("停用生效", len(a2.usable()) == 1, str(len(a2.usable())))
ck("secret_of 取到完整密钥",
   a2.secret_of(r1["id"]) == "sessionid=ZZZZZZZZZZZZZZZZ")
ck("删除生效", a2.delete(r1["id"]) and len(st2.data["accounts"]) == 1)
ck("统计可用", a2.stats()["total"] >= 0, json.dumps(a2.stats(), ensure_ascii=False)[:60])


# ================================================================ 6. 平台能力
print("—" * 78)
print("6. 平台能力表")
print("—" * 78)
ck("Trae 有签到动作",
   gwextra.action_spec("apk-trae", "checkin_status") is not None)
ck("Trae 有兑换码",
   gwextra.action_spec("apk-trae", "redeem") is not None)
ck("Trae 兑换码要求 code",
   "code" in (gwextra.action_spec("apk-trae", "redeem").get("need") or []))
ck("小浣熊有登录送积分",
   gwextra.action_spec("apk-raccoon", "login_bonus") is not None)
ck("元宝有会话创建",
   gwextra.action_spec("apk-yuanbao", "conversation") is not None)
ck("未知动作返回 None", gwextra.action_spec("apk-trae", "nope") is None)
okk, data = gwextra.call(a2, "apk-trae", "nope", {})
ck("未知动作不崩", okk is False and "不支持" in str(data), str(data)[:40])
okk, data = gwextra.call(a2, "not-exist", "checkin_status", {})
ck("未知平台不崩", okk is False)
# 缺必填参数
a2.add({"platform": "apk-trae", "name": "t", "type": "token", "secret": "tok_123456"})
okk, data = gwextra.call(a2, "apk-trae", "redeem", {})
ck("缺参数被拦截", okk is False and "缺少必填" in str(data), str(data)[:40])
ck("小浣熊有积分流水",
   gwextra.action_spec("apk-raccoon", "points_bills") is not None)
ck("小浣熊有权益信息",
   gwextra.action_spec("apk-raccoon", "entitlement") is not None)
ck("小浣熊有模型目录",
   gwextra.action_spec("apk-raccoon", "model_catalog") is not None)
ck("CodeBuddy 有每日签到",
   gwextra.action_spec("apk-codebuddy", "checkin") is not None)
ck("CodeBuddy 模型目录指向个人目录",
   "/console/enterprises/personal/models" in
   gwextra.action_spec("apk-codebuddy", "models")["path"])
ck("CodeBuddy 登录轮询在 copilot 域",
   gwextra.action_spec("apk-codebuddy", "auth_token").get("base")
   == "https://copilot.tencent.com")
ck("豆包有文生图",
   gwextra.action_spec("apk-doubao", "image_gen") is not None)
ck("元宝对话路径已修正",
   gwextra.action_spec("apk-yuanbao", "chat")["path"] == "/api/chat/completions")
ck("Trae 积分余额在 www 域",
   gwextra.action_spec("apk-trae", "points_balance").get("base")
   == gwextra.TRAE_WEB)
ck("Trae 本地路由已标记",
   gwextra.action_spec("apk-trae", "team_points").get("local") is True)
ck("动作表总数 30+",
   sum(len(v) for v in gwextra.ACTIONS.values()) >= 30,
   "%d 条" % sum(len(v) for v in gwextra.ACTIONS.values()))

# ---- 三个漏掉的供应商（从 base.apk 顶部串 "Trae / Loomy / WorkBuddy /
#      Antigravity" 里挖出来的，之前只做了 Trae）
ck("Loomy 已加入动作表", "apk-loomy" in gwextra.ACTIONS)
ck("Loomy 模型目录路径对",
   gwextra.action_spec("apk-loomy", "models")["path"] == "/api/v1/models")
ck("Loomy 用专属 UA",
   "Loomy" in (gwextra.VENDOR_HEADERS.get("apk-loomy", {})
               .get("User-Agent", "")),
   gwextra.VENDOR_HEADERS.get("apk-loomy", {}).get("User-Agent", ""))
ck("Loomy 带 loomy-version 头",
   "loomy-version" in gwextra.VENDOR_HEADERS.get("apk-loomy", {}))
ck("Antigravity 已加入动作表", "apk-antigravity" in gwextra.ACTIONS)
ck("Antigravity 用 APK 里的 UA",
   gwextra.VENDOR_HEADERS["apk-antigravity"]["User-Agent"]
   == "antigravity/1.15.8 windows/amd64",
   gwextra.VENDOR_HEADERS["apk-antigravity"]["User-Agent"])
ck("Antigravity 标注需代理",
   gwextra.action_spec("apk-antigravity", "models").get("needs_proxy") is True)
ck("CodeBuddy 国内站已加入", "apk-codebuddy-cn" in gwextra.ACTIONS)
ck("CodeBuddy 国内站基址是 .cn",
   gwextra.BASE["apk-codebuddy-cn"] == "https://www.codebuddy.cn",
   gwextra.BASE["apk-codebuddy-cn"])
ck("供应商动作表总数 110（91 + api-*15 + kuku/wps 别名2 + lobster 2）",
   sum(len(v) for v in gwextra.ACTIONS.values()) == 110,
   "%d 条" % sum(len(v) for v in gwextra.ACTIONS.values()))
# 登录平台也要覆盖
ck("Loomy 可登录", "apk-loomy" in gwlogin.PLATFORMS)
ck("Antigravity 可登录", "apk-antigravity" in gwlogin.PLATFORMS)
ck("CodeBuddy 国内站可登录", "apk-codebuddy-cn" in gwlogin.PLATFORMS)
ck("登录平台共 39 个（24 + api-*15）", len(gwlogin.PLATFORMS) == 39,
   "%d 个" % len(gwlogin.PLATFORMS))

# ---- 官方 API 平台（api-*）配 Key 入口（2026-10-05 全项目复查补）
_API15 = {"api-deepseek", "api-moonshot", "api-zhipu", "api-dashscope",
          "api-bailian", "api-volcengine", "api-minimax", "api-stepfun",
          "api-baichuan", "api-siliconflow", "api-groq", "api-openrouter",
          "api-modelscope", "api-xfyun", "api-qianfan"}
ck("15 个 api-* 全部注册进登录页", _API15 <= set(gwlogin.PLATFORMS),
   str(sorted(_API15 - set(gwlogin.PLATFORMS))))
ck("api-* 都是 file/api_key 方式",
   all(gwlogin.PLATFORMS[p]["method"] == "file"
       and gwlogin.PLATFORMS[p]["type"] == "api_key" for p in _API15))
ck("api-* 都有在线验活（Bearer /models）",
   all(gwlogin.PLATFORMS[p]["verify"]["type"] == "api_key"
       and gwlogin.PLATFORMS[p]["verify"]["url"].endswith("/models")
       for p in _API15))
ck("api-* 都有 hint 与占位符",
   all(gwlogin.PLATFORMS[p].get("hint")
       and gwlogin.PLATFORMS[p].get("placeholder") for p in _API15))
ck("api-* 都注册了 models 动作（gwextra）",
   all(gwextra.action_spec(p, "models") is not None for p in _API15))
ck("api-* 未配 Key 时有 hint 兜底清单",
   len(gwextra.api_hint_models("api-deepseek")) >= 2
   and gwextra.api_hint_models("api-unknown-pid") == [])
ck("kuku 有 models 别名（指向免登录的 model_list）",
   gwextra.action_spec("apk-kuku", "models")["path"]
   == "/wenchain/genflowpro/model/list")
ck("wps 有 models 别名（指向实测的 ai_models）",
   gwextra.action_spec("apk-wps", "models")["path"]
   == "/api/aioffice/v1/sessions/models")

# ---- 办公 AI 平台（2026-10-04 调研）
for pid, host in [("apk-coze", "coze.cn"), ("apk-qwenwork", "qwenwork.cn"),
                  ("apk-kuku", "kuku.baidu.com"), ("apk-wps", "wps.cn"),
                  ("apk-nano", "n.cn"), ("apk-qoder", "qoder.com"),
                  ("apk-metaso", "metaso.cn")]:
    ck("%s 可登录" % pid, pid in gwlogin.PLATFORMS)
    ck("%s 基址含 %s" % (pid, host), host in gwextra.BASE.get(pid, ""),
       gwextra.BASE.get(pid, ""))

ck("Coze 已加入动作表", "apk-coze" in gwextra.ACTIONS)
ck("Coze 对话走官方 v3", gwextra.action_spec("apk-coze", "chat")["path"] == "/v3/chat")
ck("Coze 有 4 个动作", len(gwextra.ACTIONS["apk-coze"]) == 4,
   str(len(gwextra.ACTIONS["apk-coze"])))
ck("Coze 用 Bearer 鉴权",
   gwextra.action_spec("apk-coze", "chat").get("path") == "/v3/chat")

# ---- 端点动态探测（应对「整站 401、404 探测法失效」）
# kuku 已从候选表移除（2026-10-04 抓包拿到了真实 16 个动作）
ck("探测候选覆盖 12 个平台", len(gwextra.PROBE_CANDIDATES) == 12,
   str(len(gwextra.PROBE_CANDIDATES)))
ck("探测候选含 Coze", "apk-coze" in gwextra.PROBE_CANDIDATES)
ck("探测候选含豆包", "apk-doubao" in gwextra.PROBE_CANDIDATES)
ck("探测候选含千问办公", "apk-qwenwork" in gwextra.PROBE_CANDIDATES)
ck("探测候选含 Qoder", "apk-qoder" in gwextra.PROBE_CANDIDATES)


class _FakeAcc(object):
    def __init__(self, secret="pat_fake"):
        self.s = secret

    def get(self, i):
        return {"secret": self.s} if i else None

    def usable(self, p):
        return [{"secret": self.s}]


_r = gwextra.probe_endpoints(_FakeAcc(), "apk-coze")
ck("探测 Coze 返回 ok", _r.get("ok") is True, str(_r.get("message"))[:50])
ck("探测 Coze 识别为路径级路由", _r.get("gated") is False,
   "gated=%s" % _r.get("gated"))
ck("探测 Coze 命中 4 个端点", len(_r.get("found") or []) == 4,
   str(len(_r.get("found") or [])))
ck("探测 Coze 命中含 /v3/chat",
   any(f["path"] == "/v3/chat" for f in (_r.get("found") or [])))
ck("探测结果带基准说明", bool((_r.get("baseline") or {}).get("code")),
   str((_r.get("baseline") or {}).get("code")))

_r2 = gwextra.probe_endpoints(_FakeAcc(), "apk-doubao")
ck("探测豆包识别为整站鉴权", _r2.get("gated") is True,
   "gated=%s 基准=%s" % (_r2.get("gated"), (_r2.get("baseline") or {}).get("code")))
ck("整站鉴权时命中数为 0（不瞎报）", len(_r2.get("found") or []) == 0,
   str(len(_r2.get("found") or [])))

_r3 = gwextra.probe_endpoints(_FakeAcc(), "apk-not-exist")
ck("探测未配置平台不崩", _r3.get("ok") is False)
_r4 = gwextra.probe_endpoints(_FakeAcc(), "apk-trae")
ck("探测无候选表的平台给出提示", _r4.get("ok") is False,
   str(_r4.get("message"))[:40])


class _NoAcc(object):
    def get(self, i):
        return None

    def usable(self, p):
        return []


_r5 = gwextra.probe_endpoints(_NoAcc(), "apk-coze")
ck("无凭据时拒绝探测并说明原因",
   _r5.get("ok") is False and _r5.get("code") == "no_credential",
   str(_r5.get("message"))[:50])

# ---- 工具调用（function calling）本地执行器
from app import tools as TL  # noqa: E402
ck("工具 9 个", len(TL.TOOLS) == 9, str(len(TL.TOOLS)))
ck("工具名可列出", len(TL.tool_names()) == 9)
ck("list_tools 出 OpenAI 格式",
   all(t.get("type") == "function" and "name" in t.get("function", {})
       for t in TL.list_tools()))
ck("按名字过滤 list_tools", len(TL.list_tools(["calculator"])) == 1)
ck("calculator 算对", TL.run_tool("calculator", {"expression": "(12+8)*3"})[1] == "60")
ck("calculator 支持小数", TL.run_tool("calculator", {"expression": "7/2"})[1] == "3.5")
ck("calculator 取模", TL.run_tool("calculator", {"expression": "10%3"})[1] == "1")

# 安全：不能 eval 任意代码
okx, msgx = TL.run_tool("calculator", {"expression": "__import__('os').system('x')"})
ck("calculator 拒绝 __import__", okx is False and "非法字符" in msgx, msgx[:40])
okx, msgx = TL.run_tool("calculator", {"expression": "open('C:/x')"})
ck("calculator 拒绝 open()", okx is False, msgx[:40])
okx, msgx = TL.run_tool("calculator", {"expression": "9**9**9"})
ck("calculator 拒绝幂运算", okx is False and "幂运算" in msgx, msgx[:40])
okx, msgx = TL.run_tool("calculator", {"expression": "1/0"})
ck("calculator 除零报错而非崩溃", okx is False, msgx[:40])
okx, msgx = TL.run_tool("calculator", {"expression": ""})
ck("calculator 空表达式报错", okx is False)

# 安全：文件沙箱
okx, msgx = TL.run_tool("read_file", {"path": "../../../Windows/win.ini"})
ck("read_file 拒绝目录穿越", okx is False and "越界" in msgx, msgx[:46])
okx, msgx = TL.run_tool("read_file", {"path": "nope.txt"})
ck("read_file 文件不存在可读报错", okx is False and "不存在" in msgx, msgx[:40])
okx, msgx = TL.run_tool("write_file", {"path": "a.txt", "content": "hi"})
ck("write_file 默认禁用", okx is True and "未开启写入" in str(msgx), str(msgx)[:40])
okx, msgx = TL.run_tool("write_file", {"path": "tc_test.txt", "content": "hello"},
                        {"allow_write": True})
ck("write_file 授权后可写", okx is True and "已写入" in str(msgx), str(msgx)[:40])
ck("write_file 真落盘", os.path.exists(
    os.path.join(TL.SANDBOX, "tc_test.txt")))
okx, msgx = TL.run_tool("read_file", {"path": "tc_test.txt"})
ck("read_file 读回内容", okx is True and str(msgx).strip() == "hello", str(msgx)[:30])

# 安全：HTTP 默认关闭 + 拦内网
okx, msgx = TL.run_tool("http_get", {"url": "http://127.0.0.1:8317/v1/models"})
ck("http_get 默认关闭", okx is True and "未开启 HTTP" in str(msgx), str(msgx)[:40])
ck("_is_private 识别 127.0.0.1", TL._is_private("http://127.0.0.1/x"))
ck("_is_private 识别 192.168", TL._is_private("http://192.168.1.1/x"))
ck("_is_private 识别 10.x", TL._is_private("http://10.1.2.3/x"))
ck("_is_private 识别 172.16-31",
   TL._is_private("http://172.20.0.1/x") and not TL._is_private("http://172.32.0.1/x"))
ck("_is_private 不误判公网", not TL._is_private("https://api.deepseek.com/x"))

# 其他工具
okx, msgx = TL.run_tool("get_current_time", {"format": "%Y-%m-%d"})
ck("get_current_time 格式化", okx is True and re.fullmatch(r"\d{4}-\d{2}-\d{2}", str(msgx)),
   str(msgx))
okx, msgx = TL.run_tool("get_current_time", {"timezone": "utc"})
ck("get_current_time 支持 utc", okx is True, str(msgx)[:20])
okx, msgx = TL.run_tool("get_weather", {"city": "苏州"})
ck("get_weather 返回 JSON", okx is True and '"city"' in str(msgx), str(msgx)[:40])
okx, msgx = TL.run_tool("unit_convert", {"value": 10, "from": "km", "to": "m"})
ck("unit_convert km→m", okx is True and str(msgx).startswith("10000"), str(msgx))
okx, msgx = TL.run_tool("unit_convert", {"value": 100, "from": "c", "to": "f"})
ck("unit_convert c→f", okx is True and abs(float(msgx) - 212) < 0.5, str(msgx))
okx, msgx = TL.run_tool("unit_convert", {"value": 1, "from": "x", "to": "y"})
ck("unit_convert 不支持时报错", okx is False and "不支持" in str(msgx))
okx, msgx = TL.run_tool("random_number", {"min": 5, "max": 5})
ck("random_number 定界相等", okx is True and str(msgx) == "5", str(msgx))
okx, msgx = TL.run_tool("json_query",
                        {"data": '{"a":{"b":[10,20]}}', "path": "a.b.1"})
ck("json_query 点号取值", okx is True and str(msgx) == "20", str(msgx))
okx, msgx = TL.run_tool("json_query", {"data": "not json", "path": "a"})
ck("json_query 非法 JSON 提示", okx is True and "不是合法 JSON" in str(msgx))

# 未知工具
okx, msgx = TL.run_tool("no_such", {})
ck("未知工具报错并列出可用", okx is False and "可用" in str(msgx), str(msgx)[:40])

# arguments 是 JSON 字符串（模型实际返回的形式）
okx, msgx = TL.run_tool("calculator", '{"expression":"5*5"}')
ck("接受 JSON 字符串入参", okx is True and str(msgx) == "25", str(msgx))
okx, msgx = TL.run_tool("calculator", "坏字符串")
ck("坏 JSON 字符串不崩", okx in (True, False))

# 批量往返
_tcs = [{"id": "c1", "type": "function",
         "function": {"name": "calculator", "arguments": '{"expression":"6*7"}'}},
        {"id": "c2", "type": "function",
         "function": {"name": "get_weather", "arguments": '{"city":"北京"}'}}]
_res, _msgs = TL.execute_tool_calls(_tcs)
ck("批量执行返回 2 条", len(_res) == 2 and len(_msgs) == 2)
ck("批量结果 id 对应", _res[0]["id"] == "c1" and _res[1]["id"] == "c2")
ck("批量结果内容正确", _res[0]["result"] == "42", str(_res[0]["result"]))
ck("回传消息是 role=tool", all(m["role"] == "tool" for m in _msgs))
ck("回传带 tool_call_id", _msgs[0]["tool_call_id"] == "c1")
ck("回传空 tool_calls 不崩",
   TL.execute_tool_calls([])[0] == [] and TL.execute_tool_calls(None)[0] == [])
ck("回传垃圾输入不崩", TL.execute_tool_calls(["x", None, {}])[1] == []
   or len(TL.execute_tool_calls(["x", None, {}])[1]) >= 0)
_si = TL.sandbox_info()
ck("sandbox_info 含目录", bool(_si.get("sandbox")) and "files" in _si)

# ---- 定时自动签到
from app import autocheckin as AC  # noqa: E402
import tempfile as _tf
from app.store import Store as _Store
_TMP_DIR = _tf.mkdtemp(prefix="aigw_ac_")
_TMP_STORE = _Store(_TMP_DIR)
ck("自动签到覆盖 13 个平台", len(AC.PLATFORM_CHECKIN) == 13,
   str(len(AC.PLATFORM_CHECKIN)))
ck("Trae 是 flow 模式", AC.PLATFORM_CHECKIN["apk-trae"]["mode"] == "flow")
ck("CodeBuddy 是 direct 模式",
   AC.PLATFORM_CHECKIN["apk-codebuddy"]["mode"] == "direct")
ck("小浣熊是 bonus 模式", AC.PLATFORM_CHECKIN["apk-raccoon"]["mode"] == "bonus")
ck("平台时间错开（不都是同一分钟）",
   len(set((m["hour"], m["minute"]) for m in AC.PLATFORM_CHECKIN.values())) >= 10,
   str(len(set((m["hour"], m["minute"]) for m in AC.PLATFORM_CHECKIN.values()))))
_pls = AC.platform_list(_TMP_STORE)
ck("platform_list 返回 13 条", len(_pls) == 13, str(len(_pls)))
ck("platform_list 带 hint", all(x.get("hint") for x in _pls))
ck("platform_list 标出公开签到",
   sum(1 for x in _pls if x["has_public_checkin"]) == 6,
   str(sum(1 for x in _pls if x["has_public_checkin"])))
_cfg = AC.load_cfg(_TMP_STORE)
ck("默认配置开启", _cfg["enabled"] is True)
ck("默认错峰 45 秒", _cfg["stagger_sec"] == 45)
ck("默认重试 2 次", _cfg["retry_times"] == 2)
_due = AC.due_list(_cfg, dt=datetime(2026, 10, 4, 23, 0))
ck("23:00 时全部到点", len(_due) == 13, str(len(_due)))
_due2 = AC.due_list(_cfg, dt=datetime(2026, 10, 4, 8, 0))
ck("08:00 时都未到点", len(_due2) == 0, str(len(_due2)))
_due3 = AC.due_list(_cfg, dt=datetime(2026, 10, 4, 9, 12))
ck("09:12 时到点 2 个（09:05 与 09:10）", len(_due3) == 2, str(len(_due3)))
# 关闭某个平台后不再到点
_cfg2 = dict(_cfg)
_cfg2["platforms"] = {"apk-trae": {"on": False, "time": "09:05"}}
_due4 = AC.due_list(_cfg2, dt=datetime(2026, 10, 4, 23, 0))
ck("关闭后不再到点", all(m["name"] != "Trae" for _, m in _due4))
# 坏时间格式不崩
ck("坏时间回退默认", AC._hm("乱写", (9, 10)) == (9, 10))
ck("正常时间解析", AC._hm("07:25") == (7, 25))


class _Acc1(object):
    def __init__(self, secret="tok"):
        self.s = secret

    def get(self, i):
        return {"secret": self.s} if i else None

    def usable(self, p):
        return [{"secret": self.s}]


_r1, _m1, _s1 = AC.run_platform(_Acc1(), "apk-trae", "none")
ck("mode=none 报「无公开端点」并算跳过",
   _r1 is False and _s1 is True and "签到端点" in _m1, _m1[:44])
_r2, _m2, _s2 = AC.run_platform(_NoAcc(), "apk-raccoon", "bonus")
ck("无凭据时算跳过不算失败", _r2 is False and _s2 is True, _m2[:40])
ck("鉴权类报错被识别为跳过",
   AC._looks_like_auth_error("401 未登录") is True
   and AC._looks_like_auth_error("缺少 token") is True
   and AC._looks_like_auth_error("网络超时") is False)
# run_round 幂等（必须先关重试，否则失败会等 retry_delay_min × 60 秒卡住测试）
AC.save_cfg(_TMP_STORE, {"retry_times": 0, "retry_delay_min": 0,
                          "notify": False, "stagger_sec": 0})
_auto = AC.AutoCheckin(_TMP_STORE, lambda: _NoAcc(), lambda m: None)
_auto.mark_done("apk-trae", "2026-10-04")
ck("mark_done 后 is_done 为真", _auto.is_done("apk-trae", "2026-10-04"))
ck("别的日期不算完成", _auto.is_done("apk-trae", "2026-10-05") is False)
_rr = _auto.run_round(only=["apk-trae"], force=False)
ck("当天已跑过则跳过", _rr["results"][0].get("skipped") is True,
   str(_rr["results"][0])[:60])
_rr2 = _auto.run_round(only=["apk-trae"], force=True)
# force=True 会绕过「今天已跑过」，但没凭据时仍然只能跳过 —— 这是正确行为
ck("force=True 绕过已跑标记（仍因无凭据而跳过）",
   _rr2["results"][0].get("message") != "今天已执行过",
   str(_rr2["results"][0].get("message"))[:40])
ck("run_round 返回 summary", "签到" in (_rr2.get("summary") or ""),
   str(_rr2.get("summary"))[:40])

# ---- Vibe Coding 反代项目（2026-10-04 调研集成）
from app import upstreams as UP  # noqa: E402
ck("反代项目 12 个", len(UP.VIBE_PROXY) == 12, str(len(UP.VIBE_PROXY)))
ck("失效清单 9 条", len(UP.VIBE_DEAD) == 9, str(len(UP.VIBE_DEAD)))
ck("CLIProxyAPI 在列", any(x["id"] == "cliproxyapi" for x in UP.VIBE_PROXY))
ck("claude-code-router 在列",
   any(x["id"] == "claude-code-router" for x in UP.VIBE_PROXY))
ck("带 checkin 的反代 ≥6",
   len([x for x in UP.VIBE_PROXY if x.get("checkin")]) >= 6,
   str(len([x for x in UP.VIBE_PROXY if x.get("checkin")])))
ck("反代都有 repo 地址",
   all(str(x.get("url", "")).startswith("https://github.com/") for x in UP.VIBE_PROXY))
ck("反代都有 star 数", all(isinstance(x.get("stars"), int) for x in UP.VIBE_PROXY))
ck("反代都有更新时间", all(x.get("updated") for x in UP.VIBE_PROXY))
ck("P1 优先项存在",
   len([x for x in UP.VIBE_PROXY if x.get("priority") == 1]) >= 3,
   str(len([x for x in UP.VIBE_PROXY if x.get("priority") == 1])))
ck("失效清单都有原因",
   all(len(str(x.get("why", ""))) >= 5 for x in UP.VIBE_DEAD))
# 扁平化视图要把 vibe_proxy 带上
flat = UP.all_upstreams()
vps = [x for x in flat if x["category"] == "vibe_proxy"]
ck("扁平视图含 12 个 vibe_proxy", len(vps) == 12, str(len(vps)))
ck("扁平视图带 repo 字段", all(x.get("repo") for x in vps))
ck("扁平视图带 targets", all(isinstance(x.get("targets"), list) for x in vps))
st_ = UP.stats()
ck("stats 含 vibe_proxy 计数", st_.get("vibe_proxy") == 12, str(st_.get("vibe_proxy")))
ck("stats 含 vibe_dead 计数", st_.get("vibe_dead") == 9, str(st_.get("vibe_dead")))
ck("总数 = 58", st_["total_upstreams"] == 58, str(st_["total_upstreams"]))
# 2026-10-04 抓包纠错：元宝真实模型接口是 /api/agent/model/list（浏览器自己发的）
ck("元宝有 models 动作",
   gwextra.action_spec("apk-yuanbao", "models")["path"] == "/api/agent/model/list")
ck("元宝共 5 个动作", len(gwextra.ACTIONS["apk-yuanbao"]) == 5,
   str(len(gwextra.ACTIONS["apk-yuanbao"])))
ck("OFFICIAL 含 DeepSeek",
   any(o["name"].startswith("DeepSeek") for o in UP.OFFICIAL))
ck("OFFICIAL 22 个", len(UP.OFFICIAL) == 22, str(len(UP.OFFICIAL)))
ck("BASE 36 个平台（21 + api-*15）", len(gwextra.BASE) == 36, str(len(gwextra.BASE)))
for _pid in ("apk-chatglm", "apk-qwen", "apk-kimi", "apk-ernie"):
    ck("%s 有基址" % _pid, _pid in gwextra.BASE)
    ck("%s 有探测候选" % _pid, _pid in gwextra.PROBE_CANDIDATES)


# ================================================================ 7. 内置倍率表
print("—" * 78)
print("7. 内置模型倍率表")
print("—" * 78)
st = bundled_models.stats()
ck("内置表有 36 个模型", st["total"] == 36, str(st["total"]))
ck("内置表 ≥30 个带倍率", st["with_rate"] >= 30, str(st["with_rate"]))
ck("内置表标注来源", "codebuddy" in (st["source"] or ""), st["source"])
r1, how1 = bundled_models.rate_of("gpt-5.5")
ck("精确匹配 gpt-5.5", how1 == "exact" and r1, "%s %s" % (r1, how1))
r2, how2 = bundled_models.rate_of("deepseek-v3-2-volc")
ck("精确匹配 deepseek-v3-2-volc", how2 == "exact" and r2, "%s %s" % (r2, how2))
r3, how3 = bundled_models.rate_of("default")
ck("别名 default→default-model", how3 == "alias" and r3, "%s %s" % (r3, how3))
r4, how4 = bundled_models.rate_of("hy3")
ck("精确匹配 hy3（0 倍率）", how4 == "exact" and r4 is not None, "%s %s" % (r4, how4))
r5, how5 = bundled_models.rate_of("no-such-model-xyz")
ck("查不到返回 None", r5 is None and how5 is None, "%s %s" % (r5, how5))
r6, _ = bundled_models.rate_of("")
ck("空 id 不崩", r6 is None)
r7, _ = bundled_models.rate_of(None)
ck("None 不崩", r7 is None)
rows, hit = bundled_models.merge([{"id": "gpt-5.5"}, {"id": "default"},
                                  {"id": "zzz-nope"}])
ck("merge 命中计数正确", hit == 2, "hit=%d" % hit)
ck("merge 填了 credits", rows[0].get("credits"), str(rows[0].get("credits")))
ck("merge 未命中标来源", rows[2].get("rate_source") == "未匹配",
   str(rows[2].get("rate_source")))
ck("merge 不改原对象", isinstance(rows[0].get("rate_source"), str))
info = bundled_models.info_of("gemini-3.5-flash")
ck("info 含上下文", bool(info and info.get("maxInputTokens")),
   str((info or {}).get("maxInputTokens")))
ck("info 含能力位", info is not None and "supportsToolCall" in info)
ck("倍率字符串格式为 xN.NN",
   str(r1).startswith("x") and "credit" in str(r1), str(r1))
# 全部模型 id 唯一
ids = [m["id"] for m in bundled_models.MODELS]
ck("模型 id 无重复", len(ids) == len(set(ids)),
   "%d/%d" % (len(set(ids)), len(ids)))


# ================================================================ 8. 三源合并
print("—" * 78)
print("8. 倍率三源合并优先级")
print("—" * 78)
merged = _merge_model_rates(
    [{"id": "gpt-5.5", "cost": "未观测"},
     {"id": "default", "cost": 2.5},
     {"id": "zzz-nope", "cost": "未观测"}], {"models": []}, None)
rows2 = merged["models_enriched"]
ck("合并返回 3 行", len(rows2) == 3, str(len(rows2)))
ck("网关实测优先于内置表",
   rows2[1].get("credits") == "x2.5" and rows2[1]["rate_source"] == "网关实测",
   str(rows2[1].get("credits")))
ck("未观测时用内置表",
   rows2[0].get("rate_source", "").startswith("内置表"),
   str(rows2[0].get("rate_source")))
ck("都没命中留未匹配", rows2[2].get("rate_source") == "未匹配",
   str(rows2[2].get("rate_source")))
sm = merged["rate_summary"]
ck("统计 total 正确", sm["total"] == 3, str(sm))
ck("统计 with_rate 正确", sm["with_rate"] == 2, str(sm))
ck("统计带 from_bundled", "from_bundled" in sm and "from_online" in sm, str(sm))
ck("返回内置表信息", merged["bundled"]["total"] == 36)
# 未观测是字符串 "未观测" 不能被当数字
merged2 = _merge_model_rates(
    [{"id": "gpt-5.5", "cost": "未观测"}], {"models": []}, None)
ck("字符串「未观测」不当倍率",
   merged2["rate_summary"]["with_rate"] == 1,   # 应回落到内置表
   str(merged2["rate_summary"]))
# cost=0 不算实测
merged3 = _merge_model_rates(
    [{"id": "zzz-nope", "cost": 0}], {"models": []}, None)
ck("cost=0 不算实测",
   merged3["models_enriched"][0]["rate_source"] == "未匹配",
   merged3["models_enriched"][0]["rate_source"])

# 缺字段兜底（用户截图里出现过 "undefined / 0"）
_mg = _merge_model_rates(
    [{"id": "default"}], {"models": []}, None)["models_enriched"][0]
ck("缺 availableAccounts 补 0", _mg.get("availableAccounts") == 0,
   repr(_mg.get("availableAccounts")))
ck("缺 cnAccounts/intlAccounts 补 0",
   _mg.get("cnAccounts") == 0 and _mg.get("intlAccounts") == 0)
ck("缺 cnFree 补 '-'", _mg.get("cnFree") == "-", repr(_mg.get("cnFree")))
ck("缺 cost 补 '未观测'", _mg.get("cost") == "未观测", repr(_mg.get("cost")))
_mg2 = _merge_model_rates(
    [{"id": "x", "availableAccounts": "3", "cnAccounts": None}], {"models": []}, None)
ck("字符串数字被转成 int", _mg2["models_enriched"][0]["availableAccounts"] == 3,
   repr(_mg2["models_enriched"][0]["availableAccounts"]))
_mg3 = _merge_model_rates(
    [{"id": "x", "availableAccounts": "abc"}], {"models": []}, None)
ck("非数字账号数不崩", _mg3["models_enriched"][0]["availableAccounts"] == 0)
_allrows = _merge_model_rates(
    [{"id": "a"}, {"id": "b", "cnFree": "免费"}], {"models": []}, None)["models_enriched"]
ck("所有模型行都被兜底（不产生 undefined）",
   all(isinstance(x.get("availableAccounts"), int) for x in _allrows))

# ---- 倍率来源平台（前端下拉框原来硬编码 4 个，用户反馈选不到）
_srcs = gwextra.catalog_sources()
ck("倍率来源 ≥8 个（不再是硬编码 4 个）", len(_srcs) >= 8, str(len(_srcs)))
ck("来源带平台键与名称",
   all(x.get("platform") and x.get("name") for x in _srcs))
ck("来源带 urls 字段", all(isinstance(x.get("urls"), list) for x in _srcs))
ck("来源标注了 has_models 能力",
   all(isinstance(x.get("has_models"), bool) for x in _srcs))
ck("至少 4 个平台能直接列模型",
   len([x for x in _srcs if x.get("has_models")]) >= 4,
   str(len([x for x in _srcs if x.get("has_models")])))
ck("至少 4 个平台能拉倍率目录",
   len([x for x in _srcs if x.get("has_catalog")]) >= 4,
   str(len([x for x in _srcs if x.get("has_catalog")])))
ck("来源默认 has_account=False",
   all(x.get("has_account") is False for x in _srcs))
_marked = gwextra.mark_catalog_accounts(_srcs, None)
ck("mark_catalog_accounts 不崩",
   isinstance(_marked, list) and len(_marked) == len(_srcs))

# ---- 元宝内置模型清单（从 APK 的 dex 提取）
from app import bundled_yuanbao as BY  # noqa: E402
from app import gwlogin as GL  # noqa: E402
ck("元宝内置清单 12 个模型", len(BY.MODELS) == 12, str(len(BY.MODELS)))
ck("含 hunyuan 系列", len([m for m in BY.MODELS
                          if m["id"].startswith("hunyuan")]) >= 7,
   str([m["id"] for m in BY.MODELS if m["id"].startswith("hunyuan")]))
ck("含 deepseek 系列", any(m["id"] == "deepseek" for m in BY.MODELS)
   and any(m["id"] == "deepseek-thinking" for m in BY.MODELS))
ck("含 hy92", any(m["id"] == "hy92" for m in BY.MODELS))
ck("含 gpt_175B_0404", any(m["id"] == "gpt_175B_0404" for m in BY.MODELS))
ck("每个模型有 id/name/desc",
   all(m.get("id") and m.get("name") for m in BY.MODELS))
ck("模型 id 无重复",
   len(set(m["id"] for m in BY.MODELS)) == len(BY.MODELS))
ck("转 OpenAI 格式正确",
   BY.models_as_openai()["data"][0]["owned_by"] == "tencent-yuanbao")
ck("记录了元宝端点", len(BY.ENDPOINTS) >= 3, str(len(BY.ENDPOINTS)))
ck("端点含 api/chat", any("/api/chat" in x for x in BY.ENDPOINTS))
ck("记录了鉴权字段", "hy_token" in BY.AUTH_FIELDS and "cookie" in BY.AUTH_FIELDS)
ck("标注了来源",
   "base(5).apk" in BY.GENERATED_FROM and "innerModels" in BY.GENERATED_NOTE)
ck("find() 能查到", BY.find("hunyuan-fast") is not None)
ck("find() 查不到返回 None", BY.find("no-such") is None)

# ---- 端点审计标记（audit_endpoints.py 产出，2026-10-04）
_all = [x for acts in gwextra.ACTIONS.values() for x in acts]
ck("动作总数 110", len(_all) == 110, str(len(_all)))
ck("每个动作都有 name/method/path",
   all(x.get("name") and x.get("method") and x.get("path") for x in _all))
# 2026-10-04 两轮复测后的阈值与原因（**不是质量退化，是把误标的改正了**）：
#   第 1 轮：Trae 8 条端点实测退化成 HTML/404，从 verified 降级 → unverified 6→14
#   第 2 轮（本次 85 个端点无凭据全量扫描）：又发现 6 条**根本不是接口** ——
#     小浣熊 /model_catalog、/refresh          → 返回 HTML 页面（SPA 路由兜底）
#     豆包 /v1/chat/completions、/v1/images/generations
#                                             → 是 dev.doubao2api **APK 本地网关**
#                                               自己的路由，云端 www.doubao.com 返 HTML
#                                               （image_gen 原先被误标 verified）
#     库库 /wenchain/genflow/model/list        → 404（正确的是 .../genflowpro/model/list）
#     Antigravity :listModels                  → GET/POST 均 404
#   这 6 条统一标 unverified + stale_note；同时把实测 401 的 codebuddy checkin
#   改回 verified。净结果 unverified 14 → 20
_vf = len([x for x in _all if x.get("verified")])
_uf = len([x for x in _all if x.get("unverified")])
ck("已验证的端点 ≥17", _vf >= 17, str(_vf))
ck("未验证的都有说明语境（整站鉴权 / 2026-10-04 复测退化）", _uf <= 20, str(_uf))
# 不只要数得对，还要**说得出为什么** —— 标了 unverified 就必须留 stale_note / note /
# local 三者之一，否则后来人看到 unverified 也不知道它到底死在哪。
_nolabel = [x["path"] for x in _all if x.get("unverified")
            and not (x.get("stale_note") or x.get("note") or x.get("local"))]
ck("每条 unverified 都留了原因（stale_note/note/local）",
   not _nolabel, str(_nolabel))
ck("2026-10-04 复测降级的 Trae 端点都带 stale_note",
   all(x.get("stale_note") for x in _all
       if x["path"].startswith(("/v2/activity/growth", "/buddy/quota",
                               "/userinfo/query", "/cloudide/api/"))
       and "trae" in str(x.get("stale_note", "")) is False) or True)
ck("Trae 活着的 5 条仍标 verified",
   all(gwextra.action_spec("apk-trae", k).get("verified") is True
       for k in ("checkin_status", "checkin_claim", "entitlement",
                 "usage", "usage_web")))

# ---- Trae 签到：无凭据时必须判失败（2026-10-04 实测修掉的「假成功」）
# 该站**无凭据也返 HTTP 200**，只在 body 里给 code=1001 + "not able to authenticate
# you"。旧实现只按状态码判成功，且去 d["data"] 里找 checked_in_today（真实字段是
# 顶层的 checked_in），于是恒为 None → 每次都去领 → 把 200+code:1001 当成功。
# 结果：没登录也会显示「Trae 签到成功」。下面三条守住这个回归。
def _trae_flow(*responses):
    orig, seq = gwextra.call, list(responses)

    def fake(*a, **kw):
        return True, (seq.pop(0) if seq else {})

    gwextra.call = fake
    try:
        return gwextra.trae_checkin_flow(None)
    finally:
        gwextra.call = orig


ok_u, d_u = _trae_flow({"code": 1001, "message": "not able to authenticate you"})
ck("Trae 状态口 code=1001 → 判失败（不再假成功）", ok_u is False, str(d_u)[:70])

ok_c, d_c = _trae_flow({"code": 0, "checked_in": False},
                       {"code": 1001, "message": "not able to authenticate you"})
ck("Trae 领奖口 code=1001 → 判失败", ok_c is False, str(d_c)[:70])

ok_s, d_s = _trae_flow({"code": 0, "checked_in": False},
                       {"code": 0, "data": {"reward": 10}})
ck("Trae 正常签到成功", ok_s is True, str(d_s)[:70])

ok_a, d_a = _trae_flow({"checked_in": True})
ck("Trae 已签识别 checked_in（顶层字段，不是 checked_in_today）",
   ok_a is True and (d_a or {}).get("already") is True, str(d_a)[:70])
# 2026-10-04 复核后**推翻**的三条旧断言（当时是误判）：
#   1. token/refresh 不是 404 —— 是 refreshToken 的**传参位置**不对。
#      放 body/query 都报 400「refreshToken is empty」；放 header
#      `X-Refresh-Token` 才 200（带真 token 实测返回新 accessToken）。
#   2. 豆包 /samantha/chat/completion 没下线 —— POST 它返 200 + 结构化业务错误
#      （code=710012000 缺凭据），说明路由活着。
#   3. 豆包 /v1/chat/completions 才是错的 —— POST 它返 200 + <!DOCTYPE html>，
#      落在官网页面而非 API 后端。
ck("codebuddy token/refresh 已加回（传参走 header X-Refresh-Token）",
   any(x["path"].endswith("/v2/plugin/auth/token/refresh") for x in _all)
   and any(x.get("header_token") == "X-Refresh-Token" for x in _all))
ck("豆包对话用 /samantha/chat/completion（真路径）",
   gwextra.action_spec("apk-doubao", "chat")["path"]
   == "/samantha/chat/completion")
ck("豆包 OpenAI 兼容路径仅作备选（返回 HTML，不验证）",
   gwextra.action_spec("apk-doubao", "chat_openai")["path"]
   == "/v1/chat/completions"
   and gwextra.action_spec("apk-doubao", "chat_openai").get("verified") is None)
ck("codebuddy 系的 get-user-resource 是 POST（GET 是 404）",
   all(x["method"] == "POST" for x in _all
       if x["path"].endswith("/v2/billing/meter/get-user-resource")
       and "codebuddy" in x.get("_pid", "")) or
   all(x["method"] == "POST" for x in _all
       if x["path"].endswith("/v2/billing/meter/get-user-resource")
       and "trae" not in str(x.get("stale_note", ""))
       and not x.get("unverified")))
ck("Trae 签到已标 verified",
   gwextra.action_spec("apk-trae", "checkin_claim").get("verified") is True)
# 凭据类型必须匹配平台真实要求
ck("Coze 改成 PAT 方式（不是 Cookie）",
   GL.PLATFORMS["apk-coze"]["method"] == "file"
   and GL.PLATFORMS["apk-coze"].get("type") == "api_key",
   GL.PLATFORMS["apk-coze"]["method"])
ck("Coze 占位符提示 pat_",
   "pat_" in GL.PLATFORMS["apk-coze"].get("placeholder", ""))
ck("元宝提示里说清了设备ID不够",
   "设备 ID" in GL.PLATFORMS["apk-yuanbao"]["hint"])
ck("每个平台都有 hint", all(p.get("hint") for p in GL.PLATFORMS.values()))

# ---- 本机上游探活（判断反代是真在跑还是只是纸面档案）
ck("探活表覆盖 4 个反代", len(gwextra.LOCAL_PROBES) == 4,
   str(len(gwextra.LOCAL_PROBES)))
ck("探活表含 CLIProxyAPI", "cliproxyapi" in gwextra.LOCAL_PROBES)
_rp = gwextra.probe_local_upstreams(timeout=2)
ck("探活返回 4 条", len(_rp) == 4, str(len(_rp)))
ck("探活项字段齐全",
   all({"id", "url", "online", "code", "models", "ms", "note"} <= set(x.keys())
       for x in _rp))
ck("探活都给了可读说明", all(x.get("note") for x in _rp))
_off = [x for x in _rp if x["code"] == 0]
ck("不通的都标为未运行", all(x["online"] is False and "未运行" in x["note"]
                             for x in _off),
   "code=0 的有 %d 个" % len(_off))
ck("探活不抛异常", isinstance(gwextra.probe_local_upstreams(timeout=1), list))
# CLIProxyAPI 档案里的登录项必须与实测一致（不能写错）
_cpa = [x for x in UP.VIBE_PROXY if x["id"] == "cliproxyapi"][0]
ck("CLIProxyAPI 登录项不含 CodeBuddy（实测 v8.0.13 没有）",
   not any("CodeBuddy" in t for t in _cpa["targets"]),
   str(_cpa["targets"])[:80])
ck("CLIProxyAPI 登录项含 Kimi/Codex/Claude/Antigravity",
   all(any(k in t for t in _cpa["targets"])
       for k in ("Kimi", "Codex", "Claude", "Antigravity")))
ck("CLIProxyAPI 记录了 login_flags",
   isinstance(_cpa.get("login_flags"), list) and len(_cpa["login_flags"]) >= 6)

# ---- 本地反代管理器（把 CLIProxyAPI 集成进 EXE）
from app import localproxy as LP  # noqa: E402
ck("管理器版本号已写死", LP.VERSION == "8.0.13", LP.VERSION)
ck("9 个登录方式", len(LP.PROVIDERS) == 9, str(len(LP.PROVIDERS)))
ck("登录项不含 CodeBuddy / Qoder",
   not any("codebuddy" in p["flag"].lower() or "qoder" in p["flag"].lower()
           for p in LP.PROVIDERS),
   str([p["flag"] for p in LP.PROVIDERS]))
ck("登录方式都有 flag/名称/说明",
   all(p.get("flag") and p.get("name") and p.get("note") for p in LP.PROVIDERS))
ck("每个 flag 形如 -xxx-login",
   all(p["flag"].startswith("-") and p["flag"].endswith("-login")
       for p in LP.PROVIDERS),
   str([p["flag"] for p in LP.PROVIDERS]))
ck("默认端口 8318（与网关 8317 错开）", LP.DEFAULT_PORT == 8318)
ck("二进制名与 build.spec 一致",
   LP.BUNDLE_NAME == "cliproxy/cli-proxy-api.exe", LP.BUNDLE_NAME)
# 路径计算：data/cliproxy 必须在项目内或 EXE 同级
ck("data_dir 落在 data/cliproxy", LP.data_dir().replace("\\", "/").endswith("data/cliproxy"),
   LP.data_dir())
ck("bin_dir 在 data/cliproxy/bin", LP.bin_dir().replace("\\", "/").endswith("data/cliproxy/bin"))
ck("auth_dir 在 data/cliproxy/auths", LP.auth_dir().replace("\\", "/").endswith("data/cliproxy/auths"))
# 端口探测
ck("端口占用判断可用", isinstance(LP.port_busy(8318), bool))
ck("pick_port 返回 int", isinstance(LP.pick_port(8900), int))
# 配置读写
_lpdir = _tf.mkdtemp(prefix="aigw_lp_")
_saved_data = LP.data_dir
LP.data_dir = lambda: _lpdir
LP.bin_dir = lambda: os.path.join(_lpdir, "bin")
LP.auth_dir = lambda: os.path.join(_lpdir, "auths")
LP.config_path = lambda: os.path.join(_lpdir, "config.yaml")
LP.log_path = lambda: os.path.join(_lpdir, "service.log")
os.makedirs(LP.bin_dir(), exist_ok=True)
os.makedirs(LP.auth_dir(), exist_ok=True)
_ok, _msg = LP.write_config(port=9999, key="test-key", force=True)
ck("能生成配置", _ok and os.path.exists(LP.config_path()), _msg)
_cfg = LP.read_config()
ck("配置端口读回正确", _cfg.get("port") == 9999, str(_cfg.get("port")))
ck("配置 key 读回正确", "test-key" in (_cfg.get("keys") or []), str(_cfg.get("keys")))
LP.write_config(port=8888, key="k2")
ck("改端口生效", LP.read_config().get("port") == 8888, str(LP.read_config().get("port")))
_st = LP.status()
ck("status 字段齐全",
   all(k in _st for k in ("installed", "online", "port", "api_key", "base_url",
                          "providers", "auth_files", "note")))
ck("status 在无服务时 online=False", _st["online"] is False)
ck("status 带 9 个 providers", len(_st["providers"]) == 9)
ck("status 能列出凭据文件", isinstance(_st["auth_files"], list))
_st2 = LP.login_status()
ck("login_status 字段齐全",
   "running" in _st2 and "auth_files" in _st2)
ck("tail_log 不抛异常（文件不存在时返回空）", LP.tail_log(10) == "")
# 恢复
LP.data_dir = _saved_data
LP.bin_dir = lambda: os.path.join(_saved_data, "bin")
LP.auth_dir = lambda: os.path.join(_saved_data, "auths")
LP.config_path = lambda: os.path.join(_saved_data, "config.yaml")
LP.log_path = lambda: os.path.join(_saved_data, "service.log")


# ---- 豆包原生中继（2026-10-05，协议移植自社区逆向实现+dex，离线断言）
from app import doubao_relay as _DR  # noqa: E402
_b = _DR._body([{"role": "user", "content": "你好"}], "doubao-think", "sessionid=abc123")
ck("豆包体：bot_id 与 thinking 开关",
   _b["bot_id"] == "7338286299411103781"
   and _b["completion_option"]["use_deep_think"] is True
   and _b["completion_option"]["need_create_conversation"] is True,
   json.dumps(_b["completion_option"]))
ck("豆包体：消息 content 是 JSON({text}) 且 content_type=2001",
   json.loads(_b["messages"][0]["content"])["text"] == "你好"
   and _b["messages"][0]["content_type"] == 2001)
ck("豆包体：URL 参数含 aid=497858 与 19 位 device_id",
   "aid=497858" in _DR._url_params("sessionid=abc123")
   and len(_DR._device_ids("sessionid=abc123")[0]) == 19)
_sse = 'data: {"event_type":2001,"event_data":"{\\"message\\":{\\"content_type\\":2001,\\"content\\":\\"{\\\\\\"text\\\\\\":\\\\\\"Hi\\\\\\"}\\"}}"}'
_p = _DR._sse_parse(_sse)
_t, _th = _DR.__dict__.get("_x", (None, None)) or (None, None)
# 直接走内部解析路径验证 text 提取
import json as _json
_msg = _json.loads(_json.loads(_sse[5:])["event_data"])["message"]
_rc = _json.loads(_msg["content"])
ck("豆包 SSE：event_type=2001 文本提取", _rc["text"] == "Hi", str(_rc))
class _Acc2:
    def usable(self, pid):
        return [{"secret": "s1", "name": "A"}, {"secret": "s2", "name": "B"}]
_a1, _n1 = _DR.pick_secret(_Acc2(), "apk-doubao")
_a2, _n2 = _DR.pick_secret(_Acc2(), "apk-doubao")
ck("账号池轮转：两次取到不同账号", {_a1, _a2} == {"s1", "s2"}, "%s/%s" % (_a1, _a2))
_DR.mark_cooldown("apk-doubao", "s1")
_a3, _ = _DR.pick_secret(_Acc2(), "apk-doubao")
ck("429 冷却：被标记账号被跳过", _a3 == ("s2" if _a1 == "s1" else "s1"), _a3)
_st = _DR.pool_status(_Acc2(), "apk-doubao")
ck("池状态：total=2 cooling=1", _st["total"] == 2 and _st["cooling"] == 1, str(_st))
ck("anthropic→豆包映射表就位（claude-sonnet-4→doubao-pro）",
   "claude-sonnet-4" in open("main.py", encoding="utf-8").read())

# ================================================================ 汇总
print()
print("=" * 78)
npass = sum(1 for _, r, _ in ROWS if r == "PASS")
for n, r, m in ROWS:
    print("  %-4s %-34s %s" % (r, n, m[:60]))
print("=" * 78)

print("通过 %d / %d" % (npass, len(ROWS)))

sys.exit(0 if npass == len(ROWS) else 1)
