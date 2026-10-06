# -*- coding: utf-8 -*-
"""
Chrome DevTools Protocol 客户端（标准库实现）
=============================================
为什么需要它：
  Chrome 127+ 把本地 Cookie 换成了 app-bound 加密（"v20" 前缀），
  密钥绑定系统级 DPAPI，普通用户进程**无法**解密。
  但浏览器进程自己持有明文 —— 通过 CDP 的 Network.getAllCookies
  可以让 Chrome 直接把解密后的 Cookie 交出来。

  这就让「Cookie 类网关」（豆包 / 元宝 / 小浣熊 / Trae 网页版）的登录
  变成真正的**一键**：面板开一个调试浏览器 → 用户在里面登录 → 面板取 Cookie。

组成：
  1. 最小 WebSocket 客户端（RFC 6455，含掩码 / 分片 / 64bit 长度）
  2. CDP 会话（Target.attachToTarget + Network.getAllCookies / Page.navigate）
  3. 浏览器启动器（找 chrome/edge 可执行文件 + 独立 user-data-dir）

零第三方依赖。
"""

import base64
import json
import os
import queue
import socket
import struct
import subprocess
import sys
import threading
import time

# ------------------------------------------------------------------ WebSocket

class WSError(Exception):
    pass


class WS:
    """最小 WebSocket 客户端：只处理文本帧 + 分片 + close/ping/pong"""

    def __init__(self, sock):
        self.sock = sock
        self.buf = b""
        self._frag = []

    @classmethod
    def connect(cls, url, timeout=5):
        if not url.startswith("ws://"):
            raise WSError("仅支持 ws:// ，收到 %s" % url)
        rest = url[5:]
        if "/" in rest:
            hostport, path = rest.split("/", 1)
            path = "/" + path
        else:
            hostport, path = rest, "/"
        host, _, port = hostport.partition(":")
        port = int(port or 80)
        sock = socket.create_connection((host, port), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (
            "GET %s HTTP/1.1\r\n"
            "Host: %s:%d\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            "Sec-WebSocket-Key: %s\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n" % (path, host, port, key)
        ).encode()
        sock.sendall(req)
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = sock.recv(4096)
            if not chunk:
                raise WSError("WebSocket 握手无响应")
            buf += chunk
        head = buf.split(b"\r\n\r\n", 1)[0].decode("latin-1")
        line = head.split("\r\n", 1)[0]
        if "101" not in line:
            raise WSError("WebSocket 握手失败：%s" % line)
        # 关键：握手响应之后的残留字节不能丢。
        # 服务端常在 101 之后立刻发帧（CDP 的 Target.attachedToTarget 等事件），
        # TCP 会把它和握手响应粘在同一个包里；直接 cls(sock) 会让 self.buf 为空，
        # 这些帧就被永久丢弃，后续 recv 全部错位 —— 表现为随机超时或解出垃圾。
        rest = buf.split(b"\r\n\r\n", 1)[1] if b"\r\n\r\n" in buf else b""
        cli = cls(sock)
        cli.buf = rest
        return cli

    # ---------------------------------------------------------- 发送
    def send(self, text):
        payload = text.encode("utf-8")
        header = bytearray()
        header.append(0x81)  # FIN + opcode=1(text)
        n = len(payload)
        mask = os.urandom(4)
        if n < 126:
            header.append(0x80 | n)
        elif n < 65536:
            header.append(0x80 | 126)
            header += struct.pack(">H", n)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", n)
        header += mask
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    # ---------------------------------------------------------- 接收
    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise WSError("连接已关闭")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv(self, timeout=20):
        """读一个完整消息（自动合并分片），返回 str"""
        self.sock.settimeout(timeout)
        while True:
            b0, b1 = self._read(2)
            fin = (b0 >> 7) & 1
            opcode = b0 & 0x0F
            masked = (b1 >> 7) & 1
            n = b1 & 0x7F
            if n == 126:
                n = struct.unpack(">H", self._read(2))[0]
            elif n == 127:
                n = struct.unpack(">Q", self._read(8))[0]
            if masked:
                mask = self._read(4)
            payload = self._read(n)
            if masked:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))

            if opcode == 0x8:            # close
                raise WSError("服务端关闭连接")
            if opcode == 0x9:            # ping → 回 pong
                self._send_raw(0x8A, payload)
                continue
            if opcode == 0xA:            # pong
                continue
            if opcode in (0x1, 0x2):     # text / binary
                # 宽容处理：若上一条消息还没收完，这里**追加**而不是重置。
                # 严格按 RFC 应该在这种情况下报协议错误，但实测有服务端
                # （以及旧版 CDP mock）会发「0x01 FIN=0 + 0x81 FIN=1」这种
                # 非标准分片，重置会静默丢掉第一片的数据。
                self._frag.append(payload)
                if fin:
                    out = b"".join(self._frag)
                    self._frag = []
                    return out.decode("utf-8", "replace")
            elif opcode == 0x0:          # continuation —— 标准分片续传
                # 真实 Chrome（CDP）发的分片用的就是这个 opcode，
                # 漏掉它会导致大响应永远收不齐。
                self._frag.append(payload)
                if fin:
                    out = b"".join(self._frag)
                    self._frag = []
                    return out.decode("utf-8", "replace")

    def _send_raw(self, opcode, payload):
        header = bytearray([opcode])
        n = len(payload)
        mask = os.urandom(4)
        if n < 126:
            header.append(0x80 | n)
        elif n < 65536:
            header.append(0x80 | 126)
            header += struct.pack(">H", n)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", n)
        header += mask
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    def close(self):
        try:
            self._send_raw(0x88, b"")
        except Exception:
            pass
        try:
            self.sock.close()
        except Exception:
            pass


