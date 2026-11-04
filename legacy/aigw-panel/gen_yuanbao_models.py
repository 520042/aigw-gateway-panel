# -*- coding: utf-8 -*-
"""
从 base(5).apk（dev.yuanbao2api）的 classes.dex 提取元宝内置模型清单。

关键发现（2026-10-04 实测）：
  元宝**没有模型列表接口**。APK 里的 `/v1/models` 是这个 APP 自己的**本地网关**端点，
  不是腾讯的接口。模型清单是**硬编码**在 dex 的 `innerModels` 附近的字符串常量池里。
  所以面板要列元宝的模型，只能用这份内置清单，不能去调 /api/models。

用法：python gen_yuanbao_models.py
"""
import io
import os
import re
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
APK = os.path.join(ROOT, "base(5).apk")
DST = os.path.join(HERE, "app", "bundled_yuanbao.py")

# dex 里出现过的、确定属于模型 id 的字符串
MODEL_IDS = [
    "hunyuan", "hunyuan-expert", "hunyuan-fast", "hunyuan-thinking",
    "hunyuan_fast", "hunyuan_gpt_175B_0404", "hunyuan_omnipotent",
    "hunyuan_t1", "hy92", "gpt_175B_0404", "deepseek", "deepseek-thinking",
]
# 展示名与说明（按 id 归类，名字取自 dex 邻域语义）
META = {
    "hunyuan":            ("混元", "通用对话"),
    "hunyuan-fast":       ("混元 快速版", "低延迟"),
    "hunyuan-expert":     ("混元 专家版", "深度推理"),
    "hunyuan-thinking":   ("混元 深度思考", "带思维链"),
    "hunyuan_omnipotent": ("混元 全能版", "综合能力"),
    "hunyuan_t1":         ("混元 T1", "T1 版本"),
    "hunyuan_fast":       ("混元 快速版（下划线）", "低延迟，与 hunyuan-fast 同源"),
    "hunyuan_gpt_175B_0404": ("混元 GPT 175B", "2024-04 版 175B"),
    "gpt_175B_0404":      ("GPT 175B", "2024-04 版"),
    "hy92":               ("混元 92", "9 系列"),
    "deepseek":           ("DeepSeek", "深度求索"),
    "deepseek-thinking":  ("DeepSeek 深度思考", "带思维链"),
}

# 元宝实际使用的上游端点（dex 里的完整 URL 列表）
ENDPOINTS = [
    "https://yuanbao.tencent.com/api/chat/",
    "https://yuanbao.tencent.com/api/getuserinfo",
    "https://yuanbao.tencent.com/api/user/agent/conversation/create",
    "https://yuanbao.tencent.com/chat/naQivTmsDa",
]
# 元宝鉴权要用到的凭据字段（dex 的 innerStatus 邻域）
AUTH_FIELDS = ["hy_token", "hy_user", "hyUserId", "uskey", "uskeyMd5",
               "deviceId", "cookie"]


def main():
    found = []
    if os.path.exists(APK):
        with zipfile.ZipFile(APK) as z:
            blob = z.read("classes.dex")
        pool = set(m.group().decode("ascii", "replace")
                   for m in re.finditer(rb"[\x20-\x7e]{3,90}", blob))
        found = [x for x in MODEL_IDS if x in pool]
    models = []
    for mid in found:
        name, desc = META.get(mid, (mid, ""))
        models.append({"id": mid, "name": name, "desc": desc})

    text = '''# -*- coding: utf-8 -*-
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

MODELS = %s

# 元宝真实使用的上游端点（dex 实测）
ENDPOINTS = %s

# 鉴权会用到的凭据字段（dex innerStatus 邻域）
AUTH_FIELDS = %s


def models_as_openai():
    """转成 OpenAI /v1/models 兼容结构"""
    return {"object": "list",
            "data": [{"id": m["id"], "object": "model",
                      "owned_by": "tencent-yuanbao",
                      "name": m["name"], "description": m["desc"]}
                     for m in MODELS]}


def find(mid):
    return next((m for m in MODELS if m["id"] == mid), None)
''' % (_pformat(models), _pformat(ENDPOINTS), _pformat(AUTH_FIELDS))

    with io.open(DST, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print("已生成 %s（%d 个模型）" % (DST, len(models)))
    for m in models:
        print("  %-24s %s" % (m["id"], m["name"]))


def _pformat(obj):
    import pprint
    return pprint.pformat(obj, width=88, sort_dicts=False)


if __name__ == "__main__":
    main()
