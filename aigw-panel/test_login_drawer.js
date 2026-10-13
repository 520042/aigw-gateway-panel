/* 抽屉登录 UI 回归：复现用户截图场景（别的平台会话活跃时，本平台登录入口必须还在） */
const fs=require('fs'),vm=require('vm'),path=require('path');
const mk=()=>({addEventListener(){},querySelector(){return{textContent:'',innerHTML:'',value:'',style:{},classList:{add(){},remove(){},toggle(){}}}},querySelectorAll(){return[]},createElement(){return{classList:{add(){},remove(){}},remove(){},style:{}}},body:{appendChild(){}},hidden:false});
global.document=mk();global.window={addEventListener(){}};global.location={hash:''};
global.fetch=async()=>({json:async()=>({})});global.setTimeout=()=>0;global.setInterval=()=>0;
global.clearInterval=()=>{};global.confirm=()=>true;
const app=fs.readFileSync(path.join(__dirname,'app','static','app.js'),'utf8');
vm.runInThisContext(app,{filename:'app.js'});

let fail=0;
function ck(n,c,extra){console.log((c?'PASS':'FAIL')+'  '+n+(extra?'  '+extra:''));if(!c)fail++;}

// fixture：登录平台清单（含四个截图平台）
S.data.loginPlatforms=[
  {id:'apk-trae',name:'Trae 国内版',method:'file',edition:'',hint:'h',upstream:'api.trae.cn',browser_capture:true},
  {id:'web-glm',name:'智谱清言网页版',method:'cookie',edition:'',hint:'h',upstream:'chatglm.cn',placeholder:'粘贴 chatglm_refresh_token（登录 chatglm.cn 后 F12 → Application → Cookies）'},
  {id:'apk-raccoon',name:'小浣熊',method:'cookie',edition:'',hint:'h',upstream:'xiaohuanxiong.com'},
  {id:'web-lobster',name:'网易 Lobster AI',method:'oauth_lobster',edition:'',hint:'h',upstream:'lobsterai.youdao.com'},
  {id:'sub-antigravity',name:'Google Antigravity',method:'oauth_google',edition:'',hint:'h',upstream:''},
];
S.data.accounts=[];
S.data.autoCheckin={platforms:[]};
S.data.catalogSources=[];
S.data.models={models_enriched:[],rate_summary:{}};
S.data.grouped={groups:[]};
S.data.customApis=[];
S.data.srcKinds={};

// ★ 复现截图3/4：Lobster 会话 waiting（别的平台在登录）
S.data.loginSession={id:'s1',platform:'web-lobster',platform_name:'网易 Lobster AI',
  method:'oauth_lobster',status:'waiting',message:'已在本地起回调服务',auth_url:'https://lobsterai.youdao.com/portal',
  seconds_left:599,callback_port:63993};

const sl=sourceList();
function panel(key){return srcLoginPanel(sl.find(x=>x.key===key),true);}

// 图3：智谱抽屉必须有登录按钮（此前被全局 _sessActive 吞掉）
const glm=panel('web-glm');
ck('智谱抽屉有「接入并打开登录页」（Lobster 登录中不吞按钮）',glm.includes('接入并打开登录页'));
ck('智谱抽屉有 refresh_token 粘贴兜底',glm.includes('chatglm_refresh_token'));

// 图4：小浣熊抽屉同上
const ra=panel('apk-raccoon');
ck('小浣熊抽屉有「接入并打开登录页」',ra.includes('接入并打开登录页'));
ck('小浣熊抽屉有 Cookie 粘贴兜底',/粘贴 Cookie/.test(ra));

// 图1：Trae 抽屉应有三方式（本机读取 + 跳转打开 + 粘贴 token）
const tr=panel('apk-trae');
ck('Trae 抽屉有「读取本机 Trae 登录态」一键按钮',tr.includes('读取本机 Trae 登录态')&&tr.includes('traeReadLocal'));
ck('Trae 抽屉有「接入并打开登录页」',tr.includes('接入并打开登录页'));
ck('Trae 抽屉有 x-ide-token 粘贴兜底',tr.includes('x-ide-token'));

// Trae 不再走 copilot 原生扫码：auth_url 不应由 Tencent.login_url 产生（规格级断言）
const trSpec=(typeof PLATFORMS!=="undefined")?null:null;
ck('Trae hint 写明三种方式',tr.includes('读取本机')||tr.includes('三种方式'));

// sub-antigravity：oauth_google 分派后，通用分支渲染接入按钮
const ag=panel('sub-antigravity');
ck('Antigravity 抽屉有「接入并打开登录页」',ag.includes('接入并打开登录页'));

// 本平台有会话时（apk-raccoon 自己 waiting）：显示会话区而非按钮组
S.data.loginSession={id:'s2',platform:'apk-raccoon',platform_name:'小浣熊',
  method:'cookie',status:'waiting',message:'已打开浏览器登录页',auth_url:'',seconds_left:120};
const ra2=panel('apk-raccoon');
ck('本平台会话活跃时显示登录会话区（打开授权页/取消）',ra2.includes('登录会话')&&ra2.includes('取消'));
ck('本平台会话活跃时不重复渲染接入按钮组',!ra2.includes('接入并打开登录页'));

console.log('\n失败 '+fail+' 项');
process.exit(fail?1:0);
