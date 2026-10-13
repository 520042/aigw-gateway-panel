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
// ★ 2026-10-13（用户要求）：删掉「工具与集成」整栏；「任务中心」并入原任务
//   管理的手动任务，侧边栏只剩 设置与日志 / 任务中心 两个高级项。
const TITLES={sources:'接入源',route:'路由与模型',settings:'设置与日志',
  growth:'任务中心'};
/* 高级（二级）菜单里的视图：默认折叠，进入其中任一项时自动展开分组 */
const ADV=['settings','growth'];
function toggleAdv(){
  const sub=$('#advSub'), t=$('#advToggle');
  const open=sub.classList.toggle('open');
  t.classList.toggle('open',open);
  // 记住用户的手动选择（autoFitNav 之后不再覆盖它）
  NAV_MANUAL.adv = !open;
  if(open) autoFitNav();
}
function openAdv(){
  $('#advSub').classList.add('open');
  $('#advToggle').classList.add('open');
  NAV_MANUAL.adv = false;
  autoFitNav();
}
/* ★ 2026-10-12（用户要求「分析还有没有空间，有就不折叠」）：
   侧边栏分组按**实际可用空间**动态决策 —— 默认全展开；只有侧边栏
   真的会溢出时才从后往前逐组收起（留 24px 余量）。用户手动折叠过
   的分组（NAV_MANUAL）保持尊重，不自动展开。 */
let NAV_MANUAL = {adv:false};
function autoFitNav(){
  const side=document.querySelector('.side');
  if(!side) return;
  const sub=$('#advSub'), t=$('#advToggle');
  if(!sub||!t) return;
  if(!NAV_MANUAL.adv){
    sub.classList.add('open');
    t.classList.add('open');
  }
  let guard=0;
  while(side.scrollHeight > side.clientHeight+24 && guard++<8){
    if(!sub.classList.contains('open')) break;
    sub.classList.remove('open');
    t.classList.remove('open');
  }
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
    const o=await api('api/overview');
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
  // 一键执行按钮只在任务中心显示
  const oneClickBtn=$('#hdOneClick');
  if(oneClickBtn) oneClickBtn.style.display = (S.v==='growth') ? '' : 'none';
  // ★ 侧边栏分组按实际空间决定展开/收起（有空间就不折叠）
  autoFitNav();
}
async function switchView(v,force){
  // 切走视图 = 离开当前登录界面 → 停掉没成功的登录会话，浏览器立即释放
  if(v!==S.v)leaveLoginView();
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
    api('api/login?action=platforms').catch(()=>({platforms:[]})),
    api('api/accounts').catch(()=>({accounts:[],stats:{}})),
    api('api/autocheckin?action=status').catch(()=>({platforms:[]})),
    api('api/models?action=sources').catch(()=>({sources:[]})),
    api('api/models').catch(()=>({models_enriched:[],rate_summary:{}})),
    api('api/overview').catch(()=>({})),
    api('api/custom_apis?action=list').catch(()=>({apis:[]})),
    // ★ 2026-10-07：接入源卡片要显示「模型 已启用/总数」，grouped 是唯一带
    //   启用状态与凭据数的接口（并行拉，不拖慢首屏）
    api('api/models?action=grouped').catch(()=>({groups:[]})),
  ]).then(([lp,ac,au,src,md,ov,ca,gr])=>{
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
    S.data.customApis=ca.apis||[];
    S.data.grouped=gr||{groups:[]};
    render();
  }).catch(e=>toast('加载接入源失败：'+e.message,'err'));
}

// 手动刷新接入源数据
async function refreshSources(){
  toast('正在刷新…');
  try{
    await loadSources();
    toast('刷新完成','ok');
  }catch(e){
    toast('刷新失败：'+e.message,'err');
  }
}

// 全局刷新：刷新当前页面数据
async function globalRefresh(){
  toast('正在刷新…');
  try{
    const v = S.v;
    const loader = LOADERS[v];
    if(loader){
      S.loaded = '';  // 强制重新加载
      await loader();
    }
    toast('刷新完成','ok');
  }catch(e){
    toast('刷新失败：'+e.message,'err');
  }
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
  // 获取后端分好的类
  const srcKinds=S.data.srcKinds||{};
  function getCategory(pid){
    const k=srcKinds[pid];
    if(!k)return '账号接入';
    const kind=k.kind||'';
    if(kind==='API')return '开放平台 API';
    // LOCAL、WEB、其他都归到账号接入（扫码/设备码登录的）
    return '账号接入';
  }
  
  // 0) 自定义 API（放在最前面）
  const customApis = S.data.customApis || [];
  for(const ca of customApis){
    out.push({
      key: ca.id, name: ca.name, kind: 'API', category: '开放平台 API',
      desc: ca.base_url,
      method: 'file',
      logged: !!ca.api_key,
      endpoint: ca.base_url,
      models: ca.model_count || 0,
      withRate: 0, checkin: false,
      canModels: true,
      custom: true,
      protocol: ca.protocol || 'openai',
    });
  }
  // 加一个"新建自定义 API"的卡片（也放在最前面）
  out.push({
    key: 'custom-new', name: '➕ 新建自定义 API', kind: 'API', category: '开放平台 API',
    desc: '自定义名称、URL、Key，自动同步模型',
    method: 'new_custom',
    logged: false, endpoint: '',
    models: 0, withRate: 0, checkin: false,
    canModels: false,
    custom_new: true,
  });
  
  // 官方API（只需要填API key）
  // ★ 2026-10-13：url 必须为可直连的 OpenAI 兼容前缀（router.py / custom_apis.py
  //   会往后拼 /chat/completions、/models）。以下 base_url 均经官方文档核验：
  //   - 多数厂商用 /v1；Meta 用 /compat/v1；商汤用 /compatible-mode/v1；Cohere 用 /compatibility/v1；
  //   - 智谱 /v4、千帆 /v2、火山 /api/v3、通义 /compatible-mode/v1、有道 /llmgateway/api/v1 为各自原生前缀。
  const officialApis=[
    // 海外官方API
    {id:'openai', name:'OpenAI', desc:'OpenAI 官方', url:'https://api.openai.com/v1'},
    {id:'anthropic', name:'Claude', desc:'Anthropic 官方', url:'https://api.anthropic.com/v1'},
    {id:'google', name:'Gemini', desc:'Google AI 官方', url:'https://generativelanguage.googleapis.com/v1beta/openai'},
    {id:'xai', name:'Grok', desc:'xAI 官方', url:'https://api.x.ai/v1'},
    {id:'meta', name:'Llama', desc:'Meta 官方', url:'https://api.llama.com/compat/v1'},
    {id:'mistral', name:'Mistral', desc:'Mistral 官方', url:'https://api.mistral.ai/v1'},
    {id:'cohere', name:'Cohere', desc:'Cohere 官方', url:'https://api.cohere.ai/compatibility/v1'},
    {id:'groq', name:'Groq', desc:'高速推理', url:'https://api.groq.com/openai/v1'},
    {id:'together', name:'Together', desc:'开源模型', url:'https://api.together.xyz/v1'},
    {id:'fireworks', name:'Fireworks', desc:'开源模型', url:'https://api.fireworks.ai/inference/v1'},
    {id:'deepinfra', name:'DeepInfra', desc:'开源模型', url:'https://api.deepinfra.com/v1/openai'},
    {id:'perplexity', name:'Perplexity', desc:'AI 搜索', url:'https://api.perplexity.ai/v1'},
    {id:'replicate', name:'Replicate', desc:'模型托管', url:'https://api.replicate.com/v1'},
    {id:'nvidia', name:'NVIDIA NIM', desc:'GPU 推理（免费）', url:'https://integrate.api.nvidia.com/v1'},
    {id:'aws_bedrock', name:'AWS Bedrock', desc:'亚马逊云（需填完整地址）', url:'https://bedrock-runtime.us-east-1.amazonaws.com'},
    {id:'azure_openai', name:'Azure OpenAI', desc:'微软云', url:'https://{your-resource}.openai.azure.com/openai'},
    {id:'huggingface', name:'Hugging Face', desc:'HF 推理端点', url:'https://router.huggingface.co/v1'},
    {id:'stability', name:'Stability AI', desc:'图像生成', url:'https://api.stability.ai/v1'},
    {id:'openrouter', name:'OpenRouter', desc:'聚合 100+ 模型', url:'https://openrouter.ai/api/v1'},
    {id:'blackforest', name:'FLUX', desc:'图像生成', url:'https://api.bfl.ml/v1'},
    {id:'elevenlabs', name:'ElevenLabs', desc:'语音合成', url:'https://api.elevenlabs.io/v1'},
    // 国内官方API
    {id:'deepseek', name:'DeepSeek', desc:'深度求索', url:'https://api.deepseek.com/v1'},
    {id:'zhipu', name:'智谱清言', desc:'智谱 AI', url:'https://open.bigmodel.cn/api/paas/v4'},
    {id:'baichuan', name:'百川', desc:'百川智能', url:'https://api.baichuan-ai.com/v1'},
    {id:'qwen', name:'通义千问', desc:'阿里云百炼', url:'https://dashscope.aliyuncs.com/compatible-mode/v1'},
    {id:'wenxin', name:'文心一言', desc:'百度千帆', url:'https://qianfan.baidubce.com/v2'},
    {id:'minimax', name:'MiniMax', desc:'MiniMax', url:'https://api.minimax.io/v1'},
    {id:'stepfun', name:'阶跃星辰', desc:'StepFun', url:'https://api.stepfun.com/v1'},
    {id:'siliconflow', name:'硅基流动', desc:'SiliconFlow', url:'https://api.siliconflow.cn/v1'},
    {id:'tencent', name:'混元', desc:'腾讯云', url:'https://api.hunyuan.cloud.tencent.com/v1'},
    {id:'xfyun', name:'讯飞星火', desc:'科大讯飞', url:'https://spark-api-open.xf-yun.com/v1'},
    {id:'sensechat', name:'商汤日日新', desc:'商汤科技', url:'https://api.sensenova.cn/compatible-mode/v1'},
    {id:'volcengine', name:'豆包', desc:'火山引擎', url:'https://ark.cn-beijing.volces.com/api/v3'},
    {id:'360gpt', name:'360智脑', desc:'360', url:'https://api.360.cn/v1'},
    {id:'youdao', name:'网易有道', desc:'网易', url:'https://openapi.youdao.com/llmgateway/api/v1'},
    {id:'huawei', name:'华为盘古', desc:'华为云', url:'https://api.modelarts-maas.com/openai/v1'},
    {id:'modelscope', name:'魔搭', desc:'阿里达摩院', url:'https://api.modelscope.cn/v1'},
  ];
  
  for(const p of officialApis){
    const pid='api-'+p.id;
    // ★ 2026-10-12：loginPlatforms 里同 id 的平台（api-deepseek 等）带控制台
    //   地址与登录态 —— 这里合并进去，避免下面第 3 段再推一张重复卡。
    const lp=(S.data.loginPlatforms||[]).find(x=>x.id===pid);
    out.push({
      key:pid, name:p.name, kind:'API', category:'开放平台 API',
      desc:p.desc,
      method:'api_key',
      login_url:(lp&&lp.login_url)||'',
      paste_type:'api_key',
      placeholder:(lp&&lp.placeholder)||'',
      logged:logged.has(pid), endpoint:p.url,
      models:0, withRate:0, checkin:false,
      canModels:false, native:true, subType:p.type,
    });
  }
  
  // 2.5) 公益中转站（只放第三方公益站，官方平台已在开放平台API里）
  // 后端 all_sites() 已给每条 site 打 category：'relay'=第三方公益站，'official'=官方平台。
  // 直接按 category 过滤，避免官方平台（阿里云百炼/百度千帆/硅基流动/讯飞/Groq/OpenRouter…）
  // 因为名字没命中硬编码 officialNames 而漏进公益中转站，造成与开放平台API重复。
  const freeSites = (S.data.catalogSites || []).filter(s => s.category === 'relay');
  for(const s of freeSites){
    out.push({
      key:'free-'+s.name, name:s.name, kind:'公益站', category:'公益中转站',
      desc:s.bonus || '公益中转站',
      method:'api_key',
      // ★ 2026-10-13：用 api_url（网站根址 + /v1）而非纯网站根址，否则抽屉预填 URL 缺前缀
      logged:false, endpoint:s.api_url || s.url,
      models:0, withRate:0, checkin:!!s.checkin,
      canModels:false, native:false,
    });
  }
  
  // 3) 可登录的平台 —— WorkBuddy 国内/国际两张卡合并成一张
  const COPILOT_IDS={'wb-gateway':'国内站','wb-gateway-intl':'国际站'};
  let copilotDone=false;
  // Kimi 国内/国际也合并成一张
  const KIMI_IDS={'sub-kimi':'国内站','sub-kimi-ai':'国际站'};
  let kimiDone=false;
  // ★ 2026-10-12：api-* 已在 officialApis 段推过卡（并带上了控制台地址），
  //   这里跳过，否则同一 key 出两张重复卡。
  const pushedKeys=new Set(out.map(x=>x.key));
  for(const p of (S.data.loginPlatforms||[])){
    if(pushedKeys.has(p.id)) continue;
    if(COPILOT_IDS[p.id]){
      if(copilotDone)continue;
      copilotDone=true;
      const anyLogged=['wb-gateway','wb-gateway-intl'].some(x=>logged.has(x));
      out.push({
        key:'copilot', name:'WorkBuddy', kind:'账号接入', category:'账号接入',
        desc:'国内/国际独立账号，扫码登录',
        method:'qrcode', merged:true,
        variants:[
          {pid:'wb-gateway', label:'国内站账号', logged:logged.has('wb-gateway')},
          {pid:'wb-gateway-intl', label:'国际站账号', logged:logged.has('wb-gateway-intl')},
        ],
        logged:anyLogged, endpoint:'copilot.tencent.com',
        models:0, withRate:0, checkin:false, canModels:false,
      });
      continue;
    }
    if(KIMI_IDS[p.id]){
      if(kimiDone)continue;
      kimiDone=true;
      const anyLogged=['sub-kimi','sub-kimi-ai'].some(x=>logged.has(x));
      out.push({
        key:'kimi', name:'Kimi Code', kind:'账号接入', category:'账号接入',
        desc:'国内/国际独立账号，设备码登录',
        method:'oauth_device', merged:true,
        variants:[
          {pid:'sub-kimi', label:'国内站', logged:logged.has('sub-kimi')},
          {pid:'sub-kimi-ai', label:'国际站', logged:logged.has('sub-kimi-ai')},
        ],
        logged:anyLogged, endpoint:'auth.kimi.com',
        models:0, withRate:0, checkin:false, canModels:false,
      });
      continue;
    }
    const pl=plan[p.id];
    let desc=p.hint||'';
    if(desc.length>60) desc=desc.slice(0,60)+'…';
    out.push({
      key:p.id, name:p.name, kind:getCategory(p.id), category:getCategory(p.id),
      desc:desc, method:p.method,
      no_remote:!!p.no_remote_scan,
      browser_capture:!!p.browser_capture,
      paste_type:p.paste_type||'',
      placeholder:p.placeholder||'',
      login_url:p.login_url||'',
      logged:logged.has(p.id), endpoint:p.upstream||'',
      models:0, withRate:0,
      checkin:!!(pl&&pl.has_public_checkin),
      at:pl?pl.time:'', canModels:canModels.has(p.id),
      on:pl?pl.on:true, done:pl?pl.done_today:false,
    });
  }
  
  return out;
}

/* ★ 2026-10-13（用户要求：「工具集成一栏好像也没啥用了，可以不要了」）：
   原「工具与集成」页（工具调用 / 本地反代 / 签到计划 / 账号与凭据四个 tab）
   整栏删除。viewLocalproxy 仍被接入源抽屉引用，予以保留。 */

/** 路由与模型：合并了原来的「智能路由」「模型与倍率」「上游档案」「用量看板」 */
let ROUTE_TAB = 'models';

