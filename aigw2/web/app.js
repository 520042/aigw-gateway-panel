// aigw v2 控制台 · hash 路由 SPA（M5 管理面）
// 2026-10-19：接入源页（73 平台分组卡片 + 跳转登录 + 手动粘贴凭据闭环 + 自定义 API + 公益站）
// 自诊断：BUILD 指纹 + 全局错误上屏（页脚），用于区分浏览器缓存/环境问题
window.addEventListener('error', (e) => {
  const f = document.getElementById('foot');
  if (f) { f.textContent = '⚠ JS错误: ' + String(e.message || 'unknown').slice(0, 60); f.style.color = '#f06a6a'; }
});
const BUILD = 'v20261026c';

// 公益站聚合源状态（/api/free 的 sources + last_refresh）：只在需要时拉一次，
// 手动刷新后置空，下次渲染重取。
let FREE_META = null;
async function freeMeta() {
  if (FREE_META) return FREE_META;
  try { FREE_META = await api('/api/free'); } catch (_) { FREE_META = null; }
  return FREE_META;
}
// 「聚合源 3 个：a · b；上次刷新 2 分钟前（…聚合源 b 命中）」——
// 备用源生效、以及全部源失败，都要在面板上看得见，而不是只在刷新时的 alert 里闪一次。
function freeSourceLine(fm) {
  const host = (u) => { try { return new URL(u).host; } catch (_) { return String(u || '').slice(0, 60); } };
  const srcs = (fm.sources || []).map(host);
  const lr = fm.last_refresh || {};
  const okSec = typeof lr.last_ok_s_ago === 'number' ? lr.last_ok_s_ago : null;
  const errSec = typeof lr.last_err_s_ago === 'number' ? lr.last_err_s_ago : null;
  let s = '聚合源 ' + srcs.length + ' 个：' + (esc(srcs.join(' · ')) || '（无）');
  // 失败比成功新才算「当前是坏的」：成功过一次之后又失败，只显示成功会掩盖问题。
  if (errSec !== null && (okSec === null || errSec < okSec)) {
    s += '；<span class="warn">上次刷新失败 ' + esc(agoText(errSec)) + '：' + esc(lr.last_err) + '</span>';
    if (okSec !== null) s += '（上次成功 ' + esc(agoText(okSec)) + '）';
  } else if (okSec !== null) {
    s += '；上次刷新 ' + esc(agoText(okSec)) + '（' + esc(lr.last_ok) + '）';
  } else {
    s += '；尚未刷新';
  }
  // 与内置 providers/free-*.toml 同域的动态条目不再重复展示（同一个站两张卡片），
  // 这里说清差额，否则用户会以为「聚合页 47 条，面板怎么只有 46 个」。
  // 刷新台账（last_ok）里本来就带这句，别重复说两遍；只有台账没提（例如还没刷过）时才补。
  if (fm.shadowed > 0 && !/与内置接入源同域/.test(lr.last_ok || '')) {
    s += '；另有 <b>' + fm.shadowed + '</b> 个与内置接入源同域，已并入内置条目';
  }
  return s;
}

// 刷新公益站聚合（拉聚合导航 → 合并；已接入的保留）
async function refreshFree() {
  const r = await post('/api/free', { action: 'refresh' }).catch((e) => ({ ok: false, message: e.message }));
  // 并发点刷新时后端回 409 busy：这不是失败（没有真的抓取失败），只是「上一轮还没跑完」，
  // 用「刷新失败：」开头会让人以为聚合源挂了，所以单独一条文案。
  if (r.busy) { alert(r.message || '上一次刷新还在进行中，稍等再看结果。'); return; }
  alert(r.ok ? r.message : ('刷新失败：' + (r.message || '网络错误')));
  if (r.ok) { FREE_META = null; SRC_OPEN['free'] = true; srcRerender(); }
}

const $ = (s) => document.querySelector(s);
// ── 弹窗骨架 ──────────────────────────────────────────────
// 全站 14 处 modal 都是同一套外壳（遮罩 + 标题栏 + ✕ + 内容区），标题文案与内容不同而已。
// 抽成两个小函数，避免每加一个弹窗就重抄一遍关闭逻辑（过去就因为某处漏写 or 写错
// "this.closest('.modal-mask').remove()" 导致点了 ✕ 关不掉）。
//
// 点击遮罩关闭：判定 event.target === this，只有点在**遮罩本身**（不是冒泡上来的内容区）
// 才关 —— 否则在输入框里拖选文本松手都会被当成「点外面」。
function modalShell(title, bodyHtml, opts) {
  const o = opts || {};
  const cls = o.drawer ? 'modal drawer' : 'modal';
  const id = o.id ? ' id="' + o.id + '"' : '';
  const onMask = o.onMask || '';
  const close = o.close || 'this.closest(\'.modal-mask\').remove()';
  const x = o.closable === false ? '' : '<b class="x" onclick="' + close + '">✕</b>';
  return '<div class="modal-mask" onclick="if(event.target===this){' + onMask + '}">'
    + '<div class="' + cls + '"' + id + '>'
    + '<div class="modal-hd">' + title + x + '</div>'
    + '<div class="modal-bd">' + bodyHtml + '</div>'
    + '</div></div>';
}

const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) =>
  ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
// ★ 事件属性（onclick="f('…')"）里的插值必须先 JS 转义再 HTML 转义：
// HTML 解析器会先把 &#39; 解码回单引号、再把整段交给 JS 引擎 —— 只用 esc() 时
// 一个来自上游 /v1/models 的模型名 bad');alert(1);// 就能逃逸出 JS 字符串执行
// （已实测：旧版点击「停用」按钮 window.__xss 被置 1，且按钮功能被注释掉）。
// 所以凡是把字符串插进 handler 的 JS 字面量，一律用 att() 而不是 esc()。
const jsq = (s) => String(s ?? '').replace(/[\\'"\r\n]/g, (c) =>
  ({'\\':'\\\\',"'":"\\'",'"':'\\"','\r':'\\r','\n':'\\n'}[c]));
const att = (s) => esc(jsq(s));
// 进 <a href> / window.open 的 URL 一律先过 scheme 白名单：esc() 挡不住
// javascript://evil.com/%0Aalert(1) —— host 看着合规、实体转义也无懈可击，
// 但浏览器点击时先百分号解码再执行（服务端入库口同样有 scheme_ok，这里是纵深）。
const safeUrl = (u) => { const t = String(u ?? '').trim(); return /^https?:\/\//i.test(t) ? t : ''; };

// 面板令牌：后端配置了 AIGW_PANEL_TOKEN/--token 时，所有 /api/* 都要带
// X-AIGW-Token。首次从 URL ?token= 或 localStorage 取，成功后写回 localStorage。
const TOKEN_KEY = 'aigw_token';
let PANEL_TOKEN = (() => {
  try {
    const u = new URL(location.href);
    const q = u.searchParams.get('token');
    if (q) { localStorage.setItem(TOKEN_KEY, q); u.searchParams.delete('token');
      history.replaceState(null, '', u.pathname + (u.search ? u.search : '') + u.hash); return q; }
    return localStorage.getItem(TOKEN_KEY) || '';
  } catch (_) { return ''; }
})();
function withToken(init) {
  const o = init || {};
  const h = Object.assign({}, o.headers || {});
  if (PANEL_TOKEN) h['X-AIGW-Token'] = PANEL_TOKEN;
  return Object.assign({}, o, { headers: h });
}
// 401 时提示一次，避免每帧弹窗
let TOKEN_WARNED = false;
function tokenGuard(r) {
  if (r.status === 401 && !TOKEN_WARNED) {
    TOKEN_WARNED = true;
    const f = document.getElementById('foot');
    if (f) { f.textContent = '⚠ 面板已启用访问令牌：请在地址后加 ?token=你的令牌 重新打开（或设置 localStorage.aigw_token）'; f.style.color = '#fbbf24'; }
  }
  return r;
}
async function api(path) {
  const r = tokenGuard(await fetch(path, withToken()));
  if (!r.ok) throw new Error(`${path} → HTTP ${r.status}`);
  return r.json();
}
async function post(path, body) {
  const r = tokenGuard(await fetch(path, withToken({
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body || {}),
  })));
  try { return await r.json(); } catch (_) { return { ok: false, message: `HTTP ${r.status}` }; }
}
const pill = (cls, txt) => `<span class="pill ${cls}">${esc(txt)}</span>`;
const nfmt = (n) => Number(n || 0).toLocaleString('en-US');

// ══════════════════════════════════════════════════════════════
// 视图注册
// ══════════════════════════════════════════════════════════════
const VIEWS = {
  overview: { t: '总览', s: '免费 AI 额度 → 本地 OpenAI 兼容端点', f: viewOverview },
  sources:  { t: '接入源', s: '官方 API · 订阅反代 · 客户端 · 公益站 · 自定义', f: viewSources },
  accounts: { t: '账号', s: '凭据掩码展示，secret 仅后台使用', f: viewAccounts },
  usage:    { t: '用量', s: 'SQLite(WAL) 用量台账', f: viewUsage },
  // ★ 2026-10-26（P3）：运行时配置热更新（并发/超时/重试/限流），改完立即生效。
  runtime:  { t: '运行配置', s: '并发 · 超时 · 重试 · 限流（热更新，无需重启）', f: viewRuntime },
  about:    { t: '关于', s: '版本 · 在线更新 · 备份恢复 · 使用说明 · 免责声明', f: viewAbout },
};

// ─────────────────────────────────────────────
// 顶部按钮（按页面显示）
// ─────────────────────────────────────────────
function renderHdBtns(v) {
  const box = $('#hdbtns');
  const btns = [];
  // 总览 + 接入源：服务网关信息
  if (v === 'overview' || v === 'sources') {
    btns.push(`<button class="btn sm pri" onclick="showGatewayInfo()" title="查看本地网关地址 / KEY / 模型">服务网关信息</button>`);
  }
  // 代理设置：每个页面都有（直连/全局/绕过大陆，所有流程即时生效）
  btns.push(`<button class="btn sm" onclick="showProxySettings()" title="出站网络代理（直连 / 全局 / 绕过大陆）">⇅ 代理设置</button>`);
  // 全局刷新：每个页面都有（替代原「退出」）
  btns.push(`<button class="btn sm" onclick="window.location.reload()" title="全局刷新（重载面板数据）">⟳ 刷新</button>`);
  box.innerHTML = btns.join('');
}

// 代理设置弹窗（direct / global / bypass_cn）
async function showProxySettings() {
  let cur = { mode: 'direct', url: '' };
  try { const r = await api('/api/proxy'); cur = r; } catch (_) {}
  const MODES = [
    ['direct', '全部直连', '不经过任何代理（默认）'],
    ['global', '全局代理', '所有流程（登录/上游/模型同步）都走代理'],
    ['bypass_cn', '绕过大陆', '大陆域名（含阿里云/百度云/华为云/讯飞等国内厂商）直连，其余走代理（推荐给 CODEX/Kimi 等海外平台）'],
  ];
  const mk = modalShell('代理设置', `
      <div class="radios">
        ${MODES.map(([m, t, d]) => `
          <label class="radio-row ${cur.mode === m ? 'on' : ''}" onclick="document.querySelectorAll('.radio-row').forEach(r=>r.classList.remove('on'));this.classList.add('on');$('#px-mode').value='${m}';$('#px-url').disabled=${m === 'direct' ? 'true' : 'false'}">
            <input type="radio" name="pxmode" value="${m}" ${cur.mode === m ? 'checked' : ''}>
            <b>${t}</b><span class="faint">${d}</span>
          </label>`).join('')}
      </div>
      <input type="hidden" id="px-mode" value="${cur.mode}">
      <label>代理地址</label>
      <input class="q mono" id="px-url" placeholder="http://127.0.0.1:7890" value="${esc(cur.url || '')}" ${cur.mode === 'direct' ? 'disabled' : ''}>
      <div class="faint" style="font-size:12px">保存后立即生效（登录、上游转发、模型同步全部从代理走），无需重启</div>
      <div class="faint" style="font-size:12px">本机与局域网地址（127.0.0.1 / 192.168.x / localhost / *.local 等）在任何模式下都直连：代理进程连不到你的内网，硬走代理只会得到"连不上"的假故障</div>
      <div class="modal-foot">
        <button class="btn pri" onclick="saveProxy()">保存并应用</button>
      </div>`);
  document.body.insertAdjacentHTML('beforeend', mk);
}
async function saveProxy() {
  const mode = $('#px-mode').value || 'direct';
  const url = $('#px-url').value.trim();
  // 非本机代理要二次确认：一旦保存，所有上游请求（含你填的 Key / Cookie）
  // 都会先经过这台主机 —— 它能读到的东西比任何一个上游都多。
  // 非回环代理要二次确认：一旦保存，所有上游请求（含你填的 Key / Cookie）
  // 都会先经过这台主机 —— 它能读到的东西比任何一个上游都多。
  // 服务端同样要求 confirm_nonlocal:true，前后端两道闸门都不许静默换代理。
  const nonlocal = mode !== 'direct' && !!url && !isLocalProxy(url);
  if (nonlocal) {
    const ok = confirm(
      '你填写的代理不是本机回环地址：\n\n' + url + '\n\n' +
      '保存后，登录、模型同步、每一次上游转发（含请求里的 Key / Cookie）都会先经过这台主机。\n' +
      '只有在你信任这台主机的情况下才继续。\n\n确定要应用吗？'
    );
    if (!ok) {
      const btn0 = document.querySelector('.modal-foot .btn.pri');
      if (btn0) { btn0.textContent = '✗ 已取消'; setTimeout(() => btn0.textContent = '保存并应用', 2000); }
      return;
    }
  }
  const payload = { mode, url };
  if (nonlocal) payload.confirm_nonlocal = true;
  const r = await post('/api/proxy', payload);
  const btn = document.querySelector('.modal-foot .btn.pri');
  if (btn) { btn.textContent = r.ok ? '✓ 已应用（即时生效）' : ('✗ ' + (r.message || '失败')); setTimeout(() => btn.textContent = '保存并应用', 2000); }
}
// 代理地址是否指向本机回环（127.x / localhost / ::1）。
function isLocalProxy(url) {
  const s = String(url || '').trim();
  const at = s.indexOf('://');
  if (at < 0) return false;
  let host = s.slice(at + 3);
  for (const sep of ['/', '?', '#']) {
    const k = host.indexOf(sep);
    if (k >= 0) host = host.slice(0, k);
  }
  if (host.startsWith('[')) host = host.slice(1).split(']')[0];
  const h = host.split(':')[0].toLowerCase();
  return h === 'localhost' || h === '::1' || h === '127.0.0.1' || h.indexOf('127.') === 0;
}

// 秒数 → 人话（后台任务台账用；只做粗略刻度，精确到分钟足够判断「还活着」）。
function agoText(sec) {
  if (typeof sec !== 'number' || !isFinite(sec) || sec < 0) return '—';
  if (sec < 90) return '刚刚';
  if (sec < 5400) return Math.round(sec / 60) + ' 分钟前';
  if (sec < 172800) return Math.round(sec / 3600) + ' 小时前';
  return Math.round(sec / 86400) + ' 天前';
}

// 服务网关信息弹窗（地址 + KEY + 模型）
async function showGatewayInfo() {
  const [h, pl, md] = await Promise.all([
    api('/api/health').catch(() => null),
    api('/api/platforms').catch(() => null),
    fetch('/v1/models', withToken()).then(r => r.json()).catch(() => null),
  ]);
  const base = location.origin;
  // 这里要显示的是「可路由的模型数」，不是平台数（pl.total 是 122 个平台规格）。
  const modelsN = md && Array.isArray(md.data) ? md.data.length : '—';
  const platN = pl ? pl.total : '—';
  // 令牌是否必需由后端说（/api/health 的 token_required / token_source）。
  // 旧代码在这里写死「/v1 不校验 Key」，而 --token、AIGW_PANEL_TOKEN、对外监听
  // 自动生成这三种情况下 /v1 都要令牌 —— 用户照着填任意值只会拿到 401，且界面上
  // 没有任何线索指向令牌从哪来。
  const tokenOn = !!(h && h.token_required);
  const known = (typeof PANEL_TOKEN !== 'undefined' && PANEL_TOKEN) ? PANEL_TOKEN : '';
  const SRC_LABEL = { cli: '启动参数 --token', env: '环境变量 AIGW_PANEL_TOKEN', auto: '对外监听时自动生成（见启动日志）' };
  const srcLabel = SRC_LABEL[(h && h.token_source) || ''] || '启动参数 --token 或环境变量 AIGW_PANEL_TOKEN';
  const gwKey = tokenOn ? (known || '<面板令牌>') : 'aigw-local';
  let keyNote;
  if (!tokenOn) {
    keyNote = '本机回环监听且未设令牌：/api 与 /v1 都不校验 Key，客户端填任意非空值即可';
  } else if (known) {
    keyNote = '令牌保护已开启（来源：' + srcLabel + '）：/api 与 /v1 都要带这个令牌，这里回显以便复制';
  } else {
    keyNote = '令牌保护已开启（来源：' + srcLabel + '），但浏览器里还没保存令牌：/v1 的 API Key 必须填它；先把 ?token=你的令牌 加到地址栏重新打开本面板';
  }
  // 后台任务台账：/api/health 的 tasks 里带着「成功几次 / 最近一次什么时候 / 最近错误」，
  // 让「公益站该刷新却没刷新」「预热 panic 过」这类静默故障在界面上能直接看出来。
  const TASK_LABEL = { warm_models: '启动预热模型清单', free_refresh: '公益站每日刷新' };
  const tasks = (h && h.tasks) || {};
  let taskHtml = '';
  const tkeys = Object.keys(tasks);
  for (let i = 0; i < tkeys.length; i++) {
    const k = tkeys[i];
    const t = tasks[k] || {};
    let line = (TASK_LABEL[k] || k) + '：成功 ' + (t.ok_runs || 0) + ' 次';
    line += (typeof t.last_ok_s_ago === 'number') ? ('，最近一次 ' + agoText(t.last_ok_s_ago)) : '，尚未成功过';
    // 成功也要带说明：否则「刷新成功但只拿到 0 个站」与「拿到 43 个站」在面板上一样。
    if (t.last_ok) line += '（' + t.last_ok + '）';
    if (t.last_err) line += ' · 最近失败（' + agoText(t.last_err_s_ago) + '）：' + t.last_err;
    taskHtml += '<div class="faint" style="font-size:12px">' + esc(line) + '</div>';
  }
  if (taskHtml) taskHtml = '<label>后台任务</label>' + taskHtml;
  // 限流现场：429 只出现在响应体里，界面上必须说清「挡住过几次、多久前、哪档触顶」，
  // 否则「面板突然全都失败」时用户只能猜是不是上游坏了。
  const lim = (h && h.limits) || {};
  let limHtml = '';
  if (lim.enabled) {
    let line = '每 IP ' + (lim.per_ip_rpm || 0) + '/分钟 · 全局 ' + (lim.global_rpm || 0) + '/分钟';
    if ((lim.key_rpm || 0) > 0) line += ' · 每把访问密钥 ' + lim.key_rpm + '/分钟';
    if ((lim.rejected_total || 0) > 0) {
      line += ' · 已挡下 ' + lim.rejected_total + ' 次，最近一次 ' + agoText(lim.last_reject_s_ago || 0);
      // 三档要说清是哪一档：把「某把 key 被限」说成「单 IP 额度」会让人去调错旋钮。
      const scopeTxt = lim.last_reject_scope === 'global' ? '全局额度'
        : (lim.last_reject_scope === 'key' ? '某把访问密钥的额度' : '单 IP 额度');
      line += '（' + scopeTxt + '触顶，建议客户端等 ' + (lim.last_retry_after_s || 60) + ' 秒）';
    } else {
      line += ' · 还没有请求被挡下';
    }
    limHtml = '<label>限流</label><div class="faint" style="font-size:12px">' + esc(line) + '</div>';
  }
  const mk = modalShell('服务网关信息', `
      <label>代理地址（base_url）</label>
      <input class="q mono" readonly value="${esc(base)}/v1" onclick="this.select();copyText(this.value)">
      <label>API Key</label>
      <input class="q mono" readonly value="${esc(gwKey)}" onclick="this.select();copyText(this.value)">
      <div class="faint" style="font-size:12px;margin:-4px 0 6px">${esc(keyNote)}</div>
      <label>可用模型</label>
      <div class="faint" style="font-size:12px">${modelsN} 个模型可用（共 ${platN} 个平台规格）· 模型名写 <code>模型id@平台</code> 或任意清单内模型</div>
      ${taskHtml}
      ${limHtml}
      <div class="modal-foot">
        <button class="btn pri" onclick="navigator.clipboard.writeText('${att(base)}/v1');this.textContent='已复制地址';setTimeout(()=>this.textContent='复制地址',1200)">复制地址</button>
        <button class="btn" onclick="navigator.clipboard.writeText('${att(gwKey)}');this.textContent='已复制KEY';setTimeout(()=>this.textContent='复制KEY',1200)">复制 KEY</button>
        <button class="btn" onclick="rotatePanelToken('generate')">轮换令牌</button>
        <button class="btn" onclick="rotatePanelToken('set')">设置令牌</button>
        ${h && h.token_required ? '<button class="btn" onclick="rotatePanelToken(\'clear\')">关闭令牌</button>' : ''}
      </div>
`);
  document.body.insertAdjacentHTML('beforeend', mk);
}
// 面板令牌轮换：改令牌不再等于重启服务。
// 返回值里的 token 只回显这一次（后端 /api/token 是唯一出口），所以必须当场写进
// localStorage 并弹给用户，否则「换了令牌但没记下来」会让面板直接进不去。
async function rotatePanelToken(mode) {
  let payload = { action: 'generate' };
  if (mode === 'set') {
    const t = (prompt('输入新的面板令牌（至少 16 个字符、不能含空白）：\n设置后旧令牌立即失效。') || '').trim();
    if (!t) return;
    payload = { action: 'set', token: t };
  } else if (mode === 'clear') {
    if (!confirm('关闭面板令牌？\n\n关闭后 /api 与 /v1 都不再校验令牌 —— 只有服务仅监听本机回环地址时才安全。')) return;
    payload = { action: 'clear' };
  } else if (!confirm('生成新的面板令牌？\n\n旧令牌立即失效：正在使用旧令牌的 SDK 与浏览器面板都要换成新的。')) {
    return;
  }
  const r = await post('/api/token', payload);
  if (!r || !r.ok) { alert('操作失败：' + ((r && r.message) || '未知错误')); return; }
  if (r.token) {
    try { localStorage.setItem(TOKEN_KEY, r.token); } catch (_) {}
    PANEL_TOKEN = r.token;
    alert('新的面板令牌（只显示这一次，已写入本浏览器）：\n\n' + r.token +
      (r.persisted === false ? '\n\n注意：没能落盘，重启服务后会失效。' : ''));
  } else {
    try { localStorage.removeItem(TOKEN_KEY); } catch (_) {}
    PANEL_TOKEN = '';
    alert(r.message || '已关闭令牌');
  }
  location.reload();
}

async function copyText(t) { try { await navigator.clipboard.writeText(t); } catch (_) { /* 用户手动复制 */ } }

// ─────────────────────────────────────────────
// 接入源页：分组卡片 + 登录面板 + 自定义 API + 公益站
// ─────────────────────────────────────────────
let SRC_Q = '';
let SRC_HELP = false;
let SRC_OPEN = {};   // 分组收起状态：group → bool（默认收起 false）
let SRC_BADONLY = false; // 只看「近 24h 有失败」的接入源
// 只看「支持扫码登录」的接入源（按平台声明的 login_methods 含 qr 判定）
let SRC_QRSONLY = false;
let SRC_EXPAND = null; // 展开的平台卡片 id

// 分组规则（用户拍板 + 参考项目登录方式核对）：
//   custom-*        → 自定义 API（最前，默认展开）
//   api-* / apk-coze / kimi-code / codex → 开放平台 API（填官方 Key，默认收起）
//     （扣子 Coze、Kimi 开放平台、Codex 官方 Key 均为官方 API Key 直连）
//   sub-*           → 订阅反代（OAuth/跳转，默认收起）
//   free-*          → 公益站（填 Key 即用，默认收起）
//   其余（apk-*/web-*/wb-*/anon-*/kilo/pollinations + minimax + codearts）→ 客户端反代
//     （minimax=MiniMax Code 设备码、codearts=华为云账号跳转登录+AK/SK，均非单纯 Key）
function srcGroupOf(p) {
  if (p.id.startsWith('custom-')) return 'custom';
  if (p.id.startsWith('api-') || GROUP_OVERRIDE.official.includes(p.id)) return 'official';
  if (p.id.startsWith('sub-')) return 'sub';
  if (p.id.startsWith('free-') || GROUP_OVERRIDE.free.includes(p.id)) return 'free';
  return 'client';
}
const GROUP_META = {
  custom:   { t: '自定义 API', desc: '自填 名称/协议/URL/Key，自动同步模型（CherryStudio 式）', open: true },
  official: { t: '开放平台 API', desc: '官方平台 API Key 直连', open: false },
  sub:      { t: '订阅反代', desc: '订阅账号 OAuth 授权 → 网关反代', open: false },
  client:   { t: '客户端反代', desc: '网页版 / 客户端凭据（Cookie / Token）', open: false },
  free:     { t: '公益站 · 免费车道', desc: '公益中转聚合 + 免 Key 免费车道（Kilo / Pollinations / OpenCode Zen）', open: false },
};

async function viewSources(el) {
  const pl = await api('/api/platforms');
  const fm = await freeMeta();
  window.__DISABLED_MODELS = pl.disabled_models || [];
  // 「近 24h 有问题」= 台账里这段窗口内有失败记录。样本数为 0 的平台不进来
  //（没跑过 ≠ 坏了，把它算进来只会让人以为一堆平台挂了）。
  const isBad = (p) => !!(p.recent && p.recent.failed > 0);
  const badN = pl.rows.filter(isBad).length;
  // 「支持扫码登录」= toml 里 auth 声明为 qr_scan 的平台（有出码 + 轮询的纯服务端协议）
  const isQr = (p) => (Array.isArray(p.login_methods) ? p.login_methods : [p.auth]).includes('qr');
  const qrN = pl.rows.filter(isQr).length;
  const rows = pl.rows.filter((p) => {
    if (SRC_BADONLY && !isBad(p)) return false;
    if (SRC_QRSONLY && !isQr(p)) return false;
    if (!SRC_Q) return true;
    const q = SRC_Q.toLowerCase();
    return (p.name || '').toLowerCase().includes(q) || (p.id || '').toLowerCase().includes(q) || (p.notes || '').toLowerCase().includes(q);
  });
  const groups = ['custom', 'official', 'sub', 'client', 'free'];
  const byGroup = {};
  for (const g of groups) byGroup[g] = rows.filter((p) => srcGroupOf(p) === g);
  // 「已接入」= 有凭据，或免密车道（anon：kilo / pollinations / anon-zen 无需任何账号）。
  // 旧实现只数 accounts_n > 0，免密车道因为账号池里没有占位行就永远显示「未接入」。
  const isIn = (p) => !!p.anon || p.accounts_n > 0;
  const loggedN = pl.rows.filter(isIn).length;

  let h = `<div class="srcbar">
    <div class="srcbar-i"><i>平台</i><b>${pl.total}</b></div>
    <div class="srcbar-i"><i>已接入</i><b>${loggedN}</b></div>
    <div class="srcbar-i"><i>账号凭据</i><b>${pl.rows.reduce((a, p) => a + (p.accounts_n || 0), 0)}</b></div>
    <div class="srcbar-i" title="支持扫码登录的接入源（点下面「仅看扫码」可筛）"><i>可扫码</i><b>${qrN}</b></div>
    <div class="srcbar-i" title="台账里近 24 小时有失败记录的接入源"><i>近24h有问题</i><b${badN ? ' style="color:#e5484d"' : ''}>${badN}</b></div>
    <button class="btn sm gh" style="margin-left:auto" onclick="SRC_HELP=!SRC_HELP;render()">怎么用 ${SRC_HELP ? '▴' : '▾'}</button>
  </div>`;
  if (SRC_HELP) {
    h += `<div class="note">接入三步：① 点卡片接入（官方/公益站填 Key；客户端/订阅跳转登录后粘贴凭据）② 客户端 base_url 填 <code>${esc(location.origin)}/v1</code>、api_key 填 <code>admin</code> ③ model 填 <code>模型id@平台</code> 或清单内模型。</div>`;
  }
  h += `<div class="srcbar2">
    <input class="q" placeholder="搜索平台名称 / id / 说明…" value="${esc(SRC_Q)}"
      oninput="SRC_Q=this.value;srcRerender()" onkeydown="if(event.key==='Escape'){SRC_Q='';this.value='';srcRerender()}">
    <button class="btn sm ${SRC_BADONLY ? 'pri' : 'gh'}" onclick="SRC_BADONLY=!SRC_BADONLY;srcRerender()"
      title="只看台账里近 24 小时有失败记录的接入源">${SRC_BADONLY ? '✔ ' : ''}仅看有问题${badN ? '（' + badN + '）' : ''}</button>
    <button class="btn sm ${SRC_QRSONLY ? 'pri' : 'gh'}" onclick="SRC_QRSONLY=!SRC_QRSONLY;srcRerender()"
      title="只看支持扫码登录的接入源（豆包 / 库库 / 小浣熊 / Loomy）">${SRC_QRSONLY ? '✔ ' : ''}仅看扫码${qrN ? '（' + qrN + '）' : ''}</button>
    <span class="faint" style="font-size:11.5px">显示 ${rows.length} 个</span>
  </div>`;

  for (const g of groups) {
    const meta = GROUP_META[g];
    const items = byGroup[g] || [];
    const onN = items.filter(isIn).length;
    // 任何筛选生效时都强制展开：否则命中的卡片全在收起的分组里，用户点了一下看不到任何变化。
    const open = SRC_BADONLY || SRC_QRSONLY || (SRC_OPEN[g] ?? meta.open);
    h += `<section class="card glass srcgroup">
      <div class="srcgroup-hd" onclick="SRC_OPEN['${att(g)}']=!SRC_OPEN['${att(g)}'];srcRerender()">
        <span class="sg-arrow">${open ? '▾' : '▸'}</span>
        <b>${esc(meta.t)}</b>
        <span class="faint">${esc(meta.desc)}</span>
        <span class="pill mut">${onN}/${items.length} 接入</span>
        ${g === 'custom' ? '<button class="btn sm pri" style="margin-left:auto" onclick="event.stopPropagation();openCustomApi()">＋ 新建自定义 API</button>' : ''}
        ${g === 'free' ? '<button class="btn sm" style="margin-left:auto" onclick="event.stopPropagation();refreshFree()">⟳ 刷新公益站</button>' : ''}
      </div>
      ${open && g === 'free' && fm ? `<div class="note" style="margin:8px 0 10px">${freeSourceLine(fm)}</div>` : ''}
      ${open ? `<div class="srcgrid">${items.map((p) => srcCard(p)).join('') || '<div class="empty">该分组暂无平台</div>'}</div>` : ''}
    </section>`;
  }
  el.innerHTML = h;
  // 登录抽屉
  if (SRC_EXPAND) {
    const p = pl.rows.find((x) => x.id === SRC_EXPAND);
    if (p) {
      el.insertAdjacentHTML('beforeend', srcDrawer(p));
      const d = document.getElementById('srcDrawer');
      if (d) { d.scrollIntoView({ behavior: 'smooth', block: 'nearest' }); }
    }
  }
}

// 近 24h 失败率徽章（数据由后端 /api/platforms 的 recent 字段给出）。
//
// 为什么需要：接入源有 128 个，其中一大半是「上游随时改反爬 / 匿名车道被封」的
// 免费站，卡片长得一模一样，用户只能逐个点开试。台账里本来就有 ok 标记，后端
// 顺手聚合出 recent；这里把它变成一眼能看见的红标。
// 口径：少于 3 次样本不标（一次失败说明不了什么，标了全是噪声）；失败率 ≥50%
// 才是红标（这类基本可以认定「这条车道这段时间废了」），其余有失败给黄标。
function recentHealthPill(p) {
  const r = p.recent;
  if (!r || !r.requests) return '';
  const req = r.requests || 0;
  const bad = r.failed || 0;
  if (req < 3) return '';
  const rate = bad / req;
  if (rate >= 0.5) return pill('err', `近24h ${bad}/${req} 失败`);
  if (bad > 0) return pill('warn', `近24h 失败 ${Math.round(rate * 100)}%`);
  return '';
}

// 平台卡片（精简：名称 + 状态 + 接入按钮，无长介绍）
// 最近一次失败原因。只显示「近24h 失败 40%」用户没法处置——401 要重登、
// 402 要等额度、连接错误要看网络，三者动作完全不同。台账里只有成败计数，
// 所以后端单独记了一份原因（成功一次即清除）。
function lastErrNote(p) {
  const e = p && p.last_error;
  if (!e || !e.text) return '';
  const code = e.status ? 'HTTP ' + e.status + ' · ' : '';
  const when = typeof e.s_ago === 'number' ? ' ' + fmtAgo(e.s_ago) : '';
  // 上游常把整段 JSON 当原因返回（几百字符且没有空格），一行塞进卡片会把布局撑破：
  // 只显示前 140 字，全文放 title；overflow-wrap 兜住没有空格的长串。
  const full = String(e.text);
  const shown = full.length > 140 ? full.slice(0, 140) + '…' : full;
  return '<div class="faint" style="font-size:11px;margin:-2px 0 7px;overflow-wrap:anywhere" title="' +
    esc(full) + '">最近失败' + when + '：' + esc(code) + esc(shown) + '</div>';
}

// 卡片上的「注册页 / 邀请链接」入口。117 个接入源在 providers/*.toml 里都写了 login_url，
// 但旧版只在登录抽屉里用，卡片上一个字都看不见 —— 想接入一个还没配过的平台，
// 用户得先去搜官网地址。公益站（free-*）的链接还带邀请码，额外给「复制邀请链接」。
// 注意：URL 不能拼进 onclick 的 JS 字符串里 —— esc() 会把 ' 转成 &#39;，
// 浏览器解码后再交给 JS 解析就把字符串打断了；所以走 data-url + dataset。
function loginBtn(p) {
  const u = safeUrl(p.login_url);
  if (!u) return '';
  const isFree = String(p.id).startsWith('free-');
  return `<a class="btn sm" href="${esc(u)}" target="_blank" rel="noopener noreferrer"
      title="${esc(u)}" onclick="event.stopPropagation()">注册页</a>
    ${isFree ? `<button class="btn sm" data-url="${esc(u)}" title="${esc(u)}"
      onclick="event.stopPropagation();copyAffLink(this)">复制邀请链接</button>` : ''}`;
}
// 复制邀请链接：成功把按钮文字临时改成「已复制」。
// 剪贴板在「非本机地址」的 http 页面上会被浏览器禁用（LAN IP 访问就是这种情况），
// 这时不能默默失败 —— 直接弹一个可手动复制的框，否则用户以为已经复制好了。
async function copyAffLink(btn) {
  if (!btn) return;
  const u = btn.dataset.url || '';
  let ok = false;
  try { await navigator.clipboard.writeText(u); ok = true; } catch (_) { ok = false; }
  if (!ok) {
    window.prompt('浏览器在这个地址上禁用了剪贴板（用局域网 IP 访问时常见），请手动复制：', u);
    return;
  }
  const t = btn.dataset.label || btn.textContent;
  btn.dataset.label = t;
  btn.textContent = '已复制';
  setTimeout(() => { btn.textContent = t; }, 1200);
}

function srcCard(p) {
  // 徽章按「实际登录方式」显示（参考项目一手核对），不按 toml auth 字段一刀切
  const AUTH_LABEL = {
    minimax: '设备码', codearts: '跳转/AK-SK',
    lobster: 'OAuth', 'web-lobster': 'OAuth',
    'wb-gateway': '账号登录', 'wb-gateway-intl': '账号登录', cline: '设备码',
    'apk-qoder': '设备码', 'web-qoder': '设备码',
    'apk-trae': '跳转回调', 'web-trae': '跳转回调',
    'apk-loomy': '微信扫码', 'web-deepseek': '粘贴Token', 'web-qwen': '粘贴Token+Cookie', 'mimo': '粘贴Cookie', 'sub-kiro': '粘贴Kiro凭据', 'apk-coze': '粘贴PAT',
    'apk-metaso': '免密/Cookie',
    'apk-doubao': '扫码', 'apk-yuanbao': 'Cookie', 'apk-kuku': '扫码',
    'apk-raccoon': '微信扫码', 'apk-wps': 'Cookie', 'apk-nano': 'Cookie',
    'sub-antigravity': '跳转登录/Token', 'apk-antigravity': '粘贴Token',
    'sub-devin': '跳转登录', 'sub-meta': '设备码',
    'sub-codex': '跳转登录', 'sub-claude': '跳转登录', 'sub-grok': '设备码',
    'sub-kimi': '设备码', 'sub-kimi-ai': '设备码',
  };
  // p.auth 的取值来自后端 AuthKind（snake_case）：anon / api_key / o_auth_device / qr_scan /
  // cookie / client_file。旧表里写了并不存在的 'token'，又漏了 device/scan/file 三种，
  // 结果那些卡片在徽章上直接显示英文枚举值。
  const authTxt = AUTH_LABEL[p.id] || {
    api_key: '填 Key', cookie: 'Cookie', anon: '免密',
    o_auth_device: '设备码', qr_scan: '扫码登录', client_file: '客户端文件',
  }[p.auth] || p.auth;
  // 自定义 API：已配置 = 有 key 掩码；免密车道（anon）恒可用；其余 = 有账号
  const configured = p.custom ? (p.key_mask ? true : false) : p.anon ? true : p.accounts_n > 0;
  // 面板上被停用（flags.json）与「没接入」是两种状态，必须分开显示：
  // anon-zen 这类「有账号但已被停用」的卡片，旧版显示「1 账号·已配置」却一个模型都不出，
  // 用户分不清是自己关的、还是坏了。
  const disabled = p.enabled === false;
  // usable = 后端判定「真能转发」（已启用 + 有凭据）。未接入平台的模型已被
  // /v1/models 过滤，卡片要讲清楚，否则用户会以为模型丢了。
  const usable = p.usable !== false;
  return `<div class="srccard glass ${p.enabled ? '' : 'off'}" data-id="${esc(p.id)}">
    <div class="srcname">${esc(p.name)}<span class="faint mono" style="font-size:10.5px"> ${esc(p.id)}</span></div>
    <div class="srcmeta">
      ${p.custom ? pill('ok', '自定义') : p.id.startsWith('sub-') ? pill('mut', '订阅') : p.id.startsWith('free-') ? pill('mut', '公益') : pill('mut', authTxt)}
      ${p.region === 'cn' ? pill('mut', '国内') : p.region === 'intl' ? pill('mut', '国际') : ''}
      ${qrPill(p)}
      ${disabled ? pill('mut', '已停用') : configured ? pill('ok', p.custom ? '已配置' : p.anon ? '免凭据' : `${p.accounts_n} 账号`) : pill('mut', '未接入')}
      ${unusablePill(p, disabled, usable, configured)}
      ${p.chat_supported === false && usable ? pill('mut', '仅验活·不可对话') : ''}
      ${recentHealthPill(p)}
      ${modelCountPill(p)}
    </div>
    ${lastErrNote(p)}
    <div class="row" style="gap:6px">
      <button class="btn sm ${configured ? '' : 'pri'}" onclick="event.stopPropagation();SRC_EXPAND='${att(p.id)}';render()">
        ${configured ? (p.custom ? '编辑' : '管理 / 重接') : '接入'}
      </button>
      <button class="btn sm" title="停用后不出现在 /v1/models，也不参与路由"
        onclick="event.stopPropagation();togglePlatform('${att(p.id)}', ${p.enabled ? 'false' : 'true'})">${p.enabled ? '停用' : '启用'}</button>
      ${loginBtn(p)}
    </div>
  </div>`;
}

// 平台启用/停用：写 data/flags.json（不改 providers/*.toml），即时生效
async function togglePlatform(id, enabled) {
  // ★ 自定义 API 的启停存 custom.json 的 enabled 字段，不在 flags.json。
  //   旧实现对 custom-* 也发 toggle_platform → 写进 flags.disabled_platforms，
  //   卡片 enabled 读的是 custom.enabled → 按钮文字不变、卡片不置灰、模型照旧可用。
  const isCustom = String(id).startsWith('custom-');
  const r = await post(isCustom ? '/api/custom' : '/api/platforms', isCustom ? { action: 'toggle', id, enabled } : { action: 'toggle_platform', id, enabled }).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert('操作失败：' + (r.message || '未知错误')); return; }
  srcRerender();
}

