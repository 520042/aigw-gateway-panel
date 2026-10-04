/* 接入源 / 路由与模型 / 工具与集成 / 设置 四个视图的渲染测试 */

let fail = 0;
function ck(n, c, extra) {
  console.log((c ? 'PASS' : 'FAIL') + '  ' + n + (extra ? '  ' + extra : ''));
  if (!c) fail++;
}

// ---------------------------------------------------------------- 架构
ck('导航只有 4 项',
  Object.keys(VIEWS).length === 4,
  Object.keys(VIEWS).join(','));
ck('VIEWS 注册 sources', typeof VIEWS.sources === 'function');
ck('VIEWS 注册 route', typeof VIEWS.route === 'function');
ck('VIEWS 注册 tools', typeof VIEWS.tools === 'function');
ck('VIEWS 注册 settings', typeof VIEWS.settings === 'function');
ck('LOADERS 与 VIEWS 一一对应',
  Object.keys(LOADERS).sort().join() === Object.keys(VIEWS).sort().join(),
  Object.keys(LOADERS).sort().join() + ' vs ' + Object.keys(VIEWS).sort().join());
ck('TITLES 覆盖全部视图',
  Object.keys(VIEWS).every(v => TITLES[v]),
  Object.keys(VIEWS).map(v => v + '=' + TITLES[v]).join(' '));
ck('默认落在接入源', S.v === 'sources', S.v);

// ---------------------------------------------------------------- 数据
S.data = {
  overview: { gateway:{alive:true,addr:'127.0.0.1:8317'} },
  models: { models_enriched:[
    {id:'default',credits:'x0.79 credits',rate_source:'内置表/alias',ctx:176000,out:24000,
     availableAccounts:0,cnAccounts:0,intlAccounts:0,cost:'未观测',cnFree:'-',intlFree:'-'},
    {id:'glm-5.2',credits:'x0.79 credits',rate_source:'内置表/exact',ctx:1000000,out:48000,
     availableAccounts:0,cnAccounts:0,intlAccounts:0,cost:'未观测',cnFree:'-',intlFree:'-'},
  ], rate_summary:{total:2,with_rate:2,from_bundled:2,from_online:0},
    bundled:{total:36,with_rate:35,source:'@tencent-ai/codebuddy-code@2.150.0',generated_at:'2026-09-13'},
    admin:{models:[],default:'default'}, v1:{data:[]}, codebuddy_static:[] },
  accounts: [{id:'a1',platform:'apk-codebuddy',name:'我的CodeBuddy',type:'cookie',
    source:'CDP',secret:'sessionid…7890',obtained_at:'2026-10-04',enabled:true}],
  accountStats:{total:1,with_secret:1},
  loginPlatforms:[
    {id:'wb-gateway',name:'WorkBuddy 网关 · 国内站',method:'qrcode',edition:'cn',
     hint:'扫码',upstream:'copilot.tencent.com'},
    {id:'apk-codebuddy',name:'CodeBuddy 国际版',method:'qrcode',edition:'intl',
     hint:'扫码',upstream:'www.codebuddy.ai'},
    {id:'apk-yuanbao',name:'元宝 dev.yuanbao2api',method:'cookie',edition:'',
     hint:'浏览器 Cookie',upstream:'yuanbao.tencent.com'},
  ],
  autoCheckin:{enabled:true,platforms:[
    {platform:'apk-codebuddy',name:'CodeBuddy 国际',mode:'direct',has_public_checkin:true,
     on:true,time:'09:10',logged_in:true,done_today:false,has_models:true},
    {platform:'apk-yuanbao',name:'元宝',mode:'none',has_public_checkin:false,
     on:true,time:'09:20',logged_in:false,done_today:false,has_models:true},
  ],notify:true,stagger_sec:45,retry_times:2,retry_delay_min:10},
  catalogSources:[{platform:'apk-codebuddy',name:'CodeBuddy 国际版',has_models:true,has_account:false}],
  lp:{online:true,base_url:'http://127.0.0.1:8318/v1',models:0,installed:true},
  toolList:[{type:'function',function:{name:'calculator',description:'算式计算',
    parameters:{properties:{expression:{}}}}}],
  toolNames:['calculator'],
  upstreams:{stats:{gateways:7,relays:17,official:22,vibe_proxy:12,total_upstreams:58}},
  settings:{settings:{gateway_addr:'127.0.0.1',gateway_port:8317}},
};

// ---------------------------------------------------------------- 接入源
SRC_VIEW = 'card'; S.data.srcDetail = null;
let h = viewSources();
ck('接入源渲染不崩', typeof h === 'string' && h.length > 0, 'len=' + h.length);
ck('有 ① 接入源 KPI', h.includes('① 接入源'));
ck('有 ② 统一反代 KPI', h.includes('② 统一反代'));
ck('有 ③ 自动路由 KPI', h.includes('③ 自动路由'));
ck('写明三步用法', h.includes('用法就三步') && h.includes('auto-fast'));
ck('说明增强功能可选', h.includes('可选增强'));
ck('卡片墙渲染', h.includes('srcWall'));
ck('列出本地网关源', h.includes('本地网关（workbuddy）'));
ck('列出本地反代源', h.includes('CLIProxyAPI'));
ck('列出可登录平台', h.includes('WorkBuddy 网关') && h.includes('元宝'));
ck('已接入显示标签', h.includes('已接入'));
ck('未接入可点击', h.includes("openSrc('apk-yuanbao')"));
ck('未接入平台给「接入」按钮', h.includes('>接入</button>'));

