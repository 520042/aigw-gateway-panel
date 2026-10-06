# -*- coding: utf-8 -*-
"""
全量 APK 内部结构提取器（2026-10-05，回应用户质疑：
"明明我的几个APP都有网关，里面也有模型，也有供应商的接入方式"）

对 6 个 base*.apk 逐个做：
  1) zip 条目清单（assets/ 下的 JSON/文本全部另存）
  2) classes*.dex 字符串池提取（标准库实现，不需要 dex 库）
  3) 从字符串里分类提取：
     - 网关路由   /v1/* /api/* 等本地网关端点
     - 模型清单   gpt/claude/gemini/glm/kimi/deepseek/doubao/hunyuan/qwen… 模式
     - 上游供应商 https?:// URL + 域名归类
     - 鉴权方式   Authorization/Bearer/cookie/token 字段名
     - 网关端口   监听端口线索
输出：data/apk_<n>_<pkg>/ 目录 + data/apk_extract_summary.json
"""
import io, os, re, json, zipfile, sys
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
OUT = os.path.join(HERE, "data")
os.makedirs(OUT, exist_ok=True)

APKS = {
    "base.apk":     "aigw.app (Trae 国内)",
    "base(1).apk":  "dev.doubao2api (豆包)",
    "base(2).apk":  "com.joy4fire.wb2apimobile (Go)",
    "base(3).apk":  "com.joy4fire.workbuddy2api (CodeBuddy 国际)",
    "base(4).apk":  "dev.raccoon2api (小浣熊)",
    "base(5).apk":  "dev.yuanbao2api (元宝)",
}

# dex 字符串池粗提取：可打印 UTF8 片段（>=4 长度）。对 .dex 直接扫
# MUTF-8 连续段足够拿到绝大多数字面量（与项目既有 gen_*_models.py 同思路）
_STR = re.compile(rb"[\x20-\x7e\xe0-\xef\x80-\xbf]{4,}")

def dex_strings(blob):
    out = []
    for m in _STR.finditer(blob):
        try:
            s = m.group().decode("utf-8", "strict")
        except UnicodeDecodeError:
            continue
        if 4 <= len(s) <= 300:
            out.append(s)
    return out

# 分类正则
RE_ROUTE = re.compile(r"^/(v\d|api|alice|samantha|admin|console|trae|wenchain|apiAi)[/\w.\-:{}]*$")
RE_MODEL = re.compile(r"(?i)^[a-z0-9_\-./]*(gpt|claude|gemini|glm|hunyuan|kimi|moonshot|deepseek"
                      r"|doubao|seed|qwen|abab|minimax|step|baichuan|ernie|grok|llama|o[134]"
                      r"|mixtral|qwen[\w\-]*|longcat|wizardlm|codestral|auto)[a-z0-9_.\-]*$")
RE_EXCLUDE_MODEL = re.compile(r"(?i)(http|www\.|\.png|\.jpg|\.json|\.so|\.dex|android|kotlin"
                              r"|java|com\.|org\.|cn\.|intent|permission|activity|service"
                              r"|layout|drawable|string|style|color|layout|meta|package|gradle"
                              r"|support|appcompat|compose|material|runtime|annotation)")
RE_URL = re.compile(r"https?://[\w.\-]+(?:/[\w./\-?=&%{}]*)?")
RE_PORT = re.compile(r"(?i)(?:port|listen)[\"':=\s]{1,4}(\d{4,5})")

def extract(apk_path, tag):
    res = {"apk": os.path.basename(apk_path), "tag": tag,
           "assets": [], "dex_count": 0, "routes": [], "models": [],
           "urls": {}, "auth": [], "ports": []}
    with zipfile.ZipFile(apk_path) as z:
        names = z.namelist()
        # 1) 资产文件
        for n in names:
            if n.startswith(("assets/", "res/raw/")) and not n.endswith("/"):
                try:
                    b = z.read(n)
                except Exception:
                    continue
                ext = os.path.splitext(n)[1].lower()
                if ext in (".json", ".txt", ".md") or b[:1] in (b"{", b"["):
                    safe = n.replace("/", "_")
                    with open(os.path.join(OUT, "apk_%s_%s" % (tag, safe)), "wb") as f:
                        f.write(b)
                    res["assets"].append({"name": n, "size": len(b)})
        # 2) dex 字符串
        blobs = []
        for n in names:
            if re.match(r"classes\d*\.dex$", n):
                res["dex_count"] += 1
                blobs.append(z.read(n))
            elif n.endswith(".dex") and n.startswith("lib/"):
                pass
        allstr = []
        for b in blobs:
            allstr.extend(dex_strings(b))
        # 去重保序
        seen, uniq = set(), []
        for s in allstr:
            if s not in seen:
                seen.add(s)
                uniq.append(s)
        res["_nstrings"] = len(uniq)
        # 3) 分类
        routes, models = set(), set()
        urls = {}
        for s in uniq:
            if RE_ROUTE.match(s) and len(s) > 3:
                routes.add(s)
            if RE_MODEL.match(s) and not RE_EXCLUDE_MODEL.search(s) and 2 < len(s) < 64:
                models.add(s)
            for u in RE_URL.findall(s):
                host = u.split("/")[2]
                urls.setdefault(host, []).append(u)
        for s in uniq:
            m = RE_PORT.search(s)
            if m:
                res["ports"].append(s[:80])
        # 鉴权线索
        for kw in ("Authorization", "Bearer ", "x-api-key", "X-Api-Key",
                   "api_key", "apiKey", "access_token", "accessToken",
                   "hy_token", "sessionKey", "uskey", "device_id", "Cookie"):
            hits = [s for s in uniq if kw.lower() in s.lower() and len(s) < 120]
            if hits:
                res["auth"].append({kw: hits[:3]})
        res["routes"] = sorted(routes)
        res["models"] = sorted(models)
        res["urls"] = {h: sorted(set(v))[:12] for h, v in
                       sorted(urls.items(), key=lambda kv: -len(kv[1]))[:20]}
    return res


def main():
    summary = {}
    for apk, tag in APKS.items():
        p = os.path.join(ROOT, apk)
        if not os.path.exists(p):
            print("!! 缺文件", apk)
            continue
        print("=" * 70)
        print("◆", apk, "→", tag)
        r = extract(p, tag.split(" ")[0].replace(".", "_").replace("/", "_"))
        summary[apk] = r
        print("  assets: %d 个 | dex: %d 个 | 字符串: %d 条"
              % (len(r["assets"]), r["dex_count"], r["_nstrings"]))
        print("  资产文件:", [a["name"] for a in r["assets"]][:10])
        v1 = [x for x in r["routes"] if x.startswith("/v1")]
        print("  本地网关 /v1 路由 (%d):" % len(v1))
        for x in v1[:25]:
            print("    ", x)
        print("  上游域名 TOP:")
        for h, us in list(r["urls"].items())[:10]:
            print("    %-34s %d 条  例: %s" % (h, len(us), us[0][:70]))
        print("  模型样字符串 (%d):" % len(r["models"]))
        for x in r["models"][:40]:
            print("    ", x)
    with open(os.path.join(OUT, "apk_extract_summary.json"), "w", encoding="utf-8") as f:
        json.dump(summary, f, ensure_ascii=False, indent=1)
    print("\n汇总已落盘 data/apk_extract_summary.json")


if __name__ == "__main__":
    main()
