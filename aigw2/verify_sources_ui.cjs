// 接入源页 5 项需求的浏览器实测（真实点击，不 mock）
const { chromium } = require('C:/Users/liang.zhao/AppData/Roaming/adspower_global/cwd_global/lib/internal/adspower-browser/node_modules/playwright-core');

const OUT = [];
function ok(name, pass, detail) {
  OUT.push((pass ? 'PASS' : 'FAIL') + ' | ' + name + (detail ? ' | ' + detail : ''));
  return pass;
}

(async () => {
  const browser = await chromium.launch({
    executablePath: 'C:/Program Files/Google/Chrome/Application/chrome.exe',
    headless: true,
  });
  const page = await browser.newPage({ viewport: { width: 1500, height: 1000 } });
  const errs = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  page.on('console', (m) => { if (m.type() === 'error') errs.push('console: ' + m.text()); });

  await page.goto('http://127.0.0.1:8800/', { waitUntil: 'networkidle' });
  await page.waitForTimeout(1200);

  // 底部 BUILD 指纹：确认加载的是新前端
  const foot = await page.evaluate(() => (document.getElementById('foot') || {}).textContent || '');
  ok('前端为新构建 v20261028k', foot.indexOf('20261028k') >= 0, foot.slice(0, 60));

  // 进入接入源页（导航是 button.navbtn[data-v=sources]，不是 <a>）
  await page.click('button.navbtn[data-v="sources"]');
  await page.waitForTimeout(2500);
  // 分组默认折叠：逐个点开分组标题（每次点完 DOM 会重绘，必须按索引重新取元素）
  const nGroups = await page.evaluate(() => document.querySelectorAll('.srcgroup-hd').length);
  for (let i = 0; i < nGroups; i++) {
    const h = (await page.$$('.srcgroup-hd'))[i];
    if (h) { await h.click({ timeout: 3000 }).catch(() => {}); await page.waitForTimeout(600); }
  }
  await page.waitForTimeout(1500);
  let viewTxt = await page.evaluate(() => (document.getElementById('view') || {}).innerText || '');
  const nCards = await page.evaluate(() => document.querySelectorAll('.srccard').length);
  ok('已进入接入源页', viewTxt.indexOf('接入源') >= 0, '文本长度 ' + viewTxt.length + '，卡片 ' + nCards + ' 个');

  // ── 需求1：卡片内显示已登录账号数 ──
  const cardAcct = await page.evaluate(() => {
    const cards = [...document.querySelectorAll('.srccard')];
    const wb = cards.find((c) => (c.getAttribute('data-id') || '') === 'wb-gateway');
    if (!wb) return { found: false };
    return { found: true, text: wb.innerText };
  });
  ok('wb-gateway 卡片存在', cardAcct.found === true);
  const hasAcctLine = cardAcct.found && /账号\s*\d+/.test(cardAcct.text);
  ok('需求1 卡片显示已登录账号数', hasAcctLine, (cardAcct.text || '').split('\n').filter((l) => /账号|积分/.test(l)).join(' / ').slice(0, 120));

  // ── 需求4：卡片显示积分合计 ──
  const hasCredit = cardAcct.found && /积分/.test(cardAcct.text) && /[\d,]{3,}/.test(cardAcct.text);
  ok('需求4 卡片显示积分', hasCredit, (cardAcct.text || '').match(/[\d,.]+\s*积分|积分\s*[\d,.]+/g)?.join(',') || '未匹配');

  // ── 需求2：抽屉展示账号列表 ──
  await page.evaluate(() => { SRC_EXPAND = 'wb-gateway'; render(); });
  await page.waitForTimeout(800);
  const drawer = await page.evaluate(() => {
    const d = document.getElementById('srcDrawer') || document.querySelector('.drawer, .modal-drawer, .modal');
    if (!d) return { found: false };
    return {
      found: true,
      text: d.innerText,
      acctRows: document.querySelectorAll('.acct-row').length,
      sections: [...document.querySelectorAll('.srcsec-h')].map((e) => e.innerText),
    };
  });
  ok('抽屉已打开', drawer.found === true);
  ok('需求2 抽屉列出账号行', (drawer.acctRows || 0) >= 2, '账号行 ' + drawer.acctRows + ' 个');
  ok('抽屉分区标题（已登录账号 / 模型清单）',
    (drawer.sections || []).some((s) => s.indexOf('已登录账号') >= 0) &&
    (drawer.sections || []).some((s) => s.indexOf('模型清单') >= 0),
    JSON.stringify(drawer.sections));

  // ── 需求2b：点击账号弹出当前登录账号信息 ──
  const firstRow = await page.$('.acct-row');
  if (firstRow) { await firstRow.click(); await page.waitForTimeout(700); }
  const opened = await page.evaluate(() => {
    const d = document.getElementById('srcDrawer');
    const t = d ? d.innerText : '';
    return { hasDetail: t.indexOf('账号 ID') >= 0, hasName: t.indexOf('完整名称') >= 0, hasProgress: t.indexOf('当前进度') >= 0 };
  });
  ok('需求2b 点账号展开登录信息', opened.hasDetail && opened.hasName && opened.hasProgress, JSON.stringify(opened));

  // ── 需求3：刷新模型按钮 ──
  const refreshBtn = await page.evaluate(() => {
    const btns = [...document.querySelectorAll('#srcDrawer button')];
    const b = btns.find((x) => x.textContent.indexOf('刷新模型') >= 0);
    return b ? { found: true, text: b.textContent.trim() } : { found: false };
  });
  ok('需求3 抽屉内有「刷新模型」按钮', refreshBtn.found === true, refreshBtn.text || '');

  // 真实点一次（kilo 是 live 源，能真拉到清单）——先切到 kilo
  await page.evaluate(() => { SRC_EXPAND = 'kilo'; render(); });
  await page.waitForTimeout(1200);
  let dialogMsg = '';
  page.once('dialog', async (d) => { dialogMsg = d.message(); await d.accept(); });
  const clicked = await page.evaluate(() => {
    const btns = [...document.querySelectorAll('#srcDrawer button')];
    const b = btns.find((x) => x.textContent.indexOf('刷新模型') >= 0);
    if (!b) return false;
    b.click();
    return true;
  });
  await page.waitForTimeout(6000);
  ok('需求3 点刷新模型有真实结果', clicked && dialogMsg.length > 0, dialogMsg.slice(0, 100));

  // 关闭抽屉
  await page.evaluate(() => { SRC_EXPAND = null; render(); });
  await page.waitForTimeout(500);

  // 账号页回归（确认没被改崩）
  await page.click('button.navbtn[data-v="accounts"]');
  await page.waitForTimeout(2500);
  const acctPage = await page.evaluate(() => (document.getElementById('view') || {}).innerText || '');
  ok('账号页回归正常', acctPage.indexOf('账号凭据') >= 0, '长度 ' + acctPage.length);

  ok('无 JS 运行时错误', errs.length === 0, errs.slice(0, 3).join(' ;; '));

  await page.screenshot({ path: 'C:/Users/liang.zhao/Downloads/网关项目/aigw2/_shot_sources.png', fullPage: false });
  await browser.close();
  console.log(OUT.join('\n'));
  const fails = OUT.filter((l) => l.startsWith('FAIL'));
  console.log('\n汇总：' + (OUT.length - fails.length) + '/' + OUT.length + ' 通过');
  process.exit(fails.length ? 1 : 0);
})();
