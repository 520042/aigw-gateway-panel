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
import socket
import struct
import subprocess
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

    def open_tab(self, url):
        import urllib.request
        req = urllib.request.Request(
            "http://127.0.0.1:%d/json/new?%s" % (self.port, url), method="PUT")
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return json.loads(r.read().decode("utf-8", "replace"))
        except Exception:
            return None

    # ---------------------------------------------------------- Cookie
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
        out = []
        for c in allc:
            dom = (c.get("domain") or "").lstrip(".")
            if any(dom and (u.find(dom) >= 0) for u in urls):
                out.append(c)
        return out

    def cookie_header(self, hosts):
        hosts = [h.strip().lower() for h in (hosts or []) if h.strip()]
        cs = self.cookies(["https://" + h if not h.startswith("http") else h
                           for h in hosts] or None)
        sel = []
        for c in cs:
            dom = (c.get("domain") or "").lstrip(".")
            if any(dom == h or dom.endswith("." + h) or h.endswith(dom)
                   for h in hosts):
                sel.append(c)
        if not sel:
            sel = cs
        return "; ".join("%s=%s" % (c["name"], c["value"]) for c in sel), sel

    def close(self):
        if self.proc:
            try:
                self.proc.terminate()
            except Exception:
                pass
            self.proc = None


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
