/* AI 资源整合网关面板 —— 前端逻辑 */
'use strict';

const S = { v:'sources', data:{}, busy:false, loaded:'' };

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
const TITLES={sources:'接入源',route:'路由与模型',tools:'工具与集成',settings:'设置与日志',
  checkin:'签到记录',growth:'成长任务',tasks:'任务管理'};
/* 高级（二级）菜单里的视图：默认折叠，进入其中任一项时自动展开分组 */
const ADV=['tools','settings','checkin','growth','tasks'];
function toggleAdv(){
  const sub=$('#advSub'), t=$('#advToggle');
  const open=sub.classList.toggle('open');
  t.classList.toggle('open',open);
}
function openAdv(){
  $('#advSub').classList.add('open');
  $('#advToggle').classList.add('open');
}

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
    $('#stl').textContent='面板原生模式 · 面板已运行 '+dur(o.uptime)+' · 调度 '+((o.scheduler&&o.scheduler.ticks)||0)+' 次';
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

function render(){
  const f=VIEWS[S.v]||VIEWS.sources;
  $('#view').innerHTML=f();
  const box=$('#oneClickBox');
  if(box)box.innerHTML=viewOneClick();
  if(S.v==='sources'||S.v==='tools')paintLoginSession();
}
async function switchView(v,force){
  S.v=v;
  location.hash=v;
  $('#ttl').textContent=TITLES[v]||v;
  // 同步侧边栏高亮（含折叠分组内的子项）
  $$('.navbtn').forEach(x=>x.classList.remove('on'));
  const b=$(`.navbtn[data-v="${v}"]`);
  if(b)b.classList.add('on');
  if(ADV.indexOf(v)>=0)openAdv();
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
/* ================================================================
 * 接入源 —— 面板首页
 *
 * 用户心智只有一条主线：把各种 AI 接进来 → 统一反代 → 自动路由。
 * 所以首页不是「功能仪表盘」，而是一面**源卡片墙**：
 * 一眼看清「我接了哪些源 / 每个源能干什么 / 现在能不能用」。
 * 签到、模型、探测都是卡片上的操作，不占一级入口。
 * ================================================================ */
let SRC_VIEW = 'card';        // card | detail

function loadSources(){
  Promise.all([
    api('/api/login?action=platforms').catch(()=>({platforms:[]})),
    api('/api/accounts').catch(()=>({accounts:[],stats:{}})),
    api('/api/autocheckin?action=status').catch(()=>({platforms:[]})),
    api('/api/models?action=sources').catch(()=>({sources:[]})),
    api('/api/models').catch(()=>({models_enriched:[],rate_summary:{}})),
    api('/api/overview').catch(()=>({})),
  ]).then(([lp,ac,au,src,md,ov])=>{
    S.data.loginPlatforms=lp.platforms||[];
    S.data.accounts=ac.accounts||[];
    S.data.accountStats=ac.stats||{};
    S.data.autoCheckin=au;
    S.data.catalogSources=src.sources||[];
    S.data.models=md;
    S.data.overview=ov;
    S.data.srcKinds=src.platform_kinds||{};
    S.data.srcCats=src.categories||[];
    S.data.srcSummary=src.summary||{};
    render();
  }).catch(e=>toast('加载接入源失败：'+e.message,'err'));
}

/** 把「可登录平台 + 可拉目录平台 + 网关本地模型」合成一张源清单 */
function sourceList(){
  const accs=S.data.accounts||[];
  const logged=new Set(accs.map(a=>a.platform));
  const au=S.data.autoCheckin||{};
  const plan={};
  for(const p of (au.platforms||[]))plan[p.platform]=p;
  const canModels=new Set((S.data.catalogSources||[]).map(x=>x.platform));
  const rows=(S.data.models||{}).models_enriched||[];
  const withRate=rows.filter(x=>x.credits).length;

  const out=[];
  // ★ 本地网关 EXE 卡已彻底移除（2026-10-05 用户多次要求）：
  //   登录/倍率/签到/模型/多账号池轮询/429 冷却/v1/messages/用量/通知
  //   全部面板原生化，EXE 不再是任何环节的依赖。
  // 1) 本地反代（CLIProxyAPI）
  const lp=S.data.lp||{};
  out.push({
    key:'localproxy', name:'本地反代（CLIProxyAPI）', kind:'本地反代',
    desc:'Kimi / Codex / Claude / Antigravity / Grok / Devin / Meta 的 CLI 订阅反代',
    logged:!!lp.online, endpoint:lp.base_url||'http://127.0.0.1:8318/v1',
    models:lp.models||0, withRate:0, checkin:false,
    canModels:true, local:true, view:'localproxy',
  });
  // 3) 可登录的平台 —— copilot 系四张卡合并成一张（同一套腾讯接口）
  const COPILOT_IDS={'wb-gateway':'国内站','wb-gateway-intl':'国际站',
                     'apk-codebuddy':'CodeBuddy 国际','apk-codebuddy-cn':'CodeBuddy 国内'};
  // APK 本地网关（手机上监听 0.0.0.0，面板走局域网反代）
  const LAN_APK={'apk-trae':'Trae aigw.app','apk-doubao':'dev.doubao2api',
                 'apk-yuanbao':'dev.yuanbao2api','apk-raccoon':'dev.raccoon2api',
                 'apk-go':'wb2apimobile (Go)','apk-codebuddy':'workbuddy2api',
                 'apk-codebuddy-cn':'workbuddy2api'};
  let copilotDone=false;
  for(const p of (S.data.loginPlatforms||[])){
    if(COPILOT_IDS[p.id]){
      if(copilotDone)continue;
      copilotDone=true;
      const anyLogged=['wb-gateway','wb-gateway-intl','apk-codebuddy','apk-codebuddy-cn']
        .some(x=>logged.has(x));
      out.push({
        key:'copilot', name:'CodeBuddy / WorkBuddy 账号（copilot 直连）', kind:'账号接入',
        desc:'同一套接口，但国内/国际是两个独立账号（额度/积分互不互通），'
            +'分别扫码登录。二维码直接显示在面板里，不跳网页、不需要网关 EXE。',
        method:'qrcode', merged:true,
        variants:[
          {pid:'wb-gateway', label:'国内站账号', logged:logged.has('wb-gateway')},
          {pid:'wb-gateway-intl', label:'国际站账号', logged:logged.has('wb-gateway-intl')},
          {pid:'apk-codebuddy', label:'CodeBuddy 国际账号', logged:logged.has('apk-codebuddy')},
          {pid:'apk-codebuddy-cn', label:'CodeBuddy 国内账号', logged:logged.has('apk-codebuddy-cn')},
        ],
        logged:anyLogged, endpoint:'copilot.tencent.com',
        models:0, withRate:0, checkin:false, canModels:false,
      });
      continue;
    }
    const pl=plan[p.id];
    let desc=p.hint||'';
    if(LAN_APK[p.id]){
      desc+=' ▸ 局域网反代：手机与电脑同一 WiFi，在 App 里看监听端口，'
          +'到「路由与模型」添加 http://手机IP:端口/v1（Bearer API Key）。'
          +'面板据此把该平台模型接入统一端点与自动路由。';
    }
    out.push({
      key:p.id, name:p.name, kind:'账号接入',
      desc:desc, method:p.method,
      logged:logged.has(p.id), endpoint:p.upstream||'',
      models:0, withRate:0,
      checkin:!!(pl&&pl.has_public_checkin),
      at:pl?pl.time:'', canModels:canModels.has(p.id),
      on:pl?pl.on:true, done:pl?pl.done_today:false,
    });
  }
  return out;
}

/* ================================================================
 * 工具与集成 —— 可选增强，不占主流程
 * ================================================================ */
let TOOLS_TAB = 'call';

async function loadTools(){
  Promise.all([
    api('/api/localproxy').catch(()=>({})),
    api('/api/toolcall?action=list').catch(()=>({tools:[]})),
  ]).then(([lp,tc])=>{
    S.data.lp=lp; S.data.toolList=tc.tools||[]; S.data.toolNames=tc.names||[];
    render();
  }).catch(()=>{});
}

function viewTools(){
  let h='<div class="note">这些是<b>可选增强</b>。'
    +'不接也完全不影响「接入源 → 统一反代 → 自动路由」这条主线。</div>';
  h+='<div class="tabs">'
    +[['call','⚒ 工具调用'],['proxy','⇅ 本地反代'],['plan','⏰ 签到计划'],['acct','⎔ 账号与凭据']]
      .map(x=>'<button class="'+(TOOLS_TAB===x[0]?'on':'')+'" '
        +'onclick="setToolsTab(\''+x[0]+'\')">'+x[1]+'</button>').join('')
    +'</div>';
  if(TOOLS_TAB==='call') h+=viewToolcall();
  else if(TOOLS_TAB==='proxy') h+='<div class="card"><div class="cb">'+viewLocalproxy()+'</div></div>';
  else if(TOOLS_TAB==='plan') h+=viewCheckinPlan();
  else h+=viewAccountPool();
  return h;
}
function setToolsTab(k){ TOOLS_TAB=k; render(); }

function viewCheckinPlan(){
  const au=S.data.autoCheckin||{};
  const accP=au.platforms||[];
  const onN=accP.filter(p=>(p._on!==undefined?p._on:p.on)).length;
  let h='<div class="card"><div class="ch"><b>定时签到计划</b>'
    +'<div class="flex n">'
    +'<label style="margin:0;display:flex;align-items:center;gap:6px">'
    +'<input type="checkbox" style="width:auto" '+(au.enabled?'checked':'')
    +' onchange="acSet(\'enabled\',this.checked)">启用定时</label>'
    +'<label style="margin:0;display:flex;align-items:center;gap:6px">'
    +'<input type="checkbox" style="width:auto" '+(au.notify?'checked':'')
    +' onchange="acSet(\'notify\',this.checked)">完成后通知</label>'
    +'<input type="number" value="'+esc(au.stagger_sec)+'" style="width:88px" title="错峰秒数" onchange="acSet(\'stagger_sec\',+this.value)">'
    +'<input type="number" value="'+esc(au.retry_times)+'" style="width:88px" title="重试次数" onchange="acSet(\'retry_times\',+this.value)">'
    +'<input type="number" value="'+esc(au.retry_delay_min)+'" style="width:110px" title="重试间隔分钟" onchange="acSet(\'retry_delay_min\',+this.value)">'
    +'<button class="btn pri" onclick="acSave()">保存</button>'
    +'<button class="btn" onclick="acRun(false)">执行一轮</button>'
    +'<button class="btn" onclick="acRun(true)">强制全部</button>'
    +'</div></div><div class="cb">';
  h+=dataTable({
    key:'tPlan', rows:accP, size:20,
    searchHint:'搜索平台…', searchKeys:['name','hint'],
    cols:[
      {k:'name',t:'平台',render:x=>'<b>'+esc(x.name)+'</b>'},
      {k:'_on',t:'启用',render:x=>{
        const v=x._on!==undefined?x._on:x.on;
        return '<input type="checkbox" style="width:auto" '+(v?'checked':'')
          +' onchange="acToggle(this,\''+esc(x.platform)+'\',this.checked)">';
      }},
      {k:'_time',t:'每日时间',render:x=>{
        const v=x._time!==undefined?x._time:x.time;
        return '<input type="time" style="width:120px" value="'+esc(v)+'" '
          +'onchange="acTime(this,\''+esc(x.platform)+'\',this.value)">';
      }},
      {k:'logged_in',t:'凭据',render:x=>x.logged_in?tag('已登录','ok'):tag('未登录','warn')},
      {k:'done_today',t:'今日',render:x=>x.done_today?tag('已跑','ok'):'—'},
      {k:'_go',t:'',render:x=>'<button class="btn sm" onclick="acRunOne(\''+esc(x.platform)+'\')">执行</button>'},
    ]});
  h+='<div class="muted mt" style="font-size:12px">勾选 '+onN+' / '+accP.length
    +' 个平台 · 同一天每平台只跑一次</div></div></div>';
  return h;
}

function viewAccountPool(){
  const accs=S.data.accounts||[];
  let h='<div class="card"><div class="ch"><b>账号池（'+accs.length+'）</b>'
    +'<span class="faint">凭据只存本机 data/ 目录，不外传</span></div><div class="cb">';
  if(!accs.length){
    h+=empty('还没有账号。去「接入源」点卡片接入。');
  }else{
    h+=dataTable({
      key:'tAcc', rows:accs, size:20,
      searchHint:'搜索平台 / 名称…', searchKeys:['platform','name','source'],
      cols:[
        {k:'platform',t:'平台',render:x=>'<span class="mono" style="font-size:12px">'+esc(x.platform)+'</span>'},
        {k:'name',t:'名称'},
        {k:'source',t:'来源',render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.source||'')+'</span>'},
        {k:'secret',t:'凭据',render:x=>'<span class="mono faint">'+esc(x.secret||'')+'</span>'},
        {k:'obtained_at',t:'获取时间',render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.obtained_at||'')+'</span>'},
        {k:'_op',t:'',render:x=>'<button class="btn sm dgr" onclick="delAccount(\''+esc(x.id)+'\')">删除</button>'},
      ]});
  }
  h+='</div></div>';
  return h;
}

