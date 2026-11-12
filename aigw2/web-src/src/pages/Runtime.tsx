import { useEffect, useState } from 'react';
import { Card, Loading, ErrorBox, Note, Empty, Pill, useAsync } from '../components';
import { api } from '../api';

interface RuntimeField { name: string; default: number; min: number; max: number }
interface RuntimeCfg {
  cfg: {
    fields: RuntimeField[];
    [key: string]: unknown;
  };
}

const HINTS: Record<string, string> = {
  max_inflight: '单账号在单个模型上的最大并发',
  upstream_timeout_s: '上游读写超时',
  max_account_tries: '一次请求最多换几个账号重试',
  per_ip_rpm: '每 IP 每分钟请求上限，0 = 不限',
  global_rpm: '全局每分钟请求上限，0 = 不限',
  key_rpm: '每把访问密钥每分钟上限，0 = 不限',
  select_ceiling: '选号策略：0 = 默认（快到期优先 → 余额升序）',
};

/** 字段人话名 + 单位。★ 原先主标题直接铺 `max_inflight` 这种 Rust 字段名，
 *  中文说明缩成第二行小字 —— 对用户来说等于没有标签。字段名挪进 tooltip。 */
const FIELD: Record<string, { cn: string; unit?: string }> = {
  max_inflight: { cn: '单账号并发上限', unit: '个' },
  upstream_timeout_s: { cn: '上游读超时', unit: '秒' },
  max_account_tries: { cn: '换号重试次数', unit: '次' },
  per_ip_rpm: { cn: '每 IP 每分钟上限', unit: '次/分' },
  global_rpm: { cn: '全局每分钟上限', unit: '次/分' },
  key_rpm: { cn: '每密钥每分钟上限', unit: '次/分' },
  select_ceiling: { cn: '选号上限', unit: '' },
};

const PRESETS: Array<[string, string]> = [
  ['sysprompt', '系统提示词注入'], ['history_trim', '历史裁剪'],
  ['think_tags', '思考标签处理'], ['model_map', '模型名重定向'],
  ['param_override', '参数覆盖'],
];

/** 每条管线的 enabled 判据（响应形状各异，逐个对齐实测）。 */
function enabledOf(id: string, v: unknown): boolean | null {
  const j = v as { cfg?: Record<string, unknown>; mode?: unknown } | null;
  if (!j) return null;
  switch (id) {
    case 'sysprompt': return Boolean(j.cfg?.system_prompt);
    case 'history_trim': return Boolean(j.cfg?.enabled);
    case 'think_tags': return j.mode != null;
    case 'model_map': return Object.keys(j.cfg?.mapping ?? {}).length > 0;
    case 'param_override': return Boolean(j.cfg?.enabled);
    default: return null;
  }
}

function PipelineCard() {
  // 5 条管线各一个 GET；并行拉，全部完成才渲染（避免闪空态）。
  const [states, setStates] = useState<Record<string, unknown> | null>(null);
  const [err, setErr] = useState('');
  const load = () => {
    Promise.all(PRESETS.map(([id]) => api.get<unknown>('/api/' + id)))
      .then((vs) => { setStates(Object.fromEntries(PRESETS.map(([id], i) => [id, vs[i]]))); setErr(''); })
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  };
  useEffect(load, []);
  return (
    <Card title="请求处理管线" extra={
      <button className="btn sm gh" style={{ marginLeft: 'auto', fontWeight: 400 }} onClick={load}>刷新</button>
    }>
      {err ? <ErrorBox message={err} onRetry={load} /> : !states ? <Loading /> : (
        <div className="preset-grid">
          {PRESETS.map(([id, name]) => {
            const on = enabledOf(id, states[id]);
            return (
              <div className="preset-chip" key={id}>
                <span className={'pill ' + (on ? 'ok' : 'mut')}>{on ? '开启' : on === false ? '未启用' : '—'}</span>
                <b>{name}</b>
                <span className="faint fs-xs mono">/api/{id}</span>
              </div>
            );
          })}
        </div>
      )}
      <div className="faint small mt-2">
        状态实时读取；配置端点为 <code className="mono">POST /api/&lt;管线名&gt;</code>（legacy 脚本或 curl 可调）。
      </div>
    </Card>
  );
}

