/* 登录流程测试主体：与 app.js 拼接在同一脚本作用域执行 */

global.__ck = null;

async function __run(BASE, T) {
  const { CALLS, TOASTS, INTERVALS, VIEW, done } = T;
  let fail = 0;
  const DBG = process.env.FLOW_DEBUG === '1';
  const dbg = (...a) => { if (DBG) console.log('  ·', ...a); };
  const ck = (n, c, extra) => {
    console.log((c ? 'PASS' : 'FAIL') + '  ' + n + (extra ? '  ' + extra : ''));
    if (!c) fail++;
  };
  const sleep = (ms) => globalThis.__sleep(ms);
  const hit = (frag) => CALLS.some(c => c.url.includes(frag));
  dbg('__run 进入, BASE=', BASE);

  // app.js 用的是相对路径（浏览器里没问题），Node fetch 必须补全 origin
  const _f = global.fetch;
  global.fetch = (u, o) => {
    const url = String(u).startsWith('http') ? u : BASE + u;
    dbg('fetch →', url);
    return _f(url, o);
  };
  // 网络请求加超时，避免整个测试静默卡死
  global.fetch = (u, o) => Promise.race([
    _f(String(u).startsWith('http') ? u : BASE + u, o),
    new Promise((_, rj) => globalThis.__sleep(8000)
      .then(() => rj(new Error('fetch 超时 8s: ' + u)))),
  ]);
  dbg('fetch 包装完成');

  // ------------------------------------------------------------------
  // 0) 递归回归：这是「点开始登录没反应」的真正病因
  //    旧实现 render() → loadExtra() → loader → render() → … 无限循环
  // ------------------------------------------------------------------
  S.v = 'login';
  S.loaded = '';
  CALLS.length = 0;
  dbg('调 viewLogin() 直接渲染…');
  let direct = '';
  try { direct = viewLogin(); } catch (e) { dbg('viewLogin 抛异常', e.message); }
  dbg('viewLogin 返回', direct.length, '字节');
  try { render(); dbg('render() OK, VIEW=', VIEW.innerHTML.length); }
  catch (e) { dbg('render() 抛异常:', e.message); }
  dbg('调 switchView…');
  await switchView('login');
  dbg('switchView 完成');
  const n1 = CALLS.length;
  await sleep(400);
  await sleep(400);
  const n2 = CALLS.length;
  dbg('请求数 n1=', n1, 'n2=', n2);
  ck('切到 login 页请求数有限（无递归）', n2 <= 12 && n2 >= n1,
    '首次 %d → 800ms 后 %d' % (n1, n2));
  ck('login 只拉 4 个接口一次',
    CALLS.filter(c => c.url.includes('/api/login')).length <= 2,
    CALLS.filter(c => c.url.includes('/api/login')).length + ' 次 /api/login');

  // 反复 render 不应触发任何请求
  CALLS.length = 0;
  for (let i = 0; i < 20; i++) render();
  await sleep(200);
  ck('连点 20 次 render 不发请求', CALLS.length === 0, CALLS.length + ' 个');

  // 重复切同一视图不重复拉
  CALLS.length = 0;
  await switchView('login');
  await sleep(200);
  ck('重复切同一视图不重复加载', CALLS.length === 0, CALLS.length + ' 个');
  // force=true 才重拉
  CALLS.length = 0;
  await switchView('login', true);
  ck('force=true 强制重拉', CALLS.length > 0, CALLS.length + ' 个');

  // ---------------------------------------------------- 1. 平台列表
  await loadLogin();
  const pls = S.data.loginPlatforms || [];
  ck('拉到平台列表', pls.length > 0, pls.length + ' 个');
  ck('平台含 name/method',
    pls.every(p => p.id && p.name && p.method),
    pls.map(p => p.method).join(','));

  // ---------------------------------------------------- 2. 渲染出按钮
  let html = '';
  let renderErr = null;
  try { render(); html = VIEW.innerHTML || ''; }
  catch (e) { renderErr = e; }  ck('render() 不抛异常', !renderErr,
    renderErr ? (renderErr.name + ': ' + renderErr.message).slice(0, 140) : '');
  if (renderErr) {
    console.log('--- 异常堆栈 ---');
    console.log((renderErr.stack || '').split('\n').slice(0, 6).join('\n'));
  }
  ck('渲染出平台卡片', html.includes('可登录的平台'),
    'len=' + html.length + ' 含平台名=' + html.includes('WorkBuddy'));
  const onclick = (html.match(/onclick="startLogin\('([^']+)','([^']*)'\)"/g) || []);
  ck('按钮 onclick 已挂上', onclick.length === pls.length,
    onclick.length + ' / ' + pls.length);
  ck('onclick 指向真实平台 id',
    pls.every(p => html.includes("startLogin('" + p.id + "'")),
    pls.map(p => p.id).join(','));
  if (html.length && !html.includes('可登录的平台')) {
    console.log('--- 实际渲染前 400 字节 ---');
    console.log(html.slice(0, 400));
  }

  // ---------------------------------------------------- 3. 点二维码平台
  CALLS.length = 0;
  TOASTS.length = 0;
  const qr = pls.find(p => p.method === 'qrcode');
  ck('存在二维码平台', !!qr, qr && qr.id);
  if (qr) {
    await startLogin(qr.id, qr.edition || '');
    ck('点了按钮发出了请求', hit('/api/login'),
      CALLS.map(c => c.method + ' ' + c.url).join(' | ').slice(0, 110));
    ck('请求是 POST start',
      CALLS.some(c => c.url.includes('action=start') && c.method === 'POST'));
    const sent = CALLS.find(c => c.url.includes('action=start'));
    ck('请求体带 platform', sent && sent.body && sent.body.includes(qr.id),
      sent && sent.body);
    ck('toast 提示已发出', TOASTS.length > 0, TOASTS.join(' / ').slice(0, 60));

    const sess = S.data.loginSession;
    ck('会话已写进 state', !!(sess && sess.id), sess && sess.id);
    ck('会话平台正确', sess && sess.platform === qr.id, sess && sess.platform);
    if (qr.method === 'qrcode') {
      ck('二维码平台有二维码图', !!(sess && sess.qr && sess.qr.startsWith('data:image')),
        sess && String(sess.qr).slice(0, 30));
      ck('有授权链接', !!(sess && sess.auth_url),
        sess && String(sess.auth_url).slice(0, 60));
      ck('轮询已启动', INTERVALS.length > 0, INTERVALS.length + ' 个定时器');
    }
    // 渲染后 HTML 里应能看到会话卡
    const h2 = VIEW.innerHTML || "";
    ck('会话卡渲染到页面',
      h2.includes('lsStatus') || h2.includes('取消'),
      h2.length + ' 字节');
  }

  // ---------------------------------------------------- 4. 点 Cookie 平台
  CALLS.length = 0;
  const ckPlat = pls.find(p => p.method === 'cookie');
  ck('存在 Cookie 平台', !!ckPlat, ckPlat && ckPlat.id);
  if (ckPlat) {
    await startLogin(ckPlat.id, '');
    const sess = S.data.loginSession;
    ck('Cookie 平台也建了会话', !!(sess && sess.id), sess && sess.id);
    ck('Cookie 平台 method 正确', sess && sess.method === 'cookie',
      sess && sess.method);
    const h3 = VIEW.innerHTML || '';
    ck('渲染出粘贴 Cookie 框', h3.includes('manualCookie'));
    ck('渲染出「我已登录」按钮', h3.includes('pollCookieOnce'));
  }

  // ---------------------------------------------------- 5. 取消
  CALLS.length = 0;
  if (S.data.loginSession) {
    await cancelLogin();
    ck('取消发出请求', hit('action=cancel'), CALLS.length + ' 次');
    ck('会话已清空', !S.data.loginSession);
  }

  // ---------------------------------------------------- 6. 错误路径
  CALLS.length = 0;
  const okBad = await startLogin('no-such-platform', '');
  ck('未知平台不崩', true);
  ck('未知平台无会话', !S.data.loginSession);

  console.log('');
  console.log('失败 ' + fail + ' 项');
  done(fail);
}

global.__run = __run;
