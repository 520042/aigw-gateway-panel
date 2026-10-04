# -*- coding: utf-8 -*-
"""
工具调用（function calling）本地执行器
=====================================
面板自己实现 OpenAI 的 tools 协议往返：

    1) 客户端/测试台把 tools 定义发过来
    2) 面板转发给上游（网关 /v1 或任一 OpenAI 兼容端点）
    3) 上游返回 message.tool_calls
    4) 面板用这里的 TOOLS 按名字执行函数，拿到结果
    5) 客户端把结果作为 role=tool 消息回传，模型继续生成

这样不需要用户自己写执行器，模型就能真正"动手"：
查时间、算数、读文件、发 HTTP、单位换算、取随机数。

设计约束：
  - 纯标准库，不引任何依赖（PyInstaller 单 EXE 目标）
  - HTTP 工具默认关闭（防止面板变成跳板），要显式开
  - 文件工具限制在 data/tools_sandbox 之内，防目录穿越
  - 每个工具有 timeout 和大小上限
"""

import json
import os
import re
import time
import urllib.error
import urllib.parse
import urllib.request

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")

HERE = os.path.dirname(os.path.abspath(__file__))
SANDBOX = os.path.join(os.path.dirname(HERE), "data", "tools_sandbox")

MAX_TEXT = 20000


# ------------------------------------------------------------------ 沙箱
def _safe_path(rel):
    """把相对路径限制在 data/tools_sandbox 内，防目录穿越"""
    base = os.path.abspath(SANDBOX)
    p = os.path.abspath(os.path.join(base, rel or ""))
    if not p.startswith(base + os.sep) and p != base:
        raise ValueError("路径越界：只允许访问 %s 下的文件" % base)
    return p


# ------------------------------------------------------------------ 各工具实现
def t_now(args, opt):
    """当前时间"""
    tz = (args or {}).get("timezone", "local")
    fmt = (args or {}).get("format", "%Y-%m-%d %H:%M:%S")
    if tz in ("utc", "UTC"):
        return time.strftime(fmt, time.gmtime())
    return time.strftime(fmt, time.localtime())


def t_calc(args, opt):
    """四则运算计算器（只允许白名单字符）"""
    expr = str((args or {}).get("expression", "")).strip()
    if not expr:
        raise ValueError("缺少 expression 参数")
    if not re.fullmatch(r"[0-9+\-*/().%\s]+", expr):
        raise ValueError("表达式含非法字符，只允许数字和 + - * / ( ) % .")
    if len(expr) > 200:
        raise ValueError("表达式过长")
    # 不用 eval：转成受限 AST 再求值
    import ast
    tree = ast.parse(expr, mode="eval")
    allowed = (ast.Expression, ast.BinOp, ast.UnaryOp, ast.Constant,
               ast.Add, ast.Sub, ast.Mult, ast.Div, ast.Mod, ast.Pow,
               ast.USub, ast.UAdd)
    for node in ast.walk(tree):
        if not isinstance(node, allowed):
            raise ValueError("表达式含不允许的语法")
        if isinstance(node, ast.Constant) and not isinstance(
                node.value, (int, float)):
            raise ValueError("只支持数字常量")
        if isinstance(node, ast.Pow):
            raise ValueError("不允许幂运算（防资源耗尽）")
    v = eval(compile(tree, "<calc>", "eval"), {"__builtins__": {}}, {})
    return repr(v) if isinstance(v, float) else str(v)


def t_random(args, opt):
    """随机数"""
    a = int((args or {}).get("min", 0))
    b = int((args or {}).get("max", 100))
    if b < a:
        a, b = b, a
    return str(random_int(a, b))


def random_int(a, b):
    import random
    return random.randint(a, b)


def t_unit_convert(args, opt):
    """常见单位换算（长度/重量/温度/存储）"""
    v = (args or {}).get("value")
    frm = str((args or {}).get("from", "")).lower()
    to = str((args or {}).get("to", "")).lower()
    if v is None:
        raise ValueError("缺少 value")
    v = float(v)
    L = {"mm": .001, "cm": .01, "m": 1, "km": 1000, "in": .0254, "ft": .3048}
    W = {"mg": 1e-6, "g": .001, "kg": 1, "t": 1000, "lb": .45359237, "oz": .0283495}
    C = {"mm": 1e-6, "mb": 1e-3, "kb": 1e-3, "gb": 1, "tb": 1024, "b": 1}
    for tbl in (L, W, C):
        if frm in tbl and to in tbl:
            return "%.6g" % (v * tbl[frm] / tbl[to])
    if {frm, to} <= {"c", "f", "k"}:
        c = v if frm == "c" else ((v - 32) * 5 / 9 if frm == "f" else v - 273.15)
        r = c if to == "c" else (c * 9 / 5 + 32 if to == "f" else c + 273.15)
        return "%.4g" % r
    raise ValueError("不支持的换算：%s → %s" % (frm, to))


