# -*- coding: utf-8 -*-
"""
抓 workbuddy-gateway 的 OAuth 登录完整链路
=========================================
背景：面板要彻底不依赖 workbuddy-gateway 进程，就必须自己走完
      「生成 state → 拼扫码 URL → 轮询 token → 换 access_token」这条链。
      逆向只能看到二进制里的字符串，看不到运行时 state 的真实生成规则。

做法：起一个**独立 profile** 的可见 Chrome（不碰你日常浏览器），
      打开 copilot.tencent.com 登录页，挂 CDP Network 域全量录制，
      你在窗口里扫码登录 —— 录完回车即可。

运行：
    python capture_login.py          # 启动 + 录制（Ctrl+C 停止并打印链路）
    python capture_login.py --list   # 回看已抓到的链路
    python capture_login.py --analyze  # 只做链路分析，不开浏览器
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from app.cdp import Recorder, find_browser        # noqa: E402

OUT = os.path.join(HERE, "data", "capture.jsonl")
PROFILE = os.path.join(HERE, "data", "chrome-capture-profile")
PORT = 9333

# 登录页必须带 platform 和 state，缺 state 会报「登录链接不完整」。
# 实测 /v2/plugin/auth/token?state=xxx 对任意 state 都返回 11217「登录中」，
# 说明 state 只是客户端本地生成、服务端按它跟踪会话的关联键，不校验格式。
PLATFORM = "CLI"


def new_state():
    import uuid
    return str(uuid.uuid4())


def login_url(state):
    return "https://copilot.tencent.com/login?platform=%s&state=%s" % (
        PLATFORM, state)

# 关心的路径片段（小写匹配）
KEYS = ("auth/token", "auth/state", "oauth", "login", "qrcode", "qr_code",
        "/v2/plugin/", "/v3/config", "/console/", "checkin", "meter",
        "callback", "redirect", "exchange", "refresh", "user/info", "profile")


def _rows():
    if not os.path.exists(OUT):
        return []
    out = []
    with open(OUT, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except Exception:
                pass
    return out


def _hit(r):
    u = (r.get("url") or "").lower()
    return any(k in u for k in KEYS)


def analyze(verbose=True):
    rows = _rows()
    if not rows:
        print("没有抓到任何请求：%s" % OUT)
        return 2
    reqs = [r for r in rows if r.get("kind") == "request" and _hit(r)]
    resps = {r.get("reqId"): r for r in rows if r.get("kind") == "response"}
    print("=" * 78)
    print("录制文件：%s" % OUT)
    print("总事件 %d 条，其中关键请求 %d 条" % (len(rows), len(reqs)))
    print("=" * 78)
    for r in reqs:
        rs = resps.get(r.get("requestId") or "")
        code = ("%s" % rs.get("status")) if rs else "-"
        print("\n[%s] %s %s   -> %s" % (r.get("wall"), r.get("method"),
                                         r.get("url"), code))
        if r.get("post"):
            print("   POST: %s" % r["post"][:500])
        for k, v in (r.get("headers") or {}).items():
            if k.lower() in ("authorization", "cookie", "x-ide-type",
                             "x-ide-name", "content-type", "referer"):
                print("   %s: %s" % (k, str(v)[:200]))
    # 顺手把 state / token 之类的关键值抠出来
    print("\n" + "=" * 78)
    print("关键值提取")
    print("=" * 78)
    import re
    seen = set()
    for r in reqs:
        u = r.get("url") or ""
        for m in re.findall(r"(state|code|token|access_token|refresh_token|"
                            r"ticket|qrcode|qrCode|qrid)=([^&\s\"']+)", u):
            k, v = m
            if (k, v) in seen:
                continue
            seen.add((k, v))
            print("  %-14s = %s" % (k, v[:120]))
    # 响应体里的 token
    for r in rows:
        if r.get("kind") == "body":
            b = r.get("body") or ""
            if any(k in b for k in ("token", "Token", "openid", "openId")):
                print("\n  [响应体] %s" % b[:600])
    # 轮询结果
    polls = [r for r in rows if r.get("kind") == "poll"]
    if polls:
        print("\n" + "-" * 78)
        print("token 轮询（state=%s）" % polls[-1].get("state"))
        print("-" * 78)
        for r in polls:
            print("  %s -> %s" % (r.get("wall"),
                                   json.dumps(r.get("data"), ensure_ascii=False)[:400]))
    return 0


def main():
    if "--list" in sys.argv or "--analyze" in sys.argv:
        return analyze()

    exe = find_browser()
    if not exe:
        print("没找到 Chrome / Edge")
        return 2
    if os.path.exists(OUT):
        os.remove(OUT)

    state = new_state()
    url = login_url(state)
    print("本地生成 state : %s" % state)
    print("打开的登录页   : %s" % url)

    rec = Recorder(port=PORT, out=OUT, user_data_dir=PROFILE, exe=exe)
    print("浏览器: %s" % exe)
    rec.start(url)
    print("=" * 78)
    print("已挂上 CDP Network 域，正在录制。请在浏览器窗口里完成登录（扫码/账密）。")
    print("登录完成后回到这里按 Ctrl+C 停止，我会立刻打印完整链路。")
    print("=" * 78)

    # 后台按 2s 轮询 token，和网关行为一致；顺手验证 state 链路是通的
    import threading
    poll_log = []

    def poller():
        sys.path.insert(0, HERE)
        from app.tencent import Tencent
        tc = Tencent("")
        t0 = time.time()
        while time.time() - t0 < 900 and not rec._stop.is_set():
            try:
                ok, d = tc.call("oauth_token", state=state)
                code = (d or {}).get("code") if isinstance(d, dict) else None
                msg = (d or {}).get("msg") if isinstance(d, dict) else str(d)[:80]
                sig = (code, str(msg)[:40])
                if not poll_log or poll_log[-1][1] != sig:
                    poll_log.append((time.strftime("%H:%M:%S"), sig))
                    print("  [轮询] code=%s msg=%s" % (code, msg))
                if code not in (None, 11217):
                    with open(OUT, "a", encoding="utf-8") as f:
                        f.write(json.dumps({
                            "kind": "poll",
                            "wall": time.strftime("%H:%M:%S"),
                            "state": state,
                            "data": d,
                        }, ensure_ascii=False) + "\n")
            except Exception as e:
                pass
            time.sleep(2)

    threading.Thread(target=poller, daemon=True).start()

    try:
        seen = set()
        while True:
            time.sleep(1)
            for r in rec.rows()[-60:]:
                if r.get("kind") == "request" and _hit(r):
                    key = (r.get("method"), r.get("url"))
                    if key not in seen:
                        seen.add(key)
                        print("  · %-4s %s" % (r.get("method"),
                                               (r.get("url") or "")[:120]))
    except KeyboardInterrupt:
        pass
    finally:
        n = rec.stop()
        print("\n录制结束，事件 %d 条 -> %s" % (n, OUT))
    return analyze()


if __name__ == "__main__":
    sys.exit(main())
