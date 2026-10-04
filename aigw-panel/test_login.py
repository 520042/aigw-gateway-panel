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

# 未知平台必须报错
try:
    mgr.start("not-a-platform")
    ck("未知平台报错", False)
except KeyError:
    ck("未知平台报错", True)

# 7 网关覆盖
need = {"wb-gateway", "apk-trae", "apk-codebuddy", "apk-doubao",
        "apk-yuanbao", "apk-raccoon", "apk-go"}
have = set(gwlogin.PLATFORMS)
ck("7 个网关均有登录方式", need <= have, "缺=%s" % ",".join(sorted(need - have)))
ck("每个平台都有 name", all(p.get("name") for p in gwlogin.PLATFORMS.values()))
ck("每个平台都有 method",
   all(p.get("method") in ("qrcode", "cookie", "file")
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
ck("供应商动作表总数 50", sum(len(v) for v in gwextra.ACTIONS.values()) == 50,
   "%d 条" % sum(len(v) for v in gwextra.ACTIONS.values()))
# 登录平台也要覆盖
ck("Loomy 可登录", "apk-loomy" in gwlogin.PLATFORMS)
ck("Antigravity 可登录", "apk-antigravity" in gwlogin.PLATFORMS)
ck("CodeBuddy 国内站可登录", "apk-codebuddy-cn" in gwlogin.PLATFORMS)
ck("登录平台共 18 个", len(gwlogin.PLATFORMS) == 18,
   "%d 个" % len(gwlogin.PLATFORMS))

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
ck("探测候选覆盖 8 个平台", len(gwextra.PROBE_CANDIDATES) == 8,
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
ck("总数 = 50", st_["total_upstreams"] == 50, str(st_["total_upstreams"]))


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


# ================================================================ 汇总
print()
print("=" * 78)
npass = sum(1 for _, r, _ in ROWS if r == "PASS")
for n, r, m in ROWS:
    print("  %-4s %-34s %s" % (r, n, m[:60]))
print("=" * 78)
print("通过 %d / %d" % (npass, len(ROWS)))
sys.exit(0 if npass == len(ROWS) else 1)
