# -*- coding: utf-8 -*-
"""从旧版 catalog_bundled.json 批量生成 v2 providers/*.toml（缺失平台补齐）。"""
import io, json, os, re

CAT = r'C:\Users\liang.zhao\Downloads\网关项目\aigw-panel-rs\catalog_bundled.json'
OUT = r'C:\Users\liang.zhao\Downloads\网关项目\aigw2\providers'

with io.open(CAT, encoding='utf-8') as f:
    cat = json.load(f)

platforms = cat['platforms']
relay = cat.get('relay_specs', {})
existing = set(os.listdir(OUT))
existing = {p[:-5] for p in existing if p.endswith('.toml')}
print('v2 现有:', sorted(existing))
missing = [pid for pid in platforms if pid not in existing]
print('需生成:', len(missing))

def esc(s):
    return (s or '').replace('\\', '\\\\')

def gen(pid):
    v = platforms[pid]
    method = v.get('method', 'file')
    typ = v.get('type')  # api_key / subscription / None
    upstream = v.get('upstream', '')
    hosts = v.get('hosts') or []
    login_url = v.get('login_url') or ''
    hint = v.get('hint') or ''
    name = v.get('name') or pid
    # base_url 推断
    if pid.startswith('api-') or pid == 'minimax':
        if pid == 'minimax':
            base = 'https://' + upstream if upstream else 'https://agent.minimax.cn'
            auth = 'api_key'
        else:
            base = 'https://' + upstream if upstream else ''
            auth = 'api_key'
        # OpenAI 兼容上游统一拼 /v1（dashscope/qianfan 兼容模式也在 /v1）
        if pid in ('api-dashscope', 'api-bailian'):
            base = 'https://dashscope.aliyuncs.com/compatible-mode/v1'
        elif pid == 'api-qianfan':
            base = 'https://qianfan.baidubce.com/v2'
        elif pid == 'api-xfyun':
            base = 'https://spark-api-open.xf-yun.com/v1'
        elif pid == 'api-volcengine':
            base = 'https://ark.cn-beijing.volces.com/api/v3'
        elif pid == 'api-minimax':
            base = 'https://api.minimax.chat/v1'
        elif pid == 'api-openrouter':
            base = 'https://openrouter.ai/api/v1'
        elif pid == 'api-modelscope':
            base = 'https://api-inference.modelscope.cn/v1'
        elif base and not base.endswith('/v1'):
            base += '/v1'
        policy = 'live'
        dialect = 'sse_delta'
        models = []
        login_url = login_url or v.get('login_url', '')
        l = ['# 官方 API —— OpenAI 兼容透传（api_key）', '# 一手来源：旧版 catalog platforms/' + pid]
        l.append('id = "%s"' % pid)
        l.append('name = "%s"' % name.replace('"', '\\"'))
        l.append('enabled = true')
        l.append('')
        l.append('auth = "api_key"')
        l.append('base_url = "%s"' % base)
        l.append('format = "openai"')
        l.append('')
        l.append('models = []')
        l.append('models_policy = "live"')
        l.append('stream_dialect = "sse_delta"')
        if login_url:
            l.append('')
            l.append('login_url = "%s"' % login_url)
        l.append('')
        l.append('notes = "%s"' % esc(hint).replace('"', '\\"').replace('\n', ' ')[:400])
        return '\n'.join(l)
    if pid.startswith('sub-'):
        # 订阅类：OAuth 设备码/PKCE（v2 登录引擎后续接入），先声明可用路由
        method2 = v.get('method', 'oauth_device')
        base = {
            'sub-kimi': 'https://api.kimi.com/coding/v1',
            'sub-kimi-ai': 'https://api.kimi.ai/coding/v1',
            'sub-codex': 'https://api.openai.com/v1',
            'sub-claude': 'https://api.anthropic.com/v1',
            'sub-grok': 'https://api.x.ai/v1',
            'sub-antigravity': 'https://cloudcode-pa.googleapis.com',
            'sub-devin': 'https://api.devin.ai/v1',
            'sub-meta': 'https://api.meta.ai/v1',
        }.get(pid, 'https://placeholder.invalid/v1')
        l = ['# 订阅反代 —— %s（OAuth %s）' % (name, method2),
             '# 一手来源：旧版 catalog platforms/' + pid,
             '# 登录：v2 登录引擎（设备码/PKCE）后续接入；当前可手动粘贴 token 走 @sub-* 路由']
        l.append('id = "%s"' % pid)
        l.append('name = "%s"' % name.replace('"', '\\"'))
        l.append('enabled = true')
        l.append('')
        l.append('auth = "api_key"')
        l.append('base_url = "%s"' % base)
        l.append('format = "openai"')
        l.append('')
        l.append('models = []')
        l.append('models_policy = "live"')
        l.append('stream_dialect = "sse_delta"')
        l.append('')
        l.append('notes = "%s"' % esc(hint).replace('"', '\\"').replace('\n', ' ')[:400])
        return '\n'.join(l)
    # cookie / file / qrcode / loomy / anon 等普通平台
    if method == 'cookie':
        auth = 'cookie'
        base = 'https://' + upstream if upstream else 'https://placeholder.invalid'
        policy = 'none'
    elif method == 'anon':
        auth = 'anon'
        base = 'https://' + upstream if upstream else ''
        policy = 'live'
    elif method == 'loomy':
        auth = 'api_key'
        base = 'https://' + upstream if upstream else 'https://loomyad.xunfei.cn'
        policy = 'live'
    elif method == 'qrcode':
        # v2 不做扫码：qrcode 平台改 api_key（手动粘贴 token/cookie）
        auth = 'api_key'
        base = 'https://' + upstream if upstream else ''
        policy = 'live'
    else:  # file
        if typ == 'api_key':
            auth = 'api_key'
        else:
            auth = 'api_key'  # token 类凭据
        base = 'https://' + upstream if upstream else ''
        policy = 'live'
    # 特殊 base 修正
    if pid == 'apk-go':
        base = 'https://copilot.tencent.com'
    elif pid == 'apk-coze':
        base = 'https://api.coze.cn'
    elif pid == 'web-trae':
        base = 'https://trae-api-cn.mchost.guru'
    elif pid == 'web-deepseek':
        base = 'https://chat.deepseek.com'
    elif pid == 'apk-qoder':
        base = 'https://www.qoder.com'
    elif pid == 'apk-antigravity':
        base = 'https://cloudcode-pa.googleapis.com'
    elif pid == 'wb-gateway-intl':
        base = 'https://www.workbuddy.ai'
    l = ['# %s —— v2 声明式接入' % name,
         '# 一手来源：旧版 catalog platforms/' + pid]
    l.append('id = "%s"' % pid)
    l.append('name = "%s"' % name.replace('"', '\\"'))
    l.append('enabled = true')
    l.append('')
    l.append('auth = "%s"' % auth)
    l.append('base_url = "%s"' % base)
    l.append('format = "openai"')
    l.append('')
    l.append('models = []')
    l.append('models_policy = "%s"' % policy)
    l.append('stream_dialect = "sse_delta"')
    if login_url:
        l.append('')
        l.append('login_url = "%s"' % login_url)
    if hosts:
        l.append('hosts = [%s]' % ', '.join('"%s"' % h for h in hosts))
    l.append('')
    l.append('notes = "%s"' % esc(hint).replace('"', '\\"').replace('\n', ' ')[:400])
    return '\n'.join(l)

ok, fail = [], []
for pid in missing:
    try:
        text = gen(pid)
        with io.open(os.path.join(OUT, pid + '.toml'), 'w', encoding='utf-8', newline='\n') as f:
            f.write(text)
        ok.append(pid)
    except Exception as e:
        fail.append((pid, str(e)))
print('生成成功:', len(ok))
print('失败:', fail)