/** 路由与模型：合并了原来的「智能路由」「模型与倍率」「上游档案」「用量看板」 */
let ROUTE_TAB = 'models';

async function loadRoute(){
  // ★ grouped 先行并行拉（模型总表是本页主体），不被任何慢请求串行拖住
  api('/api/models?action=grouped').then(g=>{S.data.grouped=g;render();}).catch(()=>{});
  const [rt,us]=await Promise.all([
    api('/api/route').catch(()=>({})),
    api('/api/usage?range=all').catch(()=>({})),
  ]);
  S.data.route=rt; S.data.usage=us;
  render();
}

function viewRoute(){
  let h='<div class="tabs">'
    +[['models','⌗ 模型与倍率'],['auto','⇄ 自动路由'],
       ['usage','◔ 用量']]
      .map(x=>'<button class="'+(ROUTE_TAB===x[0]?'on':'')+'" '
        +'onclick="setRouteTab(\''+x[0]+'\')">'+x[1]+'</button>').join('')
    +'</div>';
  if(ROUTE_TAB==='models') h+=viewModels();
  else if(ROUTE_TAB==='auto') h+=viewRouteAuto();
  else h+=viewUsage();
  return h;
}
function setRouteTab(k){ ROUTE_TAB=k; render(); }
function viewRouteAuto(){ return viewRoute0(); }

/** 把源清单按 LOCAL / API / WEB 分组 */
let SRC_TAB = 'LOCAL';
function srcGroups(){
  const cls = S.data.srcKinds || {};
  const cats = S.data.srcCats || [];
  const out = {};
  for(const c of cats) out[c.kind] = {meta:c, items:[]};
  for(const s of sourceList()){
    const k = (cls[s.key] && cls[s.key].kind) ||
              (s.key==='copilot' ? 'LOCAL' :
               s.key==='localproxy' ? 'LOCAL' : 'WEB');
    (out[k] || (out.WEB = out.WEB || {meta:{kind:'WEB',name:'其它',icon:'·',desc:'',color:''},items:[]})).items.push(s);
  }
  return out;
}

function viewSources(){
  if(SRC_VIEW==='detail'&&S.data.srcDetail){
    return srcDetail();
  }
  const groups=srcGroups();
  const cats=S.data.srcCats||[];
  const o=S.data.overview||{};
  const g=o.gateway||{};
  const allN=Object.values(groups).reduce((n,x)=>n+x.items.length,0);
  const loggedAll=Object.values(groups).reduce(
    (n,x)=>n+x.items.filter(i=>i.logged).length,0);
  // 分类统计前端现算（后端已不再下发 summary）
  const srcItems=Object.values(groups).flatMap(x=>x.items);
  const nAcc=srcItems.filter(i=>i.kind==='账号接入').length;
  const nLp=srcItems.filter(i=>i.kind==='本地反代').length;

  let h='';
  // ---- 顶部：链路状态，一眼看清「接入 → 反代 → 路由」三个环节
  h+='<div class="grid g4 mb">'
    +'<div class="kpi"><div class="lb">① 接入源</div>'
    +'<div class="vl">'+loggedAll+' <span class="faint" style="font-size:14px">/ '+allN+'</span></div>'
    +'<div class="ex">已接入 / 可接入</div></div>'
    +'<div class="kpi"><div class="lb">② 原生直连</div>'
    +'<div class="vl" style="color:var(--ok)">已启用</div>'
    +'<div class="ex">账号池 '+(S.data.accountStats&&S.data.accountStats.total||0)
      +' · 原生中继 '+((S.data.overview.native||{}).relays||9)+' 平台 · 免 EXE</div></div>'
    +'<div class="kpi"><div class="lb">③ 自动路由</div>'
    +'<div class="vl">3</div>'
    +'<div class="ex">auto / weight / priority</div></div>'
    +'<div class="kpi"><div class="lb">分类</div>'
    +'<div class="vl" style="font-size:16px">'
    +'本'+nAcc+' · 反代'+nLp+'</div>'
    +'<div class="ex">有桌面客户端的算「本地 AI」</div></div>'
    +'</div>';

  // ---- 怎么用
  h+='<div class="note">'
    +'<b>用法就三步</b>：① 下面按类选源接入（扫码 / Cookie / 填 Key）'
    +'② 所有源都汇到面板这个 OpenAI 兼容端点：<code>'+esc(panelBaseUrl())+'</code>，'
    +'api_key 用 <code>admin</code> ③ 客户端里把 model 填成 <code>auto</code>，'
    +'面板按实测延迟自动挑最快的源。<br>'
    +'<span class="faint">分类规则：<b>有桌面客户端的一律算「本地 AI」</b>，'
    +'哪怕它同时有网页版。签到、任务、工具调用都是可选增强，不配置也不影响主流程。</span>'
    +'</div>';

  // ★ 不再区分 本地AI/平台API/网页对话——全部合并为一屏「本地 AI」
  const items=sourceList();
  h+='<div class="muted" style="font-size:12.5px;margin:-4px 0 12px">本地 AI 一览：'
    +'已接入 '+items.filter(i=>i.logged).length+' / '+items.length+'</div>';

  h+='<div class="grid g3">';
  for(const s of items){
    const badge=s.logged
      ?'<span class="tag ok">已接入</span>'
      :'<span class="tag">未接入</span>';
    const chk=s.checkin
      ?'<span class="tag acc" title="每日 '+esc(s.at||'')+' 自动签到">签到 '+(s.done?'✓':'')+'</span>'
      :'';
    const kindTag=(S.data.srcKinds||{})[s.key];
    const fact=kindTag&&kindTag.facts?kindTag.facts:{};
    const marks=((fact.desktop?'桌':'')+(fact.web?'网':'')+(fact.api?'API':''))||'';
    const act=s.logged
      ?'<button class="btn sm" onclick="event.stopPropagation();openSrc(\''+esc(s.key)+'\')">详情</button>'
      :'<button class="btn sm pri" onclick="event.stopPropagation();openSrc(\''+esc(s.key)+'\')">接入</button>';
    h+='<div class="card" style="margin:0;cursor:pointer" '
      +'onclick="openSrc(\''+esc(s.key)+'\')">'
      +'<div class="flex" style="justify-content:space-between;margin-bottom:8px">'
      +'<b style="font-size:14px">'+esc(s.name)+'</b>'+badge+'</div>'
      +'<div class="faint" style="font-size:11.5px;line-height:1.5;min-height:48px">'
      +esc(s.desc||'')+'</div>'
      +'<div class="flex" style="margin-top:8px;gap:5px">'
      +'<span class="tag info">'+esc(s.kind)+'</span>'
      +(marks?'<span class="tag">'+esc(marks)+'</span>':'')
      +chk
      +(s.models?'<span class="tag acc">'+s.models+' 模型</span>':'')
      +'</div>'
      +'<div class="faint mono" style="font-size:10.5px;margin-top:7px;word-break:break-all">'
      +esc(s.endpoint||'—')+'</div>'
      +(s.variants
        ?('<div style="margin-top:8px">'+s.variants.map(v=>
            '<div class="flex" style="justify-content:space-between;align-items:center;'
            +'padding:5px 0;border-top:1px solid var(--line2)">'
            +'<span class="faint" style="font-size:12px">'+esc(v.label)+'</span>'
            +(v.logged?'<span class="tag ok">已登录</span>'
              :'<button class="btn sm pri" onclick="event.stopPropagation();'
                +'srcLogin(\''+esc(v.pid)+'\')">扫码接入</button>')
            +'</div>').join('')+'</div>')
        :('<div style="margin-top:9px">'+act+'</div>'))
      +'</div>';
  }
  h+='</div>';
  return h;
}

