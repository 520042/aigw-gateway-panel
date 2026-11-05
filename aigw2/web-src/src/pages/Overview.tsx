import { Kpi, Card, Note, Loading, ErrorBox, useAsync } from '../components';
import { api, type Usage, type PlatformResponse, type AccountsResponse, type Health } from '../api';

const fmt = (n: number) => Number(n || 0).toLocaleString('en-US');
const ms = (v: number | undefined) =>
  v == null ? '—' : v >= 1000 ? (v / 1000).toFixed(1) + 's' : Math.round(v) + 'ms';

export function Overview({ health }: { health: Health | null }) {
  const pl = useAsync<PlatformResponse>(() => api.get('/api/platforms'), []);
  const ac = useAsync<AccountsResponse>(() => api.get('/api/accounts'), []);
  const us = useAsync<Usage>(() => api.get('/api/usage'), []);

  if (pl.error || ac.error || us.error) {
    return <ErrorBox message={pl.error ?? ac.error ?? us.error ?? ''} onRetry={() => { pl.reload(); ac.reload(); us.reload(); }} />;
  }
  if (!pl.data || !ac.data || !us.data) return <Loading />;

  const enabled = pl.data.rows.filter((p) => p.enabled).length;
  const realReq = Math.max(0, us.data.requests - us.data.cancelled);
  const rate = realReq ? Math.round((us.data.success / realReq) * 100) : 100;
  const lat = us.data.latency ?? { p50: 0, p95: 0, samples: 0 };
  const ttfb = us.data.ttfb ?? { p50: 0, p95: 0, samples: 0 };
  const platRows = [...(us.data.byPlatform?.rows ?? [])]
    .sort((a, b) => b.requests - a.requests).slice(0, 6);
  const maxReq = Math.max(1, ...platRows.map((r) => r.requests));

  const tasks = Object.entries(health?.tasks ?? {});
  const ago = (s: number | null) => s == null ? '—' : s < 60 ? `${s}s 前` : s < 3600 ? `${Math.round(s / 60)}分前` : `${Math.round(s / 3600)}小时前`;

  return (
    <>
      <div className="grid g4 mb-3">
        <Kpi label="平台" value={`${enabled}/${pl.data.total}`} hint="启用 / 声明式 spec + 自定义" />
        <Kpi label="账号" value={ac.data.total} hint="已导入 · secret 仅后台使用" />
        <Kpi label="累计调用" value={fmt(us.data.requests)} hint={`成功 ${fmt(us.data.success)} · 失败 ${fmt(us.data.failed)}`} />
        <Kpi label="成功率" value={`${rate}%`} tone={rate >= 90 ? 'ok' : rate >= 60 ? 'warn' : 'err'} hint={`tokens ${fmt(us.data.tokensIn)} in / ${fmt(us.data.tokensOut)} out`} />
      </div>

      <Card title="运行状态">
        <div className="mini-grid">
          <div className="mini"><b>{health?.service ?? 'aigw'} v{health?.version ?? '—'}</b><span className="pill ok">{health?.milestone ?? '—'}</span></div>
          <div className="mini"><b>存储</b><span className="pill ok">SQLite WAL</span></div>
          <div className="mini">
            <b>端点</b>
            <code>/v1/chat/completions</code> · <code>/v1/messages</code> · <code>/v1/responses</code>
            <br /><span className="faint">模型路由：清单匹配 或 <code>模型id@平台</code></span>
          </div>
        </div>
      </Card>

      <div className="grid g2 mt-3">
        <Card title="延迟分位">
          <div className="lat-grid">
            <div className="lat"><span className="lat-k">首字节 p50</span><span className="lat-v">{ms(ttfb.p50)}</span></div>
            <div className="lat"><span className="lat-k">首字节 p95</span><span className="lat-v">{ms(ttfb.p95)}</span></div>
            <div className="lat"><span className="lat-k">总耗时 p50</span><span className="lat-v">{ms(lat.p50)}</span></div>
            <div className="lat"><span className="lat-k">总耗时 p95</span><span className="lat-v">{ms(lat.p95)}</span></div>
          </div>
          <div className="faint small mt-2">总耗时样本 {fmt(lat.samples)} 条 · 首字节样本 {fmt(ttfb.samples)} 条</div>
        </Card>
        <Card title="平台用量排行">
          {platRows.length === 0 ? <div className="faint small">还没有调用记录</div> : platRows.map((r) => {
            const denom = Math.max(1, r.requests - (r.cancelled ?? 0));
            const pct = Math.round((r.success / denom) * 100);
            return (
              <div className="rk-row" key={r.key}>
                <span className="rk-name mono" title={r.key}>{r.key}</span>
                <div className="rk-bar"><i style={{ width: `${Math.round((r.requests / maxReq) * 100)}%` }} /></div>
                <span className="rk-num">{fmt(r.requests)}</span>
                <span className={'pill ' + (pct >= 90 ? 'ok' : pct >= 60 ? 'warn' : 'err')}>{pct}%</span>
              </div>
            );
          })}
        </Card>
      </div>

      <Card title="后台任务" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>每天自动执行 · 详情见「模型与用量」</span>}>
        {tasks.length === 0 ? <div className="faint small">暂无任务记录</div> : (
          <div className="task-ledger">
            {tasks.map(([k, t]) => (
              <div className="task-ledger-row" key={k}>
                <span className="mono">{k}</span>
                <span className={t.err_runs > 0 && (t.last_ok_s_ago == null || (t.last_err_s_ago ?? 0) < t.last_ok_s_ago) ? 'c-err' : 'c-ok'}>
                  成功 {t.ok_runs} · 失败 {t.err_runs}
                </span>
                <span className="faint small">{ago(t.last_ok_s_ago)}</span>
                {t.last_ok && <span className="faint small task-msg" title={t.last_ok}>{t.last_ok}</span>}
              </div>
            ))}
          </div>
        )}
      </Card>

      {us.data.failed > 0 && (
        <Note tone="warn">近 24 小时有 {fmt(us.data.failed)} 次失败。到「日志」页看失败根因，或到「接入源」页筛「有问题」。</Note>
      )}
    </>
  );
}