// 逐模型开关：按裸模型名记账（模型@平台 一并生效）
async function toggleModel(model, enabled) {
  const r = await post('/api/platforms', { action: 'toggle_model', model, enabled }).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert('操作失败：' + (r.message || '未知错误')); return; }
  srcRerender();
}

// 「不可用」的原因别一律写成「凭据不可用」：能转发要同时满足 凭据 / 协议 / 支持对话 /
// 地址非占位符四条，缺哪条要去改的地方完全不同（填 Key / 改 format / 换平台 / 配网关）。
// 文案与 /v1/models?platform=<id> 的 platform.message 对齐（那边是完整句，这里是短标签）。
function unusableShortReason(p) {
  if (p.enabled === false) return null;
  if (p.placeholder) return '上游地址是占位符';
  if (p.format && p.format !== 'openai') return '协议 ' + p.format + ' 不支持透传';
  if (p.chat_supported === false) return '仅验活·不可对话';
  return null;
}
function unusablePill(p, disabled, usable, configured) {
  if (disabled || usable) return '';
  if (!configured) return '';                       // 「未接入」已经有一枚徽章，别叠两个字
  const why = unusableShortReason(p);
  const tip = why ? '这个平台现在不发布模型：' + why + '（点「N 模型」可看 /v1/models 的说明）'
                  : '这个平台现在不发布模型：没有可用凭据';
  return `<span title="${att(tip)}">${pill('warn', why || '凭据不可用·模型已隐藏')}</span>`;
}
// 「N 模型」徽章做成链接：点开就是该平台在 /v1/models?platform=<id> 里的样子。
// 停用平台也照样给链接——那个接口会回一份 platform 说明（「这个平台是停用的」），
// 比让用户对着空清单猜要强。
// 支持扫码的平台给一枚显式徽章：用户常在卡片上就决定「这个能不能扫码登录」，
// 抽屉里才知道太晚了。判定用后端下发的 login_methods（支持多路登录的平台也算）。
function qrPill(p) {
  const lm = Array.isArray(p.login_methods) ? p.login_methods : [];
  if (!lm.includes('qr')) return '';
  return '<span class="pill ok" title="支持扫码登录：点开卡片有「开始登录」出二维码">可扫码</span>';
}

function modelCountPill(p) {
  if (p.models_policy === 'live') return pill('mut', 'live 模型');
  const n = p.models_n || 0;
  if (!n) return p.custom ? pill('ok', '已固定 0 模型') : '';
  const href = '/v1/models?platform=' + encodeURIComponent(p.id);
  return '<a class="pill ' + (p.custom ? 'ok' : 'mut') + '" style="text-decoration:none" href="' + att(href)
    + '" target="_blank" rel="noopener" title="在新标签页打开这个平台的模型清单（/v1/models?platform=' + esc(p.id)
    + '）" onclick="event.stopPropagation()">' + esc((p.custom ? '已固定 ' : '') + n + ' 模型') + '</a>';
}
// 抽屉里的模型清单：逐模型启用开关（停用按裸模型名记账，模型@平台 一并生效）
function srcModelsBlock(p) {
  const ms = p.models || [];
  if (!ms.length) {
    return p.models_policy === 'live'
      ? '<div class="note">该平台模型清单为 <b>live</b>：首次调用 /v1/models 时向上游拉取，拉不到则用静态清单。</div>'
      : '';
  }
  const onN = p.models_on == null ? ms.length : p.models_on;
  const rows = ms.map((m) => {
    // 后端下发的 disabled_models 是裸名；模型名自带 @ 时按同规则取裸名比对
    const bare = m.includes('@') ? m.slice(0, m.lastIndexOf('@')) : m;
    const on = !(window.__DISABLED_MODELS || []).includes(bare);
    return `<div class="mrow">
      <span class="mono" style="font-size:12px">${esc(m)}</span>
      <button class="btn sm" onclick="toggleModel('${att(m)}', ${on ? 'false' : 'true'})">${on ? '停用' : '启用'}</button>
    </div>`;
  }).join('');
  // 自定义 API 且清单已固定（models_policy 不再是 live）：说清楚「这份清单说了算」，
  // 并给一键恢复跟随上游（服务端 sync_models 不带清单 = 重拉上游并解除固定）。
  const pinnedNote = (p.custom && p.models_policy !== 'live')
    ? `<div class="note">清单已固定：/v1/models 只发布这 ${ms.length} 个模型，不随上游变化。
        <button class="btn sm" style="margin-left:6px" onclick="unpinCustomModels('${att(p.id)}')">恢复跟随上游</button></div>`
    : '';
  return `${pinnedNote}<div class="note">模型 ${onN}/${ms.length} 启用（停用后不出现在 /v1/models，路由也会拒绝）</div>
    <div class="mlist">${rows}</div>`;
}

// 恢复跟随上游：重新拉上游全量清单并解除固定。
async function unpinCustomModels(id) {
  if (!confirm('恢复跟随上游？将重新拉取上游全量模型清单，替换当前固定的白名单。')) return;
  const r = await post('/api/custom', { action: 'sync_models', id }).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert('操作失败：' + (r.message || '未知错误')); return; }
  alert(r.message || '已恢复跟随上游');
  srcRerender();
}

// 短信登录：第一步发码（上游回 msgid），第二步交验证码换 session 入库。
// msgid 与手机号存 window：抽屉重渲染时表单要保住已填内容，否则用户一刷新就得重填。
async function smsSend(id) {
  const el = document.getElementById('smsPhone');
  const phone = (el && el.value || '').trim();
  if (!phone) { alert('先填手机号'); return; }
  window.__SMS_PHONE = phone;
  const r = await post('/api/auth', { action: 'sms_send', platform: id, phone }).catch((e) => ({ ok: false, message: e.message }));
  if (!r || !r.ok) { alert('发送失败：' + ((r && r.message) || '未知错误')); return; }
  window.__SMS_MSGID = r.msgid || '';
  const code = document.getElementById('smsCode');
  if (code) code.focus();
  alert('验证码已发出（' + (r.msgid ? 'msgid 已记下' : '未拿到 msgid') + '），收到后填到下面一格再点「验证并登录」。');
}

// 登录方式切换：只改 tab 再重绘抽屉（不整页 route，避免滚动位置丢失）。
function loginTab(k) {
  window.__LOGIN_TAB = k;
  render();
}

// 密码登录：与短信是同一种 session 产物，但不需要人读码 —— 适合把它记在自己电脑上，
// session 14 天到期后自动重登（短信每次都要人守着手机）。
async function pwdLogin(id) {
  const loginid = ((document.getElementById('pwdId') || {}).value || '').trim();
  const password = (document.getElementById('pwdPw') || {}).value || '';
  if (!loginid || !password) { alert('账号和密码都要填'); return; }
  const r = await post('/api/auth', { action: 'pwd_login', platform: id, loginid, password })
    .catch((e) => ({ ok: false, message: e.message }));
  if (!r || !r.ok) { alert('登录失败：' + ((r && r.message) || '未知错误')); return; }
  const pw = document.getElementById('pwdPw');
  if (pw) pw.value = '';
  alert(r.message || '密码登录成功');
  SRC_EXPAND = '';
  await route();
}

