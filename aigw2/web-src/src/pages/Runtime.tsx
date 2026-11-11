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
  upstream_timeout_s: '上游读写超时（秒）',
  max_account_tries: '一次请求最多换几个账号重试',
  per_ip_rpm: '每 IP 每分钟请求上限，0 = 不限',
  global_rpm: '全局每分钟请求上限，0 = 不限',
  key_rpm: '每把访问密钥每分钟上限，0 = 不限',
  select_ceiling: '选号策略：0 = 默认（快到期优先 → 余额升序）',
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
          {(data.cfg.fields ?? []).map((f) => (
            <div className="runtime-row" key={f.name}>
              <div className="runtime-label">
                <b>{f.name}</b>
                <span className="faint fs-xs">{HINTS[f.name] ?? `范围 ${f.min}-${f.max}，默认 ${f.default}`}</span>
              </div>
              <input
                className="q mono rt-num" type="number"
                min={f.min} max={f.max}
                value={draft[f.name] ?? f.default}
                onChange={(e) => setDraft((d) => ({ ...d, [f.name]: Number(e.target.value) }))}
              />
            </div>
          ))}
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
    </>
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
          {rows.length === 0 && <Empty>还没有模型链。用下面的表单新建一条，或直接调 POST /api/combos。</Empty>}
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
                  <option key={m} value={m}>{m}</option>
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
