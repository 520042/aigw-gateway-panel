# -*- coding: utf-8 -*-
"""
数据源一致性检查：gwlogin.verify  vs  gwextra.ACTIONS
======================================================
本项目栽过的跟头（2026-10-04）：
  1. 明明 `data/verify2.txt` 已经存了 46 条**实测过存在**的端点，
     我却没查它，重新上网探测，还把 Trae 的 verify 填错了
     （真答案是 /cloudide/api/v3/trae/GetUserInfo，我写成了 copilot 的接口）。
  2. 明明 `app/gwextra.py` 已经实现了 **50 个动作 / 10 个平台**，
     我又在 gwlogin.py 里抄了一遍 `actions` → 两处维护，必然漂移。

所以这个脚本只做一件事：**检查两份数据不打架**，并列出已验证但没被
verify/自动签到用上的端点（= 功能漏了）。

运行：python test_spec_consistency.py
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

OK, BAD = [], []


def check(name, cond, extra=""):
    (OK if cond else BAD).append(name)
    print("  %s %s %s" % ("PASS" if cond else "FAIL", name, extra))


def parse_verify2():
    """把 data/verify2.txt 解析成 {platform: {action: (method, path, status)}}"""
    fp = os.path.join(HERE, "data", "verify2.txt")
    if not os.path.exists(fp):
        return {}
    out, cur = {}, None
    for line in open(fp, encoding="utf-8", errors="replace"):
        m = re.match(r"###\s+(\S+)\s+base=(\S+)", line)
        if m:
            cur = m.group(1)
            out[cur] = {"base": m.group(2), "actions": {}}
            continue
        if not cur:
            continue
        # ⚠ 三种前缀的空格宽度不一致：`[OK ]` `[BAD]` `[--]`
        #   —— `[BAD]` 后面**没有**空格，别用固定的 `\s+`
        m = re.match(r"\s*\[(?:OK\s|BAD|--)\]\s+(\S+)\s+(GET|POST|PUT|DELETE)\s+(\S+)"
                     r"(?:\s+(\S+))?", line)
        if m:
            act, method, path, status = (m.group(1), m.group(2),
                                         m.group(3), m.group(4) or "")
            out[cur]["actions"][act] = (method, path, status)
    return out


def main():
    from app.gwlogin import PLATFORMS
    from app.gwextra import ACTIONS

    v2 = parse_verify2()
    print("=" * 92)
    print("数据源一致性检查")
    print("  data/verify2.txt  %d 个平台的实测端点记录" % len(v2))
    print("  app/gwextra.py    %d 个平台 / %d 个动作"
          % (len(ACTIONS), sum(len(a) for a in ACTIONS.values())))
    print("=" * 92)
    # verify2 里状态是 "-" 的行也要收（apk-antigravity 三条全是网络不通，
    # 但路径确实是从 APK 里挖出来的，不算「无出处」）
    all_paths = {}
    for pid, info in v2.items():
        all_paths[pid] = {p for (_, p, _) in info["actions"].values()}

    print("\n【1】gwextra 的动作必须与 verify2.txt 对得上（同一份 APK 逆向结果）")
    for pid, acts in ACTIONS.items():
        if pid not in v2:
            print("  - %-22s verify2.txt 里没有这个平台（正常，来源不同）" % pid)
            continue
        rec = v2[pid]["actions"]
        ours = {a["id"]: a for a in acts}
        # 找 gwextra 有但 verify2 没记的
        extra = [k for k in ours if k not in rec]
        # 找 verify2 有但 gwextra 没实现的
        missing = [k for k in rec if k not in ours]
        if extra:
            print("  - %-22s gwextra 多出 %d 个（verify2 未记，需确认）"
                  % (pid, len(extra)))
        if missing:
            print("  ! %-22s verify2 里有但 gwextra **没实现**：%s"
                  % (pid, ", ".join(missing)))
            BAD.append("%s 未实现 %s" % (pid, missing))
        else:
            check("%-22s 动作与 verify2 完全对齐" % pid, True,
                  "%d 个" % len(ours))

    print("\n【2】verify2.txt 标了 verified 的端点，必须在 gwextra 里且带 verified")
    for pid, info in v2.items():
        if pid not in ACTIONS:
            continue
        ours = {a["id"]: a for a in ACTIONS[pid]}
        for act, (method, path, status) in info["actions"].items():
            if not status.startswith("2") and not status.startswith("4"):
                continue                      # 只关心 200/4xx（有信息量的）
            if act in ours:
                o = ours[act]
                same_path = o.get("path") == path or \
                    o.get("path", "").rstrip("/") == path.rstrip("/")
                same_m = (o.get("method", "GET").upper() == method.upper()
                          or act == "points_balance")
                if not (same_path and same_m):
                    print("  ! %-22s %-22s 方法/路径不一致：gwextra=%s %s / verify2=%s %s"
                          % (pid, act, o.get("method"), o.get("path"),
                             method, path))
                    BAD.append("%s.%s 不一致" % (pid, act))
                else:
                    check("%-22s %-22s 方法+路径一致" % (pid, act), True)
            else:
                print("  - %-22s %-22s 在 gwextra 里没有（可能不是要集成的动作）"
                      % (pid, act))

    print("\n【3】每个平台的 verify 端点必须来自 verify2.txt（别自己另发明）")
    for pid, spec in PLATFORMS.items():
        v = spec.get("verify")
        if not v:
            continue
        url = v.get("url") or ""
        path = "/" + "/".join(url.split("/")[3:]) if len(url.split("/")) > 3 else url
        # 端点路径（去掉 query）
        path = path.split("?")[0]
        if pid in v2:
            paths = all_paths[pid]
            # 允许 verify 用别的域（copilot 统一后端）
            hit = [p for p in paths if p.split("?")[0] == path]
            if hit:
                check("%-22s verify 来自 verify2.txt" % pid, True, path)
            else:
                # 明确标注是刻意换的
                if "copilot.tencent.com" in url:
                    print("  - %-22s verify 改用 copilot 统一后端（刻意，注释应说明）"
                          % pid)
                else:
                    print("  ! %-22s verify 路径不在 verify2.txt 里：%s" % (pid, path))
                    BAD.append("%s verify 路径无出处" % pid)
        else:
            print("  - %-22s verify2.txt 无此平台记录" % pid)

    print("\n【4】不允许在两处重复维护动作表")
    import io
    s = io.open(os.path.join(HERE, "app", "gwlogin.py"), encoding="utf-8").read()
    check("gwlogin.py 里没有重复的 actions 块", '"actions"' not in s,
          "真源在 gwextra.ACTIONS")
    s2 = io.open(os.path.join(HERE, "app", "gwextra.py"), encoding="utf-8").read()
    check("gwextra.py 是唯一动作真源", "ACTIONS = {" in s2)

    print("\n通过 %d / 失败 %d" % (len(OK), len(BAD)))
    if BAD:
        print("发现问题：")
        for b in BAD:
            print("   · %s" % b)
    return 1 if BAD else 0


if __name__ == "__main__":
    sys.exit(main())