function setSrcTab(k){ SRC_TAB=k; render(); }

function openSrc(key){
  S.data.srcDetail=key;
  SRC_VIEW='detail';
  render();
}
function closeSrc(){ S.data.srcDetail=null; SRC_VIEW='card'; render(); }

// ══════════════════════════════════════════════════════════════
// 腾讯 / CodeBuddy 原生登录（2026-10-04 抓包逆向）
// 不依赖 workbuddy-gateway 进程：面板自己走完 state → 扫码 → 换 token
// ══════════════════════════════════════════════════════════════
const TCB={state:null,url:'',busy:false,timer:null};

function viewTencentPanel(){
  const st=S.data.tcStatus;
  if(!st||!st.loaded) return '<div class="sk line" style="height:40px"></div>';
  if(!st.logged){
    return '<div class="note" style="margin-top:12px">'
      +'还没登录。点上面「扫码登录」会弹出一个浏览器窗口，'
      +'用微信扫码即可 —— 面板直接拿 token，<b>不需要 workbuddy-gateway 进程</b>。</div>';
  }
  // render() 会全量重绘 #view，所以内容必须由本函数现算，
  // 不能靠 loadTcQuota() 往 DOM 里塞 innerHTML（会被下一次 render 冲掉）
  const d=S.data.tcQuota;
  let h='<div class="note ok" style="color:var(--ok);border-color:var(--ok-line);'
    +'background:var(--ok-soft);margin-top:12px">已接入 · 来源：'
    +esc(st.account||'原生登录')+' · token '+esc(String(st.tokenLen))+' 字节'
    +(st.verify&&st.verify.rates===200?' · 倍率接口验活 200':'')+'</div>';
  if(!d||!d.summary) return h+'<div class="sk line" style="height:40px;margin-top:10px"></div>';

  const pct=d.totalSize?Math.round(d.totalUsed/d.totalSize*100):0;
  h+='<h3 style="margin-top:14px">额度（credits）</h3>';
  h+='<div class="grid g3 mb">'
    +'<div class="kpi"><div class="lb">剩余</div><div class="vl" style="color:var(--ok)">'+nfmt(d.totalRemain)+'</div>'
    +'<div class="ex">共 '+nfmt(d.totalSize)+' · 已用 '+nfmt(d.totalUsed)+'</div></div>'
    +'<div class="kpi"><div class="lb">资源条目</div><div class="vl">'+(d.quota?d.quota.totalCount:0)+'</div>'
    +'<div class="ex">累计消耗 '+nfmt(d.quota?d.quota.totalDosage:0)+'</div></div>'
    +'<div class="kpi"><div class="lb">使用率</div><div class="vl">'+pct+'%</div>'
    +'<div class="ex"><div class="bar"><i style="width:'+pct+'%"></i></div></div></div>'
    +'</div>';
  h+=dataTable({key:'tcQuota',rows:d.summary,size:10,sort:'size',
    cols:[
      {k:'name',t:'资源包',render:x=>esc(x.name)},
      {k:'unit',t:'单位',render:x=>esc(x.unit||'—')},
      {k:'size',t:'总量',render:x=>nfmt(x.size)},
      {k:'remain',t:'剩余',render:x=>'<b style="color:var(--ok)">'+nfmt(x.remain)+'</b>'},
      {k:'used',t:'已用',render:x=>nfmt(x.used)},
      {k:'cycles',t:'周期数',render:x=>x.cycles},
    ]});
  h+='<div class="btnrow mt">'
    +'<button class="btn pri" onclick="tencentCheckin()">立即签到</button>'
    +'<button class="btn" onclick="tencentModels()">拉在线模型与倍率</button>'
    +'<button class="btn" onclick="loadTcQuota()">刷新额度</button>'
    +'</div>';
  h+=viewTcModels();
  return h;
}

function viewTcModels(){
  const ms=(S.data.tcModels||[]);
  if(!ms.length) return '<div id="tcModels" class="mt"></div>';
  return '<div class="mt"><h3>在线模型与倍率（'+ms.length+'）</h3>'
    +'<div class="muted" style="font-size:12px;margin:6px 0 8px">'
    +'来源：在线目录 /console/enterprises/personal/models</div>'
    +dataTable({key:'tcModels',rows:ms,size:15,sort:'id',
      searchHint:'搜索模型…',searchKeys:['id','name'],
      cols:[
        {k:'id',t:'模型 ID',render:x=>'<span class="mono">'+esc(x.id)+'</span>'},
        {k:'name',t:'显示名',render:x=>esc(x.name||'—')},
        {k:'credits',t:'倍率',render:x=>x.credits?tag(String(x.credits),'acc'):'<span class="faint">—</span>'},
        {k:'maxInputTokens',t:'输入上限',render:x=>nfmt(x.maxInputTokens)},
        {k:'maxOutputTokens',t:'输出上限',render:x=>nfmt(x.maxOutputTokens)},
        {k:'supportsImages',t:'图像',render:x=>x.supportsImages?'<span class="tag ok">✓</span>':'<span class="faint">—</span>'},
        {k:'supportsToolCall',t:'工具',render:x=>x.supportsToolCall?'<span class="tag ok">✓</span>':'<span class="faint">—</span>'},
      ]})+'</div>';
}

async function loadTcStatus(){
  const r=await api('/api/tencent?action=status').catch(()=>({ok:false}));
  const d=(r&&r.result)||r||{};
  S.data.tcStatus={loaded:true,logged:!!d.logged,verify:d.verify||{},
    tokenLen:d.tokenLen||0,account:d.account||''};
  // 先用旧额度渲染，再静默刷新 —— 避免 loadTcQuota 里的 render 反过来重入这里
  render();
  if(d.logged) await loadTcQuota(true);
  return d;
}

async function tencentLogin(){
  if(TCB.busy) return;
  TCB.busy=true;
  toast('正在起浏览器…');
  const r=await api('/api/tencent?action=login',{}).catch(e=>({ok:false,message:e.message}));
  if(r&&r.ok===false){ toast(r.message||'起浏览器失败','err'); TCB.busy=false; return; }
  const d=(r&&r.result)||r||{};
  TCB.state=d.state; TCB.url=d.url;
  TCB.busy=false;
  toast('浏览器已打开，请扫码登录');
  S.data.tcStatus={loaded:true,logged:false};
  render();
  tcPoll();
}

function tcPoll(){
  if(TCB.timer) clearInterval(TCB.timer);
  TCB.timer=setInterval(async ()=>{
    if(!TCB.state) return;
    const r=await api('/api/tencent?action=poll',{state:TCB.state,timeout:2})
      .catch(()=>({ok:false}));
    const d=(r&&r.result)||r||{};
    if(d.accessToken||d.saved){
      if(TCB.timer) clearInterval(TCB.timer);
      TCB.timer=null; TCB.state=null;
      toast('登录成功，token 已保存');
      await loadTcStatus();
    }
  },2500);
}

async function loadTcQuota(silent){
  const r=await api('/api/tencent?action=quota').catch(()=>({ok:false}));
  if(r&&r.ok===false){ if(!silent) toast(r.message||'取额度失败','err'); return; }
  S.data.tcQuota=(r&&r.result)||r||null;
  // silent 只表示「失败别弹 toast」，成功照样要 render 才看得到额度
  render();
}

async function tencentCheckin(){
  const r=await api('/api/tencent?action=checkin').catch(e=>({ok:false,message:e.message}));
  if(r&&r.ok===false){toast(r.message||'签到失败','err');return;}
  const d=(r&&r.result)||r||{};
  if(d.already) toast('今天已经签过了');
  else if(d.checkedIn){ toast('签到成功！'); loadTcQuota(); }
  else toast(d.msg||'签到失败','err');
}

async function tencentModels(){
  const r=await api('/api/tencent?action=catalog',{}).catch(e=>({ok:false,message:e.message}));
  if(r&&r.ok===false){toast(r.message||'拉模型失败','err');return;}
  const d=(r&&r.result)||r||{};
  S.data.tcModels=d.models||[];
  render();
  toast('在线模型 '+S.data.tcModels.length+' 个（含倍率）');
}

/** 点开某个源：登录 / 看模型 / 签到 / 探测，都在这一层完成 */
function srcDetail(){
  const s=sourceList().find(x=>x.key===S.data.srcDetail);
  if(!s){
    // 防死循环：viewSources 在 detail 模式下会再调 srcDetail，
    // 这里必须先把模式切回 card，否则无限递归把浏览器卡死
    S.data.srcDetail=null; SRC_VIEW='card';
    return viewSources();
  }
  if(s.view==='localproxy'){
    return '<div class="card"><div class="ch"><b>'+esc(s.name)+'</b>'
      +'<button class="btn sm" onclick="closeSrc()">← 返回源列表</button></div>'
      +'<div class="cb">'+viewLocalproxy()+'</div></div>';
  }
  const acc=(S.data.accounts||[]).find(a=>a.platform===s.key)
    ||(s.variants?(S.data.accounts||[]).find(a=>s.variants.some(v=>v.pid===a.platform)):null);
  let h='<div class="card"><div class="ch"><b>'+esc(s.name)+'</b>'
    +'<button class="btn sm" onclick="closeSrc()">← 返回源列表</button></div><div class="cb">';
  h+='<div class="flex mb">'
    +'<span class="tag '+(s.logged?'ok':'')+'">'+(s.logged?'已接入':'未接入')+'</span>'
    +'<span class="tag info">'+esc(s.kind)+'</span>'
    +(s.checkin?'<span class="tag acc">支持每日签到'+(s.at?' '+esc(s.at):'')+'</span>':'')
    +'</div>';
  h+='<div class="muted" style="font-size:12.5px;margin-bottom:10px">'+esc(s.desc||'')+'</div>';

  if(s.variants){
    h+='<div style="margin:10px 0">'
      +s.variants.map(v=>
        '<div class="flex" style="justify-content:space-between;align-items:center;'
        +'padding:7px 0;border-top:1px solid var(--line2)">'
        +'<span style="font-size:13px">'+esc(v.label)+'</span>'
        +(v.logged
          ?'<span class="tag ok">已登录</span>'
          :'<button class="btn pri sm" onclick="srcLogin(\''+esc(v.pid)+'\')">'
            +'扫码接入</button>')
        +'</div>').join('')
      +'<div class="faint" style="font-size:11.5px;margin-top:6px">'
        +'国内/国际是两个独立账号，额度互不互通；需要哪个就登哪个。</div>'
      +'</div>';
    // 扫码后会话卡直接显示在这里（二维码就在当前页可见，不用跑去「当前登录」）
    if(S.data.loginSession)h+=viewLoginSession('copilot');
  }else if(s.logged){
    h+='<div class="note ok" style="color:var(--ok);border-color:var(--ok-line);background:var(--ok-soft)">'
      +'已接入 · 凭据 <code>'+esc(acc?String(acc.secret||''):'')+'</code>'
      +'（'+esc(acc&&acc.source||'')+'，'+esc(acc&&acc.obtained_at||'')+'）</div>';
    h+='<div class="btnrow">'
      +'<button class="btn" onclick="loadSrcModels(\''+esc(s.key)+'\')">查看该源模型</button>'
      +(s.checkin?'<button class="btn pri" onclick="acRunOne(\''+esc(s.key)+'\')">立即签到</button>':'')
      +'<button class="btn" onclick="platformProbe(\''+esc(s.key)+'\')">探测端点</button>'
      +'<button class="btn dgr" onclick="delAccountFor(\''+esc(s.key)+'\')">解除接入</button>'
      +'</div>';
    h+='<div id="srcModels" class="mt"></div>';
  }else{
    h+='<div class="btnrow">'
      +'<button class="btn pri" onclick="srcLogin(\''+esc(s.key)+'\')">'
      +(s.method==='qrcode'?'扫码接入':s.method==='cookie'?'用浏览器 Cookie 接入':'填 API Key 接入')
      +'</button>'
      +'<button class="btn" onclick="platformProbe(\''+esc(s.key)+'\')">先探测端点</button>'
      +'</div>';
    h+=viewLoginSession(s.key);
  }
  h+='</div></div>';
  return h;
}