export function Runtime() {
  const { data, error, loading, reload } = useAsync<RuntimeCfg>(() => api.get('/api/runtime'), []);
  const [draft, setDraft] = useState<Record<string, number>>({});
  const [msg, setMsg] = useState('');

  // ★ /api/runtime 的形状是 `cfg: { fields: [...], max_inflight: 2, ... }` ——
  //   fields 与实际值都在 cfg 里（第一版把 fields 当成顶层，结果表单全空）。
  //   这里只取 fields 里声明过的数值字段，避开 updated_at / fields 本身。
  useEffect(() => {
    if (!data?.cfg?.fields) return;
    const d: Record<string, number> = {};
    for (const f of data.cfg.fields) {
      const v = data.cfg[f.name];
      d[f.name] = typeof v === 'number' ? v : f.default;
    }
    setDraft(d);
  }, [data]);
  if (error) return <ErrorBox message={error} onRetry={reload} />;
  if (loading || !data) return <Loading />;

  const save = async () => {
    try {
      await api.post('/api/runtime', { cfg: draft });
      setMsg('✔ 已保存并立即生效（无需重启）');
      reload();
    } catch (e) { setMsg('保存失败：' + (e instanceof Error ? e.message : String(e))); }
  };

  return (
    <>
      <Card title="运行配置" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>改完立即生效</span>}>
        <div className="runtime-grid">
          {(data.cfg.fields ?? []).map((f) => {
            const meta = FIELD[f.name];
            return (
              <div className="runtime-row" key={f.name}>
                <div className="runtime-label" title={`${f.name} · 默认 ${f.default}`}>
                  <b>{meta?.cn ?? f.name}</b>
                  <span className="faint fs-xs">
                    {HINTS[f.name] ?? ''}
                    {` · 范围 ${f.min}–${f.max}，默认 ${f.default}`}
                  </span>
                </div>
                <span className="rt-input">
                  <input
                    className="q mono rt-num" type="number"
                    min={f.min} max={f.max}
                    value={draft[f.name] ?? f.default}
                    onChange={(e) => setDraft((d) => ({ ...d, [f.name]: Number(e.target.value) }))}
                  />
                  {meta?.unit && <span className="rt-unit faint fs-xs">{meta.unit}</span>}
                </span>
              </div>
            );
          })}
        </div>
        <div className="row mt-3">
          <button className="btn pri" onClick={save}>保存并立即生效</button>
          <button className="btn gh" onClick={reload}>刷新</button>
        </div>
        {msg && <Note tone={msg.startsWith('✔') ? 'ok' : 'err'}>{msg}</Note>}
      </Card>

      <PipelineCard />
      <ProxyCard />
      <CombosCard />
      <PoolCard />
      <KeysCard />
    </>
  );
}

/** 访问密钥（GET/POST /api/keys）。
 *
 * ★ 2026-11-10：`/api/keys` 在前端**一次都没被调用过** ——
 *   这是对外发的那串 sk- 凭据，增删改轮换全靠 curl，等于没有管理面。
 *   明文只在 action=add 时回显一次（后端设计如此），这里必须显眼提示复制。 */
interface KeyRow {
  id: string; name: string; note: string; enabled: boolean;
  allow_models?: string[]; key_broken?: boolean;
}

