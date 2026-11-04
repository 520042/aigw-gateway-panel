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
import json
import re


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


def _json_objects_containing(text, needle, limit=80):
    """找出 text 里所有**含 needle 的最小 JSON 对象**（按括号配对往回抠）。

    页面 SSR 的 JSON 是嵌在 <script> 里的，整段不一定合法（有时带未转义字符），
    所以不能一次性 json.loads 整页 —— 只能定位到 item 级对象逐个解析。
    """
    out = []
    pos = 0
    n = len(text)
    while len(out) < limit:
        i = text.find(needle, pos)
        if i < 0:
            break
        b = text.rfind("{", 0, i)
        if b < 0 or b < pos:
            pos = i + 1
            continue
        depth = 0
        end = -1
        for j in range(b, min(n, b + 4000)):
            c = text[j]
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    end = j
                    break
        if end < 0:
            pos = i + 1
            continue
        out.append(text[b:end + 1])
        pos = end + 1
    return out


def extract_doubao_page(html):
    """从豆包 /chat/ 页面 HTML 解析实时模型菜单（2026-10-12 新增）。

    为什么改这个：`/alice/basic/launch` 实测已 404（带不带 Cookie 都是裸
    "Not Found"），而网页版「模型选择」的配置本来就 SSR 嵌在 /chat/ 页面里
    （`model_list.item_list` + `modeSelectData.menu_item_list`，共 6 个
    model_item_key）—— 直接解析页面即可，且免登录也能拿到。
    """
    from app import bundled_doubao as BD
    if not html:
        return []
    found = {}
    for blob in _json_objects_containing(html, '"model_item_key"'):
        try:
            o = json.loads(blob)
        except Exception:
            continue
        if not isinstance(o, dict):
            continue
        key = o.get("model_item_key")
        if not key:
            continue
        key = str(key).strip()
        if not key:
            continue
        found.setdefault(key, {"name": o.get("name") or "",
                               "desc": o.get("desc") or ""})
    models = []
    for key, rec in found.items():
        models.append({"id": key, "name": BD.friendly(key) or rec["name"] or key,
                       "key": key, "desc": rec.get("desc") or ""})
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
      元宝 modelList:      {"modelList":[{"chatModelId":..,"modelName":..,
                            "modelDesc":..}]}   ← 2026-10-12 实测
      通用 models 键:      {"models":[...]} / {"list":[...]} / 直接列表
    """
    # 模型数组可能出现的键（大小写/驼峰都覆盖）
    ARRAY_KEYS = ("models", "model_list", "modelList", "list", "items")
    # ⚠ 只有 "data" 的值确实是 dict/list 时才下钻，否则 data=None 把真响应丢掉
    #   （元宝的响应就是顶层 {"modelList":[...]}，没有 data 键）。
    if isinstance(resp, dict) and isinstance(resp.get("data"), (dict, list)):
        data = resp["data"]
    else:
        data = resp
    # 逐层下钻：只要当前层没有模型数组键且带 "data" 键，就往下走
    for _ in range(3):
        if isinstance(data, dict) and not any(
                k in data for k in ARRAY_KEYS) \
                and isinstance(data.get("data"), (dict, list)):
            data = data["data"]
        else:
            break
    if isinstance(data, dict):
        for k in ARRAY_KEYS:
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
        # model_name / chatModelId 优先于 id：库库的 id 是菜单序号、
        # 元宝的 id 字段不是对话用的模型标识，这两个才是。
        # ★ WPS 灵犀（2026-10-12 实测）：模型项是 {key, name, desc, tag} ——
        #   id 字段叫 **key**，上面全都取不到 → 整张表被跳过 =「拉取不到模型」。
        #   放在链尾兜底，避免误伤正常带 id 的平台。
        # ★ 2026-10-13（用户实拍 web-lobster「获取不到模型」）：Lobster AI 的
        #   /api/models/available 返回 data[].{modelId,modelName,...}——
        #   驼峰 modelId 不在备选键里，整张表被跳过 = 0 个模型。
        #   链序：优先平台真实标识（model_name/chatModelId）——库库、元宝的
        #   id 字段不是对话用的标识；驼峰 modelId 紧跟其后；
        #   WPS 的 key 放链尾兜底避免误伤正常带 id 的平台。
        mid = (x.get("model_name") or x.get("chatModelId") or x.get("model_id")
               or x.get("modelId") or x.get("model")
               or x.get("id") or x.get("value") or x.get("key"))
        if not mid:
            continue
        mid = str(mid)
        if mid in seen:
            continue
        seen.add(mid)
        models.append({"id": mid,
                       "name": str(x.get("name") or x.get("modelName")
                                   or x.get("display_name")
                                   or x.get("label") or x.get("title") or mid),
                       "key": mid,
                       "desc": x.get("desc") or x.get("modelDesc")
                               or x.get("description") or "",
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
    ok, wrapped = call_fn(acc, pid, "models", {})
    if not ok or not wrapped:
        return None, False, "实时拉取失败（未登录或接口报错），回退内置"
    # gwextra.call 成功时包一层 {"platform","action","account","data"}，
    # 真实响应在 data 里（页面动作是 {"status":200,"raw":html}）。
    data = wrapped.get("data") if isinstance(wrapped, dict) else wrapped
    if not data:
        return None, False, "实时响应为空，回退内置"
    try:
        if pid == "apk-doubao":
            # page 动作（/chat/）返回 {"status":..,"raw":html}，走页面解析；
            # 旧 JSON 动作保留兼容。
            if spec.get("page") and isinstance(data, dict) and "raw" in data:
                models = extract_doubao_page(data.get("raw") or "")
            else:
                models = extract_doubao(data)
        else:
            models = extract_generic(data)
    except Exception as e:  # 解析异常也不该让界面空表
        return None, False, "实时响应解析失败：%s，回退内置" % e
    if not models:
        return None, False, "实时响应里没有模型项，回退内置"
    return models, True, "云端实时拉取（%d 个）" % len(models)
