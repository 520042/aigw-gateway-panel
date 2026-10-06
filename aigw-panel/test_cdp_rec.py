# -*- coding: utf-8 -*-
"""Session / Recorder 离线自检：用假 CDP 服务端跑通并发收发"""
import hashlib
import json
import os
import socket
import struct as st
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from app.cdp import Session, _pick          # noqa: E402


def send_text(c, txt):
    p = txt.encode()
    h = bytearray([0x81])
    n = len(p)
    if n < 126:
        h.append(n)
    elif n < 65536:
        h.append(126)
        h += st.pack(">H", n)
    else:
        h.append(127)
        h += st.pack(">Q", n)
    c.sendall(bytes(h) + p)


def read_frame(f):
    h = f.read(2)
    if not h or len(h) < 2:
        return None
    masked = bool(h[1] & 0x80)
    n = h[1] & 0x7F
    if n == 126:
        n = st.unpack(">H", f.read(2))[0]
    elif n == 127:
        n = st.unpack(">Q", f.read(8))[0]
    mask = f.read(4) if masked else None
    p = f.read(n)
    if mask:
        p = bytes(b ^ mask[i % 4] for i, b in enumerate(p))
    return p.decode("utf-8", "replace")


def read_handshake(c):
    """
    逐字节读到 \\r\\n\\r\\n 为止。
    千万不能用 recv(4096) 一次读满 —— 和 101 响应粘在同一个 TCP 包里的
    后续帧会被一起吞掉（真实 CDP 在 101 之后立刻推事件）。
    """
    buf = b""
    while not buf.endswith(b"\r\n\r\n"):
        d = c.recv(1)
        if not d:
            raise OSError("握手中断")
        buf += d
    return buf


