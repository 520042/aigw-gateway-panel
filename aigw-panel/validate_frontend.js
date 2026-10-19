#!/usr/bin/env node
/**
 * 前端自动校验（S6）
 * 每次改完 app/static 下的文件应跑一次：
 *   node validate_frontend.js
 * 目标：在「双击 EXE 才发现界面崩了」之前，把 CSS 注释闭合、JS 语法、
 *      DOM stub 渲染崩溃、重复顶层函数、真实浏览器 DOM 渲染都拦住。
 */
const fs = require('fs');
const path = require('path');
const cp = require('child_process');

const ROOT = __dirname;
const STATIC = path.join(ROOT, 'app', 'static');
const STYLE = path.join(STATIC, 'style.css');
const APP = path.join(STATIC, 'app.js');
const INDEX = path.join(STATIC, 'index.html');
const EDGE = 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';

let fail = 0;
let skip = 0;

function ck(name, cond, extra = '') {
  const ok = !!cond;
  const tag = ok ? 'PASS' : 'FAIL';
  console.log(`${tag}  ${name}${extra ? '  ' + extra : ''}`);
  if (!ok) fail++;
}
function skip_(name, why) {
  console.log(`SKIP  ${name}  (${why})`);
  skip++;
}

function run(cmd, args, opts = {}) {
  try {
    const out = cp.execFileSync(cmd, args, { encoding: 'utf8', stdio: 'pipe', timeout: 30000, ...opts });
    return { status: 0, stdout: out, stderr: '' };
  } catch (e) {
    return { status: e.status || 1, stdout: e.stdout || '', stderr: e.stderr || e.message || '' };
  }
}
function shell(cmd, opts = {}) {
  try {
    const out = cp.execSync(cmd, { encoding: 'utf8', stdio: 'pipe', timeout: 30000, ...opts });
    return { status: 0, stdout: out, stderr: '' };
  } catch (e) {
    return { status: e.status || 1, stdout: e.stdout || '', stderr: e.stderr || e.message || '' };
  }
}
function count(str, re) {
  return (str.match(re) || []).length;
}
function alive() {
  // 注意：Node 在 Windows 上直接调用 curl.exe，必须用 NUL 而非 /dev/null
  const r = run('curl', ['-s', '--noproxy', '127.0.0.1', '-o', 'NUL', '-w', '%{http_code}', 'http://127.0.0.1:8800/']);
  return r.status === 0 && r.stdout.trim() === '200';
}

// ------------------------------------------------------------------ 1. index.html 版本戳
ck('index.html 引用带版本戳的 app.js', fs.existsSync(INDEX) && /app\.js\?v=[0-9a-z]+/.test(fs.readFileSync(INDEX, 'utf8')));

// ------------------------------------------------------------------ 2. JS 语法
const jsCheck = run('node', ['--check', APP]);
ck('app.js 语法检查 (node --check)', jsCheck.status === 0, jsCheck.stderr.trim());

// ------------------------------------------------------------------ 3. CSS 静态检查
const css = fs.readFileSync(STYLE, 'utf8');
ck('style.css /* 与 */ 数量相等',
  count(css, /\/\*/g) === count(css, /\*\//g),
  `${count(css, /\/\*/g)} vs ${count(css, /\*\//g)}`);
ck('style.css { 与 } 数量相等', count(css, /\{/g) === count(css, /\}/g));
ck('style.css ( 与 ) 数量相等', count(css, /\(/g) === count(css, /\)/g));
ck('style.css [ 与 ] 数量相等', count(css, /\[/g) === count(css, /\]/g));

// ------------------------------------------------------------------ 4. 重复顶层函数名（覆盖后一个静默失效）
const appJs = fs.readFileSync(APP, 'utf8');
const fnDecl = Array.from(appJs.matchAll(/^function\s+(\w+)\s*\(/gm)).map(m => m[1]);
const seen = new Set();
const dupes = [];
for (const n of fnDecl) { if (seen.has(n) && !dupes.includes(n)) dupes.push(n); seen.add(n); }
ck('app.js 无重复顶层函数名', dupes.length === 0, dupes.join(', '));

// ------------------------------------------------------------------ 5. DOM stub 渲染冒烟
const tv = run('node', [path.join(ROOT, 'test_views.js')]);
const tvOk = tv.status === 0 && /失败\s*0\s*项/.test(tv.stdout);
ck('test_views.js DOM stub 渲染冒烟', tvOk, tv.status !== 0 ? tv.stderr || tv.stdout : '失败 0 项');

// ------------------------------------------------------------------ 6. headless Edge 真实 DOM 渲染
if (!fs.existsSync(EDGE)) {
  skip_('headless Edge 真实 DOM 渲染', 'Edge 未安装');
} else if (!alive()) {
  skip_('headless Edge 真实 DOM 渲染', '面板未在 127.0.0.1:8800 运行');
} else {
  const tmpDir = path.join(process.env.TEMP || '/tmp', 'aigw-vf-edge-' + Date.now());
  const tmpDom = path.join(process.env.TEMP || '/tmp', 'aigw-vf-dom-' + Date.now() + '.html');
  const url = 'http://127.0.0.1:8800/#credits';
  // cmd.exe 下重定向到文件
  const cmd = `"${EDGE}" --headless --no-sandbox --disable-gpu --user-data-dir="${tmpDir}" --window-size=1400,1000 --virtual-time-budget=12000 --dump-dom "${url}" > "${tmpDom}" 2>NUL`;
  const r = shell(cmd, { timeout: 20000 });
  try {
    const dom = fs.readFileSync(tmpDom, 'utf8');
    const hasCredits = dom.includes('积分到期') || dom.includes('资源包明细');
    ck('headless Edge 渲染积分到期面板', r.status === 0 && hasCredits, `DOM ${dom.length} chars, ${hasCredits ? '含目标文本' : '不含目标文本'}`);
  } catch (e) {
    ck('headless Edge 渲染积分到期面板', false, String(e));
  } finally {
    try { fs.unlinkSync(tmpDom); } catch (_) {}
  }
}

// ------------------------------------------------------------------ 汇总
console.log('');
if (fail === 0) {
  console.log(`✅ 前端校验通过${skip ? `（跳过 ${skip} 项）` : ''}`);
  process.exit(0);
} else {
  console.log(`❌ 前端校验失败：${fail} 项未通过${skip ? `，跳过 ${skip} 项` : ''}`);
  process.exit(1);
}