async function smsVerify(id) {
  const phone = (document.getElementById('smsPhone') || {}).value || window.__SMS_PHONE || '';
  const code = (document.getElementById('smsCode') || {}).value || '';
  if (!phone || !code) { alert('手机号和验证码都要填'); return; }
  const r = await post('/api/auth', {
    action: 'sms_verify', platform: id, phone: phone.trim(), code: code.trim(), msgid: window.__SMS_MSGID || '',
  }).catch((e) => ({ ok: false, message: e.message }));
  if (!r || !r.ok) { alert('登录失败：' + ((r && r.message) || '未知错误')); return; }
  window.__SMS_MSGID = '';
  alert(r.message || '短信登录成功');
  SRC_EXPAND = '';
  await route();
}

// 登录抽屉：跳转登录 + 手动粘贴凭据（不扫码）；wb/minimax/kimi/grok/cline/qoder 走设备码，codex/claude/antigravity/devin/trae/codearts 走跳转回调
// 抽屉里的「现在为什么不发布模型」横幅：卡片上只有短标签 + title，进来的人是想修的，
// 直接把完整原因、该改什么、以及 /v1/models?platform= 的证据链接摊开。
function unusableBanner(p) {
  if (p.enabled === false) {
    return '<div class="note" style="border-color:#5a4a2a">这个平台是<b>停用</b>的：开启后才会发布模型、参与路由。</div>';
  }
  // 后端说它能转发（usable=true）就没什么可报的；否则一定有原因，最次是「没有可用凭据」。
  // 注意不能用 unusableShortReason(p) 的空值当「没问题」—— 它只覆盖协议/占位符/仅验活
  // 三条，缺凭据那条它返回 null，照它写会把最常见的「未接入」静默吞掉。
  if (p.usable !== false) return '';
  const why = unusableShortReason(p) || '没有可用凭据';
  const FIX = {
    '上游地址是占位符': '把 providers/ 里这个平台的 base_url 换成真实的 OpenAI 兼容地址（或换一个自定义 API），然后重启服务。',
    '仅验活·不可对话': '这个平台只用于登录验活（chat_supported = false），不参与对话转发；要对话请改用其他平台。',
    '没有可用凭据': '在下面的输入框里保存 Key / 完成登录，凭据通过后这里会自动恢复。',
  };
  // 协议那档要带上平台自己的 format，所以单独拼，不进固定表。
  const fix = FIX[why] || ('这个平台的协议不是 openai，转发层只做 OpenAI 透传（当前 format = '
    + (p.format || '?') + '）：改 format，或改用一个 OpenAI 兼容的地址。');
  const href = '/v1/models?platform=' + encodeURIComponent(p.id);
  return '<div class="note" style="border-color:#5a4a2a">这个平台现在<b>不发布模型</b>：' + esc(why) + '。<br>'
    + esc(fix) + '<br><a href="' + att(href) + '" target="_blank" rel="noopener">看看它在 /v1/models 里的样子 →</a></div>';
}
// 分组判定的例外表：前缀规则（api- / free-）覆盖不到的少数平台。
// 与 LOGIN_UI 一样，把「代码里散落的 id 判断」收成一处。
const GROUP_OVERRIDE = {
  official: ['apk-coze', 'kimi-code', 'codex'],
  free: ['anon-zen', 'kilo', 'pollinations'],
};

// 「哪些平台走哪种登录界面」的**唯一一张表**。
//
// 之前这些判断散在 srcDrawer 开头 17 行 `p.id === '...'` 里（25 个 id 硬编码），
// 后端 providers/*.toml 改了 id、或者新增一个同族平台，前端就得跟着改代码，而且
// 漏改的症状是「抽屉画成手填 Key」。收成一张表后：新增平台要么进表，要么落进
// 「按 auth 推导」的默认分支，不会再出现「代码里少写一个 id」这种错。
//
// 注意：这里表达的是**界面形态**（画哪个表单/按钮），不是协议能力 ——
// 后者后端已经通过 login_methods / auth / upstream_kind 下发了。
const LOGIN_UI = {
  // codebuddy 在 auth_login 里有独立设备授权实现（codebuddy.cn + CLI），
  // 但 spec 侧由 wb-gateway 承载多域，故此处只列 id 供老账号复用同一张抽屉。
  wb: ['wb-gateway', 'wb-gateway-intl', 'codebuddy'],
  minimax: ['minimax'],
  kimi: ['sub-kimi', 'sub-kimi-ai', 'sub-grok'],
  cline: ['cline'],
  meta: ['sub-meta'],
  pkce: ['sub-codex', 'sub-claude', 'sub-antigravity', 'sub-devin'],
  qoder: ['apk-qoder', 'web-qoder'],
  trae: ['apk-trae', 'web-trae'],
  codearts: ['codearts'],
  // web-lobster 的 spec 已并进 lobster（同一 OAuth 链路、同一个 @lobster 占位地址），
  // 保留 id 只为兼容老备份里残留的账号行 —— 没有对应 spec 时不会渲染出卡片。
  lobster: ['lobster', 'web-lobster'],
  pasteToken: ['web-deepseek'],
  pasteQwen: ['web-qwen'],
  pasteMimo: ['mimo'],
  pasteKiro: ['sub-kiro'],
  pasteCoze: ['apk-coze'],
  pasteAntigravity: ['apk-antigravity'],
  // 拖拽式扫码（后端 relay 里有出码+轮询实现）
  qrPaste: ['apk-raccoon', 'apk-doubao', 'apk-kuku', 'apk-loomy'],
};

// 平台 id → 界面族（同一个 id 只可能属于一族；重复登记按先出现的算）
const LOGIN_UI_OF = (() => {
  const m = {};
  for (const [k, ids] of Object.entries(LOGIN_UI)) ids.forEach((id) => { if (!m[id]) m[id] = k; });
  return m;
})();