def t_weather(args, opt):
    """天气查询（离线 mock，不发外网）"""
    city = str((args or {}).get("city", "")) or "未知"
    seed = sum(ord(c) for c in city)
    return json.dumps({
        "city": city, "source": "本地 mock（不发外网）",
        "temp_c": 18 + seed % 12, "humidity": 40 + seed % 45,
        "condition": ["晴", "多云", "阴", "小雨"][seed % 4],
        "wind": "%d 级" % (2 + seed % 4),
    }, ensure_ascii=False)


def t_read_file(args, opt):
    """读沙箱内文件"""
    rel = str((args or {}).get("path", ""))
    p = _safe_path(rel)
    if not os.path.exists(p):
        raise ValueError("文件不存在：%s" % rel)
    if os.path.getsize(p) > MAX_TEXT:
        raise ValueError("文件过大（>%d 字节）" % MAX_TEXT)
    with open(p, "r", encoding="utf-8", errors="replace") as f:
        return f.read()[:MAX_TEXT]


def t_write_file(args, opt):
    """写沙箱内文件（需开 allow_write）"""
    if not opt.get("allow_write"):
        return "（未开启写入权限）"
    rel = str((args or {}).get("path", ""))
    content = str((args or {}).get("content", ""))[:MAX_TEXT]
    p = _safe_path(rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w", encoding="utf-8", newline="\n") as f:
        f.write(content)
    return "已写入 %d 字节 → %s" % (len(content.encode("utf-8")), rel)


def t_http_get(args, opt):
    """HTTP GET（默认关闭，需显式开 allow_http）"""
    if not opt.get("allow_http"):
        return "（未开启 HTTP 工具，出于安全考虑默认禁用）"
    url = str((args or {}).get("url", ""))
    if not url.startswith(("http://", "https://")):
        raise ValueError("只允许 http/https")
    if opt.get("http_block_private") and _is_private(url):
        return "（内网地址已被策略拦截）"
    t = float((args or {}).get("timeout", 12))
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(req, timeout=min(t, 20)) as r:
            return r.read().decode("utf-8", "replace")[:MAX_TEXT]
    except Exception as e:
        return "请求失败：%s" % str(e)[:120]


def _is_private(url):
    h = urllib.parse.urlparse(url).hostname or ""
    if h in ("localhost", "127.0.0.1", "::1"):
        return True
    if re.fullmatch(r"10\.\d+\.\d+\.\d+", h):
        return True
    if re.fullmatch(r"192\.168\.\d+\.\d+", h):
        return True
    m = re.fullmatch(r"172\.(\d+)\.\d+\.\d+", h)
    if m and 16 <= int(m.group(1)) <= 31:
        return True
    return False


def t_json_query(args, opt):
    """对给定 JSON 做简易提取（点号路径 / 数组下标）"""
    doc = args or {}
    data = doc.get("data")
    path = str(doc.get("path", "")).strip()
    if isinstance(data, str):
        try:
            data = json.loads(data)
        except Exception:
            return "data 不是合法 JSON"
    cur = data
    if not path:
        return json.dumps(cur, ensure_ascii=False)[:MAX_TEXT]
    for seg in path.split("."):
        if cur is None:
            return ""
        if isinstance(cur, list):
            if not re.fullmatch(r"\d+", seg):
                return "数组索引用数字"
            i = int(seg)
            cur = cur[i] if 0 <= i < len(cur) else None
        elif isinstance(cur, dict):
            cur = cur.get(seg)
        else:
            return ""
    return json.dumps(cur, ensure_ascii=False)[:MAX_TEXT] if not isinstance(
        cur, str) else cur


# ------------------------------------------------------------------ 工具表
#  name → (函数, OpenAI function 定义)
TOOLS = {
    "get_current_time": (t_now, {
        "name": "get_current_time",
        "description": "获取当前日期时间。timezone 可为 local 或 utc。",
        "parameters": {
            "type": "object",
            "properties": {
                "timezone": {"type": "string", "enum": ["local", "utc"],
                             "description": "时区，默认 local"},
                "format": {"type": "string", "description": "strftime 格式"},
            },
        },
    }),
    "calculator": (t_calc, {
        "name": "calculator",
        "description": "计算数学表达式，支持 + - * / ( ) %，不支持幂运算",
        "parameters": {
            "type": "object",
            "properties": {"expression": {"type": "string",
                                          "description": "如 (12+8)*3"}},
            "required": ["expression"],
        },
    }),
    "get_weather": (t_weather, {
        "name": "get_weather",
        "description": "查询城市天气（本地模拟数据，不联网）",
        "parameters": {
            "type": "object",
            "properties": {"city": {"type": "string", "description": "城市名"}},
            "required": ["city"],
        },
    }),
    "random_number": (t_random, {
        "name": "random_number",
        "description": "生成 [min, max] 区间内的随机整数",
        "parameters": {
            "type": "object",
            "properties": {
                "min": {"type": "integer", "default": 0},
                "max": {"type": "integer", "default": 100},
            },
        },
    }),
    "unit_convert": (t_unit_convert, {
        "name": "unit_convert",
        "description": "单位换算：长度/重量/存储/温度",
        "parameters": {
            "type": "object",
            "properties": {
                "value": {"type": "number"},
                "from": {"type": "string", "description": "源单位，如 km / kg / c"},
                "to": {"type": "string", "description": "目标单位，如 m / g / f"},
            },
            "required": ["value", "from", "to"],
        },
    }),
    "read_file": (t_read_file, {
        "name": "read_file",
        "description": "读取面板沙箱目录内的文本文件（data/tools_sandbox/ 下）",
        "parameters": {
            "type": "object",
            "properties": {"path": {"type": "string",
                                    "description": "相对路径，如 notes.txt"}},
            "required": ["path"],
        },
    }),
    "write_file": (t_write_file, {
        "name": "write_file",
        "description": "写入沙箱文件（需在设置里开启写入权限）",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"},
            },
            "required": ["path", "content"],
        },
    }),
    "http_get": (t_http_get, {
        "name": "http_get",
        "description": "发起 HTTP GET 请求（需在设置里开启；默认拦截内网地址）",
        "parameters": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "timeout": {"type": "number", "default": 12},
            },
            "required": ["url"],
        },
    }),
    "json_query": (t_json_query, {
        "name": "json_query",
        "description": "从 JSON 数据里按点号路径取值，如 data.items.0.name",
        "parameters": {
            "type": "object",
            "properties": {
                "data": {"type": "string", "description": "JSON 文本或对象"},
                "path": {"type": "string", "description": "如 items.0.name"},
            },
            "required": ["data"],
        },
    }),
}