# ------------------------------------------------------------------ CDP

class CDP:
    def __init__(self, ws):
        self.ws = ws
        self._id = 0

    def call(self, method, params=None, session_id=None, timeout=20):
        self._id += 1
        msg = {"id": self._id, "method": method, "params": params or {}}
        if session_id:
            msg["sessionId"] = session_id
        self.ws.send(json.dumps(msg, ensure_ascii=False))
        deadline = time.time() + timeout
        while time.time() < deadline:
            raw = self.ws.recv(timeout=max(1, deadline - time.time()))
            try:
                data = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if data.get("id") == self._id:
                if "error" in data:
                    raise WSError("%s 失败：%s" % (method, data["error"]))
                return data.get("result") or {}
        raise WSError("%s 超时" % method)


def http_json(url, timeout=5):
    import urllib.request
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return json.loads(r.read().decode("utf-8", "replace"))


# ------------------------------------------------------------------ 录制会话

class Session:
    """
    一个**长连接** CDP 会话（扁平模式：直接连 tab 的 webSocketDebuggerUrl）。

    为什么不用现成的 CDP.call()：
      CDP.call() 是「发一条、等一条、其余消息全丢」的同步语义，
      而录制需要**并发**接收事件流（Network.requestWillBeSent 等）
      同时又能发命令（Network.enable / Network.getResponseBody）。
      两条流抢同一个 socket 必然错位。

    做法：唯一的后台读线程负责收包，按消息类型分��：
      - 带 `id` → 唤醒对应的 call（Future 式）
      - 带 `method` → 塞进事件队列，供 drain() 取
    """

    def __init__(self, ws_url, timeout=20):
        self.ws = WS.connect(ws_url, timeout=timeout)
        self._id = 0
        self._lock = threading.Lock()
        self._pending = {}          # id -> [event, result_box]
        self._method = {}           # id -> 方法名（回包要按方法归位）
        self.events = queue.Queue()
        self.results = queue.Queue()  # (method, result|None, err|None)
        self.closed = False
        self._err = None
        self._th = threading.Thread(target=self._reader, daemon=True)
        self._th.start()

    # ---------------------------------------------------------- 后台读线程
    def _reader(self):
        while not self.closed:
            try:
                raw = self.ws.recv(timeout=1.0)
            except WSError:
                # close 帧 / 连接关闭；没事件时的读超时不会走到这里
                if self.closed:
                    return
                continue
            except socket.timeout:
                # 关键：socket.timeout 继承的是 OSError，不是 WSError。
                # 漏掉这个分支的话，空闲 1 秒后第一次超时就会被下面的
                # `except Exception` 吞掉、读线程静默退出，
                # 表现为「刚开始录了一会儿就再也不更新了」。
                continue
            except Exception as e:
                self._err = str(e)
                break
            if not raw:
                continue
            try:
                msg = json.loads(raw)
            except Exception:
                continue
            mid = msg.get("id")
            if mid is not None:
                with self._lock:
                    slot = self._pending.get(mid)
                    meth = self._method.get(mid, "?")
                    if slot is None:
                        self._method.pop(mid, None)
                if slot:
                    slot[1]["msg"] = msg
                    slot[0].set()
                # 命令结果一律投到 results 队列，**放在 if slot 之外**：
                # send()（fire-and-forget）没有 pending slot，
                # 放进来就等于把它丢掉 —— Network.getResponseBody
                # 正是靠这条路取回响应体的，丢了就永远抓不到 token。
                err = (msg.get("error") or {}).get("message")
                self.results.put((meth, msg.get("result"), err))
            else:
                self.events.put(msg)
        # 连接彻底结束 → 唤醒所有等待者，别让人卡死
        with self._lock:
            pend = list(self._pending.values())
            self._pending.clear()
            self._method.clear()
        for ev, box in pend:
            box["msg"] = {"error": {"message": self._err or "连接已关闭"}}
            ev.set()

    # ---------------------------------------------------------- 命令
    def call(self, method, params=None, timeout=20):
        with self._lock:
            self._id += 1
            mid = self._id
            ev = threading.Event()
            box = {"msg": None}
            self._pending[mid] = [ev, box]
            self._method[mid] = method
        msg = {"id": mid, "method": method, "params": params or {}}
        try:
            self.ws.send(json.dumps(msg, ensure_ascii=False))
        except Exception as e:
            with self._lock:
                self._pending.pop(mid, None)
                self._method.pop(mid, None)
            raise WSError("%s 发送失败：%s" % (method, e))
        if not ev.wait(timeout):
            with self._lock:
                self._pending.pop(mid, None)
                self._method.pop(mid, None)
            raise WSError("%s 超时" % method)
        with self._lock:
            self._pending.pop(mid, None)
            self._method.pop(mid, None)
        resp = box["msg"] or {}
        if "error" in resp:
            raise WSError("%s 失败：%s" % (method, resp["error"]))
        return resp.get("result") or {}

    def send(self, method, params=None):
        """只发不等（Fire-and-forget）"""
        with self._lock:
            self._id += 1
            mid = self._id
            self._method[mid] = method
        self.ws.send(json.dumps(
            {"id": mid, "method": method, "params": params or {}},
            ensure_ascii=False))

    # ---------------------------------------------------------- 事件
    def drain(self, timeout=1.0):
        """取回这一段时间内积压的全部事件（不阻塞超过 timeout）"""
        out = []
        end = time.time() + max(0.0, timeout)
        while True:
            left = end - time.time()
            if left <= 0:
                break
            try:
                out.append(self.events.get(timeout=left))
            except queue.Empty:
                break
        return out

    def close(self):
        self.closed = True
        try:
            self.ws.close()
        except Exception:
            pass


