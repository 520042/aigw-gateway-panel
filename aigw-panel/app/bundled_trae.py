# -*- coding: utf-8 -*-
"""
Trae（aigw.app）内置模型清单 —— 由 gen_apk_models.py 从 base.apk 提取，勿手改

来源与可信度
------------
2026-10-05 拆解 base.apk（aigw.app 0.1.18）classes.dex 字符串池：
这个 APK 本身就是「Trae/WorkBuddy 额度 → 本地 OpenAI 端点」的网关，
暴露 /v1/models 与 /v1/chat/completions（前台服务监听，OAuth 回调 51120/51121）。
下面每个模型 id 都在 dex 里逐条验证存在（缺失会列出）。

上游供应商：api.trae.cn / www.trae.com.cn / copilot.tencent.com /
            www.codebuddy.cn / www.workbuddy.ai / cloudcode-pa.googleapis.com
"""

GENERATED_FROM = "base.apk (aigw.app 0.1.18) classes.dex 字符串池"
GENERATED_NOTE = "2026-10-05 从 APK dex 逐条验证提取"

# dex 里同时存在的自定义模型槽位（用户可在 APP 里绑定任意上游模型）
CUSTOM_SLOTS = ['custom_model_claude', 'custom_model_gemini', 'custom_model_gpt-5', 'custom_model_kimi', 'custom_model_deepseek_v4', 'custom_model_placeholder']

MODELS = [
    {'id': 'auto', 'name': 'Auto', 'desc': '自动路由'},
    {'id': 'claude-opus-4-6-thinking', 'name': 'Claude Opus 4.6 Thinking', 'desc': 'Anthropic 旗舰带思维链'},
    {'id': 'claude-sonnet-4-6', 'name': 'Claude Sonnet 4.6', 'desc': 'Anthropic 均衡款'},
    {'id': 'DeepSeek-v3.2', 'name': 'DeepSeek v3.2', 'desc': 'DeepSeek 上一代'},
    {'id': 'deepseek-v4-flash', 'name': 'DeepSeek V4 Flash', 'desc': '低延迟'},
    {'id': 'deepseek-v4-pro', 'name': 'DeepSeek V4 Pro', 'desc': '能力最强'},
    {'id': 'DeepSeek-V4-Flash-Official', 'name': 'DeepSeek V4 Flash (Official)', 'desc': '官方直连通道'},
    {'id': 'doubao-seed-2.0-pro', 'name': 'Doubao Seed 2.0 Pro', 'desc': '字节豆包'},
    {'id': 'Doubao-Seed-2.0-Code', 'name': 'Doubao Seed 2.0 Code', 'desc': '字节豆包代码版'},
    {'id': 'Doubao-Seed-2.1-Pro', 'name': 'Doubao Seed 2.1 Pro', 'desc': '字节豆包新一代'},
    {'id': 'Doubao-Seed-2.1-Turbo', 'name': 'Doubao Seed 2.1 Turbo', 'desc': '字节豆包低延迟'},
    {'id': 'gemini-3-pro-high', 'name': 'Gemini 3 Pro High', 'desc': 'Google 高算力档'},
    {'id': 'gemini-3-pro-low', 'name': 'Gemini 3 Pro Low', 'desc': 'Google 低延迟档'},
    {'id': 'glm-5', 'name': 'GLM-5', 'desc': '智谱'},
    {'id': 'glm-5-turbo', 'name': 'GLM-5 Turbo', 'desc': '智谱低延迟'},
    {'id': 'glm-5.2', 'name': 'GLM-5.2', 'desc': '智谱'},
    {'id': 'glm-5.3', 'name': 'GLM-5.3', 'desc': '智谱新一代'},
    {'id': 'gpt-oss-120b-medium', 'name': 'GPT-OSS 120B Medium', 'desc': '开源 120B'},
    {'id': 'kimi-k2.5', 'name': 'Kimi K2.5', 'desc': '月之暗面'},
    {'id': 'kimi-k2.6', 'name': 'Kimi K2.6', 'desc': '月之暗面'},
    {'id': 'kimi-k2.7-code', 'name': 'Kimi K2.7 Code', 'desc': '月之暗面代码版'},
    {'id': 'kimi-k3', 'name': 'Kimi K3', 'desc': '月之暗面新一代'},
    {'id': 'minimax-m2.5', 'name': 'MiniMax M2.5', 'desc': 'MiniMax'},
    {'id': 'minimax-m3', 'name': 'MiniMax M3', 'desc': 'MiniMax 新一代'},
]

NAMES = ['Auto', 'Claude Opus 4.6 Thinking', 'Claude Sonnet 4.6', 'DeepSeek v3.2', 'DeepSeek V4 Flash', 'DeepSeek V4 Pro', 'DeepSeek V4 Flash (Official)', 'Doubao Seed 2.0 Pro', 'Doubao Seed 2.0 Code', 'Doubao Seed 2.1 Pro', 'Doubao Seed 2.1 Turbo', 'Gemini 3 Pro High', 'Gemini 3 Pro Low', 'GLM-5', 'GLM-5 Turbo', 'GLM-5.2', 'GLM-5.3', 'GPT-OSS 120B Medium', 'Kimi K2.5', 'Kimi K2.6', 'Kimi K2.7 Code', 'Kimi K3', 'MiniMax M2.5', 'MiniMax M3']
