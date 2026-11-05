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

      <Card title="请求处理管线">
        <div className="preset-grid">
          {PRESETS.map(([id, name]) => (
            <div className="preset-chip" key={id}>
              <span className="pill brand">{name}</span>
              <span className="faint fs-xs mono">/api/{id}</span>
            </div>
          ))}
        </div>
        <div className="faint small mt-2">
          这些配置各有独立端点，面板「接入源」与「模型与用量」页会分别展示对应的开关与效果。
        </div>
      </Card>
    </>
  );
}
