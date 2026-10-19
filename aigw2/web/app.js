// aigw v2 控制台 · hash 路由 SPA（M5 管理面）
// 2026-10-19：接入源页（73 平台分组卡片 + 跳转登录 + 手动粘贴凭据闭环 + 自定义 API + 公益站）
// 自诊断：BUILD 指纹 + 全局错误上屏（页脚），用于区分浏览器缓存/环境问题
window.addEventListener('error', (e) => {
  const f = document.getElementById('foot');
  if (f) { f.textContent = '⚠ JS错误: ' + String(e.message || 'unknown').slice(0, 60); f.style.color = '#f06a6a'; }
});
const BUILD = 'v20261019g';

// 刷新公益站聚合（拉聚合导航 → 合并；已接入的保留）
async function refreshFree() {
  const r = await post('/api/free', { action: 'refresh' }).catch((e) => ({ ok: false, message: e.message }));
  alert(r.ok ? r.message : ('刷新失败：' + (r.message || '网络错误')));
  if (r.ok) { SRC_OPEN['free'] = true; srcRerender(); }
}

const $ = (s) => document.querySelector(s);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) =>
  ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));

async function api(path) {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`${path} → HTTP ${r.status}`);
  return r.json();
}
async function post(path, body) {
  const r = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body || {}),
  });
  return r.json();
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
    ['bypass_cn', '绕过大陆', '大陆域名直连，其余走代理（推荐给 CODEX/Kimi 等海外平台）'],
  ];
  const mk = `<div class="modal-mask" onclick="if(event.target===this)this.remove()"><div class="modal">
    <div class="modal-hd">代理设置<b class="x" onclick="this.closest('.modal-mask').remove()">✕</b></div>
    <div class="modal-bd">
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
      <div class="modal-foot">
        <button class="btn pri" onclick="saveProxy()">保存并应用</button>
      </div>
    </div>
  </div></div>`;
  document.body.insertAdjacentHTML('beforeend', mk);
}
async function saveProxy() {
  const mode = $('#px-mode').value || 'direct';
  const url = $('#px-url').value.trim();
  const r = await post('/api/proxy', { mode, url });
  const btn = document.querySelector('.modal-foot .btn.pri');
  if (btn) { btn.textContent = r.ok ? '✓ 已应用（即时生效）' : ('✗ ' + (r.message || '失败')); setTimeout(() => btn.textContent = '保存并应用', 2000); }
}

// 服务网关信息弹窗（地址 + KEY + 模型）
async function showGatewayInfo() {
  const [h, pl] = await Promise.all([api('/api/health').catch(() => null), api('/api/platforms').catch(() => null)]);
  const base = location.origin;
  const modelsN = pl ? pl.total : '—';
  const mk = `<div class="modal-mask" onclick="if(event.target===this)this.remove()"><div class="modal">
    <div class="modal-hd">服务网关信息<b class="x" onclick="this.closest('.modal-mask').remove()">✕</b></div>
    <div class="modal-bd">
      <label>代理地址（base_url）</label>
      <input class="q mono" readonly value="${esc(base)}/v1" onclick="this.select();copyText(this.value)">
      <label>API Key</label>
      <input class="q mono" readonly value="admin" onclick="this.select();copyText(this.value)">
      <label>可用模型</label>
      <div class="faint" style="font-size:12px">${modelsN} 个平台 · 模型名写 <code>模型id@平台</code> 或任意清单内模型</div>
      <div class="modal-foot">
        <button class="btn pri" onclick="navigator.clipboard.writeText('${esc(base)}/v1');this.textContent='已复制地址';setTimeout(()=>this.textContent='复制地址',1200)">复制地址</button>
        <button class="btn" onclick="navigator.clipboard.writeText('admin');this.textContent='已复制KEY';setTimeout(()=>this.textContent='复制KEY',1200)">复制 KEY</button>
      </div>
    </div>
  </div></div>`;
  document.body.insertAdjacentHTML('beforeend', mk);
}
async function copyText(t) { try { await navigator.clipboard.writeText(t); } catch (_) { /* 用户手动复制 */ } }

// ─────────────────────────────────────────────
// 接入源页：分组卡片 + 登录面板 + 自定义 API + 公益站
// ─────────────────────────────────────────────
let SRC_Q = '';
let SRC_HELP = false;
let SRC_OPEN = {};   // 分组收起状态：group → bool（默认收起 false）
let SRC_EXPAND = null; // 展开的平台卡片 id

