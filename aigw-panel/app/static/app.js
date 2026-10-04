/* AI 资源整合网关面板 —— 前端逻辑 */
'use strict';

const S = { v:'dash', data:{}, busy:false, timer:null, loaded:'' };

// ---------------------------------------------------------------- 工具
function $(s,r){return (r||document).querySelector(s);}
function $$(s,r){return Array.from((r||document).querySelectorAll(s));}
function esc(x){return String(x==null?'':x).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
function nfmt(n){
  if(n==null||isNaN(n))return '—';
  n=Number(n);
  if(Math.abs(n)>=1e8)return (n/1e8).toFixed(2)+'亿';
  if(Math.abs(n)>=1e4)return (n/1e4).toFixed(2)+'万';
  return n.toLocaleString();
}
function nraw(n){return n==null?'—':Number(n).toLocaleString();}
function dur(s){
  s=Math.floor(s||0);
  const d=Math.floor(s/86400),h=Math.floor(s%86400/3600),m=Math.floor(s%3600/60);
  if(d)return d+'天'+(h?h+'小时':'');
  if(h)return h+'小时'+(m?m+'分':'');
  return m+'分';
}
function ago(ts){
  if(!ts)return '—';
  const d=Math.floor(Date.now()/1000-ts);
  return d<0?new Date(ts*1000).toLocaleString():dur(d)+'前';
}
function toast(msg,kind){
  const t=document.createElement('div');
  t.className='toast'+(kind?' '+kind:'');
  t.textContent=msg;
  document.body.appendChild(t);
  setTimeout(()=>t.remove(),4200);
}
async function api(path,body){
  const opt={method:body?'POST':'GET',headers:{'Content-Type':'application/json'}};
  if(body)opt.body=JSON.stringify(body);
  const r=await fetch(path,opt);
  const j=await r.json().catch(()=>({ok:false,message:'HTTP '+r.status}));
  return j;
}
function tag(t,c){return '<span class="tag '+(c||'')+'">'+esc(t)+'</span>';}
function bar(pct,cls){
  pct=Math.max(0,Math.min(100,pct||0));
  return '<div class="bar"><i class="'+(cls||'')+'" style="width:'+pct.toFixed(1)+'%"></i></div>';
}
function empty(t){return '<div class="empty">'+esc(t)+'</div>';}

// ---------------------------------------------------------------- 数据表格
// 统一表格组件：搜索 + 排序 + 分页。
// 老代码里到处是 .scroll + table，长列表没有搜索也没有分页，数据一多就没法看。
const DT = {};                       // 每个表一个状态，key 自取
function dt(key, opts){
  opts = opts || {};
  const st = DT[key] = DT[key] || {q:'', sort:opts.sort||'', dir:opts.dir||-1, page:1, size:opts.size||25};
  return {key:key, st:st, opts:opts};
}

/**
 * 渲染一张带工具条的表。
 * cols: [{k:'id', t:'模型 ID', cls:'', w:'', num:true}]
 * rows: 数据数组
 * o:    {key, cols, rows, pageSize, searchKeys, empty}
 */
function dataTable(o){
  const key=o.key, cols=o.cols||[], all=o.rows||[];
  const st=DT[key]||(DT[key]={q:'',sort:o.sort||'',dir:o.dir||-1,page:1,size:o.size||25});
  const q=(st.q||'').trim().toLowerCase();
  const keys=o.searchKeys||cols.map(c=>c.k);

  // 过滤
  let rows=all;
  if(q){
    rows=rows.filter(r=>keys.some(k=>{
      const v=r[k];
      return v!==null&&v!==undefined&&String(v).toLowerCase().indexOf(q)>=0;
    }));
  }
  // 排序
  if(st.sort){
    const c=cols.find(x=>x.k===st.sort);
    if(c){
      const dir=st.dir;
      rows=rows.slice().sort((a,b)=>{
        let x=a[st.sort], y=b[st.sort];
        if(c.num){x=Number(x)||0; y=Number(y)||0; return (x-y)*dir;}
        x=String(x==null?'':x); y=String(y==null?'':y);
        return x.localeCompare(y,'zh-CN')*dir;
      });
    }
  }
  // 分页
  // size=0 表示「不分页、显示全部」。判断必须用 ==null，
  // 不能用 ||（0 是 falsy，会被吞成默认 25 —— 「显示全部」就永远选不中）
  const size=(st.size==null)?(o.size||25):st.size;
  const total=rows.length;
  const pages=Math.max(1,Math.ceil(total/size));
  if(st.page>pages)st.page=pages;
  const view=size>0?rows.slice((st.page-1)*size,st.page*size):rows;

  // 工具条
  let h='<div class="dt"><div class="dtbar">'
    +'<input class="grow" id="dtq_'+esc(key)+'" placeholder="'+
      esc(o.searchHint||'搜索…（按模型名 / 平台 / 状态过滤）')+'" value="'+esc(st.q||'')+'">';
  if(o.extraTools)h+=o.extraTools;
  h+='<select id="dts_'+esc(key)+'" style="width:auto">'
    +[10,25,50,100,0].map(n=>'<option value="'+n+'"'+
      (n===size?' selected':'')+'>'+(n===0?'全部':n+' 条/页')+'</option>').join('')
    +'</select>';
  h+='<span class="cnt">'+total+' 条'+(total!==all.length?'（共 '+all.length+'）':'')
    +(q?' · 匹配「'+esc(st.q)+'」':'')+'</span></div>';

  // 表头
  h+='<div class="dtw"><table><thead><tr>';
  for(const c of cols){
    const on=st.sort===c.k;
    h+='<th class="sortable'+(on?' on':'')+(c.cls?' '+c.cls:'')+'"'
      +' data-dtk="'+esc(key)+'" data-dtc="'+esc(c.k)+'"'
      +(c.w?' style="width:'+c.w+'"':'')+'>'+esc(c.t)
      +'<span class="ar">'+(on?(st.dir<0?'▼':'▲'):'◆')+'</span></th>';
  }
  h+='</tr></thead><tbody>';
  if(!view.length){
    h+='<tr><td colspan="'+cols.length+'">'+empty(q?'没有匹配「'+esc(st.q)+'」的记录':'暂无数据')+'</td></tr>';
  }else{
    for(const r of view){
      h+='<tr>';
      for(const c of cols){
        h+='<td'+(c.cls?' class="'+c.cls+'"':'')+'>'
          +(c.render?c.render(r):esc(r[c.k]==null?'—':r[c.k]))+'</td>';
      }
      h+='</tr>';
    }
  }
  h+='</tbody></table></div>';

  // 分页条
  if(size>0&&pages>1){
    h+='<div class="dtfoot">'
      +'<button class="btn sm" data-dtp="'+esc(key)+'" data-dtd="1"'+(st.page<=1?' disabled':'')+'>« 首页</button>'
      +'<button class="btn sm" data-dtp="'+esc(key)+'" data-dtd="'+(st.page-1)+'"'+(st.page<=1?' disabled':'')+'>‹ 上一页</button>'
      +'<span>第 <b>'+st.page+'</b> / '+pages+' 页</span>'
      +'<button class="btn sm" data-dtp="'+esc(key)+'" data-dtd="'+(st.page+1)+'"'+(st.page>=pages?' disabled':'')+'>下一页 ›</button>'
      +'<button class="btn sm" data-dtp="'+esc(key)+'" data-dtd="'+pages+'"'+(st.page>=pages?' disabled':'')+'>末页 »</button>'
      +'<span class="sp"></span>'
      +'<span class="faint">'+(st.page-1)*size+1+'-'+Math.min(st.page*size,total)+' / '+total+'</span>'
      +'</div>';
  }
  h+='</div>';
  return h;
}

/* 表格交互（事件委托，一次绑定全局有效） */
function dtRefresh(){ render(); }
function dtBindOnce(){
  if(dtBindOnce._done)return;
  dtBindOnce._done=true;
  document.addEventListener('click',e=>{
    const th=e.target.closest('th.sortable');
    if(th){
      const st=DT[th.dataset.dtk];
      if(st){
        if(st.sort===th.dataset.dtc)st.dir=-st.dir;
        else{st.sort=th.dataset.dtc;st.dir=-1;}
        st.page=1; dtRefresh();
      }
      return;
    }
    const pg=e.target.closest('[data-dtp]');
    if(pg&&!pg.disabled){
      const st=DT[pg.dataset.dtp];
      if(st){st.page=parseInt(pg.dataset.dtd,10)||1; dtRefresh();}
    }
  });
  document.addEventListener('input',e=>{
    const q=e.target.closest('[id^="dtq_"]');
    if(!q)return;
    const key=q.id.slice(4);
    if(DT[key]){DT[key].q=q.value;DT[key].page=1; dtRefresh();
      // 重绘后焦点丢了，补回来
      const el=document.getElementById('dtq_'+key);
      if(el){el.focus();
        const n=el.value.length;try{el.setSelectionRange(n,n);}catch(_){}}
    }
  });
  document.addEventListener('change',e=>{
    const s=e.target.closest('[id^="dts_"]');
    if(!s)return;
    const key=s.id.slice(4);
    if(DT[key]){DT[key].size=parseInt(s.value,10)||25;DT[key].page=1;dtRefresh();}
  });
}

// ---------------------------------------------------------------- 路由
const TITLES={dash:'总览',gateway:'网关与账号',login:'账号登录',toolcall:'工具调用',autocheckin:'自动签到',checkin:'签到中心',growth:'成长任务',
  tasks:'任务中心',usage:'用量看板',toolcall:'工具调用',models:'模型与价格',sites:'资源站点',
  catalog:'免费资源导航',route:'智能路由',upstreams:'上游档案',
  notify:'通知中心',logs:'日志',settings:'设置'};

document.addEventListener('click',e=>{
  const b=e.target.closest('.navbtn');
  if(!b)return;
  $$('.navbtn').forEach(x=>x.classList.remove('on'));
  b.classList.add('on');
  switchView(b.dataset.v);
});

async function refresh(){
  if(S.busy)return;
  S.busy=true;
  try{
    const o=await api('/api/overview');
    S.data.overview=o;
    $('#stl').textContent='网关 '+(o.gateway&&o.gateway.alive?'运行中':'未运行')+
      ' · '+o.gateway.addr+' · 面板已运行 '+dur(o.uptime)+' · 调度 '+((o.scheduler&&o.scheduler.ticks)||0)+' 次';
    render();
  }catch(e){toast('加载失败：'+e.message,'err');}
  S.busy=false;
}

/* ---------------------------------------------------------------- 主题 */
function applyTheme(t){
  document.documentElement.setAttribute('data-theme', t);
  try{ localStorage.setItem('aigw.theme', t); }catch(e){}
  const b=document.getElementById('themeBtn');
  if(b)b.textContent = t==='dark' ? '☀ 亮色' : '◐ 主题';
}
function toggleTheme(){
  const cur=document.documentElement.getAttribute('data-theme')==='dark'?'light':'dark';
  applyTheme(cur);
  toast(cur==='dark'?'已切换到暗色':'已切换到亮色');
}

/* ---------------------------------------------------------------- 渲染内核
 *
 * 重要：数据加载与重绘必须分开。
 * 旧实现是 render() 里调 loadExtra() → loader 末尾又调 render()，
 * 14 个 loader 全都这样 → 无限递归，页面永远停在「加载中…」
 * （用户反馈「点开始登录没反应」就是它，签到/用量/路由同理）。
 * 现在改成：render() 只负责画，switchView() 负责加载，且同一视图不重复加载。
 */
const LOADABLE = {};          // 哪些视图需要 loader（LOADERS 里有就是需要）

/* 页头「立即签到」按钮只在签到相关的页面出现。
 * 之前它写死在 index.html 的 .hd 里，导致每个子界面都顶着一个签到按钮，
 * 跟页面内容毫无关系（用户反馈）。 */
const CHECKIN_VIEWS = ['dash', 'checkin', 'autocheckin'];

function render(){
  const f=VIEWS[S.v]||VIEWS.dash;
  $('#view').innerHTML=f();
  if(S.v==='login')paintLoginSession();
  const hb=$('#hdCheckin');
  if(hb)hb.classList.toggle('hidden', CHECKIN_VIEWS.indexOf(S.v)<0);
}
async function switchView(v,force){
  S.v=v;
  location.hash=v;
  $('#ttl').textContent=TITLES[v]||v;
  render();
  const loader=LOADERS[v];
  if(!loader)return;
  if(!force && S.loaded===v)return;   // 同一视图不重复拉
  S.loaded=v;
  try{
    await loader();
  }catch(e){
    toast('加载失败：'+e.message,'err');
  }
}

// ---------------------------------------------------------------- 总览
function viewDash(){
  const o=S.data.overview||{};
  const g=o.gateway||{},c=o.checkin||{},t=o.tasks||{},u=o.usage||{};
  const pct=c.total_checkin_sites?Math.round(c.done_today/c.total_checkin_sites*100):0;
  return `
  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">网关状态</div>
      <div class="vl" style="color:${g.alive?'var(--ok)':'var(--err)'}">${g.alive?'在线':'离线'}</div>
      <div class="ex">${esc(g.addr||'')} · ${g.models||0} 个模型</div></div>
    <div class="kpi"><div class="lb">今日签到</div>
      <div class="vl">${c.done_today||0} / ${c.total_checkin_sites||0}</div>
      <div class="ex">最长连续 ${c.best_streak||0} 天 · 累计 ${c.total_history||0} 次</div></div>
    <div class="kpi"><div class="lb">今日请求</div>
      <div class="vl">${nfmt(u.requests)}</div>
      <div class="ex">成功 ${nfmt(u.success)} · 失败 ${nfmt(u.failed)}</div></div>
    <div class="kpi"><div class="lb">累计 Token</div>
      <div class="vl">${nfmt(u.tokensTotal)}</div>
      <div class="ex">入 ${nfmt(u.tokensIn)} · 出 ${nfmt(u.tokensOut)}</div></div>
  </div>

  <div class="grid g2">
    <div class="card">
      <h2>签到进度</h2>
      ${bar(pct,'ok')}
      <div class="flex mt" style="justify-content:space-between">
        <span class="muted">${esc(c.date||'')} · 完成 ${pct}%</span>
        <button class="btn pri sm" onclick="quickCheckin()">全部签到</button>
      </div>
      <div class="mt">
        <table><thead><tr><th>站点</th><th>连续天数</th><th>状态</th></tr></thead><tbody>
        ${Object.entries(c.streaks||{}).map(([k,v])=>{
          const site=(S.data.sites||[]).find(x=>x.id===k);
          return `<tr><td>${esc(site?site.name:k)}</td><td>${v} 天</td>
            <td>${tag(v?'已连续签到':'今日未签',v?'ok':'warn')}</td></tr>`;
        }).join('')||'<tr><td colspan="3">'+empty('尚未配置签到站点')+'</td></tr>'}
        </tbody></table>
      </div>
    </div>

    <div class="card">
      <h2>任务与额度</h2>
      <table><tbody>
        <tr><td>任务总数</td><td class="right">${t.total||0}</td>
            <td>已完成 ${t.done||0} / 待办 ${t.pending||0}</td></tr>
        <tr><td>免费调用</td><td class="right">${nfmt(u.freeCalls)}</td>
            <td>付费调用 ${nfmt(u.paidCalls)}</td></tr>
        <tr><td>消耗积分</td><td class="right">${nraw(u.credit)}</td>
            <td>计费次数 ${nfmt(u.creditCount)}</td></tr>
        <tr><td>覆盖模型</td><td class="right">${nfmt(u.models)}</td>
            <td>覆盖账号 ${nfmt(u.accounts)}</td></tr>
        <tr><td>用量保留</td><td class="right">90 天</td><td>调度器 ${(o.scheduler&&o.scheduler.ticks)||0} 次心跳</td></tr>
        <tr><td>上次签到</td><td class="right">${esc((o.scheduler&&o.scheduler.last_checkin_date)||'—')}</td>
            <td>上次成长 ${esc((o.scheduler&&o.scheduler.last_growth_date)||'—')}</td></tr>
      </tbody></table>
      <div class="flex mt">
        <button class="btn sm" onclick="go('growth')">执行成长任务</button>
        <button class="btn sm" onclick="go('usage')">查看看量</button>
        <button class="btn sm" onclick="go('sites')">管理站点</button>
      </div>
    </div>
  </div>

  <div class="card">
    <h2>面板能力</h2>
    <div class="grid g3">
      ${[['✓','每日签到','网关账号池 + 外部中转站统一签到，支持补签'],
         ['✦','成长任务','领奖励 / 补签 / 抽奖 / 旅行 四步自动执行'],
         ['◔','用量看板','Token、请求数、免费付费分布、24 小时曲线'],
         ['⌗','模型价格','模型清单 + 免费/收费属性探测'],
         ['☷','站点管理','批量探测健康度、余额、签到状态'],
         ['✉','通知推送','企微/钉钉/飞书/PushPlus/Server酱/Bark'],
         ['≡','日志审计','网关请求日志 + 面板运行日志'],
         ['▣','账号池','多凭据轮询、429 冷却、开机自启'],
         ['★','资源导航','公益站 + 官方免费额度平台清单']]
        .map(([i,t,d])=>`<div><div class="flex" style="gap:7px"><b>${i}</b><b>${t}</b></div>
          <div class="muted" style="font-size:12.5px;margin-left:25px">${d}</div></div>`).join('')}
    </div>
  </div>`;
}

function go(v){
  const b=$(`.navbtn[data-v="${v}"]`);
  if(b)b.click();
}

// ---------------------------------------------------------------- 网关
async function loadGateway(){
  const r=await api('/api/settings');
  S.data.settings=r;
  const c=await api('/api/credentials');
  S.data.creds=c;
  render();
}
function viewGateway(){
  const r=S.data.settings||{},g=r.gateway||{};
  const cr=S.data.creds||{};
  const files=cr.files||[];
  return `
  ${g.need_setup?'':''}
  <div class="grid g2">
    <div class="card">
      <h2>网关程序</h2>
      <table><tbody>
        <tr><td>可执行文件</td><td class="right mono">${esc(r.gateway_path||'未找到')}</td></tr>
        <tr><td>监听地址</td><td class="right mono">${esc(g.listen||(g.addr+':'+g.port))}</td></tr>
        <tr><td>控制台账号</td><td class="right">${esc(g.adminUser||'未设置')} / ${esc(g.adminPasswordMasked||'未设置')}</td></tr>
        <tr><td>API 鉴权</td><td class="right">${g.poolKeyEnabled?g.poolKeyCount+' 个密钥':'未启用（Bearer admin）'}</td></tr>
        <tr><td>账号池</td><td class="right">${files.length} 个凭据文件</td></tr>
        <tr><td>每日签到</td><td class="right">${g.checkinHour||9}:00</td></tr>
        <tr><td>成长任务</td><td class="right">${g.growthEnabled?'启用':'停用'} · ${g.growthHour||10}:00</td></tr>
        <tr><td>冷却策略</td><td class="right">账号 ${g.accountCooldownSeconds||60}s / 模型 ${g.modelCooldownSeconds||600}s</td></tr>
      </tbody></table>
      <div class="flex mt">
        <button class="btn pri" onclick="post('/api/settings?action=gateway_restart',{})">启动网关</button>
        <button class="btn dgr" onclick="post('/api/settings?action=gateway_stop',{})">停止网关</button>
        <button class="btn" onclick="loadGateway()">刷新</button>
      </div>
    </div>

    <div class="card">
      <h2>控制台登录</h2>
      <div class="note">网关 v1.29.6 首次使用需初始化控制台账号，之后用同一账号密码登录本面板。</div>
      <div class="row">
        <div><label>用户名</label><input id="gwUser" value="${esc((S.data.settings&&S.data.settings.settings&&S.data.settings.settings.gateway_admin_user)||'admin')}"></div>
        <div><label>密码</label><input id="gwPass" type="password" placeholder="首次初始化时填写"></div>
      </div>
      <div class="flex mt">
        <button class="btn pri" onclick="gwSetup()">初始化控制台</button>
        <button class="btn" onclick="gwLogin()">登录并保存</button>
      </div>
      <div class="muted mt" style="font-size:12.5px">
        登录成功后，网关的签到、成长、用量、日志数据会在本面板内直接呈现。
      </div>
    </div>
  </div>

  <div class="card">
    <h2>凭据文件（账号池）</h2>
    <div class="scroll">
      <table><thead><tr><th>文件名</th><th>站点</th><th>操作</th></tr></thead><tbody>
      ${files.length?files.map(f=>`<tr>
        <td class="mono">${esc(f.name||f)}</td>
        <td>${esc((f.site||f.origin||'-'))}</td>
        <td><button class="btn sm dgr" onclick="delCred('${esc(f.name||f)}')">删除</button></td>
      </tr>`).join(''):'<tr><td colspan="3">'+empty('尚无凭据文件。执行 workbuddy-gateway login 登录，或直接放置 workbuddy*.json 到网关目录')+'</td></tr>'}
      </tbody></table>
    </div>
    <div class="muted mt" style="font-size:12.5px">
      网关会每 5 秒热加载目录下的 <code>workbuddy*.json</code>，新增/更新无需重启。
    </div>
  </div>`;
}

async function delCred(name){
  if(!confirm('删除凭据 '+name+' ？该操作会移除本地账号池中的这个账号。'))return;
  const r=await post('/api/credentials?action=delete',{name});
  toast(r.ok?'已删除':'失败：'+r.message,r.ok?'ok':'err');
  loadGateway();
}
async function gwSetup(){
  const u=$('#gwUser').value.trim(),p=$('#gwPass').value;
  if(!u||!p)return toast('用户名和密码必填','err');
  const r=await post('/api/settings?action=gateway_setup',{username:u,password:p});
  toast(r.message||(r.ok?'成功':'失败'),r.ok?'ok':'err');
  loadGateway();
}
async function gwLogin(){
  const u=$('#gwUser').value.trim(),p=$('#gwPass').value;
  const r=await post('/api/settings?action=gateway_login',{username:u,password:p});
  toast(r.message||(r.ok?'成功':'失败'),r.ok?'ok':'err');
  loadGateway();
}

// ---------------------------------------------------------------- 签到
async function loadCheckin(){
  const r=await api('/api/checkins');
  S.data.checkin=r;
  render();
}
function viewCheckin(){
  const r=S.data.checkin||S.data.extra||{};
  const sum=r.summary||{},today=r.today||[];
  const sites=S.data.sites||[];
  const pct=sum.total_checkin_sites?Math.round(sum.done_today/sum.total_checkin_sites*100):0;
  return `
  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">今日进度</div><div class="vl">${sum.done_today||0} / ${sum.total_checkin_sites||0}</div>
      <div class="ex">${esc(sum.date||'')}</div></div>
    <div class="kpi"><div class="lb">最长连续</div><div class="vl">${sum.best_streak||0} 天</div>
      <div class="ex">累计签到 ${sum.total_history||0} 次</div></div>
    <div class="kpi"><div class="lb">今日成功</div><div class="vl">${today.filter(x=>x.ok).length}</div>
      <div class="ex">今日记录 ${today.length} 条</div></div>
    <div class="kpi"><div class="lb">失败</div>
      <div class="vl" style="color:${today.filter(x=>!x.ok).length?'var(--err)':'inherit'}">${today.filter(x=>!x.ok).length}</div>
      <div class="ex">需检查凭据或站点状态</div></div>
  </div>

  <div class="card">
    <h2>签到进度</h2>
    ${bar(pct,'ok')}
    <div class="flex mt" style="justify-content:space-between">
      <span class="muted">${pct}% 完成</span>
      <div class="flex">
        <button class="btn" onclick="checkin(false)">补签未完成</button>
        <button class="btn pri" onclick="checkin(true)">全部重签（force）</button>
      </div>
    </div>
  </div>

  <div class="card">
    <h2>今日记录</h2>
    <div class="scroll sm">
      <table><thead><tr><th>时间</th><th>站点</th><th>结果</th><th>连续</th><th>说明</th></tr></thead><tbody>
      ${today.length?today.map(x=>`<tr>
        <td class="mono nowrap">${esc((x.at||'').slice(11))}</td>
        <td>${esc(x.name||x.site)}</td>
        <td>${tag(x.ok?'成功':'失败',x.ok?'ok':'err')}</td>
        <td>${x.streak!=null?x.streak+' 天':'—'}</td>
        <td class="muted">${esc(x.message||'')}</td></tr>`).join('')
      :'<tr><td colspan="5">'+empty('今日还没有签到记录，点击右上角「立即签到」开始')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>各站点连续签到天数</h2>
    <div class="scroll sm">
      <table><thead><tr><th>站点</th><th>连续天数</th><th>进度</th><th>操作</th></tr></thead><tbody>
      ${sites.filter(s=>s.checkin).map(s=>{
        const d=sum.streaks&&sum.streaks[s.id]||0;
        return `<tr><td>${esc(s.name)}</td><td>${d} 天</td>
          <td style="width:150px">${bar(Math.min(100,d*10),d?'ok':'warn')}</td>
          <td><button class="btn sm" onclick="runOne('${esc(s.id)}')">单独签到</button></td></tr>`;
      }).join('')||'<tr><td colspan="4">'+empty('未配置支持签到的站点，去「资源站点」添加')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>`;
}
async function checkin(force){
  toast(force?'已触发强制签到…':'已触发签到…');
  await api('/api/checkins?trigger=1'+(force?'&force=1':''));
  setTimeout(loadCheckin,3000);
}
async function runOne(id){
  const r=await post('/api/sites?action=run_one',{id,force:true});
  const x=r.result||{};
  toast((x.name||'')+'：'+(x.message||''),x.ok?'ok':'err');
  loadCheckin();
}
function quickCheckin(){
  go('checkin');
  checkin(false);
}

// ---------------------------------------------------------------- 成长任务
async function loadGrowth(){
  const r=await api('/api/growth');
  S.data.growth=r;
  render();
}
function viewGrowth(){
  const g=(S.data.growth&&S.data.growth.growth)||{};
  const enabled=g.enabled;
  return `
  <div class="grid g2">
    <div class="card">
      <h2>成长任务体系</h2>
      <div class="note ${enabled?'':'warn'}">
        网关成长任务当前${enabled?'已启用':'未启用'}，每日 ${g.hour||10}:00 自动执行，共处理 ${g.scopeAccountNum||0} 个账号，
        上报前 ${g.reportCount||5} 条结果。
      </div>
      <table><tbody>
        <tr><td>领取成长奖励</td><td class="right mono">/admin/api/growth/bonus</td></tr>
        <tr><td>补签</td><td class="right mono">/admin/api/growth/makeup</td></tr>
        <tr><td>抽奖</td><td class="right mono">/admin/api/growth/lottery</td></tr>
        <tr><td>旅行</td><td class="right mono">/admin/api/growth/travel</td></tr>
        <tr><td>上报结果</td><td class="right mono">/admin/api/growth/report</td></tr>
        <tr><td>历史记录</td><td class="right">${g.total||0} 条</td></tr>
      </tbody></table>
      <div class="flex mt">
        <button class="btn pri" onclick="runGrowth()">立即执行全部成长任务</button>
        <button class="btn" onclick="loadGrowth()">刷新</button>
      </div>
    </div>

    <div class="card">
      <h2>上游成长接口</h2>
      <div class="muted" style="font-size:12.5px;margin-bottom:10px">
        网关对接的腾讯侧成长体系端点（反解自 base.apk 与 gateway 1.29.6）：
      </div>
      <pre>${esc([
'/activity/growth/buddy/first',
'/activity/growth/buddy/info',
'/activity/growth/buddy/travel/status',
'/activity/growth/buddy/travel/depart',
'/activity/growth/lottery/chances',
'/activity/growth/lottery/draw',
'/activity/growth/heatmap',
'/activity/growth/makeup',
'/activity/growth/streak',
'/activity/growth/redeem',
'/billing/meter/claim',
'/balance/activity/growth/buddy/travel/claim',
'/v2/billing/meter/daily-checkin',
'/v2/billing/meter/get-user-resource'
].join('\n'))}</pre>
    </div>
  </div>

  <div class="card">
    <h2>最近成长记录</h2>
    <div class="scroll sm">
      <table><thead><tr><th>账号</th><th>动作</th><th>状态</th><th>详情</th><th>时间</th></tr></thead><tbody>
      ${(g.results||[]).length?g.results.map(r=>`<tr>
        <td>${esc(r.account||r.name||'-')}</td>
        <td>${esc(r.action||'-')}</td>
        <td>${tag(r.status||'-',r.status==='success'||r.status==='ok'?'ok':'warn')}</td>
        <td class="muted">${esc(r.detail||r.reason||'')}</td>
        <td class="mono nowrap">${esc((r.timestamp&&new Date(r.timestamp*1000).toLocaleString())||'')}</td>
      </tr>`).join(''):'<tr><td colspan="5">'+empty('暂无成长记录。需先登录网关账号（workbuddy-gateway login）并启用成长任务')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>`;
}
async function runGrowth(){
  const r=await api('/api/growth?trigger=1');
  toast(r.message||'已触发');
  setTimeout(loadGrowth,4000);
}

// ---------------------------------------------------------------- 任务
async function loadTasks(){
  const r=await api('/api/tasks');
  S.data.tasks=r;
  render();
}
function viewTasks(){
  const t=S.data.tasks||{};
  const tasks=t.tasks||[];
  const sites=S.data.sites||[];
  return `
  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">任务总数</div><div class="vl">${t.total||0}</div></div>
    <div class="kpi"><div class="lb">已完成</div><div class="vl" style="color:var(--ok)">${t.done||0}</div></div>
    <div class="kpi"><div class="lb">待办</div><div class="vl" style="color:var(--warn)">${t.pending||0}</div></div>
    <div class="kpi"><div class="lb">累计执行</div><div class="vl">${nfmt(tasks.reduce((a,x)=>a+(x.runs||0),0))}</div></div>
  </div>

  <div class="card">
    <h2>新建任务</h2>
    <div class="row">
      <div><label>任务名</label><input id="tName" placeholder="例如：AnyRouter 每日签到"></div>
      <div><label>关联站点（留空=手动任务）</label>
        <select id="tSite"><option value="">— 不关联 —</option>
        ${sites.map(s=>`<option value="${esc(s.id)}">${esc(s.name)}</option>`).join('')}</select></div>
      <div class="n"><label>&nbsp;</label><button class="btn pri" onclick="addTask()">添加</button></div>
    </div>
  </div>

  <div class="card">
    <h2>任务清单</h2>
    <div class="scroll">
      <table><thead><tr><th>状态</th><th>任务</th><th>类型</th><th>执行次数</th><th>上次结果</th><th>操作</th></tr></thead><tbody>
      ${tasks.length?tasks.map(x=>`<tr>
        <td><input type="checkbox" ${x.done?'checked':''} style="width:auto" onchange="tgTask('${esc(x.id)}')"></td>
        <td>${esc(x.name)}</td>
        <td>${tag(x.site?'站点签到':'手动',x.site?'acc':'')}</td>
        <td>${x.runs||0}</td>
        <td class="muted">${x.last_run?esc(x.last_note||'')+' ('+ago(new Date(x.last_run.replace(/-/g,'/')).getTime()/1000)+')':'—'}</td>
        <td class="nowrap">
          <button class="btn sm" onclick="runTask('${esc(x.id)}')">执行</button>
          <button class="btn sm dgr" onclick="delTask('${esc(x.id)}')">删除</button>
        </td></tr>`).join('')
      :'<tr><td colspan="6">'+empty('还没有任务。关联站点的任务会调用真实签到接口')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>`;
}
async function addTask(){
  const n=$('#tName').value.trim(),s=$('#tSite').value;
  if(!n)return toast('任务名必填','err');
  const r=await post('/api/tasks?action=add',{name:n,site:s});
  toast(r.message||'已添加',r.ok?'ok':'err');
  $('#tName').value='';
  loadTasks();
}
async function tgTask(id){await post('/api/tasks?action=toggle',{id});loadTasks();}
async function delTask(id){if(!confirm('删除该任务？'))return;await post('/api/tasks?action=delete',{id});loadTasks();}
async function runTask(id){
  const r=await post('/api/tasks?action=run',{id});
  const x=r.result||{};
  toast(x.message||r.message||'已执行',x.ok?'ok':'err');
  loadTasks();
}

// ---------------------------------------------------------------- 用量
async function loadUsage(){
  const r=await api('/api/usage?range=all');
  S.data.usage=r;
  render();
}
function viewUsage(){
  const r=S.data.usage||{};
  const u=(r.usage&&r.usage.totals)||{};
  const days=((r.usage&&r.usage.days)||[]).slice(-30);
  const rows=((r.usage&&r.usage.rows)||[]).slice(0,200);
  const totReq=u.requests||1;
  return `
  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">总请求</div><div class="vl">${nfmt(u.requests)}</div>
      <div class="ex">成功 ${nfmt(u.success)} · 失败 ${nfmt(u.failed)}</div></div>
    <div class="kpi"><div class="lb">输入 Token</div><div class="vl">${nfmt(u.tokensIn)}</div></div>
    <div class="kpi"><div class="lb">输出 Token</div><div class="vl">${nfmt(u.tokensOut)}</div></div>
    <div class="kpi"><div class="lb">总 Token</div><div class="vl">${nfmt(u.tokensTotal)}</div>
      <div class="ex">消耗积分 ${nraw(u.credit)}</div></div>
  </div>

  <div class="grid g2">
    <div class="card">
      <h2>调用构成</h2>
      <table><tbody>
        <tr><td>免费调用</td><td class="right">${nfmt(u.freeCalls)}</td>
            <td style="width:44%">${bar(totReq?u.freeCalls/totReq*100:0,'ok')}</td></tr>
        <tr><td>付费调用</td><td class="right">${nfmt(u.paidCalls)}</td>
            <td>${bar(totReq?u.paidCalls/totReq*100:0,'warn')}</td></tr>
        <tr><td>成功率</td><td class="right">${u.requests?((u.success/u.requests)*100).toFixed(1):'—'}%</td>
            <td>${bar(u.requests?u.success/u.requests*100:0)}</td></tr>
      </tbody></table>
    </div>
    <div class="card">
      <h2>24 小时曲线</h2>
      ${chart(r.series&&r.series.points)}
      <div class="legend">
        <span><i style="background:#2f6fed"></i>请求数</span>
        <span><i style="background:#0f9d58"></i>Token 总量(千)</span>
        <span><i style="background:#dc2626"></i>失败数</span>
      </div>
    </div>
  </div>

  <div class="card">
    <h2>每日汇总</h2>
    <div class="scroll sm">
      <table><thead><tr><th>日期</th><th>请求</th><th>成功</th><th>失败</th><th>入</th><th>出</th><th>积分</th></tr></thead><tbody>
      ${days.length?days.map(d=>`<tr>
        <td class="mono">${esc(d.date||d.day||'')}</td>
        <td>${nraw(d.requests)}</td><td>${nraw(d.success)}</td>
        <td>${nraw(d.failed)}</td><td>${nraw(d.tokensIn)}</td>
        <td>${nraw(d.tokensOut)}</td><td>${nraw(d.credit)}</td></tr>`).join('')
      :'<tr><td colspan="7">'+empty('暂无用量数据，发起一次对话后即会记录')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>账号维度明细</h2>
    <div class="scroll sm">
      <table><thead><tr><th>账号</th><th>模型</th><th>请求</th><th>Token</th><th>积分</th><th>最后调用</th></tr></thead><tbody>
      ${rows.length?rows.map(x=>`<tr>
        <td class="mono">${esc(x.account||x.credential||'-')}</td>
        <td class="mono">${esc(x.model||'-')}</td>
        <td>${nraw(x.requests)}</td><td>${nraw(x.tokensTotal)}</td>
        <td>${nraw(x.credit)}</td>
        <td class="mono nowrap">${esc(x.lastUsed&&new Date(x.lastUsed*1000).toLocaleString()||'')}</td></tr>`).join('')
      :'<tr><td colspan="6">'+empty('暂无账号维度明细')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>`;
}
function chart(pts){
  if(!pts||!pts.length)return empty('暂无曲线数据');
  const W=760,H=180,P=30;
  const maxR=Math.max(1,...pts.map(p=>p.requests||0));
  const maxT=Math.max(1,...pts.map(p=>(p.tokensTotal||0)/1000));
  const maxF=Math.max(1,...pts.map(p=>p.failed||0));
  const n=pts.length,dx=(W-P*2)/(n-1||1);
  const y=v=>H-P-(v)*((H-P*2));
  const line=(key,max)=>pts.map((p,i)=>(i?'L':'M')+(P+i*dx).toFixed(1)+','+y((p[key]||0)/max*(H-P*2)).toFixed(1)).join(' ');
  const area=(key,max)=>line(key,max)+' L'+(P+(n-1)*dx).toFixed(1)+','+(H-P)+' L'+P+','+(H-P)+' Z';
  return `<svg class="chart" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">
    <defs><linearGradient id="ga" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#2f6fed" stop-opacity=".22"/>
      <stop offset="100%" stop-color="#2f6fed" stop-opacity="0"/></linearGradient></defs>
    ${[0,.25,.5,.75,1].map(t=>`<line x1="${P}" y1="${P+t*(H-P*2)}" x2="${W-P}" y2="${P+t*(H-P*2)}" stroke="#e6e8ec" stroke-width="1"/>`).join('')}
    <path d="${area('requests',maxR)}" fill="url(#ga)"/>
    <path d="${line('requests',maxR)}" fill="none" stroke="#2f6fed" stroke-width="1.8"/>
    <path d="${line('tokensTotal',maxT)}" fill="none" stroke="#0f9d58" stroke-width="1.5"/>
    <path d="${line('failed',maxF)}" fill="none" stroke="#dc2626" stroke-width="1.5"/>
    <text x="4" y="${P+4}" font-size="10" fill="#9aa1ab">${maxR}</text>
    <text x="${W-P}" y="${H-6}" font-size="10" fill="#9aa1ab" text-anchor="end">${esc((pts[0].label||'')+' → '+(pts[n-1].label||''))}</text>
  </svg>`;
}

// ---------------------------------------------------------------- 模型
async function loadModels(){
  const r=await api('/api/models');
  S.data.models=r;
  // 倍率来源清单（下拉框用），以前是硬编码 4 个选项
  try{
    const s=await api('/api/models?action=sources').catch(()=>({sources:[]}));
    S.data.catalogSources=s.sources||[];
  }catch(e){ S.data.catalogSources=[]; }
  render();
}
/* ---------------------------------------------------------------- 模型选择
 * 用户反馈「模型清单里选不到我需要的模型」—— 原来这张表只能看，不能选。
 * 现在每行一个勾选框，支持全选 / 复制 ID / 导出。
 */
const MODEL_SEL = new Set();

function allModelIds(){
  const r=S.data.models||{};
  const enr=r.models_enriched||[];
  const m=(r.admin&&r.admin.models)||[];
  const rows=enr.length?enr:m;
  return rows.map(x=>String(x.id));
}
function selOne(el,id){
  if(el&&el.checked)MODEL_SEL.add(id); else MODEL_SEL.delete(id);
  const c=$('#selCount');
  if(c)c.textContent=MODEL_SEL.size;
}
function selAllModels(on){
  const ids=allModelIds();
  if(on)ids.forEach(i=>MODEL_SEL.add(i));
  else MODEL_SEL.clear();
  render();
}
function selClear(){
  MODEL_SEL.clear();
  render();
}
function selCopyIds(){
  const ids=[...MODEL_SEL];
  if(!ids.length){toast('先勾选要复制的模型','err');return;}
  const txt=ids.join('\n');
  const done=()=>toast('已复制 '+ids.length+' 个模型 ID','ok');
  if(navigator.clipboard&&navigator.clipboard.writeText){
    navigator.clipboard.writeText(txt).then(done).catch(()=>fallbackCopy(txt,done));
  }else fallbackCopy(txt,done);
}
function fallbackCopy(txt,done){
  const ta=document.createElement('textarea');
  ta.value=txt; ta.style.position='fixed'; ta.style.opacity='0';
  document.body.appendChild(ta); ta.select();
  try{document.execCommand('copy');done();}
  catch(e){toast('复制失败，请手动选择','err');}
  ta.remove();
}
function selExport(){
  const ids=[...MODEL_SEL];
  if(!ids.length){toast('先勾选要导出的模型','err');return;}
  const r=S.data.models||{};
  const enr=r.models_enriched||[];
  const byId={};
  for(const x of enr)byId[String(x.id)]=x;
  const out=ids.map(id=>{
    const x=byId[id]||{id:id};
    return {
      id:id, credits:x.credits||null, rate_source:x.rate_source||'',
      max_input_tokens:x.ctx||null, max_output_tokens:x.out||null,
      tools:x.tools, vision:x.vision, reasoning:x.reasoning,
      cn_free:x.cnFree||null, intl_free:x.intlFree||null,
    };
  });
  const blob=new Blob([JSON.stringify(out,null,2)],{type:'application/json'});
  const a=document.createElement('a');
  a.href=URL.createObjectURL(blob);
  a.download='aigw-models-'+ids.length+'.json';
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(()=>URL.revokeObjectURL(a.href),3000);
  toast('已导出 '+ids.length+' 个模型','ok');
}

async function fetchCatalog(){
  const sel=$('#catalogPlatform');
  const pid=sel?sel.value:'apk-codebuddy';
  toast('正在用账号池凭据拉取 '+pid+' 的线上模型倍率…');
  const r=await api('/api/models?action=catalog',{platform:pid}).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'拉取失败','err');S.data.modelCatalog={error:r.message,platform:pid};}
  else{S.data.modelCatalog=r.catalog; if(r.sources)S.data.catalogSources=r.sources;}
  render();
}

