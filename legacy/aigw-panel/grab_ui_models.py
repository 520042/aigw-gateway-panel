# -*- coding: utf-8 -*-
"""
从网页 UI 里直接抠模型选项（用户截图里那个菜单）。
思路：找到「模型」按钮 → 用 CDP 模拟点击展开 → 读弹出层的文本。
"""
import os
import sys
import time
import json

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app import cdp

PROFILE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "data", "chrome-capture-profile")

# 页内 JS：找到含指定文本的可点元素并点击
CLICK_JS = """
(function(kw){
  var els = document.querySelectorAll('button, [role="button"], [class*="select"], [class*="model"], div, span');
  var best=null;
  for (var i=0;i<els.length;i++){
    var t=(els[i].innerText||'').trim();
    if (t && t.length<20 && t.indexOf(kw)>=0){
      var r=els[i].getBoundingClientRect();
      if (r.width>0 && r.height>0 && r.width<400 && r.height<80){ best=els[i]; }
    }
  }
  if(best){ best.click(); return 'clicked:'+best.tagName+':'+(best.innerText||'').slice(0,20);}
  return 'notfound';
})(%(kw)s)
"""

READ_JS = """
(function(){
  var t = document.body.innerText || '';
  return JSON.stringify(t.split('\\n').map(function(x){return x.trim();})
    .filter(function(x){return x && x.length<40;}));
})()
"""


def ev(s, expr):
    try:
        r = s.call("Runtime.evaluate",
                   {"expression": expr, "returnByValue": True}, timeout=20)
        return ((r or {}).get("result") or {}).get("value")
    except Exception as e:
        return "ERR:%s" % type(e).__name__


def grab(b, url, label, trigger_kw, wanted):
    print("\n" + "=" * 78)
    print(label, url)
    print("=" * 78)
    sessions = b.attach_all_pages()
    if not sessions:
        print("无标签")
        return []
    s = list(sessions.values())[0]
    try:
        s.call("Page.enable", {}, timeout=10)
        s.call("Runtime.enable", {}, timeout=10)
        s.call("Page.navigate", {"url": url}, timeout=15)
    except Exception as e:
        print("nav 失败:", e)
    time.sleep(9)

    out = []
    for kw in trigger_kw:
        r = ev(s, CLICK_JS % {"kw": json.dumps(kw, ensure_ascii=False)})
        print("  点击[%s]: %s" % (kw, r))
        time.sleep(1.6)
        lines = ev(s, READ_JS)
        try:
            lines = json.loads(lines) if isinstance(lines, str) else []
        except Exception:
            lines = []
        for ln in lines:
            if any(w.lower() in ln.lower() for w in wanted):
                out.append(ln)
    uniq = sorted(set(out))
    for x in uniq:
        print("   模型项:", x)
    cdp.Browser.close_all(sessions)
    return uniq


def main():
    b = cdp.Browser(port=9488, user_data_dir=PROFILE)
    b.start(url="https://yuanbao.tencent.com/chat/")
    print("浏览器已启动")
    time.sleep(4)

    res = {}
    res["yuanbao"] = grab(b, "https://yuanbao.tencent.com/chat/", "元宝",
                          ["模型", "Hy4", "模型选择"],
                          ["Hy4", "Hy3", "DeepSeek", "hunyuan", "混元", "专用"])
    res["doubao"] = grab(b, "https://www.doubao.com/chat/", "豆包",
                         ["模型", "DeepSeek", "模型选择"],
                         ["Seed", "DeepSeek", "doubao", "深度思考", "Thinking"])
    b.close()
    with open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                           "data", "ui_models.json"), "w", encoding="utf-8") as f:
        json.dump(res, f, ensure_ascii=False, indent=1)
    print("\n已存 data/ui_models.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
