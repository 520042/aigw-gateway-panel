#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
二维码真伪检测回归测试（2026-10-06）

背景：面板在服务端无头浏览器里抓登录页二维码回传，抓错图用户就扫不出来。
本文件用**真实站点截图**（/tmp 下历史 E2E 产物）做回归。

★ 期望值以「人眼逐张放大确认的真实内容」为准（用 PIL 逐张看过），不是猜测：
  真码：圆角码+头像水印（豆包）、纯黑白码（小浣熊）、微信 qrconnect（元宝）、
        圆形艺术码（秘塔）、圆角纯码（WPS）、标准大方码（秘塔 3s 抓取）
  非码：蓝色渐变插画、深色 logo、loading 转圈、邮戳插画、整页 UI

用法：python3 test_qr_detect.py [-v]
"""
import base64
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from app.gwlogin import _looks_like_qr, _is_blank_png  # noqa: E402

# (路径, 期望 _looks_like_qr 结果, 说明)
CASES = [
    # ---------------- 真二维码（期望 True） ----------------
    ("/tmp/e2e_doubao_qr.png", True, "豆包·圆角码(带头像水印)"),
    ("/tmp/doubao_qr_final.png", True, "豆包·最终抓取码"),
    ("/tmp/yb_qr_iframe.png", True, "元宝·微信 qrconnect 码"),
    ("/tmp/fin_apk-raccoon.png", True, "小浣熊·纯黑白码"),
    ("/tmp/fin_apk-wps.png", True, "WPS·圆角纯码"),
    ("/tmp/fin_apk-metaso.png", True, "秘塔·圆形艺术码"),
    ("/tmp/t_metaso_3s.png", True, "秘塔·标准大方码"),

    # ---------------- 干扰图 / 整页（期望 False） ----------------
    # ★ 小浣熊这一帧是「同意并继续」同意页弹出前抓到的：码被半透明遮罩盖住、
    #   主体是暗色卡片，扫不出来。正确行为是**拒绝、继续重试**（看守线程下一轮
    #   会先点掉「同意并继续」再抓）。它不代表失败，而是兜底逻辑生效的证据。
    ("/tmp/E2E_apk-raccoon.png", False, "小浣熊·同意页遮挡帧(应拒)"),
    ("/tmp/E2E_apk-yuanbao.png", False, "元宝·E2E 空白(码未渲染)"),
    ("/tmp/E2E_apk-metaso.png", False, "秘塔·loading 转圈"),
    ("/tmp/vf_apk-qwenwork.png", False, "千问·方形邮戳插画"),
    ("/tmp/doubao_qr_svg.png", False, "豆包·蓝色渐变插画"),
    ("/tmp/yuanbao_qr.png", False, "元宝·深色 logo"),
    ("/tmp/doubao_login.png", False, "豆包·登录页整页"),
    ("/tmp/doubao_dialog.png", False, "豆包·登录弹窗整页"),
    ("/tmp/doubao_dialog2.png", False, "豆包·登录弹窗整页 2"),
    ("/tmp/qw_full.png", False, "千问·首页整页"),
    ("/tmp/ui_sources.png", False, "面板·源列表整页"),
    ("/tmp/yuanbao_qr2.png", False, "元宝·780x437 截图(码太小)"),
    ("/tmp/vf_apk-nano.png", False, "纳米·非码图"),
    ("/tmp/t_metaso_full.png", False, "秘塔·整页截图"),
    ("/tmp/yb_state.png", False, "元宝·整页截图"),
]


def load_b64(path):
    with open(path, "rb") as f:
        return base64.b64encode(f.read()).decode()


def main():
    verbose = "-v" in sys.argv
    ok = miss = skip = 0
    fails = []
    for path, expect, label in CASES:
        if not os.path.exists(path) or os.path.getsize(path) == 0:
            skip += 1
            if verbose:
                print("SKIP  %-40s（文件不存在/空）" % label)
            continue
        try:
            b = load_b64(path)
            got = _looks_like_qr(b)
        except Exception as e:
            miss += 1
            fails.append((label, expect, "异常 %s" % e))
            print("ERROR %-40s %s" % (label, e))
            continue
        if got == expect:
            ok += 1
            if verbose:
                print("PASS  %-40s look=%s" % (label, got))
        else:
            miss += 1
            fails.append((label, expect, got))
            print("FAIL  %-40s expect=%-5s got=%-5s" % (label, expect, got))

    total = ok + miss
    print("\n准确率: %d/%d  (skip %d)" % (ok, total, skip))
    if fails:
        print("失败项：")
        for label, exp, got in fails:
            print("  · %s  期望=%s 实际=%s" % (label, exp, got))
    return 0 if not fails else 1


if __name__ == "__main__":
    sys.exit(main())
