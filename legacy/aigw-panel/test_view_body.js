/* 测试主体：与 app.js 拼接后在同一脚本作用域执行（const 是脚本级绑定） */

let fail = 0;
function ck(n, c, extra) {
  console.log((c ? 'PASS' : 'FAIL') + '  ' + n + (extra ? '  ' + extra : ''));
  if (!c) fail++;
}

// ---------------------------------------------------------------- 视图注册
ck('VIEWS 注册 login', typeof VIEWS.login === 'function');
ck('LOADERS 注册 login', typeof LOADERS.login === 'function');
ck('TITLES 有 login', TITLES.login === '账号登录', TITLES.login);

// ---------------------------------------------------------------- 空数据
S.data = {};
let h = viewLogin();
ck('空数据渲染不崩', typeof h === 'string' && h.length > 0, 'len=' + h.length);
ck('空数据含平台区', h.includes('可登录的平台'));

// ---------------------------------------------------------------- 完整数据
S.data.loginPlatforms = [
  { id: 'wb-gateway', name: 'WorkBuddy 网关 · 国内站', method: 'qrcode', edition: 'cn', hint: '扫码', upstream: 'copilot.tencent.com' },
  { id: 'apk-doubao', name: '豆包', method: 'cookie', edition: '', hint: '浏览器登录', upstream: 'www.doubao.com' },
  { id: 'apk-go', name: 'Go 网关', method: 'file', edition: '', hint: '填 Key', upstream: 'copilot.tencent.com' }
];
S.data.accounts = [{ id: 'a1', platform: 'apk-doubao', name: '张三', type: 'cookie', source: 'CDP',
  secret: 'sessionid…7890', secret_len: 26, obtained_at: '2026-10-04 00:00:00', enabled: true }];
S.data.accountStats = { total: 1, enabled: 1, with_secret: 1 };
S.data.platformActions = {
  'apk-trae': [{ id: 'checkin_status', name: '签到状态', method: 'GET', path: '/x' },
               { id: 'redeem', name: '兑换码兑换', method: 'POST', path: '/y', need: ['code'] }],
  'apk-raccoon': [{ id: 'login_bonus', name: '登录送积分', method: 'POST', path: '/z' }]
};
h = viewLogin();
ck('平台卡片渲染', h.includes('WorkBuddy 网关') && h.includes('豆包') && h.includes('Go 网关'));
ck('方式标签渲染', h.includes('二维码') && h.includes('Cookie') && h.includes('文件/Key'));
ck('账号池表格渲染', h.includes('张三') && h.includes('CDP'));
ck('掩码密钥显示', h.includes('sessionid…7890'));
ck('平台能力渲染', h.includes('签到状态') && h.includes('登录送积分'));
ck('兑换码有输入框', h.includes('id="pa_code"'));
ck('一键签到按钮', h.includes('platformCheckin'));

// ---------------------------------------------------------------- 二维码会话
S.data.loginSession = { id: 's1', platform: 'wb-gateway', platform_name: 'WorkBuddy 网关 · 国内站',
  method: 'qrcode', status: 'waiting', message: '等待授权', qr: 'data:image/png;base64,AAAA',
  auth_url: 'https://copilot.tencent.com/login?state=x', seconds_left: 299 };
h = viewLogin();
ck('二维码渲染', h.includes('src="data:image/png;base64,AAAA"'));
ck('授权链接按钮', h.includes('打开授权页') && h.includes('copilot.tencent.com'));
ck('剩余秒数', h.includes('剩余 299 秒'));

// ---------------------------------------------------------------- Cookie 会话
S.data.loginSession = { id: 's2', platform: 'apk-doubao', platform_name: '豆包',
  method: 'cookie', status: 'waiting', message: '请在浏览器登录', qr: '', auth_url: '', seconds_left: 0 };
h = viewLogin();
ck('Cookie 手动输入框', h.includes('id="manualCookie"'));
ck('我已登录按钮', h.includes('pollCookieOnce'));
ck('无二维码时不渲染 img', !h.includes('src="data:image'));

// ---------------------------------------------------------------- XSS 转义
S.data.loginSession = null;
S.data.loginPlatforms = [{ id: 'x', name: '<img src=x onerror=alert(1)>', method: 'cookie',
  edition: '', hint: '<script>bad</script>', upstream: 'u' }];
h = viewLogin();
ck('平台名被转义', !h.includes('<img src=x onerror') && h.includes('&lt;img'));
ck('提示被转义', !h.includes('<script>bad'));

console.log('');
console.log('失败 ' + fail);
if (fail) { process.exitCode = 1; }