function viewModels(){
  const r=S.data.models||{};
  const m=(r.admin&&r.admin.models)||[];
  const v1=((r.v1&&r.v1.data)||[]);
  const enr=r.models_enriched||[];
  const cat=S.data.modelCatalog||null;
  const rows=enr.length?enr:m;
  const sum=r.rate_summary||{};
  const bd=r.bundled||{};
  // 倍率覆盖率条
  const pct=sum.total?Math.round(100*(sum.with_rate||0)/sum.total):0;
  let rateBar='';
  if(sum.total){
    rateBar='<div class="card"><div class="ch"><b>倍率覆盖</b>'
      +'<span class="faint">'+sum.with_rate+' / '+sum.total+'（'+pct+'%）</span></div><div class="cb">'
      +'<div style="height:8px;background:var(--line2);border-radius:5px;overflow:hidden">'
      +'<div style="height:100%;width:'+pct+'%;background:var(--acc);border-radius:5px"></div></div>'
      +'<div class="muted mt" style="font-size:12.5px">'
      +'内置表 '+bd.total+' 个模型（带倍率 '+bd.with_rate+'）'
      +' · 本次由内置表命中 '+sum.from_bundled+' 个'
      +' · 线上目录缓存 '+sum.from_online+' 个'
      +(bd.source?('<br>内置表来源：<span class="mono">'+esc(bd.source)+'</span>'
        +(bd.generated_at?('（'+esc(String(bd.generated_at).slice(0,10))+'）'):'')):'')
      +'</div></div></div>';
  }
  // 在线目录（登录后拉取，含线上倍率）
  let catHtml='';
  if(cat){
    const cms=cat.models||[];
    catHtml='<div class="card"><div class="ch"><b>线上模型目录（含倍率）</b>'
      +'<span class="faint">'+esc(cat.platform||'')+' · '+esc(cat.endpoint||'')+'</span></div><div class="cb">';
    if(cms.length){
      catHtml+='<div class="scroll" style="max-height:360px"><table><thead><tr><th>模型</th><th>倍率</th>'
        +'<th>上下文</th><th>输出上限</th><th>来源</th></tr></thead><tbody>';
      for(const x of cms){
        const rt=x.rate==null?'—':String(x.rate);
        catHtml+='<tr><td class="mono">'+esc(x.id)+'</td>'
          +'<td>'+tag(rt,rt&&rt!=='—'?'acc':'')+'</td>'
          +'<td>'+esc(x.ctx||'—')+'</td><td>'+esc(x.out||'—')+'</td>'
          +'<td class="faint">'+esc(x.source||'')+'</td></tr>';
      }
      catHtml+='</tbody></table></div>';
    }else{
      catHtml+=empty('未解析出模型：'+esc(cat.error||cat.raw_auth_error||'可能是凭据无效或接口结构变化'));
    }
    catHtml+='</div></div>';
  }
  return `
  ${catHtml}
  ${rateBar}
  <div class="card">
    <div class="flex" style="justify-content:space-between">
      <h2>模型清单（${rows.length||v1.length}）</h2>
      <div class="btnrow">
        <label class="faint" style="margin:0;white-space:nowrap" for="catalogPlatform"
          title="选择「从哪个平台拉取线上模型与倍率」，与下面的模型清单不是一回事">
          倍率来源
        </label>
        <select id="catalogPlatform" style="width:auto;min-width:210px">
          ${(S.data.catalogSources||[]).length
            ? (S.data.catalogSources||[]).map(x=>
                '<option value="'+esc(x.platform)+'"'
                +(x.has_account?' data-ok="1"':'')+'>'
                +esc(x.name)+(x.has_account?' ✓已登录':' · 未登录')+'</option>').join('')
            : '<option value="apk-codebuddy">CodeBuddy 国际</option>'}
        </select>
        <button class="btn" onclick="fetchCatalog()">拉取线上倍率</button>
        <button class="btn" onclick="probeModels()">探测免费/收费</button>
      </div>
    </div>
    <div class="muted" style="font-size:12px;margin:-4px 0 10px">
      上面的下拉框选的是「从哪个平台拉倍率」；下面这张表是网关当前真实可用的模型清单。
      勾选左侧方框可选择模型，用来复制 ID 或导出配置。
    </div>
    <div class="dtbar" style="border:1px solid var(--line);border-radius:var(--r);margin-bottom:0">
      <label style="margin:0;display:flex;align-items:center;gap:6px;white-space:nowrap">
        <input type="checkbox" style="width:auto" ${MODEL_SEL.size===rows.length&&rows.length?'checked':''}
          onchange="selAllModels(this.checked)"> 全选
      </label>
      <span class="cnt" style="margin-left:0">已选 <b id="selCount">${MODEL_SEL.size}</b> / ${rows.length}</span>
      <span class="sp"></span>
      <button class="btn sm" onclick="selCopyIds()">复制选中 ID</button>
      <button class="btn sm" onclick="selExport()">导出选中</button>
      <button class="btn sm" onclick="selClear()">清空选择</button>
    </div>
    <div class="mt" id="modelTable">${dataTable({
      key:'models',
      rows:rows,
      size:25,
      sort:'id',
      searchHint:'搜索模型 ID / 倍率 / 来源…',
      searchKeys:['id','credits','rate_source','name'],
      cols:[
        {k:'_sel', t:'', w:'34px', render:x=>
          '<input type="checkbox" style="width:auto" '+(MODEL_SEL.has(x.id)?'checked':'')
          +' onchange="selOne(this,\''+esc(x.id)+'\')">'},
        {k:'id', t:'模型 ID', cls:'mono', render:x=>'<span class="mono">'+esc(x.id)+'</span>'},
        {k:'credits', t:'倍率', render:x=>x.credits
            ?tag(x.credits,'acc')
            :'<span class="faint" title="公开接口里没查到该模型的倍率">未知</span>'},
        {k:'rate_source', t:'来源', render:x=>'<span class="faint" style="font-size:12px">'+esc(x.rate_source||'—')+'</span>'},
        {k:'ctx', t:'上下文', num:true, render:x=>x.ctx?Number(x.ctx).toLocaleString():'<span class="faint">—</span>'},
        {k:'out', t:'输出', num:true, render:x=>x.out?Number(x.out).toLocaleString():'<span class="faint">—</span>'},
        {k:'cnFree', t:'国内站', render:x=>x.cnFree&&x.cnFree!=='-'?tag(x.cnFree,'ok'):'<span class="faint">—</span>'},
        {k:'intlFree', t:'国际站', render:x=>x.intlFree&&x.intlFree!=='-'?tag(x.intlFree,'ok'):'<span class="faint">—</span>'},
        {k:'cost', t:'实测成本', render:x=>x.cost&&x.cost!=='未观测'
            ?esc(x.cost):'<span class="faint" title="需要真实跑一次请求才会观测到">未观测</span>'},
        {k:'availableAccounts', t:'可用账号', num:true, render:x=>{
            const a=Number(x.availableAccounts||0), t=Number(x.cnAccounts||0)+Number(x.intlAccounts||0);
            if(!a&&!t) return '<span class="faint">0</span>';
            return a+' <span class="faint">/ '+t+'</span>';
        }},
      ]})}</div>
    <div class="muted mt" style="font-size:12.5px">
      倍率按优先级取三源：① 网关实测 <code>cost</code>（跑过真实流量才有数值）
      ② 线上目录（登录后点「拉取线上倍率」实时获取，端点
      <code>/console/enterprises/personal/models</code>）
      ③ 内置倍率表（APK 自带 codebuddy-code 目录，${bd.total||0} 个模型 ${bd.with_rate||0} 个带倍率，
      含精确 / 别名 / 归一化 / 同族前缀四级匹配）。<br>
      默认模型：<code>${esc((r.admin&&r.admin.default)||'—')}</code> ·
      静态表 <code>${(r.codebuddy_static||[]).length}</code> 条 ·
      面板聚合端点 <code>/v1/models</code> 共 ${v1.length} 个
    </div>
  </div>

  <div class="card">
    <h2>接入方式</h2>
    <div class="note">本面板自身也是一个 OpenAI 兼容端点，可直接作为 base_url 使用（内部转发到本地网关）。</div>
    <pre>${esc([
'# OpenAI 兼容（面板聚合入口）',
'base_url = http://127.0.0.1:<面板端口>/v1',
'api_key  = admin',
'',
'# Claude Code / Anthropic 兼容',
'ANTHROPIC_BASE_URL = http://127.0.0.1:<面板端口>',
'ANTHROPIC_API_KEY  = admin',
'',
'# 直连本地网关（更快，少一跳）',
'base_url = http://127.0.0.1:8317/v1',
'api_key  = admin',
'',
'# curl 自测',
'curl http://127.0.0.1:<面板端口>/v1/chat/completions \\',
'  -H "Content-Type: application/json" \\',
'  -H "Authorization: Bearer admin" \\',
'  -d \'{"model":"default","messages":[{"role":"user","content":"你好"}]}\''
].join('\n'))}</pre>
  </div>`;
}
async function probeModels(){
  const r=await post('/api/settings?action=probe_models',{limit:8});
  toast(r.ok?('探测已启动：'+(r.message||'')):'失败：'+(r.message||r.code),r.ok?'ok':'err');
  setTimeout(loadModels,5000);
}