function srcLogin(pid){
  const p=(S.data.loginPlatforms||[]).find(x=>x.id===pid);
  startLogin(pid,(p&&p.edition)||'');
}
async function loadSrcModels(pid){
  const box=$('#srcModels');
  if(box)box.innerHTML='<div class="sk line" style="height:34px"></div>';
  const r=await api('/api/models?action=platform_models',{platform:pid})
    .catch(e=>({ok:false,message:e.message}));
  if(!box)return;
  if(r.ok===false){box.innerHTML='<div class="note warn">'+esc(r.message||'取不到')+'</div>';return;}
  const ms=r.models||[];
  if(!ms.length){box.innerHTML=empty('该源返回空列表（可能还没登录）');return;}
  const srcNote=(r.source||r.note)
    ?'<div class="muted" style="font-size:12px;margin:6px 0 8px">'
    +esc(r.source?('来源：'+r.source):'')+'　'+esc(r.note||'')+'</div>' : '';
  box.innerHTML='<h3 style="margin-top:12px">'+esc(r.name||pid)+' 的模型（'+ms.length+'）</h3>'
    +srcNote
    +dataTable({
      key:'srcM_'+pid, rows:ms, size:15, sort:'id',
      searchHint:'搜索模型…', searchKeys:['id','name','desc'],
      cols:[
        {k:'id',t:'模型 ID',render:x=>'<span class="mono">'+esc(x.id)+'</span>'},
        {k:'name',t:'显示名',render:x=>esc(x.name||'—')},
        {k:'desc',t:'说明',render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.desc||'')+'</span>'},
        {k:'credits',t:'倍率',render:x=>x.credits?tag(String(x.credits),'acc'):'<span class="faint">—</span>'},
      ]});
}
function delAccountFor(pid){
  const a=(S.data.accounts||[]).find(x=>x.platform===pid);
  if(!a){toast('没有找到该源的凭据','err');return;}
  if(!confirm('确定解除「'+(a.name||pid)+'」的接入？凭据会从账号池删除。'))return;
  delAccount(a.id);
}

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
        <button class="btn pri sm" onclick="oneClick()">⚡ 一键执行</button>
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
  </div>
  ${viewPanelUsage(r.panel)}`;
}

/** 面板原生直连用量（路由 / 原生中继 / @pid 直连，独立于网关 EXE） */
function viewPanelUsage(p){
  if(!p||!p.events)return '';
  const t=p.today||{};
  const byModel=Object.entries(p.by_model||{}).sort((a,b)=>b[1].n-a[1].n).slice(0,20);
  const byPlat=Object.entries(p.by_platform||{}).sort((a,b)=>b[1].n-a[1].n);
  const days=p.days||[];
  return `
  <div class="card">
    <h2>面板原生直连用量 <span class="faint" style="font-size:11.5px">（auto 路由 / 免密车道 / @平台 直连；保留 90 天）</span></h2>
    <div class="grid g4 mb">
      <div class="kpi"><div class="lb">今日请求</div><div class="vl">${nfmt(t.requests||0)}</div>
        <div class="ex">成功 ${nraw(t.success||0)} · 失败 ${nraw(t.failed||0)}</div></div>
      <div class="kpi"><div class="lb">今日输入</div><div class="vl">${nfmt(t.tokensIn||0)}</div></div>
      <div class="kpi"><div class="lb">今日输出</div><div class="vl">${nfmt(t.tokensOut||0)}</div></div>
      <div class="kpi"><div class="lb">累计请求</div><div class="vl">${nfmt(p.events)}</div>
        <div class="ex">入 ${nfmt(p.in)} · 出 ${nfmt(p.out)}</div></div>
    </div>
    <div class="grid g2">
      <div class="scroll sm">
        <table><thead><tr><th>模型（Top 20）</th><th>请求</th><th>失败</th><th>入</th><th>出</th></tr></thead><tbody>
        ${byModel.length?byModel.map(([m,x])=>`<tr>
          <td class="mono">${esc(m)}</td><td>${nraw(x.n)}</td>
          <td>${x.fail?'<span class="tag warn">'+x.fail+'</span>':'0'}</td>
          <td>${nraw(x.in)}</td><td>${nraw(x.out)}</td></tr>`).join('')
        :'<tr><td colspan="5">'+empty('暂无数据')+'</td></tr>'}
        </tbody></table>
      </div>
      <div class="scroll sm">
        <table><thead><tr><th>上游平台</th><th>请求</th><th>失败</th><th>入</th><th>出</th></tr></thead><tbody>
        ${byPlat.length?byPlat.map(([m,x])=>`<tr>
          <td class="mono">${esc(m)}</td><td>${nraw(x.n)}</td>
          <td>${x.fail?'<span class="tag warn">'+x.fail+'</span>':'0'}</td>
          <td>${nraw(x.in)}</td><td>${nraw(x.out)}</td></tr>`).join('')
        :'<tr><td colspan="5">'+empty('暂无数据')+'</td></tr>'}
        </tbody></table>
      </div>
    </div>
    ${days.length?`<div class="scroll sm mt"><table><thead>
      <tr><th>日期</th><th>请求</th><th>成功</th><th>失败</th><th>输入</th><th>输出</th></tr></thead><tbody>
      ${days.map(d=>`<tr><td class="mono">${esc(d.date)}</td><td>${nraw(d.requests)}</td>
        <td>${nraw(d.success)}</td><td>${d.failed?'<span class="tag warn">'+d.failed+'</span>':'0'}</td>
        <td>${nraw(d.tokensIn)}</td><td>${nraw(d.tokensOut)}</td></tr>`).join('')}
      </tbody></table></div>`:''}
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
/** 模型总表数据（按来源分组，含每模型连接测试） */
async function loadModels(){
  const g=await api('/api/models?action=grouped').catch(()=>null);
  S.data.grouped=g;
  render();
}
function panelBaseUrl(){
  const u=location.origin||'';
  return (u?u.replace(/\/$/,'')+'/v1':'http://127.0.0.1:8790/v1');
}

/* ---------------------------------------------------------------- 模型总表
 * 用户要求：所有模型一张表、按来源分组、去掉倍率覆盖横幅/上游档案/已失效区，
 * 每行一个「连接测试」按钮（发一条极小真实请求，通→延迟，不通→具体原因）。
 * 数据来自 /api/models?action=grouped；测试走 /api/models?action=test_model。
 */
const GM_TEST = {};               // model id -> {state:'loading'|'ok'|'err', ms, reply, error}
// 分组折叠状态：默认全部收起，点组头展开；只持久化手动展开过的组
const GM_OPEN = (()=>{
  try{ return JSON.parse(localStorage.getItem('aigw.gmOpen')||'{}')||{}; }
  catch(e){ return {}; }
})();
function gmSaveOpen(){
  try{
    const on={};
    for(const k in GM_OPEN) if(GM_OPEN[k])on[k]=true;
    localStorage.setItem('aigw.gmOpen', JSON.stringify(on));
  }catch(e){}
}
function gmAll(open){
  document.querySelectorAll('.gm-group').forEach(grp=>{
    const pid=grp.getAttribute('data-pid');
    const head=grp.querySelector('.gm-head');
    const rows=grp.querySelector('.gm-rows');
    head.classList.toggle('open',open);
    if(rows)rows.style.display=open?'':'none';
    GM_OPEN[pid]=open;
  });
  gmSaveOpen();
}

function gmJs(s){                 // 模型 id 只含 [\w.@:-]，兜底替换掉危险字符
  return String(s==null?'':s).replace(/[^\w.@:\-]/g,'_');
}
function gmShort(s){
  s=String(s||'');
  return s.length>34?s.slice(0,34)+'…':s;
}

function gmTestCell(id){
  const r=GM_TEST[id];
  if(!r)return '<button class="btn sm" onclick="gmTest(this,\''+gmJs(id)+'\')">测试</button>';
  if(r.state==='loading')return '<span class="tag info">测试中…</span>';
  if(r.state==='ok')return '<span class="tag ok" title="'+esc(r.reply||'通过')+'">✓ '
    +r.ms+'ms</span>'+(r.reply?'<div class="faint" style="font-size:10.5px;max-width:110px;overflow:hidden;'
      +'text-overflow:ellipsis;white-space:nowrap">'+esc(r.reply)+'</div>':'');
  return '<span class="tag err" title="'+esc(r.error||'')+'">✗ '+gmShort(r.error||'失败')+'</span>';
}