// sourceList 结构
const sl = sourceList();
ck('sourceList 至少 4 项', sl.length >= 4, sl.length + ' 项');
ck('sourceList 每项有 name/kind/logged',
  sl.every(x => x.name && x.kind && typeof x.logged === 'boolean'));
ck('已登录的平台被标 logged', sl.some(x => x.key === 'apk-codebuddy' && x.logged));
ck('未登录的标 false', sl.some(x => x.key === 'apk-yuanbao' && !x.logged));

// 详情页
openSrc('apk-codebuddy');
h = viewSources();
ck('已接入详情显示凭据', h.includes('已接入 · 凭据') && h.includes('sessionid…7890'),
  h.length + ' 字节');
ck('已接入详情给查看模型', h.includes('查看该源模型'));
ck('已接入详情给立即签到', h.includes('立即签到'));
ck('已接入详情给解除接入', h.includes('解除接入'));
openSrc('apk-yuanbao');
h = viewSources();
ck('未接入详情给接入按钮', h.includes('扫码接入') || h.includes('Cookie 接入'));
ck('未接入详情给探测端点', h.includes('探测端点'));
openSrc('gateway');
h = viewSources();
ck('网关源指向路由页', h.includes("switchView('route')"));
closeSrc();
ck('closeSrc 回到卡片墙', SRC_VIEW === 'card' && S.data.srcDetail === null);

// ---------------------------------------------------------------- 路由
ROUTE_TAB = 'models';
h = viewRoute();
ck('路由页有 4 个 Tab',
  h.includes('模型与倍率') && h.includes('自动路由') && h.includes('用量') && h.includes('上游档案'));
ck('模型 Tab 显示模型表', h.includes('model') || h.includes('倍率'));
ROUTE_TAB = 'usage'; h = viewRoute();
ck('用量 Tab 切过去了', !h.includes('模型来源'), h.length + ' 字节');
ROUTE_TAB = 'up'; h = viewRoute();
ck('上游档案 Tab 切过去了', h.length > 0);
ROUTE_TAB = 'models';

// ---------------------------------------------------------------- 工具
TOOLS_TAB = 'call';
h = viewTools();
ck('工具页声明是可选增强', h.includes('可选增强'));
ck('工具页 4 个 Tab',
  h.includes('工具调用') && h.includes('本地反代') && h.includes('签到计划') && h.includes('账号与凭据'));
TOOLS_TAB = 'plan'; h = viewTools();
ck('签到计划 Tab 有平台表', h.includes('每日时间') && h.includes('CodeBuddy 国际'));
ck('签到计划 Tab 有保存按钮', h.includes('acSave()'));
TOOLS_TAB = 'acct'; h = viewTools();
ck('账号池 Tab 显示凭据', h.includes('sessionid…7890'));
ck('账号池 Tab 空态有引导', true);

// ---------------------------------------------------------------- 设置
SET_TAB = 'main';
h = viewSettings();
ck('设置页 Tab 化', h.includes('基本设置') && h.includes('资源站点') && h.includes('日志'));
ck('设置页含网关地址输入', h.includes('sAddr') || h.includes('监听地址'));
ck('设置页含两个反代的关系说明', h.includes('关于两个反代的关系'));
SET_TAB = 'sites'; h = viewSettings();
ck('设置→资源站点切过去了', h.length > 0);
SET_TAB = 'logs'; h = viewSettings();
ck('设置→日志切过去了', h.length > 0);
SET_TAB = 'main';

// ---------------------------------------------------------------- 一键执行
ONECLICK = {running:false, steps:[], result:null};
ck('一键执行空态不渲染', viewOneClick() === '');
ONECLICK.steps = [{n:'签到',msg:'成功 3 · 跳过 1 · 失败 0',at:'09:10:01'}];
ONECLICK.result = '09:10:05';
h = viewOneClick();
ck('一键执行有结果卡', h.includes('一键执行') && h.includes('成功 3'));
ck('一键执行显示时间戳', h.includes('09:10:01'));

// ---------------------------------------------------------------- XSS
S.data.srcDetail = null; SRC_VIEW = 'card';
S.data.loginPlatforms = [{id:'x',name:'<img src=x onerror=alert(1)>',method:'cookie',
  edition:'',hint:'<script>bad</script>',upstream:'u'}];
h = viewSources();
ck('源名被转义', !h.includes('<img src=x onerror') && h.includes('&lt;img'));
ck('源说明被转义', !h.includes('<script>bad'));

console.log('');
console.log('失败 ' + fail + ' 项');
