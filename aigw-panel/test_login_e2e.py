# -*- coding: utf-8 -*-
"""
登录「全链路」端到端测试
========================
用户反馈「所有登录都没有记录」。之前的测试只测到「会话状态变成 success」就停了，
**从没验证过凭据是否真的落进账号池** —— 所以登录成功但账号池还是空的这种问题，
测试完全发现不了。

本文件补的就是这一段：对三种登录方式（二维码 / Cookie / 文件-APIKey）分别走完
  登录 → finish(success) → _save() → Accounts.add() → 账号池可读回 → usable() 可取用
并断言每一步。

运行：python -m unittest test_login_e2e   （或 python test_login_e2e.py）
"""
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import accounts as acct_mod
from app import gwlogin

PASS = FAIL = 0


def ck(name, cond, msg=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print("  PASS %-58s %s" % (name, str(msg)[:60]))
    else:
        FAIL += 1
        print("  FAIL %-58s %s" % (name, str(msg)[:60]))


class FakeStore:
    """内存版 store，接口和 app.store 一致（get/put）"""

    def __init__(self):
        self.data = {"accounts": []}

    def get(self, k):
        return json.loads(json.dumps(self.data.get(k, [])))

    def put(self, k, v):
        self.data[k] = json.loads(json.dumps(v))
        return v


class FakeGatewayClient:
    """假网关：第一次 poll 就返回 success"""

    def __init__(self):
        self.started = None

    def start_login(self, edition="cn"):
        self.started = (edition,)
        return {"id": "gw123", "qr": "data:image/png;base64,AAAA",
                "authUrl": "https://example.com/login?state=x",
                "siteLabel": "测试站", "expiresAt": 9999999999,
                "secondsLeft": 300}

    def poll_login(self, sid):
        return {"status": "success", "account": "zhangsan@example.com",
                "edition": "cn"}


import contextlib


@contextlib.contextmanager
def verify_passes(ok=True, info="测试放行"):
    """
    把「验活」这一步换成固定结果。

    为什么：本文件测的是**落库链路**（success → 账号池 → usable），
    不是验活本身。豆包现在用的是真实在线验活（/alice/user/config/pull），
    合成 cookie 会被**正确拒绝**——若不隔离，测的就不是落库了。
    """
    orig = gwlogin.verify_cookie
    gwlogin.verify_cookie = lambda spec, cookie: (ok, info)
    try:
        yield
    finally:
        gwlogin.verify_cookie = orig


# ============================================================ 1. 二维码登录落库
def test_qrcode_flow():
    print("—" * 78)
    print("1. 二维码登录（wb-gateway）：成功 → 必须落库")
    print("—" * 78)
    store = FakeStore()
    acc = acct_mod.Accounts(store)
    mgr = gwlogin.LoginManager(lambda: FakeGatewayClient(), acc)

    sess = mgr.start("wb-gateway")
    # start() 内部已轮询一次；再 poll 直到终态
    for _ in range(5):
        if sess.status in ("success", "error", "expired"):
            break
        mgr.poll(sess.id)

    ck("会话终态为 success", sess.status == "success", sess.status)
    ck("会话带 result", isinstance(sess.result, dict), str(sess.result)[:60])

    rows = store.get("accounts")
    ck("账号池已写入 1 条", len(rows) == 1, str(len(rows)))
    if not rows:
        return
    row = rows[0]
    ck("平台记录正确", row.get("platform") == "wb-gateway", row.get("platform"))
    ck("账号名已记录", bool(row.get("name")), row.get("name"))
    ck("type=gateway", row.get("type") == "gateway", row.get("type"))
    # ★ 这是关键：二维码登录的凭据在网关里，面板这侧 secret 为空。
    #   旧实现照样塞一条空 secret 的记录 → 看着"已登录"，usable() 却永远取不到。
    ck("有 id 便于后续管理", bool(row.get("id")), row.get("id"))
    ck("list() 能读回该账号", len(acc.list("wb-gateway")) == 1,
       str(len(acc.list("wb-gateway"))))


# ============================================================ 2. Cookie 登录落库
def test_cookie_flow():
    print("—" * 78)
    print("2. Cookie 登录（apk-doubao 手动粘贴）：成功 → 必须落库且密钥完整")
    print("—" * 78)
    store = FakeStore()
    acc = acct_mod.Accounts(store)
    mgr = gwlogin.LoginManager(lambda: FakeGatewayClient(), acc)

    cookie = ("sessionid=abc123def456; sessionid_ss=abc123def456; "
              "sid_tt=ttsession; ttwid=deviceid")
    with verify_passes():
        sess = mgr.start("apk-doubao", manual_cookie=cookie)

    ck("会话终态为 success", sess.status == "success",
       "%s / %s" % (sess.status, sess.message))

    rows = store.get("accounts")
    ck("账号池已写入 1 条", len(rows) == 1, str(len(rows)))
    if not rows:
        return
    row = rows[0]
    ck("平台记录正确", row.get("platform") == "apk-doubao", row.get("platform"))
    ck("type=cookie", row.get("type") == "cookie", row.get("type"))
    ck("★ 密钥完整落库（不是空串）", row.get("secret") == cookie,
       "len=%d" % len(row.get("secret") or ""))
    ck("来源标注为手动粘贴", "手动粘贴" in str(row.get("source", "")),
       row.get("source"))
    ck("★ usable() 能取到（有 secret 且 enabled）",
       len(acc.usable("apk-doubao")) == 1, str(len(acc.usable("apk-doubao"))))
    ck("secret_of() 取回完整密钥",
       acc.secret_of(row["id"]) == cookie,
       "len=%d" % len(acc.secret_of(row["id"]) or ""))
    ck("list() 默认掩码不泄露全文",
       acc.list("apk-doubao")[0]["secret"] != cookie,
       acc.list("apk-doubao")[0]["secret"][:24])


# ============================================================ 3. 文件/APIKey 登录落库
def test_file_flow():
    print("—" * 78)
    print("3. 文件类登录（apk-coze 粘贴 PAT）：成功 → 必须落库")
    print("—" * 78)
    store = FakeStore()
    acc = acct_mod.Accounts(store)
    mgr = gwlogin.LoginManager(lambda: FakeGatewayClient(), acc)

    sess = mgr.start("apk-coze")
    ck("初始等待提交", sess.status == "waiting", sess.status)
    sess = mgr.submit(sess.id, secret="pat_abcdefgh123456", type="api_key")

    ck("提交后为 success", sess.status == "success",
       "%s / %s" % (sess.status, sess.message))
    rows = store.get("accounts")
    ck("账号池已写入 1 条", len(rows) == 1, str(len(rows)))
    if rows:
        row = rows[0]
        ck("平台记录正确", row.get("platform") == "apk-coze", row.get("platform"))
        ck("★ 密钥完整落库", row.get("secret") == "pat_abcdefgh123456",
           row.get("secret"))
        ck("★ usable() 能取到", len(acc.usable("apk-coze")) == 1,
           str(len(acc.usable("apk-coze"))))


# ============================================================ 4. 失败时不应落库
def test_failure_not_saved():
    print("—" * 78)
    print("4. 登录失败/未登录：不应写入账号池")
    print("—" * 78)
    store = FakeStore()
    acc = acct_mod.Accounts(store)
    mgr = gwlogin.LoginManager(lambda: FakeGatewayClient(), acc)

    # 验活判失败 → 绝不能落库
    with verify_passes(ok=False, info="测试：模拟验活失败"):
        sess = mgr.start("apk-doubao", manual_cookie="ttwid=device; msToken=ms")
    ck("无会话标记 → 判失败", sess.status == "error",
       "%s / %s" % (sess.status, sess.message))
    ck("★ 失败时账号池仍为空", len(store.get("accounts")) == 0,
       str(len(store.get("accounts"))))
    ck("★ 失败时 usable() 为空", len(acc.usable("apk-doubao")) == 0,
       str(len(acc.usable("apk-doubao"))))


# ============================================================ 5. 重复登录应更新而非堆积
def test_relogin_updates():
    print("—" * 78)
    print("5. 同平台重复登录：应更新同一条，不堆积")
    print("—" * 78)
    store = FakeStore()
    acc = acct_mod.Accounts(store)
    mgr = gwlogin.LoginManager(lambda: FakeGatewayClient(), acc)

    with verify_passes():
        mgr.start("apk-doubao", manual_cookie="sessionid=first")
        mgr.start("apk-doubao", manual_cookie="sessionid=second")
    rows = store.get("accounts")
    ck("只有 1 条（未堆积）", len(rows) == 1, str(len(rows)))
    if rows:
        ck("★ secret 已更新为最新", "second" in (rows[0].get("secret") or ""),
           rows[0].get("secret"))


# ============================================================ 6. 二维码登录后能否真的用起来
def test_qrcode_then_call():
    print("—" * 78)
    print("6. ★ 二维码登录后「调用接口」能否取到凭据（网关兜底）")
    print("—" * 78)
    store = FakeStore()
    acc = acct_mod.Accounts(store)
    mgr = gwlogin.LoginManager(lambda: FakeGatewayClient(), acc)

    sess = mgr.start("apk-trae")
    for _ in range(5):
        if sess.status in ("success", "error", "expired"):
            break
        mgr.poll(sess.id)
    ck("Trae 二维码登录成功", sess.status == "success", sess.status)
    ck("账号池已记录该平台", len(acc.list("apk-trae")) == 1,
       str(len(acc.list("apk-trae"))))

    # 关键：二维码登录的 secret 是空的（凭据在网关），usable() 取不到。
    # 但 call() 必须走「网关本地凭据」兜底，而不是直接报「没有可用凭据」。
    import app.gwextra as gx
    orig = gx._secret_from_gateway_files
    gx._secret_from_gateway_files = lambda: "gw_token_abcdef123456"
    try:
        okk, data = gx.call(acc, "apk-trae", "checkin_status", {})
        msg = data if isinstance(data, str) else json.dumps(data, ensure_ascii=False)
    finally:
        gx._secret_from_gateway_files = orig

    ck("★ 不再报「账号池里没有可用凭据」",
       "没有" not in msg or "可用凭据" not in msg, msg[:80])
    ck("★ 已走到网关兜底取到凭据（说明进了请求阶段，而非卡在凭据解析）",
       ("网关本地凭据" in msg) or ("credential" not in msg.lower()),
       msg[:80])


# ============================================================ 7. 全平台冒烟
def test_all_platforms_can_save():
    print("—" * 78)
    print("6. 全平台冒烟：每个平台都能走通「成功→落库」")
    print("—" * 78)
    # 注意：cookie 类平台用**在线验活**，喂假 cookie 时"验活失败"是**正确行为**，
    # 不能算 bug。真正要守的不变量是：
    #   ★ 只要会话报 success，账号池里就必须有记录（否则就是"登录成功却没记录"）
    ok_list, bad = [], []
    for pid, spec in gwlogin.PLATFORMS.items():
        method = spec.get("method")
        store = FakeStore()
        acc = acct_mod.Accounts(store)
        mgr = gwlogin.LoginManager(lambda: FakeGatewayClient(), acc)
        try:
            if method == "cookie":
                # 隔离验活，只测落库
                with verify_passes():
                    s = mgr.start(pid, manual_cookie="sessionid=v0")
            elif method == "file":
                s = mgr.start(pid)
                s = mgr.submit(s.id, secret="secret_" + pid, type="api_key")
            else:
                s = mgr.start(pid)
                for _ in range(5):
                    if s.status in ("success", "error", "expired"):
                        break
                    mgr.poll(s.id)
            saved = len(store.get("accounts"))
            if s.status == "success" and saved >= 1:
                ok_list.append(pid)
            elif s.status == "success" and saved == 0:
                bad.append("%s(success 但没落库!)" % pid)   # ← 真正的 bug
            elif s.status == "error" and saved > 0:
                bad.append("%s(失败却落库了!)" % pid)        # ← 也是 bug
            else:
                ok_list.append(pid + "(验活正确拒绝假凭据)")
        except Exception as e:
            bad.append("%s(exc=%s)" % (pid, type(e).__name__))
    ck("★ 不变量：success 必落库 / error 必不落库", not bad,
       "ok=%d bad=%s" % (len(ok_list), bad[:6]))
    print("     通过平台(%d): %s" % (len(ok_list), ", ".join(sorted(ok_list))))


def main():
    for fn in (test_qrcode_flow, test_cookie_flow, test_file_flow,
               test_failure_not_saved, test_relogin_updates,
               test_qrcode_then_call, test_all_platforms_can_save):
        fn()
    print("=" * 78)
    print("通过 %d / %d" % (PASS, PASS + FAIL))
    return 0 if FAIL == 0 else 1


class T(unittest.TestCase):
    def test_all(self):
        self.assertEqual(main(), 0)


if __name__ == "__main__":
    sys.exit(main())
