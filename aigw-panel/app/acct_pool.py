# -*- coding: utf-8 -*-
"""
通用账号池（2026-10-05 从 doubao_relay 抽出，对齐社区 2api 项目标配能力）

对齐的社区能力（lobsterai2api / qwen2api / kimi2api / doubao-free-api）：
- 多账号轮询（round-robin）
- 429/限流软冷却 60s、凭据失效硬冷却 12h（lobsterai2api 同款分级）
- 请求级换号：单请求失败自动换下一个账号，最多 3 次（lobsterai2api 同款）
- 池状态查询（total / cooling）
"""

import time
import hashlib

COOLDOWN_SOFT = 60.0       # 429/限流 → 60s（社区同款）
COOLDOWN_HARD = 12 * 3600  # 凭据失效/额度耗尽 → 12h（lobsterai2api 同款）
MAX_ROTATION = 3           # 单请求最多换号次数（lobsterai2api 同款）

_cooldown = {}   # (platform, secret_fp) -> 冷却截止 ts
_rr = {}         # platform -> 轮转计数

# copilot 家族共享凭据：wb-gateway（原生登录落库）与 apk-codebuddy/-cn
# 是同一套腾讯接口，同一个 accessToken 通用（2026-10-06 live 实测通过）。
# 取凭据时把家族内其它平台的账号一并算进来，聚合路由才能用上真凭据。
COPILOT_FAMILY = ("wb-gateway", "wb-gateway-intl", "apk-codebuddy", "apk-codebuddy-cn")


def _family_pids(pid):
    if pid not in COPILOT_FAMILY:
        return (pid,)
    return COPILOT_FAMILY


def _fp(secret):
    return hashlib.md5((secret or "").encode("utf-8")).hexdigest()[:12]


def usable_secrets(accounts, pid):
    """账号池里该平台全部可用凭据（带名字），不过滤冷却。
    copilot 家族平台互相共享凭据（见 COPILOT_FAMILY）；按密钥指纹去重。"""
    if not accounts:
        return []
    rows, seen = [], set()
    for p in _family_pids(pid):
        for a in (accounts.usable(p) or []):
            if not a.get("secret"):
                continue
            fp = _fp(a["secret"])
            if fp in seen:
                continue
            seen.add(fp)
            rows.append(a)
    return rows


def pick_secret(accounts, pid):
    """轮询挑一个不在冷却期的凭据。返回 (secret, name)；全冷却返回 ("","")。"""
    rows = usable_secrets(accounts, pid)
    if not rows:
        return "", ""
    now = time.time()
    fresh = [a for a in rows if _cooldown.get((pid, _fp(a["secret"])), 0) <= now]
    if not fresh:
        return "", "全部账号冷却中"
    i = _rr.get(pid, 0) % len(fresh)
    _rr[pid] = i + 1
    a = fresh[i]
    return a.get("secret", ""), a.get("name") or pid


def mark_cooldown(pid, secret, level="soft"):
    """level: soft=60s（429/限流）| hard=12h（失效/额度耗尽）"""
    _cooldown[(pid, _fp(secret))] = time.time() + (
        COOLDOWN_HARD if level == "hard" else COOLDOWN_SOFT)


def clear_cooldown(pid, secret):
    _cooldown.pop((pid, _fp(secret)), None)


def pool_status(accounts, pid):
    rows = usable_secrets(accounts, pid)
    now = time.time()
    return {"total": len(rows),
            "cooling": sum(1 for a in rows
                           if _cooldown.get((pid, _fp(a.get("secret", ""))), 0) > now)}


def try_accounts(accounts, pid, fn):
    """
    请求级换号：对池内账号依次执行 fn(secret, name, acct_row)，
    返回第一个成功结果 (ok, payload, name)。
    fn 第三个参数是账号行 dict（可带 uid 等账号级字段，用于风控头）。
    失败分类由 fn 返回的 payload.level 决定冷却档位
    （fn 约定：失败时 payload 为 dict 且带 "cooldown": "soft"/"hard"）。
    全部失败返回最后一次的 (False, payload, name)。
    """
    rows = usable_secrets(accounts, pid)
    if not rows:
        return False, "账号池里没有 %s 的可用凭据" % pid, ""
    now = time.time()
    fresh = [a for a in rows if _cooldown.get((pid, _fp(a["secret"])), 0) <= now]
    if not fresh:
        return False, {"message": "全部账号冷却中",
                       "pool": pool_status(accounts, pid)}, ""
    last = (False, "无可用账号", "")
    tried = 0
    for a in fresh:
        if tried >= MAX_ROTATION:
            break
        tried += 1
        secret, name = a.get("secret", ""), a.get("name") or pid
        ok, payload = fn(secret, name, a)
        if ok:
            return True, payload, name
        last = (False, payload, name)
        level = "soft"
        if isinstance(payload, dict):
            level = payload.get("cooldown", "soft")
        mark_cooldown(pid, secret, level)
    return last[0], last[1], last[2]