def make_server(scenario):
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]

    def run():
        try:
            c, _ = srv.accept()
        except OSError:
            return
        try:
            read_handshake(c)
        except OSError:
            return
        c.sendall(b"HTTP/1.1 101 Switching Protocols\r\n"
                  b"Upgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
        try:
            scenario(c)
        except Exception as e:
            print("  [server] %s: %s" % (type(e).__name__, e))

    threading.Thread(target=run, daemon=True).start()
    return port


OK = []
BAD = []


def check(name, cond, extra=""):
    (OK if cond else BAD).append(name)
    print("  %s %s %s" % ("PASS" if cond else "FAIL", name, extra))


# ---------------------------------------------------------------- 场景 1
def scen_normal(c):
    # 未订阅事件先到（模拟握手后立刻推事件）
    send_text(c, json.dumps({
        "method": "Network.requestWillBeSent", "params": {
            "timestamp": 1, "requestId": "R1", "request": {
                "method": "GET",
                "url": "https://copilot.tencent.com/v2/plugin/auth/token?state=abc123",
                "headers": {"Cookie": "sid=1", "X-Ide-Type": "production",
                            "Junk": "x"}}}}))
    f = c.makefile("rb")
    got_enable = False
    while not got_enable:
        t = read_frame(f)
        if t is None:
            return
        m = json.loads(t)
        if m.get("method") == "Network.enable":
            # 回响应的同时再夹一个事件，验证不会互相错位
            send_text(c, json.dumps({"method": "Network.responseReceived",
                                     "params": {"requestId": "R1",
                                                "response": {"url": "u",
                                                             "status": 200}}}))
            send_text(c, json.dumps({"id": m["id"], "result": {"ok": 1}}))
            got_enable = True
    for i in range(3):
        send_text(c, json.dumps({
            "method": "Network.requestWillBeSent", "params": {
                "timestamp": 2 + i, "requestId": "R%d" % (i + 2),
                "request": {"method": "POST", "url": "https://x/y",
                            "headers": {}, "postData": "a=1"}}}))
    time.sleep(3)


print("场景 1：并发收发 + 事件分发")
port = make_server(scen_normal)
s = Session("ws://127.0.0.1:%d/x" % port)
res = s.call("Network.enable", {}, timeout=8)
check("call 返回 result", res == {"ok": 1}, repr(res))
evs = s.drain(1.0)
mets = [e.get("method") for e in evs]
check("拿到 5 条事件", len(evs) == 5, "-> %d" % len(evs))
check("事件顺序未错位",
      mets.count("Network.requestWillBeSent") == 4
      and mets.count("Network.responseReceived") == 1, str(mets))
r1 = evs[0]["params"]["request"]
check("url/state 完整", "state=abc123" in r1["url"])
check("_pick 过滤噪声头",
      _pick(r1["headers"]) == {"Cookie": "sid=1", "X-Ide-Type": "production"},
      str(_pick(r1["headers"])))
check("postData 抓到", any(e["params"]["request"].get("postData") == "a=1"
                          for e in evs if e.get("method") ==
                          "Network.requestWillBeSent"))
s.close()

# ---------------------------------------------------------------- 场景 2
print("\n场景 2：服务端不回 → call 超时抛错，不死锁")


def scen_silent(c):
    time.sleep(8)


port = make_server(scen_silent)
s2 = Session("ws://127.0.0.1:%d/x" % port)
t0 = time.time()
try:
    s2.call("Network.enable", {}, timeout=2)
    check("超时抛 WSError", False, "竟然没抛")
except Exception as e:
    check("超时抛 WSError", "超时" in str(e), "%.1fs %s" % (time.time() - t0, e))
check("超时后 drain 仍可用", isinstance(s2.drain(0.3), list))
s2.close()

# ---------------------------------------------------------------- 场景 3
print("\n场景 3：服务端直接断开 → 所有等待者被唤醒")


def scen_hangup(c):
    c.close()


port = make_server(scen_hangup)
s3 = Session("ws://127.0.0.1:%d/x" % port)
time.sleep(0.3)
t0 = time.time()
try:
    s3.call("Network.enable", {}, timeout=10)
    check("断开会抛错而非挂死", False)
except Exception as e:
    check("断开会抛错而非挂死", True, "%.1fs %s" % (time.time() - t0, str(e)[:40]))

print("\n场景 4：Recorder._on_req/_on_resp 必须读 params 层（曾全字段 null）")
from app.cdp import Recorder                       # noqa: E402
r = Recorder(port=1, out=None)
written = []
r._write = lambda rec: written.append(rec)
r._on_req({"method": "Network.requestWillBeSent", "params": {
    "timestamp": 12.5, "requestId": "R9", "type": "XHR",
    "request": {"method": "POST", "url": "https://copilot.tencent.com/v2/plugin/auth/token?state=S1",
                "headers": {"Cookie": "k=v", "Accept": "*/*", "Junk": "x"},
                "postData": "{\"state\":\"S1\"}"}}})
w = written[-1]
check("url 非空", bool(w.get("url")), w.get("url") or "None")
check("method 非空", w.get("method") == "POST")
check("requestId 带出", w.get("requestId") == "R9")
check("timestamp 带出", w.get("ts") == 12.5)
check("postData 带出", w.get("post") == '{"state":"S1"}')
check("headers 已过滤", w.get("headers") == {"Cookie": "k=v", "Accept": "*/*"},
      str(w.get("headers")))

written.clear()
r._on_resp({"method": "Network.responseReceived", "params": {
    "timestamp": 13.0, "requestId": "R9",
    "response": {"url": "https://x/y", "status": 200, "mimeType": "application/json"}}})
w = written[-1]
check("resp status 带出", w.get("status") == 200 and w.get("reqId") == "R9",
      json.dumps(w, ensure_ascii=False))

written.clear()
r._on_body("R9", '{"access_token":"AT1"}', False)
check("响应体落盘 kind=body", written[-1].get("kind") == "body"
      and "AT1" in written[-1]["body"])
written.clear()
r._on_body("R9", "aGVsbG8=", True)
check("base64 响应体解码", written[-1].get("body") == "hello",
      repr(written[-1].get("body")))

print("\n场景 5：fire-and-forget 的回包按方法归位到 results 队列")


def scen_fireforget(c):
    f = c.makefile("rb")
    n = 0
    while n < 2:
        t = read_frame(f)
        if t is None:
            return
        m = json.loads(t)
        meth = m.get("method")
        if meth == "Network.enable":
            send_text(c, json.dumps({"id": m["id"], "result": {}}))
        elif meth == "Network.getResponseBody":
            send_text(c, json.dumps({"id": m["id"], "result": {
                "requestId": m["params"]["requestId"],
                "body": "{\"ok\":1}", "base64Encoded": False}}))
        n += 1
    time.sleep(2)


port = make_server(scen_fireforget)
s5 = Session("ws://127.0.0.1:%d/x" % port)
s5.call("Network.enable", {}, timeout=6)
s5.send("Network.getResponseBody", {"requestId": "R7"})
time.sleep(0.8)
got = []
while True:
    try:
        got.append(s5.results.get_nowait())
    except Exception:
        break
methods = [g[0] for g in got]
check("results 收到两条", len(got) == 2, str(methods))
check("方法名归位正确", "Network.getResponseBody" in methods, str(methods))
gb = [g for g in got if g[0] == "Network.getResponseBody"]
check("响应体内容正确", bool(gb) and gb[0][1].get("body") == '{"ok":1}',
      json.dumps(gb[0][1]) if gb else "无")
s5.close()

print("\n通过 %d / 失败 %d" % (len(OK), len(BAD)))
if BAD:
    print("失败项：", BAD)
sys.exit(1 if BAD else 0)