// ---------------------------------------------------------------- 站点
async function loadSites(){
  const r=await api('/api/sites');
  S.data.sites=r.sites||[];
  S.data.catalogSites=r.catalog||[];
  render();
}
function viewSites(){
  const sites=S.data.sites||[];
  const cat=S.data.catalogSites||[];
  return `
  <div class="card">
    <div class="flex" style="justify-content:space-between">
      <h2>已接入站点（${sites.length}）</h2>
      <div class="flex">
        <button class="btn" onclick="probeAll()">批量探测</button>
        <button class="btn" onclick="balAll()">批量查余额</button>
        <button class="btn pri" onclick="importCatalog()">从导航导入</button>
      </div>
    </div>
    <div class="scroll">
      <table><thead><tr><th>名称</th><th>地址</th><th>凭据</th><th>签到</th><th>操作</th></tr></thead><tbody>
      ${sites.length?sites.map(s=>`<tr>
        <td>${esc(s.name)}${s.checkin?' '+tag('签到','ok'):''}
          ${s.in_route?' '+tag('路由中','acc'):''}</td>
        <td><a href="${esc(s.url)}" target="_blank" rel="noopener">${esc(s.url)}</a></td>
        <td class="muted">${s.token?'Token':(s.cookie?'Cookie':(s.password?'账密':'<b style="color:var(--err)">未配置</b>'))}</td>
        <td>${s.checkin?tag('已启用','ok'):tag('关闭','')}</td>
        <td class="nowrap">
          <button class="btn sm" onclick="probeOne('${esc(s.id)}')">探测</button>
          <button class="btn sm" onclick="runOne('${esc(s.id)}')">签到</button>
          <button class="btn sm" onclick="editSite('${esc(s.id)}')">编辑</button>
          <button class="btn sm dgr" onclick="delSite('${esc(s.id)}')">删除</button>
        </td></tr>`).join('')
      :'<tr><td colspan="5">'+empty('还没有接入站点。点右上角「从导航导入」可一键导入全部公益站')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>添加站点</h2>
    <input type="hidden" id="sId">
    <div class="row">
      <div><label>名称 *</label><input id="sName" placeholder="AnyRouter"></div>
      <div><label>地址 *</label><input id="sUrl" placeholder="https://anyrouter.top"></div>
    </div>
    <div class="row">
      <div><label>Access Token</label><input id="sToken" placeholder="sk-… 或系统 access token"></div>
      <div><label>API User（new-api-user 头）</label><input id="sUser" placeholder="用户 id，Cookie 模式必填"></div>
    </div>
    <div class="row">
      <div><label>Cookie（可选）</label><input id="sCookie" placeholder="session=…"></div>
      <div><label>用户名（可选）</label><input id="sU2"></div>
      <div><label>密码（可选）</label><input id="sP2" type="password"></div>
    </div>
    <div class="row">
      <div><label>签到路径</label><input id="sPath" value="/api/user/checkin"></div>
      <div class="n" style="flex:0 0 auto"><label>&nbsp;</label>
        <button class="btn" onclick="clearForm()">清空</button></div>
      <div class="n" style="flex:0 0 auto"><label>&nbsp;</label>
        <button class="btn pri" onclick="saveSite()">保存</button></div>
    </div>
    <label style="margin-top:16px;font-weight:600;color:var(--fg)">纳入智能路由</label>
    <div class="note">勾选后该站点成为「自动模型」的候选上游，路由引擎会探延迟、按策略自动选路。</div>
    <div class="row">
      <div class="n" style="flex:0 0 auto"><div class="flex">
        <input type="checkbox" id="sRoute" style="width:auto"><span>加入路由</span></div></div>
      <div><label>路由端点（留空=上面的地址 + /v1）</label><input id="sRep" placeholder="https://anyrouter.top/v1"></div>
    </div>
    <div class="row">
      <div><label>上游模型名（该站支持的模型 id）</label><input id="sRModel" placeholder="claude-sonnet-4"></div>
      <div><label>权重（越大越优先）</label><input id="sRWeight" type="number" value="100"></div>
      <div><label>优先级（越小越优先）</label><input id="sRPriority" type="number" value="10"></div>
    </div>
    <div class="muted mt" style="font-size:12.5px">
      认证优先级：Token + new-api-user ＞ Cookie ＞ 账密自动登录。
      AnyRouter 用 <code>/api/user/sign_in</code>，其余多数为 <code>/api/user/checkin</code>。
    </div>
  </div>

  <div class="card">
    <h2>探测结果</h2>
    <pre id="probeOut">${esc(S.data.probeOut||'（点击「批量探测」或单个站点的「探测」按钮）')}</pre>
  </div>

  <div class="card">
    <h2>可一键导入的公益站（${cat.length}）</h2>
    <div class="scroll">
      <table><thead><tr><th>名称</th><th>地址</th><th>福利</th><th>签到</th><th>限制</th></tr></thead><tbody>
      ${cat.map(s=>`<tr>
        <td>${esc(s.name)}</td>
        <td><a href="${esc(s.url)}" target="_blank" rel="noopener">${esc(s.url)}</a></td>
        <td class="muted">${esc(s.bonus)}</td>
        <td>${s.checkin?tag('支持','ok'):tag('—')}</td>
        <td class="muted">${esc(s.limit||'')}</td></tr>`).join('')}
      </tbody></table>
    </div>
    <div class="muted mt" style="font-size:12.5px">
      导入后需为每个站点补凭据（Token / Cookie / 账密），凭据不会自动获取。
    </div>
  </div>`;
}
async function saveSite(){
  const s={
    id:$('#sId').value||undefined,
    name:$('#sName').value.trim(),
    url:$('#sUrl').value.trim(),
    token:$('#sToken').value.trim(),
    api_user:$('#sUser').value.trim(),
    cookie:$('#sCookie').value.trim(),
    username:$('#sU2').value.trim(),
    password:$('#sP2').value,
    checkin_path:$('#sPath').value.trim()||'/api/user/checkin',
    checkin:true,
    in_route:$('#sRoute').checked,
    route_endpoint:$('#sRep').value.trim(),
    route_model:$('#sRModel').value.trim()||'default',
    route_weight:parseInt($('#sRWeight').value||'100',10),
    route_priority:parseInt($('#sRPriority').value||'10',10),
  };
  if(!s.name||!s.url)return toast('名称与地址必填','err');
  const r=await post('/api/sites?action='+(s.id?'update':'add'),s);
  toast(r.message||'已保存',r.ok?'ok':'err');
  clearForm();loadSites();
}
function clearForm(){
  ['sId','sName','sUrl','sToken','sUser','sCookie','sU2','sP2','sRep','sRModel'].forEach(i=>$('#'+i).value='');
  $('#sPath').value='/api/user/checkin';
  $('#sRoute').checked=false;
  $('#sRWeight').value='100';
  $('#sRPriority').value='10';
}
function editSite(id){
  const s=(S.data.sites||[]).find(x=>x.id===id);
  if(!s)return;
  $('#sId').value=s.id;$('#sName').value=s.name||'';$('#sUrl').value=s.url||'';
  $('#sToken').value=s.token||'';$('#sUser').value=s.api_user||'';
  $('#sCookie').value=s.cookie||'';$('#sU2').value=s.username||'';$('#sP2').value=s.password||'';
  $('#sPath').value=s.checkin_path||'/api/user/checkin';
  $('#sRoute').checked=!!s.in_route;
  $('#sRep').value=s.route_endpoint||'';
  $('#sRModel').value=s.route_model||'';
  $('#sRWeight').value=s.route_weight!=null?s.route_weight:100;
  $('#sRPriority').value=s.route_priority!=null?s.route_priority:10;
  window.scrollTo({top:document.body.scrollHeight,behavior:'smooth'});
}
async function delSite(id){
  if(!confirm('删除该站点配置？'))return;
  const r=await post('/api/sites?action=delete',{id});
  toast(r.message||'已删除',r.ok?'ok':'err');
  loadSites();
}
async function probeOne(id){
  S.data.probeOut='探测中…';
  render();
  const r=await post('/api/sites?action=probe',{id});
  S.data.probeOut=JSON.stringify(r.probe||r,null,2);
  render();
}
async function probeAll(){
  S.data.probeOut='批量探测中，请稍候…';render();
  const r=await post('/api/sites?action=probe_all',{});
  S.data.probeOut=JSON.stringify(r.results||r,null,2);
  render();
}
async function balAll(){
  S.data.probeOut='查询余额中…';render();
  const r=await post('/api/sites?action=balance_all',{});
  S.data.probeOut=JSON.stringify(r.results||r,null,2);
  render();
}
async function importCatalog(){
  const cat=S.data.catalogSites||[];
  const payload=cat.map(s=>({
    name:s.name,url:s.url,token:'',cookie:'',username:'',password:'',
    checkin:!!s.checkin,checkin_path:s.name==='AnyRouter'?'/api/user/sign_in':'/api/user/checkin',
    category:s.category,bonus:s.bonus,limit:s.limit,
  }));
  const r=await post('/api/sites?action=import',{sites:payload});
  toast('已导入 '+r.imported+' 个站点，请逐个补凭据',r.ok?'ok':'err');
  loadSites();
}