// 分组规则（用户拍板）：
//   custom-*        → 自定义 API（最前，默认展开）
//   api-* / minimax → 开放平台 API（填 Key 即用，默认收起）
//   sub-*           → 订阅反代（OAuth/跳转，默认收起）
//   free-*          → 公益站（填 Key 即用，默认收起）
//   其余（apk-*/web-*/wb-*/anon-*/kilo/pollinations/codex/kimi-code）→ 客户端反代（默认收起）
function srcGroupOf(p) {
  if (p.id.startsWith('custom-')) return 'custom';
  if (p.id.startsWith('api-') || p.id === 'minimax') return 'official';
  if (p.id.startsWith('sub-')) return 'sub';
  if (p.id.startsWith('free-')) return 'free';
  return 'client';
}
const GROUP_META = {
  custom:   { t: '自定义 API', desc: '自填 名称/协议/URL/Key，自动同步模型（CherryStudio 式）', open: true },
  official: { t: '开放平台 API', desc: '官方平台 API Key 直连', open: false },
  sub:      { t: '订阅反代', desc: '订阅账号 OAuth 授权 → 网关反代', open: false },
  client:   { t: '客户端反代', desc: '网页版 / 客户端凭据（Cookie / Token）', open: false },
  free:     { t: '公益站', desc: '公益中转聚合站（排除官方 API）', open: false },
};

