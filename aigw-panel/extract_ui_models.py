# -*- coding: utf-8 -*-
"""从豆包/元宝页面 HTML 里抠出真实的模型选项列表"""
import re
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data")


def balanced(s, i):
    """从 s[i]（'{''['）开始取平衡块，正确跳过字符串与转义"""
    op = s[i]
    cl = {"{": "}", "[": "]"}[op]
    d = 0
    j = i
    n = len(s)
    while j < n:
        c = s[j]
        if c == '"':
            j += 1
            while j < n and s[j] != '"':
                if s[j] == "\\":
                    j += 1
                j += 1
        elif c == op:
            d += 1
        elif c == cl:
            d -= 1
            if d == 0:
                return s[i:j + 1]
        j += 1
    return None


def extract(html, anchor_key, list_key="item_list"):
    out = []
    for m in re.finditer(r'"%s"\s*:\s*' % anchor_key, html):
        blk = balanced(html, m.end())
        if not blk:
            continue
        try:
            d = json.loads(blk)
        except Exception:
            continue
        items = d.get(list_key) if isinstance(d, dict) else None
        if items:
            out.append(d)
    return out


def show(name, html, anchor):
    blocks = extract(html, anchor)
    print("=" * 70)
    print(name, "anchor=%s 命中 %d 块" % (anchor, len(blocks)))
    seen = set()
    for d in blocks:
        for it in (d.get("item_list") or []):
            if not isinstance(it, dict):
                continue
            keys = ("name", "key", "item_key", "model_item_key",
                    "description", "desc", "mode_id", "bot_id", "id")
            row = {k: it.get(k) for k in keys if it.get(k) not in (None, "")}
            sig = json.dumps(row, ensure_ascii=False, sort_keys=True)
            if sig in seen:
                continue
            seen.add(sig)
            print("  -", sig[:220])
    return len(seen)


y = open(os.path.join(DATA, "page_yuanbao.html"), encoding="utf-8").read()
d = open(os.path.join(DATA, "page_doubao.html"), encoding="utf-8").read()

show("豆包", d, "model_list")
show("豆包", d, "support_models")
show("元宝", y, "model_list")
