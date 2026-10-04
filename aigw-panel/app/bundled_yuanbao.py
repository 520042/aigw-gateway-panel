# -*- coding: utf-8 -*-
"""
元宝内置模型清单（由 gen_yuanbao_models.py 从 APK 提取，勿手改）

为什么需要这个
--------------
元宝（腾讯）**没有模型列表接口**。base(5).apk（dev.yuanbao2api）里出现的
`/v1/models` 是**这个 APP 自己的本地网关**端点，不是腾讯的接口 ——
拿它去问 yuanbao.tencent.com 只会得到 401，看起来像"要鉴权"，
其实是**接口根本不存在**。

模型清单是硬编码在 APK 的 dex 字符串池里（`innerModels` 邻域），
所以面板要列元宝的模型，只能用这份内置清单。

模型 id 与展示名都来自 dex 实测，上游端点来自 dex 里的完整 URL。
"""

GENERATED_FROM = "base(5).apk :: classes.dex"
GENERATED_NOTE = "2026-10-04 从 innerModels 邻域字符串池提取"

MODELS = [{'id': 'hunyuan', 'name': '混元', 'desc': '通用对话'},
 {'id': 'hunyuan-expert', 'name': '混元 专家版', 'desc': '深度推理'},
 {'id': 'hunyuan-fast', 'name': '混元 快速版', 'desc': '低延迟'},
 {'id': 'hunyuan-thinking', 'name': '混元 深度思考', 'desc': '带思维链'},
 {'id': 'hunyuan_fast', 'name': '混元 快速版（下划线）', 'desc': '低延迟，与 hunyuan-fast 同源'},
 {'id': 'hunyuan_gpt_175B_0404', 'name': '混元 GPT 175B', 'desc': '2024-04 版 175B'},
 {'id': 'hunyuan_omnipotent', 'name': '混元 全能版', 'desc': '综合能力'},
 {'id': 'hunyuan_t1', 'name': '混元 T1', 'desc': 'T1 版本'},
 {'id': 'hy92', 'name': '混元 92', 'desc': '9 系列'},
 {'id': 'gpt_175B_0404', 'name': 'GPT 175B', 'desc': '2024-04 版'},
 {'id': 'deepseek', 'name': 'DeepSeek', 'desc': '深度求索'},
 {'id': 'deepseek-thinking', 'name': 'DeepSeek 深度思考', 'desc': '带思维链'}]

# 元宝真实使用的上游端点（dex 实测）
ENDPOINTS = ['https://yuanbao.tencent.com/api/chat/',
 'https://yuanbao.tencent.com/api/getuserinfo',
 'https://yuanbao.tencent.com/api/user/agent/conversation/create',
 'https://yuanbao.tencent.com/chat/naQivTmsDa']

# 鉴权会用到的凭据字段（dex innerStatus 邻域）
AUTH_FIELDS = ['hy_token', 'hy_user', 'hyUserId', 'uskey', 'uskeyMd5', 'deviceId', 'cookie']


def models_as_openai():
    """转成 OpenAI /v1/models 兼容结构"""
    return {"object": "list",
            "data": [{"id": m["id"], "object": "model",
                      "owned_by": "tencent-yuanbao",
                      "name": m["name"], "description": m["desc"]}
                     for m in MODELS]}


def find(mid):
    return next((m for m in MODELS if m["id"] == mid), None)