// ---------------------------------------------------------------- 导航
async function loadCatalog(){
  const r=await api('/api/catalog');
  S.data.catalog=r;
  render();
}
function viewCatalog(){
  const c=S.data.catalog||{};
  const relay=c.relay_sites||[],off=c.official||[],os=c.open_source||[],loc=c.local_gateways||[],apk=c.apk_analysis||{};
  return `
  <div class="note">清单核验日期：<b>${esc(c.verified_at||'—')}</b>。公益站额度随时变化，以各站实时页面为准；
  请勿传入隐私数据、勿绑定主账号、勿大额充值。</div>

  <div class="card">
    <h2>本地网关（反解自工作区文件）</h2>
    <div class="scroll">
      <table><thead><tr><th>名称</th><th>类型</th><th>包名/版本</th><th>上游</th><th>能力</th></tr></thead><tbody>
      ${loc.map(g=>`<tr>
        <td><b>${esc(g.name)}</b><div class="faint mono" style="font-size:11.5px">${esc(g.file||'')}</div></td>
        <td>${tag(g.kind==='exe'?'Windows EXE':'Android APK',g.kind==='exe'?'acc':'info')}</td>
        <td class="mono">${esc(g.package||'—')}<div class="faint">${esc(g.version||'')}</div></td>
        <td class="muted" style="font-size:12px">${(g.upstream||[]).map(esc).join('<br>')}</td>
        <td class="nowrap">
          ${g.checkin?tag('签到','ok'):''} ${g.tasks?tag('任务','acc'):''} ${g.usage?tag('用量','info'):''}
          <div class="faint" style="font-size:11.5px;margin-top:4px">${esc(g.free||'')}</div>
        </td></tr>`).join('')}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>公益中转站（${relay.length}）</h2>
    <div class="scroll">
      <table><thead><tr><th>名称</th><th>地址</th><th>福利</th><th>覆盖</th><th>限制</th></tr></thead><tbody>
      ${relay.map(s=>`<tr>
        <td><b>${esc(s.name)}</b></td>
        <td><a href="${esc(s.signup)}" target="_blank" rel="noopener">${esc(s.url)}</a></td>
        <td class="muted">${esc(s.bonus)}</td>
        <td class="muted" style="font-size:12px">${esc(s.models)}</td>
        <td>${s.checkin?tag('每日签到','ok'):tag('无签到','')}
          <div class="faint" style="font-size:11.5px">${esc(s.limit||'')}</div></td></tr>`).join('')}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>官方免费额度平台（${off.length}）</h2>
    <div class="scroll">
      <table><thead><tr><th>平台</th><th>区域</th><th>免费内容</th><th>备注</th></tr></thead><tbody>
      ${off.map(s=>`<tr>
        <td><a href="${esc(s.url)}" target="_blank" rel="noopener"><b>${esc(s.name)}</b></a></td>
        <td>${tag(s.region,s.region==='国内'?'acc':'info')}</td>
        <td class="muted">${esc(s.free)}</td>
        <td class="faint" style="font-size:12px">${esc(s.note)}</td></tr>`).join('')}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>APK 深度反解</h2>
    <div class="scroll">
      <table><thead><tr><th>文件</th><th>包名</th><th>版本</th><th>SDK</th><th>组件</th><th>能力</th></tr></thead><tbody>
      ${Object.entries(apk).map(([f,a])=>`<tr>
        <td class="mono">${esc(f)}</td>
        <td class="mono">${esc(a.package)}</td>
        <td>${esc(a.version)}</td>
        <td class="faint">${esc(a.sdk)}</td>
        <td class="muted" style="font-size:11.5px">${(a.components||[]).map(c=>esc(c.split('.').pop())).join('<br>')}</td>
        <td class="nowrap">${(a.capabilities||[]).map(x=>tag(x,'info')).join(' ')}</td></tr>`).join('')}
      </tbody></table>
    </div>
    <div class="muted mt" style="font-size:12.5px">
      权限清单（base.apk）：INTERNET · FOREGROUND_SERVICE · WAKE_LOCK · DUMP ·
      REQUEST_IGNORE_BATTERY_OPTIMIZATIONS · POST_NOTIFICATIONS
    </div>
  </div>

  <div class="card">
    <h2>开源聚合 / 自建工具</h2>
    <table><tbody>
      ${os.map(s=>`<tr><td><a href="${esc(s.url)}" target="_blank" rel="noopener"><b>${esc(s.name)}</b></a></td>
        <td class="muted">${esc(s.desc)}</td></tr>`).join('')}
    </tbody></table>
  </div>`;
}