# ------------------------------------------------------------------ 对外 API
def list_tools(names=None):
    """返回 OpenAI tools 数组"""
    out = []
    for k, (_, spec) in TOOLS.items():
        if names and k not in names:
            continue
        out.append({"type": "function", "function": spec})
    return out


def tool_names():
    return sorted(TOOLS.keys())


def run_tool(name, arguments, options=None):
    """
    执行一个工具调用。
    arguments 可以是 str（模型给的是 JSON 字符串）或 dict。
    返回 (ok, 结果字符串 或 错误信息)
    """
    ent = TOOLS.get(name)
    if not ent:
        return False, "未知工具：%s（可用：%s）" % (name, ", ".join(tool_names()))
    fn = ent[0]
    if isinstance(arguments, str):
        try:
            arguments = json.loads(arguments or "{}")
        except Exception:
            arguments = {"_raw": arguments}
    if not isinstance(arguments, dict):
        arguments = {"value": arguments}
    opt = options or {}
    try:
        return True, fn(arguments, opt)
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, str(e)[:200])


def execute_tool_calls(tool_calls, options=None):
    """
    批量执行上游返回的 tool_calls。
    返回 [{id, name, args, ok, result}]，同时产出可直接回传的
    role=tool 消息数组。
    """
    results, messages = [], []
    for i, tc in enumerate(tool_calls or []):
        if not isinstance(tc, dict):
            continue
        fn = tc.get("function") or {}
        name = fn.get("name") or ""
        raw_args = fn.get("arguments")
        okk, res = run_tool(name, raw_args, options)
        cid = tc.get("id") or ("call_%d" % i)
        results.append({"id": cid, "name": name, "args": raw_args,
                        "ok": okk, "result": res})
        messages.append({"role": "tool", "tool_call_id": cid,
                         "content": str(res)[:MAX_TEXT]})
    return results, messages


def sandbox_info():
    os.makedirs(SANDBOX, exist_ok=True)
    files = []
    for root, dirs, fs in os.walk(SANDBOX):
        for f in fs[:200]:
            p = os.path.join(root, f)
            try:
                files.append({"name": os.path.relpath(p, SANDBOX),
                              "size": os.path.getsize(p)})
            except OSError:
                pass
    return {"sandbox": SANDBOX, "files": files}
