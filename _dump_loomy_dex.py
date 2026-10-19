# -*- coding: utf-8 -*-
"""从 base.apk 的 dex 提取全部可打印字符串，过滤 loomy/鉴权相关。"""
import zipfile, re

z = zipfile.ZipFile('base.apk')
raw = z.read('classes.dex')
# dex 字符串池：抓所有可打印 ASCII 运行（≥5）
strs = re.findall(rb'[\x20-\x7e]{5,}', raw)
strs = [x.decode('ascii') for x in strs]

def show(title, pats, limit=30):
    print('\n===', title, '===')
    out = []
    for t in strs:
        for pat in pats:
            if re.search(pat, t, re.I):
                out.append(t[:150])
                break
    for t in sorted(set(out))[:limit]:
        print(' ', t)

show('loomy 上下文', [r'loomy'])
show('鉴权头/token 猜测（/api/ + auth）', [r'api/v\d.+(token|auth|login)', r'(token|auth).{0,40}api/v\d'])
show('loomyad 域名相关', [r'loomyad'])
