# -*- coding: utf-8 -*-
"""
全平台凭据验活测试（正向 + 反向）
==================================
用户要求：除了 WorkBuddy，其他平台的账号凭据都要实测。

两个方向的验证缺一不可：
  正向：真凭据 → 必须 PASS
  反向：**假凭据 → 必须 FAIL**。只做正向的话，验活端点选错
        （比如拿一个免登录就能 200 的接口）也会显示「已登录」，
        等于没验 —— 本项目就踩过这个坑（/v3/config 免登录 200）。

运行：python test_platform_creds.py
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

OK, BAD = [], []


def check(name, cond, extra=""):
    (OK if cond else BAD).append(name)
    print("  %s %s %s" % ("PASS" if cond else "FAIL", name, extra))


def get_token():
    try:
        from app import tlogin
        return (tlogin.load() or {}).get("accessToken") or ""
    except Exception:
        return ""


def main():
    from app.gwlogin import PLATFORMS, verify_cookie
    tok = get_token()

    print("=" * 92)
    print("平台清单完整性（%d 个平台）" % len(PLATFORMS))
    print("=" * 92)
    with_v = [p for p, s in PLATFORMS.items() if s.get("verify")]
    without = [p for p, s in PLATFORMS.items() if not s.get("verify")]
    check("verify 覆盖率 >= 85%%", len(with_v) >= len(PLATFORMS) * 0.85,
          "%d/%d" % (len(with_v), len(PLATFORMS)))
    check("缺 verify 的都标了 verify_pending",
          all(PLATFORMS[p].get("verify_pending") for p in without),
          "缺: %s" % ", ".join(without))

    print("\n每个平台的 verify 定义必须自洽")
    for pid, spec in PLATFORMS.items():
        v = spec.get("verify")
        if not v:
            continue
        u = v.get("url") or ""
        check("%-22s verify 是 dict" % pid, isinstance(v, dict))
        check("%-22s verify 有 url" % pid, u.startswith("http"), u[:50])
        # 只有 copilot.tencent.com 系才必须伪装成 CodeBuddy CLI 的 UA；
        # 别的站（Google / 扣子）用它们自家的 UA，不该套这条断言。
        if "copilot.tencent.com" in u:
            check("%-22s copilot 系带伪装 UA" % pid,
                  (v.get("ua") or "") == "CLI/2.143.1 CodeBuddy/2.143.1",
                  "少了会报 12403 check ua")

    print("\n%s" % ("=" * 92))
    print("★ 反向验证：假凭据必须被判失败（这是验活的意义所在）")
    print("=" * 92)
    if not tok:
        print("  SKIP 本机没有 CodeBuddy token，先跑 app/tlogin.py --token")
        return 1
    fake = tok[:-40] + "x" * 40
    copilot_like = [p for p, s in PLATFORMS.items()
                    if isinstance(s.get("verify"), dict)
                    and "copilot.tencent.com" in (s["verify"].get("url") or "")]
    print("copilot 系平台：%s" % ", ".join(copilot_like))
    for pid in copilot_like:
        ok, msg = verify_cookie(PLATFORMS[pid], fake)
        check("假 token 被拒 · %s" % pid, not ok, msg[:40])

    print("\n" + "=" * 92)
    print("★ 正向验证：真凭据必须通过，并显示用户标识")
    print("=" * 92)
    for pid in copilot_like:
        ok, msg = verify_cookie(PLATFORMS[pid], tok)
        check("真 token 通过 · %s" % pid, ok, msg[:40])

    print("\n" + "=" * 92)
    print("特例：只收 Cookie 的平台不能误判")
    print("=" * 92)
    # 拿一个格式不对的「cookie」去验 cookie 类平台，应该判失败而不是崩
    for pid in ("apk-doubao", "apk-yuanbao", "apk-raccoon"):
        spec = PLATFORMS[pid]
        if not spec.get("verify"):
            continue
        try:
            ok, msg = verify_cookie(spec, "garbage_not_a_cookie")
            check("垃圾 Cookie 被拒 · %s" % pid, not ok, msg[:44])
        except Exception as e:
            check("垃圾 Cookie 被拒 · %s" % pid, False,
                  "抛异常 %s" % type(e).__name__)

    print("\n空凭据不应崩")
    for pid in list(PLATFORMS)[:5]:
        try:
            ok, msg = verify_cookie(PLATFORMS[pid], "")
            check("空凭据不崩 · %s" % pid, ok is not None, msg[:34])
        except Exception as e:
            check("空凭据不崩 · %s" % pid, False, type(e).__name__)

    print("\n通过 %d / 失败 %d" % (len(OK), len(BAD)))
    if BAD:
        print("失败项：", BAD)
    return 1 if BAD else 0


if __name__ == "__main__":
    sys.exit(main())
