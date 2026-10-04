# -*- coding: utf-8 -*-
"""
从 6 个 APK 的 DEX 里挖「模型 → 倍率」硬编码表。
目标：找到 credits / multiplier / rate 与模型名同时出现的结构。
"""
import os
import re
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APKS = ["base.apk", "base(1).apk", "base(2).apk",
        "base(3).apk", "base(4).apk", "base(5).apk"]

# 倍率相关关键词（DEX 里是 ASCII 或 UTF-16）
RATE_WORDS = [b"credits", b"credit", b"multiplier", b"creditRate",
              b"credit_rate", b"creditsMultiplier", b"credits_multiplier",
              b"creditCost", b"costMultiplier", b"pointsPer", b"rate"]
# 模型名特征
MODEL_WORDS = [b"claude", b"gpt-", b"gpt5", b"gemini", b"deepseek", b"glm-",
               b"kimi", b"qwen", b"doubao", b"hunyuan", b"o3", b"o4", b"sonnet",
               b"opus", b"haiku", b"flash", b"pro-"]


def dex_strings(data):
    """粗提 DEX 中的 ASCII / UTF-16 可打印串"""
    out = []
    # ASCII
    for m in re.finditer(rb"[\x20-\x7e]{5,}", data):
        out.append(m.group().decode("ascii", "replace"))
    return out


def dex_u16(data):
    out = []
    for m in re.finditer(rb"(?:[\x20-\x7e]\x00){5,}", data):
        try:
            out.append(m.group().decode("utf-16-le"))
        except Exception:
            pass
    return out


def main():
    for name in APKS:
        p = os.path.join(ROOT, name)
        if not os.path.exists(p):
            print("跳过（不存在）%s" % name)
            continue
        blob = b""
        try:
            with zipfile.ZipFile(p) as z:
                for n in z.namelist():
                    if n.endswith(".dex"):
                        blob += z.read(n)
        except Exception as e:
            print("%s 读取失败 %s" % (name, e))
            continue
        if not blob:
            print("%s 无 dex" % name)
            continue
        strs = dex_strings(blob)
        strs_u = dex_u16(blob)
        all_strs = strs + strs_u
        print("\n" + "=" * 70)
        print("%s  dex=%d B  串=%d(+u16 %d)" % (name, len(blob), len(strs), len(strs_u)))

        # 1) 倍率关键词命中的上下文
        hits = {}
        for w in RATE_WORDS:
            cnt = 0
            for s in all_strs:
                low = s.lower()
                if w.decode().lower() in low and len(s) < 200:
                    cnt += 1
            if cnt:
                hits[w.decode()] = cnt
        print("倍率词命中：%s" % (hits or "无"))

        # 2) 形如 "0.5" / "1.0" 的纯数字串（倍率常以字符串常量出现）
        nums = [s for s in all_strs if re.fullmatch(r"\d+(\.\d+)?", s)]
        from collections import Counter
        print("纯数字串 TOP20：%s" % Counter(nums).most_common(20))

        # 3) 模型名与倍率数字相邻的串
        cand = []
        pats = ("claude", "gpt-", "gemini", "deepseek", "glm-", "kimi",
                "doubao", "hunyuan", "sonnet", "opus", "haiku")
        for s in all_strs:
            low = s.lower()
            if any(w in low for w in pats) and len(s) < 120:
                cand.append(s)
        print("模型名串样例（前 25）：")
        for s in cand[:25]:
            print("   ", s)


if __name__ == "__main__":
    main()
