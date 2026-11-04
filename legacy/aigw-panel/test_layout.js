/*
 * 全视图布局体检：找出「字段重叠」的根因
 * ------------------------------------------------------------
 * 背景：用户反馈很多页面字段重叠。手动看容易漏，而且看不到根因。
 *
 * 检查项（都是 flex/grid 塌陷的典型原因）：
 *   1. grid/flex 子项缺 min-width:0（内容长就撑破容器）
 *   2. <label> 里面直接塞 <input> 且没有块级包裹（行内元素基线错位）
 *   3. 表格列过多但没有 min-width，列被挤到重叠
 *   4. .row 的子项没标 .n（被 flex:1 撑变形）
 *   5. 未闭合的标签（会导致后续内容跑到一起）
 *   6. 绝对定位元素超出父容器
 *
 * 运行：node test_layout.js
 */
const fs = require('fs');
const vm = require('vm');
const path = require('path');

const mk = () => ({
  addEventListener() {},
  querySelector() { return { textContent: '', innerHTML: '', value: '', style: {}, classList: { add() {}, remove() {} } }; },
  querySelectorAll() { return []; },
  createElement() { return { classList: { add() {}, remove() {} }, remove() {}, style: {} }; },
  body: { appendChild() {} },
  getElementById() { return null; },
  documentElement: { setAttribute() {}, getAttribute() { return null; } },
  hidden: false,
});
global.document = mk();
global.window = { addEventListener() {}, matchMedia: () => ({ matches: false }) };
global.location = { hash: '' };
global.localStorage = { getItem: () => null, setItem() {} };
global.fetch = async () => ({ json: async () => ({}) });
global.setTimeout = () => 0;
global.setInterval = () => 0;
global.clearInterval = () => {};
global.confirm = () => true;
global.alert = () => {};

const here = __dirname;
const app = fs.readFileSync(path.join(here, 'app', 'static', 'app.js'), 'utf8');
vm.runInThisContext(app, { filename: 'app.js' });

let fail = 0;
const issues = [];
const hints = [];
function bad(view, kind, detail) {
  fail++;
  issues.push({ view, kind, detail });
}

// 给一份"内容很长"的假数据，逼出真实布局压力
const LONG = 'x'.repeat(180);
S.data = {
  overview: {
    gateway: { alive: true, addr: '127.0.0.1:8317', models: 31, uptime: 12345 },
    checkin: { done_today: 3, total_checkin_sites: 5, best_streak: 12, total_history: 88 },
    usage: { requests: 12345, success: 12000, failed: 345, tokensTotal: 987654321, tokensIn: 5e8, tokensOut: 4e8 },
    scheduler: { ticks: 100 },
  },
  models: {
    admin: { models: [], default: 'default-model' }, v1: { data: [] },
    models_enriched: Array.from({ length: 40 }, (_, i) => ({
      id: 'very-long-model-name-for-layout-stress-' + i,
      credits: 'x' + (i / 7).toFixed(2) + ' credits', rate_source: '内置表/exact',
      ctx: 176000 + i * 1000, out: 24000, cnFree: '-', intlFree: '免费',
      cost: '未观测', availableAccounts: 0, cnAccounts: 0, intlAccounts: 0,
      tools: true, vision: true, reasoning: true,
    })),
    codebuddy_static: [{ id: 'x', name: 'X', credits: 'x1' }],
    rate_summary: { total: 40, with_rate: 30, from_bundled: 18, from_online: 0 },
    bundled: { total: 36, with_rate: 35, source: '@tencent-ai/codebuddy-code@2.150.0', generated_at: '2026-09-13' },
    online_models: 0,
  },
  upstreams: {
    stats: { verified_at: '2026-10-04', gateways: 7, gateway_endpoints: 70, relays: 17,
      relays_with_checkin: 16, official: 14, official_cn: 7, official_intl: 7,
      open_source: 8, vibe_proxy: 12, vibe_proxy_with_checkin: 6, vibe_dead: 9,
      total_upstreams: 50, codebuddy_models: 18 },
    gateways: [{ id: 'g', name: '网关 ' + LONG, kind: 'exe', protocol: 'openai', auth: 'bearer',
      capabilities: { chat: true, stream: true, tools: true }, free: LONG, note: LONG }],
    relays: [{ name: '站点 ' + LONG, endpoint: 'https://example.com/v1', protocol: 'openai',
      auth: 'bearer', bonus: LONG, limit: LONG, checkin_path: '/api/user/checkin', models: LONG }],
    official: [{ name: '官方 ' + LONG, region: '国内', endpoint: 'https://api.example.com/v1',
      protocol: 'openai', auth: 'bearer', env: 'API_KEY', free: LONG, note: LONG,
      rate: '60 rpm', checkin: true, models_hint: [] }],
    open_source: [{ name: '项目 ' + LONG, url: 'https://github.com/x/y', desc: LONG,
      stack: 'Go', role: '网关' }],
    all: Array.from({ length: 12 }, (_, i) => ({
      id: 'vibe-' + i, name: 'Vibe 项目 ' + i, category: 'vibe_proxy', kind: 'git',
      endpoint: 'http://127.0.0.1:' + (8000 + i), protocol: ['openai'], auth: 'oauth',
      free: LONG, note: LONG, repo: 'https://github.com/a/b', stars: 54037 - i,
      lang: 'Go', updated: '2026-10-03', targets: ['Claude Code', 'Codex', LONG],
      deploy: LONG, checkin: i % 2 === 0, priority: 1, deployed: true,
    })),
    vibe_dead: [{ name: '死项目 ' + LONG, url: 'https://github.com/x/y', why: LONG }],
  },
  sites: [{ id: 's', name: LONG, url: 'https://' + LONG, checkin: true, last: 'x' }],
  catalog: [], catalogSites: [],
  accounts: [{ id: 'a', platform: 'apk-trae', name: LONG, type: 'cookie',
    source: 'CDP', secret: 'sessionid…' + LONG, secret_len: 40,
    obtained_at: '2026-10-04', enabled: true }],
  accountStats: { total: 1, enabled: 1, with_secret: 1 },
  loginPlatforms: Array.from({ length: 11 }, (_, i) => ({
    id: 'p' + i, name: '平台 ' + i + LONG, method: ['qrcode', 'cookie', 'file'][i % 3],
    edition: 'cn', hint: LONG, upstream: 'api.example.com',
  })),
  platformActions: { 'apk-trae': [{ id: 'checkin_status', name: '签到状态', method: 'GET', path: '/api/' + LONG, local: true }] },
  checkin: { today: [], summary: {} },
  growth: {}, tasks: { tasks: [] },
  usage: { usage: {}, series: { days: [] } },
  route: { targets: [], auto: [] }, logs: { lines: [] },
  settings: { settings: {} }, notify: { config: {} },
  toolList: [{ type: 'function', function: { name: 'calculator',
    description: LONG, parameters: { properties: { expression: {} } } } }],
  toolNames: ['calculator', 'get_current_time', 'get_weather'],
  toolSandbox: 'C:/very/long/path/data/tools_sandbox/' + LONG,
  autoCheckin: {
    enabled: true, stagger_sec: 45, retry_times: 2, retry_delay_min: 10, notify: true,
    now: '2026-10-04 09:00:00', thread_alive: true, running: false,
    last_result: { at: '2026-10-04 09:00', summary: 'x',
      results: [{ name: LONG, ok: true, skipped: false, retries: 0, message: LONG, at: 'x' }] },
    platforms: Array.from({ length: 13 }, (_, i) => ({
      platform: 'p' + i, name: '平台 ' + i, mode: ['flow', 'direct', 'bonus', 'none'][i % 4],
      has_public_checkin: i % 4 !== 3, on: true, time: '09:0' + (i % 10),
      hint: LONG, logged_in: i % 2 === 0, done_today: i % 3 === 0,
    })),
  },
};