function KeysCard() {
  const list = useAsync<{ rows?: KeyRow[]; count?: number }>(
    () => api.get('/api/keys'), [],
  );
  const [name, setName] = useState('');
  const [note, setNote] = useState('');
  const [allow, setAllow] = useState('');
  const [msg, setMsg] = useState('');
  const [fresh, setFresh] = useState('');   // 新建后回显一次的明文
  const [busy, setBusy] = useState(false);

  const post = async (body: Record<string, unknown>, done: string) => {
    setBusy(true);
    try {
      const r = await api.post('/api/keys', body) as { key?: string; unknown_models?: string[] };
      if (body.action === 'add' && r?.key) setFresh(r.key);
      if (body.action === 'update' && (body as { rotate?: boolean }).rotate && r?.key) setFresh(r.key);
      const unknown = r?.unknown_models ?? [];
      setMsg('✔ ' + done + (unknown.length ? `（注意：${unknown.join('、')} 当前匹配不到任何模型）` : ''));
      list.reload();
    } catch (e) { setMsg('失败：' + (e instanceof Error ? e.message : String(e))); }
    finally { setBusy(false); }
  };

  const create = () => {
    const n = name.trim();
    if (!n) { setMsg('名字不能为空（写清这把 key 给谁用）'); return; }
    const models = allow.split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
    void post({ action: 'add', name: n, note: note.trim(), allow_models: models },
      `已创建「${n}」`);
    setName(''); setNote(''); setAllow('');
  };

  const rows = list.data?.rows ?? [];

  return (
    <Card title="访问密钥" extra={
      <span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
        客户端调用 /v1 用的 sk- 凭据 · {rows.length} 把
      </span>
    }>
      {list.loading && !list.data ? <Loading /> : list.error ? <ErrorBox message={list.error} onRetry={list.reload} /> : (
        <>
          {fresh && (
            <Note tone="ok">
              <b>新密钥（只显示这一次，请立刻复制）：</b>
              <div className="row mt-2">
                <input className="q mono" readOnly value={fresh} onFocus={(e) => e.currentTarget.select()} />
                <button className="btn sm" onClick={() => { void navigator.clipboard?.writeText(fresh); }}>复制</button>
                <button className="btn sm gh" onClick={() => setFresh('')}>我已保存</button>
              </div>
            </Note>
          )}
          {rows.length === 0 && <Empty>还没有访问密钥。填下面的表单创建一把，客户端才能调用 /v1。</Empty>}
          {rows.map((k) => (
            <div className="key-row" key={k.id}>
              <div className="key-hd">
                <b>{k.name}</b>
                <Pill tone={k.enabled ? 'ok' : 'mut'}>{k.enabled ? '启用' : '停用'}</Pill>
                {k.key_broken && <Pill tone="err">密文损坏</Pill>}
                <span className="faint fs-xs mono">{k.id}</span>
              </div>
              {k.note && <div className="faint fs-xs">{k.note}</div>}
              <div className="combo-models">
                {(k.allow_models ?? []).length === 0
                  ? <span className="faint fs-xs">不限模型</span>
                  : (k.allow_models ?? []).map((m) => <span className="model-chip" key={m}>{m}</span>)}
              </div>
              <div className="key-acts">
                <button className="btn sm gh" disabled={busy}
                  onClick={() => post({ action: 'update', id: k.id, enabled: !k.enabled },
                    `${k.enabled ? '已停用' : '已启用'}「${k.name}」`)}>
                  {k.enabled ? '停用' : '启用'}
                </button>
                <button className="btn sm gh" disabled={busy}
                  onClick={() => { if (confirm(`轮换「${k.name}」的密钥？旧密钥立刻失效，客户端要换成新的。`)) void post({ action: 'update', id: k.id, rotate: true }, `已轮换「${k.name}」`); }}>
                  轮换密钥
                </button>
                <button className="btn sm gh danger" disabled={busy}
                  onClick={() => { if (confirm(`删除访问密钥「${k.name}」？用它调用的客户端会立刻失败。`)) void post({ action: 'remove', id: k.id }, `已删除「${k.name}」`); }}>
                  删除
                </button>
              </div>
            </div>
          ))}
          <div className="key-new">
            <div className="faint fs-xs mb-2">新建访问密钥</div>
            {/* ★ 原先三个输入全靠 placeholder 当标签，输入后标签消失，
                填到第三个框时已经分不清哪个是名字哪个是模型白名单。 */}
            <div className="key-form">
              <label className="key-field">
                <span>名字</span>
                <input className="q" placeholder="给谁用，如「部门脚本」" value={name} onChange={(e) => setName(e.target.value)} />
              </label>
              <label className="key-field">
                <span>备注（可选）</span>
                <input className="q" placeholder="用途说明" value={note} onChange={(e) => setNote(e.target.value)} />
              </label>
              <label className="key-field">
                <span>允许的模型（留空 = 不限）</span>
                <input className="q mono" placeholder="kilo-auto/free, gpt-4o" value={allow} onChange={(e) => setAllow(e.target.value)} />
              </label>
              <button className="btn pri" disabled={busy} onClick={create}>创建密钥</button>
            </div>
          </div>
          {msg && <Note tone={msg.startsWith('✔') ? 'ok' : 'err'}>{msg}</Note>}
        </>
      )}
    </Card>
  );
}

/** 模型链（GET/POST /api/combos）。
 *
 * ★ 2026-11-10：/api/combos 一直有完整增删改（add/update/remove），
 *   但界面零入口——而实际已经在跑 `combo:kilo-fallback` 这条链。
 *   模型链是「主模型挂了自动换下一个」的开关，看不见就等于没法管。 */