// ---------------------------------------------------------------- 智能路由
async function loadRoute(){
  const r=await api('/api/route');
  S.data.route=r;
  render();
}
function viewRoute(){
  const r=S.data.route||{};
  const models=r.models||{};
  const names=Object.keys(models);
  const cfg=r.config||{};
  return `
  <div class="note">
    面板即中转层。客户端把 <code>base_url</code> 指到本面板，<code>model</code> 填
    <code>auto-fast</code> / <code>auto-weight</code> / <code>auto-priority</code>，
    剩下的选路由路由引擎决定：探延迟、熔断、失败自动降级、会话亲和。
  </div>

  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">自动模型</div><div class="vl">${names.length}</div>
      <div class="ex">默认三种策略</div></div>
    <div class="kpi"><div class="lb">纳入路由的站点</div><div class="vl">${(r.in_route_sites||[]).length}</div>
      <div class="ex">在「资源站点」里勾选</div></div>
    <div class="kpi"><div class="lb">延迟探测</div>
      <div class="vl" style="font-size:19px">${r.auto_probe?'自动':'关'}</div>
      <div class="ex">每 ${r.probe_interval||60} 秒一轮</div></div>
    <div class="kpi"><div class="lb">熔断策略</div>
      <div class="vl" style="font-size:19px">${r.breaker_fail||3} 次 / ${r.breaker_cooldown||60}s</div>
      <div class="ex">连续失败即摘除</div></div>
  </div>

  <div class="card">
    <div class="flex" style="justify-content:space-between">
      <h2>路由表</h2>
      <div class="flex">
        <button class="btn" onclick="probeRoute()">探延迟</button>
        <button class="btn" onclick="testRoute('auto-fast')">试跑 auto-fast</button>
        <button class="btn pri" onclick="testRoute('auto-weight')">试跑 auto-weight</button>
      </div>
    </div>
    <div class="scroll">
      <table><thead><tr><th>自动模型</th><th>策略</th><th>候选上游</th><th>延迟 (EWMA)</th><th>健康</th><th>成功率</th><th>最近错误</th></tr></thead><tbody>
      ${names.length?names.map(n=>{
        const m=models[n],tg=m.targets||[];
        return `<tr>
        <td><b class="mono">${esc(n)}</b></td>
        <td>${tag(strategyLabel(cfg[n]),cfg[n]==='fastest'?'ok':(cfg[n]==='weight'?'info':'acc'))}</td>
        <td>${m.healthy} / ${m.total}</td>
        <td class="mono">${tg.map(t=>`${t.ewma_ms!=null?t.ewma_ms+'ms':'—'}`).join(' · ')||'—'}</td>
        <td>${tg.filter(t=>t.state==='closed').length} 正常 / ${tg.filter(t=>t.state==='open').length} 熔断</td>
        <td>${tg.map(t=>t.success_rate!=null?t.success_rate+'%':'—').join(' · ')||'—'}</td>
        <td class="faint" style="font-size:11.5px">${esc((tg.find(t=>t.last_error)||{}).last_error||'')}</td>
      </tr>`}).join(''):'<tr><td colspan="7">'+empty('路由表为空。请确认网关路径正确，或在「资源站点」里把站点纳入路由')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>候选上游明细</h2>
    <div class="scroll" style="max-height:600px">
      <table><thead><tr><th>自动模型</th><th>上游</th><th>端点</th><th>权重</th><th>优先级</th><th>最近延迟</th><th>状态</th></tr></thead><tbody>
      ${names.length?names.flatMap(n=>(models[n].targets||[]).map(t=>`<tr>
        <td class="mono">${esc(n)}</td>
        <td>${esc(t.name)}<div class="faint mono" style="font-size:11px">${esc(t.upstream)}</div></td>
        <td class="mono" style="font-size:11.5px">${esc(t.endpoint)}</td>
        <td>${t.weight}</td><td>${t.priority}</td>
        <td class="mono">${t.latency_ms!=null?t.latency_ms+'ms':'—'}</td>
        <td>${tag(t.state==='closed'?'正常':(t.state==='open'?'熔断 '+t.cooldown_left+'s':'半开'),
            t.state==='closed'?'ok':(t.state==='open'?'err':'warn'))}</td>
      </tr>`)).join(''):'<tr><td colspan="7">'+empty('暂无候选')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>策略说明</h2>
    <table><tbody>
      <tr><td><b>auto-fast</b></td><td>优先实测延迟（EWMA 平滑）最低的上游。同一会话会尽量粘在同一上游，避免上下文抖动。</td></tr>
      <tr><td><b>auto-weight</b></td><td>按站点配置的权重排序，权重高的先用；同权重比延迟。适合「主用某家、备用某家」。</td></tr>
      <tr><td><b>auto-priority</b></td><td>严格按优先级数值从上到下，只有失败才降级。适合「必须走某家，不行再退」。</td></tr>
      <tr><td>能力过滤</td><td>请求带 <code>tools</code> 时自动排除不支持工具调用的上游；带图片时排除不支持视觉的。</td></tr>
      <tr><td>熔断</td><td>连续失败 ${r.breaker_fail||3} 次进 OPEN，冷却 ${r.breaker_cooldown||60} 秒后半开试探，成功即恢复。</td></tr>
      <tr><td>降级</td><td>429 / 5xx / 超时自动切下一个上游，最多试 4 个。4xx（除 429）不降级，直接报错。</td></tr>
    </tbody></table>
  </div>

  <div class="card">
    <h2>试跑结果</h2>
    <pre id="routeOut">${esc(S.data.routeOut||'（点上方按钮试跑一个自动模型）')}</pre>
  </div>

  <div class="card">
    <h2>客户端接入</h2>
    <div class="row" style="margin-bottom:12px">
      <div><label>面板地址</label><input readonly value="${esc(location.origin)}"></div>
      <div><label>base_url</label><input readonly value="${esc(location.origin)}/v1"></div>
      <div><label>api_key</label><input readonly value="admin"></div>
    </div>
    <div class="row">
      <div><label>最快策略模型名</label><input readonly value="auto-fast"></div>
      <div><label>权重策略模型名</label><input readonly value="auto-weight"></div>
      <div><label>优先级策略模型名</label><input readonly value="auto-priority"></div>
    </div>
    <pre class="mt">${esc([
'# 1) 客户端只填面板地址，模型填 auto-fast',
'base_url = '+location.origin+'/v1',
'api_key  = admin',
'model    = auto-fast          # 自动挑延迟最低的上游',
'',
'# 2) 单次请求临时指定策略（面板扩展字段，不影响标准客户端）',
'{',
'  "model": "auto-weight",',
'  "aigw_strategy": "priority",   # fastest | weight | priority',
'  "aigw_session": "conv-123",    # 会话亲和 ID，让同一对话固定同一上游',
'  "messages": [{"role":"user","content":"你好"}]',
'}',
'',
'# 3) curl 实测',
'curl '+location.origin+'/v1/chat/completions \\',
'  -H "Content-Type: application/json" \\',
'  -d \'{"model":"auto-fast","messages":[{"role":"user","content":"ping"}]}\'',
'',
'# 响应里会带回路由痕迹 _route / _aigw_route，方便确认走了哪家'
].join('\n'))}</pre>
  </div>`;
}
function strategyLabel(s){
  return {fastest:'最快延迟',weight:'权重优先',priority:'优先级'}[s]||s||'—';
}
async function probeRoute(){
  toast('探测中，各上游并发测 /models…');
  const r=await post('/api/route?action=probe',{});
  S.data.routeOut=JSON.stringify(r.results||r,null,2);
  S.data.route=r;
  render();
  toast('探测完成，延迟已写入 EWMA','ok');
}
async function testRoute(model){
  S.data.routeOut='试跑 '+model+' …';
  render();
  const r=await post('/api/route?action=test_chat',{model,message:'ping'});
  S.data.routeOut=JSON.stringify(r,null,2);
  S.data.route=r;
  render();
  const x=r.result||{};
  toast(x.choices?'试跑成功，走 '+(x._route||{}).upstream:'试跑失败：'+(x.message||'未知'),
        x.choices?'ok':'err');
}

// ---------------------------------------------------------------- 上游档案
async function loadUpstreams(){
  const r=await api('/api/upstreams');
  S.data.upstreams=r;
  render();
}
function viewUpstreams(){
  const u=S.data.upstreams||{};
  const st=u.stats||{};
  const gw=u.gateways||[],rel=u.relays||[],off=u.official||[],os=u.open_source||[];
  const all=u.all||[];
  const vp=all.filter(x=>x.category==='vibe_proxy').map(x=>
    Object.assign({},x,{_targets:(x.targets||[]).join(' ')}));
  const dead=u.vibe_dead||[];
  return `
  <div class="note">清单核验 <b>${esc(st.verified_at||'—')}</b> ·
  共 <b>${st.total_upstreams||0}</b> 个可作为路由目标的上游，
  合计 <b>${st.gateway_endpoints||0}</b> 个已反解端点。
  这些档案直接喂给路由引擎，可在「智能路由」里启用。</div>

  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">本地网关</div><div class="vl">${st.gateways||0}</div>
      <div class="ex">${st.gateway_endpoints||0} 个端点</div></div>
    <div class="kpi"><div class="lb">公益中转站</div><div class="vl">${st.relays||0}</div>
      <div class="ex">${st.relays_with_checkin||0} 个支持签到</div></div>
    <div class="kpi"><div class="lb">官方免费平台</div><div class="vl">${st.official||0}</div>
      <div class="ex">国内 ${st.official_cn||0} · 海外 ${st.official_intl||0}</div></div>
    <div class="kpi"><div class="lb">开源工具</div><div class="vl">${st.open_source||0}</div>
      <div class="ex">自建/管理方案</div></div>
  </div>

  <div class="card">
    <h2>本地网关解析（${gw.length}）</h2>
    <div class="scroll">
      <table><thead><tr><th>名称</th><th>形态</th><th>版本 / 架构</th><th>协议</th><th>鉴权</th><th>能力</th><th>说明</th></tr></thead><tbody>
      ${gw.map(g=>{
        const c=g.capabilities||{};
        const caps=[];
        if(c.chat)caps.push('对话');if(c.stream)caps.push('流式');
        if(c.vision)caps.push('视觉');if(c.tools)caps.push('工具');
        if(c.responses)caps.push('Responses');if(c.anthropic)caps.push('Anthropic');
        if(c.token_count)caps.push('计 token');
        if(c.image_gen)caps.push('文生图');
        if(c.checkin)caps.push('签到');if(c.tasks)caps.push('任务');
        if(c.redeem)caps.push('兑换码');if(c.usage)caps.push('用量');
        return `<tr>
        <td><b>${esc(g.name)}</b><div class="faint mono" style="font-size:11px">${esc(g.file||'')}</div>
            <div class="faint" style="font-size:11px">${esc(g.free||'')}</div></td>
        <td>${tag(g.kind==='exe'?'Windows EXE':'Android APK',g.kind==='exe'?'acc':'info')}
            ${g.managed?tag('面板托管','ok'):''}</td>
        <td class="mono" style="font-size:11.5px">${esc(g.version||'')}<div class="faint">${esc(g.lang||'')}</div></td>
        <td class="mono" style="font-size:11.5px">${esc(g.protocol)}</td>
        <td class="mono" style="font-size:11.5px">${esc(g.auth||'')}</td>
        <td style="max-width:180px">${caps.map(x=>tag(x,'info')).join(' ')}</td>
        <td class="muted" style="font-size:12px;max-width:280px">${esc(g.note||'')}</td>
      </tr>`}).join('')}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>已反解端点明细</h2>
    <div class="scroll" style="max-height:560px">
      <table><thead><tr><th>网关</th><th>端点数</th><th>端点列表</th></tr></thead><tbody>
      ${gw.map(g=>`<tr>
        <td><b>${esc(g.name)}</b></td>
        <td>${(g.endpoints||[]).length}</td>
        <td class="mono" style="font-size:11px;max-width:520px;word-break:break-all">
          ${(g.endpoints||[]).map(e=>esc(e)).join('<br>')}</td>
      </tr>`).join('')}
      </tbody></table>
    </div>
  </div>

  ${u.codebuddy_models?`<div class="card">
    <h2>CodeBuddy 国际版模型清单（${u.codebuddy_models.length}）</h2>
    <div class="faint mb" style="font-size:12px">反解自 base(3).apk 内 assets/codebuddy-international-models.json，
      生成自 @tencent-ai/codebuddy-code@2.150.0</div>
    <div class="scroll sm">
      <table><thead><tr><th>模型 ID</th><th>名称</th><th>计费倍率</th><th>上下文</th><th>输出上限</th><th>视觉</th><th>工具</th><th>推理</th></tr></thead><tbody>
      ${u.codebuddy_models.map(m=>`<tr>
        <td class="mono">${esc(m.id)}</td><td>${esc(m.name)}</td>
        <td>${esc(m.credits)}</td>
        <td>${m.ctx?nraw(m.ctx):'—'}</td><td>${m.out?nraw(m.out):'—'}</td>
        <td>${m.vision?'✓':'—'}</td><td>${m.tools?'✓':'—'}</td><td>${m.reasoning?'✓':'—'}</td>
      </tr>`).join('')}
      </tbody></table>
    </div>
  </div>`:''}

  <div class="card">
    <h2>公益中转站（${rel.length}）</h2>
    <div class="scroll">
      <table><thead><tr><th>名称</th><th>端点</th><th>认证</th><th>签到路径</th><th>福利</th><th>限制</th></tr></thead><tbody>
      ${rel.map(r=>`<tr>
        <td><b>${esc(r.name)}</b><div class="faint" style="font-size:11px">${esc(r.models)}</div></td>
        <td class="mono" style="font-size:11.5px">${esc(r.endpoint)}</td>
        <td class="mono" style="font-size:11.5px">${esc(r.auth)}</td>
        <td class="mono" style="font-size:11.5px">${esc(r.checkin_path||'—')}</td>
        <td class="muted" style="font-size:12px">${esc(r.bonus)}</td>
        <td class="faint" style="font-size:11.5px">${esc(r.limit)}</td>
      </tr>`).join('')}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>官方免费额度平台（${off.length}）</h2>
    <div class="scroll">
      <table><thead><tr><th>平台</th><th>区域</th><th>端点</th><th>密钥变量</th><th>免费内容</th><th>限流</th><th>签到</th></tr></thead><tbody>
      ${off.map(o=>`<tr>
        <td><b>${esc(o.name)}</b><div class="faint" style="font-size:11px">${esc(o.note||'')}</div></td>
        <td>${tag(o.region,o.region==='国内'?'acc':'info')}</td>
        <td class="mono" style="font-size:11px;word-break:break-all">${esc(o.endpoint)}</td>
        <td class="mono" style="font-size:11.5px">${esc(o.env||'')}</td>
        <td class="muted" style="font-size:12px;max-width:260px">${esc(o.free)}</td>
        <td class="faint" style="font-size:11.5px">${esc(o.rate||'—')}</td>
        <td>${o.checkin?tag('有','ok'):'—'}</td>
      </tr>`).join('')}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>开源工具（${os.length}）</h2>
    <table><tbody>
      ${os.map(s=>`<tr>
        <td><a href="${esc(s.url)}" target="_blank" rel="noopener"><b>${esc(s.name)}</b></a>
            <div class="faint" style="font-size:11px">${esc(s.stack)}</div></td>
        <td>${tag(s.role,'acc')}</td>
        <td class="muted" style="font-size:12px">${esc(s.desc)}</td>
      </tr>`).join('')}
    </tbody></table>
  </div>

  <!-- ============ Vibe Coding 反代项目（2026-10-04 调研） ============ -->
  <div class="card">
    <div class="ch">
      <div><b>Vibe Coding 反代项目（${vp.length}）</b>
        <div class="faint" style="font-size:12px;margin-top:2px">
          把 Vibe Coding 工具的登录凭据转成 OpenAI 兼容 API。本机部署任意一个，
          就能直接作为上游挂进路由引擎</div></div>
      <div class="flex n">
        ${tag('带签到 '+st.vibe_proxy_with_checkin,'ok')}
        ${tag('有默认端点 '+st.vibe_proxy_deployed,'acc')}
        ${tag('已失效 '+st.vibe_dead,'err')}
      </div>
    </div>
    ${dataTable({
      key:'vibe',
      rows:vp,
      size:12,
      sort:'stars',
      searchHint:'搜索反代项目 / 支持的平台…',
      searchKeys:['name','lang','_targets','deploy','note'],
      cols:[
        {k:'name', t:'项目', render:x=>'<a href="'+esc(x.repo)+'" target="_blank" rel="noopener">'
          +'<b>'+esc(x.name)+'</b></a>'
          +'<div class="faint" style="font-size:11px">★'+x.stars+' · '+esc(x.lang)
          +' · 更新 '+esc(x.updated)+'</div>'},
        {k:'priority', t:'优先级', num:true,
         render:x=>x.priority===1?tag('P1 优先','ok'):x.priority===2?tag('P2','acc'):tag('P'+x.priority,'')},
        {k:'_targets', t:'支持的上游',
         render:x=>'<div style="font-size:11.5px;line-height:1.5">'
           +(x.targets||[]).map(t=>'<span class="tag" style="margin:1px 2px 1px 0">'+esc(t)+'</span>').join('')
           +'</div>'},
        {k:'protocol', t:'协议',
         render:x=>'<span class="mono" style="font-size:11px">'+esc((x.protocol||[]).join(' / ')||'—')+'</span>'},
        {k:'auth', t:'鉴权', render:x=>'<span class="mono" style="font-size:11.5px">'+esc(x.auth||'—')+'</span>'},
        {k:'checkin', t:'签到', render:x=>x.checkin?tag('有','ok'):'—'},
        {k:'endpoint', t:'本机端点', render:x=>x.endpoint
            ?'<span class="mono" style="font-size:11px">'+esc(x.endpoint)+'</span>'
            :'<span class="faint" style="font-size:11.5px">未部署</span>'},
        {k:'deploy', t:'部署方式',
         render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.deploy||'')+'</span>'},
        {k:'note', t:'说明', render:x=>'<span class="muted" style="font-size:11.5px;display:block;min-width:240px">'
            +esc(x.note||'')+'</span>'},
      ]})}
  </div>

  <div class="card">
    <h2>已失效 / 不推荐（${dead.length}）</h2>
    <div class="note warn">这些是 2026-10-04 实测已死或已停更的项目，留着当反面清单，别再浪费时间。</div>
    <table><tbody>
      ${dead.map(d=>`<tr>
        <td><a href="${esc(d.url)}" target="_blank" rel="noopener"><b>${esc(d.name)}</b></a></td>
        <td class="muted" style="font-size:12px">${esc(d.why)}</td>
      </tr>`).join('')}
    </tbody></table>
  </div>`;
}

