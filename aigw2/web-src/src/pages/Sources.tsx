import { useEffect, useMemo, useRef, useState } from 'react';
import { Card, Pill, Note, Loading, ErrorBox, Empty, useAsync } from '../components';
import { api, type Account, type Platform, type PlatformResponse } from '../api';

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
  manual: '粘贴凭据', none: '免密',
};
type AccessKind = 'one_click' | 'qr' | 'device' | 'oauth' | 'manual' | 'none';
const accessOf = (p: Platform): { kind: AccessKind; way: string } => {
  if (ACCESS[p.id]) return ACCESS[p.id];
  if (p.auth === 'anon') return { kind: 'none', way: '免密，无需登录' };
  if (p.upstream_kind === 'openai' || !p.upstream_kind) return { kind: 'manual', way: '填官方 API Key' };
  return { kind: 'manual', way: '从客户端 / 网页取登录态后粘贴' };
};
// ★ isAuto / Filter 已随三屏重构退役：「免手动」语义被选择器的 KIND_CHIPS 取代。

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

/** 接入源 · 三屏结构（2026-11-14 重构，方案见仓库根「接入源重构方案-2026-11-13.md」）：
 *  home(我的接入) → picker(选择器) → wizard(向导) / manage(管理详情)。
 *  旧版把 205 个平台当主角——94% 的面积在展示「你还没接的」；新方案以
 *  「我的源」为主角（出问题的永远排最上），205 退到选择器，接入动作
 *  收敛成 3 步向导（选平台 → 接入 → 完成），粘贴凭据路径边粘边识别。 */
type Screen = 'home' | 'picker' | 'wizard' | 'manage';

/** 接入方式 chips（选择器第一行：用户关心「要花多少力气」，不是业务分类）。 */
const KIND_CHIPS: Array<[AccessKind | 'all', string]> = [
  ['all', '全部'], ['none', '免登录'], ['one_click', '一键导入'], ['qr', '扫码'],
  ['device', '设备码'], ['oauth', '跳转授权'], ['manual', '粘贴凭据'],
];

/** 我的接入的行状态：红=要处理（过期/失败）· 黄=未验活 · 绿=可用 · 灰=已停用。
 *  rank 同时用于排序（小的排上面）。 */
function srcStatus(p: Platform, accs: Account[]): { tone: 'ok' | 'warn' | 'err' | 'mut'; label: string; rank: number } {
  if (!p.enabled) return { tone: 'mut', label: '已停用', rank: 3 };
  if (accs.some((a) => a.needs_relogin)) return { tone: 'err', label: '凭据过期', rank: 0 };
  if ((p.recent?.failed ?? 0) > 0) return { tone: 'err', label: `${p.recent!.failed} 次失败`, rank: 0 };
  if (p.anon) return { tone: 'ok', label: '免登录', rank: 2 };
  if (accs.length === 0) return { tone: 'warn', label: '未验活', rank: 1 };
  if (!accs.some((a) => a.health?.status === 'ok')) return { tone: 'warn', label: '未验活', rank: 1 };
  return { tone: 'ok', label: '可用', rank: 2 };
}

