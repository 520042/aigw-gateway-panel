import { useEffect, useState } from 'react';
import { Card, Loading, ErrorBox, Note, useAsync } from '../components';
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
    </>
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