// ---------------------------------------------------------------- 通知
async function loadNotify(){
  const r=await api('/api/notify');
  S.data.notify=r;
  render();
}
function viewNotify(){
  const r=S.data.notify||{};
  const cfg=r.notify||{},hooks=cfg.webhooks||[],hist=cfg.history||[],types=r.types||[];
  return `
  <div class="grid g2">
    <div class="card">
      <h2>通知开关</h2>
      <label>签到 / 成长任务结果</label>
      <div class="flex"><input type="checkbox" id="nCheck" style="width:auto" ${cfg.notify_checkin!==false?'checked':''} onchange="saveNotifyFlags()"></div>
      <label>额度 / 用量告警</label>
      <div class="flex"><input type="checkbox" id="nQuota" style="width:auto" ${cfg.notify_quota!==false?'checked':''} onchange="saveNotifyFlags()"></div>
      <label>错误与失败提醒</label>
      <div class="flex"><input type="checkbox" id="nErr" style="width:auto" ${cfg.notify_error!==false?'checked':''} onchange="saveNotifyFlags()"></div>
    </div>

    <div class="card">
      <h2>添加 Webhook</h2>
      <div class="row">
        <div><label>类型</label><select id="wType">${types.map(t=>`<option value="${esc(t.type)}">${esc(t.label)}</option>`).join('')}</select></div>
        <div><label>名称（可选）</label><input id="wName" placeholder="我的企业微信"></div>
      </div>
      <label>地址</label>
      <input id="wUrl" placeholder="${esc(types[0]?types[0].url_hint:'')}">
      <div class="flex mt">
        <button class="btn" onclick="testHook()">测试</button>
        <button class="btn pri" onclick="addHook()">添加</button>
      </div>
    </div>
  </div>

  <div class="card">
    <h2>已配置 Webhook（${hooks.length}）</h2>
    <div class="scroll sm">
      <table><thead><tr><th>类型</th><th>名称</th><th>地址</th><th>状态</th><th>操作</th></tr></thead><tbody>
      ${hooks.length?hooks.map((h,i)=>`<tr>
        <td>${esc((types.find(t=>t.type===h.type)||{}).label||h.type)}</td>
        <td>${esc(h.name||'-')}</td>
        <td class="mono" style="font-size:11.5px">${esc(h.url).slice(0,54)}…</td>
        <td>${h.enabled!==false?tag('启用','ok'):tag('停用','')}</td>
        <td><button class="btn sm dgr" onclick="delHook(${i})">删除</button></td></tr>`).join('')
      :'<tr><td colspan="5">'+empty('尚未配置 Webhook。加一个就能在签到完成时收到推送')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>

  <div class="card">
    <h2>发送历史（${hist.length}）</h2>
    <div class="scroll sm">
      <table><thead><tr><th>时间</th><th>事件</th><th>目标</th><th>结果</th></tr></thead><tbody>
      ${hist.length?hist.map(h=>`<tr>
        <td class="mono nowrap">${esc(h.time)}</td><td>${esc(h.event)}</td>
        <td class="muted">${esc(h.target||'')}</td>
        <td>${tag(h.ok?'成功':'失败',h.ok?'ok':'err')} <span class="faint" style="font-size:11.5px">${esc((h.message||'').slice(0,60))}</span></td>
      </tr>`).join(''):'<tr><td colspan="4">'+empty('暂无发送记录')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>`;
}
async function saveNotifyFlags(){
  await api('/api/notify?action=send',{event:'checkin',title:'开关已更新',body:'通知开关修改已保存'});
  const r=await fetch('/api/notify');
  const j=await r.json();
  const cfg=j.notify;
  cfg.notify_checkin=$('#nCheck').checked;
  cfg.notify_quota=$('#nQuota').checked;
  cfg.notify_error=$('#nErr').checked;
  await post('/api/notify?action=__noop',{});
  // 直接覆盖保存
  await api('/api/notify?action=save_flags',{notify:cfg});
  toast('已保存','ok');
}
async function addHook(){
  const h={type:$('#wType').value,name:$('#wName').value.trim(),url:$('#wUrl').value.trim(),enabled:true,events:[]};
  if(!h.url)return toast('地址必填','err');
  const r=await post('/api/notify?action=add',h);
  toast(r.message||'已添加',r.ok?'ok':'err');
  loadNotify();
}
async function delHook(i){
  if(!confirm('删除该 Webhook？'))return;
  await post('/api/notify?action=delete',{index:i});
  loadNotify();
}
async function testHook(){
  const h={type:$('#wType').value,url:$('#wUrl').value.trim()};
  const r=await post('/api/notify?action=test',h);
  toast((r.sent?'发送成功：':'失败：')+r.message,r.sent?'ok':'err');
  loadNotify();
}

// ---------------------------------------------------------------- 日志
async function loadLogs(){
  const r=await api('/api/logs');
  S.data.logs=r;
  render();
}
function viewLogs(){
  const g=(S.data.logs&&S.data.logs.gateway)||{};
  const entries=(g.entries||g.logs||[]).slice().reverse();
  const panel=(S.data.logs&&S.data.logs.panel)||[];
  return `
  <div class="grid g2">
    <div class="card">
      <h2>网关请求日志（${entries.length}）</h2>
      <div class="scroll" style="max-height:600px">
        <table><thead><tr><th>时间</th><th>方法</th><th>路径</th><th>状态</th><th>耗时</th></tr></thead><tbody>
        ${entries.length?entries.map(e=>`<tr>
          <td class="mono nowrap">${esc(new Date(e.time*1000).toLocaleTimeString())}</td>
          <td class="mono">${esc(e.method)}</td>
          <td class="mono" style="font-size:11.5px">${esc(e.path)}</td>
          <td>${tag(e.status,e.status<400?'ok':(e.status<500?'warn':'err'))}</td>
          <td class="muted">${e.durationMs||0}ms</td></tr>`).join('')
        :'<tr><td colspan="5">'+empty('网关未运行或暂无日志')+'</td></tr>'}
        </tbody></table>
      </div>
    </div>
    <div class="card">
      <h2>面板运行日志</h2>
      <pre>${esc(panel.slice(-120).reverse().join('\n')||'暂无日志')}</pre>
    </div>
  </div>`;
}

// ---------------------------------------------------------------- 设置
async function loadSettings(){
  const r=await api('/api/settings');
  S.data.settings=r;
  render();
}
function viewSettings(){
  const r=S.data.settings||{};
  const s=r.settings||{};
  return `
  <div class="grid g2">
    <div class="card">
      <h2>网关连接</h2>
      <div class="row">
        <div><label>监听地址</label><input id="sAddr" value="${esc(s.gateway_addr||'127.0.0.1')}"></div>
        <div><label>端口</label><input id="sPort" type="number" value="${esc(s.gateway_port||8317)}"></div>
        <div><label>API Key</label><input id="sKey" value="${esc(s.gateway_api_key||'admin')}"></div>
      </div>
      <div class="row">
        <div><label>控制台用户名</label><input id="sAU" value="${esc(s.gateway_admin_user||'admin')}"></div>
        <div><label>控制台密码</label><input id="sAP" type="password" placeholder="留空则不修改"></div>
      </div>
    </div>

    <div class="card">
      <h2>自动化</h2>
      <label>每日签到时间（小时）</label>
      <input id="sCH" type="number" min="0" max="23" value="${esc(s.checkin_hour??9)}">
      <label>成长任务时间（小时）</label>
      <input id="sGH" type="number" min="0" max="23" value="${esc(s.growth_hour??10)}">
      <div class="flex mt" style="gap:18px">
        <div class="flex"><input type="checkbox" id="sAC" style="width:auto" ${s.auto_checkin!==false?'checked':''}><span>自动签到</span></div>
        <div class="flex"><input type="checkbox" id="sAG" style="width:auto" ${s.auto_growth!==false?'checked':''}><span>自动成长任务</span></div>
        <div class="flex"><input type="checkbox" id="sAS" style="width:auto" ${s.auto_start_gateway!==false?'checked':''}><span>自动拉起网关</span></div>
      </div>
    </div>
  </div>

  <div class="card">
    <button class="btn pri" onclick="saveSettings()">保存设置</button>
    <div class="muted mt" style="font-size:12.5px">
      网关程序路径：<code>${esc(r.gateway_path||'未找到')}</code><br>
      数据目录：<code>${esc(S.data.dataDir||'data/')}</code> ·
      settings.json / sites.json / checkins.json / tasks.json / notify.json
    </div>
  </div>

  <div class="card">
    <div class="flex" style="justify-content:space-between">
      <h2>使用须知</h2>
      <button class="btn dgr" onclick="quitPanel()">退出面板</button>
    </div>
    <table><tbody>
      <tr><td>签到范围</td><td>本地网关账号池（workbuddy-gateway 自身 09:00）+
        外部中转站（面板 09:05 错峰）+
        <b>APP 平台自动签到</b>（默认 09:05~10:02 逐个错峰，可逐平台开关与改时间）</td></tr>
      <tr><td>凭据安全</td><td>全部凭据只存本机 <code>data/</code> 目录，不外传；导出的 JSON 含明文凭据，请自行保管</td></tr>
      <tr><td>公益站风险</td><td>社区站可能掺水或跑路，勿传隐私、勿绑主账号、勿大额充值</td></tr>
      <tr><td>免费额度</td><td>多数平台额度有 30/90 天有效期，务必开「用完即停」避免超额扣费</td></tr>
      <tr><td>端口冲突</td><td>面板默认自动选 8790+ 空闲端口；网关默认 8317</td></tr>
      <tr><td>工具调用</td><td>面板自带 9 个本地执行器（计算器/时间/单位换算/文件沙箱/HTTP…），
        文件工具锁定在 <code>data/tools_sandbox/</code>，HTTP 默认关闭且拦内网地址</td></tr>
    </tbody></table>
  </div>`;
}
async function saveSettings(){
  const p={    gateway_addr:$('#sAddr').value.trim(),
    gateway_port:parseInt($('#sPort').value||'8317',10),
    gateway_api_key:$('#sKey').value.trim(),
    gateway_admin_user:$('#sAU').value.trim(),
    checkin_hour:parseInt($('#sCH').value||'9',10),
    growth_hour:parseInt($('#sGH').value||'10',10),
    auto_checkin:$('#sAC').checked,
    auto_growth:$('#sAG').checked,
    auto_start_gateway:$('#sAS').checked,
  };
  if($('#sAP').value)p.gateway_admin_password=$('#sAP').value;
  const r=await post('/api/settings?action=save',p);
  toast(r.message||'已保存',r.ok?'ok':'err');
  refresh();loadSettings();
}

// ---------------------------------------------------------------- 通用
async function quitPanel(){
  if(!confirm('退出面板？\n\n· 面板会关闭，http 服务停止\n· 若勾了「自动拉起网关」，网关闭后也会停止\n· 想后台常驻请用「开机自启动」'))return;
  const r=await post('/api/settings?action=shutdown',{});
  toast(r.ok?'已退出面板':'退出失败：'+(r.message||''),r.ok?'ok':'err');
  if(r.ok){
    setTimeout(()=>{
      document.body.innerHTML='<div style="padding:60px;text-align:center;font:14px/1.6 -apple-system,sans-serif;color:#6b7280">'
        +'<div style="font-size:34px;margin-bottom:12px">✓</div>'
        +'<div style="font-size:16px;color:#1c1f26;font-weight:500;margin-bottom:6px">面板已退出</div>'
        +'<div>托盘图标已移除。重新启动请再双击一次 aigw-panel.exe</div></div>';
    },600);
  }
}
async function post(path,body){
  try{return await api(path,body||{});}
  catch(e){toast('请求失败：'+e.message,'err');return {ok:false,message:e.message};}
}

// ---------------------------------------------------------------- 账号登录
let LOGIN_TIMER=null;
function stopLoginPoll(){
  if(LOGIN_TIMER){clearInterval(LOGIN_TIMER);LOGIN_TIMER=null;}
}

async function loadLogin(){
  const lp=await api('/api/login?action=platforms').catch(()=>({platforms:[]}));
  const ac=await api('/api/accounts').catch(()=>({accounts:[],stats:{}}));
  const pa=await api('/api/platform?action=actions').catch(()=>({}));
  const bd=await api('/api/login?action=browser').catch(()=>({browser:null}));
  S.data.loginPlatforms=lp.platforms||[];
  S.data.accounts=ac.accounts||[];
  S.data.accountStats=ac.stats||{};
  S.data.platformActions=pa||{};
  S.data.browserDiag=(bd&&bd.browser)||null;
  render();
}

async function startLogin(pid,edition){
  toast('正在发起登录…');
  const body={platform:pid};
  if(edition)body.edition=edition;
  const r=await api('/api/login?action=start',body).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'发起登录失败','err');return;}
  S.data.loginSession=r.session||null;
  render();
  const s=S.data.loginSession;
  if(s&&s.method==='qrcode'&&(s.status==='pending'||s.status==='waiting'))startLoginPoll();
}

function startLoginPoll(){
  stopLoginPoll();
  LOGIN_TIMER=setInterval(async()=>{
    const s=S.data.loginSession;
    if(!s){stopLoginPoll();return;}
    const r=await api('/api/login?action=poll',{id:s.id}).catch(()=>null);
    if(!r||!r.session){stopLoginPoll();return;}
    S.data.loginSession=r.session;
    paintLoginSession();
    if(r.session.status==='success'){
      stopLoginPoll();
      toast('登录成功，凭据已保存','ok');
      loadLogin();
    }else if(r.session.status==='error'||r.session.status==='expired'){
      stopLoginPoll();
      toast(r.session.message||r.session.error||'登录结束','err');
      paintLoginSession();
    }
  },2000);
}

function paintLoginSession(){
  const s=S.data.loginSession;
  if(!s)return;
  const st=$('#lsStatus'),qr=$('#lsQr'),msg=$('#lsMsg'),left=$('#lsLeft');
  if(st)st.innerHTML=loginStatusTag(s);
  if(msg)msg.textContent=s.message||s.error||'';
  if(left)left.textContent=s.seconds_left?('剩余 '+s.seconds_left+' 秒'):'';
  if(qr){
    if(s.qr)qr.innerHTML='<img src="'+s.qr+'" alt="登录二维码" style="width:196px;height:196px;border-radius:10px;border:1px solid var(--line)">';
    else qr.innerHTML='';
  }
}

function loginStatusTag(s){
  const m={pending:'info',waiting:'info',success:'ok',error:'err',expired:'warn'};
  return tag(s.status||'—',m[s.status]||'');
}

async function cancelLogin(){
  const s=S.data.loginSession;
  stopLoginPoll();
  if(s)await api('/api/login?action=cancel',{id:s.id}).catch(()=>null);
  S.data.loginSession=null;
  render();
}

async function pollCookieOnce(){
  const s=S.data.loginSession;
  if(!s){toast('没有进行中的会话','err');return;}
  const r=await api('/api/login?action=poll',{id:s.id}).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'读取失败','err');return;}
  S.data.loginSession=r.session;
  paintLoginSession();
  if(r.session.status==='success'){
    toast('已获取 Cookie 并保存','ok');
    loadLogin();
  }else{
    toast(r.session.message||'还没检测到登录 Cookie','warn');
  }
}

async function submitManualCookie(){
  const s=S.data.loginSession;
  const box=$('#manualCookie');
  const v=box?box.value.trim():'';
  if(!v){toast('请先粘贴 Cookie','err');return;}
  const pid=s?s.platform:(($('#manualPlatform')||{}).value||'');
  if(!pid){toast('缺少平台','err');return;}
  const r=await api('/api/login?action=submit',
    {id:s?s.id:undefined,platform:pid,cookie:v}).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'保存失败','err');return;}
  if(r.session){
    S.data.loginSession=r.session;
    paintLoginSession();
    if(r.session.status==='success'){toast('已保存','ok');loadLogin();}
    else toast(r.session.message||r.session.error||'未通过校验','warn');
  }else{toast('已保存','ok');loadLogin();}
}

async function delAccount(id){
  if(!confirm('确认删除该账号凭据？'))return;
  const r=await api('/api/accounts?action=delete',{id:id}).catch(e=>({ok:false}));
  if(r.ok===false){toast('删除失败','err');return;}
  toast('已删除','ok');
  loadLogin();
}

