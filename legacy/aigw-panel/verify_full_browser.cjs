const { chromium } = require('C:/Users/liang.zhao/AppData/Roaming/adspower_global/cwd_global/lib/internal/adspower-browser/node_modules/playwright-core');

const results = [];
function check(name, pass, detail) {
  results.push(`${pass ? 'PASS' : 'FAIL'} | ${name}${detail ? ' | ' + detail : ''}`);
}

async function viewLen(page) {
  return await page.evaluate(() => {
    const el = document.getElementById('view');
    return el ? el.innerHTML.length : -1;
  });
}
async function viewText(page) {
  return await page.evaluate(() => {
    const el = document.getElementById('view');
    return el ? el.innerText : '';
  });
}

(async () => {
  const browser = await chromium.launch({
    executablePath: 'C:/Program Files/Google/Chrome/Application/chrome.exe',
    headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage']
  });
  const page = await browser.newPage();
  const pageErrors = [];
  const netBad = [];
  page.on('pageerror', e => pageErrors.push(e.message));
  page.on('response', r => { if (r.status() >= 400) netBad.push(r.url() + ' -> ' + r.status()); });

  await page.goto('http://127.0.0.1:8800/', { waitUntil: 'networkidle' });
  await page.waitForTimeout(1500);
  check('主界面渲染', (await viewLen(page)) > 5000, 'len=' + (await viewLen(page)));

  for (const v of ['sources', 'route', 'growth', 'settings']) {
    await page.evaluate((v) => {
      const btn = document.querySelector('.navbtn[data-v="' + v + '"]');
      if (btn) btn.click();
    }, v);
    await page.waitForTimeout(1000);
    const len = await viewLen(page);
    check('视图 ' + v, len > 1000, 'len=' + len);
  }

  await page.evaluate(() => document.querySelector('.navbtn[data-v="growth"]').click());
  await page.waitForTimeout(1200);
  const gText = await viewText(page);
  check('任务中心含平台数据', gText.includes('Trae') || gText.includes('WorkBuddy') || gText.includes('签到') || gText.includes('平台'), gText.slice(0,80));

  const checkinBtn = await page.$('button:has-text("签到"), button:has-text("执行"), button:has-text("一键")');
  if (checkinBtn) {
    await checkinBtn.click();
    await page.waitForTimeout(2500);
    check('签到触发点击', true);
  } else {
    check('签到触发点击', false, '未找到签到按钮');
  }

  await page.evaluate(() => document.querySelector('.navbtn[data-v="settings"]').click());
  await page.waitForTimeout(1000);
  const sText = await viewText(page);
  check('设置页含通知/签到配置', sText.includes('通知') || sText.includes('签到') || sText.includes('自动') || sText.includes('代理'), sText.slice(0,80));

  await page.evaluate(() => document.querySelector('.navbtn[data-v="route"]').click());
  await page.waitForTimeout(1200);
  const rText = await viewText(page);
  check('路由页含模型总表', rText.includes('模型') || rText.includes('路由') || rText.includes('倍率'), rText.slice(0,80));

  const freeBtn = await page.$('button:has-text("刷新"), button:has-text("免费")');
  if (freeBtn) {
    await freeBtn.click();
    await page.waitForTimeout(3000);
    check('免费模型榜单刷新点击', true);
  } else {
    check('免费模型榜单刷新点击', false, '未找到刷新按钮（榜单在子标签）');
  }

  await page.evaluate(() => document.querySelector('.navbtn[data-v="sources"]').click());
  await page.waitForTimeout(800);
  const srcCard = await page.$('.src-card, .card');
  if (srcCard) {
    await srcCard.click();
    await page.waitForTimeout(1000);
    check('接入源卡片点击', true);
  } else {
    check('接入源卡片点击', false, '未找到卡片');
  }

  console.log('===== 浏览器功能核查 =====');
  results.forEach(r => console.log(r));
  console.log('===== 页面 JS 错误 =====');
  console.log(pageErrors.length ? pageErrors.join('\n') : 'none');
  console.log('===== 网络 >=400 =====');
  console.log(netBad.length ? netBad.join('\n') : 'none');

  await browser.close();
})();
