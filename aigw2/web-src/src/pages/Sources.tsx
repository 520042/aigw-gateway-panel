import { useEffect, useMemo, useRef, useState } from 'react';
import { Card, Kpi, Pill, Note, Loading } from '../components';
import { api, type Platform, type PlatformResponse } from '../api';

/** 接入能力表：与旧版 ACCESS 同口径，但作为纯数据放在框架层，界面从它派生。 */
const ACCESS: Record<string, { kind: 'one_click' | 'qr' | 'device' | 'oauth'; way: string }> = {
  'apk-orcaterm': { kind: 'one_click', way: 'OrcaTerm 桌面端' },
  'web-monkeycode': { kind: 'one_click', way: 'MonkeyCode 客户端' },
  'apk-doubao': { kind: 'qr', way: '抖音/豆包 App 扫码' },
  'apk-kuku': { kind: 'qr', way: '百度 App 扫码' },
  'apk-raccoon': { kind: 'qr', way: '商汤小浣熊扫码' },
  'apk-loomy': { kind: 'qr', way: '讯飞 Loomy 扫码' },
  'apk-metaso': { kind: 'qr', way: '秘塔扫码' },
  'web-glm': { kind: 'qr', way: '智谱清言微信扫码' },
  minimax: { kind: 'device', way: '设备码授权' },
  cline: { kind: 'device', way: '设备码授权' },
  'sub-meta': { kind: 'device', way: '设备码授权' },
  codearts: { kind: 'device', way: '华为云跳转 / AK-SK' },
  'wb-gateway': { kind: 'device', way: 'WorkBuddy 设备授权' },
  'wb-gateway-intl': { kind: 'device', way: 'WorkBuddy 设备授权' },
  'apk-qoder': { kind: 'device', way: 'Qoder 跳转回调' },
  'web-qoder': { kind: 'device', way: 'Qoder 跳转回调' },
  'apk-trae': { kind: 'device', way: 'Trae 跳转回调' },
  'web-trae': { kind: 'device', way: 'Trae 跳转回调' },
  lobster: { kind: 'device', way: 'Lobster OAuth' },
  'sub-grok': { kind: 'device', way: 'OAuth 跳转' },
  'sub-codex': { kind: 'oauth', way: 'Codex 跳转登录' },
  'sub-claude': { kind: 'oauth', way: 'Claude 跳转登录' },
  'sub-antigravity': { kind: 'oauth', way: 'Google 跳转登录' },
  'sub-devin': { kind: 'oauth', way: 'Devin 跳转登录' },
  'apk-accio': { kind: 'oauth', way: '阿里 Phoenix 跳转登录' },
  'apk-accio-cn': { kind: 'oauth', way: '阿里 Phoenix 跳转登录' },
};
const LABEL: Record<string, string> = {
  one_click: '一键导入', qr: '扫码', device: '设备码', oauth: '跳转登录',
};
type AccessKind = 'one_click' | 'qr' | 'device' | 'oauth' | 'manual' | 'none';
const accessOf = (p: Platform): { kind: AccessKind; way: string } => {
  if (ACCESS[p.id]) return ACCESS[p.id];
  if (p.auth === 'anon') return { kind: 'none', way: '免密，无需登录' };
  if (p.upstream_kind === 'openai' || !p.upstream_kind) return { kind: 'manual', way: '填官方 API Key' };
  return { kind: 'manual', way: '从客户端 / 网页取登录态后粘贴' };
};
const isAuto = (p: Platform) => ['one_click', 'qr', 'device', 'oauth'].includes(accessOf(p).kind);

type Filter = 'all' | 'auto' | 'qr' | 'bad';

/** 分组规则（与 legacy srcGroupOf 同口径，用户拍板 + spec 自报优先）：
 *  ① spec 自报 group 优先（providers/*.toml 的 group 字段，后端随行下发）；
 *  ② 没声明再按 id 前缀推导。只看前缀会归错组（文件名习惯≠契约），也不能按
 *     auth 判（api_key 同时被开放平台/客户端凭据源/60 多个公益站使用）。 */