async function toggleAccount(id,on){
  await api('/api/accounts?action=update',{id:id,enabled:on}).catch(()=>null);
  loadLogin();
}

async function runPlatformAction(pid,act,need){
  let params={};
  if(need&&need.length){
    for(const k of need){
      const el=$('#pa_'+k);
      const v=el?el.value.trim():'';
      if(!v){toast('请填写 '+k,'err');return;}
      params[k]=v;
    }
  }
  toast('正在调用 '+act+' …');
  const r=await api('/api/platform?action=call',{platform:pid,action:act,params:params})
    .catch(e=>({ok:false,message:e.message}));
  const box=$('#paResult');
  if(box)box.textContent=JSON.stringify(r.result||r,null,2);
  if(r.ok===false)toast(r.message||'调用失败','err');
  else toast('调用成功','ok');
}

async function platformCheckin(pid){
  toast('正在签到…');
  const r=await api('/api/platform?action=checkin',{platform:pid})
    .catch(e=>({ok:false,message:e.message}));
  const box=$('#paResult');
  if(box)box.textContent=JSON.stringify(r.result||r,null,2);
  if(r.ok===false)toast(r.message||'签到失败','err');
  else toast('签到完成','ok');
}

async function platformProbe(pid){
  toast('正在扫描 '+pid+' 的真实端点…（整站鉴权的平台需要先存凭据）');
  const r=await api('/api/platform?action=probe',{platform:pid})
    .catch(e=>({ok:false,message:e.message}));
  S.data.probeResult=r;
  if(r.ok===false){
    toast(r.message||'探测失败','err');
  }else{
    toast('扫 '+r.scanned+' 条，命中 '+(r.found||[]).length+' 个真实端点',
      (r.found||[]).length?'ok':'');
  }
  render();
}

function viewProbe(){
  const r=S.data.probeResult;
  if(!r)return '';
  let h='<div class="card"><div class="ch"><b>端点探测结果</b>'
    +'<span class="faint">'+esc(r.platform||'')+' · '+esc(r.base||'')+'</span></div><div class="cb">';
  if(r.ok===false){
    h+='<div class="note warn">'+esc(r.message||'探测失败')+'</div>';
  }else{
    h+='<div class="note'+(r.gated?' warn':'')+'">'+esc(r.note||'')+'</div>';
    h+='<div class="muted" style="font-size:12.5px;margin-bottom:8px">'
      +'基准请求（故意用不存在的路径）：<code>'+esc(r.baseline.path)+'</code> → HTTP '
      +r.baseline.code+(r.gated?' <b>（整站鉴权，404 探测法在此站失效）</b>':'')+'</div>';
    if((r.found||[]).length){
      h+=dataTable({
        key:'probe', rows:r.found, size:10,
        searchHint:'搜索探测到的路径…', searchKeys:['path','body'],
        cols:[
          {k:'path', t:'路径', render:x=>'<span class="mono" style="font-size:12px">'+esc(x.path)+'</span>'},
          {k:'method', t:'方法', render:x=>tag(x.method,'info')},
          {k:'code', t:'状态', num:true, render:x=>tag(String(x.code),
              x.code===200?'ok':x.code<500?'warn':'err')},
          {k:'body', t:'响应', render:x=>'<span class="mono faint" style="font-size:11px">'
              +esc(String(x.body||'').replace(/\s+/g,' ').slice(0,110))+'</span>'},
        ]});
      h+='<div class="note" style="margin-top:12px">'
        +'这些路径可以直接用「平台内部能力 → 调用」执行；也可以把它们加进 '
        +'<code>gwextra.ACTIONS</code> 变成固定动作。</div>';
    }else{
      h+=empty('没扫到与基准不同的路径。可能该站没有公开接口，'
        +'或候选表需要按抓包结果补充（改 app/gwextra.py 的 PROBE_CANDIDATES）');
    }
  }
  h+='</div></div>';
  return h;
}

/* ================================================================ 工具调用
 * 完整跑通 OpenAI 的 tools 协议往返：
 *   填问题 → 发请求 → 上游返回 tool_calls → 本地执行 → 回传 role=tool
 *   → 模型根据结果继续回答。
 */
const TC = { messages: [], enabled: {}, result: null, execLog: [] };

/* ================================================================ 定时自动签到 */
function loadAutocheckin(){
  api('/api/autocheckin?action=status').then(r=>{
    S.data.autoCheckin=r;
    render();
  }).catch(e=>toast('加载自动签到配置失败：'+e.message,'err'));
}

async function acSave(){
  const st=S.data.autoCheckin||{};
  const pls=(st.platforms||[]).map(p=>({
    platform:p.platform,
    on:!!(p._on!==undefined?p._on:p.on),
    time:(p._time!==undefined?p._time:p.time),
  }));
  const b={
    enabled:!!(st._enabled!==undefined?st._enabled:st.enabled),
    stagger_sec:st.stagger_sec, retry_times:st.retry_times,
    retry_delay_min:st.retry_delay_min, notify:!!st.notify,
    platforms:pls,
  };
  const r=await api('/api/autocheckin?action=save',b).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'保存失败','err');return;}
  S.data.autoCheckin=r;
  toast('已保存','ok'); render();
}

async function acRun(force){
  toast(force?'立即执行全部平台（忽略今天已跑）':'开始执行一轮…');
  const r=await api('/api/autocheckin?action=run',{force:!!force})
    .catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'执行失败','err');return;}
  toast('已触发，结果稍后刷新','ok');
  setTimeout(loadAutocheckin,3000);
  setTimeout(loadAutocheckin,12000);
}

function acToggle(p,on){
  p._on=on;
}
function acTime(p,v){
  p._time=v;
}

function viewAutocheckin(){
  const st=S.data.autoCheckin||{};
  const pls=st.platforms||[];
  const on=st._enabled!==undefined?st._enabled:st.enabled;
  const last=st.last_result;
  const thr=st.thread_alive;

  let h='';
  h+='<div class="grid g4 mb">'
    +'<div class="kpi"><div class="lb">自动签到</div>'
    +'<div class="vl" style="color:'+(on?'var(--ok)':'var(--faint)')+'">'+(on?'已启用':'已停用')+'</div>'
    +'<div class="ex">线程 '+(thr?'运行中':'未运行')+(st.running?' · 正在执行':'')+'</div></div>'
    +'<div class="kpi"><div class="lb">覆盖平台</div>'
    +'<div class="vl">'+pls.filter(p=>(p._on!==undefined?p._on:p.on)).length+' / '+pls.length+'</div>'
    +'<div class="ex">已登录 '+pls.filter(p=>p.logged_in).length+' 个</div></div>'
    +'<div class="kpi"><div class="lb">公开签到接口</div>'
    +'<div class="vl">'+pls.filter(p=>p.has_public_checkin).length+'</div>'
    +'<div class="ex">其余需先「探测端点」</div></div>'
    +'<div class="kpi"><div class="lb">错峰 / 重试</div>'
    +'<div class="vl">'+esc(st.stagger_sec)+'s / '+esc(st.retry_times)+' 次</div>'
    +'<div class="ex">重试间隔 '+esc(st.retry_delay_min)+' 分钟</div></div>'
    +'</div>';

  if(!thr){
    h+='<div class="note warn">自动签到线程未运行（面板刚启动时会静默观察 50 秒，'
      +'之后进入循环）。若长时间不出现，重启面板。</div>';
  }
  if(last){
    const okN=(last.results||[]).filter(x=>x.ok).length;
    const skN=(last.results||[]).filter(x=>x.skipped).length;
    const flN=(last.results||[]).filter(x=>!x.ok&&!x.skipped).length;
    h+='<div class="card"><div class="ch"><b>最近一次执行</b>'
      +'<span class="faint">'+esc(last.at||'')+'</span></div><div class="cb">'
      +'<div class="flex mb">'+tag('成功 '+okN,'ok')+tag('跳过 '+skN,'')+tag('失败 '+flN,flN?'err':'ok')+'</div>'
      +dataTable({
        key:'acRes', rows:last.results||[], size:20,
        searchHint:'搜索平台 / 结果…', searchKeys:['name','message'],
        cols:[
          {k:'name',t:'平台'},
          {k:'ok',t:'结果',render:x=>x.ok?tag('成功','ok'):x.skipped?tag('跳过',''):tag('失败','err')},
          {k:'retries',t:'重试',num:true,render:x=>x.retries||0},
          {k:'message',t:'说明',render:x=>'<span class="muted" style="font-size:12px">'+esc(x.message||'')+'</span>'},
          {k:'at',t:'时间',render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.at||'')+'</span>'},
        ]})
      +'</div></div>';
  }

  // 配置
  h+='<div class="card"><div class="ch"><b>签到计划</b>'
    +'<div class="flex n">'
    +'<label style="margin:0;display:flex;align-items:center;gap:6px">'
    +'<input type="checkbox" style="width:auto" '+(on?'checked':'')
    +' onchange="S.data.autoCheckin._enabled=this.checked">启用定时</label>'
    +'<label style="margin:0;display:flex;align-items:center;gap:6px">'
    +'<input type="checkbox" style="width:auto" '+(st.notify?'checked':'')
    +' onchange="S.data.autoCheckin.notify=this.checked">完成后通知</label>'
    +'<input type="number" value="'+esc(st.stagger_sec)+'" style="width:88px" '
    +'onchange="S.data.autoCheckin.stagger_sec=+this.value" title="平台间错峰秒数">'
    +'<input type="number" value="'+esc(st.retry_times)+'" style="width:88px" '
    +'onchange="S.data.autoCheckin.retry_times=+this.value" title="失败重试次数">'
    +'<input type="number" value="'+esc(st.retry_delay_min)+'" style="width:110px" '
    +'onchange="S.data.autoCheckin.retry_delay_min=+this.value" title="重试间隔分钟">'
    +'</div></div><div class="cb">';

  h+=dataTable({
    key:'acPlan', rows:pls, size:20,
    searchHint:'搜索平台…', searchKeys:['name','hint'],
    cols:[
      {k:'name',t:'平台',render:x=>'<b>'+esc(x.name)+'</b><div class="faint" style="font-size:11px">'
        +esc(x.platform)+'</div>'},
      {k:'_on',t:'启用',render:x=>{
        const v=x._on!==undefined?x._on:x.on;
        return '<input type="checkbox" style="width:auto" '+(v?'checked':'')
          +' onchange="acToggle(this.closest(\'tr\'),this.checked)">';
      }},
      {k:'_time',t:'每日时间',render:x=>{
        const v=x._time!==undefined?x._time:x.time;
        return '<input type="time" style="width:120px" value="'+esc(v)+'" '
          +'onchange="acTime(this.closest(\'tr\'),this.value)">';
      }},
      {k:'mode',t:'方式',render:x=>tag(
        {flow:'先查后领',direct:'直接领',bonus:'登录送',none:'需探测'}[x.mode]||x.mode,
        x.mode==='none'?'':'acc')},
      {k:'logged_in',t:'凭据',render:x=>x.logged_in?tag('已登录','ok'):tag('未登录','warn')},
      {k:'done_today',t:'今日',render:x=>x.done_today?tag('已跑','ok'):'—'},
      {k:'hint',t:'说明',render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.hint||'')+'</span>'},
      {k:'_act',t:'',render:x=>'<button class="btn sm" onclick="acRunOne(\''+esc(x.platform)+'\')">单独执行</button>'},
    ]});

  h+='<div class="flex mt">'
    +'<button class="btn pri" onclick="acSave()">保存配置</button>'
    +'<button class="btn" onclick="acRun(false)">执行一轮</button>'
    +'<button class="btn" onclick="acRun(true)">强制执行全部</button>'
    +'<span class="faint" style="font-size:12px">当前时间 '+esc(st.now||'')+'</span>'
    +'</div>';
  h+='<div class="muted mt" style="font-size:12px">'
    +'每个平台到点后跑一次，同一天同一平台只跑一次；失败会按配置重试。'
    +'「需探测」的平台表示公开接口里没找到签到端点 —— 先在「账号登录 → 平台内部能力」'
    +'里点「🔍 探测端点」扫出真实路径，我再帮你写进 '
    +'<code>gwextra.ACTIONS</code>。'
    +'</div>';
  h+='</div></div>';

  return h;
}

async function acRunOne(platform){
  toast('正在执行 '+platform+' …');
  const r=await api('/api/autocheckin?action=run',{platforms:[platform],force:true})
    .catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'失败','err');return;}
  toast('已触发','ok');
  setTimeout(loadAutocheckin,4000);
}

function loadToolcall(){
  api('/api/toolcall?action=list').then(r=>{
    S.data.toolList=r.tools||[];
    S.data.toolNames=r.names||[];
    if(!Object.keys(TC.enabled).length){
      for(const n of S.data.toolNames) TC.enabled[n]=true;
    }
    render();
  }).catch(e=>toast('加载工具失败：'+e.message,'err'));
}

async function tcSend(){
  const q=$('#tcQuery');
  const text=(q&&q.value||'').trim();
  if(!text){toast('先写一句问题','err');return;}
  const names=Object.keys(TC.enabled).filter(k=>TC.enabled[k]);
  if(!names.length){toast('至少启用一个工具','err');return;}
  TC.messages.push({role:'user',content:text});
  if(q)q.value='';
  TC.execLog=[]; TC.result=null;
  render();
  toast('正在请求（带 '+names.length+' 个工具）…');
  const r=await api('/api/toolcall?action=chat',{
    messages:TC.messages, names:names,
    tool_choice:($('#tcChoice')||{}).value||''
  }).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'请求失败','err');TC.result={error:r.message};render();return;}
  TC.result=r;
  const msg={role:'assistant',content:r.content||''};
  if((r.tool_calls||[]).length)msg.tool_calls=r.tool_calls;
  TC.messages.push(msg);
  render();
  if((r.tool_calls||[]).length)toast('模型请求调用 '+(r.tool_calls||[]).length+' 个工具','ok');
  else toast(r.content?'模型直接回答了':'无返回内容', r.content?'ok':'');
}

async function tcExec(){
  const tcs=(TC.result&&TC.result.tool_calls)||[];
  if(!tcs.length){toast('没有待执行的 tool_calls','err');return;}
  toast('本地执行 '+tcs.length+' 个工具…');
  const r=await api('/api/toolcall?action=exec',{tool_calls:tcs})
    .catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'执行失败','err');return;}
  TC.execLog=r.results||[];
  for(const m of (r.tool_messages||[])) TC.messages.push(m);
  render();
  toast('已执行 '+TC.execLog.length+' 个，结果已回传', TC.execLog.every(x=>x.ok)?'ok':'err');
}

async function tcContinue(){
  if(!TC.messages.length){toast('先发一次请求','err');return;}
  const names=Object.keys(TC.enabled).filter(k=>TC.enabled[k]);
  toast('把工具结果回传给模型…');
  const r=await api('/api/toolcall?action=chat',{
    messages:TC.messages, names:names
  }).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'失败','err');return;}
  TC.result=r;
  TC.messages.push({role:'assistant',content:r.content||''});
  render();
  toast('模型已根据工具结果作答','ok');
}

function tcReset(){
  TC.messages=[];TC.result=null;TC.execLog=[];
  render();toast('已清空');
}

