# -*- coding: utf-8 -*-
"""
豆包内置模型清单（从网页版 UI 抓取，勿手改）

来源与可信度
------------
2026-10-05 用面板自己的 CDP 浏览器打开 www.doubao.com/chat/，
把整页 HTML dump 下来（data/page_doubao.html），从其中的
`model_list.item_list` 结构里解析出模型选项 —— **这就是网页版
「模型选择」菜单里的真实条目**（用户指出"网页版明明有模型选项"）。

字段说明
--------
model_item_key 是网页版选择模型用的键（对话请求里用）；
name 是界面展示名。没有登录态时页面也带这份配置，所以免登录可用。
"""

GENERATED_FROM = "www.doubao.com/chat/ 页面内嵌 model_list.item_list"
GENERATED_NOTE = "2026-10-05 从豆包网页版 SSR 配置解析（dump 见 data/page_doubao.html）"

MODELS = [
    {'id': 'auto', 'name': '自动', 'key': '9',
     'desc': '网页版默认的自动选择'},
    {'id': 'doubao-fast', 'name': '豆包 快速', 'key': '0',
     'desc': '低延迟快速对话'},
    {'id': 'doubao-2.1-turbo', 'name': '豆包 2.1 Turbo', 'key': '3',
     'desc': '2.1 Turbo（菜单键 3）'},
    {'id': 'doubao-2.1-turbo-alt', 'name': '豆包 2.1 Turbo', 'key': '4',
     'desc': '2.1 Turbo（同名的另一菜单项）'},
    {'id': 'doubao-2.1-pro', 'name': '豆包 2.1 Pro', 'key': '5',
     'desc': '2.1 Pro，能力最强'},
    {'id': 'doubao-2.1-lite', 'name': '豆包 2.1 Lite', 'key': 'seed-lite-7b',
     'desc': '2.1 Lite 轻量版'},
]

# 菜单里真实出现的展示名（去重后），便于界面直接显示
NAMES = ['自动', '豆包 快速', '豆包 2.1 Turbo', '豆包 2.1 Pro', '豆包 2.1 Lite']

# model_item_key → 友好展示名（实时接口只给短名/key，UI 用这个映射成可读名）
NAMES_BY_KEY = {m['key']: m['name'] for m in MODELS}


def stats():
    return {"count": len(MODELS), "source": GENERATED_FROM}


def rate_of(mid):
    """豆包网页版不公开倍率，这里恒返回未观测（保持与三源合并的兼容）"""
    return None, "豆包网页版无公开倍率"


def friendly(key):
    """把实时接口返回的 model_item_key 映射成可读名；没有映射就用 key 本身"""
    return NAMES_BY_KEY.get(str(key), str(key))

# ========================================================
# APK 网关自有模型 id（base(1).apk dev.doubao2api dex 提取）
# —— 走 dev.doubao2api 本地网关时用这组 id；网页版菜单键见上 MODELS
APK_GATEWAY_MODELS = [
    {'id': 'doubao-pro', 'name': '豆包 Pro', 'desc': 'APK 网关自有 id：常规对话'},
    {'id': 'doubao-think', 'name': '豆包 Think', 'desc': 'APK 网关自有 id：思考模式'},
    {'id': 'doubao-expert', 'name': '豆包 Expert', 'desc': 'APK 网关自有 id：专家/深度'},
    {'id': 'doubao-image', 'name': '豆包 Image', 'desc': 'APK 网关自有 id：文生图（/v1/images/generations）'},
    {'id': 'doubao-music', 'name': '豆包 Music', 'desc': 'APK 网关自有 id：音乐生成'},
    {'id': 'doubao-video', 'name': '豆包 Video', 'desc': 'APK 网关自有 id：视频生成'},
]