interface ComboRow {
  id: string; name: string; models: string[]; strategy: string;
  enabled: boolean; sticky_limit: number;
  effective?: string[]; unavailable?: string[];
}

function CombosCard() {
  const list = useAsync<{ rows?: ComboRow[]; count?: number }>(
    () => api.get('/api/combos'), [],
  );
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);

  const act = async (body: Record<string, unknown>, done: string) => {
    setBusy(true);
    try {
      await api.post('/api/combos', body);
      setMsg('✔ ' + done);
      list.reload();
    } catch (e) { setMsg('失败：' + (e instanceof Error ? e.message : String(e))); }
    finally { setBusy(false); }
  };

  const rows = list.data?.rows ?? [];

  return (
    <Card title="模型链" extra={
      <span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
        主模型不可用时按顺序换下一个 · {rows.length} 条
      </span>
    }>
      {list.loading && !list.data ? <Loading /> : list.error ? <ErrorBox message={list.error} onRetry={list.reload} /> : (
        <>
          {rows.length === 0 && <Empty>还没有模型链。新建需要 POST /api/combos（界面暂未提供表单），创建后可在这里启停与删除。</Empty>}
          {rows.map((c) => (
            <div className="combo-row" key={c.id}>
              <div className="combo-hd">
                <b>{c.name}</b>
                <Pill tone={c.enabled ? 'ok' : 'mut'}>{c.enabled ? '启用' : '停用'}</Pill>
                <span className="faint fs-xs">{c.strategy} · sticky {c.sticky_limit}</span>
                <div className="combo-acts">
                  <button className="btn sm gh" disabled={busy}
                    onClick={() => act({ action: 'update', id: c.id, enabled: !c.enabled },
                      `${c.enabled ? '已停用' : '已启用'} ${c.name}`)}>
                    {c.enabled ? '停用' : '启用'}
                  </button>
                  <button className="btn sm gh" disabled={busy}
                    onClick={() => { if (confirm(`删除模型链 ${c.name}？`)) act({ action: 'remove', id: c.id }, `已删除 ${c.name}`); }}>
                    删除
                  </button>
                </div>
              </div>
              <div className="combo-models">
                {c.models.map((m) => {
                  const bad = (c.unavailable ?? []).includes(m);
                  return (
                    <span className={'model-chip' + (bad ? ' off' : '')} key={m} title={bad ? '当前不可用（上游没有这个模型）' : m}>
                      {m}{bad ? ' ✕' : ''}
                    </span>
                  );
                })}
              </div>
            </div>
          ))}
          {msg && <Note tone={msg.startsWith('✔') ? 'ok' : 'err'}>{msg}</Note>}
        </>
      )}
    </Card>
  );
}

/** 号池实时快照（GET /api/pool，POST action=thaw）。
 *
 * ★ 一直有后端但没界面：账号卡在冷却/WAF 里时，用户只能等它自己过期，
 *   而 thaw 就是「立刻解冻」。排障时这是最需要的按钮。 */
interface PoolSnapshot {
  cooling?: Array<Record<string, unknown>>;
  waf?: Array<Record<string, unknown>>;
  inflight?: Array<Record<string, unknown>>;
  max_inflight?: number;
  sticky_entries?: number;
  sticky_max_entries?: number;
}

