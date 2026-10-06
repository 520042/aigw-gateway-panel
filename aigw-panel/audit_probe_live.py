# -*- coding: utf-8 -*-
"""无凭据探活：验证面板登记的路径是否真实存在（404 基线 vs 401/200 = 真实端点）"""
import ssl, json, os, urllib.request, urllib.error, concurrent.futures as cf

for k in list(os.environ):
    if k.upper().endswith("_PROXY"):
        os.environ.pop(k, None)

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36")

TARGETS = [
    # (标签, URL, 方法)
    ("kuku model_list (免登录可读?)", "https://kuku.baidu.com/wenchain/genflowpro/model/list", "GET"),
    ("kuku model_list2",             "https://kuku.baidu.com/wenchain/genflow/model/list", "GET"),
    ("kuku ping",                    "https://kuku.baidu.com/api/genflowpro/ping", "GET"),
    ("kuku act_conf",                "https://kuku.baidu.com/act/api/conf", "GET"),
    ("kuku profile(需登录)",          "https://kuku.baidu.com/api/genflowpro/settings/profile", "GET"),
    ("raccoon model_catalog",        "https://xiaohuanxiong.com/model_catalog", "GET"),
    ("doubao user_info(验活)",        "https://www.doubao.com/api/v1/user/info", "GET"),
    ("yuanbao models",               "https://yuanbao.tencent.com/api/models", "GET"),
    ("yuanbao getuserinfo",          "https://yuanbao.tencent.com/api/getuserinfo", "GET"),
    ("wps lingxi settings",          "https://lingxi.kdocs.cn/api/public/v1/settings", "GET"),
    ("wps lingxi credits",           "https://lingxi.kdocs.cn/api/public/v1/credits/balance", "GET"),
    ("wps drive userinfo",           "https://drive.kdocs.cn/api/v3/userinfo", "GET"),
    ("trae checkin_status",          "https://api.trae.cn/trae/api/v2/ug/checkin_credits/status", "GET"),
    ("trae entitlement",             "https://api.trae.cn/trae/api/v2/pay/user_current_entitlement_list", "GET"),
    ("codebuddy models",             "https://www.codebuddy.ai/console/enterprises/personal/models", "GET"),
    ("coze v3 chat(POST)",           "https://api.coze.cn/v3/chat", "POST"),
]

def probe(item):
    label, url, m = item
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    hdrs = {"User-Agent": UA, "Accept": "application/json"}
    data = b"{}" if m == "POST" else None
    if data is not None:
        hdrs["Content-Type"] = "application/json"
    try:
        req = urllib.request.Request(url, data=data, headers=hdrs, method=m)
        with urllib.request.urlopen(req, timeout=12, context=ctx) as r:
            body = r.read(300)
            return label, url, r.status, body.decode("utf-8", "replace")[:160]
    except urllib.error.HTTPError as e:
        return label, url, e.code, (e.read(200).decode("utf-8", "replace")[:160])
    except Exception as e:
        return label, url, "ERR", "%s: %s" % (type(e).__name__, str(e)[:120])

with cf.ThreadPoolExecutor(max_workers=8) as ex:
    for label, url, code, body in ex.map(probe, TARGETS):
        print("%-4s %-38s %s" % (code, label, url))
        if body:
            print("        %s" % body.replace("\n", " ")[:150])
