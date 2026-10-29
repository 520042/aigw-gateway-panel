// 验证：本机网关源已移除、anon-zen（真正的 Our Free Model）仍在、账号页正确标孤儿
const { chromium } = require('C:/Users/liang.zhao/AppData/Roaming/adspower_global/cwd_global/lib/internal/adspower-browser/node_modules/playwright-core');

const OUT = [];
function ok(name, pass, detail) { OUT.push((pass ? 'PASS' : 'FAIL') + ' | ' + name + (detail ? ' | ' + detail : '')); }

(async () => {
  const browser = await chromium.launch({
    executablePath: 'C:/Program Files/Google/Chrome/Application/chrome.exe',
    headless: true,
  });
  const page = await browser.newPage({ viewport: { width: 1500, height: 1050 } });
  const errs = [];
  page.on('pageerror', (e) => errs.push(String(e)));
  page.on('console', (m) => { if (m.type() === 'error' && m.text().indexOf('favicon') < 0) errs.push('console: ' + m.text()); });

  await page.goto('http://127.0.0.1:8800/', { waitUntil: 'networkidle' });
  await page.waitForTimeout(1000);
  await page.click('button.navbtn[data-v="sources"]');
  await page.waitForTimeout(2500);
  const n = await page.evaluate(() => document.querySelectorAll('.srcgroup-hd').length);
  for (let i = 0; i < n; i++) {
    const h = (await page.$$('.srcgroup-hd'))[i];
    if (h) { await h.click({ timeout: 3000 }).catch(() => {}); await page.waitForTimeout(500); }
  }
  await page.waitForTimeout(1200);

  // ① 本机网关源已彻底消失，且不再有任何 127.0.0.1 接入源
  const local = await page.evaluate(() => {
    const cards = [...document.querySelectorAll('.srccard')];
    return {
      ofmGone: !document.querySelector('.srccard[data-id="our-free-model"]'),
      totalCards: cards.length,
      // 卡片里如果还残留本机地址，文字里会出现 127.0.0.1
      anyLocal: cards.filter((c) => c.innerText.indexOf('127.0.0.1') >= 0).map((c) => c.getAttribute('data-id')),
    };
  });
  ok('① 本机网关源已移除', local.ofmGone === true);
  ok('① 没有残留 127.0.0.1 接入源', local.anyLocal.length === 0, local.anyLocal.join(',') || '干净');
  ok('① 卡片总数 203', local.totalCards === 203, local.totalCards + ' 张');

  // ② 真正的 Our Free Model（anon-zen）仍在免费组
  const zen = await page.evaluate(() => {
    const c = document.querySelector('.srccard[data-id="anon-zen"]');
    if (!c) return { found: false };
    const sec = c.closest('.srcgroup');
    const hd = sec ? sec.querySelector('.srcgroup-hd') : null;
    const parts = hd ? hd.innerText.split('\n').map((s) => s.trim()).filter(Boolean) : [];
    return { found: true, group: parts[1] || '?', text: c.innerText.replace(/\n/g, ' ') };
  });
  ok('② anon-zen（Our Free Model）仍在', zen.found === true);
  ok('② 它在公益站组', zen.found && zen.group.indexOf('公益站') >= 0, zen.group || '');
  ok('② 名称仍是 Our Free Model', zen.found && zen.text.indexOf('Our Free Model') >= 0,
    (zen.text || '').slice(0, 50));

  // ③ 账号页：孤儿账号被标「平台已移除」并提示可删
  await page.click('button.navbtn[data-v="accounts"]');
  await page.waitForTimeout(2500);
  const acct = await page.evaluate(() => {
    const t = (document.getElementById('view') || {}).innerText || '';
    return {
      len: t.length,
      hasOrphanNotice: t.indexOf('平台已移除') >= 0,
      hasOfmRow: t.indexOf('our-free-model') >= 0,
    };
  });
  ok('③ 账号页正常渲染', acct.len > 800, '长度 ' + acct.len);
  ok('③ 孤儿账号被标「平台已移除」', acct.hasOrphanNotice === true);
  ok('③ 孤儿账号仍可见（未擅自删用户数据）', acct.hasOfmRow === true);

  ok('无 JS 运行时错误', errs.length === 0, errs.slice(0, 2).join(' ;; '));

  await page.screenshot({ path: 'C:/Users/liang.zhao/Downloads/网关项目/aigw2/_shot_removed.png' });
  await browser.close();
  console.log(OUT.join('\n'));
  const fails = OUT.filter((l) => l.startsWith('FAIL'));
  console.log('\n汇总：' + (OUT.length - fails.length) + '/' + OUT.length + ' 通过');
  process.exit(fails.length ? 1 : 0);
})();