function srcDrawer(p) {
  const ui = LOGIN_UI_OF[p.id] || '';
  const isWb = ui === 'wb';
  const isMinimax = ui === 'minimax';
  const isKimi = ui === 'kimi';
  const isCline = ui === 'cline';
  const isMeta = ui === 'meta';
  const isPkce = ui === 'pkce';
  const isQoder = ui === 'qoder';
  const isTrae = ui === 'trae';
  const isCodearts = ui === 'codearts';
  const isLobster = ui === 'lobster';
  const isPasteToken = ui === 'pasteToken';
  const isPasteQwen = ui === 'pasteQwen';
  const isPasteMimo = ui === 'pasteMimo';
  const isPasteKiro = ui === 'pasteKiro';
  const isPasteCoze = ui === 'pasteCoze';
  const isPasteAntigravity = ui === 'pasteAntigravity';
  const isSms = p.auth === 'sms_code';
  // 多路登录：toml 的 login_methods 列出多种方式（例如 Loomy = sms/pwd/qr）。后端下发 p.login_methods，
  // 没配就退回按 auth 推导。用它决定画哪些选项卡 —— 而不是在前端写死平台 id。
  const LM = (Array.isArray(p.login_methods) && p.login_methods.length)
    ? p.login_methods
    : ({ qr_scan: ['qr'], sms_code: ['sms'], o_auth_device: ['device'], api_key: ['paste'], cookie: ['paste'], client_file: ['paste'], anon: [] }[p.auth] || []);
  const multiLogin = LM.filter((m) => m === 'sms' || m === 'pwd' || m === 'qr').length >= 2;
  const isKey = p.auth === 'api_key';
  const isAnon = p.auth === 'anon';
  const loginUrl = safeUrl(p.login_url);
  let body = '';
  if (isSms || multiLogin) {
    // ★ 多路登录：同一个平台有三条路（短信 / 密码 / 扫码），原来是「短信主路 + 密码 + 折叠扫码」
    //   堆在一起，用户得自己读三段说明才知道选哪条。改成上面一排选项卡，一次只显示一条路，
    //   下面说明当前这条路的要点。点过哪条就记住（window.__LOGIN_TAB），下次进来还是那条。
    const ALL_TABS = {
      sms: { k: 'sms', name: '短信验证码', hint: '手机号收验证码，拿到 session 约 14 天有效，过期重走一次' },
      pwd: { k: 'pwd', name: '账号密码', hint: '不用等短信；密码登录不需要人读码，可以用来做到期续期' },
      qr: { k: 'qr', name: '微信扫码', hint: '要求这个微信已经绑定讯飞账号，手机确认后自动入库' },
    };
    const TABS = LM.filter((m) => ALL_TABS[m]).map((m) => ALL_TABS[m]);
    const cur = TABS.some((t) => t.k === window.__LOGIN_TAB) ? window.__LOGIN_TAB : (TABS[0] || { k: 'sms' }).k;
    const head = TABS.map((t) =>
      '<button class="btn sm ' + (t.k === cur ? 'pri' : 'gh') + '" onclick="loginTab(\'' + t.k + '\')">' + t.name + '</button>'
    ).join(' ');
    let pane = '';
    if (cur === 'sms') {
      pane = '<div class="row">'
        + '<input class="q mono" id="smsPhone" placeholder="11 位手机号，例如 13800138000" value="' + att(window.__SMS_PHONE || '') + '">'
        + '<button class="btn" onclick="smsSend(\'' + att(p.id) + '\')">发送验证码</button></div>'
        + '<div class="row"><input class="q mono" id="smsCode" placeholder="短信里的验证码">'
        + '<button class="btn pri" onclick="smsVerify(\'' + att(p.id) + '\')">验证并登录</button></div>';
    } else if (cur === 'pwd') {
      pane = '<div class="row">'
        + '<input class="q mono" id="pwdId" placeholder="账号（手机号）" value="' + att(window.__SMS_PHONE || '') + '">'
        + '<input class="q mono" type="password" id="pwdPw" placeholder="密码">'
        + '<button class="btn pri" onclick="pwdLogin(\'' + att(p.id) + '\')">登录</button></div>';
    } else {
      pane = '<div class="row"><button class="btn pri" onclick="authStart(\'' + att(p.id) + '\')">生成微信二维码</button>'
        + '<button class="btn" onclick="authPoll(\'' + att(p.id) + '\')">查询扫码状态</button></div>';
    }
    const hint = (TABS.find((t) => t.k === cur) || TABS[0]).hint;
    body = '<div class="row">' + head + '</div>'
      + '<div class="note">' + esc(hint) + '（<b>讯飞账号</b>）。三条路拿到的 session 等价，随便挑一条。</div>'
      + pane + '<div id="authBox"></div>';
  } else if (isAnon) {
    body = `<div class="note">免密平台：无需凭据，直接可用。点下面的「测试连通」看是否可转发。</div>
      <button class="btn pri" onclick="testAnon('${att(p.id)}')">测试连通</button>`;
  } else if (isWb || isMinimax || isKimi || isCline || isMeta || isQoder) {
    // ★ WorkBuddy 有两个站：国内（workbuddy.cn）与国际（workbuddy.ai）。文案必须点名
    //   当前是哪一个 —— 两站账号不通用，用户在错的那个站登录会拿到「登录成功但没有可用模型」。
    const wbName = p.id === 'wb-gateway' ? 'WorkBuddy 国内站（workbuddy.cn）'
      : p.id === 'wb-gateway-intl' ? 'WorkBuddy 国际站（workbuddy.ai）'
      : p.id === 'codebuddy' ? 'CodeBuddy（codebuddy.cn）' : '';
    // 用户直接用账号登录：不需要填 Key、不需要装客户端、不需要任何本地反代程序。
    // 授权在服务端发起普通 HTTPS 请求完成，因此部署到服务器后照样可用（跳转型登录
    // 那些「本地端口收回调」的平台才受部署位置限制，WorkBuddy 不属于那一类）。
    const wbNote = p.id === 'wb-gateway' || p.id === 'wb-gateway-intl'
      ? `${wbName} 账号登录：点「开始登录」→ 打开授权页用你的 WorkBuddy 账号登录 → 面板自动取回凭据入库。
        <br>全程是服务端发出的普通 HTTPS 请求，<b>不需要装客户端、不需要本地反代、不需要填任何 Key</b>，部署在服务器上同样可用。`
      : `${wbName} 设备授权：点「开始登录」生成授权链接，在浏览器完成授权后面板自动拿 token 入库。`;
    body = `<div class="note">${isCline ? 'Cline 设备码（WorkOS OAuth）：' : isWb ? wbNote : isMinimax ? 'MiniMax Code 设备码（客户端订阅，非开放平台 Key）：' : isMeta ? 'Meta Muse 设备码（登录后自动铸造 API Key）：' : isQoder ? 'Qoder 设备授权（免客户端，登录后自动拿 dt- Token）：' : isKimi && p.id === 'sub-grok' ? 'Grok 设备码（RFC 8628，需代理访问）：' : 'Kimi Code 设备码（RFC 8628）：'}点「开始登录」生成授权链接，在浏览器完成授权后面板自动拿 token 入库。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${att(p.id)}')">开始登录</button>
        <button class="btn" onclick="authPoll('${att(p.id)}')">查询授权状态</button>
      </div>
      <div id="authBox"></div>${
        isWb ? `<div class="faint">国内站账号不能用于国际站，反之亦然；两站模型族相同（hy4-preview / deepseek-v4.x / glm-5.x / kimi / minimax）。</div>` : ''
      }`;
  } else if (isPkce || isTrae || isCodearts) {
    const hint = isTrae
      ? 'Trae 跳转登录（字节 Trae 账号，本地 18080 收 refreshToken；服务器部署直接把地址栏 URL 粘回来）'
      : isCodearts
      ? '华为云 CodeArts 跳转登录（华为云账号授权 → 自动换临时 AK/SK 入库；服务器部署把地址栏 URL 粘回来）'
      : p.id === 'sub-antigravity'
      ? 'Antigravity 跳转登录（Google 账号 + 本地 51121 回调）'
      : p.id === 'sub-devin'
      ? 'Devin 跳转登录（headless PKCE，官方 CLI 无回调端口；服务器部署直接把地址栏 URL 粘回来）'
      : p.id === 'sub-codex' ? 'Codex 跳转登录（PKCE + 本地 1455 回调）' : 'Claude Code 跳转登录（PKCE + 本地 54545 回调）';
    body = `<div class="note">${hint}：点「打开授权页」在浏览器完成登录（跳转登录，非扫码）。面板会自动捕获回调并换取 token —— 授权完成后你可以直接关掉浏览器那个页面回到这里，**不用再点任何按钮**；若部署在服务器，把浏览器跳转后地址栏的完整 URL 粘贴到下方输入框。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${att(p.id)}')">打开授权页</button>
        <button class="btn" onclick="authPoll('${att(p.id)}')">立即查询一次</button>
      </div>
      <div id="pollBox"></div>
      <div class="row">
        <input class="q mono" id="cbUrl" placeholder="${isTrae ? '粘贴回调地址（含 ?refreshToken=…）' : isCodearts ? '粘贴回调地址（含 ?secret=…）' : '粘贴回调地址（含 ?code= 与 &state=）'}…">
        <button class="btn" onclick="authCallback('${att(p.id)}')">提交回调</button>
      </div>
      <div id="authBox"></div>
      ${p.id === 'sub-antigravity' ? `<details class="src-paste"><summary>已有 OAuth access token？直接粘贴</summary>
        <div class="note">从 Antigravity / gcloud 拿到的 access token（<span class="mono">ya29.…</span>）。项目号默认由面板调 loadCodeAssist 自动获取；取不到时粘 JSON：<span class="mono">{"access_token":"ya29.…","project_id":"…"}</span></div>
        <div class="row"><input class="q mono" id="keyInput" placeholder='粘贴 access token，或 {"access_token":"…","project_id":"…"}'><button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button></div>
      </details>` : ''}
      ${isTrae ? `<details class="src-paste"><summary>已有 accessToken？直接粘贴（免跳转）</summary>
        <div class="note">从 Trae 客户端 <span class="mono">storage.json</span> 的 <span class="mono">auth.accessToken</span> 取 Cloud-IDE-JWT。带上 refreshToken 后，面板会在过期前自动续期。<br>推荐粘 JSON：<span class="mono">{"accessToken":"…","refreshToken":"…","uid":"…","machineId":"…","deviceId":"…"}</span>；只粘 token 串也能用。</div>
        <div class="row"><input class="q mono" id="keyInput" placeholder='粘贴 accessToken，或 {"accessToken":"…","refreshToken":"…"}'><button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button></div>
      </details>` : ''}
      ${p.id === 'sub-devin' ? `<details class="src-paste"><summary>已有 API token？直接粘贴（跳过跳转）</summary>
        <div class="note">Devin 是<b>会话型 Agent</b>（没有 chat/completions）：面板收到请求会先 <span class="mono">POST /v1/sessions</span> 建会话，轮询到 <span class="mono">finished</span> 再把最后一条 devin_message 当回复返回。模型名固定 <span class="mono">devin</span>，调用写 <span class="mono">devin@sub-devin</span>。<br>⚠ 每次请求都会新建一个 Devin 会话并消耗额度，别当普通聊天模型刷。token 取法：<span class="mono">app.devin.ai → Settings → API Keys</span>，也可粘 <span class="mono">{"token":"…","org_id":"…"}</span>。</div>
        <div class="row"><input class="q mono" id="keyInput" placeholder='粘贴 Devin API token，或 {"token":"…","org_id":"…"}'><button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button></div>
      </details>` : ''}`;
  } else if (isLobster) {
    body = `<div class="note">Lobster 跳转登录（有道龙虾授权页 → 本地随机端口回调换码）：点「打开授权页」在浏览器完成登录（跳转登录，非扫码）。面板自动捕获回调换 token 入库；若部署在服务器，把浏览器跳转后地址栏的完整 URL 粘贴到下方输入框。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${att(p.id)}')">打开授权页</button>
        <button class="btn" onclick="authPoll('${att(p.id)}')">查询授权状态</button>
      </div>
      <div class="row">
        <input class="q mono" id="cbUrl" placeholder="粘贴回调地址（含 ?code= 与 &state=）…">
        <button class="btn" onclick="authCallback('${att(p.id)}')">提交回调</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isPasteToken) {
    body = `<div class="note">DeepSeek 网页版（<b>原生网页协议</b>，面板自己建会话、内置 wasm 解 POW，不需要 node / 外部 exe）：在 <a href="https://chat.deepseek.com" target="_blank" rel="noopener">chat.deepseek.com</a> 登录后 → F12 → Application → <b>Local Storage</b> 复制 <span class="mono">token</span> 粘下面。<br>首次请求会多花 1~2 秒解 POW（每张挑战只解一次），之后就正常速度。</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder="粘贴 token…">
        <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isPasteQwen) {
    body = `<div class="note">Qwen AI 网页版登录态（<b>无 OpenAI 兼容端点</b>，面板后台自动建会话再走 /api/v2/chat/completions，思考流映射成 reasoning_content）：在 <a href="${esc(loginUrl || 'https://chat.qwen.ai/')}" target="_blank" rel="noopener">chat.qwen.ai</a> 登录后，
      <br>① DevTools → Application → <b>Local Storage</b> 复制 <span class="mono">token</span>；
      <br>② Network 里任意一条请求复制<b>整段 Cookie</b>（含 cnaui / aui / sca / xlly_s 等）。
      <br>两段一起粘，推荐 JSON：<span class="mono">{"token":"…","cookies":"…"}</span>；也可只粘整段 Cookie（只要里面带 token=…）。</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder='粘贴 {"token":"…","cookies":"…"} 或整段 Cookie…'>
        <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isPasteMimo) {
    // 小米 MiMo 网页版（AISTUDIO 私有协议）：三个 Cookie 缺一不可，缺哪个上游都当未登录
    body = `<div class="note">小米 MiMo 网页版（<b>无 OpenAI 兼容端点</b>，后台先建会话再走 /open-apis/bot/chat，思考内容以 &lt;think&gt; 标签夹在正文里会被剥掉）：在 <a href="${esc(loginUrl)}" target="_blank" rel="noopener">aistudio.xiaomimimo.com</a> 登录后，
      <br>F12 → Network 任选一条请求 → 复制<b>整段 Cookie</b>（必须含 <span class="mono">serviceToken</span> / <span class="mono">userId</span> / <span class="mono">xiaomichatbot_ph</span> 三条）。
      <br>也可只粘 JSON：<span class="mono">{"serviceToken":"…","userId":"…","xiaomichatbot_ph":"…"}</span>。</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder='粘贴整段 Cookie，或 {"serviceToken":"…","userId":"…","xiaomichatbot_ph":"…"}…'>
        <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isPasteKiro) {
    // Kiro（AWS CodeWhisperer / Amazon Q）：凭据只能从客户端导出，官方没给第三方扫码/OAuth
    body = `<div class="note">Kiro（AWS CodeWhisperer / Amazon Q Developer）凭据导入：面板用 <span class="mono">Authorization: Bearer …</span> 调 <span class="mono">generateAssistantResponse</span>，并解析 AWS Event Stream 二进制帧（思考内容映射成 reasoning_content）。<br>凭据来源：登录 <a href="${esc(loginUrl || 'https://kiro.dev/')}" target="_blank" rel="noopener">kiro.dev</a> 客户端后从本地凭据导出 JSON，面板会用 refreshToken 自动续期。<br>推荐粘 JSON：<span class="mono">{"accessToken":"…","refreshToken":"…","region":"us-east-1","authMethod":"social","profileArn":"arn:aws:codewhisperer:…"}</span>。<br>Builder ID / IdC 账号再带 <span class="mono">clientId</span> 与 <span class="mono">clientSecret</span>。<br><b>profileArn 必填</b>（AWS 在鉴权前就要求它）；缺失时面板会用 accessToken 调 ListAvailableProfiles 自动取，取不到会直接说明原因。</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder="粘贴 Kiro 凭据 JSON…">
        <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isPasteCoze) {
    // 扣子 Coze：开放平台 v3 chat（upstream_kind=coze），对话对象是用户自己的 bot
    body = `<div class="note">扣子 Coze 开放平台（<b>原生 v3 chat 协议</b>，<span class="mono">/v1/chat/completions</span> 在扣子并不存在）：对话对象是你自己发布的 <b>bot</b>，所以<b>模型名就是 bot_id</b> —— 调用时把模型位置写成 <span class="mono">&lt;bot_id&gt;@apk-coze</span>（也可写 <span class="mono">bot:&lt;bot_id&gt;</span>）。<br>凭据：扣子「个人访问令牌」<span class="mono">pat_…</span>（开放平台 → 授权 → 个人访问令牌）。<br>bot 需先在扣子里「发布 → 发布为 API 服务」才能被调用；国际站请把 <span class="mono">providers/apk-coze.toml</span> 的 base_url 改成 <span class="mono">https://api.coze.com</span>。</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder='粘贴 PAT（pat_…），或 {"token":"pat_…","bot_id":"7400000000000000000","user_id":"u-1"}'>
        <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isPasteAntigravity) {
    // Antigravity / Google Cloud Code：走 Gemini 私有协议（upstream_kind=cloudcode），不是 OpenAI 兼容端点
    body = `<div class="note">Google Antigravity / Cloud Code（<b>Gemini 私有协议</b>，面板后台走 /v1internal:streamGenerateContent，思考流映射成 reasoning_content）：在 Antigravity 客户端或 gcloud 登录后取 OAuth <b>access token</b>（<span class="mono">ya29.…</span>）粘贴到下面。
      <br>项目号（cloudaicompanionProject）默认由面板调 loadCodeAssist 自动获取；若账号返回空，请改粘 JSON：<span class="mono">{"access_token":"ya29.…","project_id":"…"}</span>。</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder='粘贴 access token，或 {"access_token":"…","project_id":"…"}…'>
        <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else if (ui === 'qrPaste' && p.id === 'apk-raccoon') {
    // 小浣熊（商汤）微信小程序扫码：code 由服务端自造（32 位 hex），二维码内容 = xiaohuanxiong.com/login/mp?code=…
    // 扫码后用同一个 code 轮询 login_with_qrcode_code 换 Bearer access_token（约 3 小时有效）。
    body = `<div class="note">小浣熊（商汤）<b>微信扫码登录</b>（纯 HTTP，免粘贴 Cookie）：点「开始登录」生成二维码，用<b>微信</b>扫一扫（进入「商汤小浣熊」小程序）并在手机上确认，面板自动换取 access_token 入账号池。token 约 3 小时有效，过期后重新扫码即可。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${att(p.id)}')">开始登录</button>
        <button class="btn" onclick="authPoll('${att(p.id)}')">查询扫码状态</button>
      </div>
      <div id="authBox"></div>
      <details class="src-paste">
        <summary class="faint" style="cursor:pointer">也可以不用扫码：手动粘贴 access_token（登录 xiaohuanxiong.com 后，F12 → Network 任选一条请求，复制请求头 Authorization: Bearer 后面那串）</summary>
        <div class="row">
          <input class="q mono" id="keyInput" placeholder="粘贴 access_token…">
          <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
        </div>
      </details>`;
  } else if (isKey) {
    body = `<div class="note">${p.id.startsWith('sub-') ? '订阅平台：' : '填官方 API Key：'}把 Key 粘贴到下面，保存即入库（自动作为 Bearer 头转发）。${loginUrl ? `<br>还没有 Key？<a href="${esc(loginUrl)}" target="_blank" rel="noopener">去官网申请/登录 →</a>` : ''}</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder="粘贴 API Key / token 或 Cookie…">
        <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else if (ui === 'qrPaste' && p.id === 'apk-doubao') {
    // ★ 可达性：豆包 auth=qr_scan（不是 api_key/cookie），所以上面的 isKey 分支不会截走；
    //   这条分支与 kuku/loomy 都是「有扫码实现」的平台，靠 toml 的 auth=qr_scan 标记能力。
    // 豆包扫码登录（doubao2api qr_login 的纯 HTTP 实现）：start 返回二维码 → 自动轮询收整段 Cookie
    body = `<div class="note">豆包<b>扫码登录</b>（纯 HTTP，免粘贴 Cookie）：点「开始登录」生成二维码，用<b>豆包 / 抖音 App</b> 扫码并在手机上确认，面板自动收整段 Cookie 入库（直连 samantha 端点）。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${att(p.id)}')">开始登录</button>
        <button class="btn" onclick="authPoll('${att(p.id)}')">查询扫码状态</button>
      </div>
      <div id="authBox"></div>
      <details class="src-paste">
        <summary class="faint" style="cursor:pointer">也可以不用扫码：手动粘贴 Cookie（登录 www.doubao.com 后 F12 → Application → Cookies 复制整段，含 sessionid=…）</summary>
        <div class="row">
          <input class="q mono" id="keyInput" placeholder="粘贴整段 Cookie（含 sessionid=…）…">
          <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
        </div>
      </details>`;
  } else if (ui === 'qrPaste' && p.id === 'apk-kuku') {
    // 库库 AI 扫码登录（kuku2api passport 的纯 HTTP 实现）：start 出二维码 → 轮询拿 BDUSS/PTOKEN → 换 STOKEN
    body = `<div class="note">库库 AI<b>扫码登录</b>（纯 HTTP，免粘贴 Cookie）：点「开始登录」生成二维码，用<b>百度 App / 百度网盘 App</b> 扫一扫并在手机上确认，面板自动完成 BDUSS/PTOKEN → STOKEN 交换并入账号池。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${att(p.id)}')">开始登录</button>
        <button class="btn" onclick="authPoll('${att(p.id)}')">查询扫码状态</button>
      </div>
      <div id="authBox"></div>
      <details class="src-paste">
        <summary class="faint" style="cursor:pointer">也可以不用扫码：手动粘贴 Cookie（登录 kuku.baidu.com 后 F12 → Application → Cookies 复制整段，必须含 BDUSS=…; STOKEN=…）</summary>
        <div class="row">
          <input class="q mono" id="keyInput" placeholder="粘贴整段 Cookie（含 BDUSS=…; STOKEN=…）…">
          <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
        </div>
      </details>`;
  } else if (ui === 'qrPaste' && p.id === 'apk-loomy') {
    // Loomy（讯飞）微信扫码登录：open.weixin qrconnect → 长轮询 wx_code → 讯飞 bind/skip → session
    // 一手来源 _refs/Ebony-Vinyl-dsh-our-free-model/.../loomy-wechat.ts
    body = `<div class="note">Loomy（讯飞）<b>微信扫码登录</b>（纯 HTTP，免粘贴 session）：点「开始登录」生成二维码，用<b>微信</b>扫一扫并在手机上确认，面板自动换取 14 天有效的 session 入账号池。<br>若这个微信还没绑定过讯飞账号，微信侧会要求先绑定手机号（可先去 <a href="https://loomy.xunfei.cn/login" target="_blank" rel="noopener">loomy.xunfei.cn</a> 用手机号登录并绑定微信，再回来扫码）。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${att(p.id)}')">开始登录</button>
        <button class="btn" onclick="authPoll('${att(p.id)}')">查询扫码状态</button>
      </div>
      <div id="authBox"></div>
      <details class="src-paste">
        <summary class="faint" style="cursor:pointer">也可以不用扫码：手动粘贴 session（loomy 客户端/网页登录后，请求头 Authorization 里的 32 位字符串）</summary>
        <div class="row">
          <input class="q mono" id="keyInput" placeholder="粘贴 32 位 session…">
          <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
        </div>
      </details>`;
  } else {
    // cookie 类：跳转登录 + 手动粘贴 Cookie（纯 HTTP，禁扫码）
    const COOKIE_HINT = {
      'apk-doubao': '登录 <b>www.doubao.com</b> 后，F12 → Application → Cookies，复制整段 Cookie（含 sessionid 等）。面板走豆包旧版 samantha 端点，纯 Cookie 直连，无需扫码。',
      'apk-yuanbao': '登录 <b>yuanbao.tencent.com</b> 后，F12 → Application → Cookies，复制整段 Cookie，<b>必须包含 hy_user 和 hy_token</b> 两条，否则验活失败。',
      'apk-kuku': '登录 <b>kuku.baidu.com</b> 后，F12 → Application → Cookies，复制整段 Cookie，<b>必须包含 BDUSS 和 STOKEN</b>（httpOnly，在 Cookies 列表里可见）。',
      'apk-raccoon': '登录小浣熊网页版后，F12 → Application → Cookies 复制整段 Cookie。',
    };
    const COOKIE_PH = {
      'apk-yuanbao': '粘贴整段 Cookie（含 hy_user=…; hy_token=…）…',
      'apk-kuku': '粘贴整段 Cookie（含 BDUSS=…; STOKEN=…）…',
      'apk-doubao': '粘贴整段 Cookie（含 sessionid=…）…',
      'apk-raccoon': '粘贴整段 Cookie…',
    };
    if (p.id === 'apk-metaso') {
      body = `<div class="note oknote">秘塔<b>免登录即可用</b>（匿名约 13 次 / IP / 窗口，搜索带引用来源）：无需粘贴任何凭据，直接在对话里选 metaso 模型即可。</div>
        <div class="note">想提高额度（约 500/天）：登录 <a href="https://metaso.cn/" target="_blank" rel="noopener">metaso.cn</a> → F12 → Application → Cookies，复制 <b>uid</b> 和 <b>sid</b>，拼成 <span class="mono">uid-sid</span> 粘到下面（可选）。遇人机验证可在「设置 → 代理」配置代理。</div>
        <div class="row">
          <input class="q mono" id="keyInput" placeholder="可选：粘贴 uid-sid（留空即匿名，无需保存）…">
          <button class="btn pri" onclick="authSave('${att(p.id)}', true)">保存登录态</button>
        </div>
        <div id="authBox"></div>`;
    } else if (p.chat_supported === false) {
      body = `<div class="note errnote">该平台<b>暂不支持对话反代</b>（仅可登录/验活）：全网暂无公开的纯 HTTP 自由对话逆向，官方仅有付费 API 或办公文档技能。下方仅保存登录态用于验活。</div>
        <div class="note">${loginUrl ? `① <a href="${esc(loginUrl)}" target="_blank" rel="noopener">打开登录页 →</a> 在自己的浏览器登录　` : ''}② F12 → Application → Cookies 复制整段 Cookie 粘到下面。</div>
        <div class="row">
          <input class="q mono" id="keyInput" placeholder="粘贴整段 Cookie…">
          <button class="btn pri" onclick="authSave('${att(p.id)}')">保存登录态</button>
        </div>
        <div id="authBox"></div>`;
    } else {
      // 只有「没有专门分支」的 cookie 平台才落到这里；不写「面板不支持扫码」（豆包 /
      // 库库 / 小浣熊 / Loomy 都有真扫码分支），免得一句话否定掉四个平台的扫码能力。
      const hint = COOKIE_HINT[p.id] || '登录后 F12 → Application → Cookies 复制整段 Cookie。';
      body = `<div class="note">${loginUrl ? `① <a href="${esc(loginUrl)}" target="_blank" rel="noopener">打开登录页 →</a> 在自己的浏览器完成登录　` : ''}② ${hint}　③ 粘贴到下面保存入库。</div>
        <div class="row">
          <input class="q mono" id="keyInput" placeholder="${COOKIE_PH[p.id] || '粘贴整段 Cookie / 凭据…'}">
          <button class="btn pri" onclick="authSave('${att(p.id)}')">保存凭据</button>
        </div>
        <div id="authBox"></div>`;
    }
  }
  return modalShell(
    `${esc(p.name)}<span class="faint mono"> ${esc(p.id)}</span>`,
    `
    <div class="modal-bd">
      ${p.accounts_n > 0 ? `<div class="note oknote">已接入 ${p.accounts_n} 个账号（账号页可查看掩码 / 停用 / 删除）</div>` : ''}
      ${unusableBanner(p)}
      ${srcModelsBlock(p)}
      ${body}
    </div>
`,
    // 抽屉形态：点遮罩或 ✕ 都要清掉 SRC_EXPAND 再重渲染（直接 remove 会留下展开态）
    { drawer: true, id: 'srcDrawer', onMask: 'SRC_EXPAND=null;render()', close: 'SRC_EXPAND=null;render()' },
  );
}

// ── 小浣熊二维码编码器 ──
// 服务端（Rust）没引二维码库，登录接口只回 qr_text（小程序登录页 URL），由前端画成内联 SVG。
// 下面这段是参考实现 _refs/.../channel-pack/src/raccoon-qr.ts 用 stripTypeScriptTypes 机器转译的产物：
// 零依赖、byte 模式、纠错等级 M、版本 1-10（容量上限 213 字节）。**不要手改**，要改就改参考实现再转译。
var RACCOON_QR=(function(){
const DATA_CODEWORDS = [
    0,
    16,
    28,
    44,
    64,
    86,
    108,
    124,
    154,
    182,
    216
];
const EC_BLOCKS_M = [
    undefined,
    {
        ecPerBlock: 10,
        groups: [
            [
                1,
                16
            ]
        ]
    },
    {
        ecPerBlock: 16,
        groups: [
            [
                1,
                28
            ]
        ]
    },
    {
        ecPerBlock: 26,
        groups: [
            [
                1,
                44
            ]
        ]
    },
    {
        ecPerBlock: 18,
        groups: [
            [
                2,
                32
            ]
        ]
    },
    {
        ecPerBlock: 24,
        groups: [
            [
                2,
                43
            ]
        ]
    },
    {
        ecPerBlock: 16,
        groups: [
            [
                4,
                27
            ]
        ]
    },
    {
        ecPerBlock: 18,
        groups: [
            [
                4,
                31
            ]
        ]
    },
    {
        ecPerBlock: 22,
        groups: [
            [
                2,
                38
            ],
            [
                2,
                39
            ]
        ]
    },
    {
        ecPerBlock: 22,
        groups: [
            [
                3,
                36
            ],
            [
                2,
                37
            ]
        ]
    },
    {
        ecPerBlock: 26,
        groups: [
            [
                4,
                43
            ],
            [
                1,
                44
            ]
        ]
    }
];
const GF_EXP = new Uint8Array(512);
const GF_LOG = new Uint8Array(256);
{
    let x = 1;
    for(let i = 0; i < 255; i++){
        GF_EXP[i] = x;
        GF_LOG[x] = i;
        x <<= 1;
        if ((x & 0x100) !== 0) x ^= 0x11d;
    }
    for(let i = 255; i < 512; i++)GF_EXP[i] = GF_EXP[i - 255] ?? 0;
}function gfMul(a, b) {
    if (a === 0 || b === 0) return 0;
    return GF_EXP[(GF_LOG[a] ?? 0) + (GF_LOG[b] ?? 0)] ?? 0;
}
function polyMul(a, b) {
    const result = new Array(a.length + b.length - 1).fill(0);
    for(let i = 0; i < a.length; i++){
        for(let j = 0; j < b.length; j++){
            result[i + j] ^= gfMul(a[i] ?? 0, b[j] ?? 0);
        }
    }
    return result;
}
function rsGeneratorPoly(degree) {
    let poly = [
        1
    ];
    for(let i = 0; i < degree; i++){
        poly = polyMul(poly, [
            1,
            GF_EXP[i] ?? 0
        ]);
    }
    return poly;
}
function rsEncode(data, ecCount) {
    const gen = rsGeneratorPoly(ecCount);
    const buf = [
        ...data,
        ...new Array(ecCount).fill(0)
    ];
    for(let i = 0; i < data.length; i++){
        const coef = buf[i] ?? 0;
        if (coef === 0) continue;
        for(let j = 0; j < gen.length; j++){
            buf[i + j] ^= gfMul(gen[j] ?? 0, coef);
        }
    }
    return buf.slice(data.length);
}
function pickVersion(byteLength) {
    for(let version = 1; version <= 10; version++){
        const capacityBits = (DATA_CODEWORDS[version] ?? 0) * 8;
        const overheadBits = 4 + (version <= 9 ? 8 : 16);
        if (overheadBits + byteLength * 8 <= capacityBits) return version;
    }
    return undefined;
}
function buildCodewords(bytes, version) {
    const blocks = EC_BLOCKS_M[version];
    if (blocks === undefined) throw new Error(`raccoon: 不支持的二维码版本 ${version}`);
    const totalDataCodewords = DATA_CODEWORDS[version] ?? 0;
    const capacityBits = totalDataCodewords * 8;
    const bits = [];
    const pushBits = (value, length)=>{
        for(let i = length - 1; i >= 0; i--)bits.push(value >> i & 1);
    };
    pushBits(0b0100, 4);
    pushBits(bytes.length, version <= 9 ? 8 : 16);
    for (const byte of bytes)pushBits(byte, 8);
    const terminator = Math.min(4, capacityBits - bits.length);
    pushBits(0, terminator);
    while(bits.length % 8 !== 0)bits.push(0);
    const PAD_BYTES = [
        0xec,
        0x11
    ];
    for(let i = 0; bits.length < capacityBits; i++){
        pushBits(PAD_BYTES[i % 2] ?? 0, 8);
    }
    const dataCodewords = [];
    for(let i = 0; i < bits.length; i += 8){
        let byte = 0;
        for(let j = 0; j < 8; j++)byte = byte << 1 | (bits[i + j] ?? 0);
        dataCodewords.push(byte);
    }
    const dataBlocks = [];
    const ecBlocks = [];
    let offset = 0;
    for (const [count, dataPerBlock] of blocks.groups){
        for(let b = 0; b < count; b++){
            const block = dataCodewords.slice(offset, offset + dataPerBlock);
            offset += dataPerBlock;
            dataBlocks.push(block);
            ecBlocks.push(rsEncode(block, blocks.ecPerBlock));
        }
    }
    const result = [];
    const maxDataLen = Math.max(...dataBlocks.map((b)=>b.length));
    for(let i = 0; i < maxDataLen; i++){
        for (const block of dataBlocks){
            if (i < block.length) result.push(block[i] ?? 0);
        }
    }
    for(let i = 0; i < blocks.ecPerBlock; i++){
        for (const block of ecBlocks){
            result.push(block[i] ?? 0);
        }
    }
    return result;
}
function alignmentPositions(version, size) {
    if (version === 1) return [];
    const numAlign = Math.floor(version / 7) + 2;
    const step = Math.ceil((version * 4 + 4) / (numAlign * 2 - 2)) * 2;
    const result = [
        6
    ];
    for(let pos = size - 7; result.length < numAlign; pos -= step){
        result.splice(1, 0, pos);
    }
    return result;
}
function drawFinderPattern(modules, isFunction, size, x, y) {
    for(let dy = -4; dy <= 4; dy++){
        for(let dx = -4; dx <= 4; dx++){
            const xx = x + dx;
            const yy = y + dy;
            if (xx < 0 || xx >= size || yy < 0 || yy >= size) continue;
            const dist = Math.max(Math.abs(dx), Math.abs(dy));
            const dark = dist !== 2 && dist !== 4;
            modules[yy][xx] = dark;
            isFunction[yy][xx] = true;
        }
    }
}
function drawAlignmentPattern(modules, isFunction, x, y) {
    for(let dy = -2; dy <= 2; dy++){
        for(let dx = -2; dx <= 2; dx++){
            const dark = Math.max(Math.abs(dx), Math.abs(dy)) !== 1;
            modules[y + dy][x + dx] = dark;
            isFunction[y + dy][x + dx] = true;
        }
    }
}
function drawFunctionPatterns(modules, isFunction, size, version) {
    for(let i = 0; i < size; i++){
        const dark = i % 2 === 0;
        modules[6][i] = dark;
        isFunction[6][i] = true;
        modules[i][6] = dark;
        isFunction[i][6] = true;
    }
    drawFinderPattern(modules, isFunction, size, 3, 3);
    drawFinderPattern(modules, isFunction, size, size - 4, 3);
    drawFinderPattern(modules, isFunction, size, 3, size - 4);
    const positions = alignmentPositions(version, size);
    const n = positions.length;
    for(let i = 0; i < n; i++){
        for(let j = 0; j < n; j++){
            const isCorner = i === 0 && j === 0 || i === 0 && j === n - 1 || i === n - 1 && j === 0;
            if (isCorner) continue;
            drawAlignmentPattern(modules, isFunction, positions[i] ?? 0, positions[j] ?? 0);
        }
    }
    drawFormatBits(modules, isFunction, size, 0);
    if (version >= 7) {
        let rem = version;
        for(let i = 0; i < 12; i++){
            rem = rem << 1 ^ (rem >>> 11) * 0x1f25;
        }
        const bits = version << 12 | rem;
        for(let i = 0; i < 18; i++){
            const dark = (bits >> i & 1) === 1;
            const a = size - 11 + i % 3;
            const b = Math.floor(i / 3);
            modules[b][a] = dark;
            isFunction[b][a] = true;
            modules[a][b] = dark;
            isFunction[a][b] = true;
        }
    }
}
function drawFormatBits(modules, isFunction, size, mask) {
    const data = 0b00 << 3 | mask;
    let rem = data;
    for(let i = 0; i < 10; i++){
        rem = rem << 1 ^ (rem >>> 9) * 0x537;
    }
    const bits = (data << 10 | rem) ^ 0x5412;
    const set = (x, y, dark)=>{
        modules[y][x] = dark;
        isFunction[y][x] = true;
    };
    const bit = (i)=>(bits >> i & 1) === 1;
    for(let i = 0; i <= 5; i++)set(8, i, bit(i));
    set(8, 7, bit(6));
    set(8, 8, bit(7));
    set(7, 8, bit(8));
    for(let i = 9; i < 15; i++)set(14 - i, 8, bit(i));
    for(let i = 0; i < 8; i++)set(size - 1 - i, 8, bit(i));
    for(let i = 8; i < 15; i++)set(8, size - 7 + (i - 8), bit(i));
    set(8, size - 8, true);
}
function drawCodewords(modules, isFunction, size, codewords) {
    let i = 0;
    for(let right = size - 1; right >= 1; right -= 2){
        if (right === 6) right = 5;
        for(let vert = 0; vert < size; vert++){
            for(let j = 0; j < 2; j++){
                const x = right - j;
                const upward = (right + 1 & 2) === 0;
                const y = upward ? size - 1 - vert : vert;
                if (isFunction[y][x] !== true && i < codewords.length * 8) {
                    const byte = codewords[i >>> 3] ?? 0;
                    modules[y][x] = (byte >> 7 - (i & 7) & 1) === 1;
                    i++;
                }
            }
        }
    }
}
function applyMask(modules, isFunction, size, mask) {
    for(let y = 0; y < size; y++){
        for(let x = 0; x < size; x++){
            if (isFunction[y][x] === true) continue;
            let invert;
            switch(mask){
                case 0:
                    invert = (x + y) % 2 === 0;
                    break;
                case 1:
                    invert = y % 2 === 0;
                    break;
                case 2:
                    invert = x % 3 === 0;
                    break;
                case 3:
                    invert = (x + y) % 3 === 0;
                    break;
                case 4:
                    invert = (Math.floor(y / 2) + Math.floor(x / 3)) % 2 === 0;
                    break;
                case 5:
                    invert = x * y % 2 + x * y % 3 === 0;
                    break;
                case 6:
                    invert = (x * y % 2 + x * y % 3) % 2 === 0;
                    break;
                default:
                    invert = ((x + y) % 2 + x * y % 3) % 2 === 0;
                    break;
            }
            if (invert) modules[y][x] = !modules[y][x];
        }
    }
}
function computePenalty(modules, size) {
    const N1 = 3;
    const N2 = 3;
    const N3 = 40;
    const N4 = 10;
    let result = 0;
    for(let y = 0; y < size; y++){
        const row = modules[y];
        result += penaltyForLine(row, size, N1, N3);
    }
    for(let x = 0; x < size; x++){
        const col = [];
        for(let y = 0; y < size; y++)col.push(modules[y][x] === true);
        result += penaltyForLine(col, size, N1, N3);
    }
    for(let y = 0; y < size - 1; y++){
        for(let x = 0; x < size - 1; x++){
            const c = modules[y][x];
            if (c === modules[y][x + 1] && c === modules[y + 1][x] && c === modules[y + 1][x + 1]) {
                result += N2;
            }
        }
    }
    let dark = 0;
    for (const row of modules){
        for (const cell of row)if (cell) dark++;
    }
    const total = size * size;
    const k = Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1;
    result += Math.max(0, k) * N4;
    return result;
}
function penaltyForLine(line, size, N1, N3) {
    let result = 0;
    let runLength = 1;
    for(let i = 1; i < size; i++){
        if (line[i] === line[i - 1]) {
            runLength++;
        } else {
            if (runLength >= 5) result += N1 + (runLength - 5);
            runLength = 1;
        }
    }
    if (runLength >= 5) result += N1 + (runLength - 5);
    const PATTERN_A = [
        true,
        false,
        true,
        true,
        true,
        false,
        true,
        false,
        false,
        false,
        false
    ];
    const PATTERN_B = [
        false,
        false,
        false,
        false,
        true,
        false,
        true,
        true,
        true,
        false,
        true
    ];
    for(let i = 0; i + 11 <= size; i++){
        let matchA = true;
        let matchB = true;
        for(let j = 0; j < 11; j++){
            if (line[i + j] !== PATTERN_A[j]) matchA = false;
            if (line[i + j] !== PATTERN_B[j]) matchB = false;
            if (!matchA && !matchB) break;
        }
        if (matchA) result += N3;
        if (matchB) result += N3;
    }
    return result;
}
function buildQrMatrix(text, options = {}) {
    const level = options.errorCorrection ?? 'M';
    if (level !== 'M') {
        throw new Error(`raccoon: 二维码目前只支持纠错等级 M（收到 ${level}）`);
    }
    const forcedMask = options.mask;
    if (forcedMask !== undefined && (!Number.isInteger(forcedMask) || forcedMask < 0 || forcedMask > 7)) {
        throw new Error(`raccoon: 掩码必须是 0..7 的整数（收到 ${String(forcedMask)}）`);
    }
    const bytes = Array.from(new TextEncoder().encode(text));
    const version = pickVersion(bytes.length);
    if (version === undefined) {
        throw new Error(`raccoon: 二维码内容过长（${bytes.length} 字节，上限 213 字节），请缩短内容`);
    }
    const size = version * 4 + 17;
    const modules = Array.from({
        length: size
    }, ()=>new Array(size).fill(false));
    const isFunction = Array.from({
        length: size
    }, ()=>new Array(size).fill(false));
    drawFunctionPatterns(modules, isFunction, size, version);
    drawCodewords(modules, isFunction, size, buildCodewords(bytes, version));
    let bestMask = 0;
    if (forcedMask !== undefined) {
        bestMask = forcedMask;
    } else {
        let bestPenalty = Number.POSITIVE_INFINITY;
        for(let mask = 0; mask < 8; mask++){
            applyMask(modules, isFunction, size, mask);
            drawFormatBits(modules, isFunction, size, mask);
            const penalty = computePenalty(modules, size);
            if (penalty < bestPenalty) {
                bestPenalty = penalty;
                bestMask = mask;
            }
            applyMask(modules, isFunction, size, mask);
        }
    }
    applyMask(modules, isFunction, size, bestMask);
    drawFormatBits(modules, isFunction, size, bestMask);
    return {
        size,
        modules
    };
}
function renderQrSvg(text, options = {}) {
    const px = options.size ?? 158;
    const margin = options.margin ?? 4;
    const dark = options.dark ?? '#000000';
    const light = options.light ?? '#ffffff';
    const { size, modules } = buildQrMatrix(text);
    const dim = size + margin * 2;
    const segments = [];
    for(let y = 0; y < size; y++){
        for(let x = 0; x < size; x++){
            if (modules[y]?.[x] === true) {
                segments.push(`M${x + margin},${y + margin}h1v1h-1z`);
            }
        }
    }
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${px}" height="${px}" ` + `viewBox="0 0 ${dim} ${dim}" shape-rendering="crispEdges" role="img">` + `<rect width="${dim}" height="${dim}" fill="${light}"/>` + `<path d="${segments.join('')}" fill="${dark}"/>` + '</svg>';
}
return {buildQrMatrix:buildQrMatrix,renderQrSvg:renderQrSvg};
})();
function raccoonQrSvg(text) {
  try {
    return RACCOON_QR.renderQrSvg(String(text), { size: 200, margin: 3, dark: '#111111', light: '#ffffff' });
  } catch (e) {
    return '';
  }
}
// 登录动作
async function authStart(platform) {
  const box = $('#authBox');
  if (box) box.innerHTML = '<div class="faint">正在生成授权链接…</div>';
  const r = await post('/api/auth', { action: 'start', platform }).catch((e) => ({ ok: false, message: e.message }));
  if (!box) return;
  if (r.status === 'error' || r.ok === false) {
    box.innerHTML = `<div class="note errnote">${esc(r.message || '启动失败')}</div>`;
    return;
  }
  const lu = safeUrl(r.login_url);
  if (lu) {
    window.open(lu, '_blank', 'noopener');
  }
  // 二维码两种来源：服务端编码好的 data URL（qr_image），或只有文本（qr_text，前端用内嵌编码器画）。
  const qrSvg = (!r.qr_image && r.qr_text) ? raccoonQrSvg(r.qr_text) : '';
  if (r.qr_image || qrSvg) {
    // 扫码登录：显示二维码 + 自动轮询
    const qrMark = qrSvg || `<img class="qr" src="${esc(r.qr_image)}" alt="二维码" style="width:200px;height:200px">`;
    box.innerHTML = `<div class="note">${esc(r.message || '请扫码')}</div>
      <div class="qrwrap">${qrMark}</div>
      <div class="faint" style="text-align:center">${platform === 'apk-doubao' ? '用豆包 / 抖音 App 扫码，在手机上确认登录' : platform === 'apk-kuku' ? '用百度 App / 百度网盘 App 扫一扫，在手机上确认登录' : platform === 'apk-loomy' ? '用微信扫一扫，在手机上确认登录' : platform === 'apk-raccoon' ? '用微信扫一扫（进入「商汤小浣熊」小程序），在手机上确认登录' : '扫码并在手机上确认登录'}（二维码有有效期，失效后重新点「开始登录」）</div>
      <div id="pollBox"></div>`;
    srcQrStop();
    SRC_QR_TRIES = 0;
    SRC_POLL_N = 0;
    srcQrSchedule(platform, 1200);   // 给用户 1.2s 先看清二维码，再开始轮询
  } else if (r.login_url) {
    // ★ 跳转授权（PKCE / Trae / CodeArts）：以前这里只给一个「点这里查询」按钮，
    //   于是用户浏览器授权成功后回到面板必须再手点一次才会去取码换 token —— 而回调
    //   其实早已进了后端队列（auth_login.rs 的 push_callback），白白让人以为「没自动获取」。
    //   现在与扫码一样起自调度轮询：授权完成 → 下一次 poll 取到 code → 直接入库。
    box.innerHTML = `<div class="note">${esc(r.message || '已打开授权页，等待你在浏览器里完成授权…')}</div>
      ${r.callback_warning ? `<div class="note errnote">${esc(r.callback_warning)}</div>` : ''}
      <div id="pollBox"></div>`;
    srcQrStop();
    SRC_QR_TRIES = 0;
    SRC_POLL_N = 0;
    srcQrSchedule(platform, 1500);   // 留出浏览器跳转 + 登录的时间，再开始问
  } else {
    box.innerHTML = `<div class="note">${esc(r.message || '已生成授权会话')}</div>
      ${r.user_code ? `<div class="usercode">输入设备码：<b>${esc(r.user_code)}</b></div>` : ''}
      <div class="row"><button class="btn" onclick="authPoll('${att(platform)}')">授权完成后，点这里查询</button></div>
      <div id="pollBox"></div>`;
  }
  SRC_SESSION = r.id;
}
// 扫码轮询：**自调度**（每次 poll 回来再排下一次），不再用 setInterval。
// 原因：库库 unicast 是 ~30s 长轮询，固定 2s 的 setInterval 会在一个请求还没回来时
// 继续叠加新请求 —— 实测 15s 以上的 poll 能叠出十几个并发连接，既浪费又把提示刷成乱码。
let SRC_QR_TIMER = null;       // 下一次 poll 的 setTimeout 句柄
let SRC_QR_ACTIVE = false;     // 是否处于自动轮询（终态/过期后关掉）
let SRC_QR_TRIES = 0;          // 连续自动重开二维码次数（防死循环）
let SRC_POLL_INFLIGHT = false; // 在途请求闩，双保险
let SRC_SESSION = null;
function srcQrStop() {
  if (SRC_QR_TIMER) clearTimeout(SRC_QR_TIMER);
  SRC_QR_TIMER = null;
  SRC_QR_ACTIVE = false;
  SRC_POLL_INFLIGHT = false;
}
let SRC_POLL_N = 0;             // 已轮询次数（跳转授权有上限，扫码没有）
const SRC_POLL_MAX_REDIRECT = 150;  // 3 秒一次 ≈ 7.5 分钟；够走完登录 + 二次验证
function srcQrSchedule(platform, ms) {
  if (SRC_QR_TIMER) clearTimeout(SRC_QR_TIMER);
  SRC_QR_ACTIVE = true;
  SRC_QR_TIMER = setTimeout(() => { SRC_QR_TIMER = null; authPoll(platform); }, ms);
}
async function authCallback(platform) {
  const input = $('#cbUrl');
  const url = input ? input.value.trim() : '';
  if (!url) { alert('请先粘贴回调地址（含 ?code= 与 &state=）'); return; }
  const box = $('#authBox');
  if (box) box.innerHTML = '<div class="faint">提交回调中…</div>';
  const r = await post('/api/auth', { action: 'callback', url }).catch((e) => ({ ok: false, message: e.message }));
  const b = $('#authBox');
  if (!b) return;
  b.innerHTML = `<div class="note ${r.ok ? 'oknote' : 'errnote'}">${esc(r.message || '回调提交结果')}</div>`;
  if (r.ok) {
    b.innerHTML += `<div class="row"><button class="btn pri" onclick="authPoll('${att(platform)}')">查询授权状态</button></div>`;
  }
}
async function authPoll(platform) {
  const id = SRC_SESSION;
  if (!id) { alert('请先点「开始登录」'); return; }
  if (SRC_POLL_INFLIGHT) return;              // 上一次还没回来：不叠加请求
  SRC_POLL_INFLIGHT = true;
  let r;
  try {
    r = await post('/api/auth', { action: 'poll', id }).catch((e) => ({ ok: false, message: e.message }));
  } finally {
    SRC_POLL_INFLIGHT = false;
  }
  // 请求在途期间用户可能已经重新开始（换了新会话 id）：旧响应必须丢弃，
  // 否则它会给「已经结束的会话」再排一轮轮询 —— 就是我们要消灭的叠请求。
  if (SRC_SESSION !== id) return;
  const pb = $('#pollBox') || $('#authBox');
  if (!pb) { srcQrStop(); return; }            // 抽屉被关掉：别再空转
  if (r.ok && r.session && r.session.status === 'success') {
    srcQrStop();
    pb.innerHTML = `<div class="note oknote">✓ ${esc(r.session.message || '登录成功，已入库')}</div>`;
    setTimeout(() => render(), 800);
    return;
  }
  const msg = (r.session && r.session.message) || r.message || '还没完成，稍后再查';
  const expired = msg.indexOf('过期') >= 0 || msg.indexOf('失效') >= 0 || msg.indexOf('已取消') >= 0;
  const scanned = msg.indexOf('已扫码') >= 0 || msg.indexOf('已扫描') >= 0;
  if (expired) {
    srcQrStop();
    if (SRC_QR_TRIES < 3 && $('#pollBox')) {
      // 二维码过期是**常态**（豆包/百度只有 1–3 分钟），自动换一张，别让用户点第二次。
      SRC_QR_TRIES += 1;
      pb.innerHTML = `<div class="note">二维码已过期，正在自动刷新（第 ${SRC_QR_TRIES} 次）…</div>`;
      setTimeout(() => { if ($('#pollBox')) authStart(platform); }, 500);
      return;
    }
    pb.innerHTML = `<div class="note errnote">${esc(msg)}</div>
      <div class="row"><button class="btn pri" onclick="authStart('${att(platform)}')">重新生成二维码</button></div>`;
    return;
  }
  // 跳转授权（浏览器里完成）时后端一直回「等待授权中」，这是**正常的中间态**，
  // 不能像扫码过期那样停轮询 —— 否则用户点完同意回面板，界面就定格在「等待授权中」。
  const waiting = msg.indexOf('等待授权中') >= 0;
  pb.innerHTML = `<div class="note ${scanned || waiting ? '' : 'oknote'}">${esc(msg)}</div>
    ${SRC_QR_ACTIVE
      ? `<div class="faint" style="text-align:center;margin-top:6px">${waiting
          ? '等待浏览器完成授权…授权完成后会自动入库，不用回来点任何按钮'
          : '自动轮询中…（百度/豆包是长轮询，最长 ~30 秒才回一次，停在「等待扫码」不报错就是正常）'}</div>`
      : `<div class="row"><button class="btn" onclick="authPoll('${att(platform)}')">再查一次</button></div>`}`;
  if (waiting) {
    SRC_POLL_N += 1;
    if (SRC_POLL_N > SRC_POLL_MAX_REDIRECT) {
      srcQrStop();
      pb.innerHTML = '<div class="note errnote">等太久了（约 7 分钟还没拿到授权回调）。可能这次授权页没走完，或本机回调端口被占用 —— 请把浏览器跳转后地址栏的完整 URL 粘贴到下面输入框，点「提交回调」。</div>'
        + `<div class="row"><button class="btn" onclick="authPoll('${att(platform)}')">再查一次</button></div>`;
      return;
    }
  }
  // 跳转授权要等用户在浏览器登录（可能还要过二次验证），用 3 秒慢一点，别把 /api/auth 打满。
  if (SRC_QR_ACTIVE) srcQrSchedule(platform, waiting ? 3000 : 2000);
}
async function authSave(platform, allowEmpty) {
  const input = $('#keyInput');
  const secret = input ? input.value.trim() : '';
  if (!secret) {
    if (allowEmpty) {
      const b0 = $('#authBox');
      if (b0) b0.innerHTML = '<div class="note oknote">留空即匿名免登录、直接可用，无需保存。关闭窗口后在对话里选 metaso 模型即可；想提高额度再粘贴 uid-sid。</div>';
      return;
    }
    alert('请先粘贴凭据');
    return;
  }
  const box = $('#authBox');
  if (box) box.innerHTML = '<div class="faint">保存中…</div>';
  const r = await post('/api/auth', { action: 'save', platform, secret }).catch((e) => ({ ok: false, message: e.message }));
  const b = $('#authBox');
  if (!b) return;
  if (r.ok) {
    b.innerHTML = `<div class="note oknote">✓ ${esc(r.message || '凭据已保存')}</div>`;
    setTimeout(() => render(), 800);
  } else {
    b.innerHTML = `<div class="note errnote">${esc(r.message || '保存失败')}</div>`;
  }
}
// 免密平台连通性测试。
// ★ 旧实现只 fetch /v1/models 数「owned_by 命中条数」就报「连通正常」：
//   /v1/models 只证明模型清单非空，与「此刻能不能真的转发」无关 —— anon-zen
//   整条车道被上游 403 FreeTierError 封锁时，面板照样显示绿色「连通正常」。
//   现在改成用该平台第一个模型真实发一次最小对话（max_tokens=8），失败就把
//   上游状态码 / 错误文案 / hint 原样摊开。
async function testAnon(platform) {
  const b = $('#authBox');
  const set = (html) => { if (b) b.innerHTML = html; };
  set('<div class="faint">测试中：正在通过本面板真实发一次最小对话…</div>');
  try {
    const mr = await fetch('/v1/models', withToken());
    const mj = await mr.json().catch(() => ({}));
    const owned = (mj.data || []).filter((m) => m.owned_by === platform).map((m) => m.id);
    if (!owned.length) {
      set('<div class="note errnote">该平台当前没有模型：清单为空，先确认平台可用或检查是否需要同步模型。</div>');
      return;
    }
    const model = owned[0];
    const r = await fetch('/v1/chat/completions', withToken({
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model, messages: [{ role: 'user', content: 'ping' }], max_tokens: 8, stream: false }),
    }));
    const j = await r.json().catch(() => ({}));
    if (r.ok) {
      set(`<div class="note oknote">连通正常 ✓ 已用 <code>${esc(model)}</code> 真实对话成功（该平台共 ${owned.length} 个模型）</div>`);
      return;
    }
    const err = (j && j.error) || {};
    const msg = String(err.message || j.message || ('HTTP ' + r.status)).slice(0, 300);
    const hint = err.hint || '';
    set(`<div class="note errnote">连通失败（HTTP ${r.status}）：${esc(msg)}`
      + (hint ? `<br><span class="faint">${esc(hint)}</span>` : '')
      + `<br><span class="faint">模型 ${esc(model)}；这是 max_tokens=8 的最小探活，失败说明该平台此刻确实转发不了。</span></div>`);
  } catch (e) {
    set(`<div class="note errnote">测试失败：${esc(e.message)}</div>`);
  }
}