async function gmTest(btn,id){
  const cur=GM_TEST[id];
  if(cur&&cur.state==='loading')return;
  GM_TEST[id]={state:'loading'};
  const cell=btn.closest('td');
  if(cell)cell.innerHTML=gmTestCell(id);
  const r=await api('/api/models?action=test_model',{model:id})
    .catch(e=>({ok:false,message:e.message}));
  // 后端把测试结果平铺在顶层：{ok:是否连通, latency_ms, via, reply, error}
  // （ok() 包装后的 "ok" 键被结果本身的 ok=okk 覆盖——正是设计意图）
  if(r&&typeof r.latency_ms!=='undefined'){
    GM_TEST[id]={state:r.ok?'ok':'err',
      ms:r.latency_ms||0, reply:r.reply||'', error:r.error||''};
  }else{
    GM_TEST[id]={state:'err',ms:0,reply:'',error:(r&&r.message)||'请求失败'};
  }
  if(cell)cell.innerHTML=gmTestCell(id);
}

function gmToggle(pid){
  const grp=document.querySelector('.gm-group[data-pid="'+pid+'"]');
  if(!grp)return;
  const head=grp.querySelector('.gm-head');
  const rows=grp.querySelector('.gm-rows');
  const open=!head.classList.contains('open');
  head.classList.toggle('open',open);
  GM_OPEN[pid]=open;
  gmSaveOpen();
  if(rows)rows.style.display=open?'':'none';
}

function gmFilter(q){
  q=String(q||'').trim().toLowerCase();
  let n=0;
  document.querySelectorAll('.gm-group').forEach(grp=>{
    let hit=0;
    grp.querySelectorAll('tbody tr').forEach(tr=>{
      const show=!q||(tr.getAttribute('data-s')||'').indexOf(q)>=0;
      tr.style.display=show?'':'none';
      if(show)hit++;
    });
    const pid=grp.getAttribute('data-pid');
    const head=grp.querySelector('.gm-head');
    const rows=grp.querySelector('.gm-rows');
    if(q){
      if(hit&&rows){rows.style.display='';head.classList.add('open');}
      grp.style.display=hit?'':'none';
    }else{
      grp.style.display='';
      const open=!!GM_OPEN[pid];
      head.classList.toggle('open',open);
      if(rows)rows.style.display=open?'':'none';
    }
    n+=hit;
  });
  const c=document.getElementById('gmCount');
  if(c)c.textContent=q?(n+' 个匹配'):'';
}

function viewModelsHelp(){
  const base=panelBaseUrl();
  return '<div class="card"><h2>接入方式（所有工具通用，一个接口就够了）</h2>'
    +'<pre>'+esc([
'# OpenAI 兼容（面板统一入口）',
'base_url = '+base,
'api_key  = admin            # 任意值',
'# 模型：填 auto（自动挑最快），或上面总表里的任意模型 ID',
'',
'# Claude Code / Anthropic 兼容',
'ANTHROPIC_BASE_URL = '+base.replace(/\/v1$/,''),
'ANTHROPIC_API_KEY  = admin',
'',
'# curl 自测（model 换成总表任意 ID）',
'curl '+base+'/chat/completions \\',
'  -H "Content-Type: application/json" \\',
'  -d \'{"model":"auto","messages":[{"role":"user","content":"你好"}]}\'',
    ].join('\n'))+'</pre>'
    +'<div class="muted mt" style="font-size:12px">表里的「测试」按钮走的就是这条链路：'
    +'发一条极小真实请求，通→显示延迟，不通→给出原因（多半是该平台还没登录）。</div></div>';
}

