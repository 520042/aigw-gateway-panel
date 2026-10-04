/*
 * 「点开始登录」端到端流程测试
 * ------------------------------------------------------------------
 * 起因：用户反馈前台「账号登录」页点按钮没反应。
 * 后端 /api/login?action=start 实测 200 正常，所以要验证的是前端链路：
 *   按钮 onclick 是否挂上 → startLogin 是否执行 → fetch 是否发出
 *   → 响应是否写进 S.data → render 是否把会话画出来 → 轮询是否启动
 *
 * 这里不 mock fetch，直接打真实面板（devboot 起的那份），
 * DOM 用最小 stub，只提供 app.js 真正用到的那几个入口。
 *
 * 运行：node test_login_flow.js [面板地址]
 */
const fs = require('fs');
const vm = require('vm');
const path = require('path');

const BASE = process.argv[2] || 'http://127.0.0.1:8830';

// ---------------------------------------------------------------- DOM stub
const TOASTS = [];
const INTERVALS = [];
function mkEl(id) {
  return {
    id: id || '',
    innerHTML: '',
    textContent: '',
    value: '',
    className: '',
    style: {},
    classList: {
      _s: new Set(),
      add(...c) { c.forEach((x) => this._s.add(x)); },
      remove(...c) { c.forEach((x) => this._s.delete(x)); },
      // classList.toggle(name, force) 是标准 API —— app.js 用它控制页头按钮显隐
      toggle(c, force) {
        const on = force === undefined ? !this._s.has(c) : !!force;
        if (on) this._s.add(c); else this._s.delete(c);
        return on;
      },
      contains(c) { return this._s.has(c); },
    },
    appendChild() {},
    remove() {},
    setAttribute() {},
    getAttribute() { return null; },
    addEventListener() {},
    closest() { return null; },
    querySelector() { return null; },
    querySelectorAll() { return []; },
  };
}
const VIEW = mkEl('view');
const store = {};                       // id → element
const document = {
  getElementById(id) { return store[id] || (store[id] = mkEl(id)); },
  querySelector(sel) {
    if (sel === '#view') return VIEW;
    const m = /^#(.+)$/.exec(sel);
    if (m) return store[m[1]] || (store[m[1]] = mkEl(m[1]));
    return mkEl(sel);
  },
  querySelectorAll() { return []; },
  createElement() { return mkEl(); },
  body: { appendChild(t) { TOASTS.push(t.textContent || t.innerHTML || ''); } },
  addEventListener() {},
  documentElement: mkEl(),
};
global.document = document;
global.window = { addEventListener() {}, location: { hash: '' } };
global.location = { hash: '' };
global.localStorage = { getItem: () => null, setItem() {} };
global.confirm = () => true;
global.alert = () => {};
// 注意：app.js 里的定时器要 stub 掉，但它自己也会吞掉测试用的 sleep，
// 所以先存一份原生 setTimeout 给测试主体用。
const REAL_SET_TIMEOUT = globalThis.setTimeout;
const REAL_SET_INTERVAL = globalThis.setInterval;
global.setTimeout = (fn, ms) => 0;
global.setInterval = (fn, ms) => { INTERVALS.push({ fn, ms }); return INTERVALS.length; };
global.clearInterval = () => {};
globalThis.__sleep = (ms) => new Promise(r => REAL_SET_TIMEOUT(r, ms));
global.Event = function () {};

// 记录真实 fetch（先存原生，再包一层）
const CALLS = [];
const REAL_FETCH = globalThis.fetch;
global.fetch = async (url, opt) => {
  CALLS.push({ url: String(url), method: (opt && opt.method) || 'GET',
               body: opt && opt.body });
  return REAL_FETCH(url, opt);
};

// ---------------------------------------------------------------- 载入 app.js
const here = __dirname;
const app = fs.readFileSync(path.join(here, 'app', 'static', 'app.js'), 'utf8');
const body = fs.readFileSync(path.join(here, 'test_login_flow_body.js'), 'utf8');
vm.runInThisContext(app + '\n' + body, { filename: 'app.js' });

// ---------------------------------------------------------------- 跑
(async () => {
  if (typeof global.__run !== 'function') {
    console.error('FATAL: __run 未定义，测试体没加载');
    process.exit(3);
  }
  await global.__run(BASE, { CALLS, TOASTS, INTERVALS, VIEW,
    done: (fail) => process.exit(fail ? 1 : 0) });
})().catch(e => {
  console.error('测试执行异常：', e && e.stack ? e.stack.split('\n').slice(0, 6).join('\n') : e);
  process.exit(2);
});