// 自定义 API 弹窗（CherryStudio 式：填 URL/Key，自动同步模型）
function openCustomApi(editId) {
  const mk = modalShell(`${editId ? '编辑' : '新建'}自定义 API`,
    `
      <label>名称</label>
      <input class="q" id="cName" placeholder="如：我的中转">
      <label>协议</label>
      <select class="q" id="cFormat">
        <option value="openai">OpenAI 兼容（当前唯一支持）</option>
      </select>
      <div class="faint small">面板只做 OpenAI 兼容透传（不做协议转换）：Anthropic / Responses 协议请填各自的 OpenAI 兼容网关地址。</div>
      <div class="faint small" id="cPinNote" style="color:var(--ok,#3a7)"></div>
      <label>URL（base_url）</label>
      <input class="q mono" id="cUrl" placeholder="https://api.example.com/v1">
      <label>API Key</label>
      <input class="q mono" id="cKey" placeholder="${editId ? '留空＝保留原 Key（不回显）' : 'sk-…'}">
      <div class="modal-foot">
        <button class="btn pri" onclick="saveCustomApi('${att(editId || '')}')">保存并同步模型</button>
        ${editId ? `<button class="btn" onclick="removeCustomApi('${att(editId)}')">删除</button>` : ''}
      </div>`);
  document.body.insertAdjacentHTML('beforeend', mk);
  // 编辑态回填
  if (editId) {
    api('/api/custom').then((j) => {
      const c = (j.rows || []).find((x) => x.id === editId);
      if (!c) return;
      $('#cName').value = c.name || '';
      $('#cFormat').value = c.format || 'openai';
      $('#cUrl').value = c.base_url || '';
      // 固定过清单的条目：保存不会重拉上游（服务端跳过自动同步），提前说清楚，
      // 免得用户以为「保存并同步模型」没生效。
      const pn = document.querySelector('#cPinNote');
      if (pn && c.models_pinned) {
        pn.textContent = '该条目模型清单已固定（' + (c.models || []).length + ' 个）：保存不会重拉上游清单；要恢复跟随上游，在接入源卡片抽屉里点「恢复跟随上游」。';
      }
    });
  }
}
async function saveCustomApi(editId) {
  const name = $('#cName').value.trim();
  const format = $('#cFormat').value;
  const base_url = $('#cUrl').value.trim();
  const api_key = $('#cKey').value.trim();
  // 编辑时 Key 不回显（掩码原则），留空＝沿用旧值：后端 update 对空 api_key
  // 本来就是「保留原值」，旧前端却在这里硬拦，等于改个名字都做不到。
  if (!name || !base_url) { alert('名称 / URL 都要填'); return; }
  if (!editId && !api_key) { alert('新建自定义 API 必须填 Key'); return; }
  const action = editId ? 'update' : 'add';
  const body = { action, name, format, base_url, api_key };
  if (editId) body.id = editId;
  const r = await post('/api/custom', body).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert(r.message || '保存失败'); return; }
  // 保存后自动同步模型（服务端已顺手拉过一次，这里兜底补同步）
  const cid = r.api ? r.api.id : editId;
  if (cid && !r.models_synced) await syncCustomModels(cid);
  document.querySelector('.modal-mask')?.remove();
  render();
}
async function syncCustomModels(id) {
  // 模型清单由**服务端**拉取：浏览器直连 {base}/models 是跨域请求，绝大多数上游
  // 不带 CORS 头，响应会被浏览器拦掉 —— 旧实现等于从来没同步成功过。
  // 这里只调后端 sync_models，让服务端用统一代理/超时/凭据去拉。
  const r = await post('/api/custom', { action: 'sync_models', id }).catch(() => ({ ok: false }));
  return !!(r && r.ok);
}
async function removeCustomApi(id) {
  if (!confirm('删除该自定义 API？')) return;
  const r = await post('/api/custom', { action: 'remove', id }).catch((e) => ({ ok: false, message: e.message }));
  document.querySelector('.modal-mask')?.remove();
  render();
}
function srcRerender() {
  const el = $('#view');
  if (el) viewSources(el);
}

// ─────────────────────────────────────────────
// 原四视图（总览 / 账号 / 用量）
// ─────────────────────────────────────────────
// health.skipped_specs：平台 spec 写错时后端只 warn 后跳过，面板必须让用户看见，
// 否则「平台凭空少了一个」只能靠 curl /api/health 才发现。
function skippedNote(h) {
  if (!Array.isArray(h.skipped_specs) || !h.skipped_specs.length) return '';
  const rows = h.skipped_specs
    .map((s) => esc(String(s.file || '?') + '：' + String(s.reason || '')))
    .join('<br>');
  const head = h.warning || (h.skipped_specs.length + ' 个平台 spec 非法已被跳过');
  return '<div class="note warn">⚠ ' + esc(head)
    + '<div class="faint mono" style="font-size:11.5px;margin-top:6px">' + rows + '</div>'
    + '<div class="faint" style="font-size:11.5px;margin-top:6px">这些平台不会出现在「接入源」与 /v1/models 里；修好 providers/ 下对应的 .toml 后重启即可。</div></div>';
}
async function viewOverview(el) {
  const [h, pl, ac, us, pool] = await Promise.all([
    api('/api/health'), api('/api/platforms'), api('/api/accounts'), api('/api/usage'),
    api('/api/pool').catch(() => null),
  ]);
  const enabled = pl.rows.filter((p) => p.enabled).length;
  // 成功率的分母要扣掉「客户端主动取消」：那是用户点停止，不是平台失败，
  // 算进去等于每按一次停止都把成功率拉低一点。
  const realReq = Math.max(0, (us.requests || 0) - (us.cancelled || 0));
  const rate = realReq ? Math.round((us.success / realReq) * 100) : 100;
  el.innerHTML = `
    <div class="grid g4">
      <div class="card glass kpi"><div class="lb">平台</div><div class="vl">${enabled}/${pl.total}</div>
        <div class="ex">启用 / 声明式 spec + 自定义</div>
        <div class="bar"><i style="width:${pl.total ? (enabled / pl.total) * 100 : 0}%"></i></div></div>
      <div class="card glass kpi"><div class="lb">账号</div><div class="vl">${ac.total}</div>
        <div class="ex">已导入（secret 后台专用）</div></div>
      <div class="card glass kpi"><div class="lb">累计调用</div><div class="vl">${nfmt(us.requests)}</div>
        <div class="ex">成功 ${nfmt(us.success)} · 失败 ${nfmt(us.failed)}${us.cancelled ? ` · 取消 ${nfmt(us.cancelled)}` : ''}</div></div>
      <div class="card glass kpi"><div class="lb">成功率</div><div class="vl">${rate}%</div>
        <div class="ex">tokens ${nfmt(us.tokensIn)} in / ${nfmt(us.tokensOut)} out</div></div>
    </div>
    ${(() => {
      if (!pool) return '';
      const cooling = pool.cooling || [];
      const waf = pool.waf || [];
      const total = cooling.length + waf.length;
      if (!total) return '';
      const fmt = (v) => { const t = Math.round(v || 0); return t >= 60 ? Math.floor(t / 60) + ' 分 ' + String(t % 60).padStart(2, '0') + ' 秒' : t + ' 秒'; };
      const top = [...waf.map((w) => ({ kind: 'WAF', account: w.account, left: w.left_s })),
                   ...cooling.map((c) => ({ kind: '冷却', account: c.account, model: c.model, left: c.left_s }))]
        .sort((x, y) => y.left - x.left).slice(0, 3);
      return '<div class="note"><b>号池有 ' + total + ' 条阻断</b>：'
        + top.map((x) => esc(x.account) + (x.model ? ' · ' + esc(x.model) : '') + '（' + x.kind + ' ' + fmt(x.left) + '）').join('、')
        + (total > top.length ? ' 等' : '')
        + ' —— 这些请求会立刻 429 或换号，到「账号」页可看完整明细。</div>';
    })()}
    <section class="card glass"><h3>运行状态</h3>
      ${skippedNote(h)}
      <div class="mini-grid">
        <div class="mini glass"><b>${esc(h.service)} v${esc(h.version)}</b>
          <span class="pill ok">${esc(h.milestone)}</span></div>
        <div class="mini glass"><b>存储</b><span class="pill ${us.db ? 'ok' : 'warn'}">${us.db ? 'SQLite WAL' : 'JSON 降级'}</span></div>
        <div class="mini glass"><b>端点</b><code>/v1/chat/completions</code> · <code>/v1/messages</code> · <code>/v1/responses</code><br><span class="faint">模型路由：清单匹配 或 <code>模型id@平台</code></span></div>
      </div>
    </section>`;
}

