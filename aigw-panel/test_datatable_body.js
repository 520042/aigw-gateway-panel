/* 数据表格组件（dataTable）测试：搜索 / 排序 / 分页 / 空态 */

let fail2 = 0;
// 表头本身也是一个 <tr>，数数据行要减 1
const bodyRows = (h) => (h.match(/<tr>/g) || []).length - 1;
function ck2(n, c, extra) {
  console.log((c ? 'PASS' : 'FAIL') + '  ' + n + (extra ? '  ' + extra : ''));
  if (!c) fail2++;
}

const ROWS = [];
for (let i = 1; i <= 57; i++) {
  ROWS.push({
    id: 'model-' + String(i).padStart(3, '0'),
    credits: i % 3 === 0 ? '' : 'x' + (i / 10).toFixed(2) + ' credits',
    rate_source: i % 2 ? '内置表/exact' : '线上目录',
    ctx: i * 1000,
    _i: i,
  });
}

const COLS = [
  { k: 'id', t: '模型 ID' },
  { k: 'credits', t: '倍率' },
  { k: 'rate_source', t: '来源' },
  { k: 'ctx', t: '上下文', num: true },
];

// ---------------------------------------------------------------- 基本渲染
let h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('渲染出 .dt 容器', h.includes('class="dt"'));
ck2('渲染出工具条', h.includes('class="dtbar"'));
ck2('渲染出搜索框', h.includes('id="dtq_t1"'));
ck2('渲染出每页条数选择', h.includes('id="dts_t1"'));
ck2('渲染出排序表头', h.includes('th class="sortable"'));
ck2('统计显示总数 57', h.includes('57 条'), h.match(/\d+ 条/)?.[0]);
ck2('默认 25 条 → 只渲染 25 行', bodyRows(h) === 25,
  bodyRows(h) + ' 行');
ck2('有分页条', h.includes('class="dtfoot"'));
ck2('分页显示 1 / 3', h.includes('第 <b>1</b> / 3 页'));

// ---------------------------------------------------------------- 搜索
DT.t1.q = 'model-01';
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('搜索生效（model-01x 共 10 条）', h.includes('10 条'), h.match(/(\d+) 条/)?.[1]);
ck2('搜索结果不含 model-02', !h.includes('>model-02<'));
ck2('搜索框回填关键词', h.includes('value="model-01"'));
DT.t1.q = '不存在的关键词';
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('无匹配时空态', h.includes('没有匹配'));
DT.t1.q = '';
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('清空搜索后恢复', h.includes('57 条'));

// ---------------------------------------------------------------- 排序
DT.t1.sort = 'ctx'; DT.t1.dir = 1;
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('按 ctx 升序 → 首行是最小值', h.includes('>1,000<') || h.includes('>1000<'));
ck2('升序箭头 ▲', h.includes('▲'));
DT.t1.dir = -1;
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('降序箭头 ▼', h.includes('▼'));
DT.t1.sort = ''; DT.t1.dir = -1;

// ---------------------------------------------------------------- 分页
DT.t1.page = 3;
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('第 3 页渲染 7 行', bodyRows(h) === 7,
  bodyRows(h) + ' 行');
ck2('第 3 页末行是 model-057', h.includes('model-057'));
ck2('分页按钮 disabled 正确',
  h.includes('data-dtd="3" disabled') && h.includes('data-dtd="2"'));
DT.t1.page = 99;
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 25 });
ck2('页码越界自动收敛', h.includes('第 <b>3</b> / 3 页'));
DT.t1.page = 1;

// ---------------------------------------------------------------- 每页条数
DT.t1.size = 0;
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 0 });
ck2('size=0 渲染全部 57 行', bodyRows(h) === 57,
  bodyRows(h) + ' 行');
ck2('size=0 时无分页条', !h.includes('class="dtfoot"'));
DT.t1.size = 10;
h = dataTable({ key: 't1', rows: ROWS, cols: COLS, size: 10 });
ck2('size=10 → 6 页', h.includes('第 <b>1</b> / 6 页'));

// ---------------------------------------------------------------- 空数据
h = dataTable({ key: 'tEmpty', rows: [], cols: COLS, size: 25 });
ck2('空数据显示暂无数据', h.includes('暂无数据'));
ck2('空数据统计 0 条', h.includes('0 条'));

// ---------------------------------------------------------------- XSS
h = dataTable({
  key: 'tXss', size: 25,
  rows: [{ id: '<img src=x onerror=alert(1)>', credits: '<script>bad</script>' }],
  cols: COLS,
});
ck2('恶意 ID 被转义', !h.includes('<img src=x') && h.includes('&lt;img'));
ck2('恶意倍率被转义', !h.includes('<script>bad') && h.includes('&lt;script&gt;'));

// ---------------------------------------------------------------- 自定义渲染
h = dataTable({
  key: 'tRender', size: 25,
  rows: [{ id: 'a', n: 5 }],
  cols: [{ k: 'id', t: 'ID' }, { k: 'n', t: 'N', render: r => '<b>' + r.n + '</b>' }],
});
ck2('自定义 render 生效', h.includes('<b>5</b>'));

console.log('');
console.log('失败 ' + fail2 + ' 项');
if (typeof global !== 'undefined' && global.__DT_DONE) global.__DT_DONE(fail2);