function viewToolcall(){
  const names=S.data.toolNames||[];
  const tools=S.data.toolList||[];
  const byId={}; for(const t of tools) byId[t.function.name]=t.function;
  const live=((S.data.overview||{}).gateway||{}).alive;

  let h='';
  if(!live){
    h+='<div class="note warn">网关未运行。工具调用需要上游模型配合'
      +'（先在「网关与账号」里登录账号），面板只能负责「执行工具」这一半。</div>';
  }

  // ---- 工具清单 ----
  h+='<div class="card"><div class="ch"><b>可用工具（'+names.length+'）</b>'
    +'<span class="faint">勾选后随请求一起发给模型</span></div><div class="cb">';
  h+='<div class="grid g3">';
  for(const n of names){
    const f=byId[n]||{};
    const props=Object.keys((f.parameters||{}).properties||{});
    h+='<label style="border:1px solid var(--line);border-radius:10px;padding:10px;margin:0;cursor:pointer">'
      +'<div style="display:flex;align-items:center;gap:8px">'
      +'<input type="checkbox" style="width:auto;flex:0 0 auto" '+(TC.enabled[n]?'checked':'')
      +' onchange="tcEnabled(\''+esc(n)+'\',this.checked)">'
      +'<b class="mono" style="font-size:12.5px">'+esc(n)+'</b></div>'
      +'<div class="faint" style="font-size:11.5px;margin:6px 0 4px;line-height:1.5">'+esc(f.description||'')+'</div>'
      +(props.length?'<div class="faint mono" style="font-size:10.5px">参数：'+esc(props.join(', '))+'</div>':'')
      +'</label>';
  }
  h+='</div>';
  h+='<div class="muted mt" style="font-size:12px">'
    +'沙箱目录：<code>'+esc(S.data.toolSandbox||'data/tools_sandbox')+'</code>'
    +'（文件工具只能读写这里，防目录穿越）。'
    +'<code>http_get</code> 默认关闭，<code>write_file</code> 需在设置里开写入权限。</div>';
  h+='</div></div>';

  // ---- 对话 ----
  h+='<div class="card"><div class="ch"><b>工具调用测试台</b>'
    +'<div class="flex n">'
    +'<select id="tcChoice" style="width:auto"><option value="">tool_choice: auto</option>'
    +'<option value="required">required（强制调工具）</option></select>'
    +'<button class="btn sm" onclick="tcReset()">清空</button>'
    +'</div></div><div class="cb">';

  h+='<div class="scroll" style="max-height:300px;margin-bottom:10px">';
  if(!TC.messages.length){
    h+=empty('还没有对话。写一句需要用工具的话，比如「现在几点？帮我算 (12+8)*3」');
  }else{
    for(const m of TC.messages){
      if(m.role==='user'){
        h+='<div style="margin-bottom:8px"><span class="tag acc">user</span> <span>'+esc(m.content)+'</span></div>';
      }else if(m.role==='assistant'){
        h+='<div style="margin-bottom:8px"><span class="tag ok">assistant</span> <span>'+esc(m.content||'(空)')+'</span></div>';
      }else if(m.role==='tool'){
        h+='<div style="margin-bottom:8px"><span class="tag info">tool</span> '
          +'<code style="word-break:break-all">'+esc(m.content)+'</code></div>';
      }
    }
  }
  h+='</div>';

  h+='<div class="row n" style="gap:8px">'
    +'<input id="tcQuery" placeholder="问一句需要用工具的问题…" '
    +'onkeydown="if(event.key===\'Enter\')tcSend()" style="flex:1">'
    +'<button class="btn pri" onclick="tcSend()">发送</button></div>';

  // tool_calls
  const r=TC.result;
  if(r&&r.error){
    h+='<div class="note err mt">请求失败：'+esc(r.error)+'</div>';
  }else if(r&&(r.tool_calls||[]).length){
    h+='<div class="card" style="margin:12px 0 0;box-shadow:none;background:var(--panel-2)">'
      +'<div class="ch"><b>模型请求调用 '+r.tool_calls.length+' 个工具</b>'
      +'<span class="faint">finish_reason='+esc(r.finish_reason||'—')+'</span></div>'
      +'<div class="cb">';
    for(const tc of r.tool_calls){
      const fn=tc.function||{};
      h+='<div style="border:1px solid var(--line);border-radius:8px;padding:9px;margin-bottom:7px">'
        +'<div><b class="mono" style="font-size:12.5px">'+esc(fn.name)+'</b> '
        +'<span class="faint mono" style="font-size:11px">id='+esc(tc.id||'—')+'</span></div>'
        +'<pre style="margin-top:6px;max-height:120px">'+esc(fn.arguments||'{}')+'</pre></div>';
    }
    const done=TC.execLog.length;
    h+='<div class="flex n">'
      +'<button class="btn pri" onclick="tcExec()">'
      +(done?'重新执行 '+TC.execLog.length+' 个':'执行这 '+r.tool_calls.length+' 个工具')+'</button>'
      +(done?'<button class="btn" onclick="tcContinue()">把结果回传，让模型继续回答</button>':'')
      +'</div>';
    // 执行结果
    if(TC.execLog.length){
      h+='<div class="mt">'+dataTable({
        key:'tcExec', rows:TC.execLog, size:10,
        searchHint:'搜索执行结果…', searchKeys:['name','result','args'],
        cols:[
          {k:'name',t:'工具',render:x=>'<span class="mono" style="font-size:12px">'+esc(x.name)+'</span>'},
          {k:'ok',t:'状态',render:x=>x.ok?tag('OK','ok'):tag('ERR','err')},
          {k:'args',t:'入参',render:x=>'<code style="font-size:11px;word-break:break-all">'+esc(String(x.args||''))+'</code>'},
          {k:'result',t:'结果',render:x=>'<code style="font-size:11px;word-break:break-all;color:'+(x.ok?'var(--ok)':'var(--err)')+'">'+esc(String(x.result||''))+'</code>'},
        ]})+'</div>';
    }
    h+='</div></div>';
  }else if(r&&r.content){
    h+='<div class="note mt">模型直接回答（未调用工具）：<br>'+esc(r.content)+'</div>';
  }
  h+='</div></div>';

  // ---- 协议说明 ----
  h+='<div class="card"><h2>接入你自己的客户端</h2>'
    +'<div class="note">面板本身也是一个 OpenAI 兼容端点。工具调用标准流程：'
    +'① 请求带 <code>tools</code> → ② 响应 <code>message.tool_calls</code> → '
    +'③ 执行函数 → ④ 以 <code>role:"tool"</code> 回传 → 模型继续回答。</div>'
    +'<pre>'+esc([
'POST http://127.0.0.1:<面板端口>/v1/chat/completions',
'{"model":"default",',
' "messages":[{"role":"user","content":"现在几点？"}],',
' "tools":[{"type":"function","function":{',
'   "name":"get_current_time",',
'   "description":"获取当前时间",',
'   "parameters":{"type":"object","properties":{}}}}],',
' "tool_choice":"auto"}',
'',
'# 响应里会有：',
'#   choices[0].message.tool_calls[] = {id, type, function:{name, arguments}}',
'#   arguments 是 JSON 字符串，执行后按 role:"tool" + tool_call_id 回传。'
].join('\n'))+'</pre></div>';

  return h;
}

function tcEnabled(name, on){
  TC.enabled[name]=on;
}

function viewLogin(){
  const pls=S.data.loginPlatforms||[];
  const sess=S.data.loginSession;
  const accs=S.data.accounts||[];
  const st=S.data.accountStats||{};
  const acts=S.data.platformActions||{};

  let h='';

  // ---- 会话区 ----
  if(sess){
    h+='<div class="card" style="margin-bottom:14px"><div class="ch"><b>登录会话</b>'
      +'<span id="lsStatus">'+loginStatusTag(sess)+'</span></div><div class="cb">';
    h+='<div class="row" style="align-items:flex-start">';
    h+='<div class="n" id="lsQr">';
    if(sess.qr)h+='<img src="'+sess.qr+'" alt="登录二维码" style="width:196px;height:196px;border-radius:10px;border:1px solid var(--line)">';
    h+='</div>';
    h+='<div style="flex:1;min-width:220px">';
    h+='<div style="font-weight:600;margin-bottom:6px">'+esc(sess.platform_name||sess.platform)+'</div>';
    h+='<div class="faint" id="lsMsg" style="margin-bottom:4px">'+esc(sess.message||sess.error||'')+'</div>';
    h+='<div class="faint" id="lsLeft">'+(sess.seconds_left?('剩余 '+sess.seconds_left+' 秒'):'')+'</div>';
    h+='<div class="row n" style="margin-top:10px;gap:8px">';
    if(sess.auth_url){
      h+='<a class="btn pri n" href="'+esc(sess.auth_url)+'" target="_blank" rel="noreferrer">打开授权页</a>';
    }
    if(sess.method==='cookie'){
      h+='<button class="btn n" onclick="pollCookieOnce()">我已登录，立即获取</button>';
    }
    h+='<button class="btn dgr n" onclick="cancelLogin()">取消</button>';
    h+='</div></div></div>';
    if(sess.method==='cookie'){
      const dg=S.data.browserDiag;
      let tip='自动获取会被浏览器加密策略挡住时，可直接粘贴 Cookie'
        +'（浏览器 F12 → Network → 复制 Request Headers 里的 cookie）';
      if(dg){
        tip=(dg.auto_supported
          ? '本机浏览器可直接读取 Cookie（'+esc(dg.reason)+'）'
          : '⚠ '+esc(dg.reason));
      }
      h+='<div style="margin-top:12px"><div class="faint" style="margin-bottom:5px">'+tip+'</div>'
        +'<textarea id="manualCookie" rows="3" placeholder="粘贴 Cookie，例如 sessionid=xxx; passport_csrf_token=yyy" '
        +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;font-family:monospace;font-size:12px"></textarea>'
        +'<button class="btn pri sm" style="margin-top:6px" onclick="submitManualCookie()">提交 Cookie</button></div>';
    }
    h+='</div></div>';
  }

  // ---- 平台列表 ----
  h+='<div class="card" style="margin-bottom:14px"><div class="ch"><b>可登录的平台 / 网关</b>'
    +'<span class="faint">'+pls.length+' 个</span></div><div class="cb"><div class="grid g3">';
  if(!pls.length)h+=empty('加载中…');
  for(const p of pls){
    const mm={qrcode:'二维码',cookie:'Cookie',file:'文件/Key'};
    h+='<div style="border:1px solid var(--line);border-radius:10px;padding:12px">';
    h+='<div style="display:flex;justify-content:space-between;align-items:center;gap:8px">';
    h+='<b style="font-size:13.5px">'+esc(p.name)+'</b>'+tag(mm[p.method]||p.method,'acc');
    h+='</div>';
    h+='<div class="faint" style="margin:6px 0 8px;font-size:12px;min-height:32px">'+esc(p.hint||'')+'</div>';
    if(p.upstream)h+='<div class="faint" style="font-size:11.5px">上游 '+esc(p.upstream)+'</div>';
    h+='<button class="btn pri sm" style="margin-top:8px;width:100%" onclick="startLogin(\''+esc(p.id)+'\',\''+esc(p.edition||'')+'\')">'
      +(p.method==='file'?'填写凭据':'开始登录')+'</button>';
    h+='</div>';
  }
  h+='</div></div></div>';

  // ---- 账号池 ----
  h+='<div class="card" style="margin-bottom:14px"><div class="ch"><b>已保存的账号凭据</b>'
    +'<span class="faint">共 '+esc(st.total||0)+' 条 · 有凭据 '+esc(st.with_secret||0)+'</span></div><div class="cb">';
  if(!accs.length){
    h+=empty('还没有账号。在上面点「开始登录」，扫码或粘贴 Cookie 即可。');
  }else{
    h+='<div class="scroll"><table><thead><tr><th>平台</th><th>名称</th><th>类型</th>'
      +'<th>来源</th><th>密钥</th><th>获取时间</th><th>状态</th><th></th></tr></thead><tbody>';
    for(const a of accs){
      h+='<tr><td>'+esc(a.platform)+'</td><td>'+esc(a.name)+'</td><td>'+esc(a.type)+'</td>'
        +'<td>'+esc(a.source||'')+'</td>'
        +'<td class="mono" title="长度 '+esc(a.secret_len||0)+'">'+esc(a.secret||'')+'</td>'
        +'<td class="faint">'+esc(a.obtained_at||'')+'</td>'
        +'<td>'+(a.enabled===false?tag('已停用','warn'):tag('启用','ok'))+'</td>'
        +'<td><button class="btn sm" onclick="toggleAccount(\''+esc(a.id)+'\','+(a.enabled===false?'true':'false')+')">'
        +(a.enabled===false?'启用':'停用')+'</button> '
        +'<button class="btn sm dgr" onclick="delAccount(\''+esc(a.id)+'\')">删除</button></td></tr>';
    }
    h+='</tbody></table></div>';
  }
  h+='</div></div>';

  // ---- 平台能力 ----
  const pids=Object.keys(acts).filter(k=>Array.isArray(acts[k])&&acts[k].length);
  h+='<div class="card"><div class="ch"><b>平台内部能力</b>'
    +'<span class="faint">签到 / 任务 / 积分 / 兑换 / 送积分</span></div><div class="cb">';
  h+=viewProbe();
  if(!pids.length){
    h+=empty('暂无');
  }else{
    for(const pid of pids){
      h+='<div style="margin-bottom:14px"><div style="font-weight:600;margin-bottom:6px;display:flex;align-items:center;gap:8px">'
        +esc(pid)
        +'<button class="btn sm n" title="登录凭据后动态扫描该平台的真实端点（应对整站 401）" '
        +'onclick="platformProbe(\''+esc(pid)+'\')">🔍 探测端点</button></div>';
      h+='<div class="row" style="gap:6px;align-items:flex-start">';
      for(const a of acts[pid]){
        // local = APP 本地网关路由（公网不可达）
        // needs_proxy = Google 系，国内直连不通，需要走代理
        const isLocal=!!a.local, isProxy=!!a.needs_proxy;
        const title=isLocal?('APP 本地网关路由，面板无法直接调用（'+esc(a.path)+'）')
          :(isProxy?('需代理：'+esc(a.path))
                           :(esc(a.method||'GET')+' '+esc(a.path)));
        if(isLocal){
          h+='<button class="btn sm n" disabled title="'+title+'" '
            +'style="opacity:.45;cursor:not-allowed">'+esc(a.name)+'（本地）</button>';
        }else if(a.id==='checkin_claim'||a.id==='login_bonus'){
          h+='<button class="btn pri sm n" title="'+title+'" onclick="platformCheckin(\''+esc(pid)+'\')">'
            +esc(a.name)+' ⚡</button>';
        }else{
          h+='<button class="btn sm n"'+(isProxy?' style="border-color:var(--warn)"':'')
            +' title="'+title+'" onclick="runPlatformAction(\''+esc(pid)+'\',\''+esc(a.id)+'\','
            +JSON.stringify(a.need||[]).replace(/"/g,'&quot;')+')">'+esc(a.name)
            +(isProxy?' 🌍':'')+'</button>';
        }
      }
      h+='</div>';
      const need=acts[pid].filter(a=>a.need&&a.need.length);
      if(need.length){
        const keys=[];
        for(const a of need)for(const k of a.need)if(keys.indexOf(k)<0)keys.push(k);
        h+='<div class="row" style="margin-top:6px">';
        for(const k of keys){
          h+='<input id="pa_'+esc(k)+'" placeholder="'+esc(k)+'" '
            +'style="padding:5px 8px;border:1px solid var(--line2);border-radius:7px;font-size:12px">';
        }
        h+='</div>';
      }
      h+='</div>';
    }
    h+='<div class="faint" style="margin-bottom:4px">调用结果</div>';
    h+='<pre id="paResult" style="max-height:260px;overflow:auto;background:var(--bg);'
      +'border:1px solid var(--line);border-radius:8px;padding:10px;font-size:12px;margin:0">'
      +'（点击上方按钮执行）</pre>';
  }
  h+='</div></div>';
  return h;
}

const VIEWS={dash:viewDash,gateway:viewGateway,login:viewLogin,toolcall:viewToolcall,autocheckin:viewAutocheckin,checkin:viewCheckin,growth:viewGrowth,
  tasks:viewTasks,usage:viewUsage,models:viewModels,sites:viewSites,catalog:viewCatalog,
  route:viewRoute,upstreams:viewUpstreams,
  notify:viewNotify,logs:viewLogs,settings:viewSettings};

const LOADERS={gateway:loadGateway,login:loadLogin,toolcall:loadToolcall,autocheckin:loadAutocheckin,checkin:loadCheckin,growth:loadGrowth,tasks:loadTasks,
  usage:loadUsage,models:loadModels,sites:loadSites,catalog:loadCatalog,
  route:loadRoute,upstreams:loadUpstreams,notify:loadNotify,logs:loadLogs,
  settings:loadSettings};

function loadExtra(){
  const f=LOADERS[S.v];
  if(f)f();
}

window.addEventListener('DOMContentLoaded',()=>{
  // 主题（先应用，避免闪白）
  let th='light';
  try{ th=localStorage.getItem('aigw.theme')
        || (window.matchMedia&&window.matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light'); }catch(e){}
  applyTheme(th);
  dtBindOnce();

  const h=(location.hash||'').replace('#','');
  refresh().then(()=>{
    // 预取签到中心依赖的站点列表
    api('/api/sites').then(r=>{S.data.sites=r.sites||[];S.data.catalogSites=r.catalog||[];});
    if(TITLES[h]){
      const b=$(`.navbtn[data-v="${h}"]`);
      if(b)b.classList.add('on');
      switchView(h);
    }
  });
  // 后台轮询总览
  setInterval(()=>{
    if(document.hidden)return;
    api('/api/overview').then(o=>{S.data.overview=o;$('#stl').textContent=
      '网关 '+(o.gateway&&o.gateway.alive?'运行中':'未运行')+' · '+o.gateway.addr+
      ' · 面板已运行 '+dur(o.uptime);}).catch(()=>{});
  },15000);
});