// 号池治理状态（/api/pool）：在途租约、账号/模型冷却、WAF 限速、会话粘性占用。
//
// 这些数据决定「同一个平台为什么有时快、有时立刻 429」，但面板以前完全不展示：
// 用户只看得到失败，看不到「这个账号正在冷却 37 秒」。
function poolSecs(v) {
  var t = Math.max(0, Math.round(v || 0));
  if (t >= 60) return Math.floor(t / 60) + ' 分 ' + String(t % 60).padStart(2, '0') + ' 秒';
  return t + ' 秒';
}

// 解冻按钮：冷却时长是网关猜的（WAF/402 一律 30 分钟），上游常常早就好了。
// 让用户确认后一键放行，而不是干等或重启服务。
function thawBtn(acct) {
  if (!acct) return '—';
  return '<button class="btn sm gh" onclick="poolThaw(\'' + esc(acct) + '\')" title="清掉这个账号的冷却与整号限速">解冻</button>';
}

async function poolThaw(acct) {
  const r = await post('/api/pool', { action: 'thaw', account: acct });
  alert(r.message || (r.ok ? '已解冻' : '解冻失败'));
  render();
}
function poolCard(p) {
  if (!p) return '';
  var inflight = p.inflight || [];
  var cooling = p.cooling || [];
  var waf = p.waf || [];
  var st = p.sticky || {};
  var nIn = 0;
  inflight.forEach(function (x) { nIn += (x.n || 0); });
  var stickyN = (typeof p.sticky_entries === 'number') ? p.sticky_entries : (st.total || 0);
  var stickyMax = p.sticky_max_entries || 0;
  var rows = [];
  inflight.slice(0, 8).forEach(function (x) {
    rows.push('<tr><td>' + pill('ok', '在途') + '</td>'
      + '<td class="mono">' + esc(x.account || '—') + '</td>'
      + '<td class="faint">' + esc(x.model || '—') + '</td>'
      + '<td>' + (x.n || 0) + ' 个在飞</td>'
      + '<td class="faint">' + esc(x.platform || '—') + '</td>'
      + '<td class="faint">—</td></tr>');
  });
  cooling.slice(0, 8).forEach(function (c) {
    rows.push('<tr><td>' + pill('warn', '冷却') + '</td>'
      + '<td class="mono">' + esc(c.account || '—') + '</td>'
      + '<td class="faint">' + esc(c.model || '—') + '</td>'
      + '<td>' + poolSecs(c.left_s) + '</td>'
      + '<td class="faint">' + (c.strikes == null ? '—' : esc(String(c.strikes)) + ' 次连击') + '</td>'
      + '<td>' + thawBtn(c.account) + '</td></tr>');
  });
  waf.slice(0, 8).forEach(function (w) {
    rows.push('<tr><td>' + pill('err', 'WAF') + '</td>'
      + '<td class="mono">' + esc(w.account || '—') + '</td>'
      + '<td class="faint">整号限速</td>'
      + '<td>' + poolSecs(w.left_s) + '</td><td class="faint">—</td>'
      + '<td>' + thawBtn(w.account) + '</td></tr>');
  });
  var hidden = (inflight.length > 8 ? inflight.length - 8 : 0)
    + (cooling.length > 8 ? cooling.length - 8 : 0)
    + (waf.length > 8 ? waf.length - 8 : 0);
  var idle = (!nIn && !cooling.length && !waf.length && !stickyN);
  var h = '<section class="card glass" style="margin-top:14px">'
    + '<h3 style="display:flex;align-items:center;gap:8px">号池与限速'
    + '<button class="btn sm gh" style="margin-left:auto" onclick="render()" title="重新拉取号池状态">刷新</button></h3>'
    + '<div class="mini-grid">';
  h += '<div class="mini glass"><b>在途请求 ' + nIn + ' / 上限 ' + esc(String(p.max_inflight == null ? '—' : p.max_inflight)) + '</b>'
    + '<span class="faint">同一 (平台,账号,模型) 的并发租约，满了直接 429「provider busy」</span></div>';
  h += '<div class="mini glass"><b>冷却中 ' + cooling.length + ' 个</b>'
    + '<span class="faint">429 / 额度不足后按指数退避停用的「账号-模型」</span></div>';
  h += '<div class="mini glass"><b>WAF 限速 ' + waf.length + ' 个</b>'
    + '<span class="faint">整账号被上游风控拦下时按账号停用</span></div>';
  h += '<div class="mini glass"><b>会话粘性 ' + stickyN + (stickyMax ? ' / ' + stickyMax : '') + ' 条</b>'
    + '<span class="faint">同一 conversation_id 钉住同一账号（TTL ' + esc(String(st.ttl_s == null ? '—' : st.ttl_s)) + ' 秒）</span></div>';
  h += '</div>';
  if (rows.length) {
    h += '<table style="margin-top:10px"><thead><tr><th style="width:70px">原因</th><th>账号</th><th>模型</th><th>剩余</th><th>备注</th><th style="width:88px">操作</th></tr></thead><tbody>'
      + rows.join('') + '</tbody></table>';
  }
  if (hidden > 0) h += '<div class="faint" style="margin-top:6px">还有 ' + hidden + ' 条未显示（每类只列前 8 条）</div>';
  if (idle) h += '<div class="empty">号池空闲：没有在途请求、没有账号处于冷却或限速，也没有活跃的会话粘性绑定</div>';
  h += '</section>';
  return h;
}

let AC_CACHE = [];

async function viewAccounts(el) {
  // /api/pool 拿不到不该让整页空白：账号表是主内容，号池状态是附加信息。
  const [ac, pool] = await Promise.all([
    api('/api/accounts'),
    api('/api/pool').catch(() => null),
  ]);
  AC_CACHE = ac.rows || [];
  const wbN = ac.rows.filter((a) => a.platform === 'wb-gateway' || a.platform === 'codebuddy').length;
  // 免密车道（anon）不需要账号：池里出现的行都是历史遗留占位（name=匿名公共池 / secret=public），
  // 单列出来提示可删，别让用户以为那是一份要维护的凭据。
  const anonRows = ac.rows.filter((a) => a.anon);
  el.innerHTML = `<section class="card glass">
    <div style="display:flex;align-items:center;gap:10px;margin-bottom:12px">
      <b>账号凭据</b>
      ${wbN > 0 ? `<button class="btn sm" onclick="wbCheckin()">wb 每日签到（${wbN} 账号）</button>` : ''}
      ${wbN > 0 ? `<button class="btn sm ${ac.daily_checkin ? 'pri' : 'gh'}" onclick="wbAutoToggle(${ac.daily_checkin ? 'false' : 'true'}, this)" title="开启后后台每 12 小时自动领取一次；默认关，开关存 flags.json，重启保持。当天已领过时上游回「今日已领取」，无副作用">自动签到：${ac.daily_checkin ? '开 ✔' : '关'}</button>` : ''}
      ${wbN > 0 ? `<button class="btn sm" onclick="wbGrowth()" title="一键完成成长任务（17 项）：事件链上报 + 领奖，全新账号约 +1950 积分">⚡ 成长任务</button>` : ''}
      ${wbN > 0 ? `<button class="btn sm ${ac.daily_growth ? 'pri' : 'gh'}" onclick="wbGrowthToggle(${ac.daily_growth ? 'false' : 'true'}, this)" title="开启后后台每 12 小时自动跑一轮成长任务；默认关">自动成长：${ac.daily_growth ? '开 ✔' : '关'}</button>` : ''}
      <span class="faint" style="margin-left:auto">健康列来自最近一次真实请求 / 「检查」验活的结果（重启后清空）· Buddy 加油站每日 100 积分仅 wb 族账号可签到</span>
    </div>
    ${checkinTaskLine(ac.checkin_task)}
    ${ac.rows.some((a) => a.platform_known === false) ? `<div class="note">有 ${ac.rows.filter((a) => a.platform_known === false).length} 个账号所属的平台已经不在 providers/ 里了（标记「平台已移除」）：这些号转不出去，留着只会混淆，可以直接删除。</div>` : ''}
    ${ac.rows.length ? `<table><thead><tr><th>账号名</th><th>平台</th><th>凭据类型</th><th>凭据</th><th>状态</th><th>健康</th><th style="width:250px">操作</th></tr></thead>
    <tbody>${ac.rows.map((a) => `<tr>
      <td><b>${esc(a.name || a.id)}</b><br><span class="faint mono">${esc(a.id)}</span></td>
      <td>${a.platform_known === false ? `<span class="faint mono">${esc(a.platform)}</span><br>${pill('warn', '平台已移除')}` : `${esc(platShort(a.platform_name || a.platform))}<br><span class="faint mono" style="font-size:11px">${esc(a.platform)}</span>`}</td><td>${pill('mut', esc(a.kind || '—'))}</td>
      <td class="faint">${a.anon ? '无需凭据' : a.needs_relogin ? '本机解不开（换过 Windows 用户/机器）' : a.secret_len ? `${a.secret_len} 字符（掩码）` : '空'}</td>
      <td>${a.enabled ? pill('ok', '启用') : pill('mut', '停用')}</td>
      <td>${a.anon ? pill('mut', '免密车道') + healthPill(a.health) + blockPills(a) : healthPill(a.health) + blockPills(a)}</td>
      <td>
        ${a.anon
          ? `<span class="faint" style="font-size:11.5px">免密车道不需要账号，此行是历史占位数据；「状态」列显示的是这条车道当前的封锁/冷却</span>
             <button class="btn sm" onclick="acDelete('${att(a.id)}')">删除占位行</button>`
          : a.needs_relogin
          ? `<span class="faint" style="font-size:11.5px">重新登录一次（或「手动粘贴」）即可恢复</span>
             <button class="btn sm" onclick="acDelete('${att(a.id)}')">删除</button>`
          : `<button class="btn sm" onclick="acVerify('${att(a.id)}', this)" title="立刻用该凭据发一次验活请求">检查</button>
             <button class="btn sm" onclick="acBalance('${att(a.id)}', this)" title="查询积分余额（仅 WorkBuddy / CodeBuddy 族支持）">积分</button>
             <button class="btn sm" onclick="acRename('${att(a.id)}', this)" title="给这个号起个认得出的名字（同平台不能重名）">改名</button>
             <button class="btn sm" onclick="acToggle('${att(a.id)}', ${a.enabled ? 'false' : 'true'})">${a.enabled ? '停用' : '启用'}</button>
             <button class="btn sm" onclick="acDelete('${att(a.id)}')">删除</button>`}
      </td>
    </tr>`).join('')}</tbody></table>` : '<div class="empty">还没有账号：到「接入源」页挑一个平台接入</div>'}
    ${ac.rows.some((a) => a.needs_relogin) ? `<div class="note"><b>有 ${ac.rows.filter((a) => a.needs_relogin).length} 个账号的凭据在本机解不开</b>：磁盘上的凭据用 Windows DPAPI 加密，密钥绑定「当前 Windows 用户 + 本机」—— 从别的机器 / 别的 Windows 用户拷过来的 accounts.json 在这里解不开。把文件搬回原机器即可恢复，也可以在本机重新登录一次（旧的密文会原样保留，不会被覆盖）。</div>` : ''}
    ${anonRows.length ? `<div class="note">上面有 ${anonRows.length} 行属于免密车道（anon-zen / kilo / pollinations 这类不需要任何凭据的平台）：它们不影响转发，删掉也不影响使用 —— 想清理就点「删除占位行」。</div>` : ''}
  </section>`;
  el.innerHTML += poolCard(pool);
}

// 号池此刻的阻断剩余时间（与 healthPill 互补：那个说「上次为什么失败」，这个说「还要等多久」）。
function blockPills(a) {
  if (!a) return '';
  var fmt = function (v) {
    var t = Math.round(v || 0);
    return t >= 60 ? Math.floor(t / 60) + ' 分 ' + String(t % 60).padStart(2, '0') + ' 秒' : t + ' 秒';
  };
  if (a.waf_s > 0) return ' ' + pill('err', 'WAF 限速 ' + fmt(a.waf_s));
  if (a.cooling_s > 0) return ' ' + pill('warn', '冷却 ' + fmt(a.cooling_s));
  return '';
}

// 账号健康（号池按最近一次真实请求结论 + 「检查」验活结果写入内存）
// ok=正常 · auth=鉴权失败(过期) · quota=额度不足 · rate=被上游限流
function healthPill(h) {
  if (!h) return pill('mut', '未验活');
  const map = {
    ok: ['ok', '正常'],
    auth: ['err', '鉴权失败'],
    quota: ['warn', '额度不足'],
    rate: ['warn', '被限流'],
    // 车道级封锁/地域封锁：不是凭据问题，用 warn 而不是 err，避免用户去重新登录。
    lane: ['warn', '车道被封锁'],
    region: ['warn', '地域受限'],
  };
  const [cls, text] = map[h.status] || ['mut', h.status || '未知'];
  const ago = h.age_secs != null ? fmtAgo(h.age_secs) : '';
  const tip = (h.label || '') + (ago ? '（' + ago + '）' : '');
  return `<span title="${esc(tip)}">${pill(cls, text)}</span>`;
}
function fmtAgo(sec) {
  if (sec < 60) return sec + ' 秒前';
  if (sec < 3600) return Math.floor(sec / 60) + ' 分钟前';
  if (sec < 86400) return Math.floor(sec / 3600) + ' 小时前';
  return Math.floor(sec / 86400) + ' 天前';
}

// 字节数 → 人话（「关于」页数据占用卡片用）。
function fmtBytes(n) {
  n = Number(n) || 0;
  if (n < 1024) return n + ' B';
  if (n < 1048576) return (n / 1024).toFixed(1) + ' KB';
  if (n < 1073741824) return (n / 1048576).toFixed(1) + ' MB';
  return (n / 1073741824).toFixed(2) + ' GB';
}

// 立即验活：过期 Cookie / 被限流的号不用等下一次真实对话才发现。
// 平台短名：账号表里只要认得出是哪个产品就够，不必铺满一整行长名。
// 「Our Free Model（OpenCode Zen 免 Key 车道）」→「Our Free Model」；
// 「WPS AI（金山办公）」→「WPS AI」。没有括号的原样返回。
function platShort(name) {
  const s = String(name || '');
  const i = s.search(/[（(：:]/);
  return (i > 0 ? s.slice(0, i) : s).trim();
}

// 改账号名：账号名是面板上唯一的辨识依据（同平台挂几个号时尤其重要）。
async function acRename(id, btn) {
  const cur = (typeof AC_CACHE !== 'undefined' && AC_CACHE.find((a) => a.id === id)) || null;
  const v = prompt('给这个账号起个认得出的名字（同平台不能重名）：', cur ? cur.name || '' : '');
  if (v === null) return;
  const r = await post('/api/accounts', { action: 'rename', id, name: v }).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert('改名失败：' + (r.message || '未知错误')); return; }
  render();
}

async function acVerify(id, btn) {
  if (btn) { btn.disabled = true; btn.textContent = '检查中…'; }
  const r = await post('/api/accounts', { action: 'verify', id }).catch((e) => ({ ok: false, message: e.message }));
  if (btn) { btn.disabled = false; btn.textContent = '检查'; }
  if (!r.ok) { alert('检查失败：' + (r.message || '未知错误')); return; }
  if (!r.verified) alert('验活未通过：' + (r.message || '凭据可能已过期'));
  render();
}
// ★ 2026-10-26（用户排队消息④）：账号积分余额查询（WorkBuddy / CodeBuddy 族）。
// 结果用 toast 式浮层显示在按钮旁，不整页重绘 —— 用户点完立刻能看到数字。
async function acBalance(id, btn) {
  if (btn) { btn.disabled = true; btn.textContent = '查询中…'; }
  let r;
  try { r = await post('/api/accounts', { action: 'balance', id }); }
  catch (e) { r = { ok: false, message: e.message }; }
  if (btn) { btn.disabled = false; btn.textContent = '积分'; }
  if (!r.ok) { alert(r.supported === false ? '该平台没有积分余额端点（仅 WorkBuddy / CodeBuddy 族支持）' : '查询失败：' + (r.message || '未知错误')); return; }
  const parts = [];
  if (typeof r.credit === 'number') parts.push('积分 ' + r.credit);
  if (typeof r.energy === 'number' && r.energy > 0) parts.push('能量 ' + r.energy);
  if (typeof r.growth_score === 'number' && r.growth_score > 0) parts.push('成长值 ' + r.growth_score);
  alert(parts.length ? ('当前余额：' + parts.join(' · ')) : '上游未返回余额字段（账号可能刚登录，资源尚未同步）');
}
// 账号停用/启用（后端会同时清号池治理状态与粘性绑定）
async function acToggle(id, enabled) {
  const r = await post('/api/accounts', { action: 'toggle', id, enabled }).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert('操作失败：' + (r.message || '未知错误')); return; }
  render();
}
async function acDelete(id) {
  if (!confirm('删除账号 ' + id + '？\n凭据会一并删除，无法恢复（如需留存请先做离线备份）。')) return;
  const r = await post('/api/accounts', { action: 'delete', id }).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert('删除失败：' + (r.message || '未知错误')); return; }
  render();
}

// 自动签到后台任务的上一次结果（ok 优先；最近一次是失败就显示失败——
// 成功过一次不能把之后的失败盖住，与 freeSourceLine 同一套口径）。
function checkinTaskLine(t) {
  if (!t) return '';
  const okSec = typeof t.last_ok_s_ago === 'number' ? t.last_ok_s_ago : null;
  const errSec = typeof t.last_err_s_ago === 'number' ? t.last_err_s_ago : null;
  const showErr = errSec !== null && (okSec === null || errSec < okSec);
  const txt = showErr ? t.last_err : t.last_ok;
  if (!txt) return '';
  const sec = showErr ? errSec : okSec;
  return `<div class="faint" style="margin:-4px 0 10px">上次自动签到：${esc(txt)}${sec !== null ? '（' + fmtAgo(sec) + '）' : ''}</div>`;
}

// 自动签到开关：POST /api/checkin {action:'schedule',enabled}。
async function wbAutoToggle(on, btn) {
  btn.disabled = true;
  const r = await post('/api/checkin', { action: 'schedule', enabled: on }).catch((e) => ({ ok: false, message: String(e) }));
  btn.disabled = false;
  if (!r || !r.ok) { alert('操作失败：' + ((r && r.message) || '未知错误')); return; }
  render();
}

// ★ 2026-10-26（P0）：成长任务一键完成（17 项：事件链上报 + 领奖）。
async function wbGrowth() {
  if (!confirm('一键完成成长任务？\n\n会对 wb 族所有账号：读取任务清单 → 接受未完成任务 → 以桌面客户端指纹上报行为事件链 → 逐个领奖。\n全新账号约可拿 +1950 积分（部分任务需真实对话）。\n\n⚠ 这是对上游的主动写操作，请确认账号允许。')) return;
  const mk = modalShell('⚡ 成长任务执行中…',
    '<div id="growthBox"><div class="faint">正在逐个账号跑 17 项任务（可能需要 30-60 秒）…</div></div>');
  document.body.insertAdjacentHTML('beforeend', mk);
  const box = document.getElementById('growthBox');
  const r = await post('/api/checkin', { action: 'growth', platform: 'wb-gateway' }).catch((e) => ({ ok: false, message: String(e) }));
  if (!box) return;
  if (!r || !r.ok) { box.innerHTML = `<div class="note errnote">执行失败：${esc((r && r.message) || '未知错误')}</div>`; return; }
  const accs = r.accounts || [];
  box.innerHTML = accs.length ? accs.map((a) => {
    const d = a.result || {};
    const steps = (d.steps || []).map((s) => `· ${esc(s.action)}：${s.ok ? '✓' : '✗'}${s.message ? ' ' + esc(String(s.message).slice(0, 60)) : ''}${s.credit ? ' +' + s.credit : ''}`).join('<br>');
    return `<div class="note ${d.ok ? 'oknote' : 'errnote'}">
      <b>${esc(a.account)}</b> — 任务 ${d.total_tasks || 0} 项，待做 ${d.pending || 0}，领取 ${d.claimed || 0} 项，
      本次 <b>+${d.credit || 0} 积分</b>${d.energy ? ' +' + d.energy + ' 能量' : ''}
      <div class="faint" style="margin-top:6px;font-size:11.5px">${steps || esc(d.message || '')}</div>
    </div>`;
  }).join('') : '<div class="note">没有 wb 族账号</div>';
  setTimeout(() => render(), 1200);
}

async function wbGrowthToggle(on, btn) {
  btn.disabled = true;
  const r = await post('/api/checkin', { action: 'growth_schedule', enabled: on }).catch((e) => ({ ok: false, message: String(e) }));
  btn.disabled = false;
  if (!r || !r.ok) { alert('操作失败：' + ((r && r.message) || '未知错误')); return; }
  render();
}

// 模态底部附一行自动签到状态（只有 GET /api/checkin 带 daily/task；claim 响应没有）。
function fmtCheckinAuto(r) {
  if (!r || r.daily === undefined) return '';
  const t = r.task || {};
  const last = t.last_ok || t.last_err || '';
  return `<div class="faint" style="margin-top:6px">每日自动签到：${r.daily ? '已开启（每 12 小时一次，账号页可关）' : '未开启（账号页「自动签到」按钮可开）'}${last ? ' · 上次：' + esc(last) : ''}</div>`;
}

// wb 每日签到：GET=/api/checkin 查状态，POST=/api/checkin 领取。
async function wbCheckin() {
  const mk = modalShell('wb 每日签到（Buddy 加油站 100 积分）',
    `
      <div class="faint">先查各 wb 账号签到状态，再一键领取。仅 wb-gateway / codebuddy（CN）账号可签到。</div>
      <div class="modal-foot" style="margin-top:14px">
        <button class="btn" onclick="wbCheckinRun('status', this)">仅查状态</button>
        <button class="btn pri" onclick="wbCheckinRun('claim', this)">全部领取</button>
        <button class="btn" onclick="this.closest('.modal-mask').remove()">取消</button>
      </div>
      <div id="wbCheckinOut" style="margin-top:12px"></div>`);
  document.body.insertAdjacentHTML('beforeend', mk);
}

async function wbCheckinRun(action, btn) {
  btn.disabled = true;
  const out = document.getElementById('wbCheckinOut');
  out.innerHTML = '<div class="faint">执行中…</div>';
  try {
    // GET=查状态，POST=领取。api() 只接一个参数，写操作必须走 post()。
    const r = action === 'claim'
      ? await post('/api/checkin', {})
      : await api('/api/checkin');
    out.innerHTML = fmtCheckin(r);
  } catch (e) {
    out.innerHTML = `<div class="err">失败：${esc(e)}</div>`;
  } finally {
    btn.disabled = false;
  }
}