function viewModels(){
  const g=S.data.grouped;
  let h='<div class="card">'
    +'<div class="ch"><div><b>模型总表</b>'
    +'<div class="faint" style="font-size:12px;margin-top:2px">全部来源统一从一个接口出：<code>'
    +esc(panelBaseUrl())+'</code> · api_key 任意值</div></div>'
    +'<div class="btnrow n"><button class="btn sm" onclick="gmAll(true)">展开全部</button>'
    +'<button class="btn sm" onclick="gmAll(false)">收起全部</button>'
    +'<button class="btn sm" onclick="copyEndpoint()">复制接入地址</button></div></div>'
    +'<div class="dtbar"><input class="grow" id="gmSearch" placeholder="搜索模型 / 来源…" '
    +'oninput="gmFilter(this.value)"><span class="cnt" id="gmCount"></span></div>';
  if(!g){
    h+='<div style="padding:12px 14px 16px"><div class="sk line"></div>'
      +'<div class="sk line"></div><div class="sk block"></div></div></div>'
      +viewModelsHelp();
    return h;
  }
  const groups=g.groups||[];
  for(const grp of groups){
    // ★ 默认全部收起（2026-10-06 用户要求）：点组头展开，手动展开过的组
    //   记在 localStorage（GM_OPEN 初始化时读回）。行节点总是渲染，
    //   收起只切 display —— 否则点开时没有内容可显示。
    const open=!!GM_OPEN[grp.pid];
    h+='<div class="gm-group" data-pid="'+esc(grp.pid)+'">'
      +'<div class="gm-head'+(open?' open':'')+'" onclick="gmToggle(\''+gmJs(grp.pid)+'\')">'
      +'<span class="caret">▸</span><b>'+esc(grp.name)+'</b>'
      +'<span class="tag">'+grp.models.length+' 模型</span>'
      +(grp.usable>0
        ?'<span class="tag ok">✓ '+grp.usable+' 凭据</span>'
        :'<span class="tag">未接入</span>')
      +(grp.live?'<span class="tag info">实时清单</span>':'')
      +(grp.hint?'<span class="faint gm-hint">'+esc(grp.hint)+'</span>':'')
      +'</div>';
    if(grp.models.length){
      h+='<div class="gm-rows"'+(open?'':' style="display:none"')+'><div class="dtw" style="max-height:420px"><table><thead><tr>'
        +'<th>模型 ID（点击复制）</th><th>名称</th><th>倍率</th><th>上下文</th>'
        +'<th>输出</th><th>说明</th><th style="width:130px">连接测试</th>'
        +'</tr></thead><tbody>';
      for(const m of grp.models){
        const id=m.id||'';
        h+='<tr data-s="'+esc((id+' '+(m.name||'')+' '+(m.desc||'')).toLowerCase())+'">'
          +'<td><span class="mono gm-id" title="点击复制" '
          +'onclick="copyText(\''+gmJs(id)+'\',()=>toast(\'已复制\',\'ok\'))">'
          +esc(id)+'</span></td>'
          +'<td>'+esc(m.name||'')+'</td>'
          +'<td>'+(m.credits?'<span class="tag acc">'+esc(String(m.credits))+'</span>'
            :'<span class="faint">—</span>')+'</td>'
          +'<td class="muted">'+(m.ctx?Number(m.ctx).toLocaleString():'—')+'</td>'
          +'<td class="muted">'+(m.out?Number(m.out).toLocaleString():'—')+'</td>'
          +'<td class="faint gm-desc">'+esc(m.desc||'')+'</td>'
          +'<td class="nowrap">'+gmTestCell(id)+'</td>'
          +'</tr>';
      }
      h+='</tbody></table></div></div>';
    }else if(!grp.models.length){
      h+='<div class="gm-rows"><div class="empty" style="padding:14px">'
        +'该来源没有静态清单 —— 登录后用「接入源 → 详情 → 查看该源模型」动态拉取</div></div>';
    }
    h+='</div>';
  }
  h+='<div class="dtfoot"><span>共 '+(g.total||0)+' 个模型 · '+groups.length
    +' 个来源 · 「测试」= 发一条极小真实请求验证连通（消耗极少额度）</span></div></div>'
    +viewModelsHelp();
  return h;
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

function viewRoute0(){
  const r=S.data.route||{};
  const models=r.models||{};
  const names=Object.keys(models);
  const cfg=r.config||{};
  return `
  <div class="note">
    面板即中转层。客户端把 <code>base_url</code> 指到本面板，<code>model</code> 填
    <code>auto</code>（最快优先）/ <code>free</code>（免费优先），
    剩下的选路由路由引擎决定：探延迟、熔断、失败自动降级、会话亲和。
  </div>

  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">自动模型</div><div class="vl">${names.length}</div>
      <div class="ex">默认 auto / free 两种</div></div>
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
        <button class="btn" onclick="testRoute('auto')">试跑 auto</button>
        <button class="btn pri" onclick="testRoute('free')">试跑 free</button>
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
      <tr><td><b>auto</b></td><td>优先实测延迟（EWMA 平滑）最低的上游。同一会话会尽量粘在同一上游，避免上下文抖动。</td></tr>
      <tr><td><b>free</b></td><td>免费优先：先选带「免费」标记的车道（anon-zen 匿名车道 / 豆包等），组内再比延迟；非免费车道作兜底。适合「白嫖优先」。</td></tr>
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
      <div><label>最快策略模型名（auto）</label><input readonly value="auto"></div>
      <div><label>免费优先模型名（free）</label><input readonly value="free"></div>
    </div>
    <pre class="mt">${esc([
'# 1) 客户端只填面板地址，模型填 auto',
'base_url = '+location.origin+'/v1',
'api_key  = admin',
'model    = auto          # 自动挑延迟最低的上游',
'',
'# 2) 单次请求临时指定策略（面板扩展字段，不影响标准客户端）',
'{',
'  "model": "free",',
'  "aigw_strategy": "priority",   # fastest | weight | priority',
'  "aigw_session": "conv-123",    # 会话亲和 ID，让同一对话固定同一上游',
'  "messages": [{"role":"user","content":"你好"}]',
'}',
'',
'# 3) curl 实测',
'curl '+location.origin+'/v1/chat/completions \\',
'  -H "Content-Type: application/json" \\',
'  -d \'{"model":"auto","messages":[{"role":"user","content":"ping"}]}\'',
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
let SET_TAB = "main";

async function loadSettings(){
  // 设置页现在是 Tab 容器，数据一次性拉齐
  await Promise.all([
    api('/api/settings').catch(()=>({settings:{}})),
    api('/api/sites').catch(()=>({sites:[],catalog:[]})),
    api('/api/notify').catch(()=>({config:{}})),
    api('/api/logs').catch(()=>({lines:[]})),
  ]);
}

function viewSettings(){
  const tabs=[['main','⚙ 基本设置'],['sites','☷ 资源站点'],
              ['catalog','★ 免费资源'],['notify','✉ 通知'],['logs','≡ 日志']];
  let h='<div class="tabs">'
    +tabs.map(x=>'<button class="'+(SET_TAB===x[0]?'on':'')+'" '
      +'onclick="setSetTab(\''+x[0]+'\')">'+x[1]+'</button>').join('')
    +'</div>';
  if(SET_TAB==='main') h+=viewSettingsMain();
  else if(SET_TAB==='sites') h+=viewSites();
  else if(SET_TAB==='catalog') h+=viewCatalog();
  else if(SET_TAB==='notify') h+=viewNotify();
  else if(SET_TAB==='logs') h+=viewLogs();
  return h;
}
function setSetTab(k){ SET_TAB=k; render(); }

function viewSettingsMain(){
  const r=S.data.settings||{};
  const s=r.settings||{};
  return `
  <div class="grid g2">
    <div class="card">
      <h2>自动化</h2>
      <label>每日签到时间（小时）</label>
      <input id="sCH" type="number" min="0" max="23" value="${esc(s.checkin_hour??9)}">
      <label>成长任务时间（小时）</label>
      <input id="sGH" type="number" min="0" max="23" value="${esc(s.growth_hour??10)}">
      <div class="flex mt" style="gap:18px">
        <div class="flex"><input type="checkbox" id="sAC" style="width:auto" ${s.auto_checkin!==false?'checked':''}><span>自动签到</span></div>
        <div class="flex"><input type="checkbox" id="sAG" style="width:auto" ${s.auto_growth!==false?'checked':''}><span>自动成长任务</span></div>
      </div>
      <div class="flex mt" style="gap:8px;align-items:center">
        <span style="font-size:12.5px">Lobster 上游地址</span>
        <input id="sLob" class="mono" style="flex:1;max-width:420px" placeholder="lobsterai2api 的 LB2A_UPSTREAM_BASE，如 https://xxx.youdao.com" value="${esc(s.lobster_server||'')}">
        <span class="faint" style="font-size:11.5px">网易 Lobster AI（web-lobster）的动态模型/对话中继都走它</span>
      </div>
      <div class="note" style="margin-top:10px">
        <b>关于本地反代（CLIProxyAPI）</b>：<br>
        · <b>CLIProxyAPI</b>（端口 8318，面板内置）—— 反代
        <b>Kimi / Codex / Claude Code / Antigravity / Grok / Devin / Meta</b> 这些 CLI 订阅，
        面板自己实现不了，只能内嵌这个开源项目。<br>
        <span class="faint">如果你只用 CodeBuddy/WorkBuddy，用不到 CLIProxyAPI：
        删掉 dist 目录里 data/cliproxy 那份二进制（或用轻量版打包）即可，
        其余功能不受影响。</span>
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
      <tr><td>签到范围</td><td>
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
  const p={
    checkin_hour:parseInt($('#sCH').value||'9',10),
    growth_hour:parseInt($('#sGH').value||'10',10),
    auto_checkin:$('#sAC').checked,
    auto_growth:$('#sAG').checked,
    lobster_server:$('#sLob').value.trim(),
  };
  const r=await post('/api/settings?action=save',p);
  toast(r.message||'已保存',r.ok?'ok':'err');
  refresh();loadSettings();
}

// ---------------------------------------------------------------- 通用
async function quitPanel(){
  if(!confirm('退出面板？\n\n· 面板会关闭，http 服务停止\n· 想后台常驻请用「开机自启动」'))return;
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

function qrDataURL(text){
  // 客户端把授权 URL 渲染成二维码（vendor/qrcode.min.js，MIT）
  try{
    if(typeof qrcode!=='function')return '';
    const qr=qrcode(0,'M'); qr.addData(text); qr.make();
    return qr.createDataURL(8,8);
  }catch(e){ return ''; }
}

function qrOrLinkHTML(sess){
  // 网关给过 qr 图就直接用；原生直连没有图时，用授权 URL 现场出码 —— 不用跳网页
  if(sess.qr)return '<img src="'+sess.qr+'" alt="登录二维码" style="width:196px;height:196px;border-radius:10px;border:1px solid var(--line)">';
  if(sess.auth_url){
    const d=qrDataURL(sess.auth_url);
    if(d)return '<img src="'+d+'" alt="登录二维码（由授权链接生成）" style="width:196px;height:196px;border-radius:10px;border:1px solid var(--line)">'
      +'<div class="faint" style="margin-top:6px;max-width:196px">用 CodeBuddy / WorkBuddy App 扫码，或点下方按钮在浏览器打开</div>';
  }
  return '';
}

function paintLoginSession(){
  const s=S.data.loginSession;
  if(!s)return;
  const st=$('#lsStatus'),qr=$('#lsQr'),msg=$('#lsMsg'),left=$('#lsLeft');
  if(st)st.innerHTML=loginStatusTag(s);
  if(msg)msg.textContent=s.message||s.error||'';
  if(left)left.textContent=s.seconds_left?('剩余 '+s.seconds_left+' 秒'):'';
  if(qr)qr.innerHTML=qrOrLinkHTML(s);
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

/* ================================================================ 本地反代上游
 * 把 CLIProxyAPI 集成进面板：启停、配置、OAuth 登录全在界面里完成，
 * 用户不必手动装服务、改 YAML、敲命令行。
 */
function loadLocalproxy(){
  api('/api/localproxy?action=status').then(r=>{
    S.data.lp=r;
    render();
  }).catch(e=>toast('读取本地上游状态失败：'+e.message,'err'));
}
async function lpAct(action, body){
  const label={start:'启动',stop:'停止',restart:'重启',config:'保存配置',login:'登录'}[action]||action;
  toast(label+'中…');
  const r=await api('/api/localproxy?action='+action, body||{})
    .catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||label+'失败','err');return r;}
  if(action==='login'){
    S.data.lpLogin=r;
    if(r.url){
      const box=$('#lpLoginBox');
      if(box){box.style.display='block';
        box.innerHTML='<div class="note">已生成授权链接，'
          +'<a href="'+esc(r.url)+'" target="_blank" rel="noreferrer" class="mono">'+esc(r.url)+'</a>'
          +(r.user_code?('　设备码：<b class="mono">'+esc(r.user_code)+'</b>'):'')
          +'<br>'+esc(r.note||'')+'</div>';}
      toast('授权链接已生成，点上面打开','ok');
    }else{
      toast('没抓到授权链接，看下方日志','err');
    }
  }else{
    toast(r.message||label+'成功','ok');
  }
  loadLocalproxy();
  return r;
}
function viewLocalproxy(){
  const st=S.data.lp||{};
  const lg=S.data.lpLogin||null;
  const logs=S.data.lpLog||'';

  let h='';
  // 状态卡
  h+='<div class="grid g4 mb">'
    +'<div class="kpi"><div class="lb">服务状态</div>'
    +'<div class="vl" style="color:'+(st.online?'var(--ok)':'var(--faint)')+'">'
    +(st.online?'运行中':'已停止')+'</div>'
    +'<div class="ex">'+(st.online?esc(st.base_url):esc(st.note||''))+'</div></div>'
    +'<div class="kpi"><div class="lb">内置二进制</div>'
    +'<div class="vl">'+(st.installed?'已就位':'缺失')+'</div>'
    +'<div class="ex">v'+esc(st.version||'')+' · '+esc(st.size_mb||0)+' MB'
    +(st.bundled?' · 已内置在 EXE':'')+'</div></div>'
    +'<div class="kpi"><div class="lb">可用模型</div>'
    +'<div class="vl">'+esc(st.models||0)+'</div>'
    +'<div class="ex">'+((st.auth_files||[]).length)+' 个账号凭据</div></div>'
    +'<div class="kpi"><div class="lb">端点</div>'
    +'<div class="vl" style="font-size:15px">:'+esc(st.port||8318)+'</div>'
    +'<div class="ex">key '+esc((st.api_key||'').slice(0,6))+'…</div></div>'
    +'</div>';

  if(!st.installed){
    h+='<div class="note err">没有找到 <code>cli-proxy-api.exe</code>。'
      +'用打包好的面板 EXE 会自动内置；如果是从源码跑，把二进制放到 '
      +'<code>网关项目/cli-proxy/</code> 或 <code>aigw-panel/data/cliproxy/bin/</code>。</div>';
  }

  // 控制
  h+='<div class="card"><div class="ch"><b>CLIProxyAPI</b>'
    +'<span class="faint">把 Kimi / Codex / Claude / Antigravity / Grok / Devin / Meta '
    +'的 CLI 订阅转成 OpenAI 兼容 API</span></div><div class="cb">'
    +'<div class="btnrow mb">'
    +(st.online?'<button class="btn" onclick="lpAct(\'restart\')">重启</button>'
              :'<button class="btn pri" onclick="lpAct(\'start\')">启动</button>')
    +'<button class="btn" onclick="lpAct(\'stop\')">停止</button>'
    +'<button class="btn" onclick="loadLocalproxy()">刷新状态</button>'
    +'<button class="btn gh" onclick="lpShowLog()">查看日志</button>'
    +'</div>'
    +'<div class="row n">'
    +'<div><label>端口</label><input id="lpPort" type="number" value="'
    +esc(st.port||8318)+'"></div>'
    +'<div><label>API Key</label><input id="lpKey" value="'
    +esc(st.api_key||'')+'"></div>'
    +'<div class="n" style="align-self:flex-end">'
    +'<button class="btn" onclick="lpAct(\'config\',{port:+$(\'#lpPort\').value,'
    +'key:$(\'#lpKey\').value,apply:true})">保存并重启</button></div>'
    +'</div>'
    +'<div class="muted mt" style="font-size:12px">配置文件：<code>'+esc(st.config_path||'')+'</code>'
    +' · 凭据目录：<code>'+esc(st.auth_dir||'')+'</code></div>'
    +'</div></div>';

  // 登录
  h+='<div class="card"><div class="ch"><b>登录账号</b>'
    +'<span class="faint">授权链接会在下方生成，点开完成授权，凭据自动落盘、服务热加载</span>'
    +'</div><div class="cb">';
  if(st.login_status&&st.login_status.running){
    h+='<div class="note">有登录流程正在进行中，等你授权完…</div>';
  }
  h+=dataTable({
    key:'lpLogin', rows:st.providers||[], size:20,
    searchHint:'搜索平台…', searchKeys:['name','note','id'],
    cols:[
      {k:'name',t:'平台',render:x=>'<b>'+esc(x.name)+'</b>'
        +'<div class="faint mono" style="font-size:11px">'+esc(x.flag)+'</div>'},
      {k:'note',t:'说明',render:x=>'<span class="faint" style="font-size:12px">'+esc(x.note)+'</span>'},
      {k:'_go',t:'',render:x=>'<button class="btn sm pri" onclick="lpAct(\'login\','
        +'{provider:\''+esc(x.id)+'\'})">获取授权链接</button>'},
    ]});
  h+='<div id="lpLoginBox" style="display:none"></div>';
  h+='<div class="muted mt" style="font-size:12px">'
    +'说明：v'+esc(st.version||'8.0.13')+' 的登录项就是上面这些 —— '
    +'<b>没有 CodeBuddy，也没有 Qoder</b>（早期资料说有，实测 --help 确认是错的）。'
    +'</div>';
  h+='</div></div>';

  // 已登录账号
  if((st.auth_files||[]).length){
    h+='<div class="card"><h2>已保存的凭据（'+(st.auth_files||[]).length+'）</h2>'
      +dataTable({
        key:'lpAuth', rows:st.auth_files, size:20,
        searchHint:'搜索凭据文件…', searchKeys:['name'],
        cols:[
          {k:'name',t:'文件',render:x=>'<span class="mono" style="font-size:12px">'+esc(x.name)+'</span>'},
          {k:'size',t:'大小',num:true,render:x=>x.size+' B'},
          {k:'mtime',t:'修改时间',render:x=>'<span class="faint">'+esc(x.mtime)+'</span>'},
        ]})+'</div>';
  }

  // 模型列表
  if((st.model_ids||[]).length){
    h+='<div class="card"><h2>可路由的模型（'+esc(st.models)+'）</h2>'
      +'<pre>'+esc((st.model_ids||[]).join('\n'))+'</pre></div>';
  }

  // 日志
  if(logs){
    h+='<div class="card"><h2>服务日志</h2><pre>'+esc(logs)+'</pre></div>';
  }
  if(lg&&lg.raw){
    h+='<div class="card"><h2>登录输出</h2><pre>'+esc(lg.raw)+'</pre></div>';
  }
  return h;
}
async function lpShowLog(){
  const r=await api('/api/localproxy?action=log',{lines:120}).catch(e=>({ok:false}));
  S.data.lpLog=(r&&r.log)||'';
  render();
}

/* ================================================================
 * 账号与签到（把原来 5 个页面合并成 1 个）
 *   原来：账号登录 / 签到中心 / 成长任务 / 任务中心 / 自动签到
 *   现在：一个页面，顶部 Tab 切换，登录态和签到计划放在一起
 * ================================================================ */
let ACC_TAB = 'login';

function loadAccount(){
  Promise.all([
    api('/api/login?action=platforms').catch(()=>({platforms:[]})),
    api('/api/accounts').catch(()=>({accounts:[],stats:{}})),
    api('/api/platform?action=actions').catch(()=>({})),
    api('/api/autocheckin?action=status').catch(()=>({platforms:[]})),
    api('/api/login?action=browser').catch(()=>({browser:null})),
  ]).then(([lp,ac,pa,au,bd])=>{
    S.data.loginPlatforms=lp.platforms||[];
    S.data.accounts=ac.accounts||[];
    S.data.accountStats=ac.stats||{};
    S.data.platformActions=pa||{};
    S.data.autoCheckin=au;
    S.data.loginBrowser=(bd&&bd.browser)||null;
    render();
  }).catch(e=>toast('加载账号信息失败：'+e.message,'err'));
}

function viewAccount(){
  const pls=S.data.loginPlatforms||[];
  const accs=S.data.accounts||[];
  const st=S.data.accountStats||{};
  const au=S.data.autoCheckin||{};
  const accP=au.platforms||[];
  const acts=S.data.platformActions||{};
  const logged=new Set(accs.map(a=>a.platform));

  let h='';
  // ---- 概览
  const doneN=accP.filter(p=>(p._on!==undefined?p._on:p.on)&&p.logged_in).length;
  const onN=accP.filter(p=>(p._on!==undefined?p._on:p.on)).length;
  h+='<div class="grid g4 mb">'
    +'<div class="kpi"><div class="lb">已登录平台</div><div class="vl">'+logged.size+' / '+pls.length+'</div>'
    +'<div class="ex">账号池 '+esc(st.with_secret||0)+' 条有效凭据</div></div>'
    +'<div class="kpi"><div class="lb">自动签到</div>'
    +'<div class="vl" style="color:'+(au.enabled?'var(--ok)':'var(--faint)')+'">'
    +(au.enabled?'已启用':'已停用')+'</div>'
    +'<div class="ex">勾选 '+onN+' 个 · 其中 '+doneN+' 个已登录</div></div>'
    +'<div class="kpi"><div class="lb">公开签到接口</div>'
    +'<div class="vl">'+accP.filter(p=>p.has_public_checkin).length+'</div>'
    +'<div class="ex">其余需先「探测端点」</div></div>'
    +'<div class="kpi"><div class="lb">今日已执行</div>'
    +'<div class="vl">'+accP.filter(p=>p.done_today).length+'</div>'
    +'<div class="ex">同一天每平台只跑一次</div></div>'
    +'</div>';

  // ---- Tab
  h+='<div class="tabs">'
    +[['login','① 登录与凭据'],['plan','② 签到计划'],
       ['sess','③ 当前登录'],['log','④ 账号池与能力']]
      .map(x=>'<button class="'+(ACC_TAB===x[0]?'on':'')+'" '
      +'onclick="setAccTab(\''+x[0]+'\')">'+x[1]+'</button>').join('')
    +'</div>';

  if(ACC_TAB==='login'){
    h+='<div class="card"><div class="ch"><b>可登录的平台（'+pls.length+'）</b>'
      +'<span class="faint">'+logged.size+' 个已登录</span></div><div class="cb">';
    h+=dataTable({
      key:'accPlat', rows:pls, size:25,
      searchHint:'搜索平台…', searchKeys:['name','hint','upstream'],
      cols:[
        {k:'name',t:'平台',render:x=>'<b>'+esc(x.name)+'</b>'
          +'<div class="faint mono" style="font-size:11px">'+esc(x.id)+'</div>'},
        {k:'method',t:'方式',render:x=>tag(
          {qrcode:'扫码',cookie:'Cookie',file:'文件/Key'}[x.method]||x.method,
          {qrcode:'ok',cookie:'acc',file:''}[x.method]||'acc')},
        {k:'_in',t:'登录态',render:x=>logged.has(x.id)
          ?'<span class="tag ok">已登录</span>'
          :'<button class="btn sm" onclick="setAccTab(\'login\');startLogin(\''+esc(x.id)+'\',\''+esc(x.edition||'')+'\')">去登录</button>'},
        {k:'upstream',t:'上游',render:x=>'<span class="mono faint" style="font-size:11.5px">'+esc(x.upstream||'—')+'</span>'},
        {k:'hint',t:'说明',render:x=>'<span class="faint" style="font-size:11.5px;min-width:220px;display:block">'+esc(x.hint||'')+'</span>'},
      ]});
    h+='</div></div>';
  }

  if(ACC_TAB==='plan'){
    h+='<div class="card"><div class="ch"><b>定时签到计划</b>'
      +'<div class="flex n">'
      +'<label style="margin:0;display:flex;align-items:center;gap:6px">'
      +'<input type="checkbox" style="width:auto" '+(au.enabled?'checked':'')
      +' onchange="acSet(\'enabled\',this.checked)">启用定时</label>'
      +'<label style="margin:0;display:flex;align-items:center;gap:6px">'
      +'<input type="checkbox" style="width:auto" '+(au.notify?'checked':'')
      +' onchange="acSet(\'notify\',this.checked)">完成后通知</label>'
      +'<input type="number" value="'+esc(au.stagger_sec)+'" style="width:88px" title="平台间错峰秒数" onchange="acSet(\'stagger_sec\',+this.value)">'
      +'<input type="number" value="'+esc(au.retry_times)+'" style="width:88px" title="失败重试次数" onchange="acSet(\'retry_times\',+this.value)">'
      +'<input type="number" value="'+esc(au.retry_delay_min)+'" style="width:110px" title="重试间隔分钟" onchange="acSet(\'retry_delay_min\',+this.value)">'
      +'<button class="btn pri" onclick="acSave()">保存</button>'
      +'<button class="btn" onclick="acRun(false)">执行一轮</button>'
      +'<button class="btn" onclick="acRun(true)">强制全部</button>'
      +'</div></div><div class="cb">';
    h+=dataTable({
      key:'accPlan', rows:accP, size:20,
      searchHint:'搜索平台…', searchKeys:['name','hint'],
      cols:[
        {k:'name',t:'平台',render:x=>'<b>'+esc(x.name)+'</b>'
          +'<div class="faint mono" style="font-size:11px">'+esc(x.platform)+'</div>'},
        {k:'_on',t:'启用',render:x=>{
          const v=x._on!==undefined?x._on:x.on;
          return '<input type="checkbox" style="width:auto" '+(v?'checked':'')
            +' onchange="acToggle(this,\''+esc(x.platform)+'\',this.checked)">';
        }},
        {k:'_time',t:'每日时间',render:x=>{
          const v=x._time!==undefined?x._time:x.time;
          return '<input type="time" style="width:120px" value="'+esc(v)+'" '
            +'onchange="acTime(this,\''+esc(x.platform)+'\',this.value)">';
        }},
        {k:'mode',t:'方式',render:x=>tag(
          {flow:'先查后领',direct:'直接领',bonus:'登录送',none:'需探测'}[x.mode]||x.mode,
          x.mode==='none'?'':'acc')},
        {k:'logged_in',t:'凭据',render:x=>x.logged_in?tag('已登录','ok'):tag('未登录','warn')},
        {k:'done_today',t:'今日',render:x=>x.done_today?tag('已跑','ok'):'—'},
        {k:'hint',t:'说明',render:x=>'<span class="faint" style="font-size:11.5px;min-width:200px;display:block">'+esc(x.hint||'')+'</span>'},
        {k:'_go',t:'',render:x=>'<button class="btn sm" onclick="acRunOne(\''+esc(x.platform)+'\')">单独执行</button>'},
      ]});
    h+='<div class="muted mt" style="font-size:12px">每个平台到点跑一次，同一天同一平台只跑一次；'
      +'失败按上面配的次数重试。「需探测」表示公开接口里没找到签到端点，'
      +'先在下面「当前登录」里点「🔍 探测端点」扫出真实路径。</div>';
    h+='</div></div>';
  }

  if(ACC_TAB==='sess'){
    h+=viewLoginSession();
  }

  if(ACC_TAB==='log'){
    h+='<div class="card"><div class="ch"><b>账号池（'+accs.length+'）</b>'
      +'<span class="faint">凭据只存本机 data/ 目录，不外传</span></div><div class="cb">';
    if(!accs.length){
      h+=empty('还没有账号。去「登录与凭据」扫码或粘贴 Cookie。');
    }else{
      h+=dataTable({
        key:'accList', rows:accs, size:20,
        searchHint:'搜索平台 / 名称…', searchKeys:['platform','name','source'],
        cols:[
          {k:'platform',t:'平台',render:x=>'<span class="mono" style="font-size:12px">'+esc(x.platform)+'</span>'},
          {k:'name',t:'名称'},
          {k:'type',t:'类型',render:x=>tag(x.type||'')},
          {k:'source',t:'来源',render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.source||'')+'</span>'},
          {k:'secret',t:'密钥',render:x=>'<span class="mono faint">'+esc(x.secret||'')+'</span>'},
          {k:'obtained_at',t:'获取时间',render:x=>'<span class="faint" style="font-size:11.5px">'+esc(x.obtained_at||'')+'</span>'},
          {k:'enabled',t:'状态',render:x=>x.enabled===false?tag('已停用','warn'):tag('启用','ok')},
          {k:'_op',t:'',render:x=>'<button class="btn sm" onclick="toggleAccount(\''+esc(x.id)+'\','
            +(x.enabled===false?'true':'false')+')">'+(x.enabled===false?'启用':'停用')+'</button> '
            +'<button class="btn sm dgr" onclick="delAccount(\''+esc(x.id)+'\')">删除</button>'},
        ]});
    }
    h+='</div></div>';
    // 平台内部能力也放这里（点进去才展开）
    h+=viewPlatformCaps(acts);
  }
  return h;
}