function PoolCard() {
  const snap = useAsync<PoolSnapshot>(() => api.get('/api/pool'), []);
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);

  const thaw = async (account: string) => {
    setBusy(true);
    try {
      await api.post('/api/pool', { action: 'thaw', account });
      setMsg(`✔ 已解冻 ${account}`);
      snap.reload();
    } catch (e) { setMsg('解冻失败：' + (e instanceof Error ? e.message : String(e))); }
    finally { setBusy(false); }
  };

  const d = snap.data ?? {};
  const cooling = d.cooling ?? [];
  const waf = d.waf ?? [];
  const inflight = d.inflight ?? [];
  const keyOf = (x: Record<string, unknown>, i: number) =>
    String(x.account ?? x.id ?? x.key ?? `#${i}`);

  return (
    <Card title="号池状态" extra={
      <span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
        进行中 {inflight.length}/{d.max_inflight ?? '—'} · 冷却 {cooling.length} · WAF {waf.length} · 粘性 {d.sticky_entries ?? 0}/{d.sticky_max_entries ?? '—'}
      </span>
    }>
      {snap.loading && !snap.data ? <Loading /> : snap.error ? <ErrorBox message={snap.error} onRetry={snap.reload} /> : (
        <>
          {cooling.length === 0 && waf.length === 0 && (
            <Empty>没有账号在冷却或限速中，号池健康。</Empty>
          )}
          {[...cooling.map((x) => ({ x, kind: '冷却' })), ...waf.map((x) => ({ x, kind: 'WAF' }))]
            .map(({ x, kind }, i) => (
              <div className="pool-row" key={kind + keyOf(x, i)}>
                <Pill tone="warn">{kind}</Pill>
                <b className="mono fs-xs">{keyOf(x, i)}</b>
                <span className="faint fs-xs">
                  {x.until != null ? `到 ${new Date(Number(x.until) * 1000).toLocaleTimeString('zh-CN')}` : ''}
                  {x.reason != null ? ` · ${String(x.reason)}` : ''}
                  {x.left_s != null ? ` · 剩 ${String(x.left_s)}s` : ''}
                </span>
                <button className="btn sm gh ml-auto" disabled={busy} onClick={() => thaw(keyOf(x, i))}>
                  立即解冻
                </button>
              </div>
            ))}
          {msg && <Note tone={msg.startsWith('✔') ? 'ok' : 'err'}>{msg}</Note>}
        </>
      )}
    </Card>
  );
}

/** 出站代理（GET/POST /api/proxy）。
 *
 * ★ 2026-11-10：这个端点和它的三个模式一直都在后端，但界面从来没入口——
 *   代理配错时用户只能改配置文件，而走代理是所有上游请求的总开关。
 *   放在运行配置页最下面（比并发/超时更少动，但出错影响全局）。 */
function ProxyCard() {
  const proxy = useAsync<{
    mode?: string; modes?: string[]; url?: string; ok?: boolean;
  }>(() => api.get('/api/proxy'), []);
  const [mode, setMode] = useState('direct');
  const [url, setUrl] = useState('');
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!proxy.data) return;
    setMode(proxy.data.mode ?? 'direct');
    setUrl(proxy.data.url ?? '');
  }, [proxy.data]);

  const save = async () => {
    setBusy(true);
    try {
      await api.post('/api/proxy', { mode, url });
      setMsg('✔ 已保存并立即生效');
      proxy.reload();
    } catch (e) { setMsg('保存失败：' + (e instanceof Error ? e.message : String(e))); }
    finally { setBusy(false); }
  };

  const MODE_HINT: Record<string, string> = {
    direct: '直连（不走代理）',
    global: '全部上游请求走代理',
    bypass_cn: '国内域名直连，境外走代理',
  };

  return (
    <Card title="出站代理" extra={
      <span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
        影响所有上游请求 · 改完立即生效
      </span>
    }>
      {proxy.loading && !proxy.data ? <Loading /> : proxy.error ? <ErrorBox message={proxy.error} onRetry={proxy.reload} /> : (
        <>
          <div className="runtime-grid">
            <div className="runtime-row">
              <div className="runtime-label">
                <b>模式</b>
                <span className="faint fs-xs">{MODE_HINT[mode] ?? ''}</span>
              </div>
              <select className="q w-auto" value={mode} onChange={(e) => setMode(e.target.value)}>
                {(proxy.data?.modes ?? ['direct', 'global', 'bypass_cn']).map((m) => (
                  <option key={m} value={m}>{MODE_HINT[m] ?? m}</option>
                ))}
              </select>
            </div>
            {mode !== 'direct' && (
              <div className="runtime-row">
                <div className="runtime-label">
                  <b>代理地址</b>
                  <span className="faint fs-xs">如 http://127.0.0.1:7890</span>
                </div>
                <input className="q mono" placeholder="http://127.0.0.1:7890"
                  value={url} onChange={(e) => setUrl(e.target.value)} />
              </div>
            )}
          </div>
          <div className="row mt-3">
            <button className="btn pri" disabled={busy} onClick={save}>保存并立即生效</button>
          </div>
          {msg && <Note tone={msg.startsWith('✔') ? 'ok' : 'err'}>{msg}</Note>}
        </>
      )}
    </Card>
  );
}