async function loadRoute(){
  // ★ grouped 先行并行拉（模型总表是本页主体），不被任何慢请求串行拖住
  api('api/models?action=grouped').then(g=>{S.data.grouped=g;render();}).catch(()=>{});
  loadFreeModels();   // 每日免费模型榜（非阻塞，失败不影响本页）
  const [rt,us]=await Promise.all([
    api('api/route').catch(()=>({})),
    api('api/usage?range=all').catch(()=>({})),
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

/* ★ 2026-10-07 用户反馈「接入源那块排布很不整齐，介绍部分太多」——重写：
 *   1) 顶部 4 张 KPI 卡 + 一整段三步说明 → 压成「一条状态条 + 默认折叠的用法」
 *      （原来那 4 张卡里有硬编码的假数据：『③自动路由 3』『已启用』『relays||9』，
 *        现在全部换成真实统计）
 *   2) 卡片改等高：说明固定 2 行截断、tag 行定高、按钮贴底
 *      （原来 desc 用 min-height:48px，LAN_APK 那种长说明能撑到 6 行，
 *        copilot 卡还内嵌 4 条子账号列表 → 整片卡片高矮不一）
 *   3) 补搜索 + 已接入/未接入筛选（参考 new-api 渠道列表的做法） */
let SRC_HELP = false;          // 「怎么用」是否展开
let SRC_F = 'all';             // 筛选：all | on | off | proxy
let SRC_Q = '';                // 搜索关键词
// ★ 2026-10-07 重构（照 dsh-our-free-model 的来源卡片）：
//   顶部分类 tab + 卡片上直接给「账号 n / 模型 启用数·总数 / 积分」+ 卡片内
//   可展开账号与模型清单（模型行带启用开关）。
let SRC_CAT = '全部';          // 当前分类 tab
let SRC_ACCT_OPEN = {};        // 卡片内「账号」区展开态
let SRC_MOD_OPEN = {};         // 卡片内「模型」区展开态
let SRC_EXPAND = null;         // 卡片下方「登录路径」内联展开态（key）
let SRC_POP_ANIM = false;      // 抽屉入场动画只播一次（轮询重绘不重播）

/** 分类归属：免凭据车道单独成组（照参考项目的「免费模型 / 白嫖」分组） */
function srcCatOf(s){
  const grp = ((S.data.grouped||{}).groups||[]).find(g=>g.pid===s.key);
  if(grp && grp.free_lane) return '免凭据白嫖';
  return s.category || '其他';
}
function srcCatCounts(items){
  const m = {};
  for(const s of items){ const c = srcCatOf(s); m[c] = (m[c]||0)+1; }
  return m;
}
function setSrcCat(c){ SRC_CAT = c; render(); }
function srcToggleMod(k){
  SRC_MOD_OPEN[k] = !(SRC_MOD_OPEN[k] !== false);
  // ★ 2026-10-13：只局部换抽屉模型区，不整页 render()（点收起也闪）
  srcPatch(k);
}

/** 懒拉某个源的模型清单（只有平台动作/内置表能给的才有），结果缓存在内存 */
async function refreshSrcModels(pid){
  toast('正在拉取 '+pid+' 的模型清单…');
  try{
    const r = await post('api/models?action=platform_models', {platform: pid});
    if(r.ok === false){ toast('拉取失败：'+(r.message||''), 'err'); return; }
    S.data.srcModels = S.data.srcModels || {};
    S.data.srcModels[pid] = r.models || [];
    SRC_MOD_OPEN[pid] = true;
    toast('已获取 '+((r.models||[]).length)+' 个模型'+(r.live?'（实时清单）':'（内置清单）'), 'ok');
    // ★ 2026-10-13（用户三次反馈「点抽屉刷新模型界面就闪一下」）：
    //   原先这里 loadModels()（内部 render() 整页重绘）+ 本函数末尾又 render()
    //   一次，一帧内全量替换两次 innerHTML → 卡片墙连抽屉拆掉重建，就是「闪」。
    //   现在：① grouped 后台静默更新（路由页切过去时 switchView→render 读新数据，
    //   「路由与模型」页照样马上能看到新清单）；② 抽屉/卡片只局部替换（srcPatch）。
    api('api/models?action=grouped').then(g=>{ if(g&&g.groups) S.data.grouped=g; }).catch(()=>{});
    srcPatch(pid);
  }catch(e){ toast('拉取失败：'+e.message, 'err'); }
}
/** 删掉某个源下的一个账号 */
async function delSrcAccount(id, name){
  if(!confirm('删除账号 '+(name||id)+' ？该操作只影响本机账号池。')) return;
  const r = await post('api/accounts?action=delete', {id});
  if(r.ok===false){ toast('删除失败：'+(r.message||''), 'err'); return; }
  toast('已删除','ok');
  // ★ 2026-10-13：删除后只局部刷新账号池 + 卡片/抽屉，不整页 render（不闪）
  try{
    const a2=await api('api/accounts');
    if(a2&&a2.accounts){S.data.accounts=a2.accounts;S.data.accountStats=a2.stats||S.data.accountStats;}
  }catch(e){}
  if(SRC_EXPAND) srcPatch(SRC_EXPAND);
}

function srcFilter(list){
  const q=(SRC_Q||'').trim().toLowerCase();
  return list.filter(s=>{
    if(SRC_F==='on' && !s.logged) return false;
    if(SRC_F==='off' && s.logged) return false;
    if(SRC_F==='proxy' && s.kind!=='本地反代') return false;
    if(!q) return true;
    return ((s.name||'')+' '+(s.endpoint||'')+' '+(s.desc||'')+' '+(s.key||''))
             .toLowerCase().indexOf(q) >= 0;
  });
}

/** 渲染卡片网格（纯函数，供首次渲染与搜索/筛选局部重绘共用） */
function srcCards(items){
  if(!items.length)
    return '<div class="empty">没有匹配的源，换个关键词或点「全部」</div>';
  let h='';
  for(const s of items) h+=srcCardRow(s);
  return h;
}

/** ★ 2026-10-13：单张卡片行的 HTML（从 srcCards 抽出）。
 *  存在意义：srcPatch 局部替换用（row.outerHTML=srcCardRow(s)）——
 *  整页 render() 的 $('#view').innerHTML 全量替换就是「点刷新界面闪一下」的根因。
 *  data-sk = 卡片 key，srcPatch 靠它定位这一行。 */
function srcCardRow(s){
  const on=!!s.logged;
  const expanded = SRC_EXPAND===s.key;
  // 「新建自定义 API」入口：需要独立表单，单独一行
  if(s.custom_new){
    return '<div class="sr-row" onclick="openSrc(\'custom-new\')">'
      +'<span class="sdot"></span>'
      +'<div class="sr-name"><b>'+esc(s.name)+'</b>'
      +'<span class="faint">'+esc(s.desc||'')+'</span></div>'
      +'<button class="btn sm pri" onclick="event.stopPropagation();openSrc(\'custom-new\')">新建</button>'
      +'</div>';
  }
    // ★ 2026-10-12（用户实测截图：WorkBuddy 合并卡「账号 0」但明明已登录）：
    //   合并卡 key 是 'copilot'/'kimi'，账号池里存的是 variants 的 pid
    //   （wb-gateway/wb-gateway-intl）—— 按 s.key 过滤永远是 0。
    //   统一按 variants 的 pid 集合过滤账号、查 grouped 分组。
    const vpids=(s.variants||[]).map(v=>v.pid);
    const accts=(S.data.accounts||[]).filter(a=>
      vpids.length ? (vpids.indexOf(a.platform)>=0) : (a.platform===s.key));
    const grp=((S.data.grouped||{}).groups||[]).find(gr=>
      vpids.length ? (vpids.indexOf(gr.pid)>=0) : (gr.pid===s.key));
    // ★ 2026-10-13：refreshSrcModels 按**卡片 key**（合并卡是 'copilot'）存，
    //   这里必须优先按 s.key 读、再回落 variants[0] 的 pid —— 此前只读
    //   vpids[0]（'wb-gateway'）与写入键（'copilot'）不一致，拉到的清单
    //   永远显示不出来，卡片/抽屉只能回落 grouped 的老数据。
    const _sm=S.data.srcModels||{};
    const lazy=_sm[s.key] || (vpids.length ? _sm[vpids[0]] : null);
    const mods=lazy||(grp?grp.models:null);
    const modTot=mods?mods.length:0;
    const modOn=mods?mods.filter(m=>m.on!==false).length:0;
    // ★ 2026-10-13（用户要求）：卡片外面不显示积分/倍率了 —— 账号的实时
    //   积分余额改在抽屉详情里按账号行显示（见 srcDrawer）。
    // ★ 2026-10-12 终版（new-api 渠道列表范式）：所有源统一一行 ——
    //   两段式紧凑卡：上行=状态+名称+分类；下行=详情+账号+模型+积分+⟳。
    //   外层 .src-list 是 auto-fill 网格 —— 宽度/缩放不同，列数自动变。
    return '<div class="sr-row'+(on?' on':'')+(expanded?' open':'')+'" data-sk="'+esc(s.key)+'" onclick="expandSrc(\''+esc(s.key)+'\')">'
      +'<div class="sr-top">'
      +'<span class="sdot'+(on?' ok':'')+'" title="'+(on?'已接入':'未接入')+'"></span>'
      +'<div class="sr-name">'
      +'<b title="'+esc(s.name)+'">'+esc(s.name)+'</b>'
      +'<span class="faint" title="'+esc(s.endpoint||'')+'">'+esc(s.endpoint||s.desc||'—')+'</span>'
      +'</div>'
      +'<span class="tag info sr-cat">'+esc(s.kind||'')+'</span>'
      +'</div>'
      +'<div class="sc-f">'
      +'<button class="btn sm'+(on?'':' pri')+'" onclick="event.stopPropagation();expandSrc(\''+esc(s.key)+'\')">'+(expanded?'收起':(on?'详情':'接入'))+'</button>'
      +'<span class="sc-stat" title="账号数">账号 <b>'+accts.length+'</b></span>'
      +'<span class="sc-stat" title="模型（启用/总数）">模型 <b>'+(mods?(modOn+'/'+modTot):(s.models||0))+'</b></span>'
      +'<button class="btn sm gh" style="margin-left:auto" title="刷新账号状态与模型清单" onclick="event.stopPropagation();refreshSrcCard(\''+esc(s.key)+'\')">⟳</button>'
      +'</div>'
      +'</div>';
}

/** 抽屉「账号区」HTML（独立块：srcPatch 局部替换用）。
 *  刷新账号状态不再整页 render() —— 那是抽屉「闪一下」的根因。 */
function srcAcctBoxHtml(s){
  // ★ 合并卡按 variants 的 pid 过滤（key 是 copilot/kimi，账号存 wb-gateway 等）
  const vpids=(s.variants||[]).map(v=>v.pid);
  const accts=(S.data.accounts||[]).filter(a=>
    vpids.length ? (vpids.indexOf(a.platform)>=0) : (a.platform===s.key));
  return '<div class="sc-sect"><i>账号（'+accts.length+'）</i>'
    +'<button class="btn sm gh" onclick="refreshSrcAccounts(\''+esc(s.key)+'\')">⟳ 刷新状态与积分</button></div>'
    +'<div class="sc-sub">'+(accts.length?accts.map(a=>
      '<div class="sc-row">'
      +'<span class="mono" title="'+esc(a.name||'')+'">'+esc(a.name||a.platform)+'</span>'
      +'<span class="faint">'+esc(a.secret||'')+(a.status&&a.status!=='unknown'?' · '+esc(a.status):'')+'</span>'
      +(a.balance?'<span class="tag acc" title="实时积分余额（15 分钟缓存）">积分 '+esc(a.balance)+'</span>':'')
      +'<button class="btn sm dgr" onclick="delSrcAccount(\''+esc(a.id)+'\',\''+esc(a.name||'')+'\')">删除</button>'
      +'</div>').join(''):'<div class="faint" style="padding:6px 0;font-size:11.5px">还没有账号，完成上方登录后会自动出现在这里</div>')+'</div>';
}

/** 抽屉「模型区」HTML（独立块：srcPatch 局部替换用）。
 *  拉取清单 / 收起展开只换这块，不整页 render()。 */
function srcModBoxHtml(s){
  const vpids=(s.variants||[]).map(v=>v.pid);
  const grp=((S.data.grouped||{}).groups||[]).find(gr=>
    vpids.length ? (vpids.indexOf(gr.pid)>=0) : (gr.pid===s.key));
  // ★ 2026-10-13：与 srcCards 同键修复 —— 按 s.key 读（拉取时按它存）
  const _sm=S.data.srcModels||{};
  const lazy=_sm[s.key] || (vpids.length ? _sm[vpids[0]] : null);
  const mods=lazy||(grp?grp.models:null);
  const modOn=mods?mods.filter(m=>m.on!==false).length:0;
  const modTot=mods?mods.length:0;
  // ★ 2026-10-13（用户要求）：模型清单默认展开，点「收起清单」才折起来
  const modOpen = SRC_MOD_OPEN[s.key] !== false;
  return '<div class="sc-sect"><i>模型（'+modOn+'/'+modTot+'）</i>'
    +(mods?'<button class="btn sm gh" onclick="srcToggleMod(\''+esc(s.key)+'\')">'+(modOpen?'收起清单':'展开清单')+'</button>'
          :'<button class="btn sm gh" onclick="refreshSrcModels(\''+esc(s.key)+'\')">拉取清单</button>')
    +'<button class="btn sm gh" onclick="refreshSrcModels(\''+esc(s.key)+'\')">⟳ 刷新</button></div>'
    +((modOpen&&mods)?'<div class="sc-sub">'+mods.slice(0,40).map(m=>
      '<div class="sc-row" title="'+(m.ctx?('上下文 '+Number(m.ctx).toLocaleString()+' / '):'')+(m.out?('输出 '+Number(m.out).toLocaleString()):'')+'">'
      +'<span class="mono">'+esc(m.id)+'</span>'
      +'<span class="faint" style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">'+esc(m.name||'')+'</span>'
      +(m.credits?'<span class="tag acc" title="积分倍率">'+esc(gmCred(m.credits))+'</span>':'<span class="faint">—</span>')
      +'</div>').join('')+'</div>':'');
}

/** ★ 2026-10-13（用户三次反馈「点抽屉里的刷新模型，界面就闪一下」）：
 *  局部补丁 —— 所有抽屉内刷新（模型/账号/删除/卡片⟳）只走这里，
 *  **不再整页 render()**。根因：render() 是 $('#view').innerHTML 整页替换，
 *  卡片墙连抽屉一起拆掉重建，看着就是「闪一下」；且 refreshSrcModels 原先
 *  loadModels() 内部 render() + 函数末尾 render() 一帧内闪两次。
 *  只换：① 卡片行（账号数/模型数/接入态）② 抽屉账号区 ③ 抽屉模型区。
 *  登录面板不碰（输入框焦点、扫码轮询都不受影响），抽屉动画不重播。 */
function srcPatch(pid){
  const s=sourceList().find(x=>x.key===pid);
  if(!s) return;
  // 1) 卡片行（被搜索词过滤掉时 row 为 null，只补抽屉）
  try{
    const row=document.querySelector('.sr-row[data-sk="'+pid.replace(/"/g,'\\"')+'"]');
    if(row) row.outerHTML=srcCardRow(s);
  }catch(e){}
  // 2) 抽屉两个区（开着才补；API 类源抽屉没有这两个盒，自然落空）
  if(SRC_EXPAND===pid){
    const ab=document.getElementById('srcAcctBox');
    if(ab) ab.innerHTML=srcAcctBoxHtml(s);
    const mb=document.getElementById('srcModBox');
    if(mb) mb.innerHTML=srcModBoxHtml(s);
  }
}

/** 右侧抽屉：某源的详情面（登录流程 + 账号 + 模型，new-api Drawer 范式）
 *  animate=false → 不加入场动画（轮询重绘时不能重播，否则每 2 秒闪一次） */
function srcDrawer(s, animate){
  const on=!!s.logged;
  let body='';
  // ★ 2026-10-12（用户反馈：公益中转站等 api_key 平台也被迫「先点接入再粘贴」，
  //   与开放平台 API 的「直接输入 Key」不一致，多此一举）：直接输入 Key 的判定
  //   从 kind==='API' 扩展到 method==='api_key' —— 凡纯粘贴 Key/Token 的平台
  //   （官方 API、公益中转站等）一律内联输入框 + 接入按钮，不再绕一圈「接入」按钮。
  //   saveApiKey(key) 从抽屉输入框读取 URL+Key 后新增自定义 API，不会错配。
  if(s.kind==='API' || s.method==='api_key'){
    // ★ 2026-10-13（用户反馈：平台 API / 公益站要把 URL 栏也显示出来，去掉
    //   「方式一 · 跳转」「方式二 · 粘贴 API Key」文案，按 URL / KEY / 模型获取排布；
    //   URL 默认填好 endpoint，用户可手动修改）。
    const defaultUrl = esc(s.endpoint||'');
    body='<div style="display:flex;flex-direction:column;gap:10px">'
      +'<div style="display:flex;flex-direction:column;gap:4px">'
      +'<span class="faint" style="font-size:11px">API URL</span>'
      +'<input type="text" placeholder="https://..." style="width:100%;box-sizing:border-box" id="api_url_'+esc(s.key)+'" value="'+defaultUrl+'">'
      +'</div>'
      +'<div style="display:flex;flex-direction:column;gap:4px">'
      +'<span class="faint" style="font-size:11px">API Key</span>'
      +'<div style="display:flex;gap:6px">'
      +'<input type="password" placeholder="填入 API Key..." style="flex:1" id="api_key_'+esc(s.key)+'">'
      +'<button class="btn sm pri" onclick="saveApiKey(\''+esc(s.key)+'\')">'+(on?'更新':'接入并获取模型')+'</button>'
      +'</div></div>'
      +'</div>';
  }else{
    // ★ 2026-10-13：账号区/模型区各自包一层带 id 的盒（srcAcctBox/srcModBox），
    //   srcPatch 局部替换这两块的 innerHTML 即可，不用整页 render()（不闪）。
    body=srcLoginPanel(s, true)
      +'<div id="srcAcctBox">'+srcAcctBoxHtml(s)+'</div>'
      +'<div id="srcModBox">'+srcModBoxHtml(s)+'</div>';
  }
  return '<div class="sc-mask" onclick="closeSrcPop()"></div>'
    +'<div class="sc-drawer'+(animate?'':' no-anim')+'" onclick="event.stopPropagation();">'
    +'<div class="dr-h"><b title="'+esc(s.name)+'">'+esc(s.name)+'</b>'
    +'<span class="tag '+(on?'ok':'')+'">'+(on?'已接入':'未接入')+'</span>'
    +'<button class="btn sm gh" style="margin-left:auto" onclick="closeSrcPop()">✕ 关闭</button></div>'
    +(s.desc?'<div class="faint" style="font-size:11.5px;margin:2px 0 8px">'+esc(s.desc)+'</div>':'')
    +body
    +'</div>';
}

/** 关闭抽屉 */
function closeSrcPop(){
  SRC_EXPAND=null;
  render();
}

/** 卡片 ⟳：统一刷新 —— 账号状态 + 平台态；该平台拉过模型清单的话连模型一起刷 */
async function refreshSrcCard(pid){
  toast('正在刷新 '+pid+' …');
  try{
    // ★ 2026-10-13：不再 loadSources()（它末尾 render() 整页重绘，抽屉开着也闪）。
    //   只拉账号池 + grouped 进 S.data，再走 srcPatch 局部补丁。
    const [ac,gr]=await Promise.all([
      api('api/accounts').catch(()=>null),
      api('api/models?action=grouped').catch(()=>null),
    ]);
    if(ac&&ac.accounts){S.data.accounts=ac.accounts;S.data.accountStats=ac.stats||S.data.accountStats;}
    if(gr&&gr.groups)S.data.grouped=gr;
    srcPatch(pid);
    if((S.data.srcModels||{})[pid]) await refreshSrcModels(pid);
    else toast('已刷新','ok');
  }catch(e){ toast('刷新失败：'+e.message,'err'); }
}

/** 刷新某个源的账号状态（refresh=1 让后端绕过 TTL 强刷各账号积分余额） */
async function refreshSrcAccounts(pid){
  toast('正在刷新账号状态与积分…');
  try{
    const r=await api('api/accounts',{refresh:1});
    if(r&&r.accounts){S.data.accounts=r.accounts;S.data.accountStats=r.stats||S.data.accountStats;}
  }catch(e){}
  // ★ 2026-10-13：不再 loadSources()（末尾 render() 整页重绘 → 抽屉闪一下）。
  //   账号数据已进 S.data，srcPatch 局部刷新抽屉账号区 + 卡片即可。
  srcPatch(pid);
  toast('账号状态与积分已刷新','ok');
}

/** 保存 API Key 并获取模型（从抽屉 URL/Key 输入框读取） */
async function saveApiKey(key){
  const urlInput = document.getElementById('api_url_'+key);
  const keyInput = document.getElementById('api_key_'+key);
  if(!urlInput || !keyInput){
    toast('找不到输入框', 'err');
    return;
  }
  const url = urlInput.value.trim();
  const apiKey = keyInput.value.trim();
  if(!url){
    toast('请输入 API URL', 'warn');
    return;
  }
  if(!apiKey){
    toast('请输入 API Key', 'warn');
    return;
  }
  
  toast('正在保存并获取模型...', 'info');
  
  try{
    // ★ 2026-10-07 重写：原来调的是 api/custom_apis?action=add_from_preset ——
    //   后端根本没有这个 action（只认 list/add/update/delete/sync），必然失败；
    //   而且请求体还被 {method,body} 二次包装（api() 只收 (path, body)）。
    //   现在走两条真实存在的链路：
    //     ① 已知官方平台（api-<id>）：写进账号池，platform_models 会用 Bearer
    //        拉实时模型清单；
    //     ② 同时建一条自定义 API（任意 base_url + key 都能直接当上游用）。
    let saved=false, msg='';
    const known=!!((S.data.srcKinds||{})['api-'+key]);
    if(known){
      const r=await post('api/accounts?action=add', {
        platform:'api-'+key, name:key+' 官方 Key', type:'api_key',
        secret:apiKey, source:'接入源页填写'});
      if(r.ok!==false){ saved=true; msg='已写入账号池'; }
      else msg=(r.message||'写入账号池失败');
    }
    const addRes=await post('api/custom_apis?action=add', {
      name:key+'（接入源）', protocol:'openai',
      base_url:url||'', api_key:apiKey});
    if(addRes.ok&&addRes.id){
      const sync=await post('api/custom_apis?action=sync', {id:addRes.id});
      msg=sync.ok?(sync.message||'已同步模型'):'已保存（模型同步失败）';
      saved=true;
    } else if(!saved){
      msg=(addRes.message||addRes.error||'保存失败');
    }
    if(saved){ toast('已接入：'+msg,'ok'); await loadSources(); }
    else { toast('保存失败：'+msg,'err'); }
  }catch(e){
    toast('保存失败：'+e.message, 'err');
  }
}

/** 切换路由详情展开/收起 */
function toggleRouteDetail(){
  const body = document.getElementById('routeDetailBody');
  const caret = document.getElementById('routeDetailCaret');
  if(body.style.display === 'none'){
    body.style.display = 'block';
    caret.textContent = '▾';
  } else {
    body.style.display = 'none';
    caret.textContent = '▸';
  }
}

function srcSegHtml(){
  const all=sourceList();
  const on=all.filter(i=>i.logged).length;
  return [['all','全部 '+all.length],
          ['on','已接入 '+on],
          ['off','未接入 '+(all.length-on)],
          ['proxy','本地反代']]
    .map(x=>'<button class="'+(SRC_F===x[0]?'on':'')+'" '
      +'onclick="SRC_F=\''+x[0]+'\';srcRenderGrid()">'+x[1]+'</button>').join('');
}

/** 分类 tab 条（照 dsh-our-free-model：一排分组，每组带 已接入/总数） */
let SRC_CATS_OPEN = false;   // ★ 分类竖排列表默认收起（显示三行，展开可滚动）
function toggleSrcCats(){
  SRC_CATS_OPEN = !SRC_CATS_OPEN;
  const box = document.getElementById('srcCatsList');
  if(box) box.classList.toggle('open', SRC_CATS_OPEN);
  const t = document.getElementById('srcCatsToggle');
  if(t) t.innerHTML = SRC_CATS_OPEN ? '▴ 收起' : '▾ 展开';
}
function srcCatsHtml(items){
  const catN = srcCatCounts(items);
  const names = Object.keys(catN).sort((a,b)=>catN[b]-catN[a]);
  // ★ 2026-10-12（用户反馈「这才几个怎么就收起了」）：**数量少就不折叠** ——
  //   分类 ≤5 个时全部直接显示（无展开按钮/无高度限制）；
  //   只有超过 5 个才默认收起三行 + 展开滚动。「全部」始终固定顶部。
  const collapsed = names.length > 5;
  let list = '';
  for(const c of names){
    const all = (c==='全部') ? items : items.filter(s=>srcCatOf(s)===c);
    if(!all.length) continue;
    const onN = all.filter(s=>s.logged).length;
    list += '<button class="'+(SRC_CAT===c?'on':'')+'" onclick="setSrcCat(\''+esc(c)+'\')">'
      + '<span class="cn">'+esc(c)+'</span><span class="faint">'+onN+'/'+all.length+'</span></button>';
  }
  return '<div class="src-cats-v" id="srcCats">'
    + '<button class="cat-all'+(SRC_CAT==='全部'?' on':'')+'" onclick="setSrcCat(\'全部\')">'
    + '<span class="cn">全部</span><span class="faint">'
    + items.filter(s=>s.logged).length+'/'+items.length+'</span></button>'
    + '<div class="cat-list'+(collapsed && !SRC_CATS_OPEN ? '' : ' open')+'" id="srcCatsList">'+list+'</div>'
    + (collapsed ? '<button class="cat-toggle" id="srcCatsToggle" onclick="toggleSrcCats()">'
      + (SRC_CATS_OPEN?'▴ 收起':'▾ 展开')+'</button>' : '')
    + '</div>';
}
/** 网格本体（受当前分类 tab 约束） */
function srcGridHtml(shown){
  const items = (SRC_CAT==='全部') ? shown : shown.filter(s=>srcCatOf(s)===SRC_CAT);
  if(!items.length)
    return '<div class="empty">这个分类下暂时没有源，点上面的「全部」看看</div>';
  return '<div class="src-list">'+srcCards(items)+'</div>';
}
/** ★ 2026-10-07：只重绘 tab + 网格（整页 render() 会让搜索框失焦）。
    原来这里直接 g.innerHTML=srcCards(...)，把 .srcgrid 包裹层和分类 tab 都丢了。 */
function srcRenderGrid(){
  const g=document.getElementById('srcGrid');
  if(!g) return;
  const items=srcFilter(sourceList());
  g.innerHTML=srcGridHtml(items);
  const tc=document.getElementById('srcCats');
  if(tc) tc.outerHTML=srcCatsHtml(sourceList());
  const seg=document.getElementById('srcSeg');
  if(seg) seg.innerHTML=srcSegHtml();
  const cnt=document.getElementById('srcCnt');
  if(cnt) cnt.textContent='显示 '+items.length+' 个';
}

function viewSources(){
  if(SRC_VIEW==='detail'&&S.data.srcDetail){
    return srcDetail();
  }
  const items=sourceList();
  const poolN=(S.data.accountStats&&S.data.accountStats.total)||0;
  const rows=(S.data.models||{}).models_enriched||[];
  const loggedN=items.filter(i=>i.logged).length;
  const shown=srcFilter(items);

  let h='';
  // ---- 一行状态条（真实数据，不再有硬编码）
  h+='<div class="srcbar">'
    +'<div class="srcbar-i"><i>已接入</i><b>'+loggedN
      +'<span class="faint"> / '+items.length+'</span></b></div>'
    +'<div class="srcbar-i"><i>账号凭据</i><b>'+poolN+'</b></div>'
    +'<div class="srcbar-i"><i>收录模型</i><b>'+rows.length+'</b></div>'
    +'<button class="btn sm gh" style="margin-left:auto" '
      +'onclick="SRC_HELP=!SRC_HELP;render()" '
      +'title="展开/收起接入用法说明">怎么用 '+(SRC_HELP?'▴':'▾')+'</button>'
    +'</div>';
  if(SRC_HELP){
    h+='<div class="note">'
      +'<b>就三步</b>：① 下面点一个源，扫码 / 粘 Cookie / 填 Key 完成接入 '
      +'② 客户端 base_url 填 <code>'+esc(panelBaseUrl())+'</code>、'
      +'api_key 填 <code>admin</code> '
      +'③ model 填 <code>auto</code>，面板按实测延迟自动挑最快的源。'
      +'</div>';
  }

  // ---- 搜索 + 筛选
  h+='<div class="srcbar2">'
    +'<input class="q" placeholder="搜索名称 / 端点…" value="'+esc(SRC_Q)+'" '
      +'oninput="SRC_Q=this.value;srcRenderGrid()" '
      +'onkeydown="if(event.key===\'Escape\'){SRC_Q=\'\';this.value=\'\';srcRenderGrid()}">'
    +'<div class="srcseg" id="srcSeg">'+srcSegHtml()+'</div>'
    +'<span class="faint" style="font-size:11.5px" id="srcCnt">显示 '
      +shown.length+' 个</span>'
    +'</div>';

  // ★ 2026-10-12（用户拍板）：分类改**左侧竖排**（全部固定 + 收起露三行 +
  //   展开滚动），与右侧搜索/网格组成两栏布局。
  h += '<div class="src-body">'
    + srcCatsHtml(items)
    + '<div class="src-main">'
    + '<div id="srcGrid">' + srcGridHtml(shown) + '</div>'
    + '</div></div>';
  // ★ 详情 = 右侧抽屉（new-api 范式）：登录+账号+模型全在里面。
  //   动画只在抽屉**首次打开**时播；轮询重绘不重播（否则每 2 秒闪一下）。
  if(SRC_EXPAND){
    const ds = items.find(x=>x.key===SRC_EXPAND)
      || sourceList().find(x=>x.key===SRC_EXPAND);
    if(ds){
      const animate = !SRC_POP_ANIM;
      SRC_POP_ANIM = true;
      h += srcDrawer(ds, animate);
    }
  }
  return h;
}

// （2026-10-07：分类折叠分组已换成 srcCatsHtml 的 tab 条，
//   SRC_OPEN_CATS / toggleSrcCat 一并删除，避免留下会失效的死状态）

function setSrcTab(k){ SRC_TAB=k; render(); }

function openSrc(key){
  // ★ 2026-10-12：平台卡的「详情」不再跳页（一律内联展开，见 expandSrc）。
  //   openSrc 只保留给「新建自定义 API」这种确实需要独立表单的入口。
  S.data.srcDetail=key;
  SRC_VIEW='detail';
  render();
}
function expandSrc(key){
  const opening = SRC_EXPAND!==key;
  SRC_EXPAND = (SRC_EXPAND===key ? null : key);
  // 展开时顺手把同一张卡的账号/模型区也打开，减少二次点击
  if(SRC_EXPAND){
    SRC_ACCT_OPEN[key]=true;
  }
  // 抽屉每次「从关到开」重播一次入场动画；关闭时复位标记
  SRC_POP_ANIM = false;
  render();
  // ★ 2026-10-12（用户拍板「点接入才跳转」）：**不再**在抽屉打开时自动发起登录。
  //   打开详情只是看，点「接入 / 接入并打开登录页」才真正跳转（srcLogin）。
  //   —— 这也是最初「打开详情就弹登录窗口」困惑的根因，已移除。
}

/* 返回源列表：顺手把「正在等扫码、但还没登录成功」的会话停掉。
 *
 * ★ 2026-10-06 用户点破的简化思路：
 *   「设个超时机制不就好了吗？或者检测到前端点击返回源列表就停掉浏览器啊」
 *   —— 对，所以现在服务端只用一个固定端口 + owner 归属校验（不再散列端口），
 *   浏览器实例的**生命周期由前端这一下点击来收口**：
 *     离开 = 用户不扫了 → 停浏览器、放端口，别人立刻能用。
 *   仍在 waits 的会话不会一直挂着占资源（服务端另有超时兜底）。
 */
function closeSrc(){
  leaveLoginView();
  S.data.srcDetail=null; SRC_VIEW='card'; render();
}

/** 离开任何「带登录会话」的界面（返回源列表 / 切走视图）时统一调用。
 *  justPassive：不动会话，只清理前端轮询（用于已成功的会话）。 */
function leaveLoginView(){
  stopLoginPoll();
  const s=S.data.loginSession;
  if(s&&(s.status==='pending'||s.status==='waiting')){
    // 不等后端返回，先把本地状态收掉，避免下次进来又被这张旧卡挡住
    S.data.loginSession=null;
    api('api/login?action=cancel',{id:s.id}).catch(()=>null);
  }
}

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
  const r=await api('api/tencent?action=status').catch(()=>({ok:false}));
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
  const r=await api('api/tencent?action=login',{}).catch(e=>({ok:false,message:e.message}));
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
    const r=await api('api/tencent?action=poll',{state:TCB.state,timeout:2})
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
  const r=await api('api/tencent?action=quota').catch(()=>({ok:false}));
  if(r&&r.ok===false){ if(!silent) toast(r.message||'取额度失败','err'); return; }
  S.data.tcQuota=(r&&r.result)||r||null;
  // silent 只表示「失败别弹 toast」，成功照样要 render 才看得到额度
  render();
}

async function tencentCheckin(){
  const r=await api('api/tencent?action=checkin').catch(e=>({ok:false,message:e.message}));
  if(r&&r.ok===false){toast(r.message||'签到失败','err');return;}
  const d=(r&&r.result)||r||{};
  if(d.already) toast('今天已经签过了');
  else if(d.checkedIn){ toast('签到成功！'); loadTcQuota(); }
  else toast(d.msg||'签到失败','err');
}

async function tencentModels(){
  const r=await api('api/tencent?action=catalog',{}).catch(e=>({ok:false,message:e.message}));
  if(r&&r.ok===false){toast(r.message||'拉模型失败','err');return;}
  const d=(r&&r.result)||r||{};
  S.data.tcModels=d.models||[];
  render();
  toast('在线模型 '+S.data.tcModels.length+' 个（含倍率）');
}

/** 子页面 sticky 返回栏：滚到哪都吸顶可见，手机端点击区也大（样式见 .backbar） */
function backbar(name){
  const s=S.data.loginSession;
  // 有「等扫码中」的会话时，返回按钮旁多给一个明确的「结束登录」出口 ——
  // 否则用户只能点「取消」，且不知道浏览器还在服务器上开着。
  const kill=(s&&(s.status==='pending'||s.status==='waiting'))
    ?'<button class="btn dgr" onclick="cancelLogin()" title="停掉服务端浏览器，释放资源">结束登录</button>'
    :'';
  return '<div class="backbar">'
    +'<button class="btn pri" onclick="closeSrc()">← 返回源列表</button>'
    +'<b>'+esc(name||'')+'</b>'+kill+'</div>';
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
  
  // 自定义 API：新建表单
  if(s.custom_new){
    return backbar('新建自定义 API')
      +'<div class="card"><div class="ch"><b>新建自定义 API</b>'
      +'<button class="btn sm" onclick="closeSrc()">← 返回源列表</button></div><div class="cb">'
      +'<div class="form-group">'
      +'<label>名称</label>'
      +'<input id="ca_name" type="text" placeholder="例如：我的 DeepSeek" style="width:100%">'
      +'</div>'
      +'<div class="form-group">'
      +'<label>协议类型</label>'
      +'<select id="ca_protocol" style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px">'
      +'<option value="openai">OpenAI 兼容（/v1/chat/completions）</option>'
      +'<option value="anthropic">Anthropic（/v1/messages）</option>'
      +'</select>'
      +'</div>'
      +'<div class="form-group">'
      +'<label>Base URL</label>'
      +'<input id="ca_baseurl" type="text" placeholder="例如：https://api.deepseek.com/v1" style="width:100%">'
      +'</div>'
      +'<div class="form-group">'
      +'<label>API Key</label>'
      +'<input id="ca_apikey" type="password" placeholder="sk-..." style="width:100%">'
      +'</div>'
      +'<div class="btnrow" style="margin-top:15px">'
      +'<button class="btn pri" onclick="saveCustomApi()">保存并同步模型</button>'
      +'<button class="btn" onclick="closeSrc()">取消</button>'
      +'</div>'
      +'<div id="ca_result" class="mt"></div>'
      +'</div></div>';
  }
  
  // 自定义 API：详情页
  if(s.custom){
    const ca = (S.data.customApis||[]).find(x=>x.id===s.key);
    let h = backbar(s.name)
      +'<div class="card"><div class="ch"><b>'+esc(s.name)+'</b>'
      +'<button class="btn sm" onclick="closeSrc()">← 返回源列表</button></div><div class="cb">';
    
    h += '<div class="flex mb">'
      + '<span class="tag '+(s.logged?'ok':'')+'">'+(s.logged?'已配置':'未配置')+'</span>'
      + '<span class="tag info">自定义 API</span>'
      + '</div>';
    
    h += '<div class="muted" style="font-size:12.5px;margin-bottom:10px">'
      + 'Base URL: <code>'+esc(s.endpoint)+'</code></div>';
    
    if(ca){
      h += '<div class="note">已同步 <b>'+ca.model_count+'</b> 个模型'
        + (ca.last_sync?' · 上次同步：'+esc(ca.last_sync):'') + '</div>';
    }
    
    h += '<div class="btnrow" style="margin-top:15px">'
      + '<button class="btn pri" onclick="syncCustomApi(\''+esc(s.key)+'\')">同步模型</button>'
      + '<button class="btn" onclick="editCustomApi(\''+esc(s.key)+'\')">编辑配置</button>'
      + '<button class="btn dgr" onclick="delCustomApi(\''+esc(s.key)+'\')">删除</button>'
      + '</div>';
    
    h += '<div id="ca_result" class="mt"></div>';
    h += '</div></div>';
    return h;
  }
  
  if(s.view==='localproxy'){
    return backbar(s.name)+'<div class="card"><div class="ch"><b>'+esc(s.name)+'</b>'
      +'<button class="btn sm" onclick="closeSrc()">← 返回源列表</button></div>'
      +'<div class="cb">'+viewLocalproxy()+'</div></div>';
  }
  const acc=(S.data.accounts||[]).find(a=>a.platform===s.key)
    ||(s.variants?(S.data.accounts||[]).find(a=>s.variants.some(v=>v.pid===a.platform)):null);
  let h=backbar(s.name)+'<div class="card"><div class="ch"><b>'+esc(s.name)+'</b>'
    +'<button class="btn sm" onclick="closeSrc()">← 返回源列表</button></div><div class="cb">';
  h+='<div class="flex mb">'
    +'<span class="tag '+(s.logged?'ok':'')+'">'+(s.logged?'已接入':'未接入')+'</span>'
    +'<span class="tag info">'+esc(s.kind)+'</span>'
    +(s.checkin?'<span class="tag acc">支持每日签到'+(s.at?' '+esc(s.at):'')+'</span>':'')
    +'</div>';
  h+='<div class="muted" style="font-size:12.5px;margin-bottom:10px">'+esc(s.desc||'')+'</div>';

  h+=srcLoginPanel(s);
  h+='</div></div>';
  return h;
}

function srcLogin(pid){
  const p=(S.data.loginPlatforms||[]).find(x=>x.id===pid);
  // ★ 2026-10-13（用户「登录不行」根因之一）：file 类但声明了 browser_capture
  //   的平台（web-trae / web-deepseek / apk-trae）必须带 force_browser=1，
  //   否则后端 _run 分派走 _start_file，只回一句「请粘贴凭据」——
  //   「↗ 接入并打开登录页」点了什么都不发生，浏览器根本不打开。
  //   cookie 类平台无需此参（_start_cookie 自己就会开浏览器）。
  const bc = !!(p && p.browser_capture);
  startLogin(pid, (p&&p.edition)||'', 0, bc?1:0);
}

/** ★ 2026-10-13：一键读取**本机** Trae 桌面端登录态（trae_local）。
 *  后端 api/tencent?action=trae_local 读 Trae userData 里的 x-ide-token
 *  并按传入 platform 落库（apk-trae / web-trae 中继都吃这个 token）。 */
async function traeReadLocal(pid){
  toast('正在读取本机 Trae 登录态…');
  let r;
  try{
    r=await post('api/tencent?action=trae_local',{platform:pid||'apk-trae'});
  }catch(e){ toast('读取失败：'+e.message,'err'); return; }
  if(r.ok===false){ toast(r.message||'读取失败','err'); return; }
  if(r.found===false){
    let m=r.error||'本机未找到 Trae 登录态';
    if(r.candidates&&r.candidates.length)m+='（候选目录：'+r.candidates[0]+'）';
    toast(m,'warn');
    return;
  }
  toast('已读取本机 Trae 登录态并写入账号：'+(r.account||''),'ok');
  await loadSources();
}

/** 抽出「登录路径」UI，供卡片内联展开共用。
 * 包含：variant 选择 / 已接入动作 / 未接入登录按钮 / 当前登录会话（二维码/链接）。
 * compact=true：内联卡片模式 —— 不渲染探测端点/平台内部能力等旧详情页的大块内容，
 * 登录会话也用轻量容器（2026-10-12 用户反馈展开后排版乱）。 */
function srcLoginPanel(s, compact){
  const acc=(S.data.accounts||[]).find(a=>a.platform===s.key)
    ||(s.variants?(S.data.accounts||[]).find(a=>s.variants.some(v=>v.pid===a.platform)):null);
  let h='';
  if(s.variants){
    const btnText = s.method==='qrcode' ? '跳转登录'
      : s.method==='oauth_device' ? '设备码登录'
      : s.method==='oauth_pkce' ? '授权登录'
      : '接入';
    h+='<div style="margin:4px 0">'
      +'<div class="faint" style="font-size:11.5px;margin-bottom:4px">'
      +'国内/国际是两个独立账号，额度互不互通；需要哪个就登哪个。</div>'
      +s.variants.map(v=>
        '<div class="flex" style="justify-content:space-between;align-items:center;'
        +'padding:7px 0;border-top:1px solid var(--line2)">'
        +'<span style="font-size:13px">'+esc(v.label)+'</span>'
        +(v.logged
          ?'<span class="tag ok">已登录</span>'
          :'<button class="btn pri sm" onclick="event.stopPropagation();srcLogin(\''+esc(v.pid)+'\')">'
            +btnText+'</button>')
        +'</div>').join('')
      +'<div class="faint" style="font-size:11.5px;margin-top:6px">'
        +'国内/国际是两个独立账号，额度互不互通；需要哪个就登哪个。</div>'
      +'</div>';
    if(S.data.loginSession)h+=viewLoginSession(s.key, compact);
    // ★ 2026-10-12（跳转制）：手动粘贴降级为 <details> 低调兜底，不在主区摆表单
    const vp = s.variants.map(v=>'<option value="'+esc(v.pid)+'">'+esc(v.label)+'</option>').join('');
    h+='<details style="margin-top:8px">'
      +'<summary class="faint" style="font-size:11.5px;cursor:pointer">登录页打不开？手动粘贴 accessToken</summary>'
      +'<div class="row n" style="gap:8px;margin:6px 0">'
      +'<select id="pasteVariant_'+esc(s.key)+'" style="padding:6px 8px;border:1px solid var(--line2);'
      +'border-radius:8px;font-size:12px">'+vp+'</select>'
      +'<span class="faint" style="font-size:11.5px;align-self:center">选对应站点</span></div>'
      +'<textarea id="directPaste_'+esc(s.key)+'" rows="3" placeholder="粘贴 accessToken（JWT，eyJ… 开头）" '
      +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;'
      +'font-family:monospace;font-size:12px"></textarea>'
      +'<button class="btn pri sm" style="margin-top:6px" '
      +'onclick="submitDirectPasteVariant(\''+esc(s.key)+'\',\'token\')">保存凭据</button>'
      +'</details>';
  }else if(s.logged){
    // ★ 2026-10-12：展开面板里的「已接入」精简版 ——
    //   大模型表（dataTable）从面板里移除（用户反馈冗余），模型用卡片上的
    //   「查看清单」紧凑列表；这里只留状态 + 核心动作。
    h+='<div class="note ok" style="color:var(--ok);border-color:var(--ok-line);background:var(--ok-soft)">'
      +'已接入 · 凭据 <code>'+esc(acc?String(acc.secret||''):'')+'</code>'
      +'（'+esc(acc&&acc.source||'')+'，'+esc(acc&&acc.obtained_at||'')+'）</div>';
    h+='<div class="btnrow">'
      +(s.checkin?'<button class="btn pri" onclick="acRunOne(\''+esc(s.key)+'\')">立即签到</button>':'')
      +'<button class="btn" onclick="platformProbe(\''+esc(s.key)+'\')">探测端点</button>'
      +'<button class="btn dgr" onclick="delAccountFor(\''+esc(s.key)+'\')">解除接入</button>'
      +'</div>';
  }else{
    // ★ Loomy：签名 API 登录（短信/密码/session 三方式，无需浏览器）
    if(s.method==='loomy'){
      h+='<div class="faint" style="font-size:11.5px;margin:2px 0 4px">方式一 · 短信验证码登录（免浏览器，无设备重绑问题）：</div>'
        +'<div class="row n" style="gap:6px">'
        +'<input id="loomyPhone_'+esc(s.key)+'" placeholder="手机号" style="flex:1 1 130px" oninput="event.stopPropagation()">'
        +'<button class="btn sm pri" onclick="event.stopPropagation();loomySendSms(\''+esc(s.key)+'\')">发送验证码</button>'
        +'</div>'
        +'<div class="row n" style="gap:6px;margin-top:6px">'
        +'<input id="loomyCode_'+esc(s.key)+'" placeholder="短信验证码" style="flex:1 1 130px" oninput="event.stopPropagation()">'
        +'<input id="loomyMsgid_'+esc(s.key)+'" type="hidden">'
        +'<button class="btn sm pri" onclick="event.stopPropagation();loomySmsVerify(\''+esc(s.key)+'\')">登录</button>'
        +'</div>'
        +'<div class="sc-sect"><i>方式二 · 密码登录</i>'
        +'<span class="faint" style="font-size:11px">需在讯飞账号中心设置过密码</span></div>'
        +'<div class="row n" style="gap:6px">'
        +'<input id="loomyPwdPhone_'+esc(s.key)+'" placeholder="手机号" style="flex:1 1 130px" oninput="event.stopPropagation()">'
        +'<input id="loomyPwd_'+esc(s.key)+'" type="password" placeholder="密码" style="flex:1 1 130px" oninput="event.stopPropagation()">'
        +'<button class="btn sm pri" onclick="event.stopPropagation();loomyPwdLogin(\''+esc(s.key)+'\')">登录</button>'
        +'</div>';
      const sess=S.data.loginSession;
      if(sess && sess.platform===s.key) h+=viewLoginSession(s.key, compact);
      h+='<div class="sc-sect"><i>方式三 · 粘贴 session</i>'
        +'<span class="faint" style="font-size:11px">32 字符，登录后服务端下发，14 天有效</span></div>'
        +'<textarea id="directPaste_'+esc(s.key)+'" rows="3" placeholder="粘贴 session…" '
        +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;'
        +'font-family:monospace;font-size:12px"></textarea>'
        +'<button class="btn pri sm" style="margin-top:6px" '
        +'onclick="submitDirectPaste(\''+esc(s.key)+'\',\'token\')">保存凭据</button>';
      return h;
    }
    // ★ 2026-10-12（用户拍板）：方式一收敛为「一键登录」—— 按优先级自动执行：
    //   扫码（页面有码就抓码）→ 跳转（打开登录页，用户在里面登录）→
    //   Cookie（登录后自动抓取入库）。远程无头/粘贴保留为备用。
    const pasteKind = (s.method==='cookie') ? 'Cookie'
      : (s.method==='file') ? (s.paste_type==='token' ? 'Token' : 'API Key / Token')
      : 'Token';
    const pasteType = s.paste_type || (s.method==='cookie' ? 'cookie'
      : s.method==='file' ? 'api_key' : 'token');
    // ★ 2026-10-12（用户拍板「取消所有扫码连接，全部改为跳转」）：
    //   点「接入」即跳转官方登录页（抽屉打开时自动发起），面板不渲染二维码、
    //   不摆方式一/二表单；手动粘贴降级为 <details> 里的低调兜底。
    h+=viewLoginSession(s.key, compact);
    const _sess=S.data.loginSession;
    // ★ 2026-10-13（用户截图实锤：Lobster 登录中时，智谱/小浣熊抽屉的登录按钮
    //   整个消失，抽屉里只剩说明文字）：原先 _sessActive 只看**全局**有没有
    //   活跃会话 —— 别的平台在登录，本平台的「接入并打开登录页」就被吞了。
    //   现在必须本平台（合并卡含 variants pid / copilot 家族）的会话才算活跃。
    const _sessMine=_sess&&(_sess.platform===s.key
      ||(s.variants&&s.variants.some(v=>v.pid===_sess.platform))
      ||(s.key==='copilot'&&COPILOT_FAMILY.indexOf(_sess.platform)>=0));
    const _sessActive=!!(_sessMine&&(_sess.status==='pending'||_sess.status==='waiting'));
    if(!_sessActive){
      h+='<div class="note" style="margin:8px 0 0">点「接入」即打开官方登录页——'
        +'在浏览器里完成登录，凭据会自动入库（无需在面板里扫码）。</div>';
      // ★ 2026-10-12 修复：裸 <a> 链接不会启动后端会话/轮询 → 凭据永不回收。
      //   主按钮改为 srcLogin(key)（与 variants 分支一致），后端开静默令牌通道
      //   + 设会话 + 起 _poll_native；「我自己打开」作为在自有浏览器里登录的辅助入口。
      h+='<div class="btnrow" style="margin-top:8px">'
        +'<button class="btn pri" onclick="event.stopPropagation();srcLogin(\''+esc(s.key)+'\')">↗ 接入并打开登录页</button>';
      if(s.login_url){
        h+='<a class="btn" href="'+esc(s.login_url)+'" target="_blank" rel="noreferrer" '
          +'title="在「我自己的浏览器」里打开登录页">我自己打开</a>';
      }
      // ★ 2026-10-13：Trae 系（apk-trae / web-trae）加「读取本机 Trae 登录态」
      //   一键按钮 —— 桌面端 Trae 已登录时这是最快路径，无需开浏览器
      //   （trae_local 读 Trae userData 的 x-ide-token，后端按 platform 落库）。
      if(s.key==='apk-trae'||s.key==='web-trae'){
        h+='<button class="btn" onclick="event.stopPropagation();traeReadLocal(\''+esc(s.key)+'\')" '
          +'title="读取本机已登录 Trae 桌面端的 x-ide-token，一键入库'
          +'（需面板与 Trae 在同一台电脑运行）">⌂ 读取本机 Trae 登录态</button>';
      }
      h+='<button class="btn" onclick="event.stopPropagation();platformProbe(\''+esc(s.key)+'\')">先探测端点</button>'
        +'</div>';
      h+='<details style="margin-top:8px">'
        +'<summary class="faint" style="font-size:11.5px;cursor:pointer">'
        +'登录页打不开？手动粘贴'+esc(pasteKind)+'</summary>'
        +'<textarea id="directPaste_'+esc(s.key)+'" rows="3" style="margin-top:6px" '
        +'placeholder="'+esc(s.placeholder || ('粘贴 '+pasteKind+'…'))+'" '
        +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;'
        +'font-family:monospace;font-size:12px"></textarea>'
        +'<button class="btn pri sm" style="margin-top:6px" '
        +'onclick="submitDirectPaste(\''+esc(s.key)+'\',\''+esc(pasteType)+'\')">保存凭据</button>'
        +'</details>';
    }
  }
  return h;
}

/** 方式二：不发起登录会话，直接把粘贴的凭据写进账号池。
 *  后端 api/login?action=submit 无 id 时走「手动添加」分支。 */
async function submitDirectPaste(pid, type){
  const box=document.getElementById('directPaste_'+pid);
  const v=(box?box.value:'').trim();
  if(!v){ toast('请先粘贴凭据','warn'); return; }
  const r=await post('api/login?action=submit',
    {platform:pid, name:pid+'（手动粘贴）', type:type||'cookie', secret:v});
  if(r.ok){
    toast('已保存到账号池','ok');
    box.value='';
    await loadSources();
  }else{
    toast(r.message||r.error||'保存失败','err');
  }
}

/** Loomy 手机号归一化（与后端 app/loomy_client.py norm_phone 同规则）：
 *  剥 +86/86 前缀、空格、横线、括号；^1[3-9]\d{9}$ 校验。
 *  不合法就原地 toast，别发请求 —— 账号服务对非法号段只回笼统的
 *  「请求参数错误」（010003），用户看不出是手机号写错（2026-10-13 实拍）。 */
function normLoomyPhone(raw){
  let s=(raw||'').replace(/[\s\-()（）]+/g,'');
  if(s.indexOf('+86')===0)s=s.slice(3);
  else if(s.indexOf('0086')===0)s=s.slice(4);
  else if(s.indexOf('86')===0&&s.length===13)s=s.slice(2);
  return s;
}
function loomyPhoneOk(s){ return /^1[3-9]\d{9}$/.test(s); }

/** Loomy 短信登录三步（签名 API，免浏览器） */
async function loomySendSms(key){
  const phone=normLoomyPhone(document.getElementById('loomyPhone_'+key).value);
  document.getElementById('loomyPhone_'+key).value=phone;
  if(!phone){ toast('请先输入手机号','warn'); return; }
  if(!loomyPhoneOk(phone)){
    toast('手机号格式不正确：请填 11 位大陆手机号（可带 +86 前缀）','err'); return;
  }
  toast('正在发送验证码…');
  const r=await post('api/login?action=loomy_sms_send',{phone});
  if(r.ok && r.msgid){
    document.getElementById('loomyMsgid_'+key).value=r.msgid;
    toast('验证码已发送，请查收短信','ok');
  }else{
    toast(r.message||r.error||'发送失败','err');
  }
}
async function loomySmsVerify(key){
  const phone=normLoomyPhone(document.getElementById('loomyPhone_'+key).value);
  const code=document.getElementById('loomyCode_'+key).value.trim();
  const msgid=document.getElementById('loomyMsgid_'+key).value.trim();
  if(!phone||!code||!msgid){ toast('请先填手机号并获取验证码','warn'); return; }
  if(!loomyPhoneOk(phone)){ toast('手机号格式不正确','err'); return; }
  toast('正在登录…');
  const r=await post('api/login?action=loomy_sms_verify',{phone,code,msgid});
  if(r.ok){ toast('Loomy 登录成功','ok'); await loadSources(); }
  else toast(r.message||r.error||'登录失败','err');
}
async function loomyPwdLogin(key){
  const phone=normLoomyPhone(document.getElementById('loomyPwdPhone_'+key).value);
  document.getElementById('loomyPwdPhone_'+key).value=phone;
  const pwd=document.getElementById('loomyPwd_'+key).value.trim();
  if(!phone||!pwd){ toast('请填手机号和密码','warn'); return; }
  if(!loomyPhoneOk(phone)){
    toast('手机号格式不正确：请填 11 位大陆手机号（可带 +86 前缀）','err'); return;
  }
  toast('正在登录…');
  const r=await post('api/login?action=loomy_pwd',{phone,password:pwd});
  if(r.ok){ toast('Loomy 登录成功','ok'); await loadSources(); }
  else toast(r.message||r.error||'登录失败','err');
}

/** 合并卡（copilot / kimi）变体选择版：平台 id 取下拉框当前值 */
async function submitDirectPasteVariant(key, type){
  const sel=document.getElementById('pasteVariant_'+key);
  const pid=(sel&&sel.value)||key;
  const box=document.getElementById('directPaste_'+key);
  const v=(box?box.value:'').trim();
  if(!v){ toast('请先粘贴凭据','warn'); return; }
  const r=await post('api/login?action=submit',
    {platform:pid, name:pid+'（手动粘贴）', type:type||'token', secret:v});
  if(r.ok){
    toast('已保存到 '+pid,'ok');
    box.value='';
    await loadSources();
  }else{
    toast(r.message||r.error||'保存失败','err');
  }
}

async function loadSrcModels(pid){
  const box=$('#srcModels');
  if(box)box.innerHTML='<div class="sk line" style="height:34px"></div>';
  const r=await api('api/models?action=platform_models',{platform:pid})
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
  const r=await api('api/settings');
  S.data.settings=r;
  const c=await api('api/credentials');
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
        <button class="btn pri" onclick="post('api/settings?action=gateway_restart',{})">启动网关</button>
        <button class="btn dgr" onclick="post('api/settings?action=gateway_stop',{})">停止网关</button>
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
  const r=await post('api/credentials?action=delete',{name});
  toast(r.ok?'已删除':'失败：'+r.message,r.ok?'ok':'err');
  loadGateway();
}
async function gwSetup(){
  const u=$('#gwUser').value.trim(),p=$('#gwPass').value;
  if(!u||!p)return toast('用户名和密码必填','err');
  const r=await post('api/settings?action=gateway_setup',{username:u,password:p});
  toast(r.message||(r.ok?'成功':'失败'),r.ok?'ok':'err');
  loadGateway();
}
async function gwLogin(){
  const u=$('#gwUser').value.trim(),p=$('#gwPass').value;
  const r=await post('api/settings?action=gateway_login',{username:u,password:p});
  toast(r.message||(r.ok?'成功':'失败'),r.ok?'ok':'err');
  loadGateway();
}

// ---------------------------------------------------------------- 签到
async function loadCheckin(){
  const r=await api('api/checkins');
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
    <h2>今日记录 <span class="faint" style="font-size:11.5px;font-weight:normal">（仅显示支持签到的平台）</span></h2>
    <div class="scroll sm">
      <table><thead><tr><th>时间</th><th>站点</th><th>结果</th><th>连续</th><th>说明</th></tr></thead><tbody>
      ${today.length?today.filter(x=>{
          // 只显示真正支持签到的站点（过滤掉不支持签到但被瞎跑的）
          const site=sites.find(s=>s.id===x.site||s.name===x.name||s.name===x.site);
          return site&&site.checkin;
        }).map(x=>`<tr>
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
  await api('api/checkins?trigger=1'+(force?'&force=1':''));
  setTimeout(loadCheckin,3000);
}
async function runOne(id){
  const r=await post('api/sites?action=run_one',{id,force:true});
  const x=r.result||{};
  toast((x.name||'')+'：'+(x.message||''),x.ok?'ok':'err');
  loadCheckin();
}
function quickCheckin(){
  go('checkin');
  checkin(false);
}

// ---------------------------------------------------------------- 成长任务
// ★ 2026-10-13：任务中心并入原「任务管理」的手动任务，一并加载
async function loadGrowth(){
  const r=await api('api/growth');
  S.data.growth=r;
  const t=await api('api/tasks').catch(()=>({tasks:[]}));
  S.data.tasks=t;
  render();
}
function viewGrowth(){
  // ★ 2026-10-07 重构（任务模块）：数据来自后端**原生**负载 —— api_growth 已
  //   去掉网关依赖，不再出现"网关不在 → 整页空白"。下面那张硬编码清单降级为
  //   「中文名 + 常用动作」的兜底元数据（后端 autocheckin/gwextra 拿不到时才用）。
  const g=(S.data.growth&&S.data.growth.growth)||{};
  const native=(g.platforms||[]);
  // ⚠ LEGACY_PLATFORMS 在**本函数下方**才声明（const 有 TDZ），所以这里只定义
  //   合并逻辑，等声明之后再调用 —— 否则会 "Cannot access before initialization"。
  let platforms=[];
  const buildPlatforms=()=>{
    const legacy={};
    for(const p of LEGACY_PLATFORMS) legacy[p.id]=p;
    return native.length
      ? native.map(p=>Object.assign({},p,{
          id:p.platform,
          name:p.name||(legacy[p.platform]||{}).name||p.platform,
          features:(p.features&&p.features.length)
            ? p.features : ((legacy[p.platform]||{}).features||[])}))
      : LEGACY_PLATFORMS.map(p=>Object.assign({},p,{
          platform:p.id,on:true,done_today:false,last:{},fail_streak:0}));
  };
  const nsum=g.summary||{};          // 后端任务汇总（平台/站点/心跳）
  const ck=g.checkin_summary||{};    // store.checkin_summary()
  const siteTasks=g.sites||[];       // 外部站点（store.sites 里 checkin=true 的）
  const enabled=g.enabled;
  const sites=S.data.sites||[];
  // ★ 2026-10-13：并入的手动任务（原「任务管理」页内容，随 loadGrowth 加载）
  const tsk=S.data.tasks||{};
  const taskList=tsk.tasks||[];

  // 兜底元数据：平台中文名 + 常用动作（动作 id 已与 gwextra.ACTIONS 核对过）
  const LEGACY_PLATFORMS=[
    {
      id:'wb-gateway', name:'WorkBuddy 国内',
      features:[
        {name:'成长任务列表', action:'wb-gateway:growth_tasks'},
        {name:'接受任务', action:'wb-gateway:growth_task_accept'},
        {name:'领取任务奖励', action:'wb-gateway:growth_task_claim'},
        {name:'猫猫旅行-查询', action:'wb-gateway:buddy_info'},
        {name:'猫猫旅行-派出', action:'wb-gateway:travel_depart'},
        {name:'连登状态', action:'wb-gateway:streak_status'},
        {name:'抽奖次数', action:'wb-gateway:lottery_summary'},
      ]
    },
    {
      id:'wb-gateway-intl', name:'WorkBuddy 国际',
      features:[
        {name:'成长任务列表', action:'wb-gateway-intl:growth_tasks'},
        {name:'接受任务', action:'wb-gateway-intl:growth_task_accept'},
        {name:'领取任务奖励', action:'wb-gateway-intl:growth_task_claim'},
        {name:'连登状态', action:'wb-gateway-intl:streak_status'},
      ]
    },
    {
      id:'web-glm', name:'智谱清言',
      features:[
        {name:'每日登录领积分', action:'web-glm:daily_login_score'},
        {name:'积分余额查询', action:'web-glm:score_balance'},
        {name:'积分活动领奖', action:'web-glm:score_activity_draw'},
        {name:'积分流水', action:'web-glm:score_record'},
      ]
    },
    {
      id:'apk-trae', name:'Trae',
      features:[
        {name:'签到状态', action:'apk-trae:checkin_status'},
        {name:'领取签到积分', action:'apk-trae:checkin_claim'},
      ]
    },
    {
      id:'apk-kuku', name:'百度文库',
      features:[
        {name:'免费积分任务状态', action:'apk-kuku:freepoint_taskstatus'},
        {name:'每日签到', action:'apk-kuku:checkin_claim'},
      ]
    },
    {
      id:'apk-wps', name:'WPS',
      features:[
        {name:'每日签到', action:'apk-wps:checkin_claim'},
      ]
    },
    {
      id:'apk-raccoon', name:'小浣熊',
      features:[
        {name:'积分余额', action:'apk-raccoon:points_balance'},
      ]
    },
    {
      id:'web-lobster', name:'Lobster AI',
      features:[
        {name:'积分查询', action:'web-lobster:profile'},
      ]
    },
  ];

  platforms=buildPlatforms();         // ← LEGACY_PLATFORMS 已声明，可以安全调用
  const sum=ck;                       // 兼容旧模板里的 sum.done_today 等
  const today=[];                     // 今日明细改由「平台卡 + 站点卡」承载
  const pct=sum.total_checkin_sites
    ? Math.round((sum.done_today||0)/sum.total_checkin_sites*100) : 0;
  const lastRun=Object.values(nsum).length?nsum.last_tick:null;
  const actCount=platforms.reduce((a,p)=>a+((p.features||[]).length),0);

  return `
  <div class="card">
    <h2>任务中心 <span class="faint" style="font-size:11.5px;font-weight:normal">（签到 + 成长任务 + 积分）</span></h2>
    <div class="note">
      所有签到/任务/积分功能统一在这里管理。支持定时自动执行，不用手动点。
    </div>
  </div>

  <div class="grid g4 mb">
    <div class="kpi"><div class="lb">平台任务</div>
      <div class="vl">${nsum.done_today||0} / ${nsum.on||platforms.length}</div>
      <div class="ex">今日已完成 / 已启用（共 ${platforms.length} 个平台）</div></div>
    <div class="kpi"><div class="lb">今日失败</div>
      <div class="vl" style="color:${(nsum.failed_today||0)?'var(--err)':'var(--ok)'}">${nsum.failed_today||0}</div>
      <div class="ex">${lastRun?'上次调度 '+esc(lastRun):'调度器未跑过'}</div></div>
    <div class="kpi"><div class="lb">外部站点</div>
      <div class="vl">${nsum.sites_done||0} / ${nsum.sites||0}</div>
      <div class="ex">今日已签到 / 已启用站点</div></div>
    <div class="kpi"><div class="lb">定时执行</div>
      <div class="vl">${esc(g.hour||'09:10')}</div>
      <div class="ex">${enabled?'已启用':'已关闭'} · 每轮错峰 ${g.stagger_sec||45}s · 重试 ${g.retry_times||0} 次</div></div>
  </div>

  <div class="card">
    <h2>签到进度</h2>
    ${bar(pct,'ok')}
    <div class="flex mt" style="justify-content:space-between">
      <span class="muted">${pct}% 完成 · 失败 ${today.filter(x=>!x.ok).length} 个</span>
      <div class="flex">
        <button class="btn" onclick="checkin(false)">补签未完成</button>
        <button class="btn pri" onclick="checkin(true)">全部重签</button>
      </div>
    </div>
  </div>

  <div class="card">
    <div class="flex" style="justify-content:space-between;align-items:center">
      <h2 style="margin:0">平台任务 <span class="faint" style="font-size:11.5px;font-weight:normal">（${platforms.length} 个平台 · ${actCount} 个可执行动作）</span></h2>
      <div class="flex">
        <button class="btn pri" onclick="runGrowth()">⚡ 一键执行全部</button>
        <button class="btn" onclick="acRunFailed()">只补跑失败的</button>
        <button class="btn" onclick="loadGrowth()">刷新</button>
      </div>
    </div>
    <div class="note" style="margin-top:8px">
      每个平台每天只跑一次（已完成的会被跳过）；失败会在下一轮重试，连续 3 次失败当天放弃。
      点平台名可单独执行该平台 —— 只跑这一个，不会连坐其它平台。
    </div>
  </div>

  <div class="grid g2">
    ${platforms.map(p=>{
      const hasLogin=!!((S.data.accounts||[]).find(a=>
        (a.platform===p.id||a.platform==='api-'+p.id)&&a.secret));
      const last=p.last||{};
      const failed=last.ok===false;
      const done=!!p.done_today;
      const off=p.on===false;
      const state = off?'<span class="tag">已关闭</span>'
        : failed?'<span class="tag err">今日失败</span>'
        : done?'<span class="tag ok">今日完成</span>'
        : !hasLogin?'<span class="tag warn">未接入</span>'
        : '<span class="tag">待执行</span>';
      const acts=(p.features||[]);
      const open=TASK_OPEN[p.id];
      return `
      <div class="card">
        <div class="sc-h">
          <span class="sdot${done&&!failed?' ok':''}"></span>
          <span class="sc-n">${esc(p.name)}</span>
          <span style="margin-left:auto;flex:0 0 auto">${state}</span>
        </div>
        <div class="tk-meta">
          <span>定时 <b>${esc(p.time||'—')}</b></span>
          <span>${hasLogin?'<b class="okc">已接入</b>':'<b class="warnc">未接入</b>'}</span>
          ${p.mode&&p.mode!=='none'?`<span>方式 <b>${esc(p.mode)}</b></span>`:''}
          ${p.fail_streak?`<span class="warnc">连败 ${p.fail_streak}</span>`:''}
        </div>
        <div class="tk-last${failed?' err':''}">
          ${last.at||last.message? (esc((last.at||'')+' ')+(last.message?esc(String(last.message).slice(0,90)):'')) : '还没有执行记录'}
        </div>
        <div class="sc-f">
          <button class="btn sm${done&&!failed?'':' pri'}" onclick="acRunOne('${esc(p.id)}')">
            ${done&&!failed?'重跑':'单独执行'}</button>
          <button class="btn sm gh" onclick="taskToggleActs('${esc(p.id)}')">
            动作明细 ${open?'▴':'▾'} ${acts.length?'('+acts.length+')':''}</button>
          <button class="btn sm gh" onclick="go('sources')">去接入</button>
        </div>
        ${open&&acts.length?`<div class="tk-acts">${acts.map(f=>`
          <button class="btn sm gh" onclick="runAction('${esc(f.action)}')" title="${esc(f.action)}">${esc(f.name)}</button>
        `).join('')}</div>`:''}
      </div>`;
    }).join('')}
  </div>

  ${siteTasks.length?`<div class="card">
    <h2>外部站点任务 <span class="faint" style="font-size:11.5px;font-weight:normal">（${siteTasks.filter(s=>s.checkin).length} 个站点参与定时签到）</span></h2>
    <div class="scroll sm">
      <table><thead><tr><th>站点</th><th>定时签到</th><th>今日结果</th><th class="right">操作</th></tr></thead><tbody>
      ${siteTasks.map(s=>{
        const l=s.last||{};
        return `<tr>
          <td>${esc(s.name||s.url||s.id)}</td>
          <td>${s.checkin?'<span class="tag ok">已启用</span>':'<span class="tag">未启用</span>'}</td>
          <td class="muted">${l.at?esc((l.ok?'✓ ':'✗ ')+String(l.message||'').slice(0,70)):'—'}</td>
          <td class="right nowrap"><button class="btn sm" onclick="runOne('${esc(s.id)}')">立即签到</button></td>
        </tr>`;
      }).join('')}
      </tbody></table>
    </div>
  </div>`:''}

  <div class="card">
    <div class="flex" style="justify-content:space-between;align-items:center">
      <h2 style="margin:0">手动任务 <span class="faint" style="font-size:11.5px;font-weight:normal">（勾选=已完成，「执行未完成」会跳过；关联站点的任务会真的调签到接口）</span></h2>
      <div class="flex">
        <button class="btn pri" onclick="runAllTasks()">⚡ 执行未完成</button>
        <button class="btn" onclick="loadGrowth()">刷新</button>
      </div>
    </div>
    <div class="scroll sm" style="margin-top:8px">
      <table><thead><tr><th style="width:44px">完成</th><th>任务</th><th>关联站点</th><th style="width:64px">次数</th><th>上次结果</th><th class="right">操作</th></tr></thead><tbody>
      ${taskList.length?taskList.map(x=>{
        const st=sites.find(s=>s.id===x.site);
        return `<tr>
        <td><input type="checkbox" ${x.done?'checked':''} style="width:auto" title="勾选后「执行未完成」会跳过它" onchange="tgTask('${esc(x.id)}')"></td>
        <td>${esc(x.name)}${x.done?' <span class="tag">已完成</span>':''}</td>
        <td>${x.site?(st?'<span class="tag acc">'+esc(st.name||x.site)+'</span>':'<span class="tag warn">站点已删除</span>'):'<span class="tag">手动</span>'}</td>
        <td>${x.runs||0}</td>
        <td class="muted">${x.last_run
          ?(x.last_ok===false?'<span class="tag err">失败</span> ':'<span class="tag ok">成功</span> ')
            +esc(String(x.last_note||'').slice(0,80))+' <span class="faint">'+ago(new Date(String(x.last_run).replace(/-/g,'/')).getTime()/1000)+'</span>'
          :'<span class="faint">未执行过</span>'}</td>
        <td class="right nowrap">
          <button class="btn sm${x.last_ok===false?' pri':''}" onclick="runTask('${esc(x.id)}')">立即执行</button>
          <button class="btn sm dgr" onclick="delTask('${esc(x.id)}')">删除</button>
        </td></tr>`;
      }).join('')
      :'<tr><td colspan="6">'+empty('还没有手动任务。用下面的「新建任务」添加；关联站点的任务会真的去签到')+'</td></tr>'}
      </tbody></table>
    </div>
    <div class="row" style="margin-top:10px">
      <div><label>任务名</label><input id="tName" placeholder="例如：AnyRouter 每日签到"></div>
      <div><label>关联站点（留空=手动任务）</label>
        <select id="tSite"><option value="">— 不关联 —</option>
        ${sites.map(s=>`<option value="${esc(s.id)}">${esc(s.name)}</option>`).join('')}</select></div>
      <div class="n"><label>&nbsp;</label><button class="btn pri" onclick="addTask()">添加</button></div>
    </div>
    <div class="note" style="margin-top:10px">
      定时规则（每日 ${esc(g.hour||'09:10')} 起错峰执行 · 失败重试 ${g.retry_times||0} 次 ·
      连败 3 次当天放弃）见「设置与日志 → 自动签到」。
    </div>
  </div>

  <div class="card">
    <h2>最近执行结果</h2>
    <div class="scroll sm">
      <table><thead><tr><th>平台</th><th>状态</th><th>详情</th><th>时间</th></tr></thead><tbody>
      ${platforms.filter(p=>p.last&&(p.last.at||p.last.message)).length
        ?platforms.filter(p=>p.last&&(p.last.at||p.last.message)).map(p=>`<tr>
          <td>${esc(p.name||p.id)}</td>
          <td>${p.last.ok?'<span class="tag ok">成功</span>':'<span class="tag err">失败</span>'}</td>
          <td class="muted">${esc(String(p.last.message||'').slice(0,120)||'—')}</td>
          <td class="mono nowrap">${esc(p.last.at||'')}</td>
        </tr>`).join('')
        :'<tr><td colspan="4">'+empty('今天还没有执行记录。点上面的「⚡ 一键执行全部」跑一轮')+'</td></tr>'}
      </tbody></table>
    </div>
  </div>`;
}
// ★ 2026-10-07 任务模块：动作明细展开态 + 一键执行 / 只补跑失败的
let TASK_OPEN = {};
function taskToggleActs(pid){
  TASK_OPEN[pid] = !TASK_OPEN[pid];
  render();
}
async function runGrowth(){
  toast('已触发一轮平台任务…');
  const r=await api('api/growth?trigger=1');
  toast(r.message||'已触发');
  // 任务要跑一会儿，多刷几次让状态及时更新
  setTimeout(loadGrowth,3000);
  setTimeout(loadGrowth,9000);
  setTimeout(loadGrowth,20000);
}
/** 只补跑今天失败/未完成的平台（逐个单独执行，避免连坐） */
async function acRunFailed(){
  const g=(S.data.growth&&S.data.growth.growth)||{};
  const ids=(g.platforms||[]).filter(p=>{
    const l=p.last||{};
    return p.on!==false && !p.done_today && l.ok!==true;
  }).map(p=>p.platform);
  if(!ids.length){ toast('没有需要补跑的平台','ok'); return; }
  toast('补跑 '+ids.length+' 个平台：'+ids.join(', '));
  for(const pid of ids){
    try{ await api('api/autocheckin?action=run',{platforms:[pid],force:true}); }catch(e){}
  }
  setTimeout(loadGrowth,4000);
  setTimeout(loadGrowth,12000);
}

async function runAction(action){
  // action 格式："平台id:动作id"，如 "wb-gateway:growth_tasks"
  const [pid, aid] = action.split(':');
  toast('正在执行: '+aid+'...');
  try {
    const r = await post('api/gwextra?action=exec', {pid, aid});
    if(r.ok !== false){
      toast('执行成功: '+aid, 'ok');
    } else {
      toast('执行失败: '+(r.message||'未知错误'), 'err');
    }
  } catch(e) {
    toast('执行失败: '+e.message, 'err');
  }
}

// ---------------------------------------------------------------- 任务
async function loadTasks(){
  const r=await api('api/tasks');
  S.data.tasks=r;
  render();
}
/** ⚡ 执行全部未完成任务（后端 api/tasks?action=run_all：逐条真实执行并写回结果） */
async function runAllTasks(){
  toast('正在执行未完成任务…');
  try{
    const r=await post('api/tasks?action=run_all',{});
    if(r.ok===false){ toast('执行失败：'+(r.message||''),'err'); return; }
    const rs=r.results||[];
    const okN=rs.filter(x=>x.ok).length;
    toast(rs.length?('执行完成：成功 '+okN+' / '+rs.length):'没有未完成的任务',
          rs.length&&okN<rs.length?'err':'ok');
    loadTasks();
  }catch(e){ toast('执行失败：'+e.message,'err'); }
}
async function addTask(){
  const n=$('#tName').value.trim(),s=$('#tSite').value;
  if(!n)return toast('任务名必填','err');
  const r=await post('api/tasks?action=add',{name:n,site:s});
  toast(r.message||'已添加',r.ok?'ok':'err');
  $('#tName').value='';
  loadTasks();
}
async function tgTask(id){await post('api/tasks?action=toggle',{id});loadTasks();}
async function delTask(id){if(!confirm('删除该任务？'))return;await post('api/tasks?action=delete',{id});loadTasks();}
async function runTask(id){
  const r=await post('api/tasks?action=run',{id});
  const x=r.result||{};
  toast(x.message||r.message||'已执行',x.ok?'ok':'err');
  loadTasks();
}

// ---------------------------------------------------------------- 用量
async function loadUsage(){
  const r=await api('api/usage?range=all');
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
  <div class="card mb">
    <div class="flex" style="justify-content:space-between;align-items:center">
      <h2 style="margin:0">用量统计</h2>
      <div class="srcseg">
        <button class="on">全部</button>
        <button>今天</button>
        <button>7天</button>
        <button>30天</button>
      </div>
    </div>
  </div>

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
  const recent=p.recent||[];
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
    ${recent.length?`<div class="scroll sm mt"><h3 style="margin-bottom:8px">最近调用记录（前 20 条）</h3>
      <table><thead><tr><th>时间</th><th>平台</th><th>模型</th><th>IP</th><th>UA</th><th>状态</th></tr></thead><tbody>
      ${recent.map(x=>`<tr>
        <td class="mono nowrap">${esc(new Date(x.ts*1000).toLocaleString())}</td>
        <td class="mono">${esc(x.platform||'-')}</td>
        <td class="mono" style="max-width:180px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(x.model||'-')}</td>
        <td class="mono">${esc(x.ip||'-')}</td>
        <td class="mono" style="max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(x.ua||'')}">${esc((x.ua||'').substring(0,30))}</td>
        <td>${x.ok?'<span class="tag ok">成功</span>':'<span class="tag warn">失败</span>'}</td>
      </tr>`).join('')}
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
  const g=await api('api/models?action=grouped').catch(()=>null);
  if(g) S.data.grouped=g;
  // ★ 2026-10-13：只有当前在路由页才需要立即重绘；在接入源页时整页 render
  //   就是「刷新模型闪一下」的根因之一（grouped 已进 S.data，切页自更新）。
  if(S.v==='route') render();
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
// ★ 2026-10-13（用户要求）：模型总表按接入状态拆两栏 ——
//   已接入默认展开、未接入默认收起；组内模型行仍默认折叠（GM_OPEN 不变）。
let GM_SEC={on:true,off:false};
function gmToggleSec(k){
  GM_SEC[k]=!GM_SEC[k];
  const sec=document.querySelector('.gm-sec[data-sec="'+k+'"]');
  if(sec){
    const head=sec.querySelector('.gm-head');
    const body=sec.querySelector('.gm-sec-body');
    if(head)head.classList.toggle('open',!!GM_SEC[k]);
    if(body)body.style.display=GM_SEC[k]?'':'none';
  }
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

function gmJs(s){                 // 模型 id 只含 [\w.@:/ -]，兜底替换掉危险字符
  // ★ 2026-10-13：放行 '/'，否则 Kilo 等带 org/ 前缀的免费模型 id 会被
  //   二次破坏成 poolside_laguna-s-2.1:free，导致测试/开关/复制按钮发错 id。
  //   '/' 在单引号 JS 串 + 双引号 HTML 属性里是安全字符，无需替换。
  return String(s==null?'':s).replace(/[^\w.@:\/-]/g,'_');
}
function gmCred(c){               // 积分倍率显示：'x2.00 credits' → 'x2.00'
  const s=String(c==null?'':c).trim();
  if(!s) return '';
  const m=s.match(/x?\s*([0-9]+(?:\.[0-9]+)?)/i);
  return m?('x'+m[1]):s;
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
  const r=await api('api/models?action=test_model',{model:id})
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

// ★ 2026-10-07 新增：模型总表逐个启用/关闭（写进后端 disabled_models）。
//   关闭的模型不再进路由表，也不再出现在 /v1/models —— 对应参考项目
//   "选择器只给真能用的模型" 的思路，但把决定权交给用户。
async function toggleModel(id){
  try{
    const r = await post('api/models?action=toggle_model', {id});
    if(r.ok===false){ toast('操作失败：'+(r.message||''), 'err'); return; }
    // 就地更新这一行，避免整表重绘造成滚动位置丢失
    const rows = document.querySelectorAll('.gm-id');
    for(const el of rows){
      if(el.textContent === id){
        const tr = el.closest('tr');
        const btn = tr && tr.querySelector('button');
        if(tr) tr.style.opacity = r.on ? '' : '.45';
        if(btn){
          btn.className = 'btn sm' + (r.on ? ' pri' : '');
          btn.textContent = r.on ? '已启用' : '已关闭';
          btn.title = r.on ? '当前已启用：点一下关闭' : '当前已关闭：不进路由';
        }
        break;
      }
    }
    toast((r.on?'已启用 ':'已关闭 ')+id, 'ok');
    // 路由表变了，后台静默刷新一次总表计数
    loadRoute().catch(()=>{});
  }catch(e){ toast('操作失败：'+e.message, 'err'); }
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
  // ★ 2026-10-13：搜索时两个分栏都展开（否则未接入栏默认收起，搜不到）；
  //   清空搜索后恢复各自的默认/手动状态。
  document.querySelectorAll('.gm-sec-body').forEach(b=>{
    b.style.display = q ? '' : (GM_SEC[b.getAttribute('data-sec')] ? '' : 'none');
  });
  if(!q){
    document.querySelectorAll('.gm-sec-head').forEach(h2=>{
      const sec=h2.closest('.gm-sec');
      h2.classList.toggle('open', !!(sec&&GM_SEC[sec.getAttribute('data-sec')]));
    });
  }
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
  // ★ 2026-10-13（用户要求）：按接入状态拆两栏 —— 已接入默认展开、
  //   未接入默认收起；组内模型行仍默认折叠（GM_OPEN 语义不变）。
  const isAcc=(grp)=>grp.pid==='auto'||grp.free_lane||(grp.usable||0)>0;
  for(const secKey of ['on','off']){
    const list=groups.filter(secKey==='on'?isAcc:(x=>!isAcc(x)));
    if(!list.length)continue;
    const sOpen=!!GM_SEC[secKey];
    h+='<div class="gm-sec" data-sec="'+secKey+'">'
      +'<div class="gm-head gm-sec-head'+(sOpen?' open':'')+'" onclick="gmToggleSec(\''+secKey+'\')">'
      +'<span class="caret">▸</span><b>'+(secKey==='on'?'已接入':'未接入')+'</b>'
      +'<span class="tag">'+list.length+' 个来源</span>'
      +'<span class="faint gm-hint">'+(secKey==='on'?'有账号 / 免凭据，可直接用':'登录后即可用')+'</span>'
      +'</div>'
      +'<div class="gm-sec-body"'+(sOpen?'':' style="display:none"')+'>';
    for(const grp of list){
    // ★ 默认全部收起（2026-10-06 用户要求）：点组头展开，手动展开过的组
    //   记在 localStorage（GM_OPEN 初始化时读回）。行节点总是渲染，
    //   收起只切 display —— 否则点开时没有内容可显示。
    const open=!!GM_OPEN[grp.pid];
    h+='<div class="gm-group" data-pid="'+esc(grp.pid)+'">'
      +'<div class="gm-head'+(open?' open':'')+'" onclick="gmToggle(\''+gmJs(grp.pid)+'\')">'
      +'<span class="caret">▸</span><b>'+esc(grp.name)+'</b>'
      // ★ 2026-10-07（参照 dsh-our-free-model 的来源卡片）：直接显示
      //   已启用/总数 + 账号数，一眼看清"账号 n / 模型 n"
      +'<span class="tag">'+((typeof grp.models_on==='number'?grp.models_on:grp.models.length)
        +'/'+grp.models.length+' 模型')+'</span>'
      +(grp.free_lane
        ?'<span class="tag ok">免凭据直连</span>'
        :(grp.usable>0
          ?'<span class="tag ok">✓ '+grp.usable+' 账号</span>'
          :'<span class="tag">未接入</span>'))
      +(grp.live?'<span class="tag info">实时清单</span>':'')
      +(grp.hint?'<span class="faint gm-hint">'+esc(grp.hint)+'</span>':'')
      +'</div>';
    if(grp.models.length){
      h+='<div class="gm-rows"'+(open?'':' style="display:none"')+'><div class="dtw" style="max-height:420px"><table><thead><tr>'
        +'<th style="width:88px">启用</th>'
        +'<th>模型 ID（点击复制）</th><th>名称</th><th>倍率</th><th>上下文</th>'
        +'<th>输出</th><th>说明</th><th style="width:130px">连接测试</th>'
        +'</tr></thead><tbody>';
      for(const m of grp.models){
        const id=m.id||'';
        const off = (m.on===false);
        h+='<tr'+(off?' class="off" style="opacity:.45"':'')
          +' data-s="'+esc((id+' '+(m.name||'')+' '+(m.desc||'')).toLowerCase())+'">'
          +'<td class="nowrap"><button class="btn sm'+(off?'':' pri')
          +'" title="'+(off?'当前已关闭：不进路由':'当前已启用：点一下关闭')
          +'" onclick="event.stopPropagation();toggleModel(\''+gmJs(id)+'\')">'
          +(off?'已关闭':'已启用')+'</button></td>'
          +'<td><span class="mono gm-id" title="点击复制" '
          +'onclick="copyText(\''+gmJs(id)+'\',()=>toast(\'已复制\',\'ok\'))">'
          +esc(id)+'</span></td>'
          +'<td>'+esc(m.name||'')+'</td>'
          +'<td>'+(m.credits?'<span class="tag acc">'+esc(gmCred(m.credits))+'</span>'
            :'<span class="faint">—</span>')+'</td>'
          +'<td class="muted">'+(m.ctx?Number(m.ctx).toLocaleString():'—')+'</td>'
          +'<td class="muted">'+(m.out?Number(m.out).toLocaleString():'—')+'</td>'
          +'<td class="faint gm-desc">'+esc(m.desc||'')+'</td>'
          +'<td class="nowrap">'+gmTestCell(id)+'</td>'
          +'</tr>';
      }
      h+='</tbody></table></div></div>';
    }else if(!grp.models.length && open){
      h+='<div class="gm-rows"><div class="empty" style="padding:14px">'
        +'该来源没有静态清单 —— 登录后用「接入源 → 详情 → 查看该源模型」动态拉取</div></div>';
    }
    h+='</div>';
    }
    h+='</div></div>';
  }
  h+='<div class="dtfoot"><span>共 '+(g.total||0)+' 个模型 · '+groups.length
    +' 个来源 · 「测试」= 发一条极小真实请求验证连通（消耗极少额度）</span></div></div>'
    +viewModelsHelp();
  return h;
}

// ---------------------------------------------------------------- 站点
async function loadSites(){
  const r=await api('api/sites');
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
      <div><label>签到路径</label><input id="sPath" value="api/user/checkin"></div>
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
    checkin_path:$('#sPath').value.trim()||'api/user/checkin',
    checkin:true,
    in_route:$('#sRoute').checked,
    route_endpoint:$('#sRep').value.trim(),
    route_model:$('#sRModel').value.trim()||'default',
    route_weight:parseInt($('#sRWeight').value||'100',10),
    route_priority:parseInt($('#sRPriority').value||'10',10),
  };
  if(!s.name||!s.url)return toast('名称与地址必填','err');
  const r=await post('api/sites?action='+(s.id?'update':'add'),s);
  toast(r.message||'已保存',r.ok?'ok':'err');
  clearForm();loadSites();
}
function clearForm(){
  ['sId','sName','sUrl','sToken','sUser','sCookie','sU2','sP2','sRep','sRModel'].forEach(i=>$('#'+i).value='');
  $('#sPath').value='api/user/checkin';
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
  $('#sPath').value=s.checkin_path||'api/user/checkin';
  $('#sRoute').checked=!!s.in_route;
  $('#sRep').value=s.route_endpoint||'';
  $('#sRModel').value=s.route_model||'';
  $('#sRWeight').value=s.route_weight!=null?s.route_weight:100;
  $('#sRPriority').value=s.route_priority!=null?s.route_priority:10;
  window.scrollTo({top:document.body.scrollHeight,behavior:'smooth'});
}
async function delSite(id){
  if(!confirm('删除该站点配置？'))return;
  const r=await post('api/sites?action=delete',{id});
  toast(r.message||'已删除',r.ok?'ok':'err');
  loadSites();
}
async function probeOne(id){
  S.data.probeOut='探测中…';
  render();
  const r=await post('api/sites?action=probe',{id});
  S.data.probeOut=JSON.stringify(r.probe||r,null,2);
  render();
}
async function probeAll(){
  S.data.probeOut='批量探测中，请稍候…';render();
  const r=await post('api/sites?action=probe_all',{});
  S.data.probeOut=JSON.stringify(r.results||r,null,2);
  render();
}
async function balAll(){
  S.data.probeOut='查询余额中…';render();
  const r=await post('api/sites?action=balance_all',{});
  S.data.probeOut=JSON.stringify(r.results||r,null,2);
  render();
}
async function importCatalog(){
  const cat=S.data.catalogSites||[];
  const payload=cat.map(s=>({
    name:s.name,url:s.url,token:'',cookie:'',username:'',password:'',
    checkin:!!s.checkin,checkin_path:s.name==='AnyRouter'?'api/user/sign_in':'api/user/checkin',
    category:s.category,bonus:s.bonus,limit:s.limit,
  }));
  const r=await post('api/sites?action=import',{sites:payload});
  toast('已导入 '+r.imported+' 个站点，请逐个补凭据',r.ok?'ok':'err');
  loadSites();
}

// ---------------------------------------------------------------- 导航
async function loadCatalog(){
  const r=await api('api/catalog');
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
    <div class="flex" style="justify-content:space-between;cursor:pointer" onclick="toggleRouteDetail()">
      <h2>候选上游明细 <span id="routeDetailCaret">▸</span></h2>
      <span class="faint" style="font-size:12px">点击展开/收起</span>
    </div>
    <div id="routeDetailBody" style="display:none">
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
  </div>

  <div class="card">
    <div class="flex" style="justify-content:space-between">
      <h2>每日免费模型榜 <span class="faint" style="font-size:12px" id="freeFeedTs"></span></h2>
      <button class="btn" onclick="loadFreeModels(true)">立即刷新</button>
    </div>
    <div class="faint" style="margin:4px 0 10px">数据源 ClawLabsAI/free-ai-models（每天 04:00 UTC 更新）；<b>不训练</b> 表示提供商承诺不用你的提示词训练模型。模型 id 可直接用于对应提供商的 OpenAI SDK。<br><b>已集成</b> 标记 = 面板已内置该平台（OpenRouter 填 Key 即用 / Pollinations 免密钥），不再另建重复入口；未标记的需自行注册。</div>
    <div id="freeModelsBody" class="scroll" style="max-height:560px">${freeModelsHtml()}</div>
  </div>
`;
}
function strategyLabel(s){
  return {fastest:'最快延迟',weight:'权重优先',priority:'优先级'}[s]||s||'—';
}
// -------------------------------------------------- 每日免费模型榜
function freeModelsHtml(){
  const f=S.data.freeModels;
  if(!f){return '<div class="faint">加载中…</div>';}
  if(!f.models||!f.models.length){
    return '<div class="faint">'+(f.note||'暂无数据')+'</div>';
  }
  const trainTag=u=>{
    if(u==='no-training')return tag('不训练','ok');
    if(u==='may-train')return tag('可能训练','err');
    if(u==='mixed')return tag('混合','warn');
    return tag('未知','info');
  };
  const rows=f.models.slice(0,30).map(m=>{
    const prov=(m.provider||(m.served_by&&m.served_by[0])||'—');
    const ctx=m.context_window?Math.round(m.context_window/1024)+'K':'—';
    const tools=m.supports_tools?'✓':'—';
    const integ=m.integrated;
    const integTag=integ
      ? (integ==='pollinations'
          ? tag('已集成·免密钥','ok')
          : tag('已集成·填Key即用','info'))
      : '';
    return `<tr>
      <td><b>${esc(m.rank!=null?m.rank:'—')}</b></td>
      <td><b class="mono">${esc(m.id)}</b><div class="faint" style="font-size:11px">${esc(m.name||'')}</div>${integTag}</td>
      <td>${esc(prov)}</td>
      <td>${trainTag(m.data_use)}</td>
      <td class="mono">${ctx}</td>
      <td>${tools}</td>
      <td class="faint" style="font-size:11px">${esc(m.rate_limit||'')}</td>
    </tr>`;
  }).join('');
  return `<table><thead><tr><th>#</th><th>模型 id</th><th>提供商</th><th>隐私</th><th>上下文</th><th>工具</th><th>限额</th></tr></thead><tbody>${rows}</tbody></table>`;
}
async function loadFreeModels(force){
  try{
    const r=await api('api/free_models'+(force?'?refresh=1':''));
    S.data.freeModels=r;
    const ts=r.updated_at?('源更新 '+new Date(r.updated_at).toLocaleString()):'';
    const el=document.getElementById('freeFeedTs');
    if(el)el.textContent=ts+' · 共'+(r.total||0)+'个';
    const body=document.getElementById('freeModelsBody');
    if(body)body.innerHTML=freeModelsHtml();
  }catch(e){
    toast('免费模型榜加载失败：'+(e&&e.message||e),'err');
  }
}
async function probeRoute(){
  toast('探测中，各上游并发测 /models…');
  const r=await post('api/route?action=probe',{});
  S.data.routeOut=JSON.stringify(r.results||r,null,2);
  S.data.probe=r;
  // 探测响应同样不含 config/in_route_sites：重新拉一次路由总览，别就地覆盖
  await loadRoute().catch(()=>{});
  render();
  toast('探测完成，延迟已写入 EWMA','ok');
}
async function testRoute(model){
  S.data.routeOut='试跑 '+model+' …';
  render();
  const r=await post('api/route?action=test_chat',{model,message:'ping'});
  S.data.routeOut=JSON.stringify(r,null,2);
  // ★ 2026-10-07：不能再把结果写进 S.data.route —— test_chat 的响应里没有
  //   models/config/in_route_sites，覆盖后整张路由表会变成"路由表为空"、
  //   KPI 全归零。试跑结果单独放 routeTest。
  S.data.routeTest=r;
  render();
  const x=r.result||{};
  toast(x.choices?'试跑成功，走 '+(x._route||{}).upstream:'试跑失败：'+(x.message||'未知'),
        x.choices?'ok':'err');
}

// ---------------------------------------------------------------- 上游档案
// ---------------------------------------------------------------- 通知
async function loadNotify(){
  const r=await api('api/notify');
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
  await api('api/notify?action=send',{event:'checkin',title:'开关已更新',body:'通知开关修改已保存'});
  const r=await fetch('api/notify');
  const j=await r.json();
  const cfg=j.notify;
  cfg.notify_checkin=$('#nCheck').checked;
  cfg.notify_quota=$('#nQuota').checked;
  cfg.notify_error=$('#nErr').checked;
  await post('api/notify?action=__noop',{});
  // 直接覆盖保存
  await api('api/notify?action=save_flags',{notify:cfg});
  toast('已保存','ok');
}
async function addHook(){
  const h={type:$('#wType').value,name:$('#wName').value.trim(),url:$('#wUrl').value.trim(),enabled:true,events:[]};
  if(!h.url)return toast('地址必填','err');
  const r=await post('api/notify?action=add',h);
  toast(r.message||'已添加',r.ok?'ok':'err');
  loadNotify();
}
async function delHook(i){
  if(!confirm('删除该 Webhook？'))return;
  await post('api/notify?action=delete',{index:i});
  loadNotify();
}
async function testHook(){
  const h={type:$('#wType').value,url:$('#wUrl').value.trim()};
  const r=await post('api/notify?action=test',h);
  toast((r.sent?'发送成功：':'失败：')+r.message,r.sent?'ok':'err');
  loadNotify();
}

// ---------------------------------------------------------------- 日志
async function loadLogs(){
  const r=await api('api/logs');
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
  // ★ 2026-10-07 修（设置页数据丢失）：原来 Promise.all 的返回值被直接丢弃，
  //   而 S.data.settings 只在一处死代码（loadGateway）里被赋值 —— 于是
  //   输入框显示硬编码默认值，点「保存设置」又把这些默认值 POST 回去，
  //   静默覆盖用户已存的 lobster_server / proxy_url 等；日志 tab 也永远空白。
  const [st, sites, nt, lg] = await Promise.all([
    api('api/settings').catch(()=>({settings:{}})),
    api('api/sites').catch(()=>({sites:[],catalog:[]})),
    api('api/notify').catch(()=>({config:{}})),
    api('api/logs').catch(()=>({lines:[]})),
  ]);
  S.data.settings=st;
  if(sites&&sites.sites)S.data.sites=sites.sites;
  if(sites&&sites.catalog)S.data.siteCatalog=sites.catalog;
  S.data.notify=nt;
  S.data.logs=lg;
}

function viewSettings(){
  const tabs=[['main','🌐 代理设置'],['logs','≡ 日志']];
  let h='<div class="tabs">'
    +tabs.map(x=>'<button class="'+(SET_TAB===x[0]?'on':'')+'" '
      +'onclick="setSetTab(\''+x[0]+'\')">'+x[1]+'</button>').join('')
    +'</div>';
  if(SET_TAB==='main') h+=viewSettingsMain();
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

    <div class="card">
      <h2>网络代理</h2>
      <label>代理模式</label>
      <select id="sProxyMode" style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px">
        <option value="direct" ${s.proxy_mode==='direct'?'selected':''}>全部直连（不用代理）</option>
        <option value="bypass_cn" ${s.proxy_mode==='bypass_cn'?'selected':''}>绕过大陆（推荐）— 国内直连，海外走代理</option>
        <option value="global" ${s.proxy_mode==='global'?'selected':''}>全局代理 — 所有请求都走代理</option>
      </select>
      <label style="margin-top:10px;display:block">代理地址</label>
      <input id="sProxyUrl" class="mono" placeholder="例如：http://127.0.0.1:7890 或 socks5://127.0.0.1:1080" value="${esc(s.proxy_url||'')}">
      <div class="faint" style="font-size:11.5px;margin-top:4px">
        支持 HTTP / HTTPS / SOCKS5 代理。
      </div>
      <div class="note" style="margin-top:10px">
        <b>绕过大陆模式：</b><br>
        · <b>直连：</b>Kimi、智谱、豆包、Trae、小浣熊、WorkBuddy 等国内平台<br>
        · <b>走代理：</b>OpenAI、Claude、Google、Devin、xAI、Meta 等海外平台
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
    proxy_mode:$('#sProxyMode').value,
    proxy_url:$('#sProxyUrl').value.trim(),
  };
  const r=await post('api/settings?action=save',p);
  toast(r.message||'已保存',r.ok?'ok':'err');
  refresh();loadSettings();
}

// ---------------------------------------------------------------- 通用
async function quitPanel(){
  if(!confirm('退出面板？\n\n· 面板会关闭，http 服务停止\n· 想后台常驻请用「开机自启动」'))return;
  const r=await post('api/settings?action=shutdown',{});
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
// ★ 2026-10-07：这两个函数被 6 处 onclick 引用，但**全文件从未定义** ——
//   点「复制接入地址」「点模型 ID 复制」都是 ReferenceError（按钮毫无反应）。
function copyText(text, done){
  const s=String(text==null?'':text);
  const after=(ok)=>{ try{ if(typeof done==='function') done(ok); else toast(ok?'已复制':'复制失败', ok?'ok':'err'); }catch(e){} };
  try{
    if(navigator.clipboard && navigator.clipboard.writeText){
      navigator.clipboard.writeText(s).then(()=>after(true)).catch(()=>after(fallbackCopy(s)));
      return;
    }
  }catch(e){}
  after(fallbackCopy(s));
}
function fallbackCopy(s){
  try{
    const ta=document.createElement('textarea');
    ta.value=s; ta.style.position='fixed'; ta.style.opacity='0';
    document.body.appendChild(ta); ta.select();
    const ok=document.execCommand('copy');
    ta.remove();
    return ok;
  }catch(e){ return false; }
}
function copyEndpoint(){
  copyText(panelBaseUrl(), (ok)=>toast(ok?('已复制 '+panelBaseUrl()):'复制失败', ok?'ok':'err'));
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
  const lp=await api('api/login?action=platforms').catch(()=>({platforms:[]}));
  const ac=await api('api/accounts').catch(()=>({accounts:[],stats:{}}));
  const pa=await api('api/platform?action=actions').catch(()=>({}));
  const bd=await api('api/login?action=browser').catch(()=>({browser:null}));
  S.data.loginPlatforms=lp.platforms||[];
  S.data.accounts=ac.accounts||[];
  S.data.accountStats=ac.stats||{};
  S.data.platformActions=pa||{};
  S.data.browserDiag=(bd&&bd.browser)||null;
  render();
}

async function startLogin(pid,edition,headless,forceBrowser){
  // headless=1：不依赖你本机 Chrome/Edge，由服务器起无头浏览器打开登录页，
  // 二维码抓回面板里用手机扫，登录后 Cookie 自动捕捉入库（远程部署也全流程可用）
  // forceBrowser=1：file 类平台走「跳转登录」——弹出浏览器打开登录页，
  // 用户登录后面板自动抓取 Cookie/localStorage 凭据（2026-10-12 补第二方式）
  toast(headless?'正在启动服务端无头浏览器…'
    :(forceBrowser?'正在打开浏览器，请在里面完成登录…':'正在发起登录…'));
  const body={platform:pid};
  if(edition)body.edition=edition;
  if(headless)body.headless=1;
  if(forceBrowser)body.force_browser=1;
  const r=await api('api/login?action=start',body).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'发起登录失败','err');return;}
  S.data.loginSession=r.session||null;
  render();
  const s=S.data.loginSession;
  // cookie 会话（尤其服务端无头模式）也轮询：看守线程自动捕捉到 Cookie 能即时反馈到 UI
  if(s&&(s.method==='qrcode'||s.method==='remotescan'||s.method==='cookie')
     &&(s.status==='pending'||s.status==='waiting'))startLoginPoll();
}

// 服务端远程扫码（无需本机浏览器）：直接打平台 Web 二维码接口
async function startQrScan(pid){
  toast('正在生成远程二维码…');
  const r=await api('api/login?action=qrscan',{platform:pid}).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'发起远程扫码失败','err');return;}
  S.data.loginSession=r.session||null;
  render();
  const s=S.data.loginSession;
  if(s&&s.method==='remotescan'&&(s.status==='pending'||s.status==='waiting'))startLoginPoll();
}

function startLoginPoll(){
  stopLoginPoll();
  let lastSig='';
  LOGIN_TIMER=setInterval(async()=>{
    const s=S.data.loginSession;
    if(!s){stopLoginPoll();return;}
    const r=await api('api/login?action=poll',{id:s.id}).catch(()=>null);
    if(!r||!r.session){stopLoginPoll();return;}
    S.data.loginSession=r.session;
    // ★ 抽屉打开时只在会话状态**真的变化**时才全量重绘（状态/二维码/
    //   授权链接/消息/倒计时桶）—— 无脑每 2 秒重绘会闪烁+费电。
    const sig=[r.session.status, r.session.qr?'q':'', r.session.auth_url||'',
               (r.session.seconds_left||0)>>4,
               (r.session.message||'').slice(0,80)].join('|');
    const changed = sig!==lastSig;
    lastSig = sig;
    if(SRC_EXPAND){ if(changed) render(); } else { paintLoginSession(); }
    if(r.session.status==='success'){
      stopLoginPoll();
      toast('登录成功，凭据已保存','ok');
      // ★ 2026-10-12（用户要求）：登录完成后自动收回展开的接入面板
      SRC_EXPAND=null;
      loadLogin();
      loadSources();
    }else if(r.session.status==='error'||r.session.status==='expired'){
      stopLoginPoll();
      toast(r.session.message||r.session.error||'登录结束','err');
      if(SRC_EXPAND){ render(); } else { paintLoginSession(); }
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
  // ★ 2026-10-12（用户拍板「取消所有扫码连接，全部改为跳转」）：本地**不再渲染任何
  //   二维码**——扫码所需的无头浏览器已被判定为「麻烦」，跳转制下只用 auth_url 链接。
  //   故此处恒返回空，旧调用点（viewLogin 等）也只会得到空内容，不会出码。
  return '';
}

function paintLoginSession(){
  const s=S.data.loginSession;
  if(!s)return;
  const st=$('#lsStatus'),qr=$('#lsQr'),msg=$('#lsMsg'),left=$('#lsLeft');
  if(st)st.innerHTML=loginStatusTag(s);
  if(msg)msg.textContent=s.message||s.error||'';
  if(left)left.textContent=s.seconds_left?('剩余 '+s.seconds_left+' 秒'):'';
  // ★ 2026-10-12（跳转制）：取消所有本地二维码渲染 —— 不再用 qrOrLinkHTML 覆盖
  //   #lsQr（会在 auth_url 存在时现场出码，且会冲掉 oauth_device 的设备码）。
  //   #lsQr 的静态内容由 viewLoginSession 初次渲染负责（仅 oauth_device 显示设备码）。
}

function loginStatusTag(s){
  const m={pending:'info',waiting:'info',success:'ok',error:'err',expired:'warn'};
  return tag(s.status||'—',m[s.status]||'');
}

async function cancelLogin(){
  const s=S.data.loginSession;
  stopLoginPoll();
  if(s)await api('api/login?action=cancel',{id:s.id}).catch(()=>null);
  S.data.loginSession=null;
  render();
}

async function pollCookieOnce(){
  const s=S.data.loginSession;
  if(!s){toast('没有进行中的会话','err');return;}
  const r=await api('api/login?action=poll',{id:s.id}).catch(e=>({ok:false,message:e.message}));
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

/** 规范化用户粘贴的 Cookie：
 *  · F12 → Network → Copy Request Headers 常连 `cookie:` 前缀一起复制进来；
 *  · 也有人把整行 "cookie: a=1; b=2" 或带引号的 JSON 片段粘进来。
 *  这里统一剥成 `a=1; b=2`，否则后端 verify 必然失败（用户视角＝"已登录无法识别"）。 */
function normCookieText(v){
  let s=(v||'').trim();
  if(/^cookie\s*:/i.test(s))s=s.replace(/^cookie\s*:\s*/i,'');
  // 去掉换行（多行粘贴）与首尾引号
  s=s.replace(/[\r\n]+/g,'; ').replace(/^["']|["']$/g,'').trim();
  return s;
}

async function submitManualCookie(pid){
  const s=S.data.loginSession;
  pid=pid||(s?s.platform:'');
  const box=pid?$('#manualCookie_'+pid):$('#manualCookie');
  const v=normCookieText(box?box.value:'');
  if(!v){toast('请先粘贴 Cookie','err');return;}
  if(!pid){toast('缺少平台','err');return;}
  const r=await api('api/login?action=submit',
    {id:s?s.id:undefined,platform:pid,cookie:v}).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'保存失败','err');return;}
  if(r.session){
    S.data.loginSession=r.session;
    paintLoginSession();
    if(r.session.status==='success'){toast('已保存','ok');loadLogin();loadSources();}
    else toast(r.session.message||r.session.error||'未通过校验','warn');
  }else{toast('已保存','ok');loadLogin();loadSources();}
}

async function submitManualSecret(pid,type){
  const s=S.data.loginSession;
  pid=pid||(s?s.platform:'');
  const box=pid?$('#manualSecret_'+pid):$('#manualSecret');
  let v=(box?box.value:'').trim();
  // Token 类粘贴同样可能带 `Authorization: Bearer ` 前缀 / 换行
  v=v.replace(/^authorization\s*:\s*bearer\s+/i,'').replace(/[\r\n]+/g,'').trim();
  if(!v){toast('请先粘贴凭据内容','err');return;}
  if(!pid){toast('缺少平台','err');return;}
  const r=await api('api/login?action=submit',
    {id:s?s.id:undefined,platform:pid,secret:v,type:type||'token'})
    .catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'保存失败','err');return;}
  if(r.session){
    S.data.loginSession=r.session;
    paintLoginSession();
    if(r.session.status==='success'){toast('已保存','ok');loadLogin();loadSources();}
    else toast(r.session.message||r.session.error||'未通过校验','warn');
  }else{toast('已保存','ok');loadLogin();loadSources();}
}

async function delAccount(id){
  if(!confirm('确认删除该账号凭据？'))return;
  const r=await api('api/accounts?action=delete',{id:id}).catch(e=>({ok:false}));
  if(r.ok===false){toast('删除失败','err');return;}
  toast('已删除','ok');
  loadLogin();
}

async function toggleAccount(id,on){
  await api('api/accounts?action=update',{id:id,enabled:on}).catch(()=>null);
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
  const r=await api('api/platform?action=call',{platform:pid,action:act,params:params})
    .catch(e=>({ok:false,message:e.message}));
  const box=$('#paResult');
  if(box)box.textContent=JSON.stringify(r.result||r,null,2);
  if(r.ok===false)toast(r.message||'调用失败','err');
  else toast('调用成功','ok');
}

async function platformCheckin(pid){
  toast('正在签到…');
  const r=await api('api/platform?action=checkin',{platform:pid})
    .catch(e=>({ok:false,message:e.message}));
  const box=$('#paResult');
  if(box)box.textContent=JSON.stringify(r.result||r,null,2);
  if(r.ok===false)toast(r.message||'签到失败','err');
  else toast('签到完成','ok');
}

async function platformProbe(pid){
  toast('正在扫描 '+pid+' 的真实端点…（整站鉴权的平台需要先存凭据）');
  const r=await api('api/platform?action=probe',{platform:pid})
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

/* ★ 2026-10-13：原「工具调用测试台」随「工具与集成」整栏删除。
   后端 api/toolcall 端点与本地执行器保留（OpenAI 兼容端点本身支持 tools
   协议，客户端自带 tools 字段即可用），只是不再有面板内测试台。 */

/* ================================================================ 本地反代上游
 * 把 CLIProxyAPI 集成进面板：启停、配置、OAuth 登录全在界面里完成，
 * 用户不必手动装服务、改 YAML、敲命令行。
 */
function loadLocalproxy(){
  api('api/localproxy?action=status').then(r=>{
    S.data.lp=r;
    render();
  }).catch(e=>toast('读取本地上游状态失败：'+e.message,'err'));
}
async function lpAct(action, body){
  const label={start:'启动',stop:'停止',restart:'重启',config:'保存配置',login:'登录'}[action]||action;
  toast(label+'中…');
  const r=await api('api/localproxy?action='+action, body||{})
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
  const r=await api('api/localproxy?action=log',{lines:120}).catch(e=>({ok:false}));
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
    api('api/login?action=platforms').catch(()=>({platforms:[]})),
    api('api/accounts').catch(()=>({accounts:[],stats:{}})),
    api('api/platform?action=actions').catch(()=>({})),
    api('api/autocheckin?action=status').catch(()=>({platforms:[]})),
    api('api/login?action=browser').catch(()=>({browser:null})),
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
          :'<button class="btn sm pri" onclick="setAccTab(\'login\');startLogin(\''+esc(x.id)+'\',\''+esc(x.edition||'')+'\')">去登录</button>'
           +(x.method==='cookie'
             ?' <button class="btn sm" title="无需本机浏览器：服务器起无头 Chromium 打开登录页，二维码抓回面板用手机扫，登录后 Cookie 自动入库" '
              +'onclick="startLogin(\''+esc(x.id)+'\',\''+esc(x.edition||'')+'\',1)">🌐 远程登录</button>'
             :'+')},
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
  const r1=await api('api/autocheckin?action=run',{}).catch(e=>({ok:false,message:e.message}));
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
  const r2=await api('api/tasks?action=run_all',{}).catch(()=>({ok:false}));
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
  api('api/autocheckin?action=status').then(r=>{
    S.data.autoCheckin=r;
    render();
  }).catch(e=>toast('加载自动签到配置失败：'+e.message,'err'));
}

async function acSave(){
  const st=S.data.autoCheckin||{};
  // ★ 2026-10-07 修：后端 api_autocheckin?action=save 只认 **dict**
  //   （isinstance(b["platforms"], dict)），前端原来发的是数组 —— 平台级
  //   开关/时间被整段静默忽略（顶层 enabled/notify 能存，平台级永远存不上）。
  const pls={};
  for(const p of (st.platforms||[])){
    pls[p.platform]={
      on:!!(p._on!==undefined?p._on:p.on),
      time:(p._time!==undefined?p._time:p.time),
    };
  }
  const b={
    enabled:!!(st._enabled!==undefined?st._enabled:st.enabled),
    stagger_sec:st.stagger_sec, retry_times:st.retry_times,
    retry_delay_min:st.retry_delay_min, notify:!!st.notify,
    platforms:pls,
  };
  const r=await api('api/autocheckin?action=save',b).catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'保存失败','err');return;}
  S.data.autoCheckin=r;
  toast('已保存','ok'); render();
}

async function acRun(force){
  toast(force?'立即执行全部平台（忽略今天已跑）':'开始执行一轮…');
  const r=await api('api/autocheckin?action=run',{force:!!force})
    .catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'执行失败','err');return;}
  toast('已触发，结果稍后刷新','ok');
  setTimeout(loadAutocheckin,3000);
  setTimeout(loadAutocheckin,12000);
}


async function acRunOne(platform){
  toast('正在执行 '+platform+' …');
  const r=await api('api/autocheckin?action=run',{platforms:[platform],force:true})
    .catch(e=>({ok:false,message:e.message}));
  if(r.ok===false){toast(r.message||'失败','err');return;}
  toast('已触发','ok');
  setTimeout(loadAutocheckin,4000);
}

/** copilot 系平台 id —— 合并卡 key='copilot'，会话匹配要按这一族判断 */
const COPILOT_FAMILY=['wb-gateway','wb-gateway-intl','apk-codebuddy','apk-codebuddy-cn'];

/** 登录会话区（扫码 / Cookie 粘贴）。
 *  onlyPid：来源平台 key。传入时只显示该平台的会话（copilot 卡匹配整族），
 *  防止 A 平台登录中、B 平台详情页串台显示 A 的二维码和取消按钮；
 *  不传（账号凭据→当前登录 tab）则始终显示。 */
function viewLoginSession(onlyPid, compact){
  const sess=S.data.loginSession;
  if(sess && onlyPid){
    const match = sess.platform===onlyPid
      || (onlyPid==='copilot' && COPILOT_FAMILY.indexOf(sess.platform)>=0);
    if(!match){
      return '<div class="note">「'+esc(sess.platform_name||sess.platform)+'」正在登录中，'
        +'与当前平台无关。可在「账号与凭据 → 当前登录」查看或取消。</div>';
    }
  }
  if(!sess){
    // 内联卡片模式：没有会话就什么都不渲染（旧详情页才显示引导文案）
    if(compact) return '';
    return '<div class="note">当前没有进行中的登录。去「登录与凭据」选一个平台点「去登录」。</div>';
  }
  let h='';
  // ★ 2026-10-12：成功态收敛成一行（按钮/粘贴框在成功后全是噪音）
  if(compact && sess.status==='success'){
    return '<div class="ls-compact"><div class="row n" style="align-items:center;gap:8px">'
      +'<b style="font-size:12.5px">登录会话</b>'+loginStatusTag(sess)
      +'<span class="faint" style="font-size:11.5px;margin-left:auto">凭据已入库，此面板可以收起了</span>'
      +'</div></div>';
  }
  if(compact){
    // 内联卡片：轻量容器，不带卡片头/探测/平台能力那些大块
    // （★ 状态 tag 带 id="lsStatus"，轮询 paint 才能更新——之前漏了导致
    //   抽屉里永远显示初始 pending）
    h+='<div class="ls-compact"><div class="row n" style="align-items:center;gap:8px;margin-bottom:6px">'
      +'<b style="font-size:12.5px">登录会话</b><span id="lsStatus">'+loginStatusTag(sess)+'</span>'
      +'<span class="faint" style="margin-left:auto;font-size:11px">'
        +(sess.seconds_left?('剩余 '+sess.seconds_left+' 秒'):'')+'</span></div>';
  }else{
    h+='<div class="card" style="margin-bottom:14px"><div class="ch"><b>登录会话</b>'
      +'<span id="lsStatus">'+loginStatusTag(sess)+'</span></div><div class="cb">';
  }
    
    // ★ 2026-10-12（用户拍板「取消所有扫码连接，全部改为跳转」）：
    //   面板**不再渲染二维码/设备码**——点接入即跳转浏览器，用户在那里
    //   完成登录；面板只展示等待态 + 倒计时 + 取消。
    let leftCol = '';
    if(sess.method==='oauth_device'){
      // 设备码协议要求用户在浏览器里输码：只留一个可复制的码，不出大块 UI
      leftCol = '<div style="padding:8px 10px;background:var(--panel-2);border-radius:8px;'
        +'text-align:center"><div style="font-size:11px;color:var(--faint);margin-bottom:2px">'
        +'设备码（复制到浏览器）</div>'
        +'<div style="font-size:16px;font-weight:700;font-family:monospace;letter-spacing:2px">'
        +esc(sess.user_code||'')+'</div></div>';
    }
    // qrcode / oauth_pkce / cookie / file / loomy：无左侧区域（纯跳转等待态）
    
    if(leftCol){
      h+='<div class="row" style="align-items:flex-start">';
      h+='<div class="n" id="lsQr">'+leftCol+'</div>';
      h+='<div style="flex:1;min-width:220px">';
    } else {
      h+='<div>';
    }
    
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
    h+='</div></div>';
    // ★★★ 2026-10-13（用户截图实锤：「已登录，无法识别，且无 cookie 填入框」）：
    //   原实现把手动粘贴框限定在 `!compact` —— 而接入源抽屉正是 compact 模式，
    //   会话一活跃（waiting）本平台的粘贴框就整个消失；偏偏「只开标签」模式下
    //   「我已登录，立即获取」走 browser_cookie 直读，Chrome 127+ app-bound
    //   加密（v20）在 app/browser_cookie.py decrypt_value 里直接返空
    //   （本机 diagnose 实测：auto_supported=False）→ 自动读取必然失败。
    //   → 粘贴框必须**无条件渲染**（compact 也渲染）：它是「只开标签」模式下
    //     唯一可靠的凭据回收路径，也是所有 xxxx2api 社区反代项目的标准姿势
    //     （doubao-free-api / yuanbao-chat2api 等全是手动粘贴 Cookie）。
    //   element id 带 pid 后缀，避免多个抽屉同页渲染时 id 撞车。
    //   pkce / google / lobster 有自己的 code 输入框（下面单独渲染），不重复摆。
    const _pp=(S.data.loginPlatforms||[]).find(p=>p.id===sess.platform)||{};
    const _pm=_pp.method||sess.method;
    const _ptype=_pp.paste_type||(_pm==='cookie'?'cookie':_pm==='file'?'api_key':'token');
    const _pkind=_pm==='cookie'?'Cookie'
      :(_pm==='file'?(_ptype==='token'?'Token':'API Key / Token')
      :(_pm==='loomy'?'session':'Token'));
    const _ph2=_pp.placeholder
      ||(_pm==='cookie'
        ?'粘贴 Cookie（浏览器 F12 → Network → 任选一个请求 → Request Headers 里 cookie: 整行复制）'
        :('粘贴 '+_pkind+'…'));
    if(sess.method!=='oauth_pkce'&&sess.method!=='oauth_google'
       &&sess.method!=='oauth_lobster'){
      const _isCk=(_ptype==='cookie');
      const _boxId=(_isCk?'manualCookie_':'manualSecret_')+esc(sess.platform);
      const _btn=_isCk
        ?('submitManualCookie(\''+esc(sess.platform)+'\')')
        :('submitManualSecret(\''+esc(sess.platform)+'\',\''+esc(_ptype)+'\')');
      let _tip=_isCk
        ?('自动读取被浏览器加密策略挡住时，直接粘贴 Cookie'
          +'（浏览器 F12 → Network → 复制 Request Headers 里的 cookie）')
        :('把登录后拿到的 '+_pkind+'粘贴到这里（浏览器 F12 → Network / Application 里找）');
      if(_isCk&&S.data.browserDiag){
        const dg=S.data.browserDiag;
        _tip=(dg.auto_supported
          ?'本机浏览器可直接读取 Cookie（'+esc(dg.reason)+'）'
          :'⚠ '+esc(dg.reason));
      }
      h+='<div style="margin-top:12px"><div class="faint" style="margin-bottom:5px">'+_tip+'</div>'
        +'<textarea id="'+_boxId+'" rows="3" placeholder="'+esc(_ph2)+'" '
        +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;'
        +'font-family:monospace;font-size:12px"></textarea>'
        +'<div class="row n" style="margin-top:6px;gap:8px">'
        +'<button class="btn pri sm" onclick="'+_btn+'">提交'+esc(_pkind)+'</button>'
        +'<span class="faint" style="font-size:11.5px;align-self:center">'
        +'提交后面板会在线校验，通过即入库</span>'
        +'</div></div>';
    }
    // oauth_pkce（Codex/Claude/Devin）与 oauth_google（Antigravity）都是
    // 「授权 URL → 地址栏跳 localhost 回调 → 复制 code 回来」的同款流程，
    // 共用 manualCode 输入框 + submitPkceCode（后端按 sess.method 分派）。
    if(sess.method==='oauth_pkce'||sess.method==='oauth_google'){
      h+='<div style="margin-top:12px">'
        +'<div class="faint" style="margin-bottom:5px">授权后，浏览器地址栏会跳转到带 code 的链接（localhost 页面打不开是正常的），复制 code 参数粘贴到这里：</div>'
        +'<input id="manualCode" type="text" placeholder="粘贴授权码 code=..." '
        +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;font-family:monospace;font-size:12px"></input>'
        +'<button class="btn pri sm" style="margin-top:6px" onclick="submitPkceCode()">提交授权码</button></div>';
    }
    // Lobster AI（有道）：后端起本地回调服务，正常会自动抓到 code；
    // 回调被浏览器/防火墙挡住时，这里给手动粘贴的入口（2026-10-12 用户反馈「无填写栏」）。
    if(sess.method==='oauth_lobster'){
      const autoTip = sess.callback_port
        ? ('面板已在 <code>127.0.0.1:'+esc(String(sess.callback_port))+'</code> 起本地回调服务，'
           +'授权成功后通常会自动完成；若浏览器跳转后长时间没反应，请把地址栏里的 '
           +'<code>code</code> 参数粘贴到下面。')
        : '授权后浏览器地址栏会跳到 <code>http://127.0.0.1:端口/auth/callback?code=…</code>，'
          +'把其中的 <code>code</code> 参数复制粘贴到下面。';
      h+='<div style="margin-top:12px">'
        +'<div class="faint" style="margin-bottom:5px">'+autoTip+'</div>'
        +'<input id="manualLobsterCode" type="text" placeholder="粘贴回调地址里的 code（如 a83048ca-5128-4c35-9cfd-…）" '
        +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;font-family:monospace;font-size:12px"></input>'
        +'<div class="row n" style="margin-top:6px;gap:8px">'
        +'<button class="btn pri sm" onclick="submitLobsterCode()">提交授权码</button>'
        +'<span class="faint" style="font-size:11.5px;align-self:center">整条回调地址也可以直接粘，面板会自动取 code</span>'
        +'</div></div>';
    }
    h+= compact ? '</div>' : '</div></div>';
  // 内联卡片模式不渲染旧详情页的「探测端点 / 平台内部能力」大块（排版乱的主因）
  if(!compact){
    h+=viewProbe();
    h+=viewPlatformCaps(S.data.platformActions||{});
  }
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
    if(sess.method==='file'){
      const ph=(S.data.loginPlatforms||[]).find(p=>p.id===sess.platform);
      const placeholder=(ph&&ph.placeholder)||'粘贴 API Key 或 refresh_token';
      h+='<div style="margin-top:12px">'
        +'<textarea id="manualSecret" rows="4" placeholder="'+esc(placeholder)+'" '
        +'style="width:100%;padding:8px;border:1px solid var(--line2);border-radius:8px;font-family:monospace;font-size:12px"></textarea>'
        +'<button class="btn pri sm" style="margin-top:6px" onclick="submitManualSecret()">提交凭据</button></div>';
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
    if(p.method==='cookie'){
      h+='<button class="btn sm" style="margin-top:6px;width:100%" onclick="startLogin(\''+esc(p.id)+'\',\''+esc(p.edition||'')+'\',1)">'
        +'🌐 远程在线登录（服务端无头）</button>';
    }
    if(p.qr){
      h+='<button class="btn sm" style="margin-top:6px;width:100%" onclick="startQrScan(\''+esc(p.id)+'\')">'
        +'远程扫码（无需本机浏览器）</button>';
    }
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

function showGatewayInfo(){
  const base=panelBaseUrl();
  const models=(S.data.models||{}).models_enriched||[];
  // ★ 2026-10-13（用户要求）：常用模型从 <pre> 长列表改为下拉选择 ——
  //   选中后点「复制」即拿走该模型 id，比在一长串里手动刮选更方便。
  //   模型 id 来自上游清单，必须 esc()（这段 HTML 走 innerHTML 注入）。
  const ids=['auto'].concat(models.map(m=>m.id).filter(Boolean));
  const opts=ids.map(id=>'<option value="'+esc(id)+'">'+esc(id)+'</option>').join('');

  const html=`
    <div style="padding:20px;max-width:560px">
      <h3 style="margin:0 0 16px 0;font-size:18px">📡 接入配置</h3>

      <div style="margin-bottom:14px">
        <div style="font-size:12px;color:#6b7280;margin-bottom:4px">Base URL</div>
        <div style="display:flex;gap:8px;align-items:center">
          <code style="flex:1;background:#f3f4f6;padding:8px 12px;border-radius:6px;font-size:13px">${base}</code>
          <button class="btn sm" onclick="copyText('${base}')">复制</button>
        </div>
      </div>

      <div style="margin-bottom:14px">
        <div style="font-size:12px;color:#6b7280;margin-bottom:4px">API Key</div>
        <div style="display:flex;gap:8px;align-items:center">
          <code style="flex:1;background:#f3f4f6;padding:8px 12px;border-radius:6px;font-size:13px">admin</code>
          <button class="btn sm" onclick="copyText('admin')">复制</button>
        </div>
      </div>

      <div style="margin-bottom:14px">
        <div style="font-size:12px;color:#6b7280;margin-bottom:4px">常用模型（下拉选择，点复制 · 共 ${ids.length} 个）</div>
        <div style="display:flex;gap:8px;align-items:center">
          <select id="cfgModel" style="flex:1;background:#f3f4f6;padding:8px 12px;border-radius:6px;font-size:13px;border:1px solid var(--line);min-width:0">${opts}</select>
          <button class="btn sm" onclick="copyCfgModel()">复制</button>
        </div>
      </div>

      <div style="display:flex;gap:8px;justify-content:flex-end;margin-top:16px">
        <button class="btn" onclick="closeModal()">关闭</button>
        <button class="btn pri" onclick="copyAllConfig()">一键复制全部</button>
      </div>
    </div>
  `;

  openModal(html);
}

function copyCfgModel(){
  const sel=document.getElementById('cfgModel');
  const v=sel?String(sel.value||''):'';
  if(!v){toast('没有可复制的模型','err');return;}
  copyText(v,()=>toast('已复制模型 '+v,'ok'));
}

function copyAllConfig(){
  const base=panelBaseUrl();
  const models=(S.data.models||{}).models_enriched||[];
  const text=`Base URL: ${base}/v1\nAPI Key: admin\n\n常用模型:\nauto\n${models.slice(0,20).map(m=>'  '+m.id).join('\n')}`;
  copyText(text,()=>toast('已复制全部配置','ok'));
}

function openModal(html){
  closeModal();
  const mask=document.createElement('div');
  mask.id='modalMask';
  mask.style.cssText='position:fixed;inset:0;background:rgba(0,0,0,0.5);z-index:9999;display:flex;align-items:center;justify-content:center';
  mask.onclick=(e)=>{if(e.target===mask)closeModal()};
  const box=document.createElement('div');
  box.style.cssText='background:white;border-radius:12px;box-shadow:0 20px 60px rgba(0,0,0,0.3);max-height:90vh;overflow:auto';
  box.innerHTML=html;
  mask.appendChild(box);
  document.body.appendChild(mask);
}

function closeModal(){
  const m=$('#modalMask');
  if(m)m.remove();
}

const VIEWS={sources:viewSources,route:viewRoute,settings:viewSettings,
  growth:viewGrowth};

const LOADERS={sources:loadSources,route:loadRoute,settings:loadSettings,
  growth:loadGrowth};

function loadExtra(){
  const f=LOADERS[S.v];
  if(f)f();
}

// ============================================================
// 自定义 API 操作函数
// ============================================================

/** 保存新的自定义 API */
async function saveCustomApi(){
  const name = document.getElementById('ca_name').value.trim();
  const protocol = document.getElementById('ca_protocol').value;
  const baseUrl = document.getElementById('ca_baseurl').value.trim();
  const apiKey = document.getElementById('ca_apikey').value.trim();
  const resultEl = document.getElementById('ca_result');
  
  if(!name || !baseUrl || !apiKey){
    resultEl.innerHTML = '<div class="note err">请填写完整的名称、URL 和 Key</div>';
    return;
  }
  
  resultEl.innerHTML = '<div class="note">正在保存并同步模型…</div>';
  
  try {
    // 先添加
    // ★ 2026-10-07：改用 post(path, body)。原写法把 {method,body} 传给了只认
    //   (path,body) 的 api()，后端收到的是嵌套对象 → 落一条"未命名"空记录。
    const addRes = await post('api/custom_apis?action=add',
      { name, protocol, base_url: baseUrl, api_key: apiKey });
    
    if(!addRes.ok){
      resultEl.innerHTML = '<div class="note err">添加失败：'+esc(addRes.message||addRes.error||'')+'</div>';
      return;
    }
    
    const pid = addRes.id;
    
    // 自动同步模型
    const syncRes = await post('api/custom_apis?action=sync', { id: pid });
    
    if(syncRes.ok){
      resultEl.innerHTML = '<div class="note ok">✅ '+esc(syncRes.message)+'</div>';
    } else {
      resultEl.innerHTML = '<div class="note warn">✅ 已保存，但同步模型失败：'+esc(syncRes.error||'')+'</div>';
    }
    
    // 刷新列表
    await loadSources();
    
    // 2 秒后返回列表
    setTimeout(()=>{ closeSrc(); }, 1500);
    
  } catch(e){
    resultEl.innerHTML = '<div class="note err">出错了：'+esc(e.message)+'</div>';
  }
}

/** 同步模型 */
async function syncCustomApi(pid){
  const resultEl = document.getElementById('ca_result');
  resultEl.innerHTML = '<div class="note">正在同步模型…</div>';
  
  try {
    const res = await post('api/custom_apis?action=sync', { id: pid });
    
    if(res.ok){
      resultEl.innerHTML = '<div class="note ok">✅ '+esc(res.message)+'</div>';
    } else {
      resultEl.innerHTML = '<div class="note err">❌ '+esc(res.message||res.error||'')+'</div>';
    }
    
    // 刷新列表
    await loadSources();
    // 重新渲染详情页
    render();
    
  } catch(e){
    resultEl.innerHTML = '<div class="note err">出错了：'+esc(e.message)+'</div>';
  }
}

/** 编辑自定义 API */
function editCustomApi(pid){
  // 简化：暂时只支持删除，编辑后续完善
  toast('编辑功能后续完善', 'warn');
}

/** 删除自定义 API */
async function delCustomApi(pid){
  if(!confirm('确定要删除这个自定义 API 吗？')) return;
  
  try {
    // ★ 2026-10-07：原来不检查返回值就 toast「删除成功」（实际后端因收到嵌套
    //   对象而返回"id 必填"）。
    const r = await post('api/custom_apis?action=delete', { id: pid });
    if(r.ok===false){ toast('删除失败：'+(r.message||r.error||''), 'err'); return; }
    toast('删除成功', 'ok');
    await loadSources();
    closeSrc();
  } catch(e){
    toast('删除失败：'+e.message, 'err');
  }
}

/** 提交 PKCE 授权码 */
async function submitPkceCode(){
  const code = document.getElementById('manualCode').value.trim();
  const sess = S.data.loginSession;
  
  if(!code){
    toast('请输入授权码', 'warn');
    return;
  }
  
  if(!sess || !sess.id){
    toast('没有进行中的登录会话', 'err');
    return;
  }
  
  try {
    // ★ 2026-10-07：这里是 P0 —— 原来把 {method,body} 传给只认 (path,body) 的
    //   api()，后端拿到的是嵌套对象，b.get("id")=None → 走「手动添加」分支，
    //   往账号池写一条 platform/name/secret 全空的垃圾记录，还回 ok:true，
    //   前端于是提示「登录成功」并关闭弹窗 —— 真实授权码被丢弃。
    const res = await post('api/login?action=submit', { id: sess.id, code: code });
    
    if(res.ok){
      toast('登录成功', 'ok');
      await loadSources();
      closeSrc();
    } else {
      toast('提交失败：'+(res.error||''), 'err');
    }
    
  } catch(e){
    toast('提交失败：'+e.message, 'err');
  }
}

/** 提交 Lobster AI（有道）授权码。
 *  输入框里可以只粘 code，也可以粘整条回调地址 —— 面板自动取 code 参数。 */
async function submitLobsterCode(){
  const box=document.getElementById('manualLobsterCode');
  const raw=(box?box.value:'').trim();
  const sess=S.data.loginSession;
  if(!raw){ toast('请先粘贴授权码','warn'); return; }
  if(!sess||!sess.id){ toast('没有进行中的登录会话','err'); return; }
  // 整条 URL → 取 code 参数；只给了 UUID 就用它本身
  let code=raw;
  try{
    if(/^https?:/i.test(raw)){
      const u=new URL(raw);
      code=u.searchParams.get('code')||u.searchParams.get('authCode')||raw;
    }else if(raw.includes('code=')){
      code=raw.split('code=')[1].split('&')[0];
    }
  }catch(e){ /* 不是合法 URL 就按原样当 code */ }
  code=(code||'').trim();
  if(!code){ toast('没能从输入内容里解析出 code','err'); return; }
  try{
    const res=await post('api/login?action=submit',{id:sess.id, code:code});
    if(res.ok){
      toast('登录成功','ok');
      await loadSources();
      closeSrc();
    }else{
      toast('提交失败：'+(res.message||res.error||''),'err');
    }
  }catch(e){
    toast('提交失败：'+e.message,'err');
  }
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
    api('api/sites').then(r=>{S.data.sites=r.sites||[];S.data.catalogSites=r.catalog||[];});
    // 默认着陆（无 hash）也要拉数据；带 hash 且是合法视图则直达
    const target=TITLES[h]?h:'sources';
    const b=$(`.navbtn[data-v="${target}"]`);
    if(b)b.classList.add('on');
    switchView(target);
  });
  // 后台轮询总览
  setInterval(()=>{
    if(document.hidden)return;
    api('api/overview').then(o=>{S.data.overview=o;$('#stl').textContent=
      '面板原生模式 · 账号池 '+(S.data.accounts||[]).length+' 个'+
      ' · 面板已运行 '+dur(o.uptime);}).catch(()=>{});
  },15000);
});
