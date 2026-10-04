# -*- coding: utf-8 -*-
"""
把 base(3).apk 里 assets/codebuddy-international-models.json
（来源 @tencent-ai/codebuddy-code@2.150.0）转成面板内置倍率表模块。

这是「模型 → 倍率」的权威离线来源：36 个模型，35 个带 credits。
"""
import io
import json
import os
import pprint

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "data", "bundled-intl-models.json")
DST = os.path.join(HERE, "app", "bundled_models.py")

KEEP = ("id", "name", "credits", "maxInputTokens", "maxOutputTokens",
        "supportsImages", "supportsToolCall", "supportsReasoning",
        "vendor", "tags", "descriptionZh")


def main():
    d = json.load(io.open(SRC, encoding="utf-8"))
    rows = []
    for m in d.get("models", []):
        row = {}
        for k in KEEP:
            v = m.get(k)
            if v in (None, "", [], {}):
                continue
            row[k] = v
        if "id" not in row:
            continue
        rows.append(row)

    body = pprint.pformat(rows, width=96, sort_dicts=False)
    with_rate = sum(1 for r in rows if r.get("credits"))
    # 用唯一标记占位，避免模板里的 % 和 {} 被误解析
    subs = {
        "__SRC__": str(d.get("source") or ""),
        "__GEN__": str(d.get("generated_at") or ""),
        "__N__": str(len(rows)),
        "__NR__": str(with_rate),
        "__CLI__": pprint.pformat(d.get("cli_recommended", []), width=92),
        "__MODELS__": body,
    }
    text = TEMPLATE
    for k, v in subs.items():
        text = text.replace(k, v)
    with io.open(DST, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print("已生成 %s（%d 个模型，%d 带倍率）" % (DST, len(rows), with_rate))


TEMPLATE = '''# -*- coding: utf-8 -*-
"""
内置模型倍率表（由 gen_bundled_models.py 自动生成，勿手改）
------------------------------------------------------------
数据来源：base(3).apk → assets/codebuddy-international-models.json
原始标注：__SRC__
生成时间：__GEN__

这是「模型 → 倍率」的权威离线来源：__N__ 个模型，__NR__ 个带 credits。
在线接口需要登录，未登录时本表是唯一能拿到倍率的途径。
"""


SOURCE = "__SRC__"
GENERATED_AT = "__GEN__"
CLI_RECOMMENDED = __CLI__

MODELS = __MODELS__

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
'''


if __name__ == "__main__":
    main()