function setAccTab(k){ ACC_TAB=k; render(); }
async function acSet(k,v){
  const au=S.data.autoCheckin||(S.data.autoCheckin={});
  au[k]=v;
  await acSave();
}
async function acToggle(el,pid,on){
  const au=S.data.autoCheckin||(S.data.autoCheckin={});
  au.platforms=au.platforms||[];
  const p=au.platforms.find(x=>x.platform===pid);
  if(p)p.on=on; else au.platforms.push({platform:pid,on:on});
  await acSave();
}
async function acTime(el,pid,v){
  const au=S.data.autoCheckin||(S.data.autoCheckin={});
  au.platforms=au.platforms||[];
  const p=au.platforms.find(x=>x.platform===pid);
  if(p)p.time=v; else au.platforms.push({platform:pid,time:v});
  await acSave();
}

/* ================================================================
 * 全局「一键执行」：签到 + 到期任务，一处触发
 * ================================================================ */
let ONECLICK = {running:false, steps:[], result:null};

async function oneClick(){
  if(ONECLICK.running){toast('正在执行中…','err');return;}
  ONECLICK.running=true; ONECLICK.steps=[]; ONECLICK.result=null;
  render();
  const step=(n,msg)=>{ONECLICK.steps.push({n,msg,at:new Date().toLocaleTimeString('zh-CN')});render();};

  step('准备','检查网关与凭据…');
  // 1) 自动签到（所有勾选且已登录的平台）
  const r1=await api('/api/autocheckin?action=run',{}).catch(e=>({ok:false,message:e.message}));
  if(r1&&r1.finished){
    const res=r1.results||[];
    const ok=res.filter(x=>x.ok).length;
    const sk=res.filter(x=>x.skipped).length;
    const fl=res.filter(x=>!x.ok&&!x.skipped).length;
    step('签到',`成功 ${ok} · 跳过 ${sk} · 失败 ${fl}`);
  }else if(r1&&r1.ok===false){
    step('签到','跳过：'+(r1.message||'自动签到未就绪'));
  }else{
    step('签到','已触发，后台执行中');
  }
  // 2) 任务中心里到期的任务
  step('任务','检查待执行任务…');
  const r2=await api('/api/tasks?action=run_all',{}).catch(()=>({ok:false}));
  if(r2&&r2.ok!==false){
    const rs=r2.results||[];
    step('任务',rs.length?('执行 '+rs.filter(x=>x.ok).length+' / '+rs.length):'没有到期的任务');
  }else{
    step('任务','任务中心没有需要跑的');
  }
  // 3) 刷新
  step('收尾','刷新面板数据…');
  await refresh().catch(()=>{});
  ONECLICK.running=false;
  ONECLICK.result=new Date().toLocaleTimeString('zh-CN');
  render();
  toast('一键执行完成','ok');
}
function viewOneClick(){
  if(!ONECLICK.steps.length)return '';
  let h='<div class="card"><div class="ch"><b>⚡ 一键执行</b>'
    +'<span class="faint">'+(ONECLICK.running?'执行中…':(ONECLICK.result||'记录'))+'</span></div><div class="cb">';
  for(const s of ONECLICK.steps){
    h+='<div class="flex" style="padding:4px 0;border-bottom:1px solid var(--line)">'
      +'<span class="tag acc" style="min-width:64px;justify-content:center">'+esc(s.n)+'</span>'
      +'<span class="faint" style="font-size:11.5px;min-width:70px">'+esc(s.at)+'</span>'
      +'<span style="font-size:13px">'+esc(s.msg)+'</span></div>';
  }
  h+='</div></div>';
  return h;
}

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

