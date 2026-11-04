/*
 * 数据表格组件测试入口
 * 运行：node test_datatable.js
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
const body = fs.readFileSync(path.join(here, 'test_datatable_body.js'), 'utf8');
global.__DT_DONE = (fail) => process.exit(fail ? 1 : 0);
vm.runInThisContext(app + '\n' + body, { filename: 'app.js' });
