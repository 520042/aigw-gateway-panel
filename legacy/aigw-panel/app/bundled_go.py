# -*- coding: utf-8 -*-
"""
Go 原生网关（wb2apimobile）内置模型清单 —— gen_apk_models.py 生成，勿手改

来源与可信度
------------
2026-10-05 拆解 base(2).apk 的 lib/arm64-v8a/libgojni.so（13.2MB GoMobile）:
Go 代码里内嵌了一份模型能力表（JSON：context_length / max_output_tokens /
source="seed"），并自带 Web 管理面板（/panel/*，UI 占位 :7863）。
每个 id 都在 .so 里逐条验证存在。
"""

GENERATED_FROM = "base(2).apk lib/arm64-v8a/libgojni.so 内嵌模型能力表"
GENERATED_NOTE = "2026-10-05 从 libgojni.so JSON 逐条验证提取"

MODELS = [
    {'id': 'deepseek-v4-flash', 'name': 'deepseek-v4-flash', 'context_length': 1000000, 'max_output_tokens': 384000, 'desc': '上下文 1000K'},
    {'id': 'deepseek-v4-pro', 'name': 'deepseek-v4-pro', 'context_length': 1000000, 'max_output_tokens': 384000, 'desc': '上下文 1000K'},
    {'id': 'deepseek-v4.1-flash', 'name': 'deepseek-v4.1-flash', 'context_length': 1000000, 'max_output_tokens': 384000, 'desc': '上下文 1000K'},
    {'id': 'gemini-3.5-flash', 'name': 'gemini-3.5-flash', 'context_length': 1048576, 'max_output_tokens': 65536, 'desc': '上下文 1048K'},
    {'id': 'glm-5.1', 'name': 'glm-5.1', 'context_length': 200000, 'max_output_tokens': 131072, 'desc': '上下文 200K'},
    {'id': 'glm-5.2', 'name': 'glm-5.2', 'context_length': 1000000, 'max_output_tokens': 131072, 'desc': '上下文 1000K'},
    {'id': 'glm-5.3', 'name': 'glm-5.3', 'context_length': 1000000, 'max_output_tokens': 131072, 'desc': '上下文 1000K'},
    {'id': 'glm-5.3-flash', 'name': 'glm-5.3-flash', 'context_length': 1000000, 'max_output_tokens': 131072, 'desc': '上下文 1000K'},
    {'id': 'glm-5v-turbo', 'name': 'glm-5v-turbo', 'context_length': 1000000, 'max_output_tokens': 131072, 'desc': '上下文 1000K'},
    {'id': 'gpt-5.3-codex', 'name': 'gpt-5.3-codex', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'gpt-5.4', 'name': 'gpt-5.4', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'gpt-5.5', 'name': 'gpt-5.5', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'gpt-5.6-luna', 'name': 'gpt-5.6-luna', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'gpt-5.6-sol', 'name': 'gpt-5.6-sol', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'gpt-5.6-terra', 'name': 'gpt-5.6-terra', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'gpt-6-astra', 'name': 'gpt-6-astra', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'kimi-k2.5', 'name': 'kimi-k2.5', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'kimi-k2.6', 'name': 'kimi-k2.6', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'kimi-k2.7', 'name': 'kimi-k2.7', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'kimi-k2.8-preview', 'name': 'kimi-k2.8-preview', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'kimi-k3', 'name': 'kimi-k3', 'context_length': 1050000, 'max_output_tokens': 128000, 'desc': '上下文 1050K'},
    {'id': 'auto', 'name': 'auto', 'context_length': 168000, 'max_output_tokens': None, 'desc': '上下文 168K'},
]