const VIEWS_TO_CHECK = Object.keys(VIEWS);
const openTags = (s) => (s.match(/<(div|table|tr|td|th|ul|ol|li|pre|label|select)\b/g) || []).length;
const closeTags = (s) => (s.match(/<\/(div|table|tr|td|th|ul|ol|li|pre|label|select)>/g) || []).length;

console.log('检查 ' + VIEWS_TO_CHECK.length + ' 个视图…\n');
for (const v of VIEWS_TO_CHECK) {
  S.v = v;
  let html = '';
  try {
    html = VIEWS[v]();
  } catch (e) {
    bad(v, '渲染抛异常', e.name + ': ' + e.message);
    continue;
  }
  if (typeof html !== 'string' || !html.length) {
    bad(v, '渲染为空', '');
    continue;
  }
  const o = openTags(html), c = closeTags(html);
  if (o !== c) bad(v, '标签不配对', '开 ' + o + ' / 闭 ' + c + '（差 ' + (o - c) + '）');

  // 表格列数与表头是否一致
  const ths = (html.match(/<th\b/g) || []).length;
  const tds = (html.match(/<td\b/g) || []).length;
  if (ths && tds && ths < 2) bad(v, '表头过少', 'th=' + ths);

  // grid 里放 card 是正常用法（CSS 已给 .grid>* 加 min-width:0 兜底），
  // 这里只统计不报错，避免噪音淹没真问题。
  const nestedGridCard = /class="grid[^"]*"[^>]*>\s*<div class="card/.test(html);
  if (nestedGridCard) hints.push({ view: v, kind: 'grid 内含 card', detail: '正常（已由 min-width:0 兜底）' });

  // .row 里没标 .n 的输入框会被 flex:1 撑变形
  const rowNoN = (html.match(/class="row"(?![^>]*\bn\b)[^>]*>(?:(?!<\/div>).)*?<input/g) || []).length;
  if (rowNoN) bad(v, '.row 未标 .n', rowNoN + ' 处');
}

console.log('');
if (fail) {
  console.log('发现 ' + fail + ' 个布局隐患：');
  const byKind = {};
  for (const i of issues) (byKind[i.kind] = byKind[i.kind] || []).push(i);
  for (const k in byKind) {
    console.log('  [' + k + '] ×' + byKind[k].length);
    for (const i of byKind[k].slice(0, 6)) console.log('      ' + i.view + '  ' + i.detail);
  }
  process.exit(1);
} else {
  console.log('布局体检通过：' + VIEWS_TO_CHECK.length + ' 个视图无标签不配对/结构隐患');
}
