/*
 * 前端视图渲染测试
 * 用 Node + 最小 DOM stub 直接调用 app.js 里的视图函数，
 * 不用启动真实浏览器就能抓住「渲染崩溃 / 未转义 / 字段漏渲染」。
 * 运行：node test_view.js
 */
const fs = require('fs');
const vm = require('vm');
const path = require('path');

const HTML = String.raw`<!DOCTYPE html>`;  // 占位，保持文件可独立阅读

const mk = () => ({
  addEventListener() {},
  querySelector() { return { textContent: '', innerHTML: '' }; },
  querySelectorAll() { return []; },
  createElement() { return { classList: { add() {}, remove() {} }, remove() {}, style: {} }; },
  body: { appendChild() {} },
  hidden: false,
});
global.document = mk();
global.window = { addEventListener() {} };
global.location = { hash: '' };
global.fetch = async () => ({ json: async () => ({}) });
global.setTimeout = () => 0;
global.setInterval = () => 0;
global.clearInterval = () => {};
global.confirm = () => true;

const here = __dirname;
const app = fs.readFileSync(path.join(here, 'app', 'static', 'app.js'), 'utf8');
const body = fs.readFileSync(path.join(here, 'test_views_body.js'), 'utf8');
vm.runInThisContext(app + '\n' + body, { filename: 'app.js' });