function fmtCheckin(r) {
  if (!r || !r.results || !r.results.length) return '<div class="empty">无 wb 账号（账号页先接入 wb-gateway / codebuddy）</div>';
  return `<table style="width:100%"><thead><tr><th>平台</th><th>账号</th><th>动作</th><th>结果</th></tr></thead><tbody>` +
    r.results.map((x) => {
      const d = x.data || {};
      let txt;
      if (!x.ok) txt = `<span class="err">${esc(d.error || '请求失败')}</span>`;
      // 业务失败（上游 200 但没给 data / 没给 code，多半是登录态失效或风控页）
      // 必须排在 status 分支之前：旧顺序下 d.active 缺失会被读成「活动正常」，
      // 用户看到「🕘 今日未签」以为还能签，点领取才报错。
      else if (d.ok === false) txt = `⛔ ${esc(d.status || d.error || '业务拒绝')}` +
        (d.code !== undefined && d.code !== null ? `<span class="faint">（code=${esc(d.code)}）</span>` : '');
      else if (x.action === 'status') {
        // checkin_status 回 {ok,active,today_checked_in,end_time}，要按人话渲染，
        // 否则会掉到最后那个 raw JSON 分支。
        const done = d.today_checked_in === true;
        const alive = d.active !== false;
        txt = `${done ? '✅ 今日已签' : (alive ? '🕘 今日未签' : '⛔ 活动未开启')}` +
          (d.end_time ? `<span class="faint"> · 截止 ${esc(String(d.end_time))}</span>` : '');
      // credit / streak_days / code 全部来自上游响应体，必须 esc 后再进 innerHTML：
      // 上游一旦回字符串（哪怕只是被篡改的 {"errno":"<img onerror=…>"}），未转义就等于面板内 XSS，
      // 而面板手里握着全部凭据。
      } else if (d.ok === true) txt = `✅ 成功 +${esc(d.credit)} 积分，连续 ${esc(d.streak_days)} 天`;
      else txt = esc(JSON.stringify(d).slice(0, 150));
      return `<tr><td>${esc(x.platform)}</td><td class="mono">${esc(x.account)}</td><td>${x.action === 'claim' ? '领取' : '查状态'}</td><td>${txt}</td></tr>`;
    }).join('') + `</tbody></table><div class="faint" style="margin-top:8px">共 ${r.count} 个账号</div>` + fmtCheckinAuto(r);
}

// ── M4 可视化（对齐 sub2api TokenUsageTrend/donut + dsh Σtoken 口径）──
// 零依赖纯 SVG：donut（平台输出占比）+ 面积图（token 趋势）+ 平台榜。

function donutSVG(items, size = 250, stroke = 30) {
  const total = items.reduce((s, d) => s + d.value, 0);
  if (!total) return '<div class="empty">暂无数据</div>';
  const pal = ['#4f9cf9', '#34d399', '#fbbf24', '#f472b6', '#a78bfa', '#38bdf8', '#94a3b8'];
  const R = (size - stroke) / 2, C = 2 * Math.PI * R, cx = size / 2;
  let acc = 0; const segs = [];
  items.forEach((d, i) => {
    const frac = d.value / total, len = frac * C;
    segs.push(`<circle cx="${cx}" cy="${cx}" r="${R}" fill="none" stroke="${pal[i % pal.length]}"
      stroke-width="${stroke}" stroke-dasharray="${len.toFixed(1)} ${(C - len).toFixed(1)}"
      stroke-dashoffset="${(-acc * C).toFixed(1)}" transform="rotate(-90 ${cx} ${cx})"/>`);
    acc += frac;
  });
  const top = items.slice(0, 3).map((d) => `${esc(d.label)} ${nfmt(d.value)}`).join(' · ');
  return `<svg viewBox="0 0 ${size} ${size}" width="${size}" height="${size}" style="max-width:260px">${segs.join('')}
    <text x="50%" y="47%" text-anchor="middle" class="donut-total">${nfmt(total)}</text>
    <text x="50%" y="58%" text-anchor="middle" class="donut-cap">输出 tokens</text></svg>
    <div class="faint" style="margin-top:8px">${top}</div>`;
}

function trendSVG(recent, w = 780, h = 190) {
  // recent 按分钟分桶聚合 tokensOut → 面积图
  const buckets = new Map();
  for (const e of recent) {
    const k = Math.floor((e.ts || 0) / 60); // 分钟桶
    const b = buckets.get(k) || { t: k * 60, v: 0 };
    b.v += (e.tout || 0);
    buckets.set(k, b);
  }
  const pts = [...buckets.values()].sort((a, b) => a.t - b.t);
  if (pts.length < 2) return '<div class="empty">数据不足，暂无趋势</div>';
  const max = Math.max(...pts.map((p) => p.v), 1);
  const X = (i) => (i / (pts.length - 1)) * w;
  const Y = (v) => h - 14 - (v / max) * (h - 30);
  const line = pts.map((p, i) => `${i ? 'L' : 'M'}${X(i).toFixed(1)} ${Y(p.v).toFixed(1)}`).join(' ');
  const area = `${line} L${w} ${h} L0 ${h} Z`;
  return `<svg viewBox="0 0 ${w} ${h}" width="100%" height="${h}" preserveAspectRatio="none">
    <defs><linearGradient id="ag" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#4f9cf9" stop-opacity="0.45"/><stop offset="100%" stop-color="#4f9cf9" stop-opacity="0.02"/>
    </linearGradient></defs>
    <path d="${area}" fill="url(#ag)"/><path d="${line}" fill="none" stroke="#4f9cf9" stroke-width="2" stroke-linejoin="round"/>
    <text x="${w - 8}" y="14" text-anchor="end" class="faint">峰值 ${nfmt(max)} tok/分</text></svg>`;
}

function topBars(plats, n = 8) {
  const top = plats.slice(0, n);
  if (!top.length) return '<div class="empty">暂无调用</div>';
  const max = Math.max(...top.map(([, d]) => d.tokensOut || 0), 1);
  return top.map(([p, d]) => {
    const w = Math.max(2, ((d.tokensOut || 0) / max) * 100);
    return `<div class="bar-row"><span class="bar-lb" title="${esc(p)}">${esc(p)}</span>
      <div class="bar-tr"><div class="bar-fill" style="width:${w.toFixed(1)}%"></div></div>
      <span class="bar-v mono">${nfmt(d.tokensOut || 0)}</span></div>`;
  }).join('');
}

// 用量时间窗：'all' | '5m' | '1h' | '24h'。右端留 60s 余量——事件 ts 可能略大于
// 查询时刻，不留余量会把刚发生的那一次请求切掉（实测踩过）。
let usageWin = 'all';
// 「最近请求」这一页要几条：服务端 list 与统计是两个口径（列表只是这一页），
// 所以必须带着 limit 请求，并且把 matched（窗口总数）一起显示出来。
let usageLimit = 60;
const USAGE_PAGE_MAX = 1000;
// 分组表（模型/平台/账号）一次显示多少名：服务端默认 30，上限 200。
let usageGroupLimit = 30;
const USAGE_GROUP_MAX = 200;
function setUsageGroupLimit(n) { usageGroupLimit = Math.max(10, Math.min(USAGE_GROUP_MAX, n)); route(); }
const USAGE_WINS = [['5m', '近 5 分钟', 300], ['1h', '近 1 小时', 3600], ['24h', '近 24 小时', 86400], ['all', '全部', 0]];
function usageQuery() {
  const parts = [];
  const w = USAGE_WINS.find((x) => x[0] === usageWin) || USAGE_WINS[3];
  if (w[2]) {
    const to = Math.floor(Date.now() / 1000) + 60;
    parts.push('from=' + (to - w[2]), 'to=' + to);
  }
  // 明细列表（/api/requests）用 limit；分组表（/api/usage 的 byModel 等）默认只回
  // 前 30 名，点「看全部」时把 usageGroupLimit 带上去（服务端同一个 limit 参数，上限 200）。
  parts.push('limit=' + usageLimit);
  parts.push('group_limit=' + usageGroupLimit);
  return '?' + parts.join('&');
}
function setUsageLimit(n) { usageLimit = Math.max(10, Math.min(USAGE_PAGE_MAX, n)); route(); }
function usageWinLabel() { return (USAGE_WINS.find((x) => x[0] === usageWin) || USAGE_WINS[3])[1]; }
function setUsageWin(k) { usageWin = k; route(); }

// ★ 2026-10-26（P3）：运行配置（热更新，改完立即生效，无需重启服务）。
// 语义说明写进每个字段的 hint —— 这些参数填错会直接影响线上行为，光给数字范围不够。
const RUNTIME_HINTS = {
  max_inflight: '单账号在单个模型上的最大并发。调高能提吞吐，但过高容易触发上游风控',
  upstream_timeout_s: '上游读写超时（秒）。长上下文/推理模型需要更长',
  max_account_tries: '一次请求最多换几个账号重试。换号会掉会话粘性与上游 prompt 缓存',
  per_ip_rpm: '每 IP 每分钟请求上限，0 = 不限',
  global_rpm: '全局每分钟请求上限，0 = 不限',
  key_rpm: '每把访问密钥每分钟上限，0 = 不限',
};
const RUNTIME_LABELS = {
  max_inflight: '单账号并发',
  upstream_timeout_s: '上游超时（秒）',
  max_account_tries: '最大重试账号数',
  per_ip_rpm: '每 IP 每分钟',
  global_rpm: '全局每分钟',
  key_rpm: '每密钥每分钟',
};
async function viewRuntime(el) {
  el.innerHTML = '<div class="card glass empty">加载运行配置…</div>';
  let r = null;
  try { r = await api('/api/runtime'); } catch (_) { r = null; }
  if (!r || !r.ok) { el.innerHTML = '<div class="card glass"><div class="note errnote">读取运行配置失败</div></div>'; return; }
  const c = r.cfg || {};
  const fields = c.fields || [];
  const upd = c.updated_at ? new Date(c.updated_at * 1000).toLocaleString() : '从未修改（使用默认值）';
  el.innerHTML = `<section class="card glass">
    <div class="card-t"><b>运行配置</b><span class="faint small" style="margin-left:8px">改完立即生效 · 无需重启 · 正在飞的请求不受影响</span></div>
    <div class="faint small" style="margin-bottom:12px">上次修改：${esc(upd)}。值存 <span class="mono">data/runtime.json</span>，重启保持。</div>
    ${fields.map((f) => {
      const v = c[f.name] ?? f.default;
      return `<div class="row" style="align-items:flex-start;margin-bottom:10px">
        <label style="min-width:150px;padding-top:6px"><b>${esc(RUNTIME_LABELS[f.name] || f.name)}</b><br>
          <span class="faint mono" style="font-size:11px">${esc(f.name)}</span></label>
        <div class="grow">
          <input class="q mono" id="rt_${esc(f.name)}" type="number" min="${f.min}" max="${f.max}" value="${v}" style="width:120px">
          <span class="faint" style="font-size:11.5px;margin-left:6px">范围 ${f.min}–${f.max}，默认 ${f.default}</span>
          <div class="faint" style="font-size:11.5px;margin-top:3px">${esc(RUNTIME_HINTS[f.name] || '')}</div>
        </div>
      </div>`;
    }).join('')}
    <div class="row" style="margin-top:14px">
      <button class="btn pri" onclick="runtimeSave()">保存并立即生效</button>
      <button class="btn gh" onclick="route()">刷新</button>
    </div>
    <div id="rtMsg" style="margin-top:10px"></div>
  </section>`;
}
async function runtimeSave() {
  const box = document.getElementById('rtMsg');
  const cfg = {};
  document.querySelectorAll('input[id^="rt_"]').forEach((i) => {
    const n = i.id.slice(3);
    const v = parseInt(i.value, 10);
    if (Number.isFinite(v)) cfg[n] = v;
  });
  if (box) box.innerHTML = '<span class="faint">保存中…</span>';
  const r = await post('/api/runtime', { cfg }).catch((e) => ({ ok: false, message: String(e) }));
  if (!box) return;
  if (!r || !r.ok) { box.innerHTML = `<div class="note errnote">保存失败：${esc((r && r.message) || '未知错误')}</div>`; return; }
  box.innerHTML = `<div class="note oknote">${esc(r.message || '已保存')}</div>`;
  setTimeout(() => route(), 900);
}
async function viewUsage(el) {
  const q = usageQuery();
  const [us, hist] = await Promise.all([api('/api/usage' + q), api('/api/requests' + q)]);
  // 同概览页：分母扣掉客户端取消，否则「按一次停止」就等于制造一次失败。
  const realReq = Math.max(0, (us.requests || 0) - (us.cancelled || 0));
  const rate = realReq ? Math.round((us.success / realReq) * 100) : 100;
  const plats = Object.entries(hist.by_platform || {}).sort((a, b) => (b[1].tokensOut || 0) - (a[1].tokensOut || 0));
  const donut = donutSVG(plats.slice(0, 7).map(([p, d]) => ({ label: p, value: d.tokensOut || 0 })));
  const trend = trendSVG(hist.recent || []);
  const bars = topBars(plats);
  // 列表与统计不同口径：必须明确说清「这只是最近 N 条、窗口里一共 M 条」，
  // 否则长窗口下（列表 60 / 统计 500）看起来像台账丢了数据。
  const matchedN = hist.matched || 0;
  const shownN = hist.shown || (hist.recent || []).length;
  let moreNote = '';
  if (hist.has_more) {
    moreNote = usageLimit >= USAGE_PAGE_MAX
      ? `<div class="note">已达单页上限 ${USAGE_PAGE_MAX} 条：用上面的时间窗缩小范围，或翻下面的平台归因表。</div>`
      : `<div class="row" style="margin-top:10px;gap:8px;align-items:center">
          <span class="faint small">还有 ${nfmt(matchedN - shownN)} 条更早的未显示</span>
          <button class="btn sm" onclick="setUsageLimit(${Math.min(USAGE_PAGE_MAX, usageLimit * 2)})">再多显示 ${Math.min(USAGE_PAGE_MAX, usageLimit * 2) - shownN} 条</button>
        </div>`;
  } else if (matchedN > shownN) {
    moreNote = `<div class="faint small" style="margin-top:8px">列表按时间倒序显示前 ${nfmt(shownN)} 条；共 ${nfmt(matchedN)} 条。</div>`;
  }
  const trendTitle = '输出 tokens 趋势（最近 ' + nfmt(shownN) + ' 条）';
  const rows = (hist.recent || [])
    .map((e) => `<tr>
      <td class="mono">${new Date((e.ts || 0) * 1000).toLocaleTimeString('zh-CN')}</td>
      <td>${esc(e.platform)}</td><td class="mono">${esc(e.model)}${(e.kind && e.kind !== 'chat') ? ' ' + pill('mut', '后台任务') : ''}</td>
      <td>${e.tin || 0} / ${e.tout || 0}</td><td>${e.ms || 0}ms</td>
      <td>${e.ok ? pill('ok', '成功') : (e.cancelled ? pill('mut', '已取消') : pill('err', '失败'))}</td></tr>`)
    .join('');
  // 模型 / 账号两个分组视图由服务端排好序（serde_json 的对象是键字典序，前端
  // 再排一遍既浪费又容易和 KPI 口径不一致）。
  const groupTable = (g, keyLabel, emptyTxt) => {
    const rows = (g && g.rows) || [];
    if (!rows.length) return `<div class="empty">${emptyTxt}</div>`;
    const more = g.total > rows.length
      ? (rows.length < USAGE_GROUP_MAX && g.total > rows.length
        ? `<div class="faint small" style="margin-top:8px">共 ${nfmt(g.total)} 个，只列请求数最多的 ${rows.length} 个 · <a href="#" onclick="event.preventDefault();setUsageGroupLimit(${Math.min(USAGE_GROUP_MAX, g.total)})">看全部 ${nfmt(Math.min(USAGE_GROUP_MAX, g.total))} 个</a></div>`
        : `<div class="faint small" style="margin-top:8px">共 ${nfmt(g.total)} 个，已列 ${rows.length} 个（列表上限 ${USAGE_GROUP_MAX}）</div>`)
      : '';
    return `<table><thead><tr><th>${keyLabel}</th><th>调用</th><th>成功/失败</th><th>tokens in/out</th></tr></thead><tbody>${
      rows.map((d) => `<tr><td title="${esc(d.key || '')}">${esc(d.key || '（未记录）')}</td><td>${nfmt(d.requests)}</td>
        <td>${nfmt(d.success)} / <span class="muted">${nfmt(d.failed)}</span>${d.cancelled ? ` / <span class="muted">取消 ${nfmt(d.cancelled)}</span>` : ''}</td>
        <td class="mono">${nfmt(d.tokensIn)} / ${nfmt(d.tokensOut)}</td></tr>`).join('')}</tbody></table>${more}`;
  };
  const winBtns = USAGE_WINS.map(([k, lb]) =>
    `<button class="btn sm ${usageWin === k ? 'pri' : ''}" onclick="setUsageWin('${k}')">${lb}</button>`).join('');
  // 窗口内 0 条时要说明「是这段时间真的没有请求」，而不是让人以为台账丢了。
  const winNote = us.requests ? '' : '<div class="note" style="margin-bottom:10px">该时间窗内没有请求：台账本身是全量累计的，切到「全部」看历史。</div>';
  const winRow = `<div class="row" style="margin-bottom:10px;align-items:center;gap:8px">
    <span class="faint small">时间窗</span>${winBtns}
    <span class="faint small" style="margin-left:auto">当前：${usageWinLabel()}${usageWin === 'all' ? '（累计）' : '（按请求时间过滤）'}</span>
  </div>${winNote}`;
  el.innerHTML = `
    ${winRow}
    <div class="grid g4">
      <div class="card glass kpi"><div class="lb">请求数</div><div class="vl">${nfmt(us.requests)}</div></div>
      <div class="card glass kpi"><div class="lb">成功</div><div class="vl" style="background:none;-webkit-text-fill-color:currentColor;color:var(--ok)">${nfmt(us.success)}</div></div>
      <div class="card glass kpi"><div class="lb">失败</div><div class="vl" style="background:none;-webkit-text-fill-color:currentColor;color:var(--err)">${nfmt(us.failed)}</div>${us.cancelled ? `<div class="ex">另有 ${nfmt(us.cancelled)} 次客户端取消（不计入失败与成功率）</div>` : ''}</div>
      <div class="card glass kpi"><div class="lb">成功率</div><div class="vl">${rate}%</div></div>
    </div>
    ${(us.ttfb && us.ttfb.samples) || (us.latency && us.latency.samples) ? `<div class="grid g2" style="margin-top:12px">
      ${us.ttfb && us.ttfb.samples ? `<div class="card glass kpi"><div class="lb">首字节 TTFB</div>
        <div class="vl">${us.ttfb.p50}<span style="font-size:14px">ms</span></div>
        <div class="ex">P95 ${us.ttfb.p95}ms · 平均 ${us.ttfb.avg}ms · ${nfmt(us.ttfb.samples)} 次流式样本</div></div>` : ''}
      ${us.latency && us.latency.samples ? `<div class="card glass kpi"><div class="lb">整轮耗时</div>
        <div class="vl">${us.latency.p50}<span style="font-size:14px">ms</span></div>
        <div class="ex">P95 ${us.latency.p95}ms · 平均 ${us.latency.avg}ms · ${nfmt(us.latency.samples)} 次样本</div></div>` : ''}
    </div>` : ''}
    <div class="grid g2">
      <section class="card glass"><h3>输出 tokens 分布（donut）</h3>${donut}</section>
      <section class="card glass"><h3>${trendTitle}</h3>${trend}</section>
    </div>
    <section class="card glass"><h3>平台输出榜</h3>${bars}</section>
    <section class="card glass"><h3>平台归因</h3>
      ${plats.length ? `<table><thead><tr><th>平台</th><th>调用</th><th>成功/失败</th><th>tokens in/out</th></tr></thead>
      <tbody>${plats.map(([p, d]) => `<tr><td>${esc(p)}</td><td>${d.requests}</td>
        <td>${d.success} / <span class="muted">${d.failed}</span>${d.cancelled ? ` / <span class="muted">取消 ${d.cancelled}</span>` : ''}</td>
        <td class="mono">${nfmt(d.tokensIn)} / ${nfmt(d.tokensOut)}</td></tr>`).join('')}</tbody></table>`
      : '<div class="empty">暂无调用</div>'}
    </section>
    <div class="grid g2">
      <section class="card glass"><h3>模型用量榜</h3>${groupTable(us.byModel, '模型', '该时间窗内没有模型调用')}</section>
      <section class="card glass"><h3>账号归因</h3>${groupTable(us.byAccount, '账号', '该时间窗内没有账号调用（旧台账没有账号字段）')}</section>
    </div>
    <section class="card glass"><h3>最近请求（显示 ${nfmt(hist.shown || 0)} / 共 ${nfmt(hist.matched || 0)} 条）</h3>
      ${rows ? `<table><thead><tr><th>时间</th><th>平台</th><th>模型</th><th>tokens</th><th>耗时</th><th>结果</th></tr></thead>
      <tbody>${rows}</tbody></table>` : '<div class="empty">暂无历史</div>'}
      ${moreNote}
      <p class="faint" style="margin-bottom:0">存储：${us.db ? 'SQLite（WAL，90 天 / 5000 条 prune）' : 'JSON 降级'} · 旧版数据已幂等导入</p>
    </section>`;
}

// ─────────────────────────────────────────────
// 关于页：版本 / 在线更新 / 备份恢复 / 使用说明 / 免责声明
// ─────────────────────────────────────────────
// ─────────────────────────────────────────────
// 访问密钥：把网关借出去时，限制「这把 key 能调哪些模型」
// ─────────────────────────────────────────────
let KEYS_CACHE = [];

function keysCard() {
  const rows = KEYS_CACHE || [];
  const body = rows.length
    ? `<table style="margin-top:10px"><thead><tr><th>名字</th><th>能调的模型</th><th>备注</th><th>状态</th><th style="width:210px">操作</th></tr></thead><tbody>`
      + rows.map((k) => '<tr>'
          + '<td><b>' + esc(k.name) + '</b><br><span class="faint mono" style="font-size:11px">' + esc(k.id) + '</span></td>'
          + '<td>' + ((k.allow_models || []).length
            ? (k.allow_models || []).map((m) => '<code class="mono" style="font-size:11px">' + esc(m) + '</code>').join(' ')
            : '<span class="faint">不限制（全部模型）</span>') + '</td>'
          + '<td class="faint">' + esc(k.note || '—') + '</td>'
          + '<td>' + (k.key_broken ? pill('err', '密文解不开') : k.enabled ? pill('ok', '启用') : pill('mut', '停用')) + '</td>'
          + '<td>'
            + '<button class="btn sm" onclick="keyEdit(\'' + att(k.id) + '\')">改白名单</button> '
            + '<button class="btn sm" onclick="keyToggle(\'' + att(k.id) + '\', ' + (k.enabled ? 'false' : 'true') + ')">' + (k.enabled ? '停用' : '启用') + '</button> '
            + '<button class="btn sm" onclick="keyRotate(\'' + att(k.id) + '\')" title="换一把新密钥，旧的立刻失效">轮换</button> '
            + '<button class="btn sm" onclick="keyDelete(\'' + att(k.id) + '\')">删除</button>'
          + '</td></tr>').join('')
      + `</tbody></table>`
    : '<div class="empty" style="margin-top:10px">还没有访问密钥：不建也能正常用（走面板令牌 / 本机回环免鉴权）</div>';
  return `<section class="card glass">
    <div class="card-t"><b>访问密钥</b><span class="faint small" style="margin-left:8px">借出去的 key 只能调用指定模型</span></div>
    <div class="faint small" style="margin-bottom:10px">面板令牌是「能不能进面板」的全通票；访问密钥是「这把 key 能调哪些模型、能调多快」，可建多把。用法：<code class="mono">Authorization: Bearer &lt;访问密钥&gt;</code> 调 <code class="mono">/v1/*</code>。白名单留空 = 不限制；写 <code class="mono">模型名</code> 则带平台后缀也放行，写 <code class="mono">模型@平台</code> 只认那一个组合。每把 key 另有独立速率上限（见顶部「网关信息」里的限流一行），某一把刷爆不会拖慢另外几把和面板。</div>
    <div class="row"><button class="btn pri" onclick="keyCreate()">＋ 新建访问密钥</button>
      <button class="btn gh" onclick="keysRefresh()">刷新</button></div>
    ${body}
  </section>`;
}

async function loadKeys() {
  try {
    const r = await api('/api/keys');
    KEYS_CACHE = (r && r.rows) || [];
  } catch (_) { KEYS_CACHE = []; }
}