const SRC_GROUPS = ['custom', 'official', 'sub', 'client', 'free'] as const;
type SrcGroup = (typeof SRC_GROUPS)[number];
const GROUP_OVERRIDE: Record<'official' | 'free', string[]> = {
  official: ['apk-coze', 'kimi-code', 'codex'],
  // 免密免 Key 车道一律进「公益站」：没有账号也没有凭据，归到「客户端接入」
  // 会让人以为需要先登录。anon-ovh / anon-zen 同族但 id 前缀不是 free-，点名。
  free: ['anon-zen', 'anon-ovh', 'kilo', 'pollinations'],
};
const GROUP_META: Record<SrcGroup, { t: string; desc: string; open: boolean }> = {
  custom:   { t: '自定义 API', desc: '自填 名称/协议/URL/Key', open: true },
  official: { t: '开放平台 API', desc: '官方 API Key 直连', open: false },
  sub:      { t: '订阅反代', desc: '订阅账号 OAuth 授权 → 网关反代', open: false },
  client:   { t: '客户端接入', desc: '网页版 / 客户端凭据（Cookie / Token）', open: true },
  free:     { t: '公益站 · 免费车道', desc: '公益中转聚合 + 免 Key 车道', open: true },
};
function srcGroupOf(p: Platform): SrcGroup {
  if ((p as { group?: string }).group && SRC_GROUPS.includes((p as { group?: string }).group as SrcGroup)) {
    return (p as { group?: string }).group as SrcGroup;
  }
  if (p.id.startsWith('custom-')) return 'custom';
  if (p.id.startsWith('api-') || GROUP_OVERRIDE.official.includes(p.id)) return 'official';
  if (p.id.startsWith('sub-')) return 'sub';
  if (p.id.startsWith('free-') || GROUP_OVERRIDE.free.includes(p.id)) return 'free';
  return 'client';
}

