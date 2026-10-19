# -*- coding: utf-8 -*-
"""补齐：6 个官方 API（旧版 catalog_sites 官方区遗漏）+ 17 个公益 relay 站。"""
import io, json, os

OUT = r'C:\Users\liang.zhao\Downloads\网关项目\aigw2\providers'
CAT = r'C:\Users\liang.zhao\Downloads\网关项目\aigw-panel-rs\catalog_bundled.json'

with io.open(CAT, encoding='utf-8') as f:
    cat = json.load(f)
sites = cat['catalog_sites']

def write(pid, name, auth, base, policy, models, login_url, hosts, notes):
    p = os.path.join(OUT, pid + '.toml')
    if os.path.exists(p):
        print('跳过已存在:', pid)
        return
    l = ['# %s —— v2 声明式接入' % name,
         '# 一手来源：旧版 catalog_sites（官方区/公益 relay）']
    l += ['id = "%s"' % pid,
          'name = "%s"' % name.replace('"', '\\"'),
          'enabled = true',
          '',
          'auth = "%s"' % auth,
          'base_url = "%s"' % base,
          'format = "openai"',
          '',
          'models = [%s]' % (', '.join('"%s"' % m for m in models) if models else ''),
          'models_policy = "%s"' % policy,
          'stream_dialect = "sse_delta"']
    if login_url:
        l += ['', 'login_url = "%s"' % login_url]
    if hosts:
        l += ['hosts = [%s]' % ', '.join('"%s"' % h for h in hosts)]
    if notes:
        l += ['', 'notes = "%s"' % notes.replace('"', '\\"').replace('\n', ' ')[:300]]
    with io.open(p, 'w', encoding='utf-8', newline='\n') as f:
        f.write('\n'.join(l) + '\n')
    print('生成:', pid)

# ===== 6 个官方 API（此前遗漏，能加的都加）=====
official = [
    ('api-gemini', 'Google Gemini（AI Studio）', 'api_key', 'https://generativelanguage.googleapis.com/v1beta', 'live', [],
     'https://aistudio.google.com/apikey', [], 'Gemini 免费层 15 RPM/1500 RPD。key 在 AI Studio 创建，Bearer 直连。'),
    ('api-nvidia', 'NVIDIA NIM', 'api_key', 'https://integrate.api.nvidia.com/v1', 'live', [],
     'https://build.nvidia.com', [], '100+ 模型，免费层 ~40 RPM。key 在 build.nvidia.com 创建。'),
    ('api-cloudflare', 'Cloudflare Workers AI', 'api_key', 'https://api.cloudflare.com/client/v4/accounts/self/ai/v1', 'live', [],
     'https://developers.cloudflare.com/workers-ai/', [], '免费层每天 10,000 Neurons。key 在 Cloudflare 控制台创建。'),
    ('api-huggingface', 'HuggingFace Inference', 'api_key', 'https://router.huggingface.co/v1', 'live', [],
     'https://huggingface.co/settings/tokens', [], '每月 $0.10 credits；Router 走 OpenAI 兼容。'),
    ('api-cerebras', 'Cerebras', 'api_key', 'https://api.cerebras.ai/v1', 'live', [],
     'https://cloud.cerebras.ai', [], '$5 试用额度（需绑支付方式）；Llama/Mistral 高速推理。'),
    ('api-longcat', '美团 LongCat', 'api_key', 'https://longcat.chat/v1', 'live', [],
     'https://longcat.chat/platform', [], 'LongCat-2.5-Preview 资源包 + 活动赠送额度。'),
]
for pid, name, auth, base, policy, models, login, hosts, notes in official:
    write(pid, name, auth, base, policy, models, login, hosts, notes)

# ===== 17 个公益 relay 站（category=relay，排除 official）=====
relay = [s for s in sites if s.get('category') == 'relay']
print('公益 relay 站数:', len(relay))
for s in relay:
    pid = 'free-' + s['name'].split()[0].lower().replace('.', '-')
    pid = ''.join(c for c in pid if c.isalnum() or c == '-')
    write(pid, s['name'] + '（公益）', 'api_key', s.get('api_url', s['url'] + '/v1'), 'live', [],
           s.get('signup', s['url']), [], s.get('bonus', '') + '；' + s.get('models', '') + '；' + s.get('limit', ''))