async function viewSources(el) {
  const pl = await api('/api/platforms');
  const rows = pl.rows.filter((p) => {
    if (!SRC_Q) return true;
    const q = SRC_Q.toLowerCase();
    return (p.name || '').toLowerCase().includes(q) || (p.id || '').toLowerCase().includes(q) || (p.notes || '').toLowerCase().includes(q);
  });
  const groups = ['custom', 'official', 'sub', 'client', 'free'];
  const byGroup = {};
  for (const g of groups) byGroup[g] = rows.filter((p) => srcGroupOf(p) === g);
  const loggedN = pl.rows.filter((p) => p.accounts_n > 0).length;

  let h = `<div class="srcbar">
    <div class="srcbar-i"><i>平台</i><b>${pl.total}</b></div>
    <div class="srcbar-i"><i>已接入</i><b>${loggedN}</b></div>
    <div class="srcbar-i"><i>账号凭据</i><b>${pl.rows.reduce((a, p) => a + (p.accounts_n || 0), 0)}</b></div>
    <button class="btn sm gh" style="margin-left:auto" onclick="SRC_HELP=!SRC_HELP;render()">怎么用 ${SRC_HELP ? '▴' : '▾'}</button>
  </div>`;
  if (SRC_HELP) {
    h += `<div class="note">接入三步：① 点卡片接入（官方/公益站填 Key；客户端/订阅跳转登录后粘贴凭据）② 客户端 base_url 填 <code>${esc(location.origin)}/v1</code>、api_key 填 <code>admin</code> ③ model 填 <code>模型id@平台</code> 或清单内模型。</div>`;
  }
  h += `<div class="srcbar2">
    <input class="q" placeholder="搜索平台名称 / id / 说明…" value="${esc(SRC_Q)}"
      oninput="SRC_Q=this.value;srcRerender()" onkeydown="if(event.key==='Escape'){SRC_Q='';this.value='';srcRerender()}">
    <span class="faint" style="font-size:11.5px">显示 ${rows.length} 个</span>
  </div>`;

  for (const g of groups) {
    const meta = GROUP_META[g];
    const items = byGroup[g] || [];
    const onN = items.filter((p) => p.accounts_n > 0).length;
    const open = SRC_OPEN[g] ?? meta.open;
    h += `<section class="card glass srcgroup">
      <div class="srcgroup-hd" onclick="SRC_OPEN['${g}']=!SRC_OPEN['${g}'];srcRerender()">
        <span class="sg-arrow">${open ? '▾' : '▸'}</span>
        <b>${esc(meta.t)}</b>
        <span class="faint">${esc(meta.desc)}</span>
        <span class="pill mut">${onN}/${items.length} 接入</span>
        ${g === 'custom' ? '<button class="btn sm pri" style="margin-left:auto" onclick="event.stopPropagation();openCustomApi()">＋ 新建自定义 API</button>' : ''}
        ${g === 'free' ? '<button class="btn sm" style="margin-left:auto" onclick="event.stopPropagation();refreshFree()">⟳ 刷新公益站</button>' : ''}
      </div>
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

// 平台卡片（精简：名称 + 状态 + 接入按钮，无长介绍）
function srcCard(p) {
  const authTxt = { api_key: '填 Key', cookie: 'Cookie', anon: '免密', token: 'Token' }[p.auth] || p.auth;
  // 自定义 API：已配置 = 有 key 掩码
  const configured = p.custom ? (p.key_mask ? true : false) : p.accounts_n > 0;
  return `<div class="srccard glass ${p.enabled ? '' : 'off'}" data-id="${esc(p.id)}">
    <div class="srcname">${esc(p.name)}<span class="faint mono" style="font-size:10.5px"> ${esc(p.id)}</span></div>
    <div class="srcmeta">
      ${p.custom ? pill('ok', '自定义') : p.id.startsWith('sub-') ? pill('mut', '订阅') : p.id.startsWith('free-') ? pill('mut', '公益') : pill('mut', authTxt)}
      ${configured ? pill('ok', `${p.custom ? '已配置' : p.accounts_n + ' 账号'}`) : pill('mut', '未接入')}
      ${p.models_policy === 'live' ? pill('mut', 'live 模型') : p.models_n ? pill('mut', `${p.models_n} 模型`) : ''}
    </div>
    <button class="btn sm ${configured ? '' : 'pri'}" onclick="event.stopPropagation();SRC_EXPAND='${esc(p.id)}';render()">
      ${configured ? (p.custom ? '编辑' : '管理 / 重接') : '接入'}
    </button>
  </div>`;
}

// 登录抽屉：跳转登录 + 手动粘贴凭据（不扫码）；wb/minimax/kimi/grok 走设备码，codex/claude 走 PKCE 回调
function srcDrawer(p) {
  const isWb = p.id === 'wb-gateway' || p.id === 'wb-gateway-intl' || p.id === 'codebuddy';
  const isMinimax = p.id === 'minimax';
  const isKimi = p.id === 'sub-kimi' || p.id === 'sub-kimi-ai' || p.id === 'sub-grok';
  const isPkce = p.id === 'sub-codex' || p.id === 'sub-claude';
  const isLobster = p.id === 'lobster';
  const isKey = p.auth === 'api_key';
  const isAnon = p.auth === 'anon';
  const loginUrl = p.login_url || '';
  let body = '';
  if (isAnon) {
    body = `<div class="note">免密平台：无需凭据，直接可用。点下面的「测试连通」看是否可转发。</div>
      <button class="btn pri" onclick="testAnon('${esc(p.id)}')">测试连通</button>`;
  } else if (isWb || isMinimax || isKimi) {
    body = `<div class="note">${isWb ? 'WorkBuddy/CodeBuddy 设备授权：' : isMinimax ? 'MiniMax 设备码：' : isKimi && p.id === 'sub-grok' ? 'Grok 设备码（RFC 8628，需代理访问）：' : 'Kimi Code 设备码（RFC 8628）：'}点「开始登录」生成授权链接，在浏览器完成授权后面板自动拿 token 入库。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${esc(p.id)}')">开始登录</button>
        <button class="btn" onclick="authPoll('${esc(p.id)}')">查询授权状态</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isPkce) {
    body = `<div class="note">${p.id === 'sub-codex' ? 'Codex 跳转登录（PKCE + 本地 1455 回调）' : 'Claude Code 跳转登录（PKCE + 本地 54545 回调）'}：点「打开授权页」在浏览器完成登录（跳转登录，非扫码）。面板自动捕获回调换 token；若部署在服务器，把浏览器跳转后地址栏的完整 URL 粘贴到下方输入框。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${esc(p.id)}')">打开授权页</button>
        <button class="btn" onclick="authPoll('${esc(p.id)}')">查询授权状态</button>
      </div>
      <div class="row">
        <input class="q mono" id="cbUrl" placeholder="粘贴回调地址（含 ?code= 与 &state=）…">
        <button class="btn" onclick="authCallback('${esc(p.id)}')">提交回调</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isLobster) {
    body = `<div class="note">Lobster 跳转登录（有道龙虾授权页 → 本地随机端口回调换码）：点「打开授权页」在浏览器完成登录（跳转登录，非扫码）。面板自动捕获回调换 token 入库；若部署在服务器，把浏览器跳转后地址栏的完整 URL 粘贴到下方输入框。</div>
      <div class="row">
        <button class="btn pri" onclick="authStart('${esc(p.id)}')">打开授权页</button>
        <button class="btn" onclick="authPoll('${esc(p.id)}')">查询授权状态</button>
      </div>
      <div class="row">
        <input class="q mono" id="cbUrl" placeholder="粘贴回调地址（含 ?code= 与 &state=）…">
        <button class="btn" onclick="authCallback('${esc(p.id)}')">提交回调</button>
      </div>
      <div id="authBox"></div>`;
  } else if (isKey) {
    body = `<div class="note">${p.id.startsWith('sub-') ? '订阅平台：' : '填 API Key：'}把 Key 粘贴到下面，保存即入库（自动作为 Bearer 头转发）。${loginUrl ? `<br>还没有 Key？<a href="${esc(loginUrl)}" target="_blank" rel="noopener">去官网申请/登录 →</a>` : ''}</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder="粘贴 API Key / token 或 Cookie…">
        <button class="btn pri" onclick="authSave('${esc(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  } else {
    // cookie 类：跳转登录 + 粘贴 Cookie
    body = `<div class="note">${loginUrl ? `① <a href="${esc(loginUrl)}" target="_blank" rel="noopener">打开登录页 →</a> 在自己的浏览器完成登录（面板不支持扫码）` : '该平台未配置登录页'}　② 登录后把 Cookie / 凭据粘贴到下面，点保存入库。</div>
      <div class="row">
        <input class="q mono" id="keyInput" placeholder="粘贴 Cookie / 凭据…">
        <button class="btn pri" onclick="authSave('${esc(p.id)}')">保存凭据</button>
      </div>
      <div id="authBox"></div>`;
  }
  return `<div class="modal-mask" onclick="if(event.target===this){SRC_EXPAND=null;render()}"><div class="modal drawer" id="srcDrawer">
    <div class="modal-hd">${esc(p.name)}<span class="faint mono"> ${esc(p.id)}</span>
      <b class="x" onclick="SRC_EXPAND=null;render()">✕</b></div>
    <div class="modal-bd">
      ${p.accounts_n > 0 ? `<div class="note oknote">已接入 ${p.accounts_n} 个账号（账号页可查看掩码）</div>` : ''}
      ${body}
    </div>
  </div></div>`;
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
  if (r.login_url) {
    window.open(r.login_url, '_blank', 'noopener');
  }
  box.innerHTML = `<div class="note">${esc(r.message || '已生成授权会话')}</div>
    ${r.user_code ? `<div class="usercode">输入设备码：<b>${esc(r.user_code)}</b></div>` : ''}
    <div class="row"><button class="btn" onclick="authPoll('${esc(platform)}')">授权完成后，点这里查询</button></div>
    <div id="pollBox"></div>`;
  SRC_SESSION = r.id;
}
let SRC_SESSION = null;
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
    b.innerHTML += `<div class="row"><button class="btn pri" onclick="authPoll('${esc(platform)}')">查询授权状态</button></div>`;
  }
}
async function authPoll(platform) {
  const id = SRC_SESSION;
  if (!id) { alert('请先点「开始登录」'); return; }
  const r = await post('/api/auth', { action: 'poll', id }).catch((e) => ({ ok: false, message: e.message }));
  const pb = $('#pollBox') || $('#authBox');
  if (!pb) return;
  if (r.ok && r.session && r.session.status === 'success') {
    pb.innerHTML = `<div class="note oknote">✓ ${esc(r.session.message || '登录成功，已入库')}</div>`;
    setTimeout(() => render(), 800);
  } else {
    pb.innerHTML = `<div class="note">${esc((r.session && r.session.message) || r.message || '还没完成，稍后再查')}</div>
      <div class="row"><button class="btn" onclick="authPoll('${esc(platform)}')">再查一次</button></div>`;
  }
}
async function authSave(platform) {
  const input = $('#keyInput');
  const secret = input ? input.value.trim() : '';
  if (!secret) { alert('请先粘贴凭据'); return; }
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
async function testAnon(platform) {
  const b = $('#authBox');
  if (b) b.innerHTML = '<div class="faint">测试中…</div>';
  try {
    const r = await fetch(`/v1/models`, { headers: {} });
    const j = await r.json();
    const owned = (j.data || []).filter((m) => m.owned_by === platform);
    if (b) b.innerHTML = `<div class="note oknote">连通正常：该平台 live 模型 ${owned.length} 个</div>`;
  } catch (e) {
    if (b) b.innerHTML = `<div class="note errnote">测试失败：${esc(e.message)}</div>`;
  }
}

// 自定义 API 弹窗（CherryStudio 式：协议可选 OpenAI/Anthropic，自动同步模型）
function openCustomApi(editId) {
  const mk = `<div class="modal-mask" onclick="if(event.target===this)this.remove()"><div class="modal">
    <div class="modal-hd">${editId ? '编辑' : '新建'}自定义 API<b class="x" onclick="this.closest('.modal-mask').remove()">✕</b></div>
    <div class="modal-bd">
      <label>名称</label>
      <input class="q" id="cName" placeholder="如：我的中转">
      <label>协议</label>
      <select class="q" id="cFormat">
        <option value="openai">OpenAI 兼容</option>
        <option value="anthropic">Anthropic 协议</option>
      </select>
      <label>URL（base_url）</label>
      <input class="q mono" id="cUrl" placeholder="https://api.example.com/v1">
      <label>API Key</label>
      <input class="q mono" id="cKey" placeholder="sk-…">
      <div class="modal-foot">
        <button class="btn pri" onclick="saveCustomApi('${editId || ''}')">保存并同步模型</button>
        ${editId ? `<button class="btn" onclick="removeCustomApi('${esc(editId)}')">删除</button>` : ''}
      </div>
    </div>
  </div></div>`;
  document.body.insertAdjacentHTML('beforeend', mk);
  // 编辑态回填
  if (editId) {
    api('/api/custom').then((j) => {
      const c = (j.rows || []).find((x) => x.id === editId);
      if (!c) return;
      $('#cName').value = c.name || '';
      $('#cFormat').value = c.format || 'openai';
      $('#cUrl').value = c.base_url || '';
    });
  }
}
async function saveCustomApi(editId) {
  const name = $('#cName').value.trim();
  const format = $('#cFormat').value;
  const base_url = $('#cUrl').value.trim();
  const api_key = $('#cKey').value.trim();
  if (!name || !base_url || !api_key) { alert('名称 / URL / Key 都要填'); return; }
  const action = editId ? 'update' : 'add';
  const body = { action, name, format, base_url, api_key };
  if (editId) body.id = editId;
  const r = await post('/api/custom', body).catch((e) => ({ ok: false, message: e.message }));
  if (!r.ok) { alert(r.message || '保存失败'); return; }
  // 保存后自动同步模型（live 拉取：向 /v1/models 或 /models 探测）
  await syncCustomModels(r.api ? r.api.id : editId, base_url, api_key, format);
  document.querySelector('.modal-mask')?.remove();
  render();
}
async function syncCustomModels(id, base_url, api_key, format) {
  // 尽力而为：GET {base}/models 拉模型；8s 超时兜底（不阻塞关闭弹窗）
  const modelUrl = format === 'anthropic' ? `${base_url}/models` : `${base_url}/models`;
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 8000);
  try {
    const r = await fetch(modelUrl, {
      headers: { Authorization: `Bearer ${api_key}` },
      signal: ctl.signal,
    });
    if (!r.ok) return;
    const j = await r.json();
    const ids = ((j.data || j.models || []).map((m) => m.id || m.name)).filter(Boolean).slice(0, 200);
    if (ids.length) {
      await post('/api/custom', { action: 'sync_models', id, models: ids });
    }
  } catch (_) { /* 探测失败/超时不阻塞：live 策略兜底 */ } finally { clearTimeout(timer); }
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
async function viewOverview(el) {
  const [h, pl, ac, us] = await Promise.all([
    api('/api/health'), api('/api/platforms'), api('/api/accounts'), api('/api/usage'),
  ]);
  const enabled = pl.rows.filter((p) => p.enabled).length;
  const rate = us.requests ? Math.round((us.success / us.requests) * 100) : 100;
  el.innerHTML = `
    <div class="grid g4">
      <div class="card glass kpi"><div class="lb">平台</div><div class="vl">${enabled}/${pl.total}</div>
        <div class="ex">启用 / 声明式 spec + 自定义</div>
        <div class="bar"><i style="width:${pl.total ? (enabled / pl.total) * 100 : 0}%"></i></div></div>
      <div class="card glass kpi"><div class="lb">账号</div><div class="vl">${ac.total}</div>
        <div class="ex">已导入（secret 后台专用）</div></div>
      <div class="card glass kpi"><div class="lb">累计调用</div><div class="vl">${nfmt(us.requests)}</div>
        <div class="ex">成功 ${nfmt(us.success)} · 失败 ${nfmt(us.failed)}</div></div>
      <div class="card glass kpi"><div class="lb">成功率</div><div class="vl">${rate}%</div>
        <div class="ex">tokens ${nfmt(us.tokensIn)} in / ${nfmt(us.tokensOut)} out</div></div>
    </div>
    <section class="card glass"><h3>运行状态</h3>
      <div class="mini-grid">
        <div class="mini glass"><b>${esc(h.service)} v${esc(h.version)}</b>
          <span class="pill ok">${esc(h.milestone)}</span></div>
        <div class="mini glass"><b>存储</b><span class="pill ${us.db ? 'ok' : 'warn'}">${us.db ? 'SQLite WAL' : 'JSON 降级'}</span></div>
        <div class="mini glass"><b>端点</b><code>/v1/chat/completions</code><br><span class="faint">模型路由：清单匹配 或 <code>模型id@平台</code></span></div>
      </div>
    </section>`;
}

async function viewAccounts(el) {
  const ac = await api('/api/accounts');
  el.innerHTML = `<section class="card glass">
    <table><thead><tr><th>名称</th><th>平台</th><th>类型</th><th>凭据</th><th>状态</th></tr></thead>
    <tbody>${ac.rows.map((a) => `<tr>
      <td><b>${esc(a.name || a.id)}</b><br><span class="faint mono">${esc(a.id)}</span></td>
      <td>${esc(a.platform)}</td><td>${pill('mut', esc(a.kind || '—'))}</td>
      <td class="faint">${a.secret_len ? `${a.secret_len} 字符（掩码）` : '空'}</td>
      <td>${a.enabled ? pill('ok', '启用') : pill('mut', '停用')}</td>
    </tr>`).join('')}</tbody></table>
  </section>`;
}

async function viewUsage(el) {
  const [us, hist] = await Promise.all([api('/api/usage'), api('/api/requests')]);
  const rate = us.requests ? Math.round((us.success / us.requests) * 100) : 100;
  const plats = Object.entries(hist.by_platform || {});
  const rows = (hist.recent || [])
    .map((e) => `<tr>
      <td class="mono">${new Date((e.ts || 0) * 1000).toLocaleTimeString('zh-CN')}</td>
      <td>${esc(e.platform)}</td><td class="mono">${esc(e.model)}</td>
      <td>${e.tin || 0} / ${e.tout || 0}</td><td>${e.ms || 0}ms</td>
      <td>${e.ok ? pill('ok', '成功') : pill('err', '失败')}</td></tr>`)
    .join('');
  el.innerHTML = `
    <div class="grid g4">
      <div class="card glass kpi"><div class="lb">请求数</div><div class="vl">${nfmt(us.requests)}</div></div>
      <div class="card glass kpi"><div class="lb">成功</div><div class="vl" style="background:none;-webkit-text-fill-color:currentColor;color:var(--ok)">${nfmt(us.success)}</div></div>
      <div class="card glass kpi"><div class="lb">失败</div><div class="vl" style="background:none;-webkit-text-fill-color:currentColor;color:var(--err)">${nfmt(us.failed)}</div></div>
      <div class="card glass kpi"><div class="lb">成功率</div><div class="vl">${rate}%</div></div>
    </div>
    <section class="card glass"><h3>平台归因</h3>
      ${plats.length ? `<table><thead><tr><th>平台</th><th>调用</th><th>成功/失败</th><th>tokens in/out</th></tr></thead>
      <tbody>${plats.map(([p, d]) => `<tr><td>${esc(p)}</td><td>${d.requests}</td>
        <td>${d.success} / <span class="muted">${d.failed}</span></td>
        <td class="mono">${nfmt(d.tokensIn)} / ${nfmt(d.tokensOut)}</td></tr>`).join('')}</tbody></table>`
      : '<div class="empty">暂无调用</div>'}
    </section>
    <section class="card glass"><h3>最近请求（60 条）</h3>
      ${rows ? `<table><thead><tr><th>时间</th><th>平台</th><th>模型</th><th>tokens</th><th>耗时</th><th>结果</th></tr></thead>
      <tbody>${rows}</tbody></table>` : '<div class="empty">暂无历史</div>'}
      <p class="faint" style="margin-bottom:0">存储：${us.db ? 'SQLite（WAL，90 天 / 5000 条 prune）' : 'JSON 降级'} · 旧版数据已幂等导入</p>
    </section>`;
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