export function Sources({ platforms, refresh }: { platforms: PlatformResponse | null; refresh: () => void }) {
  // 平台清单由框架层拉取一次传入；保存凭据/导入后调 refresh() 让框架层重取。
  const [filter, setFilter] = useState<Filter>('all');
  const [q, setQ] = useState('');
  const [sel, setSel] = useState<string | null>(null);
  // 快速开始引导：首次访问展开，之后默认收起（老用户每次划过它是负担）。
  const [showHelp, setShowHelp] = useState(() => {
    try { return localStorage.getItem('aigw-src-help-seen') === null; }
    catch { return true; }
  });
  const toggleHelp = () => {
    setShowHelp((v) => {
      try { localStorage.setItem('aigw-src-help-seen', '1'); } catch { /* 忽略 */ }
      return !v;
    });
  };
  // 分组折叠：`collapsed` 只记录**用户手动改过**的组（含 localStorage 持久化），
  // 没动过的组跟 GROUP_META.open 走（custom/client/free 默认展开，official/sub 收起）。
  // 搜索/筛选时强制全展开——用户在找东西，折叠是障碍。
  const [userToggled, setUserToggled] = useState<Record<string, boolean>>(() => {
    try { return JSON.parse(localStorage.getItem('aigw-src-fold') ?? '{}') as Record<string, boolean>; }
    catch { return {}; }
  });
  const toggleGroup = (g: string) => {
    setUserToggled((old) => {
      const isOpenNow = groupIsOpen(g);
      const next = { ...old, [g]: !isOpenNow };
      try { localStorage.setItem('aigw-src-fold', JSON.stringify(next)); } catch { /* 忽略 */ }
      return next;
    });
  };
  const groupIsOpen = (g: string): boolean => {
    if (userToggled[g] !== undefined) return userToggled[g];
    return GROUP_META[g as SrcGroup]?.open ?? true;
  };

  const data = platforms;
  // 模型停用集合（裸模型名，后端 flags 记账口径）——详情栏模型芯片据此着色。
  const disabledModels = useMemo(
    () => new Set(data?.disabled_models ?? []),
    [data],
  );

  const rows = useMemo(() => {
    if (!data) return [];
    return data.rows.filter((p) => {
      if (filter === 'auto' && !isAuto(p)) return false;
      if (filter === 'qr' && !(p.login_methods ?? []).includes('qr')) return false;
      if (filter === 'bad' && !(p.recent && p.recent.failed > 0)) return false;
      if (!q) return true;
      const s = q.toLowerCase();
      return p.name.toLowerCase().includes(s) || p.id.toLowerCase().includes(s) || p.notes.toLowerCase().includes(s);
    });
  }, [data, filter, q]);

  const autoN = data?.rows.filter(isAuto).length ?? 0;
  const qrN = data?.rows.filter((p) => (p.login_methods ?? []).includes('qr')).length ?? 0;
  const badN = data?.rows.filter((p) => p.recent && p.recent.failed > 0).length ?? 0;
  const loggedN = data?.rows.filter((p) => p.anon || p.accounts_n > 0).length ?? 0;
  const current = rows.find((p) => p.id === sel) ?? rows[0] ?? null;

  if (!data) return <Loading />;

  const groups: Array<[AccessKind, string, string]> = [
    ['one_click', '一键导入', '本机客户端已登录，点一下自动读凭据'],
    ['qr', '扫码登录', '手机扫一下，不用开浏览器 F12'],
    ['device', '设备码 / 跳转授权', '运行一条命令或跳到官网授权'],
    ['oauth', '跳转登录', '浏览器完成 OAuth，自动回调换 token'],
  ];
  const autoByKind = (k: AccessKind) => data.rows.filter((p) => accessOf(p).kind === k);

  return (
    <>
      <div className="grid g4 mb-3">
        <Kpi label="平台总数" value={data.total} hint={`${loggedN} 个已接入`} />
        <Kpi label="账号凭据" value={data.rows.reduce((a, p) => a + p.accounts_n, 0)} hint="secret 仅后台使用" />
        <Kpi label="免手动接入" value={autoN} hint="一键 / 扫码 / 设备码 / 跳转登录" />
        <Kpi label="近 24h 有问题" value={badN} tone={badN ? 'err' : undefined} hint="台账里有失败记录" />
      </div>

      {showHelp && (
        <Card title="快速开始 · 选一个能自动接入的" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>共 {autoN} 个，不用自己找凭据</span>}>
          <div className="onboard-grid">
            {groups.filter(([k]) => autoByKind(k).length).map(([k, title, desc]) => (
              <div className="onboard-col" key={k}>
                <div className="onboard-h">
                  <span className={'ac-tag ac-' + k}>{LABEL[k]}</span>
                  <b>{title}</b><span className="faint fs-xs">{desc}</span>
                </div>
                <div className="onboard-list">
                  {autoByKind(k).map((p) => (
                    <button className="onboard-item" key={p.id} title={accessOf(p).way} onClick={() => setSel(p.id)}>
                      {p.name}
                      {p.accounts_n > 0 && <span className="faint fs-xs">已接入 {p.accounts_n}</span>}
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
          <Note>
            接入三步：① 上面点一个平台，在右侧完成登录 ② 客户端 <code>base_url</code> 填 <code>{location.origin}/v1</code>、<code>api_key</code> 填 <code>admin</code> ③ <code>model</code> 填 <code>模型id@平台</code> 或清单内模型。
          </Note>
        </Card>
      )}

      <div className="srcbar2">
        <button className="btn sm gh" onClick={toggleHelp}>怎么用 {showHelp ? '▴' : '▾'}</button>
        <span className="faint small ml-auto">管理操作已移到「模型与用量 → WB 任务」</span>
      </div>

      <div className="src-split">
        <div className="src-list-col">
          <div className="src-list-hd">
            <div className="t">全部平台</div>
            <div className="s">共 {data.total} 个 · 显示 {rows.length} 个</div>
          </div>
          <div className="src-list-filters">
            {([['all', '全部', ''], ['auto', '免手动', autoN], ['qr', '可扫码', qrN], ['bad', '有问题', badN]] as Array<[Filter, string, number | string]>).map(([k, label, n]) => (
              <button key={k} className={'chip' + (filter === k ? ' on' : '')} onClick={() => setFilter(k)}>
                {label}{n !== '' ? ' ' + n : ''}
              </button>
            ))}
          </div>
          <div className="src-list-search">
            <input className="q" placeholder="搜索名称 / id / 说明…" value={q} onChange={(e) => setQ(e.target.value)} />
          </div>
          <div className="src-list-vp">
            {rows.length === 0 && <div className="faint p-3">没有匹配的平台</div>}
            {/* 按分组渲染：分组头可点折叠；默认收起 official/sub（与 legacy 同）。
                搜索/筛选时强制全展开（用户在找东西，折叠是障碍）。 */}
            {SRC_GROUPS.map((g) => {
              const gRows = rows.filter((p) => srcGroupOf(p) === g);
              if (gRows.length === 0) return null;
              const meta = GROUP_META[g];
              const searching = q.trim() !== '' || filter !== 'all';
              const show = searching || groupIsOpen(g);
              return (
                <div key={g} className={'src-group' + (show ? ' open' : '')}>
                  <button
                    className="src-group-h"
                    onClick={() => toggleGroup(g)}
                    title={meta.desc}
                  >
                    <span className="chev">{show ? '▾' : '▸'}</span>
                    {meta.t}
                    <span className="faint fs-xs">{gRows.length}</span>
                    {gRows.some((p) => p.accounts_n > 0) && <span className="dot g-ok" />}
                  </button>
                  {show && gRows.map((p) => {
                    const a = accessOf(p);
                    const tag = ['manual', 'none'].includes(a.kind) ? null : a.kind;
                    return (
                      <button
                        key={p.id}
                        className={'src-row static' + (current?.id === p.id ? ' on' : '')}
                        title={`${p.name} · ${p.id} · ${a.way}`}
                        onClick={() => setSel(p.id)}
                      >
                        <span className={'dot ' + (p.anon ? 'on' : p.accounts_n > 0 ? 'ok' : 'off')} />
                        <span className="nm">{p.name}</span>
                        {tag && <span className={'ac-tag ac-' + tag} title={a.way}>{LABEL[tag]}</span>}
                        {p.accounts_n > 0 && <span className="bd">{p.accounts_n}</span>}
                      </button>
                    );
                  })}
                </div>
              );
            })}
          </div>
        </div>

        <div className="src-detail-col">
          {current ? <PlatformDetail key={current.id} p={current} disabledModels={disabledModels} onChanged={refresh} /> : <div className="empty">没有匹配的平台</div>}
        </div>
      </div>
    </>
  );
}

function PlatformDetail({ p, disabledModels, onChanged }: {
  p: Platform;
  disabledModels: Set<string>;
  onChanged: () => void;
}) {
  const a = accessOf(p);
  // 保存/导入的即时状态：验活要打上游（实测最长 15s+），alert 是模态的、
  // 会把整页冻住——期间必须让用户看到「正在验活」，否则以为卡死。
  const [credMsg, setCredMsg] = useState<{ tone: 'ok' | 'err' | 'busy'; text: string } | null>(null);
  // 模型开关的即时反馈；实际状态以 onChanged() 重取后的 disabledModels 为准。
  const [modelMsg, setModelMsg] = useState('');
  const toggleModel = async (model: string) => {
    const bare = model.includes('@') ? model.slice(0, model.lastIndexOf('@')) : model;
    setModelMsg(`正在切换 ${bare}…`);
    try {
      await api.post('/api/platforms', { action: 'toggle_model', model: bare });
      setModelMsg('');
      onChanged();
    } catch (e) {
      setModelMsg('切换失败：' + (e instanceof Error ? e.message : String(e)));
    }
  };
  const saveCred = (fromClient: boolean) => {
    const el = document.getElementById('cred-' + p.id) as HTMLInputElement | null;
    if (!fromClient) {
      const v = el?.value.trim();
      if (!v) { setCredMsg({ tone: 'err', text: '请先粘贴凭据' }); return; }
    }
    setCredMsg({ tone: 'busy', text: fromClient ? '正在从本机客户端读取凭据并验活…' : '正在保存并验活…' });
    const req = fromClient
      ? api.post<{ ok?: boolean; message?: string; source?: string; credential_replaced?: boolean; verified?: boolean }>(
          '/api/auth', { action: 'save', platform: p.id, from_client: true })
      : api.post<{ ok?: boolean; message?: string; source?: string; credential_replaced?: boolean; verified?: boolean }>(
          '/api/auth', { action: 'save', platform: p.id, secret: el?.value.trim() ?? '' });
    req.then((r) => {
      if (r.ok === false) {
        setCredMsg({ tone: 'err', text: '失败：' + (r.message ?? '未知原因') });
        return;
      }
      const parts: string[] = [];
      if (fromClient && r.source) parts.push(`来源：${r.source}`);
      parts.push(r.credential_replaced ? '已替换既有凭据' : '已作为新账号存入');
      if (r.verified === true) parts.push('验活通过 ✓');
      else if (r.verified === false) parts.push('⚠ 验活未通过，凭据可能无效（可用一次真实对话再确认）');
      setCredMsg({ tone: r.verified === false ? 'err' : 'ok', text: parts.join(' · ') });
      if (!fromClient && el) el.value = '';
      onChanged();
    }).catch((e: unknown) => {
      setCredMsg({ tone: 'err', text: '失败：' + (e instanceof Error ? e.message : String(e)) });
    });
  };
  return (
    <>
      <Card title={<span>{p.name}<span className="faint mono"> {p.id}</span></span>}>
        <div className="detail-meta">
          {!p.enabled && <Pill tone="mut">已停用</Pill>}
          <Pill tone={p.accounts_n > 0 ? 'ok' : 'mut'}>{a.way}</Pill>
          <span className="mono">{p.id}</span>
          <span>· {p.models_n} 个模型</span>
          <span>· {p.region ?? '—'}</span>
        </div>
        {/* 平台级启停：停用后 /v1 不再路由该平台全部模型（legacy 同款操作）。 */}
        <div className="row" style={{ marginTop: 'var(--sp-2)' }}>
          <button
            className={'btn sm ' + (p.enabled ? 'gh' : 'pri')}
            onClick={async () => {
              if (!confirm(`${p.enabled ? '停用' : '启用'}平台 ${p.name}？${p.enabled ? '停用后客户端将无法调用它的任何模型。' : ''}`)) return;
              try {
                await api.post('/api/platforms', { action: 'toggle_platform', id: p.id, enabled: !p.enabled });
                onChanged();
              } catch (e) {
                alert('操作失败：' + (e instanceof Error ? e.message : String(e)));
              }
            }}
          >
            {p.enabled ? '⏸ 停用平台' : '▶ 启用平台'}
          </button>
          <span className="faint fs-xs">
            {p.enabled ? '当前参与路由' : '已停用：客户端调它的模型会得到「平台已停用」'}
          </span>
        </div>
      </Card>

      <Card title="登录">
        {/* 只给一句话：怎么登录。平台 notes 是给排查用的技术档案，
            默认折叠（用户明确要求「不要多事」），想看再点开。 */}
        <div className="faint small">{a.way}</div>
        {p.notes && (
          <details className="src-notes">
            <summary>详细说明 / 排查资料</summary>
            <div className="note mt-2">{p.notes}
              {p.login_url && <>注册地址：<a href={p.login_url} target="_blank" rel="noopener noreferrer">{p.login_url}</a></>}
            </div>
          </details>
        )}
        {/* ★ 设备码 / 跳转授权 / 扫码登录入口：按后端返回形状分支渲染 + 自调度轮询。 */}
        {['qr', 'device', 'oauth'].includes(a.kind) && <AuthPanel p={p} onChanged={onChanged} />}
        <div className="row" style={{ marginTop: 'var(--sp-2)' }}>
          <input className="q mono" placeholder="粘贴凭据（手动方式）…" id={'cred-' + p.id} />
          <button className="btn pri" onClick={() => saveCred(false)}>保存凭据</button>
          {a.kind === 'one_click' && (
            <button className="btn" onClick={() => saveCred(true)}>从本机客户端导入</button>
          )}
        </div>
        {credMsg && <Note tone={credMsg.tone === 'ok' ? 'ok' : credMsg.tone === 'err' ? 'err' : undefined}>{credMsg.text}</Note>}
      </Card>

      {(p.accounts?.length ?? 0) > 0 && (
        <Card title={`账号 · ${p.accounts!.length}`}>
          <table>
            <thead>
              <tr><th>账号</th><th>状态</th><th>健康</th><th>积分</th></tr>
            </thead>
            <tbody>
              {(p.accounts ?? []).map((acc) => (
                <tr key={acc.id}>
                  <td><b>{acc.name || acc.id}</b><span className="faint mono fs-xs" style={{ marginLeft: 6 }}>{acc.id.slice(0, 8)}</span></td>
                  <td><Pill tone={acc.enabled ? 'ok' : 'mut'}>{acc.enabled ? '启用' : '停用'}</Pill>
                    {acc.needs_relogin && <span className="pill warn" style={{ marginLeft: 4 }}>本机解不开</span>}</td>
                  <td>{acc.health?.label
                    ? <span className={'pill ' + (acc.health.status === 'ok' ? 'ok' : 'warn')}>{acc.health.label}</span>
                    : <span className="faint">未验活</span>}
                    {acc.cooling_s != null && acc.cooling_s > 0 && <span className="faint fs-xs" style={{ marginLeft: 6 }}>冷却 {Math.round(acc.cooling_s)}s</span>}</td>
                  <td className="mono">{acc.credit != null ? acc.credit.toLocaleString('en-US') : <span className="faint">未查</span>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}

      <Card title={<span>模型 · 已启用 {p.models_on}/{p.models_n}</span>}>
        {p.models.length === 0
          ? <div className="faint">这个平台没有声明模型清单（live 拉取）</div>
          : <div className="model-chips">
              {p.models.map((m) => {
                const bare = m.includes('@') ? m.slice(0, m.lastIndexOf('@')) : m;
                const off = disabledModels.has(bare);
                return (
                  <button
                    key={m}
                    className={'model-chip mono' + (off ? ' off' : '')}
                    title={off ? `${bare} 已停用，点击启用` : `${bare} 启用中，点击停用`}
                    onClick={() => toggleModel(m)}
                  >
                    {bare}
                    {off && <span className="chip-x">已停</span>}
                  </button>
                );
              })}
            </div>}
        {modelMsg && <Note>{modelMsg}</Note>}
        {p.models.length > 0 && <div className="faint fs-xs mt-2">点击芯片切换该模型的启用/停用（按裸模型名记账，跨平台生效）。</div>}
      </Card>
    </>
  );
}

/* ═══════════════════════════════════════════════════════════════════════
   登录授权面板（2026-11-06 补回——React 重构时整条登录链路丢失）。
   与 legacy authStart/authPoll 同状态机，按平台类型三分支：
   · qr：显示二维码（服务端 qr_image / 前端文本兜底）+ 自调度轮询
   · 跳转授权：显式「打开授权页」链接（window.open 会被拦）+ 自动轮询
   · 设备码：user_code 单独成行 + 手动查询按钮
   过期自动重试 3 次（豆包/百度二维码只有 1–3 分钟）。
   ═══════════════════════════════════════════════════════════════════════ */
interface StartResp {
  ok?: boolean; status?: string; message?: string;
  login_url?: string; qr_image?: string; qr_text?: string;
  user_code?: string; callback_warning?: string; id?: string;
}
interface PollResp {
  ok?: boolean; message?: string;
  session?: { status?: string; message?: string };
}

const QR_HINTS: Record<string, string> = {
  'apk-doubao': '用豆包 / 抖音 App 扫码，在手机上确认登录',
  'apk-kuku': '用百度 App / 百度网盘 App 扫一扫，在手机上确认登录',
  'apk-loomy': '用微信扫一扫，在手机上确认登录',
  'apk-raccoon': '用微信扫一扫（进入「商汤小浣熊」小程序），在手机上确认登录',
  'apk-metaso': '用微信扫一扫，在手机上确认登录',
  'web-glm': '用微信扫一扫，在手机上确认登录',
};

function AuthPanel({ p, onChanged }: { p: Platform; onChanged: () => void }) {
  type Phase =
    | { kind: 'idle' }
    | { kind: 'busy' }
    | { kind: 'qr'; img?: string; text?: string; hint: string }
    | { kind: 'redirect'; url: string; code?: string; warn?: string }
    | { kind: 'manual'; code?: string }
    | { kind: 'done'; text: string }
    | { kind: 'fail'; text: string };
  const [phase, setPhase] = useState<Phase>({ kind: 'idle' });
  const [pollMsg, setPollMsg] = useState('');
  const sessionRef = useRef<{ id?: string; tries: number; polls: number }>({ tries: 0, polls: 0 });
  const timerRef = useRef<number | null>(null);
  const inflightRef = useRef(false);
  const aliveRef = useRef(true);

  // ★ 换平台时：停掉旧会话的轮询与计时器，并把面板**重置回 idle**——
  //   只 stop 不 reset 的话，豆包的二维码/轮询文案会留在 MiniMax 的面板里
  //   （实测：切平台后按钮区为空、note 还是旧平台的「等待扫码」）。
  useEffect(() => {
    aliveRef.current = true;
    return () => {
      aliveRef.current = false;
      if (timerRef.current) { clearTimeout(timerRef.current); timerRef.current = null; }
      inflightRef.current = false;
    };
  }, [p.id]);

  // p.id 变化的重置单独放在一个 effect：cleanup 先停旧，body 再清状态。
  // （合成一个 effect 时 body 会先于旧 cleanup 跑，顺序不对。）
  useEffect(() => {
    setPhase({ kind: 'idle' });
    setPollMsg('');
    sessionRef.current = { tries: 0, polls: 0 };
  }, [p.id]);

  const stop = () => {
    if (timerRef.current) { clearTimeout(timerRef.current); timerRef.current = null; }
    inflightRef.current = false;
  };

  const poll = async () => {
    const id = sessionRef.current.id;
    if (!id || inflightRef.current || !aliveRef.current) return;
    inflightRef.current = true;
    let r: PollResp;
    try {
      r = await api.post<PollResp>('/api/auth', { action: 'poll', id });
    } catch (e) {
      inflightRef.current = false;
      if (aliveRef.current) setPollMsg('查询失败：' + (e instanceof Error ? e.message : String(e)));
      return;
    }
    inflightRef.current = false;
    if (!aliveRef.current || sessionRef.current.id !== id) return; // 换了新会话：丢弃旧响应

    if (r.ok && r.session?.status === 'success') {
      stop();
      setPhase({ kind: 'done', text: '✓ ' + (r.session.message || '登录成功，已入库') });
      setPollMsg('');
      onChanged();
      return;
    }
    const msg = r.session?.message || r.message || '还没完成，稍后再查';
    const expired = msg.includes('过期') || msg.includes('失效') || msg.includes('已取消');
    const waiting = msg.includes('等待授权中');
    // 二维码过期是常态（豆包/百度 1–3 分钟）：自动换一张，最多 3 次。
    if (expired) {
      if (sessionRef.current.tries < 3) {
        sessionRef.current.tries += 1;
        setPollMsg(`二维码已过期，正在自动刷新（第 ${sessionRef.current.tries} 次）…`);
        timerRef.current = window.setTimeout(() => { if (aliveRef.current) void start(); }, 600);
        return;
      }
      stop();
      setPollMsg(msg + '——请点「开始登录」重新生成。');
      return;
    }
    setPollMsg(msg);
    if (waiting) {
      sessionRef.current.polls += 1;
      if (sessionRef.current.polls > 150) { // 3s × 150 ≈ 7.5 分钟
        stop();
        setPollMsg('等太久了（约 7 分钟还没拿到授权回调）。可能这次授权页没走完，或本机回调端口被占用——请把浏览器跳转后地址栏的完整 URL 粘贴到下方「粘贴凭据」框提交。');
        return;
      }
    }
    // 自调度：轮询回来再排下一次（库库长轮询 ~30s，setInterval 会叠请求——legacy 教训）
    timerRef.current = window.setTimeout(() => { if (aliveRef.current) void poll(); }, waiting ? 3000 : 1200);
  };

  const start = async () => {
    stop();
    sessionRef.current = { tries: 0, polls: 0 };
    setPhase({ kind: 'busy' });
    setPollMsg('');
    let r: StartResp;
    try {
      r = await api.post<StartResp>('/api/auth', { action: 'start', platform: p.id });
    } catch (e) {
      setPhase({ kind: 'fail', text: '启动失败：' + (e instanceof Error ? e.message : String(e)) });
      return;
    }
    if (!aliveRef.current) return;
    if (r.status === 'error' || r.ok === false) {
      setPhase({ kind: 'fail', text: r.message || '启动失败' });
      return;
    }
    sessionRef.current.id = r.id;
    if (r.qr_image || r.qr_text) {
      setPhase({ kind: 'qr', img: r.qr_image, text: r.qr_text, hint: QR_HINTS[p.id] ?? '扫码并在手机上确认登录' });
      timerRef.current = window.setTimeout(() => { if (aliveRef.current) void poll(); }, 1200);
      return;
    }
    if (r.login_url) {
      // 必须显式给可点链接：window.open 弹窗拦截很常见，只靠它用户会失去所有入口。
      window.open(r.login_url, '_blank', 'noopener');
      setPhase({ kind: 'redirect', url: r.login_url, code: r.user_code || undefined, warn: r.callback_warning || undefined });
      timerRef.current = window.setTimeout(() => { if (aliveRef.current) void poll(); }, 1500);
      return;
    }
    setPhase({ kind: 'manual', code: r.user_code || undefined });
  };

  return (
    <div className="auth-panel">
      {phase.kind === 'idle' && (
        <button className="btn pri" onClick={start}>
          {p.id.startsWith('apk-doubao') ? '生成扫码二维码' : '开始登录'}
        </button>
      )}
      {phase.kind === 'busy' && <span className="faint">正在生成授权会话…</span>}
      {phase.kind === 'qr' && (
        <div>
          <div className="qrwrap">
            {phase.img
              ? <img className="qr" src={phase.img} alt="二维码" style={{ width: 200, height: 200 }} />
              : <div className="note">此平台返回的是二维码文本（需手动机具）：{phase.text}</div>}
          </div>
          <div className="faint fs-xs" style={{ textAlign: 'center' }}>{phase.hint}（二维码有有效期，失效后自动重试）</div>
        </div>
      )}
      {phase.kind === 'redirect' && (
        <div>
          <div className="note">已打开授权页，等待你在浏览器里完成授权…</div>
          {phase.code && <div className="usercode">设备码：<b>{phase.code}</b>（在授权页里输入）</div>}
          <div className="row">
            <a className="btn pri" href={phase.url} target="_blank" rel="noopener noreferrer">打开授权页 →</a>
          </div>
          {phase.warn && <Note tone="warn">{phase.warn}</Note>}
        </div>
      )}
      {phase.kind === 'manual' && (
        <div>
          <div className="note">已生成授权会话</div>
          {phase.code && <div className="usercode">输入设备码：<b>{phase.code}</b></div>}
          <button className="btn" onClick={() => void poll()}>授权完成后，点这里查询</button>
        </div>
      )}
      {phase.kind === 'done' && <Note tone="ok">{phase.text}</Note>}
      {phase.kind === 'fail' && <Note tone="err">{phase.text}</Note>}
      {pollMsg && (
        <div className="note mt-2">{pollMsg}
          {phase.kind !== 'done' && phase.kind !== 'fail' && <span className="faint fs-xs">（自动轮询中…）</span>}
        </div>
      )}
    </div>
  );
}