// ─────────────────────────────────────────────
// 模型链（combo）：一个模型名背后是一条有序候选链
// ─────────────────────────────────────────────
let COMBOS_CACHE = [];

function comboStrategyTxt(s) {
  return s === 'round-robin' ? '轮转' : '回退';
}

function combosCard() {
  const rows = COMBOS_CACHE || [];
  const body = rows.length
    ? '<table style="margin-top:10px"><thead><tr><th>链</th><th>候选（按顺序）</th><th>策略</th><th>现在能回退几次</th><th>状态</th><th style="width:170px">操作</th></tr></thead><tbody>'
      + rows.map((c) => {
          const eff = c.effective || [];
          const dead = c.unavailable || [];
          const cand = (c.models || []).map((m) => {
            const bad = dead.indexOf(m) >= 0;
            return '<code class="mono" style="font-size:11px' + (bad ? ';opacity:.45;text-decoration:line-through' : '') + '" title="' + att(bad ? '这个候选现在调不通（模型不在 /v1/models 里：平台没凭据/停用/下线）' : '可用') + '">' + esc(m) + '</code>';
          }).join(' → ');
          const effCell = eff.length
            ? '<b>' + eff.length + '</b> <span class="faint small">/ ' + (c.models || []).length + '</span>'
              + (dead.length ? '<br><span class="pill warn small" style="margin-top:4px">' + dead.length + ' 个调不通</span>' : '')
            : '<span class="pill err small">全部调不通</span>';
          return '<tr>'
            + '<td><b>' + esc(c.name) + '</b><br><span class="faint mono" style="font-size:11px">' + esc(c.id) + '</span></td>'
            + '<td>' + cand + '</td>'
            + '<td>' + esc(comboStrategyTxt(c.strategy)) + (c.strategy === 'round-robin' ? ' <span class="faint small">粘 ' + (c.sticky_limit || 1) + ' 轮</span>' : '') + '</td>'
            + '<td>' + effCell + '</td>'
            + '<td>' + (c.enabled ? pill('ok', '启用') : pill('mut', '停用')) + '</td>'
            + '<td>'
              + '<button class="btn sm" onclick="comboEdit(\'' + att(c.id) + '\')">编辑</button> '
              + '<button class="btn sm" onclick="comboToggle(\'' + att(c.id) + '\', ' + (c.enabled ? 'false' : 'true') + ')">' + (c.enabled ? '停用' : '启用') + '</button> '
              + '<button class="btn sm" onclick="comboDelete(\'' + att(c.id) + '\')">删除</button>'
            + '</td></tr>';
        }).join('')
      + '</tbody></table>'
    : '<div class="empty" style="margin-top:10px">还没有模型链。建一条之后，客户端填 <code class="mono">combo:&lt;名字&gt;</code> 当模型名即可</div>';
  return '<section class="card glass">'
    + '<div class="card-t"><b>模型链</b><span class="faint small" style="margin-left:8px">一个模型名 = 一条有序候选链，前一个挂了自动换下一个</span></div>'
    + '<div class="faint small" style="margin-bottom:10px">额度墙、地域拒绝、凭据过期、上游 5xx —— 这些对客户端都是「这轮挂了」，但本机明明还有别的平台能答。模型链把这次失败变成你看不见的一次重试：按顺序走候选，第一个产出内容的赢。用法：任意 OpenAI 客户端把模型名填成 <code class="mono">combo:&lt;链名&gt;</code>。'
+ '<br>策略：<b>回退</b> = 永远从链头开始（链是质量阶梯时选它）；<b>轮转</b> = 每轮换个起点（一组等价模型分摊负载时选它）。'
+ '<br>⚠ 「现在能回退几次」只数当前真能调的候选：写了 3 个但两个平台没凭据，实际只有 1 层保险 —— 划掉的那些就是来不及救你的。</div>'
+ '<div class="row"><button class="btn pri" onclick="comboCreate()">＋ 新建模型链</button>'
+ '  <button class="btn gh" onclick="combosRefresh()">刷新</button></div>'
+ body
+ '</section>';
}

async function loadCombos() {
  try {
    const r = await api('/api/combos');
    COMBOS_CACHE = (r && r.rows) || [];
  } catch (_) { COMBOS_CACHE = []; }
}

async function combosRefresh() {
  await loadCombos();
  route();
}

async function comboCreate() {
  const name = window.prompt('链名（客户端里填 combo:名字）', '');
  if (name === null) return;
  const models = window.prompt('候选模型，按优先级用逗号分隔（可写 模型@平台）', '');
  if (models === null) return;
  const list = models.split(',').map((s) => s.trim()).filter(Boolean);
  const strat = window.prompt('策略：fallback（回退）/ round-robin（轮转）', 'fallback');
  if (strat === null) return;
  try {
    const r = await api('/api/combos', { method: 'POST', body: JSON.stringify({ action: 'add', name: name.trim(), models: list, strategy: strat.trim() === 'round-robin' ? 'round-robin' : 'fallback' }) });
    await loadCombos();
    route();
    if (r && r.combo) window.alert('已建模型链 ' + r.combo.id + '\n客户端里把模型名填成这个即可。');
  } catch (e) { window.alert('建链失败：' + (e.message || e)); }
}

async function comboEdit(id) {
  const cur = (COMBOS_CACHE || []).find((c) => c.id === id);
  if (!cur) return;
  const models = window.prompt('候选模型，按优先级用逗号分隔', (cur.models || []).join(', '));
  if (models === null) return;
  const list = models.split(',').map((s) => s.trim()).filter(Boolean);
  try {
    await api('/api/combos', { method: 'POST', body: JSON.stringify({ action: 'update', id: id, models: list }) });
    await loadCombos();
    route();
  } catch (e) { window.alert('改链失败：' + (e.message || e)); }
}

async function comboToggle(id, enabled) {
  try {
    await api('/api/combos', { method: 'POST', body: JSON.stringify({ action: 'update', id: id, enabled: enabled }) });
    await loadCombos();
    route();
  } catch (e) { window.alert('操作失败：' + (e.message || e)); }
}

async function comboDelete(id) {
  if (!window.confirm('删除这条链？用 combo: 调它的客户端会立刻 404。')) return;
  try {
    await api('/api/combos', { method: 'POST', body: JSON.stringify({ action: 'remove', id: id }) });
    await loadCombos();
    route();
  } catch (e) { window.alert('删除失败：' + (e.message || e)); }
}

// 「刷新」要能看见变化：loadKeys 只填缓存，不重渲染 —— 直接拿它当按钮处理器，
// 点下去界面纹丝不动，用户只会以为按钮坏了。
async function keysRefresh() {
  await loadKeys();
  route();
}

// 白名单里匹配不到任何模型的条目：拼错（kilo-auto/fre）、模型已下线、平台没凭据都算。
// 这类错误默认是**静默**的 —— 请求只会 403 model_not_allowed，用户盯着白名单列表
// 看不出一长串名字里哪个字母错了。所以创建/修改后必须点名报出来。
function keyUnknownNote(r) {
  const bad = (r && r.unknown_models) || [];
  if (!bad.length) return '';
  return '\n\n⚠ 这些条目现在匹配不到任何可用模型：' + bad.join('、')
    + '\n（写错字母 / 模型已下线 / 平台还没接入都会这样。把模型名从上面「模型」页或 /v1/models 里复制最稳妥。）';
}
// 落盘失败必须让用户看见：persisted:false 时内存里改了、盘上没改 ——
// 重启后「已删除的 key」会复活、「刚改的白名单」会回退，而且没有任何提示。
function keyPersistNote(r) {
  return (r && r.persisted === false)
    ? '\n\n⚠ 注意：这次改动没能写进 data/keys.json（磁盘满？目录只读？），重启后会回退到旧状态。'
    : '';
}

async function keyCreate() {
  const name = (prompt('给这把 key 起个名字（写清给谁用，比如「部门脚本」）：') || '').trim();
  if (!name) return;
  const raw = prompt('允许调用哪些模型？（留空 = 不限制；多个用逗号分隔，可写 模型@平台 精确限定）\n模型名最稳妥是从 http://127.0.0.1:8800/v1/models 复制。', '') || '';
  const allow = raw.split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
  const r = await post('/api/keys', { action: 'add', name: name, allow_models: allow });
  if (!r || !r.ok) { alert('创建失败：' + ((r && r.message) || '未知错误')); return; }
  alert('已创建访问密钥（只显示这一次，请立刻复制保存）：\n\n' + (r.key || '') + keyPersistNote(r) + keyUnknownNote(r));
  await loadKeys();
  route();
}

async function keyEdit(id) {
  const cur = (KEYS_CACHE || []).find((k) => k.id === id);
  const raw = prompt('允许调用哪些模型？（留空 = 不限制；多个用逗号分隔）',
    cur && cur.allow_models ? cur.allow_models.join(',') : '');
  if (raw === null) return;
  const allow = raw.split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
  const r = await post('/api/keys', { action: 'update', id: id, allow_models: allow });
  if (!r || !r.ok) { alert('修改失败：' + ((r && r.message) || '未知错误')); return; }
  if (keyPersistNote(r) || keyUnknownNote(r)) alert('白名单已改' + keyPersistNote(r) + keyUnknownNote(r));
  await loadKeys();
  route();
}

async function keyToggle(id, enabled) {
  const r = await post('/api/keys', { action: 'update', id: id, enabled: enabled });
  if (!r || !r.ok) { alert('操作失败：' + ((r && r.message) || '未知错误')); return; }
  if (keyPersistNote(r)) alert((enabled ? '已启用' : '已停用') + keyPersistNote(r));
  await loadKeys();
  route();
}

async function keyRotate(id) {
  if (!confirm('轮换密钥？\n\n旧密钥立刻失效：正在用它的脚本/客户端都要换成新的。')) return;
  const r = await post('/api/keys', { action: 'update', id: id, rotate: true });
  if (!r || !r.ok) { alert('轮换失败：' + ((r && r.message) || '未知错误')); return; }
  alert('新密钥（只显示这一次，请立刻复制保存）：\n\n' + (r.key || '') + keyPersistNote(r));
  await loadKeys();
  route();
}

async function keyDelete(id) {
  if (!confirm('删除这把访问密钥？用它的客户端会立刻收到 401。')) return;
  const r = await post('/api/keys', { action: 'remove', id: id });
  if (!r || !r.ok) { alert('删除失败：' + ((r && r.message) || '未知错误')); return; }
  // ★ 删除是最危险的一种「没落盘」：内存里没了、盘上还在，重启后这把已吊销的 key 会复活。
  if (keyPersistNote(r)) alert('已删除（内存里）' + keyPersistNote(r));
  await loadKeys();
  route();
}

async function viewAbout(el) {
  el.innerHTML = `<div class="card glass empty">加载中…</div>`;
  let up = { ok: false };
  try { up = await api('/api/update-check'); } catch (_) { up = { ok: false, note: '更新检查失败' }; }
  let hh = null;
  try { hh = await api('/api/health'); } catch (_) { hh = null; }
  // 访问密钥 / 模型链：拿不到就让卡片渲染成「空白表」，不阻塞整页。
  await loadKeys();
  await loadCombos();
  const lg = (hh && hh.logs) || null, ar = (hh && hh.archives) || null, db = (hh && hh.db) || null;
  const fpLedger = db ? (fmtBytes(db.bytes) + ' · ' + db.events + ' 条事件') : '—';
  const fpLogs = lg ? (fmtBytes(lg.bytes) + ' · ' + lg.files + ' 个文件') : '—';
  const fpArch = ar ? (fmtBytes(ar.bytes) + ' · ' + ar.files + ' 个文件') : '—';
  const fpWarn = (db && db.backend !== 'sqlite') ? '<div class="note errnote" style="margin-top:8px">台账降级为 JSON 模式（SQLite 打不开）：建议检查数据目录权限后重启</div>' : '';
  // 落盘失败：面板平时一律回「已保存」（各 Store 只 warn 不抛错），重启后配置会回退。
  // 这条是唯一能让人看见「刚才那下其实没写进磁盘」的地方，所以在「关于」页顶端显眼位置。
  const pe = (hh && hh.persist_error) || null;
  const persistWarn = pe ? ('<div class="card glass" style="border-color:#a33">'
    + '<div class="card-t"><b style="color:#c33">上次写盘失败</b></div>'
    + '<div class="small">文件 <code class="mono">' + esc(pe.file || '?') + '</code>（' + esc(fmtAgo(pe.s_ago || 0)) + '）</div>'
    + '<div class="small faint" style="margin-top:6px">' + esc(pe.error || '') + '</div>'
    + '<div class="small" style="margin-top:6px">这次改动<b>没有</b>写进磁盘，重启后会回退到旧状态。'
    + '多半是磁盘满、数据目录只读或文件被别的程序占用；处理后再改一次即可，成功后这条会自己消失。</div>'
    + '</div>') : '';
  const L = up.local || {};
  const R = up.remote || {};
  const localV = `${L.version || '0.1.0'} · ${L.milestone || ''}`;
  const remoteTxt = R.reachable
    ? `${R.sha ? R.sha.slice(0, 8) : '—'}（${R.date || ''}）`
    : '远端不可达（离线）';
  el.innerHTML = `
  ${persistWarn}
  <section class="card glass">
    <div class="card-t"><b>aigw v2</b> 免费 AI 额度 → 本地 OpenAI 兼容端点（一包多程）</div>
    <div class="kv">
      <div><span>本地版本</span><b>${esc(localV)}</b></div>
      <div><span>远端最新 commit</span><b>${esc(remoteTxt)}</b></div>
      <div><span>数据目录</span><code class="mono">${esc(L.data_dir || '—')}</code></div>
    </div>
    <div class="row" style="margin-top:12px">
      <button class="btn" onclick="checkUpdate()">⟳ 检查更新</button>
      <button class="btn pri" onclick="backupNow()">⬇ 备份数据</button>
      <button class="btn" onclick="restorePick()">⬆ 恢复备份</button>
    </div>
    <div id="upBox"></div>
    <div class="faint small" style="margin-top:8px">${esc(up.note || '')}</div>
  </section>

  ${combosCard()}

  ${keysCard()}

  <section class="card glass">
    <div class="card-t"><b>数据占用</b><span class="faint small" style="margin-left:8px">全部在本机数据目录，不上传</span></div>
    <div class="kv">
      <div><span>用量台账（SQLite）</span><b>${esc(fpLedger)}</b></div>
      <div><span>运行日志</span><b>${esc(fpLogs)}</b></div>
      <div><span>备份留档</span><b>${esc(fpArch)}</b></div>
    </div>
    <div class="faint small" style="margin-top:8px">自动上限：台账保留 90 天 / 最多 5000 条；日志 4MB×5 滚动；留档每族最多 6 份。超出会自动清理，无需手动维护。</div>
    ${fpWarn}
  </section>

  <section class="card glass">
    <div class="card-t"><b>使用说明</b></div>
    <ol class="docs">
      <li><b>接入源</b>：按分组接入——开放平台 API（填 Key）、订阅反代（OAuth 跳转/设备码）、客户端反代（网页凭据）、公益站（聚合中转）、自定义 API（自填协议/URL/Key 自动同步模型）。</li>
      <li><b>转发调用</b>：本地端点 <code class="mono">http://127.0.0.1:8800/v1</code>，四套协议都通——OpenAI <code class="mono">/v1/chat/completions</code>、Anthropic <code class="mono">/v1/messages</code>（含工具调用，另有 <code class="mono">/v1/messages/count_tokens</code> 预算接口）、OpenAI Responses <code class="mono">/v1/responses</code>（Codex 类客户端）、模型清单 <code class="mono">/v1/models</code>；Claude Code / Codex / 任意 OpenAI SDK 填 BaseURL 即可。</li>
      <li><b>账号</b>：设备码/跳转登录自动入库；手动粘贴 Cookie/Key 走「保存凭据」。secret 仅面板后台使用，账号页只显示掩码。</li>
      <li><b>代理</b>：顶部「⇅ 代理设置」切换 全部直连 / 全局代理 / 绕过大陆，所有流程（登录、上游、模型同步）即时生效，无需重启。</li>
      <li><b>用量</b>：SQLite(WAL) 台账，总览页含调用趋势/平台占比图表。</li>
      <li><b>会话粘性</b>：同一对话优先复用同一账号（conversation_id 系列 → 最后一条用户消息指纹），降低额度碎片。</li>
    </ol>
  </section>

  <section class="card glass">
    <div class="card-t"><b>免责声明</b></div>
    <div class="docs">
      <p>1. 本面板仅用于<u>个人学习与技术研究</u>，聚合接入的各平台额度/账号均为各平台官方授权的个人使用权；请遵守各平台服务条款，勿用于商业售卖、批量滥用或任何违法违规用途。</p>
      <p>2. 各「客户端反代」「公益站」接入涉及网页版逆向/中转行为，平台可能随时调整接口或风控，导致某一路径失效——面板不保证长期可用性，失效请及时切换接入源。</p>
      <p>3. 面板为本地/自托管服务，凭据（Cookie / Token / API Key）仅存储在本机 <code class="mono">%LOCALAPPDATA%\\aigw2\\data</code>，不会上传第三方；请自行保管好数据目录，避免泄露给他人。</p>
      <p>4. 使用本面板产生的任何后果（账号风控、额度损失、法律风险）由使用者自行承担，项目作者不对因使用本面板造成的任何直接或间接损失负责。</p>
      <p>5. 请勿将本面板部署到公网并暴露管理界面；如需远程使用，请置于可信内网并自行加鉴权。</p>
    </div>
  </section>

  <section class="card glass">
    <div class="card-t"><b>在线更新</b></div>
    <div class="docs small">
      <p>本地为 Rust 工程（aigw2 workspace）。更新流程：<code class="mono">git pull</code> → <code class="mono">cargo build</code> → 用新 <code class="mono">aigw.exe</code> 与 <code class="mono">providers/</code> 覆盖部署目录 <code class="mono">%LOCALAPPDATA%\\aigw2</code> → 重启计划任务 Aigw2Service。更新前建议先「⬇ 备份数据」。</p>
    </div>
  </section>`;
}

async function checkUpdate() {
  const box = $('#upBox');
  if (box) box.innerHTML = '<div class="faint">检查中…</div>';
  try {
    const r = await api('/api/update-check');
    const R = r.remote || {};
    const rtxt = R.reachable ? `最新 commit <b>${esc(R.sha ? R.sha.slice(0, 8) : '—')}</b>（${esc(R.date || '')}）` : '<b>远端不可达</b>（离线或 GitHub 被墙，可经「代理设置」绕过）';
    if (box) box.innerHTML = `<div class="note">${rtxt}</div>`;
  } catch (e) {
    if (box) box.innerHTML = `<div class="note errnote">${esc(e.message)}</div>`;
  }
}

// 备份：POST /api/backup/export → 下载 JSON 文件（含凭据，可恢复）
// 旧实现取 GET /api/backup（脱敏版）→ 备份文件里没有 accounts/custom 内容，
// 「恢复」回来等于什么都没恢复。脱敏版只留给「看看备份范围」。
async function backupNow() {
  try {
    const r = await post('/api/backup/export', {});
    // post() 失败时返回 {ok:false,message}（HTTP 500 也可能是 {ok:false,message:'HTTP 500'}），
    // 旧实现不看 ok 直接下载 → 用户拿到一个内容为错误 JSON 的「备份文件」，还以为备份成功。
    if (!r || r.ok === false) throw new Error((r && r.message) || '备份接口返回失败');
    if (!Array.isArray(r.files)) throw new Error('备份接口返回格式不对（缺 files）');
    const blob = new Blob([JSON.stringify(r, null, 2)], { type: 'application/json' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = `aigw2-backup-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(a.href);
    const box = $('#upBox');
    // checkpoint 失败时后端会给出 checkpoint_warning：说明 panel.db 可能不含最新用量
    // 台账（真实台账还在 -wal 里，已一并导出）。不能只显示「已导出 N 个文件」了事。
    const warn = r.checkpoint_warning ? `<br><span class="warn">⚠ ${esc(r.checkpoint_warning)}</span>` : '';
    if (box) box.innerHTML = `<div class="note oknote">已导出 ${r.count || 0} 个数据文件（${esc(r.dir)}）${warn}<br><b>此文件含明文凭据，请勿上传或分享</b></div>`;
  } catch (e) {
    const box = $('#upBox');
    if (box) box.innerHTML = `<div class="note errnote">备份失败：${esc(e.message)}</div>`;
  }
}

// 恢复：选择备份 JSON → POST /api/backup
async function restorePick() {
  const inp = document.createElement('input');
  inp.type = 'file';
  inp.accept = '.json,application/json';
  inp.onchange = async () => {
    const f = inp.files && inp.files[0];
    if (!f) return;
    const box = $('#upBox');
    if (box) box.innerHTML = '<div class="faint">恢复中…（重启服务后生效）</div>';
    try {
      const txt = await f.text();
      const data = JSON.parse(txt);
      if (!Array.isArray(data.files)) throw new Error('备份文件格式不对（缺 files 数组）');
      const r = await post('/api/backup', { files: data.files });
      if (!r || r.ok === false) throw new Error((r && r.message) || '恢复接口返回失败');
      // skipped 里可能是「panel.db 被服务占用」这类必须让用户看到的失败：只报
      // restored 数量会让用户以为连台账一起恢复好了。
      const sk = Array.isArray(r.skipped) && r.skipped.length
        ? `<br><span class="warn">⚠ 跳过 ${r.skipped.length} 个：${r.skipped.map(esc).join('；')}</span>`
        : '';
      // 数据库文件是「暂存 + 下次启动落地」的：服务运行时 SQLite 独占打开 panel.db，
      // 就地覆盖必失败。必须让用户明确知道「已暂存、重启才生效」，否则重启后发现
      // 用量数字没变会以为备份坏了。
      const staged = Array.isArray(r.db_staged) ? r.db_staged : [];
      const dbw = r.db_restored === false
        ? `<div class="note warn">⚠ 数据库未恢复：${esc(r.message || '服务运行中无法覆盖 panel.db，请停止服务后再恢复一次')}</div>`
        : staged.length
        ? `<div class="note warn">⚠ 数据库（${staged.map(esc).join('、')}）已暂存，<b>重启 aigw 服务后才会生效</b>；服务运行中无法直接覆盖 panel.db。</div>`
        : '';
      if (box) box.innerHTML = `<div class="note oknote">${esc(r.note || '已恢复')}（${r.restored || 0} 个文件）${sk}</div>${dbw}`;
    } catch (e) {
      if (box) box.innerHTML = `<div class="note errnote">恢复失败：${esc(e.message)}</div>`;
    }
  };
  inp.click();
}

// ─────────────────────────────────────────────
// 路由
// ─────────────────────────────────────────────
async function route() {
  const v = (location.hash || '#/overview').replace('#/', '');
  const conf = VIEWS[v] || VIEWS.overview;
  $('#ttl').textContent = conf.t;
  $('#stl').textContent = conf.s;
  renderHdBtns(VIEWS[v] ? v : 'overview');
  document.querySelectorAll('.navbtn').forEach((b) => b.classList.toggle('on', b.dataset.v === (VIEWS[v] ? v : 'overview')));
  const el = $('#view');
  el.innerHTML = '<div class="card glass empty">加载中…</div>';
  try {
    await conf.f(el);
  } catch (e) {
    el.innerHTML = `<div class="card glass empty">加载失败：${esc(e.message)}</div>`;
  }
}
function render() { route(); }
window.addEventListener('hashchange', route);
document.querySelectorAll('.navbtn').forEach((b) => b.addEventListener('click', () => { location.hash = '#/' + b.dataset.v; }));

(async () => {
  document.getElementById('foot').textContent = `BUILD ${BUILD} · JS就绪`;
  try {
    const h = await api('/api/health');
    document.getElementById('foot').textContent = `BUILD ${BUILD} · ${h.milestone}`;
  } catch (_) { /* health 失败保留 JS就绪 标记 */ }
  route();
})();
