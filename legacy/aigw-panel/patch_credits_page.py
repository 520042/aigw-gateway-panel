import io

p = 'app.js'
s = io.open(p, encoding='utf-8').read()

# 1) loadCredits：CR_ALL 时向后端要全量（S5 分页）
old = """async function loadCredits(force){
  // accounts 顺手拉：urgency_* 字段由 /api/credits 写回 accounts.json，
  // 这里读出来做「选号优先序」可视化（请求路径就是按这个顺序挑凭据的）。
  const [r,ac]=await Promise.all([
    api('api/credits'+(force?'?refresh=1':'')).catch(()=>({})),
    api('api/accounts').catch(()=>({accounts:[]})),
  ]);
  S.data.credits=r;
  S.data.accounts=ac.accounts||[];
  render();
}"""
new = """async function loadCredits(force){
  // accounts 顺手拉：urgency_* 字段由 /api/credits 写回 accounts.json，
  // 这里读出来做「选号优先序」可视化（请求路径就是按这个顺序挑凭据的）。
  // ★ S5：后端 rows 默认只返前 30 条（响应体积治理）；展开全部时才要全量。
  const q=(force?'?refresh=1':'?')+(CR_ALL?'&all=1':'');
  const [r,ac]=await Promise.all([
    api('api/credits'+q).catch(()=>({})),
    api('api/accounts').catch(()=>({accounts:[]})),
  ]);
  S.data.credits=r;
  S.data.accounts=ac.accounts||[];
  render();
}"""
assert old in s, 'loadCredits'
s = s.replace(old, new, 1)

# 2) CR_ALL 切换：从「本地过滤」改成「重新拉全量」（服务端分页了）
old2 = """    +'<button class="btn sm" onclick="CR_ALL=!CR_ALL;render()">'+(CR_ALL?'只看有余额':'显示全部 '+rows.length+' 个包')+'</button>'"""
new2 = """    +'<button class="btn sm" onclick="CR_ALL=!CR_ALL;loadCredits()">'
      +(CR_ALL?'只看有余额':('显示全部 '+(c.rows_total||rows.length)+' 个包'))+'</button>'"""
assert old2 in s, 'CR_ALL btn'
s = s.replace(old2, new2, 1)

# 3) 明细标题旁显示「已截断」提示
old3 = """  // ④ 账号选号优先序（urgency_rank 实际生效顺序）"""
new3 = """  // ③.5 分页提示：后端默认只返前 30 条明细（汇总口径不受影响）
  if(c.rows_truncated && !CR_ALL){
    h+='<div class="note" style="margin-top:10px">明细默认只显示前 '
      +(c.rows_shown||rows.length)+' 条（共 '+(c.rows_total||0)+' 条资源包）；'
      +'汇总数字（总余额 / 风险积分 / 分布）始终是全量口径。'
      +'<button class="btn sm gh" onclick="CR_ALL=true;loadCredits()">展开全部</button></div>';
  }

  // ④ 账号选号优先序（urgency_rank 实际生效顺序）"""
assert old3 in s, 'truncated hint'
s = s.replace(old3, new3, 1)

# 4) 空状态里的「显示全部」也走重新拉取
old4 = """      +(CR_ALL?'':'<button class="btn sm gh" onclick="CR_ALL=true;render()">显示全部</button>')+'</div>';"""
new4 = """      +(CR_ALL?'':'<button class="btn sm gh" onclick="CR_ALL=true;loadCredits()">显示全部</button>')+'</div>';"""
assert old4 in s, 'empty state'
s = s.replace(old4, new4, 1)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('app.js credits pagination ok')

# 5) bump 版本
p2 = 'index.html'
s2 = io.open(p2, encoding='utf-8').read()
assert '?v=20261016c' in s2
s2 = s2.replace('?v=20261016c', '?v=20261016d')
io.open(p2, 'w', encoding='utf-8', newline='\n').write(s2)
print('bumped to 20261016d')
