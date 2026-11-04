# -*- coding: utf-8 -*-
import io, os, re
d = r'C:\Users\liang.zhao\Downloads\网关项目\aigw2\providers'
fix = {'free-berf.toml':'free-berf','free-cmapi.toml':'free-cmapi','free-kemeng.toml':'free-kemeng','free-kuaipao.toml':'free-kuaipao','free-naixi.toml':'free-naixi','free-newapi.toml':'free-newapi','free-nuomiaomiao.toml':'free-nuomiaomiao','free-pmapi.toml':'free-pmapi','free-vsllm.toml':'free-vsllm'}
for fn, nid in fix.items():
    p = os.path.join(d, fn)
    with io.open(p, encoding='utf-8') as f:
        c = f.read()
    c2 = re.sub(r'^id = .*$', 'id = "' + nid + '"', c, count=1, flags=re.M)
    with io.open(p, 'w', encoding='utf-8', newline='\n') as f:
        f.write(c2)
    print('fix', fn)
