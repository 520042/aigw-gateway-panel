// 验证：① our-free-model 归到「开放平台 API」；② OrcaTerm 抽屉说明 JWT 而非 API Key
const { chromium } = require('C:/Users/liang.zhao/AppData/Roaming/adspower_global/cwd_global/lib/internal/adspower-browser/node_modules/playwright-core');

const OUT = [];
function ok(name, pass, detail) {
  OUT.push((pass ? 'PASS' : 'FAIL') + ' | ' + name + (detail ? ' | ' + detail : ''));
}

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

  // 展开全部分组
  const n = await page.evaluate(() => document.querySelectorAll('.srcgroup-hd').length);
  for (let i = 0; i < n; i++) {
    const h = (await page.$$('.srcgroup-hd'))[i];
    if (h) { await h.click({ timeout: 3000 }).catch(() => {}); await page.waitForTimeout(500); }
  }
  await page.waitForTimeout(1200);

  // ── ① 分组归属：按 DOM 结构找 our-free-model 卡片属于哪个 section ──
  const grp = await page.evaluate(() => {
    const card = document.querySelector('.srccard[data-id="our-free-model"]');
    if (!card) return { found: false };
    const sec = card.closest('.srcgroup');
    const hd = sec ? sec.querySelector('.srcgroup-hd') : null;
    // 标题里第 0 段是展开箭头（▾/▸），真正的组名是第 1 段
    const parts = hd ? hd.innerText.split('\n').map((s) => s.trim()).filter(Boolean) : [];
    return { found: true, group: parts[1] || '(?)', headText: parts.join(' | ') };
  });
  ok('our-free-model 卡片存在', grp.found === true);
  ok('① our-free-model 归到「公益站 · 免费车道」', grp.found && grp.group.indexOf('公益站') >= 0, grp.headText || '');

  // 反向确认它不在客户端接入、也不在开放平台
  const notElsewhere = await page.evaluate(() => {
    const wrong = [];
    for (const s of document.querySelectorAll('.srcgroup')) {
      const hd = s.querySelector('.srcgroup-hd');
      const t = hd ? hd.innerText : '';
      if ((t.indexOf('客户端接入') >= 0 || t.indexOf('开放平台') >= 0)
        && s.querySelector('.srccard[data-id="our-free-model"]')) wrong.push(t.split('\n')[1]);
    }
    return wrong;
  });
  ok('① our-free-model 不在客户端接入/开放平台', notElsewhere.length === 0, notElsewhere.join(',') || '干净');

  // ★ 两个 Our Free Model 同名不同源，必须都还在、且都在免费组：
  //   anon-zen（直连 opencode.ai，无凭据）与本机网关（18899 聚合），不能互相顶掉。
  const bothOfm = await page.evaluate(() => {
    const ids = ['anon-zen', 'our-free-model'];
    const res = {};
    for (const id of ids) {
      const c = document.querySelector('.srccard[data-id="' + id + '"]');
      if (!c) { res[id] = null; continue; }
      const sec = c.closest('.srcgroup');
      const hd = sec ? sec.querySelector('.srcgroup-hd') : null;
      const parts = hd ? hd.innerText.split('\n').map((s) => s.trim()).filter(Boolean) : [];
      res[id] = { group: parts[1] || '?', text: c.innerText.replace(/\n/g, ' ').slice(0, 80) };
    }
    return res;
  });
  ok('① anon-zen 仍在免费组', !!bothOfm['anon-zen'] && bothOfm['anon-zen'].group.indexOf('公益站') >= 0,
    bothOfm['anon-zen'] ? bothOfm['anon-zen'].group : '缺失');
  ok('① 两个 Our Free Model 并存（不同上游）',
    !!bothOfm['anon-zen'] && !!bothOfm['our-free-model'],
    'anon-zen + our-free-model');

  // ── ② OrcaTerm 抽屉文案 ──
  await page.evaluate(() => { SRC_EXPAND = 'apk-orcaterm'; render(); });
  await page.waitForTimeout(1000);
  const drawer = await page.evaluate(() => {
    const d = document.getElementById('srcDrawer');
    return d ? d.innerText : '';
  });
  ok('② OrcaTerm 抽屉提到 OAuth Access Token / JWT',
    drawer.indexOf('OAuth Access Token') >= 0 && drawer.indexOf('JWT') >= 0,
    drawer.slice(0, 90).replace(/\n+/g, ' / '));
  ok('② 说明了 data.bin 取法', drawer.indexOf('data.bin') >= 0);
  ok('② 说明了 2 小时过期', drawer.indexOf('2 小时') >= 0 || drawer.indexOf('2小时间') >= 0);
  ok('② 不再误称「填官方 API Key」', drawer.indexOf('填官方 API Key') < 0);
  ok('② 输入框提示是 JWT', await page.evaluate(() => {
    const i = document.querySelector('#srcDrawer input#keyInput');
    return !!i && i.placeholder.indexOf('JWT') >= 0;
  }));

  // 回归：卡片渲染与账号数据仍正常
  await page.evaluate(() => { SRC_EXPAND = null; render(); });
  await page.waitForTimeout(600);
  const reg = await page.evaluate(() => {
    const cards = [...document.querySelectorAll('.srccard')];
    const wb = cards.find((c) => c.getAttribute('data-id') === 'wb-gateway');
    return { nCards: cards.length, wbText: wb ? wb.innerText : '' };
  });
  ok('回归：卡片总数正常', reg.nCards > 150, reg.nCards + ' 张');
  ok('回归：wb 卡片仍显示账号数与积分', /账号\s*\d+/.test(reg.wbText) && /积分/.test(reg.wbText));

  ok('无 JS 运行时错误', errs.length === 0, errs.slice(0, 2).join(' ;; '));

  await page.screenshot({ path: 'C:/Users/liang.zhao/Downloads/网关项目/aigw2/_shot_group.png' });
  await browser.close();
  console.log(OUT.join('\n'));
  const fails = OUT.filter((l) => l.startsWith('FAIL'));
  console.log('\n汇总：' + (OUT.length - fails.length) + '/' + OUT.length + ' 通过');
  process.exit(fails.length ? 1 : 0);
})();
