# -*- coding: utf-8 -*-
"""
模型清单实时提取 + 内置回退

背景
----
用户对「选了平台但模型表里没东西 / 跟网页版对不上」反复吐槽：
- 豆包网页版「模型选择」菜单真实存在，定义内嵌在 /alice/basic/launch 返回的
  `model_list.item_list` 里（无需登录的 SSR 快照已解析进 bundled_doubao）。
- 元宝网页版模型是登录后由 /api/agent/model/list 动态加载的，SSR 壳里没有。

策略（每个平台都一样）
----------------------
已登录（账号可用）时优先拉云端实时清单；任何一步失败 → 回退到内置清单。
返回 (models, live)，live=True 表示云端实时拉到，False 表示走了内置回退。
UI 据此标注「实时 / 内置」，绝不因为拉取失败就显示空表。
"""

import sys


def _walk(obj, out, want):
    """递归收集所有含 want 键的 dict（去重按 id 字段）"""
    if isinstance(obj, dict):
        if want in obj and isinstance(obj.get(want), str) and obj[want].strip():
            key = obj[want].strip()
            rec = {"key": key, "name": obj.get("name") or key,
                   "desc": obj.get("desc") or obj.get("description") or ""}
            out.setdefault(key, rec)
        for v in obj.values():
            _walk(v, out, want)
    elif isinstance(obj, list):
        for v in obj:
            _walk(v, out, want)


def extract_doubao(resp):
    """解析 /alice/basic/launch 返回的模型菜单。

    真实结构里模型项分散在 model_list.item_list（多个分组），
    每项带 model_item_key + name。把 model_item_key 映射成 bundled_doubao 的友好名。
    找不到内置映射时退用接口给的短名。
    """
    from app import bundled_doubao as BD
    found = {}
    _walk(resp, found, "model_item_key")
    models = []
    for key, rec in found.items():
        name = BD.friendly(key)
        models.append({"id": key, "name": name, "key": key,
                       "desc": rec.get("desc") or ""})
    # 保序：按内置清单顺序，多出来的实时项排在后面
    order = {m["key"]: i for i, m in enumerate(BD.MODELS)}
    models.sort(key=lambda m: order.get(m["key"], 999))
    return models


def extract_generic(resp):
    """通用解析：兼容多层包装与多种键名。

    能处理的形状（实际样本）：
      gwextra.call 包装:   {"platform":..,"action":..,"data": <真实响应>}
      OpenAI 官方 /models: {"object":"list","data":[{"id":..}]}
      库库 model_list:     {"status":..,"data":{"model_list":[{"model_name":..,
                            "display_name":..,"cost_ratio":..}]}}
      通用 models 键:      {"models":[...]} / {"list":[...]} / 直接列表
    """
    data = resp.get("data") if isinstance(resp, dict) else resp
    # 逐层下钻：只要当前层没有模型数组键且带 "data" 键，就往下走
    for _ in range(3):
        if isinstance(data, dict) and not any(
                k in data for k in ("models", "model_list", "list", "items")) \
                and isinstance(data.get("data"), (dict, list)):
            data = data["data"]
        else:
            break
    if isinstance(data, dict):
        for k in ("models", "model_list", "list", "items"):
            if isinstance(data.get(k), list):
                data = data[k]
                break
    if not isinstance(data, list):
        return []
    models = []
    seen = set()
    for x in data:
        if not isinstance(x, dict):
            continue
        # model_name 优先于 id：库库等平台的 id 是菜单序号，model_name 才是
        # 对话请求里真正用的模型标识
        mid = (x.get("model_name") or x.get("model_id") or x.get("model")
               or x.get("id") or x.get("value"))
        if not mid:
            continue
        mid = str(mid)
        if mid in seen:
            continue
        seen.add(mid)
        models.append({"id": mid,
                       "name": str(x.get("name") or x.get("display_name")
                                   or x.get("label") or x.get("title") or mid),
                       "key": mid,
                       "desc": x.get("desc") or x.get("description") or "",
                       "credits": x.get("credits") or x.get("cost_ratio") or ""})
    return models


def live_models(acc, pid, call_fn, action_spec_fn):
    """统一入口：尝试实时拉，失败回退内置。

    acc            : 账号池（app.accounts.AccountStore）
    pid            : 平台 id
    call_fn        : gwextra.call(acc, pid, action, body) -> (ok, data)
    action_spec_fn : gwextra.action_spec(pid, "models") -> dict|None
    返回 (models, live, note)
    """
    spec = action_spec_fn(pid, "models") if action_spec_fn else None
    if not spec:
        return None, False, "平台没有 models 动作"
    ok, data = call_fn(acc, pid, "models", {})
    if not ok or not data:
        return None, False, "实时拉取失败（未登录或接口报错），回退内置"
    try:
        if pid == "apk-doubao":
            models = extract_doubao(data)
        else:
            models = extract_generic(data)
    except Exception as e:  # 解析异常也不该让界面空表
        return None, False, "实时响应解析失败：%s，回退内置" % e
    if not models:
        return None, False, "实时响应里没有模型项，回退内置"
    return models, True, "云端实时拉取（%d 个）" % len(models)