export function Sources({ platforms, error, refresh }: {
  platforms: PlatformResponse | null; error?: string; refresh: () => void;
}) {
  const [screen, setScreen] = useState<Screen>('home');
  const [sel, setSel] = useState<string | null>(null);
  // 账号健康喂「我的接入」的状态点（needs_relogin / 验活结论）。
  const ac = useAsync<{ rows: Account[] }>(() => api.get('/api/accounts'), []);

  const data = platforms;
  const disabledModels = useMemo(
    () => new Set(data?.disabled_models ?? []),
    [data],
  );

  // 已接入 = 有账号 或 免密车道；按 srcStatus.rank 排（需处理最上）。
  const connected = useMemo(() => {
    if (!data) return [];
    const accs = ac.data?.rows ?? [];
    const byId = new Map<string, Account[]>();
    for (const a of accs) {
      const list = byId.get(a.platform) ?? [];
      list.push(a);
      byId.set(a.platform, list);
    }
    return data.rows
      .filter((p) => p.anon || p.accounts_n > 0)
      .sort((a, b) =>
        srcStatus(a, byId.get(a.id) ?? []).rank - srcStatus(b, byId.get(b.id) ?? []).rank
        || a.name.localeCompare(b.name));
  }, [data, ac.data]);

  const current = useMemo(
    () => data?.rows.find((p) => p.id === sel) ?? null,
    [data, sel],
  );
  // ★ hooks 只能在顶层：connectedIds 在这里算好，picker 分支只消费。
  const connectedIds = useMemo(() => new Set(connected.map((p) => p.id)), [connected]);

  if (!data) return error ? <ErrorBox message={error} onRetry={refresh} /> : <Loading />;

  if (screen === 'picker') {
    return (
      <PickerScreen
        data={data}
        connectedIds={connectedIds}
        onBack={() => setScreen('home')}
        onAdd={(id) => { setSel(id); setScreen('wizard'); }}
        onManage={(id) => { setSel(id); setScreen('manage'); }}
      />
    );
  }
  if (screen === 'wizard' && current) {
    return (
      <WizardScreen
        key={current.id}
        p={current}
        onBack={() => setScreen('picker')}
        onFinished={() => { refresh(); ac.reload(); setScreen('home'); }}
        onChanged={refresh}
      />
    );
  }
  if (screen === 'manage' && current) {
    return (
      <ManageScreen
        p={current}
        disabledModels={disabledModels}
        onChanged={() => { refresh(); ac.reload(); }}
        onBack={() => setScreen('home')}
      />
    );
  }

  return (
    <HomeScreen
      data={data}
      connected={connected}
      accounts={ac.data?.rows ?? []}
      accountsLoading={ac.loading && !ac.data}
      onManage={(id) => { setSel(id); setScreen('manage'); }}
      onAdd={() => setScreen('picker')}
    />
  );
}

/** 屏 1 · 我的接入：出问题的永远在最上，一行看清「几个账号·怎么接·几个模型」。 */
function HomeScreen({ data, connected, accounts, accountsLoading, onManage, onAdd }: {
  data: PlatformResponse;
  connected: Platform[];
  accounts: Account[];
  accountsLoading: boolean;
  onManage: (id: string) => void;
  onAdd: () => void;
}) {
  const byId = useMemo(() => {
    const m = new Map<string, Account[]>();
    for (const a of accounts) {
      const list = m.get(a.platform) ?? [];
      list.push(a);
      m.set(a.platform, list);
    }
    return m;
  }, [accounts]);

  return (
    <>
      <div className="row mb-3" style={{ alignItems: 'center' }}>
        <div>
          <b>我的接入</b>
          <span className="faint fs-xs ml-2">{connected.length} 个源 · {data.total} 个可选</span>
        </div>
        <button className="btn pri ml-auto" onClick={onAdd}>＋ 接入新源</button>
      </div>
      <Card>
        {accountsLoading && connected.length === 0 ? <Loading />
          : connected.length === 0 ? (
            <Empty>还没有接入任何源。点右上角「＋ 接入新源」，按引导 3 步接入第一个。</Empty>
          ) : (
            <div className="src-home-rows">
              {connected.map((p) => {
                const st = srcStatus(p, byId.get(p.id) ?? []);
                const a = accessOf(p);
                return (
                  <div className={'src-home-row' + (st.tone === 'err' ? ' bad' : '')} key={p.id}>
                    <span className={'sdot ' + st.tone} />
                    <div className="src-home-main">
                      <b>{p.name}</b>
                      <span className="faint fs-xs">{p.id} · {a.way} · {p.accounts_n} 个账号 · {p.models_n} 个模型</span>
                    </div>
                    <span className={'pill ' + st.tone}>{st.label}</span>
                    <button className="btn sm gh" onClick={() => onManage(p.id)}>管理</button>
                  </div>
                );
              })}
            </div>
          )}
        <div className="faint fs-xs mt-2">按「需处理 → 可用」排序 · 灰色的行已停用，不参与路由</div>
      </Card>
    </>
  );
}

