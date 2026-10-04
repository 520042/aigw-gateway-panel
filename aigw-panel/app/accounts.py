# -*- coding: utf-8 -*-
"""
面板账号池
==========
把「面板自己跑完登录」拿到的凭据统一管起来：
  - 落库只写本机 data/accounts.json，不外传
  - 列表接口一律掩码，完整密钥只在本地调用上游时才取出
  - 按平台分组统计，供路由引擎挑上游用

账号结构：
  id, platform, name, type(gateway/cookie/api_key/token),
  secret, source, edition, enabled, obtained_at, last_used,
  last_check, status(ok/unknown/dead), quota, note
"""

import time
import uuid

from .store import now_str


class Accounts:
    def __init__(self, store):
        self.store = store

    # ------------------------------------------------------------ 读写
    def _all(self):
        return self.store.get("accounts")

    def _save(self, rows):
        return self.store.put("accounts", rows)

    # ------------------------------------------------------------ 增改
    def add(self, acct):
        """新增或更新（同 platform + name 视为同一账号）"""
        rows = self._all()
        platform = acct.get("platform", "")
        name = acct.get("name") or ""
        now = now_str()
        for a in rows:
            if a.get("platform") == platform and (a.get("name") or "") == name:
                a.update({
                    "type": acct.get("type", a.get("type", "")),
                    "secret": acct.get("secret", a.get("secret", "")),
                    "source": acct.get("source", a.get("source", "")),
                    "edition": acct.get("edition", a.get("edition", "")),
                    "obtained_at": now,
                })
                a.setdefault("enabled", True)
                self._save(rows)
                return a
        item = {
            "id": uuid.uuid4().hex[:12],
            "platform": platform,
            "name": name or platform,
            "type": acct.get("type", ""),
            "secret": acct.get("secret", ""),
            "source": acct.get("source", "面板登录"),
            "edition": acct.get("edition", ""),
            "enabled": True,
            "obtained_at": now,
            "last_used": "",
            "last_check": "",
            "status": "unknown",
            "quota": "",
            "note": acct.get("note", ""),
        }
        rows.append(item)
        self._save(rows)
        return item

    def update(self, aid, patch):
        rows = self._all()
        for a in rows:
            if a.get("id") == aid:
                for k in ("name", "secret", "note", "enabled", "status",
                          "quota", "type", "edition", "last_used", "last_check"):
                    if k in patch:
                        a[k] = patch[k]
                self._save(rows)
                return a
        return None

    def delete(self, aid):
        rows = self._all()
        out = [a for a in rows if a.get("id") != aid]
        self._save(out)
        return len(out) != len(rows)

    def get(self, aid):
        for a in self._all():
            if a.get("id") == aid:
                return a
        return None

    def secret_of(self, aid):
        """取完整密钥，仅在本地调上游时使用"""
        a = self.get(aid)
        return (a or {}).get("secret", "")

    # ------------------------------------------------------------ 查询
    def list(self, platform=None, mask=True):
        rows = self._all()
        if platform:
            rows = [a for a in rows if a.get("platform") == platform]
        return [self._mask(a) for a in rows] if mask else rows

    def usable(self, platform=None):
        """enabled 且确实有凭据的账号"""
        return [a for a in self._all()
                if a.get("enabled", True) and a.get("secret")
                and (not platform or a.get("platform") == platform)]

    @staticmethod
    def _mask(a):
        out = dict(a)
        s = out.get("secret", "")
        if isinstance(s, str) and s:
            out["secret"] = (s[:6] + "…" + s[-4:]) if len(s) > 12 else "*" * len(s)
            out["secret_len"] = len(s)
        else:
            out["secret"] = ""
            out["secret_len"] = 0
        return out

    # ------------------------------------------------------------ 统计
    def stats(self):
        rows = self._all()
        by_plat = {}
        for a in rows:
            p = a.get("platform", "?")
            d = by_plat.setdefault(p, {"platform": p, "total": 0, "enabled": 0,
                                       "with_secret": 0, "dead": 0})
            d["total"] += 1
            if a.get("enabled", True):
                d["enabled"] += 1
            if a.get("secret"):
                d["with_secret"] += 1
            if a.get("status") == "dead":
                d["dead"] += 1
        return {
            "total": len(rows),
            "enabled": sum(1 for a in rows if a.get("enabled", True)),
            "with_secret": sum(1 for a in rows if a.get("secret")),
            "by_platform": sorted(by_plat.values(), key=lambda x: -x["total"]),
        }
