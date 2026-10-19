# -*- coding: utf-8 -*-
"""模拟 aigw-registry validate 规则，逐个校验 providers/*.toml，定位非法文件。"""
import io, os, re, sys

DIR = r'C:\Users\liang.zhao\Downloads\网关项目\aigw2\providers'
VALID_AUTH = {'api_key', 'cookie', 'anon', 'token', 'none', 'oauth_device', 'oauth_pkce', 'oauth_google', 'oauth_lobster', 'loomy'}
VALID_FORMAT = {'openai', 'anthropic', 'responses'}
VALID_POLICY = {'live', 'static', 'none'}
VALID_DIALECT = {'sse_delta', 'event_machine', 'anthropic_sse', 'responses'}
VALID_KIND = {'openai', 'chatglm'}
VALID_SIGNER = {'', 'glm'}
VALID_FILTER = {'', 'isFree'}

def parse_toml_simple(raw):
    """极简 toml 解析：只提取顶层键 = 值 和 [section] 下的键。够用于校验。"""
    data = {}
    section = None
    for line in raw.splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        if line.startswith('['):
            section = line.strip('[]').strip()
            data.setdefault(section, {})
            continue
        if '=' in line:
            k, _, v = line.partition('=')
            k = k.strip(); v = v.strip()
            if section:
                data[section][k] = v
            else:
                data[k] = v
    return data

def clean(s):
    return s.strip().strip('"').strip("'")

bad = []
ok = 0
for fn in sorted(os.listdir(DIR)):
    if not fn.endswith('.toml'):
        continue
    pid0 = fn[:-5]
    p = os.path.join(DIR, fn)
    with io.open(p, encoding='utf-8') as f:
        raw = f.read()
    d = parse_toml_simple(raw)
    errs = []
    pid = clean(d.get('id', ''))
    if pid != pid0:
        errs.append(f'id({pid}) != 文件名({pid0})')
    if not re.match(r'^[a-z0-9\-_]{1,64}$', pid):
        errs.append(f'id 非法: {pid!r}')
    name = clean(d.get('name', ''))
    if not name:
        errs.append('name 空')
    auth = clean(d.get('auth', ''))
    if auth not in VALID_AUTH:
        errs.append(f'auth 非法: {auth}')
    base = clean(d.get('base_url', ''))
    if not (base.startswith('http://') or base.startswith('https://') or base.startswith('@')):
        errs.append(f'base_url 前缀非法: {base!r}')
    if base.startswith('http') and base.endswith('/'):
        errs.append(f'base_url 尾斜杠: {base}')
    fmt = clean(d.get('format', ''))
    if fmt not in VALID_FORMAT:
        errs.append(f'format 非法: {fmt}')
    pol = clean(d.get('models_policy', ''))
    if pol not in VALID_POLICY:
        errs.append(f'models_policy 非法: {pol}')
    dialect = clean(d.get('stream_dialect', ''))
    if dialect not in VALID_DIALECT:
        errs.append(f'stream_dialect 非法: {dialect}')
    kind = clean(d.get('upstream_kind', 'openai'))
    if kind not in VALID_KIND:
        errs.append(f'upstream_kind 非法: {kind}')
    signer = clean(d.get('signer', ''))
    if signer not in VALID_SIGNER:
        errs.append(f'signer 非法: {signer}')
    if kind == 'chatglm' and signer != 'glm':
        errs.append('chatglm 要求 signer=glm')
    flt = clean(d.get('models_filter', ''))
    if flt not in VALID_FILTER:
        errs.append(f'models_filter 非法: {flt}')
    models = d.get('models', '[]').strip()
    if pol == 'static' and models in ('[]', ''):
        errs.append('static 策略 models 为空')
    if errs:
        bad.append((pid0, errs))
    else:
        ok += 1

print(f'OK: {ok} / BAD: {len(bad)}')
for pid0, errs in bad:
    print(f'== {pid0}:')
    for e in errs:
        print('   -', e)
