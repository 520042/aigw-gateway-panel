# -*- coding: utf-8 -*-
"""
精确定位 DEX 里的倍率表：找 "credits" 字面量在文件中的偏移，
dump 其前后 512 字节内的所有可打印串，还原 {模型名, 倍率} 配对结构。
"""
import os
import re
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGETS = sys.argv[1:] or ["base(3).apk", "base.apk", "base(4).apk"]
WORDS = [b"credits", b"credit", b"multiplier", b"creditRate", b"credit_rate",
         b"creditsMultiplier", b"credits_multiplier", b"creditCost"]


def strings_near(blob, pos, span=400):
    lo = max(0, pos - span)
    hi = min(len(blob), pos + span)
    seg = blob[lo:hi]
    out = []
    for m in re.finditer(rb"[\x20-\x7e]{3,}", seg):
        out.append((lo + m.start(), m.group().decode("ascii", "replace")))
    return out


def main():
    for name in TARGETS:
        p = os.path.join(ROOT, name)
        if not os.path.exists(p):
            continue
        blob = b""
        with zipfile.ZipFile(p) as z:
            for n in sorted(z.namelist()):
                if n.endswith(".dex"):
                    blob += z.read(n)
        print("\n" + "=" * 72)
        print("%s   dex %d B" % (name, len(blob)))
        seen_ctx = set()
        for w in WORDS:
            for m in re.finditer(re.escape(w), blob):
                near = strings_near(blob, m.start(), 260)
                # 用邻近串集合去重
                key = "|".join(s for _, s in near[:26])
                if key in seen_ctx:
                    continue
                seen_ctx.add(key)
                print("\n--- 命中 %r @0x%X ---" % (w.decode(), m.start()))
                for off, s in near[:30]:
                    print("   0x%08X  %s" % (off, s[:110]))
                if len(seen_ctx) >= 14:
                    break
            if len(seen_ctx) >= 14:
                break


if __name__ == "__main__":
    main()
