# -*- coding: utf-8 -*-
import ast, sys

def pinfo(path):
    src = open(path, encoding="utf-8").read()
    t = ast.parse(src)
    for n in ast.walk(t):
        if isinstance(n, ast.Assign) and any(isinstance(x, ast.Name) and x.id == "PLATFORMS" for x in n.targets):
            try:
                d = ast.literal_eval(n.value)
                return (set(d.keys()), [k for k, v in d.items() if not (isinstance(v, dict) and v.get("method"))])
            except Exception as e:
                names = [k.value for k in n.value.keys if isinstance(k, ast.Constant)]
                return (set(names), ["LITERAL_EVAL_FAIL:" + str(e)[:120]])
    return set(), ["PLATFORMS_NOT_FOUND"]

H = "C:/Users/liang.zhao/AppData/Local/Temp/aigw-head-probe/aigw-panel/app/gwlogin.py"
W = "C:/Users/liang.zhao/Downloads/\u7f51\u5173\u9879\u76ee/aigw-panel/app/gwlogin.py"
kh, mh = pinfo(H)
kw, mw = pinfo(W)
print("HEAD count:", len(kh))
print("WORK count:", len(kw))
print("WORK added:", sorted(kw - kh))
print("WORK removed:", sorted(kh - kw))
print("HEAD missing-method:", mh)
print("WORK missing-method:", mw)

sys.path.insert(0, "C:/Users/liang.zhao/Downloads/\u7f51\u5173\u9879\u76ee/aigw-panel")
try:
    from app import gwextra, native_relay
    print("ACTIONS total:", sum(len(v) for v in gwextra.ACTIONS.values()))
    print("MODEL_CATALOGS:", sorted(gwextra.MODEL_CATALOGS.keys()))
    print("RELAY_SPECS n:", len(native_relay.RELAY_SPECS), sorted(native_relay.RELAY_SPECS))
except Exception as e:
    print("IMPORT_FAIL:", type(e).__name__, str(e)[:200])