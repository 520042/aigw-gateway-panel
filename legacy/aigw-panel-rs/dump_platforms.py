#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
一次性生成工具：把现有 Python 面板 app.gwlogin.PLATFORMS 导出成
aigw-panel-rs/src/platforms.json，供 Rust 端（过渡期）直接 include_str! 加载。

这是「单一数据源」机制——平台清单仍由 Python 源文件保证正确，
Rust 不手抄 48 个平台规格（避免臆测/重复）。后续阶段再把这份结构
port 成 Rust 原生 struct。
"""
import json
import os
import sys

ROOT = os.path.dirname(os.path.abspath(__file__))
AIGW = os.path.join(ROOT, "..", "aigw-panel")
OUT = os.path.join(ROOT, "src", "platforms.json")

sys.path.insert(0, AIGW)
try:
    from app import gwlogin
except Exception as e:  # noqa
    print("IMPORT_FAIL", repr(e))
    sys.exit(2)

plats = getattr(gwlogin, "PLATFORMS", {})
print("platform count:", len(plats))
print("pids:", sorted(plats.keys()))

text = json.dumps(plats, ensure_ascii=False, indent=1, default=str)
os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w", encoding="utf-8") as f:
    f.write(text)
print("written:", OUT, len(text), "bytes")