/** copilot 系平台 id —— 合并卡 key='copilot'，会话匹配要按这一族判断 */
const COPILOT_FAMILY=['wb-gateway','wb-gateway-intl','apk-codebuddy','apk-codebuddy-cn'];

/** 登录会话区（扫码 / Cookie 粘贴）。
 *  onlyPid：来源平台 key。传入时只显示该平台的会话（copilot 卡匹配整族），
 *  防止 A 平台登录中、B 平台详情页串台显示 A 的二维码和取消按钮；
 *  不传（账号凭据→当前登录 tab）则始终显示。 */
function viewLoginSession(onlyPid){
  const sess=S.data.loginSession;
  if(sess && onlyPid){
    const match = sess.platform===onlyPid
      || (onlyPid==='copilot' && COPILOT_FAMILY.indexOf(sess.platform)>=0);
    if(!match){
      return '<div class="note">「'+esc(sess.platform_name||sess.platform)+'」正在登录中，'
        +'与当前平台无关。可在「账号与凭据 → 当前登录」查看或取消。</div>';
    }
  }
  let h='';
  if(sess){
    h+='<div class="card" style="margin-bottom:14px"><div class="ch"><b>登录会话</b>'
      +'<span id="lsStatus">'+loginStatusTag(sess)+'</span></div><div class="cb">';
    h+='<div class="row" style="align-items:flex-start">';
    h+='<div class="n" id="lsQr">'+qrOrLinkHTML(sess)+'</div>';
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
  }else{
    h+='<div class="note">当前没有进行中的登录。去「登录与凭据」选一个平台点「去登录」。</div>';
  }
  h+=viewProbe();
  h+=viewPlatformCaps(S.data.platformActions||{});
  return h;
}

/** 平台内部能力（签到/积分/探测端点），折叠展示 */
function viewPlatformCaps(acts){
  const pids=Object.keys(acts).filter(k=>Array.isArray(acts[k])&&acts[k].length);
  let h='<div class="card"><div class="ch"><b>平台内部能力</b>'
    +'<span class="faint">签到 / 任务 / 积分 / 兑换 / 送积分 / 探测端点</span></div><div class="cb">';
  h+=viewProbe();
  if(!pids.length){
    h+=empty('暂无。平台能力由后端接口表驱动，加了动作就会出现在这里。');
  }else{
    for(const pid of pids){
      h+='<details style="margin-bottom:10px"><summary style="cursor:pointer;font-weight:600;padding:6px 0">'
        +esc(pid)+' <span class="faint">（'+acts[pid].length+' 项）</span></summary>'
        +'<div class="btnrow" style="margin:8px 0 6px">'
        +'<button class="btn sm" onclick="platformProbe(\''+esc(pid)+'\')">🔍 探测端点</button></div>'
        +'<div class="btnrow">';
      for(const a of acts[pid]){
        const isLocal=!!a.local, isProxy=!!a.needs_proxy;
        const title=isLocal?('APP 本地网关路由，面板无法直接调用（'+esc(a.path)+'）')
          :(isProxy?('需代理：'+esc(a.path)):(esc(a.method||'GET')+' '+esc(a.path)));
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
        h+='<div class="row n" style="margin-top:6px">';
        for(const k of keys){
          h+='<input id="pa_'+esc(k)+'" placeholder="'+esc(k)+'" '
            +'style="padding:5px 8px;border:1px solid var(--line2);border-radius:7px;font-size:12px">';
        }
        h+='</div>';
      }
      h+='</details>';
    }
  }
  h+='</div></div>';
  return h;
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
    h+='<div class="n" id="lsQr">'+qrOrLinkHTML(sess)+'</div>';
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

function copyEndpoint(){
  const base=panelBaseUrl();
  copyText(base+'\napi_key = admin',()=>toast('已复制：'+base,'ok'));
}

const VIEWS={sources:viewSources,route:viewRoute,tools:viewTools,settings:viewSettings,
  checkin:viewCheckin,growth:viewGrowth,tasks:viewTasks};

const LOADERS={sources:loadSources,route:loadRoute,tools:loadTools,settings:loadSettings,
  checkin:loadCheckin,growth:loadGrowth,tasks:loadTasks};

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
    // 默认着陆（无 hash）也要拉数据；带 hash 且是合法视图则直达
    const target=TITLES[h]?h:'sources';
    const b=$(`.navbtn[data-v="${target}"]`);
    if(b)b.classList.add('on');
    switchView(target);
  });
  // 后台轮询总览
  setInterval(()=>{
    if(document.hidden)return;
    api('/api/overview').then(o=>{S.data.overview=o;$('#stl').textContent=
      '面板原生模式 · 账号池 '+(S.data.accounts||[]).length+' 个'+
      ' · 面板已运行 '+dur(o.uptime);}).catch(()=>{});
  },15000);
});