/** 屏 2 · 选择器：按「接入方式」chips（要花多少力气）+ 业务分组 chips（肌肉记忆）双行，205 个全在这。 */
function PickerScreen({ data, connectedIds, onBack, onAdd, onManage }: {
  data: PlatformResponse;
  connectedIds: Set<string>;
  onBack: () => void;
  onAdd: (id: string) => void;
  onManage: (id: string) => void;
}) {
  const [kind, setKind] = useState<AccessKind | 'all'>('all');
  const [group, setGroup] = useState<SrcGroup | 'all'>('all');
  const [q, setQ] = useState('');
  // 分组折叠沿用旧口径（localStorage 持久化；搜索时强制全展开）。
  const [userToggled, setUserToggled] = useState<Record<string, boolean>>(() => {
    try { return JSON.parse(localStorage.getItem('aigw-src-fold') ?? '{}') as Record<string, boolean>; }
    catch { return {}; }
  });
  const groupIsOpen = (g: string): boolean => {
    if (userToggled[g] !== undefined) return userToggled[g];
    return GROUP_META[g as SrcGroup]?.open ?? true;
  };
  const toggleGroup = (g: string) => {
    setUserToggled((old) => {
      const next = { ...old, [g]: !groupIsOpen(g) };
      try { localStorage.setItem('aigw-src-fold', JSON.stringify(next)); } catch { /* 忽略 */ }
      return next;
    });
  };

  const rows = useMemo(() => {
    return data.rows.filter((p) => {
      if (kind !== 'all' && accessOf(p).kind !== kind) return false;
      if (group !== 'all' && srcGroupOf(p) !== group) return false;
      if (!q) return true;
      const s = q.toLowerCase();
      return p.name.toLowerCase().includes(s) || p.id.toLowerCase().includes(s) || p.notes.toLowerCase().includes(s);
    });
  }, [data, kind, group, q]);

  const kindCount = (k: AccessKind | 'all') =>
    k === 'all' ? data.total : data.rows.filter((p) => accessOf(p).kind === k).length;

  return (
    <>
      <div className="row mb-3" style={{ alignItems: 'center' }}>
        <div>
          <b>接入新源</b>
          <span className="faint fs-xs ml-2">{data.total} 个可选 · 按接入方式分（你要花多少力气）</span>
        </div>
        <button className="btn gh ml-auto" onClick={onBack}>← 返回我的接入</button>
      </div>
      <div className="src-list-filters mb-2">
        {KIND_CHIPS.map(([k, label]) => (
          <button key={k} className={'chip' + (kind === k ? ' on' : '')} onClick={() => setKind(k)}>
            {label} {kindCount(k)}
          </button>
        ))}
      </div>
      <div className="src-list-filters mb-2">
        <button className={'chip' + (group === 'all' ? ' on' : '')} onClick={() => setGroup('all')}>业务分组 · 全部</button>
        {SRC_GROUPS.map((g) => (
          <button key={g} className={'chip' + (group === g ? ' on' : '')} onClick={() => setGroup(g)}>
            {GROUP_META[g].t} {data.rows.filter((p) => srcGroupOf(p) === g).length}
          </button>
        ))}
      </div>
      <div className="src-list-search mb-2">
        <input className="q" placeholder="搜索名称 / id / 说明…" value={q} onChange={(e) => setQ(e.target.value)} />
      </div>
      <Card>
        <div className="src-list-vp" style={{ maxHeight: '62vh' }}>
          {rows.length === 0 && <div className="faint p-3">没有匹配的平台</div>}
          {SRC_GROUPS.map((g) => {
            const gRows = rows.filter((p) => srcGroupOf(p) === g);
            if (gRows.length === 0) return null;
            const searching = q.trim() !== '' || kind !== 'all' || group !== 'all';
            const show = searching || groupIsOpen(g);
            return (
              <div key={g} className={'src-group' + (show ? ' open' : '')}>
                <button className="src-group-h" onClick={() => toggleGroup(g)} title={GROUP_META[g].desc}>
                  <span className="chev">{show ? '▾' : '▸'}</span>
                  {GROUP_META[g].t}
                  <span className="faint fs-xs">{gRows.length}</span>
                </button>
                {show && gRows.map((p) => {
                  const a = accessOf(p);
                  const tag = ['manual', 'none'].includes(a.kind) ? null : a.kind;
                  const isConnected = connectedIds.has(p.id);
                  return (
                    <div className="src-row static" key={p.id} title={`${p.name} · ${p.id} · ${a.way}`}>
                      <span className={'dot ' + (p.anon ? 'on' : isConnected ? 'ok' : 'off')} />
                      <span className="nm">{p.name}</span>
                      {tag && <span className={'ac-tag ac-' + tag} title={a.way}>{LABEL[tag]}</span>}
                      <span className="faint fs-xs">{p.models_n} 模型</span>
                      {isConnected
                        ? <button className="btn sm gh" onClick={() => onManage(p.id)}>管理</button>
                        : <button className="btn sm pri" onClick={() => onAdd(p.id)}>接入</button>}
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
      </Card>
    </>
  );
}

/** 屏 3 · 向导：选平台（已选）→ 接入（按方式自动分支）→ 完成（模型 + 可复制 curl）。 */
function WizardScreen({ p, onBack, onFinished, onChanged }: {
  p: Platform;
  onBack: () => void;
  onFinished: () => void;
  onChanged: () => void;
}) {
  const kind = accessOf(p).kind;
  const [step, setStep] = useState<'access' | 'done'>(kind === 'none' ? 'done' : 'access');
  const [text, setText] = useState('');
  const [preview, setPreview] = useState('');
  const [msg, setMsg] = useState<{ tone?: 'ok' | 'err'; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  // ★ 粘贴凭据边粘边识别（方案屏 3 的「识别到：Cookie · 4 个键值对」）：
  //   /api/auth preview 只分类不落盘；350ms 防抖，清空即清提示。
  useEffect(() => {
    const t = text.trim();
    if (!t) { setPreview(''); return; }
    const h = window.setTimeout(async () => {
      try {
        const r = await api.post('/api/auth', { action: 'preview', text: t }) as { detail?: string };
        setPreview(r.detail ?? '');
      } catch { setPreview(''); }
    }, 350);
    return () => window.clearTimeout(h);
  }, [text]);

  const save = async (fromClient: boolean) => {
    if (!fromClient && !text.trim()) { setMsg({ tone: 'err', text: '请先粘贴凭据' }); return; }
    setBusy(true);
    setMsg({ text: fromClient ? '正在从本机客户端读取凭据并验活…' : '正在保存并验活…' });
    try {
      const r = await api.post('/api/auth', fromClient
        ? { action: 'save', platform: p.id, from_client: true }
        : { action: 'save', platform: p.id, secret: text.trim() }) as {
        ok?: boolean; message?: string; verified?: boolean;
      };
      onChanged();
      if (r?.ok) {
        setMsg({ tone: 'ok', text: (r.message || '已保存') + (r.verified ? '（验活通过）' : '') });
        setStep('done');
      } else {
        setMsg({ tone: 'err', text: r?.message || '保存失败' });
      }
    } catch (e) {
      setMsg({ tone: 'err', text: '失败：' + (e instanceof Error ? e.message : String(e)) });
    } finally { setBusy(false); }
  };

  const firstModel = p.models[0] ?? '';
  const curl = `curl ${location.origin}/v1/chat/completions \\\n  -H "Authorization: Bearer <你的访问密钥>" \\\n  -H "Content-Type: application/json" \\\n  -d '{"model":"${firstModel}@${p.id}","messages":[{"role":"user","content":"hi"}]}'`;

  return (
    <>
      <div className="row mb-3" style={{ alignItems: 'center' }}>
        <div>
          <b>接入 {p.name}</b>
          <span className="faint fs-xs ml-2">{accessOf(p).way}</span>
        </div>
        <button className="btn gh ml-auto" onClick={onBack}>← 返回选择器</button>
      </div>
      <div className="wiz-steps mb-3">
        <span className="wiz-step done">✓ 选平台</span>
        <span className="wiz-line" />
        <span className={'wiz-step' + (step === 'access' ? ' now' : ' done')}>{step === 'access' ? '②' : '✓'} 接入</span>
        <span className="wiz-line" />
        <span className={'wiz-step' + (step === 'done' ? ' now' : '')}>③ 完成</span>
      </div>
      <div className="src-split">
        <div>
          {step === 'access' && (
            <Card title={`接入 · ${LABEL[kind] ?? '引导'}`}>
              {kind === 'none' && (
                <Note tone="ok">免密车道无需任何凭据，模型已可直接调用。右下「完成」看接入结果。</Note>
              )}
              {kind === 'one_click' && (
                <>
                  <Note>本机客户端已登录过 {p.name}。点下面按钮，网关自动从客户端数据文件读取凭据并验活。</Note>
                  <button className="btn pri mt-2" disabled={busy} onClick={() => save(true)}>从本机客户端导入</button>
                </>
              )}
              {kind === 'manual' && (
                <>
                  <Note>到平台网页复制登录态（Cookie / Token），粘贴到下面。保存即自动验活，失败给原因。</Note>
                  <textarea
                    className="q mt-2 mono"
                    style={{ width: '100%', minHeight: 110 }}
                    placeholder={'粘贴凭据…\n支持：Cookie（a=b;c=d）· Token/JWT · JSON'}
                    value={text}
                    onChange={(e) => setText(e.target.value)}
                  />
                  {preview && <div className="faint fs-xs mt-2">识别到：{preview}</div>}
                  <button className="btn pri mt-2" disabled={busy || !text.trim()} onClick={() => save(false)}>保存并验活</button>
                </>
              )}
              {['qr', 'device', 'oauth'].includes(kind) && (
                <AuthPanel p={p} onChanged={onChanged} onDone={() => { onChanged(); setStep('done'); }} />
              )}
              {msg && <Note tone={msg.tone}>{msg.text}</Note>}
            </Card>
          )}
          {step === 'done' && (
            <Card title="✓ 已接入">
              <Note tone="ok">
                {p.name} 已入库，模型已参与路由。下面命令可直接验证（把密钥换成你的）：
              </Note>
              <pre className="note mono mt-2" style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{curl}</pre>
              <div className="row mt-2">
                <button className="btn sm" onClick={() => { void navigator.clipboard?.writeText(curl); }}>复制命令</button>
                <button className="btn pri" onClick={onFinished}>完成，回到我的接入</button>
              </div>
            </Card>
          )}
        </div>
        <div>
          <Card title="这个源能提供">
            <div className="kv">
              <div><span>模型</span><b>{p.models_n} 个</b></div>
              <div><span>接入方式</span><b>{LABEL[kind] ?? '—'}</b></div>
              <div><span>区域</span><b>{p.region ?? '—'}</b></div>
              <div><span>状态</span><b>{p.enabled ? '已启用' : '已停用'}</b></div>
            </div>
          </Card>
          <Card title="客户端配置" className="mt-3">
            <div className="kv">
              <div><span>base_url</span><code className="mono">{location.origin}/v1</code></div>
              <div><span>api_key</span><code className="mono">「运行配置 → 访问密钥」创建</code></div>
              <div><span>model</span><code className="mono">{firstModel ? `${firstModel}@${p.id}` : '见模型清单'}</code></div>
            </div>
          </Card>
        </div>
      </div>
    </>
  );
}

/** 管理屏：复用既有 PlatformDetail（登录/账号/模型/启停全套），只加返回。 */
function ManageScreen({ p, disabledModels, onChanged, onBack }: {
  p: Platform;
  disabledModels: Set<string>;
  onChanged: () => void;
  onBack: () => void;
}) {
  return (
    <>
      <div className="row mb-3">
        <button className="btn gh" onClick={onBack}>← 返回我的接入</button>
      </div>
      <PlatformDetail key={p.id} p={p} disabledModels={disabledModels} onChanged={onChanged} />
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
  // ★ 模型多起来（api-* 平台动辄上百条）平铺成一堵墙，加搜索过滤。
  const [modelQ, setModelQ] = useState('');
  // 停用平台的二次确认（替代 window.confirm：原生弹窗会冻住整页，
  // 且在浏览器里不可样式化，与整体视觉割裂）。
  const [confirmOff, setConfirmOff] = useState(false);
  const [platMsg, setPlatMsg] = useState('');
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
      <Card title={<span>{p.name}</span>}>
        <div className="detail-meta">
          {!p.enabled && <Pill tone="mut">已停用</Pill>}
          <Pill tone={p.accounts_n > 0 ? 'ok' : 'mut'}>{a.way}</Pill>
          <span className="mono" title="平台 id（路由与排查用）">{p.id}</span>
          <span>· {p.models_n} 个模型</span>
          <span>· {p.region ?? '—'}</span>
        </div>
        {/* 平台级启停：停用后 /v1 不再路由该平台全部模型（legacy 同款操作）。 */}
        <div className="row" style={{ marginTop: 'var(--sp-2)' }}>
          <button
            className={'btn sm ' + (p.enabled ? 'gh' : 'pri') + (confirmOff ? ' danger' : '')}
            onClick={async () => {
              if (!p.enabled) {
                setPlatMsg('');
                try {
                  await api.post('/api/platforms', { action: 'toggle_platform', id: p.id, enabled: true });
                  setPlatMsg('✔ 已启用，重新参与路由');
                  onChanged();
                } catch (e) { setPlatMsg('操作失败：' + (e instanceof Error ? e.message : String(e))); }
                return;
              }
              // 停用是破坏性的（客户端立刻调不通）：第一次点击只翻成确认态，
              // 4 秒内再点才执行，避免误触。
              if (!confirmOff) {
                setConfirmOff(true);
                window.setTimeout(() => setConfirmOff(false), 4000);
                return;
              }
              setConfirmOff(false);
              try {
                await api.post('/api/platforms', { action: 'toggle_platform', id: p.id, enabled: false });
                setPlatMsg('已停用：客户端调它的模型会得到「平台已停用」');
                onChanged();
              } catch (e) { setPlatMsg('操作失败：' + (e instanceof Error ? e.message : String(e))); }
            }}
          >
            {p.enabled ? (confirmOff ? '再点确认停用' : '⏸ 停用平台') : '▶ 启用平台'}
          </button>
          <span className="faint fs-xs">
            {p.enabled ? '当前参与路由' : '已停用：客户端调它的模型会得到「平台已停用」'}
          </span>
        </div>
        {platMsg && <Note tone={platMsg.startsWith('✔') || platMsg.startsWith('已停用') ? 'ok' : 'err'}>{platMsg}</Note>}
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
        {/* ★ 免密平台（anon）原先也摆着「粘贴凭据 + 保存凭据」，
            与上面那句「免费，无需登录」自相矛盾。改成可选项折叠起来：
            少数免密站允许自带 Key 提额度，想填的人仍然找得到。 */}
        {a.kind === 'none' ? (
          <details className="src-notes mt-2">
            <summary>可选：填自己的 API Key（提高额度 / 少撞共享限流）</summary>
            <div className="row mt-2">
              <input className="q mono" placeholder="粘贴 API Key（不填也能用）…" id={'cred-' + p.id} />
              <button className="btn" onClick={() => saveCred(false)}>保存凭据</button>
            </div>
          </details>
        ) : (
          <div className="row" style={{ marginTop: 'var(--sp-2)' }}>
            <input className="q mono" placeholder="粘贴凭据（手动方式）…" id={'cred-' + p.id} />
            <button className="btn pri" onClick={() => saveCred(false)}>保存凭据</button>
            {a.kind === 'one_click' && (
              <button className="btn" onClick={() => saveCred(true)}>从本机客户端导入</button>
            )}
          </div>
        )}
        {credMsg && <Note tone={credMsg.tone === 'ok' ? 'ok' : credMsg.tone === 'err' ? 'err' : undefined}>{credMsg.text}</Note>}
      </Card>

      {(p.accounts?.length ?? 0) > 0 && (
        <Card title={`账号 · ${p.accounts!.length}`}>
          {/* 窄屏（1024）下四列会顶到卡片边，包一层横向滚动 */}
          <div className="table-scroll">
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
          </div>
        </Card>
      )}

      <Card title={<span>模型 · 已启用 {p.models_on}/{p.models_n}</span>}>
        {p.models.length === 0
          ? <div className="faint">这个平台没有声明模型清单（live 拉取）</div>
          : (() => {
              const ql = modelQ.trim().toLowerCase();
              const shown = ql ? p.models.filter((m) => m.toLowerCase().includes(ql)) : p.models;
              const offN = p.models.filter((m) => disabledModels.has(m.includes('@') ? m.slice(0, m.lastIndexOf('@')) : m)).length;
              return (
                <>
                  {p.models.length > 12 && (
                    <div className="row mb-2">
                      <input className="q" placeholder={`在 ${p.models.length} 个模型里搜…`}
                        value={modelQ} onChange={(e) => setModelQ(e.target.value)} />
                      <span className="faint fs-xs ml-auto" style={{ whiteSpace: 'nowrap' }}>
                        {ql ? `匹配 ${shown.length}` : `共 ${p.models.length}`}{offN > 0 ? ` · 已停 ${offN}` : ''}
                      </span>
                    </div>
                  )}
                  <div className="model-chips">
                    {shown.map((m) => {
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
                    {shown.length === 0 && <span className="faint fs-xs">没有匹配的模型名</span>}
                  </div>
                </>
              );
            })()}
        {modelMsg && <Note>{modelMsg}</Note>}
        {p.models.length > 0 && <div className="faint fs-xs mt-2">点击模型块切换启用/停用（按裸模型名记账，跨平台生效）。</div>}
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

function AuthPanel({ p, onChanged, onDone }: { p: Platform; onChanged: () => void; onDone?: () => void }) {
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
      // ★ 向导模式：登录成功即自动进下一步（方案屏 3：成功自动前进，不用手点）
      onDone?.();
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
