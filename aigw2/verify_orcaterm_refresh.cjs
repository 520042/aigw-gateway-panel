// 验证 OrcaTerm 抽屉已改为「自动续期」口径（不再说需要手动重取）
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
  // 分组默认折叠：展开全部分组（每次重绘后按索引重取元素）
  const nG = await page.evaluate(() => document.querySelectorAll('.srcgroup-hd').length);
  for (let i = 0; i < nG; i++) {
    const h = (await page.$$('.srcgroup-hd'))[i];
    if (h) { await h.click({ timeout: 3000 }).catch(() => {}); await page.waitForTimeout(500); }
  }
  await page.waitForTimeout(1000);
  // ★ 真实点击卡片上的按钮打开抽屉 —— SRC_EXPAND 是 let 声明的模块级变量，
  //   从 evaluate 里赋值只会在那个作用域建个局部变量，抽屉根本不会开。
  const opened = await page.evaluate(() => {
    const card = document.querySelector('.srccard[data-id="apk-orcaterm"]');
    if (!card) return false;
    const btn = [...card.querySelectorAll('button')].find((b) => /接入|管理/.test(b.textContent));
    if (!btn) return false;
    btn.click();
    return true;
  });
  await page.waitForTimeout(1500);
  const t = await page.evaluate(() => {
    const d = document.getElementById('srcDrawer');
    return d ? d.innerText : '';
  });
  ok('卡片按钮点开了抽屉', opened === true && t.length > 0, '抽屉文本 ' + t.length + ' 字');

  ok('抽屉打开且是 OrcaTerm', t.indexOf('OrcaTerm') >= 0);
  ok('说明了取 JWT 的三步', t.indexOf('data.bin') >= 0 && t.indexOf('oauth_access_token') >= 0);
  ok('明确「粘一次就够，不用定时重取」', t.indexOf('不用定时重取') >= 0 || t.indexOf('粘一次就够') >= 0,
    (t.match(/粘一次[^\n]*/) || [''])[0]);
  ok('说明临期自动续期', t.indexOf('自动') >= 0 && /续期/.test(t));
  ok('说明 401 会强制续期重试', t.indexOf('401') >= 0);
  ok('不再说「回 data.bin 重取一次即可」当作常态',
    !/过期后上游回[^\n]*重取一次即可/.test(t));
  ok('保留「续期被拒才需重粘」的边界说明', t.indexOf('INVALID_REQUEST') >= 0 || t.indexOf('才需要回 data.bin') >= 0);

  ok('无 JS 运行时错误', errs.length === 0, errs.slice(0, 2).join(' ;; '));

  await page.screenshot({ path: 'C:/Users/liang.zhao/Downloads/网关项目/aigw2/_shot_orca.png' });
  await browser.close();
  console.log(OUT.join('\n'));
  const fails = OUT.filter((l) => l.startsWith('FAIL'));
  console.log('\n汇总：' + (OUT.length - fails.length) + '/' + OUT.length + ' 通过');
  process.exit(fails.length ? 1 : 0);
})();
