# -*- coding: utf-8 -*-
"""
内置模型倍率表（由 gen_bundled_models.py 自动生成，勿手改）
------------------------------------------------------------
数据来源：base(3).apk → assets/codebuddy-international-models.json
原始标注：@tencent-ai/codebuddy-code@2.150.0
生成时间：2026-09-13T11:56:58.647046Z

这是「模型 → 倍率」的权威离线来源：36 个模型，35 个带 credits。
在线接口需要登录，未登录时本表是唯一能拿到倍率的途径。
"""


SOURCE = "@tencent-ai/codebuddy-code@2.150.0"
GENERATED_AT = "2026-09-13T11:56:58.647046Z"
CLI_RECOMMENDED = ['default-model',
 'fast-model',
 'balanced-model',
 'primary-model',
 'deep-model',
 'gpt-5.6-sol',
 'gpt-5.6-terra',
 'gpt-5.6-luna',
 'gpt-5.5',
 'gpt-5.4',
 'gpt-5.3-codex',
 'gemini-3.5-flash',
 'glm-5.3',
 'glm-5.2',
 'kimi-k3',
 'kimi-k2.6',
 'minimax-m3']

MODELS = [{'id': 'default-model',
  'name': 'Auto',
  'credits': 'x0.79 credits',
  'maxInputTokens': 176000,
  'maxOutputTokens': 24000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'default-model-lite',
  'name': 'Default-Lite',
  'credits': 'x0.67 credits',
  'maxInputTokens': 176000,
  'maxOutputTokens': 24000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'tags': ['lite']},
 {'id': 'fast-model',
  'name': 'Fast',
  'credits': 'x0.34 credits',
  'maxInputTokens': 200000,
  'maxOutputTokens': 32000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'i',
  'descriptionZh': '响应快，适合简单任务'},
 {'id': 'balanced-model',
  'name': 'Balanced',
  'credits': 'x0.59 credits',
  'maxInputTokens': 256000,
  'maxOutputTokens': 32000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'f',
  'tags': ['craft'],
  'descriptionZh': '速度与质量兼顾，日常工作首选'},
 {'id': 'primary-model',
  'name': 'Primary',
  'credits': 'x3.31 credits',
  'maxInputTokens': 272000,
  'maxOutputTokens': 72000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'descriptionZh': '高质量输出，胜任复杂任务'},
 {'id': 'deep-model',
  'name': 'Deep',
  'credits': 'x3.33 credits',
  'maxInputTokens': 176000,
  'maxOutputTokens': 24000,
  'supportsImages': True,
  'supportsToolCall': True,
  'vendor': 'e',
  'descriptionZh': '深度推理，适合深度分析与难题'},
 {'id': 'gpt-5.5',
  'name': 'GPT-5.5',
  'credits': 'x3.31 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 72000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'descriptionZh': 'OpenAI 旗舰编码模型,擅长长程任务'},
 {'id': 'gpt-5.4',
  'name': 'GPT-5.4',
  'credits': 'x1.65 credits',
  'maxInputTokens': 272000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'gpt-5.3-codex',
  'name': 'GPT-5.3-Codex',
  'credits': 'x1.25 credits',
  'maxInputTokens': 272000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'gpt-5.1-codex',
  'name': 'GPT-5.1-Codex',
  'credits': 'x0.90 credits',
  'maxInputTokens': 272000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'gpt-5.1-codex-mini',
  'name': 'GPT-5.1-Codex-Mini',
  'credits': 'x0.18 credits',
  'maxInputTokens': 272000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'gemini-3.1-pro',
  'name': 'Gemini-3.1-Pro',
  'credits': 'x1.32 credits',
  'maxInputTokens': 400000,
  'maxOutputTokens': 64000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True},
 {'id': 'gemini-3.0-flash',
  'name': 'Gemini-3.0-Flash',
  'credits': 'x0.33 credits',
  'maxInputTokens': 400000,
  'maxOutputTokens': 64000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True},
 {'id': 'gemini-3.5-flash',
  'name': 'Gemini-3.5-Flash',
  'credits': 'x0.99 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 65536,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'descriptionZh': '能力均衡，适合日常使用'},
 {'id': 'gemini-2.5-flash',
  'name': 'Gemini-2.5-Flash',
  'credits': 'x0.22 credits',
  'maxInputTokens': 400000,
  'maxOutputTokens': 64000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True},
 {'id': 'gemini-3.1-flash-lite',
  'name': 'Gemini-3.1-flash-lite',
  'credits': 'x0.17 credits',
  'maxInputTokens': 200000,
  'maxOutputTokens': 65536,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'gemini-2.5-pro',
  'name': 'Gemini-2.5-Pro',
  'credits': 'x0.90 credits',
  'maxInputTokens': 400000,
  'maxOutputTokens': 64000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True},
 {'id': 'deepseek-v3-2-volc',
  'name': 'DeepSeek-V3.2',
  'credits': 'x0.29 credits',
  'maxInputTokens': 96000,
  'maxOutputTokens': 32000,
  'supportsImages': False,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'deepseek-v4.1-flash',
  'name': 'DeepSeek-V4.1-Flash',
  'maxInputTokens': 96000,
  'maxOutputTokens': 32000,
  'supportsImages': False,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'descriptionZh': 'DeepSeek V4.1 Flash - 快速且免费'},
 {'id': 'glm-5.0',
  'name': 'GLM-5.0',
  'credits': 'x0.80 credits',
  'maxInputTokens': 200000,
  'maxOutputTokens': 48000,
  'supportsImages': False,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e'},
 {'id': 'kimi-k2.5',
  'name': 'Kimi-K2.5',
  'credits': 'x0.45 credits',
  'maxInputTokens': 164000,
  'maxOutputTokens': 32000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'f'},
 {'id': 'gemini-3.0-pro-image',
  'name': 'Gemini-3.0-Pro-Image',
  'credits': 'x4.96 credits',
  'tags': ['text-to-image', 'image-to-image']},
 {'id': 'gemini-3.1-flash-image',
  'name': 'Gemini-3.1-Flash-Image',
  'credits': 'x1.78 credits',
  'tags': ['text-to-image', 'image-to-image']},
 {'id': 'gemini-2.5-flash-image',
  'name': 'Gemini-2.5-Flash-Image',
  'credits': 'x1.14 credits',
  'tags': ['text-to-image', 'image-to-image']},
 {'id': 'hunyuan-image-v3.0',
  'name': 'Hunyuan-Image-V3',
  'credits': 'x5.00 credits',
  'tags': ['text-to-image']},
 {'id': 'hunyuan-image-v2.0-general-edit',
  'name': 'Hunyuan-Image-Edit',
  'credits': 'x5.00 credits',
  'tags': ['image-to-image']},
 {'id': 'hunyuan-video-art',
  'name': 'Hunyuan-Video-Art',
  'credits': 'x10.00 credits',
  'tags': ['text-to-video', 'image-to-video']},
 {'id': 'gpt-5.6-sol',
  'name': 'GPT-5.6-Sol',
  'credits': 'x3.47 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'tags': ['badge:企业版:#3B82F6'],
  'descriptionZh': 'OpenAI 旗舰模型，擅长复杂推理与长程任务'},
 {'id': 'gpt-5.6-terra',
  'name': 'GPT-5.6-Terra',
  'credits': 'x1.39 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'tags': ['badge:企业版:#3B82F6'],
  'descriptionZh': 'OpenAI 均衡模型，兼顾能力、速度与成本'},
 {'id': 'gpt-5.6-luna',
  'name': 'GPT-5.6-Luna',
  'credits': 'x0.14 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'tags': ['badge:企业版:#3B82F6'],
  'descriptionZh': 'OpenAI 轻量模型，响应快速，适合日常任务'},
 {'id': 'glm-5.3',
  'name': 'GLM-5.3',
  'credits': 'x0.79 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 48000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'tags': ['craft'],
  'descriptionZh': '能力均衡，适合日常使用'},
 {'id': 'glm-5.2',
  'name': 'GLM-5.2',
  'credits': 'x0.79 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 48000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'e',
  'tags': ['craft'],
  'descriptionZh': '1M 上下文，擅长长程任务'},
 {'id': 'hy3',
  'name': 'Hy3',
  'credits': 'x0.00',
  'maxInputTokens': 192000,
  'maxOutputTokens': 64000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'j',
  'tags': ['craft'],
  'descriptionZh': '混元思考模型，具有增强的推理能力'},
 {'id': 'kimi-k3',
  'name': 'Kimi-K3',
  'credits': 'x1.62 credits',
  'maxInputTokens': 1000000,
  'maxOutputTokens': 32000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'f',
  'descriptionZh': '擅长处理复杂的长程自主任务，前端开发能力突出，同时在知识工作与科研推理上表现出色。'},
 {'id': 'kimi-k2.6',
  'name': 'Kimi-K2.6',
  'credits': 'x0.52 credits',
  'maxInputTokens': 256000,
  'maxOutputTokens': 32000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'f',
  'tags': ['craft'],
  'descriptionZh': '多模态模型，适合日常任务'},
 {'id': 'minimax-m3',
  'name': 'MiniMax-M3',
  'credits': 'x0.25 credits',
  'maxInputTokens': 512000,
  'maxOutputTokens': 128000,
  'supportsImages': True,
  'supportsToolCall': True,
  'supportsReasoning': True,
  'vendor': 'f',
  'descriptionZh': '原生多模态，擅长代码、智能体任务'}]

# id → 行，精确索引
_BY_ID = {m["id"]: m for m in MODELS}

# 归一化别名：把「网关上游真实名」映射到本表的槽位名
# 例：网关 default → 本表 default-model；网关 minimax-m3-pay → minimax-m3
_ALIASES = {
    "default": "default-model",
    "auto": "default-model",
    "deepseek-v4-pro": "primary-model",
    "deepseek-v4-flash": "deepseek-v4.1-flash",
    "minimax-m3-pay": "minimax-m3",
    "hy3-preview": "hy3",
    "hy3-preview-agent": "hy3",
    "hy4-preview": "hy3",
}


def _norm(s):
    """归一化：小写、下划线/空格/点统一成连字符"""
    s = (s or "").strip().lower()
    for ch in ("_", " ", "."):
        s = s.replace(ch, "-")
    while "--" in s:
        s = s.replace("--", "-")
    return s


def rate_of(model_id):
    """
    查倍率。返回 (credits 字符串, 匹配方式)；查不到返回 (None, None)。
    匹配方式：exact 精确 | alias 别名 | norm 归一化 | prefix 同族前缀
    """
    if not model_id:
        return None, None
    mid = str(model_id)
    if mid in _BY_ID:
        return _BY_ID[mid].get("credits"), "exact"
    tgt = _ALIASES.get(mid)
    if tgt and tgt in _BY_ID:
        return _BY_ID[tgt].get("credits"), "alias"
    n = _norm(mid)
    for m in MODELS:
        if _norm(m["id"]) == n:
            return m.get("credits"), "norm"
    head = n.rsplit("-", 1)[0]
    if head:
        cands = [m for m in MODELS if _norm(m["id"]).startswith(head + "-")]
        if len(cands) == 1:
            return cands[0].get("credits"), "prefix"
    return None, None


def info_of(model_id):
    """查完整行（含上下文/输出/能力），查不到返回 None"""
    if not model_id:
        return None
    mid = str(model_id)
    hit = _BY_ID.get(mid)
    if hit:
        return dict(hit, _match="exact")
    tgt = _ALIASES.get(mid)
    if tgt and tgt in _BY_ID:
        return dict(_BY_ID[tgt], _match="alias")
    n = _norm(mid)
    for m in MODELS:
        if _norm(m["id"]) == n:
            return dict(m, _match="norm")
    head = n.rsplit("-", 1)[0]
    if head:
        cands = [m for m in MODELS if _norm(m["id"]).startswith(head + "-")]
        if len(cands) == 1:
            return dict(cands[0], _match="prefix")
    return None


def merge(models):
    """
    给一批模型行补倍率。入参是网关 /admin/api/models 的行（dict，含 id）。
    返回 (新列表, 命中数)。不改原对象。
    """
    out, hit = [], 0
    for row in models or []:
        item = dict(row) if isinstance(row, dict) else {"id": row}
        info = info_of(item.get("id"))
        if info:
            item["credits"] = info.get("credits", "")
            item["ctx"] = info.get("maxInputTokens", "")
            item["out"] = info.get("maxOutputTokens", "")
            item["tools"] = info.get("supportsToolCall")
            item["vision"] = info.get("supportsImages")
            item["reasoning"] = info.get("supportsReasoning")
            item["rate_source"] = "内置表/" + str(info.get("_match"))
            hit += 1
        else:
            item.setdefault("credits", "")
            item.setdefault("rate_source", "未匹配")
        out.append(item)
    return out, hit


def stats():
    with_rate = sum(1 for m in MODELS if m.get("credits"))
    return {"total": len(MODELS), "with_rate": with_rate,
            "source": SOURCE, "generated_at": GENERATED_AT}
