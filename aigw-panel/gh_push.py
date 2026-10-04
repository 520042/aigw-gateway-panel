# -*- coding: utf-8 -*-
"""
用 GitHub Contents API 推送（绕过 github.com 域名不通的问题）。

背景：本机网络下 `github.com` 不可达（git push 静默失败），
但 `api.github.com` 走代理可用。Contents API 逐文件 PUT：
  1) GET  取文件当前 sha（有则是更新，无则是新建）
  2) PUT  content(base64) + message + branch + sha

首次会自动建分支（用 default_branch 的第一个 PUT 触发）。
"""
import base64
import json
import os
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
REPO = os.environ.get("AIGW_GH_REPO", "520042/aigw-gateway-panel")
BRANCH = os.environ.get("AIGW_GH_BRANCH", "main")
TOKEN_FILE = os.path.join(HERE, "data", ".gh_token.tmp")
API = "https://api.github.com"

ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE
_op = urllib.request.build_opener(
    urllib.request.ProxyHandler({}),
    urllib.request.HTTPSHandler(context=ctx))


def clean():
    for k in list(os.environ):
        if k.upper().endswith("_PROXY"):
            os.environ.pop(k, None)


def api(method, path, body=None, tok=None, raw=False):
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(API + path, data=data, method=method)
    req.add_header("Authorization", "Bearer " + (tok or ""))
    req.add_header("Accept", "application/vnd.github+json")
    req.add_header("User-Agent", "aigw-panel-push")
    if data:
        req.add_header("Content-Type", "application/json")
    try:
        with _op.open(req, timeout=45) as x:
            t = x.read().decode("utf-8", "replace")
            return x.status, (t if raw else (json.loads(t) if t.strip() else {}))
    except urllib.error.HTTPError as e:
        t = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(t)
        except Exception:
            return e.code, {"_raw": t[:300]}
    except Exception as e:
        return 0, {"_err": "%s: %s" % (type(e).__name__, str(e)[:120])}


def list_files():
    """列出 git 已跟踪的文件（走 git 拿，避免自己解析 .gitignore）"""
    env = dict(os.environ)
    env["GIT_OPTIONAL_LOCKS"] = "0"
    r = subprocess.run(["git", "-C", ROOT, "ls-files", "-z"],
                       capture_output=True, env=env)
    if r.returncode != 0:
        return []
    return [x for x in r.stdout.decode("utf-8", "replace").split("\0") if x]


def get_ref(tok):
    """确保分支存在，返回 ref sha"""
    c, d = api("GET", "/repos/%s/git/ref/heads/%s" % (REPO, BRANCH), tok=tok)
    if c == 200:
        return d["object"]["sha"]
    # 分支不存在：基于默认分支建一个
    c2, repo = api("GET", "/repos/%s" % REPO, tok=tok)
    base = repo.get("default_branch", "main")
    c3, b = api("GET", "/repos/%s/git/ref/heads/%s" % (REPO, base), tok=tok)
    if c3 != 200:
        return None
    base_sha = b["object"]["sha"]
    c4, d4 = api("POST", "/repos/%s/git/refs" % REPO,
                 {"ref": "refs/heads/" + BRANCH, "sha": base_sha}, tok=tok)
    if c4 in (200, 201):
        return d4["object"]["sha"]
    return None


def get_sha(tok, path):
    c, d = api("GET", "/repos/%s/contents/%s?ref=%s"
               % (REPO, urllib.parse.quote(path), BRANCH), tok=tok)
    if c == 200 and isinstance(d, dict):
        return d.get("sha")
    return None


def put_file(tok, path, content, msg, sha=None):
    body = {
        "message": msg,
        "content": base64.b64encode(content).decode("ascii"),
        "branch": BRANCH,
    }
    if sha:
        body["sha"] = sha
    p = "/repos/%s/contents/%s" % (REPO, urllib.parse.quote(path))
    return api("PUT", p, body, tok=tok)


def main():
    clean()
    if not os.path.exists(TOKEN_FILE):
        print("找不到 token：%s" % TOKEN_FILE)
        return 2
    tok = open(TOKEN_FILE).read().strip()
    files = list_files()
    if not files:
        print("git ls-files 为空，先 git add")
        return 2
    print("仓库 %s 分支 %s，待上传 %d 个文件" % (REPO, BRANCH, len(files)))

    c, d = api("GET", "/repos/%s" % REPO, tok=tok)
    if c != 200:
        print("仓库不可访问 HTTP", c, str(d)[:160])
        return 2
    print("仓库已存在，默认分支 %s" % d.get("default_branch"))

    ref = get_ref(tok)
    print("目标 ref:", (ref or "(新建分支)")[:12])

    ok = fail = skip = 0
    errors = []
    for i, rel in enumerate(files, 1):
        src = os.path.join(ROOT, rel)
        try:
            with open(src, "rb") as f:
                data = f.read()
        except Exception as e:
            skip += 1
            continue
        if b"\0" in data[:8000]:
            skip += 1
            continue
        sha = get_sha(tok, rel)
        c2, d2 = put_file(tok, rel, data,
                          "feat: 上传 %s" % rel if not sha else "chore: 更新 %s" % rel,
                          sha)
        if c2 in (200, 201):
            ok += 1
            if i % 10 == 0 or i == len(files):
                print("  %d/%d  成功 %d" % (i, len(files), ok))
        else:
            fail += 1
            msg = d2.get("message") if isinstance(d2, dict) else str(d2)
            errors.append((rel, c2, msg))
            if fail <= 5:
                print("  [!] %s → %s %s" % (rel, c2, str(msg)[:90]))
        time.sleep(0.12)          # 避免触发 secondary rate limit

    print()
    print("完成：成功 %d / 跳过 %d / 失败 %d" % (ok, skip, fail))
    if errors:
        print("失败明细：")
        for r, c2, m in errors[:12]:
            print("  %-52s %s %s" % (r[:52], c2, str(m)[:60]))
    return 0 if fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
