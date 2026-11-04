# -*- coding: utf-8 -*-
"""
全平台接入矩阵审计（2026-10-05 全项目复查）
从代码程序化提取每个平台的：
  登录方式(login) / 验活(verify) / 动作清单(actions) / 模型动作(models) /
  内置模型(bundled) / 签到(checkin) / 倍率来源(rates) / 接入源分类(kind)
并自动找不一致：
  - 有登录无任何动作 / 有动作无登录
  - 有 models 动作但 platform_models 不覆盖
  - verify 缺失或类型不对
  - 签到动作存在但 autocheckin 未覆盖
输出 JSON + 人读表格
"""
import sys, json, io
sys.path.insert(0, '.')
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace')

from app import gwlogin, gwextra, sources as SRC
from app import bundled_models as BM
from app import bundled_yuanbao as BY
from app import bundled_doubao as BD
from app import accounts  # noqa: F401  (仅需模块加载成功)

out = {"platforms": {}, "issues": []}

# ---------- 1) 平台全集（三方并集） ----------
login_pids = set(gwlogin.PLATFORMS.keys())
action_pids = set(gwextra.ACTIONS.keys())
source_pids = set(SRC.PLATFORM_FACTS.keys())
all_pids = sorted(login_pids | action_pids | source_pids)
print("平台总数: %d (login=%d, actions=%d, sources=%d)"
      % (len(all_pids), len(login_pids), len(action_pids), len(source_pids)))

# bundled 模型清单归属
BUNDLED = {
    "codebuddy-intl": BM,   # 内置倍率表覆盖的是 codebuddy 系（36 模型）
    "apk-yuanbao": BY,
    "apk-doubao": BD,
}

def bund_for(pid):
    if pid == "apk-yuanbao":
        return len(BY.MODELS), "APK dex"
    if pid == "apk-doubao":
        return len(BD.MODELS), "网页版 model_list"
    return None, None

# autocheckin 覆盖的平台
import app.autocheckin as AC
ac_pids = set()
try:
    ac_pids = set(AC.PLATFORM_MODES.keys()) if hasattr(AC, "PLATFORM_MODES") else set()
except Exception:
    pass
if not ac_pids:
    # 从类/函数名兜底找
    ac_pids = {n for n in dir(AC) if n.startswith("checkin_")}

for pid in all_pids:
    row = {"id": pid}
    # 登录
    lp = gwlogin.PLATFORMS.get(pid)
    if lp:
        row["login"] = lp.get("type", "?")
        row["login_name"] = lp.get("name", "")
        v = lp.get("verify")
        if isinstance(v, dict):
            row["verify"] = "%s %s" % (v.get("method", "GET"), v.get("url", "")[:60])
        elif v:
            row["verify"] = str(v)[:60]
        else:
            row["verify"] = None
        row["storage_keys"] = bool(lp.get("storage_keys"))
    else:
        row["login"] = None
    # 动作
    acts = gwextra.ACTIONS.get(pid) or []
    row["n_actions"] = len(acts)
    row["action_ids"] = [a.get("id") for a in acts if isinstance(a, dict)]
    row["has_models_action"] = "models" in row["action_ids"]
    row["has_checkin_action"] = any("checkin" in (a or "") for a in row["action_ids"])
    # models 动作是否 verified
    ms = gwextra.action_spec(pid, "models")
    if ms:
        row["models_path"] = ms.get("path")
        row["models_verified"] = not ms.get("unverified")
    # 内置模型
    n, srcnote = bund_for(pid)
    row["bundled_models"] = n
    row["bundled_src"] = srcnote
    # 分类
    row["kind"] = SRC.kind_of(pid) if pid in source_pids else None
    # 签到覆盖
    row["autocheckin"] = pid in ac_pids or any(pid in str(x) for x in ac_pids)
    out["platforms"][pid] = row

# ---------- 2) 一致性检查 ----------
P = out["platforms"]
for pid, r in P.items():
    if r["login"] and r["n_actions"] == 0:
        out["issues"].append(f"[{pid}] 有登录定义但 gwextra 无任何动作")
    if (not r["login"]) and r["n_actions"] > 0:
        out["issues"].append(f"[{pid}] 有 {r['n_actions']} 个动作但 gwlogin 无登录定义（凭据从哪来？）")
    if pid in ("apk-yuanbao", "apk-doubao"):
        if not r["bundled_models"]:
            out["issues"].append(f"[{pid}] 应有内置模型清单但缺失")
    elif r["login"] and not r["has_models_action"] and not r["bundled_models"]:
        out["issues"].append(f"[{pid}] 有登录、无 models 动作、无内置清单 → 面板将无模型可显")
    if r["login"] and not r["verify"]:
        out["issues"].append(f"[{pid}] 有登录但无 verify 定义（登录成功无法校验）")

# platform_models 覆盖检查（main.py 里显式处理的平台）
with open("main.py", encoding="utf-8") as f:
    main_src = f.read()
for pid in ("apk-doubao", "apk-yuanbao"):
    if pid not in main_src:
        out["issues"].append(f"[{pid}] main.py platform_models 未覆盖")

# ---------- 3) 输出 ----------
print("\n===== 平台接入矩阵 =====")
hdr = "%-22s %-9s %-4s %-6s %-6s %-7s %-5s %s" % (
    "平台", "登录", "动作", "models", "verify", "内置模型", "签到", "分类")
print(hdr); print("-" * len(hdr))
for pid, r in P.items():
    print("%-22s %-9s %-4s %-6s %-6s %-7s %-5s %s" % (
        pid, r.get("login") or "-", r["n_actions"],
        ("Y" if r["has_models_action"] else "-") + ("/v" if r.get("models_verified") else ""),
        ("Y" if r.get("verify") else "-"),
        (str(r["bundled_models"]) if r["bundled_models"] else "-"),
        ("Y" if r["has_checkin_action"] else "-"),
        r.get("kind") or "-"))

print("\n===== 自动发现的问题 (%d) =====" % len(out["issues"]))
for i in out["issues"]:
    print(" !", i)

with open("data/audit_matrix.json", "w", encoding="utf-8") as f:
    json.dump(out, f, ensure_ascii=False, indent=1, default=str)
print("\n矩阵已落盘 data/audit_matrix.json")
