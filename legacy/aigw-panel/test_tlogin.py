# -*- coding: utf-8 -*-
"""
原生登录链路端到端测试
======================
不依赖 workbuddy-gateway 进程，走面板自己的 /api/tencent 接口。

测的是「面板能不能只靠自己完成登录 → 拿 token → 拉模型倍率」，
这正是用户要求的「只启动一个软件」。

前置：浏览器已经登录过 codebuddy.cn（CDP 端口 9333 上的那个）。

运行：python test_tlogin.py
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

OK = []
BAD = []


def check(name, cond, extra=""):
    (OK if cond else BAD).append(name)
    print("  %s %s %s" % ("PASS" if cond else "FAIL", name, extra))


print("场景 1：tlogin 模块自洽（不碰网络）")
from app import tlogin                                   # noqa: E402

st = tlogin.new_state()
check("new_state 生成非空", bool(st), st)
check("new_state 每次不同", tlogin.new_state() != st)
u = tlogin.login_url(st)
check("login_url 带 state", "state=%s" % st in u, u)
check("login_url platform=CLI", "platform=CLI" in u)
check("login_url 用 copilot.tencent.com",
      u.startswith("https://copilot.tencent.com/login"), u)
check("UA 是网关伪装串", tlogin.UA == "CLI/2.143.1 CodeBuddy/2.143.1", tlogin.UA)
check("HDRS 带 X-Ide-Type", tlogin.HDRS.get("X-Ide-Type") == "production")

print("\n场景 2：凭据落盘 / 读取")
cred = {"state": "s1", "accessToken": "AT." + "x" * 40,
        "refreshToken": "RT." + "y" * 40, "expire": 3283200}
p = tlogin.save(cred, os.path.join(HERE, "data", "_t_cred.json"))
check("save 返回路径", os.path.exists(p), p)
back = tlogin.load(os.path.join(HERE, "data", "_t_cred.json"))
check("load 还原 accessToken", back.get("accessToken") == cred["accessToken"])
os.remove(p)

print("\n场景 3：parse_models 归一化（倍率不能丢）")
raw = {"data": {"models": [
    {"id": "hy3", "name": "Hy3", "credits": "x0.00 credits",
     "maxInputTokens": 192000, "maxOutputTokens": 64000,
     "supportsImages": True, "supportsToolCall": True, "vendor": "j"},
    {"id": "auto", "name": "Auto", "maxInputTokens": 256000,
     "maxOutputTokens": 32000, "supportsImages": True,
     "supportsToolCall": True},
    {"id": "hunyuan-image-alpha", "name": "Hunyuan Image Alpha",
     "tags": ["text-to-image"]},
]}}
ms = tlogin.parse_models(raw)
check("解析出 3 个", len(ms) == 3, str(len(ms)))
check("credits 保留", ms[0]["credits"] == "x0.00 credits", ms[0]["credits"])
check("缺 credits 归一为空串", ms[1]["credits"] == "")
check("source 标记为 online", all(m["source"] == "online" for m in ms))
check("数值字段兜底为 int",
      ms[2]["maxInputTokens"] == 0 and ms[2]["maxOutputTokens"] == 0)
check("无 models 时返回空表", tlogin.parse_models({"data": {}}) == [])
check("非 dict 输入不炸", tlogin.parse_models(None) == []
      and tlogin.parse_models({"data": {"models": "x"}}) == [])

print("\n场景 4：Tencent 类集成（真 token 拉在线目录）")
from app.tencent import Tencent                          # noqa: E402
tc = Tencent("")
got, c = tc.load_cred()
if not got:
    print("  SKIP 没有已保存凭据，先跑 tlogin.py --token")
else:
    check("token 已装载到实例", len(tc.token) > 100, "len=%d" % len(tc.token))
    cat = tc.catalog()
    check("catalog 拿到模型", len(cat) > 10, "count=%d" % len(cat))
    withcred = [m for m in cat if m["credits"]]
    check("多数模型带倍率", len(withcred) >= len(cat) * 0.6,
          "%d/%d" % (len(withcred), len(cat)))
    v = tlogin.verify(tc.token)
    check("/v3/config 验活 200", v["rates"][0] == 200, str(v["rates"][0]))
    check("在线 models 验活 200", v["models"][0] == 200, str(v["models"][0]))
    # 倍率里应该有明确的数值
    nums = [m["credits"] for m in withcred if any(ch.isdigit() for ch in m["credits"])]
    check("倍率含具体数值", len(nums) > 5, "样例 %s" % nums[:4])

print("\n通过 %d / 失败 %d" % (len(OK), len(BAD)))
if BAD:
    print("失败项：", BAD)
sys.exit(1 if BAD else 0)