class Recorder:
    """
    完整链路录制器：给某个 tab 开 Network 域，把所有请求写 JSONL。

    用法：
        rec = Recorder(port=9333)
        rec.start("https://copilot.tencent.com/login?platform=CLI")
        ...  用户在浏览器里登录 ...
        rec.stop()
        rec.paths()          # 只看 URL
        rec.find("auth/token")
    """

    def __init__(self, port=9333, out=None, user_data_dir=None, exe=None):
        self.port = port
        self.exe = exe
        self.user_data_dir = user_data_dir
        self.out = out
        self.sess = None
        self.tid = None
        self.browser = Browser(port=port, exe=exe, user_data_dir=user_data_dir)
        self._stop = threading.Event()
        self._th = None
        self.count = 0
        self.bodies = {}         # requestId -> 响应体（尽力抓，失败就算了）
        # requestId -> url。getResponseBody 的**返回结果里没有 requestId**，
        # 只能靠发命令时自己记住映射，否则响应体和 URL 对不上号。
        self.sessions = {}      # {targetId: Session}，多标签
        self._rid2url = {}
        # getResponseBody 的回包不带 requestId，只能按「发命令的顺序」
        # 和回包的顺序一一配对，所以用队列而不是字典。
        self._pending_bodies = []      # [(requestId, url), ...]

    # ---------------------------------------------------------- 启动
    def start(self, url=None, headless=False, extra_args=()):
        self.browser.headless = headless
        self.browser.start(url)             # 内部会找 exe、起进程、等端口
        self.reattach()
        return self

    def reattach(self):
        """
        挂上**所有** page 标签（不是只挂一个）。

        ★ 2026-10-04 的教训：登录成功后站点常跳新标签（SSO 尤其如此），
          只挂最初那个 target 会漏掉全部后续请求 —— WPS 就是这么漏的
          （登录在 account.wps.cn，AI 接口在 lingxi.kdocs.cn 的新标签里）。
          `Target.createTarget` 开新标签后也必须重新接管。
        """
        self.close_sessions()
        self.sessions = self.browser.attach_all_pages(
            on_error=lambda t, e: sys.stderr.write(
                "[recorder] 挂标签失败 %s: %s\n" % (t.get("url"), e)))
        if not self.sessions:
            raise RuntimeError("没有可附着的标签页")
        self.sess = list(self.sessions.values())[0]
        # 老的单标签引用同步刷新
        for t in self.browser.targets():
            if t.get("id") in self.sessions:
                self.tid = t["id"]
                break
        if self._th is None or not self._th.is_alive():
            self._th = threading.Thread(target=self._pump, daemon=True)
            self._th.start()
        return len(self.sessions)

    def close_sessions(self):
        for s in getattr(self, "sessions", {}).values():
            try:
                s.close()
            except Exception:
                pass
        self.sessions = {}

    def _pump(self):
        while not self._stop.is_set():
            evs = []
            # 轮询所有已接管的标签（新开的也要）
            for _sid, _s in list(getattr(self, "sessions", {}).items()):
                try:
                    for _e in _s.drain(timeout=0.2):
                        evs.append(_e)
                except Exception:
                    pass
            if not evs:
                time.sleep(0.2)
            for ev in evs:
                m = ev.get("method")
                if m == "Network.requestWillBeSent":
                    self._on_req(ev)
                elif m == "Network.responseReceived":
                    self._on_resp(ev)
            self._collect_bodies()
        # 停机前把队列里剩下的写完
        self._collect_bodies()

    def _collect_bodies(self):
        """回收 fire-and-forget 的 getResponseBody 结果（所有标签）"""
        for _s in list(getattr(self, "sessions", {}).values()):
            self._collect_bodies_of(_s)

    def _collect_bodies_of(self, _s):
        try:
            while True:
                meth, res, err = _s.results.get_nowait()
                if meth != "Network.getResponseBody":
                    continue
                # 回包顺序 == 发命令顺序（FIFO）
                rid, url = (self._pending_bodies.pop(0)
                            if self._pending_bodies else ("", ""))
                if res and (res.get("body") or ""):
                    self._on_body(rid, res.get("body") or "",
                                   bool(res.get("base64Encoded")), url)
                else:
                    # 失败的请求不会留下待收队列，这里补位
                    if not self._pending_bodies:
                        self._on_body(rid, "", False, url)
        except queue.Empty:
            pass
        except Exception:
            pass

    def _on_body(self, rid, body, b64, url=""):
        if not body:
            return
        if b64:
            try:
                import base64 as _b
                body = _b.b64decode(body).decode("utf-8", "replace")
            except Exception:
                return
        self._write({
            "kind": "body",
            "wall": time.strftime("%H:%M:%S"),
            "requestId": rid,
            "url": url,
            "body": body[:8000],
        })

    def _on_req(self, ev):
        # 注意 CDP 事件是 {"method":..,"params":{..}}，业务字段全在 params 里
        p = ev.get("params") or {}
        req = p.get("request") or {}
        rid = p.get("requestId") or ""
        url = req.get("url")
        if rid and url:
            self._rid2url[rid] = url
        rec = {
            "kind": "request",
            "ts": round(p.get("timestamp") or 0, 3),
            "wall": time.strftime("%H:%M:%S"),
            "method": req.get("method"),
            "url": url,
            "type": p.get("type"),
            "requestId": rid,
            "headers": _pick(req.get("headers") or {}),
            "post": (req.get("postData") or "")[:4000],
        }
        self.count += 1
        self._write(rec)

    def _on_resp(self, ev):
        p = ev.get("params") or {}
        resp = p.get("response") or {}
        rid = p.get("requestId") or ""
        rec = {
            "kind": "response",
            "ts": round(p.get("timestamp") or 0, 3),
            "wall": time.strftime("%H:%M:%S"),
            "url": resp.get("url"),
            "status": resp.get("status"),
            "mime": resp.get("mimeType"),
            "reqId": rid,
        }
        self._write(rec)
        # 响应体：**成功和失败都要**。
        # 之前只抓 status<300，结果 400 的 /console/auth/login 响应体
        # 正好是最关键的错误信息，被这个条件挡掉了。
        try:
            code = int(resp.get("status") or 0)
        except (TypeError, ValueError):
            code = 0
        if 200 <= code < 400:
            try:
                self._pending_bodies.append((rid, resp.get("url")))
                self.sess.send("Network.getResponseBody", {"requestId": rid})
            except Exception:
                pass

    def _write(self, rec):
        if not self.out:
            return
        try:
            with open(self.out, "a", encoding="utf-8") as f:
                f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        except Exception:
            pass

    def stop(self):
        self._stop.set()
        time.sleep(1.2)                 # 让 pump 把队列里剩下的写完
        self.close_sessions()
        self.sess = None
        return self.count

    def close(self):
        self.stop()
        self.browser.close()

    # ---------------------------------------------------------- 回看
    def rows(self, kinds=("request", "response")):
        if not self.out or not os.path.exists(self.out):
            return []
        out = []
        with open(self.out, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    r = json.loads(line)
                except Exception:
                    continue
                if r.get("kind") in kinds:
                    out.append(r)
        return out

    def find(self, needle):
        """按 URL 子串过滤"""
        return [r for r in self.rows() if needle in (r.get("url") or "")]

    def timeline(self):
        """按时间顺序打印请求链路"""
        for r in self.rows(kinds=("request",)):
            print("%-9s %-5s %s" % (r.get("wall"), r.get("method"),
                                    (r.get("url") or "")[:130]))
            if r.get("post"):
                print("      POST %s" % r["post"][:200])
            for k, v in (r.get("headers") or {}).items():
                if k.lower() in ("authorization", "cookie", "x-ide-type",
                                 "x-ide-name", "content-type"):
                    print("      %s: %s" % (k, str(v)[:160]))


_HDR_KEEP = ("content-type", "authorization", "cookie", "referer", "origin",
             "user-agent", "accept", "x-ide-type", "x-ide-name", "x-ide-version",
             "x-client-version", "x-request-id", "x-tt-logid")


def _pick(headers):
    out = {}
    for k, v in (headers or {}).items():
        if k.lower() in _HDR_KEEP:
            out[k] = v
    return out


class Browser:
    """一个带调试端口的浏览器实例"""

    def __init__(self, port=9222, exe=None, user_data_dir=None, headless=False):
        self.port = port
        self.exe = exe
        self.user_data_dir = user_data_dir
        self.headless = headless
        self.proc = None

    # ---------------------------------------------------------- 启动
    def start(self, url=None):
        """
        起浏览器。**url 传 None 时开空白页**（复用 profile 里的登录态）。

        ⚠ 2026-10-04 用户反馈「每次开新窗口都要重新登录」的两个原因：
          1. 之前 `capture_*.py` 用 TaskStop 强杀进程 → profile 里的
             Cookie 还没落盘就被杀，下次起来自然是未登录。
             → 现在一律用 `close()`（terminate + 等退出）而不是强杀。
          2. `open_tab` 用 `/json/new` 失败时**不报错**，Chrome 会把请求
             当成「在已有实例里开新标签」，于是看起来像「开了新窗口」。
             → 现在 open_tab 失败会往 stderr 打原因。
        同一个 user_data_dir 复用时，登录态是持久的（实测 kuku/wps 登录后
        重开浏览器仍带 HMACCOUNT / XFT 等 Cookie）。
        """
        if self.is_up():
            return True
        exe = self.exe or find_browser()
        if not exe:
            raise RuntimeError("未找到 Chrome / Edge 可执行文件")
        udd = self.user_data_dir or os.path.join(
            os.path.expandvars(r"%TEMP%"), "aigw-cdp-profile")
        os.makedirs(udd, exist_ok=True)
        args = [
            exe,
            "--remote-debugging-port=%d" % self.port,
            "--user-data-dir=%s" % udd,
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-features=Translate",
        ]
        if self.headless:
            args.append("--headless=new")
        if url:
            args.append(url)
        try:
            self.proc = subprocess.Popen(args, close_fds=True)
        except Exception as e:
            raise RuntimeError("启动浏览器失败：%s" % e)
        for _ in range(60):
            time.sleep(0.5)
            if self.is_up():
                return True
        raise RuntimeError("浏览器调试端口未在 30 秒内就绪")

    def close(self, timeout=8):
        """
        **优雅关闭**（terminate 后等它自己退出并落盘 Cookie）。
        别用 taskkill /F 强杀 —— profile 里的 Cookie 还没写盘，
        下次启动就会「掉登录」。
        """
        if not self.proc:
            return
        try:
            self.proc.terminate()
        except Exception:
            pass
        t0 = time.time()
        while time.time() - t0 < timeout:
            if self.proc.poll() is not None:
                break
            time.sleep(0.3)
        else:
            try:
                self.proc.kill()
            except Exception:
                pass
        self.proc = None
        # 等端口释放，否则下一次 start() 会误判「已存活」
        t0 = time.time()
        while time.time() - t0 < 10 and self.is_up():
            time.sleep(0.5)

    def is_up(self):
        try:
            http_json("http://127.0.0.1:%d/json/version" % self.port, timeout=2)
            return True
        except Exception:
            return False

    def version(self):
        return http_json("http://127.0.0.1:%d/json/version" % self.port, timeout=3)

    def targets(self):
        try:
            return http_json("http://127.0.0.1:%d/json/list" % self.port, timeout=3)
        except Exception:
            return []

    def open_tab(self, url, new_window=False):
        """
        开新标签。**返回 target dict**（含 webSocketDebuggerUrl），失败返回 None。

        ⚠ 2026-10-04 踩过的坑：
        # 1) `/json/new?<url>` 这种 GET 形式在较新 Chrome 上会被拒（405），
        #    必须是 PUT —— 本函数已经用对了，但**返回 None 时没有任何提示**，
        #    导致上层以为标签开成功了，实际一个都没开，白等。
        # 2) 新开的标签**不会自动继承**已挂的 Network 域，
        #    登录后站点往往跳新标签，不接管就录不到（这正是 WPS 漏抓的原因）。
        #    用 `attach_all_pages()` 统一接管。
        """
        import urllib.request
        req = urllib.request.Request(
            "http://127.0.0.1:%d/json/new?%s" % (self.port, url), method="PUT")
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return json.loads(r.read().decode("utf-8", "replace"))
        except Exception as e:
            sys.stderr.write("[cdp] open_tab 失败 %s: %s\n" % (url, e))
            return None

    def attach_all_pages(self, on_error=None):
        """
        给**所有** page 标签各建一个 Session 并挂上 Network/Page/Runtime 域。
        返回 {targetId: Session}。

        为什么需要它：登录成功后站点经常跳新标签（SSO 单点登录尤其如此），
        只挂最初那个 target 会漏掉全部后续请求。WPS 就是这样漏的 ——
        登录在 account.wps.cn 完成，但真正的 AI 接口在 lingxi.kdocs.cn 的新标签里。
        """
        # ⚠ 2026-10-04 修：原来是 `from cdp import Session`，但本模块被打包成
        #   `app.cdp`，`import cdp` 在 EXE 里永远 ModuleNotFoundError ——
        #   于是**多标签 SSO 登录抓包一直是坏的**（WPS 登录在 account.wps.cn
        #   完成、AI 接口在 lingxi.kdocs.cn 新标签，正是靠这个函数挂全标签）。
        #   Session 就在本模块里，直接用即可。
        out = {}
        for t in self.targets():
            if t.get("type") != "page":
                continue
            ws = t.get("webSocketDebuggerUrl")
            if not ws:
                continue
            try:
                s = Session(ws)
                s.call("Network.enable", {"maxPostDataSize": 65536}, timeout=12)
                s.call("Page.enable", {}, timeout=10)
                s.call("Runtime.enable", {}, timeout=10)
                out[t["id"]] = s
            except Exception as e:
                if on_error:
                    on_error(t, e)
        return out

    @staticmethod
    def drain_all(sessions, seconds=30, poll=0.3):
        """
        同时从多个 Session 收事件，录 `seconds` 秒后返回。
        返回事件列表（已按 requestId 带上 session 标记）。
        """
        evs = []
        t0 = time.time()
        while time.time() - t0 < seconds:
            for tid, s in sessions.items():
                try:
                    for e in s.drain(timeout=poll):
                        e["_sid"] = tid
                        evs.append(e)
                except Exception:
                    pass
        return evs

    @staticmethod
    def close_all(sessions):
        for s in sessions.values():
            try:
                s.close()
            except Exception:
                pass

    # ---------------------------------------------------------- Cookie

    @staticmethod
    def _domain_matches(dom, h):
        """
        cookie 的 domain 是否属于 host h 所在的站点家族（双向，与浏览器一致）：
          · dom == h                → www.doubao.com 命中 www.doubao.com
          · dom.endswith("." + h)   → www.doubao.com 命中 doubao.com（子域属于家族）
          · h.endswith("." + dom)   → .aliyun.com 命中 lingma.aliyun.com（父域覆盖子域）
        注意：**绝不能用** u.find(dom) 这种子串匹配 —— 那是旧实现把豆包子域
        cookie 全丢、导致登录校验 401 的根因（www.doubao.com 不是
        "https://doubao.com" 的子串）。
        """
        dom = (dom or "").lstrip(".")
        h = (h or "").lstrip(".")
        if not dom or not h:
            return False
        if dom == h:
            return True
        if dom.endswith("." + h):
            return True
        if h.endswith("." + dom):
            return True
        return False

    @staticmethod
    def _host_of(u):
        u = (u or "").strip()
        if u.startswith("http://") or u.startswith("https://"):
            rest = u.split("://", 1)[1]
        else:
            rest = u
        return rest.split("/", 1)[0].split(":", 1)[0].lower()

    def cookies(self, urls=None):
        """
        取明文 Cookie。urls 为要过滤的 URL 列表（None 取全部）。
        返回 [{"name","value","domain","path",...}]
        """
        tabs = [t for t in self.targets() if t.get("type") == "page"]
        if not tabs:
            return []
        tab = tabs[0]
        ws = WS.connect(tab["webSocketDebuggerUrl"])
        try:
            cdp = CDP(ws)
            res = cdp.call("Network.getAllCookies", {}, timeout=30)
            allc = res.get("cookies") or []
        finally:
            ws.close()
        if not urls:
            return allc
        hosts = [self._host_of(u) for u in urls]
        out = []
        for c in allc:
            dom = (c.get("domain") or "").lstrip(".")
            if any(self._domain_matches(dom, hh) for hh in hosts):
                out.append(c)
        return out

    def storage_item(self, key, match=None):
        """
        ★ 读页面 localStorage 的某个键。

        为什么需要：2026-10-04 开浏览器实测发现，**元宝的登录态不在 Cookie 里** ——
        yuanbao.tencent.com 域下只有 `_TDID_CK`、`561553b2…` 这种**设备 ID**，
        真正的会话在 localStorage 的 `LOCAL_AUTH_INFO_KEY_yuanbao.tencent.com`
        （页面还有 `__is_guest_mode__` 标记）。只抓 Cookie 永远拿不到会话，
        于是验活必然失败——这就是「Cookie 已取得但未通过登录校验」的真因。
        match 用来挑标签（传域名片段，如 "yuanbao"），传 None 取第一个有值的。
        """
        tabs = [t for t in self.targets() if t.get("type") == "page"]
        if not tabs:
            return ""
        if match:
            hit = [t for t in tabs if match in (t.get("url") or "")]
            if hit:
                tabs = hit + [t for t in tabs if t not in hit]
        expr = ("JSON.stringify(localStorage.getItem(%s))"
                % json.dumps(key, ensure_ascii=False))
        for t in tabs:
            ws = t.get("webSocketDebuggerUrl")
            if not ws:
                continue
            try:
                w = WS.connect(ws)
            except Exception:
                continue
            try:
                cdp = CDP(w)
                r = cdp.call("Runtime.evaluate",
                             {"expression": expr, "returnByValue": True},
                             timeout=15)
                val = ((r or {}).get("result") or {}).get("value")
                if val and val != "null":
                    try:
                        return json.loads(val) or ""
                    except Exception:
                        return str(val)
            except Exception:
                continue
            finally:
                try:
                    w.close()
                except Exception:
                    pass
        return ""

    def cookie_header(self, hosts):
        """
        取出 hosts 覆盖范围内的全部 Cookie，拼成 `name=value; ...` 头。
        hosts 例：["doubao.com"] → 同时覆盖 .doubao.com / www.doubao.com /
        passport.doubao.com 等所有子域（旧实现用子串匹配把它们全丢了，
        导致豆包登录校验拿到的是残缺 Cookie，返回 401）。
        同名 cookie 按「更贴近目标 host」优先、其次「value 更长」去重，
        避免把父域的占位 cookie 盖掉子域的真实会话 cookie。
        """
        hosts = [h.strip().lower() for h in (hosts or []) if h.strip()]
        allc = self.cookies(None)  # getAllCookies 已包含全部，这里只做正确过滤
        if not hosts:
            return "; ".join("%s=%s" % (c["name"], c["value"]) for c in allc), allc

        def _score(c):
            dom = (c.get("domain") or "").lstrip(".")
            s = 0
            for h in hosts:
                if dom == h:
                    s += 2          # 精确命中 spec host
                elif dom.endswith("." + h):
                    s += 3          # 子域（更贴近实际请求的子域名，如 www，优先）
            return s

        best = {}
        for c in allc:
            dom = (c.get("domain") or "").lstrip(".")
            if not any(self._domain_matches(dom, h) for h in hosts):
                continue
            nm = c.get("name")
            cur = best.get(nm)
            if cur is None:
                best[nm] = c
                continue
            cs_, ns_ = _score(cur), _score(c)
            if ns_ > cs_ or (ns_ == cs_ and
                             len(c.get("value", "")) > len(cur.get("value", ""))):
                best[nm] = c
        sel = list(best.values())
        # 没有任何匹配 → 返回空，让上层正确提示「未检测到登录 Cookie」
        # （不再退化成全部 cookie，避免拿错站 cookie 去验活）
        if not sel:
            return "", []
        return "; ".join("%s=%s" % (c["name"], c["value"]) for c in sel), sel

# ------------------------------------------------------------------ 找浏览器

def find_browser():
    """按优先级找 Chrome / Edge 可执行文件"""
    cands = []
    pf = os.path.expandvars(r"%ProgramFiles%")
    pf86 = os.path.expandvars(r"%ProgramFiles(x86)%")
    local = os.path.expandvars(r"%LOCALAPPDATA%")
    for base in (pf, pf86, local):
        if not base:
            continue
        cands += [
            os.path.join(base, "Google", "Chrome", "Application", "chrome.exe"),
            os.path.join(base, "Microsoft", "Edge", "Application", "msedge.exe"),
            os.path.join(base, "360Chrome", "Chrome", "Application", "360chrome.exe"),
            os.path.join(base, "Chromium", "Application", "chrome.exe"),
        ]
    for c in cands:
        if os.path.isfile(c):
            return c
    return ""


if __name__ == "__main__":
    b = Browser(port=9222)
    print("浏览器路径:", find_browser())
    print("调试端口就绪:", b.is_up())
    if b.is_up():
        print("版本:", b.version().get("Browser"))
