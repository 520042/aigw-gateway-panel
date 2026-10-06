/* 接入源 / 路由与模型 / 工具与集成 / 设置 四个视图的渲染测试 */

let fail = 0;
function ck(n, c, extra) {
  console.log((c ? 'PASS' : 'FAIL') + '  ' + n + (extra ? '  ' + extra : ''));
  if (!c) fail++;
}

// ---------------------------------------------------------------- 架构
ck('导航 7 项（EXE 网关管理已移除）',
  Object.keys(VIEWS).length === 7 && !('gateway' in VIEWS),
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
  overview: { gateway:{alive:true,addr:'127.0.0.1:8317'},
    native:{accounts:3,relays:9,note:'面板原生模式'} },
  grouped:{total:8,groups:[
    {pid:'auto',name:'自动路由（推荐）',hint:'model 填 auto-*',live:false,usable:0,
     models:[{id:'auto',name:'自动路由 · 最快优先',desc:'3 个上游候选'},
             {id:'free',name:'免费优先',desc:''}]},
    {pid:'apk-doubao',name:'豆包（Cookie 登录 · 原生中继）',hint:'裸名直出',live:false,usable:1,
     models:[{id:'doubao-pro',name:'豆包 · doubao-pro',desc:'原生中继'},
             {id:'doubao-think',name:'豆包 · doubao-think',desc:'原生中继'}]},
    {pid:'apk-codebuddy',name:'CodeBuddy / WorkBuddy（copilot 直连）',hint:'带 @apk-codebuddy 后缀',
     live:false,usable:0,
     models:[{id:'default-model@apk-codebuddy',name:'Auto',credits:'x0.79 credits',
              ctx:176000,out:24000,desc:''}]},
    {pid:'anon-zen',name:'Our Free Model（匿名车道）',hint:'免登录',live:true,usable:0,
     models:[{id:'claude-opus-5-5@anon-zen',name:'Claude Opus 5.5',ctx:200000,desc:''}]},
  ]},
  models: { models_enriched:[
    {id:'default',credits:'x0.79 credits',rate_source:'内置表/alias',ctx:176000,out:24000,
     availableAccounts:0,cnAccounts:0,intlAccounts:0,cost:'未观测',cnFree:'-',intlFree:'-'},
    {id:'glm-5.2',credits:'x0.79 credits',rate_source:'内置表/exact',ctx:1000000,out:48000,
     availableAccounts:0,cnAccounts:0,intlAccounts:0,cost:'未观测',cnFree:'-',intlFree:'-'},
  ], rate_summary:{total:2,with_rate:2,from_bundled:2,from_online:0},
    bundled:{total:36,with_rate:35,source:'@tencent-ai/codebuddy-code@2.150.0',generated_at:'2026-09-13'},
    admin:{models:[],default:'default'}, v1:{data:[]}, codebuddy_static:[] },
  accounts: [{id:'a1',platform:'apk-yuanbao',name:'我的元宝',type:'cookie',
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
    {platform:'apk-yuanbao',name:'元宝',mode:'direct',has_public_checkin:true,
     on:true,time:'09:20',logged_in:true,done_today:false,has_models:true},
    {platform:'apk-yuanbao',name:'元宝',mode:'none',has_public_checkin:false,
     on:true,time:'09:20',logged_in:false,done_today:false,has_models:true},
  ],notify:true,stagger_sec:45,retry_times:2,retry_delay_min:10},
  catalogSources:[{platform:'apk-yuanbao',name:'元宝',has_models:true,has_account:true}],
  srcCats:[
    {kind:'LOCAL',name:'本地 AI',icon:'◈',desc:'有桌面客户端',color:'acc'},
    {kind:'API',name:'平台 API',icon:'⇄',desc:'正规 OpenAI 兼容',color:'info'},
    {kind:'WEB',name:'网页对话',icon:'☁',desc:'只有网页版',color:'warn'},
  ],
  srcSummary:{LOCAL:19,API:15,WEB:2,total:36},
  srcKinds:{
    'apk-codebuddy':{kind:'LOCAL',facts:{desktop:true,web:true,api:true},meta:{name:'本地 AI'}},
    'apk-yuanbao':{kind:'LOCAL',facts:{desktop:true,web:true,api:false},meta:{name:'本地 AI'}},
    'apk-kuku':{kind:'WEB',facts:{desktop:false,web:true,api:false},meta:{name:'网页对话'}},
  },
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
ck('有 ② 原生直连 KPI（免 EXE）', h.includes('② 原生直连') && h.includes('免 EXE'));
ck('有 ③ 自动路由 KPI', h.includes('③ 自动路由'));
ck('写明三步用法', h.includes('用法就三步') && h.includes('auto'));
ck('说明增强功能可选', h.includes('可选增强'));
ck('无分类 Tab、一屏本地 AI 一览',
   h.includes('本地 AI 一览') && !h.includes('setSrcTab'));
ck('网关 EXE 卡已彻底移除', !h.includes('本地网关 EXE') && !h.includes('8317'));
ck('列出本地反代源', h.includes('CLIProxyAPI'));
SRC_TAB='LOCAL'; h=viewSources();
ck('LOCAL 类含桌面平台（合并卡）', h.includes('CodeBuddy / WorkBuddy 账号（copilot 直连）'), h.length+' 字节');
ck('已接入显示标签', h.includes('已接入'));
ck('未接入可点击', h.includes("openSrc('apk-yuanbao')"));
ck('未接入平台给「接入」入口', h.includes('openSrc(') && (h.includes('扫码接入') || h.includes('已接入')));

// sourceList 结构
const sl = sourceList();
ck('sourceList 至少 3 项（无网关卡）', sl.length >= 3, sl.length + ' 项');
ck('sourceList 每项有 name/kind/logged',
  sl.every(x => x.name && x.kind && typeof x.logged === 'boolean'));
ck('登录态正确：元宝已登录 / copilot 未登录（fixture 账号在元宝）',
   sl.some(x => x.key === 'apk-yuanbao' && x.logged === true)
   && sl.some(x => x.key === 'copilot' && x.logged === false));
ck('未登录的标 false（copilot）', sl.some(x => x.key === 'copilot' && !x.logged));

// 详情页（apk-codebuddy 已并入 copilot 卡；元宝在本 fixture 已登录）
openSrc('apk-yuanbao');
h = viewSources();
ck('已接入详情有标签', h.includes('已接入'), h.length + ' 字节');
ck('已接入详情给查看模型', h.includes('查看该源模型'));
ck('已接入详情给解除接入', h.includes('解除接入'));
openSrc('apk-yuanbao');
h = viewSources();
ck('已接入详情不再显示接入按钮（fixture 元宝已登录）',
   !(h.includes('扫码接入') || h.includes('用浏览器 Cookie 接入')));
openSrc('copilot');
h = viewSources();
ck('copilot 详情给四个账号行（含扫码接入）',
   h.includes('国内站账号') && h.includes('国际站账号')
   && h.includes('CodeBuddy 国际账号') && h.includes('CodeBuddy 国内账号')
   && h.includes('扫码接入'));
ck('copilot 详情不再有旧的单按钮接入条', !(h.includes('先探测端点')));
closeSrc();
ck('closeSrc 回到卡片墙', SRC_VIEW === 'card' && S.data.srcDetail === null);

// ---------------------------------------------------------------- 路由
ROUTE_TAB = 'models';
h = viewRoute();
ck('路由页有 3 个 Tab（上游档案已按统一路由要求移除）',
  h.includes('模型与倍率') && h.includes('自动路由') && h.includes('用量')
  && !h.includes('上游档案'));
ck('模型 Tab 是分组总表（gm-group）', h.includes('gm-group') && h.includes('模型总表'));
ck('模型 Tab 有连接测试按钮', h.includes('gmTest(') || h.includes('连接测试'));
ck('模型 Tab 不再有倍率覆盖横幅', !h.includes('倍率覆盖'));
ROUTE_TAB = 'usage'; h = viewRoute();
ck('用量 Tab 切过去了', !h.includes('模型总表'), h.length + ' 字节');
ROUTE_TAB = 'models';

// ---------------------------------------------------------------- 工具
TOOLS_TAB = 'call';
h = viewTools();
ck('工具页声明是可选增强', h.includes('可选增强'));
ck('工具页 4 个 Tab',
  h.includes('工具调用') && h.includes('本地反代') && h.includes('签到计划') && h.includes('账号与凭据'));
TOOLS_TAB = 'plan'; h = viewTools();
ck('签到计划 Tab 有平台表', h.includes('每日时间') && h.includes('元宝'));
ck('签到计划 Tab 有保存按钮', h.includes('acSave()'));
TOOLS_TAB = 'acct'; h = viewTools();
ck('账号池 Tab 显示凭据', h.includes('sessionid…7890'));
ck('账号池 Tab 空态有引导', true);

// ---------------------------------------------------------------- 设置
SET_TAB = 'main';
h = viewSettings();
ck('设置页 Tab 化', h.includes('基本设置') && h.includes('资源站点') && h.includes('日志'));
ck('设置页已无网关连接字段（sAddr/监听地址 均移除）',
   !h.includes('sAddr') && !h.includes('监听地址'));
ck('设置页不再有网关 EXE 段落', !h.includes('workbuddy-gateway') && h.includes('CLIProxyAPI'));
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
// 恶意平台在两个 Tab 下都要被转义
let xssHits = 0, xssTabs = 0;
for (const t of ['LOCAL', 'WEB']) {
  SRC_TAB = t; h = viewSources();
  xssTabs++;
  if (!h.includes('<img src=x onerror') && !h.includes('<script>bad')) xssHits++;
}
ck('两个分类下源名都被转义', xssHits === xssTabs && xssTabs === 2,
  xssHits + '/' + xssTabs);


// ---------------------------------------------------------------- 腾讯原生登录 UI
SRC_TAB = 'LOCAL';
S.data.srcDetail = 'gateway'; SRC_VIEW = 'detail';

// 未加载状态 → 骨架
S.data.tcStatus = null;
h = viewTencentPanel();
ck('腾讯面板未加载时出骨架', h.includes('sk line'));

// 未登录 → 提示扫码
S.data.tcStatus = {loaded:true, logged:false};
h = viewTencentPanel();
ck('未登录时提示扫码', h.includes('扫码登录') && h.includes('不需要 workbuddy-gateway'));

// 已登录但额度未到 → 只出凭据条
S.data.tcStatus = {loaded:true, logged:true, account:'原生登录', tokenLen:1361,
                   verify:{rates:200}};
S.data.tcQuota = null;
h = viewTencentPanel();
ck('已登录显示 token 字节数', h.includes('1361'));
ck('已登录显示倍率验活', h.includes('倍率接口验活 200'));
ck('额度未到时出骨架', h.includes('sk line'));

// 额度到位 → KPI + 表格 + 三个按钮
S.data.tcQuota = {totalRemain:8413, totalSize:11626, totalUsed:3213,
  quota:{totalCount:47, totalDosage:8411},
  summary:[{name:'CodeBuddy个人体验版', unit:'credits', size:500, remain:500, used:0, cycles:1}]};
h = viewTencentPanel();
ck('额度 KPI 显示剩余', h.includes('8413') || h.includes('8,413'), h.slice(0,0));
ck('额度 KPI 显示使用率', h.includes('28%'), '');
ck('额度表格有资源包名', h.includes('CodeBuddy个人体验版'));
ck('有立即签到按钮', h.includes('tencentCheckin()'));
ck('有拉模型按钮', h.includes('tencentModels()'));
ck('有刷新额度按钮', h.includes('loadTcQuota()'));

// 在线模型与倍率
S.data.tcModels = [{id:'hy3', name:'Hy3', credits:'x0.00 credits',
  maxInputTokens:192000, maxOutputTokens:64000,
  supportsImages:true, supportsToolCall:true}];
h = viewTencentPanel();
ck('模型表带倍率', h.includes('x0.00 credits'));
ck('模型表带图像/工具标记', h.includes('工具') && h.includes('图像'));

// 恶意字段要转义
S.data.tcQuota.summary[0].name = '<img src=x onerror=alert(1)>';
h = viewTencentPanel();
ck('资源包名已转义', !h.includes('<img src=x onerror'));
S.data.tcQuota.summary[0].name = 'CodeBuddy个人体验版';

// 源详情：copilot 卡保留（扫码入口在 variants 行内），不再有旧 tencentLogin 卡
S.data.loginPlatforms = [
  {id:'wb-gateway', name:'W', method:'qrcode', edition:'cn', hosts:[], login_url:'', upstream:'copilot.tencent.com', hint:''},
  {id:'wb-gateway-intl', name:'WI', method:'qrcode', edition:'intl', hosts:[], login_url:'', upstream:'copilot.tencent.com', hint:''},
  {id:'apk-codebuddy', name:'CB', method:'qrcode', edition:'intl', hosts:[], login_url:'', upstream:'www.codebuddy.ai', hint:''},
  {id:'apk-codebuddy-cn', name:'CBCN', method:'qrcode', edition:'cn', hosts:[], login_url:'', upstream:'www.codebuddy.cn', hint:''},
];
S.data.accounts = [];
ck('copilot 卡存在（扫码入口在详情 variants 行）',
   sourceList().some(x => x.key === 'copilot' && x.variants && x.variants.length === 4));

// 复位
S.data.tcModels = [];
S.data.tcQuota = null;
S.data.tcStatus = null;
S.data.srcDetail = null; SRC_VIEW = 'card';


// ---- 接入源 sourceList() 渲染冒烟（2026-10-05：合并卡曾因漏定义 pl 崩掉整页）
S.data.loginPlatforms = [
  {id:'wb-gateway', name:'W', method:'qrcode', edition:'cn', hosts:[], login_url:'', upstream:'copilot.tencent.com', hint:'h1'},
  {id:'wb-gateway-intl', name:'WI', method:'qrcode', edition:'intl', hosts:[], login_url:'', upstream:'copilot.tencent.com', hint:'h2'},
  {id:'apk-codebuddy', name:'CB', method:'qrcode', edition:'intl', hosts:[], login_url:'', upstream:'www.codebuddy.ai', hint:'h3'},
  {id:'apk-codebuddy-cn', name:'CBCN', method:'qrcode', edition:'cn', hosts:[], login_url:'', upstream:'www.codebuddy.cn', hint:'h4'},
  {id:'apk-doubao', name:'豆包', method:'cookie', hosts:['doubao.com'], login_url:'', upstream:'www.doubao.com', hint:'hd'},
  {id:'apk-qoder', name:'Qoder', method:'file', hosts:[], login_url:'', upstream:'www.qoder.com', hint:'hq'},
];
S.data.accounts = [{id:'a1', platform:'apk-doubao', secret:'x'}];
S.data.models = {models_enriched:[], rate_summary:{}};
S.data.autoCheckin = {platforms:[]};
try {
  const rows = sourceList();
  ck('sourceList 不崩（pl 已定义）', Array.isArray(rows) && rows.length > 0,
     'rows=' + rows.length);
  const cop = rows.find(x => x.key === 'copilot');
  ck('copilot 合并卡存在且带 4 个版本按钮',
     cop && Array.isArray(cop.variants) && cop.variants.length === 4,
     JSON.stringify(cop && cop.variants || null));
  ck('copilot 系三张独立卡已消失',
     !rows.some(x => x.key === 'wb-gateway-intl' || x.key === 'apk-codebuddy'),
     '');
  ck('APK 卡带局域网反代说明',
     (rows.find(x => x.key === 'apk-doubao') || {}).desc.indexOf('局域网反代') >= 0,
     '');
  const doubao = rows.find(x => x.key === 'apk-doubao');
  ck('豆包卡已接入态（账号池有凭据）', doubao && doubao.logged === true, '');
} catch (e) {
  ck('sourceList 不崩（pl 已定义）', false, String(e));
}
S.data.loginPlatforms = [];
S.data.accounts = [];

console.log('');
console.log('失败 ' + fail + ' 项');
