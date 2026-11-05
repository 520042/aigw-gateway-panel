import { useState } from 'react';
import { Card, Kpi, Loading, ErrorBox, useAsync } from '../components';
import { api, type Usage } from '../api';

const fmt = (n: number) => Number(n || 0).toLocaleString('en-US');
const ms = (v: number | undefined) => v == null ? '—' : v >= 1000 ? (v / 1000).toFixed(1) + 's' : Math.round(v) + 'ms';

/** 零依赖 SVG 面积图（与旧版 trendSVG 同形状，但作为 React 组件可复用）。
 *  悬停 tooltip（借鉴 sub2api TokenUsageTrend 的 index-mode 交互）：
 *  鼠标横移按 x 定位到最近的样本点，显示该请求的 tokens 与时间。 */
function Trend({ values, stamps }: { values: number[]; stamps: number[] }) {
  const [hover, setHover] = useState<number | null>(null);
  if (!values.length) return <div className="faint small">暂无样本</div>;
  const w = 760, h = 160, max = Math.max(1, ...values);
  const pts = values.map((v, i) => `${(i / Math.max(1, values.length - 1)) * w},${h - (v / max) * (h - 12)}`);
  const d = `M0,${h} L${pts.join(' L')} L${w},${h} Z`;
  const onMove = (e: React.MouseEvent<SVGSVGElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const x = ((e.clientX - rect.left) / rect.width) * w;
    const idx = Math.round((x / w) * (values.length - 1));
    setHover(Math.max(0, Math.min(values.length - 1, idx)));
  };
  const fmtT = (n: number) => n >= 1_000_000 ? (n / 1e6).toFixed(1) + 'M' : n >= 1000 ? (n / 1e3).toFixed(1) + 'K' : String(n);
  const hx = hover == null ? 0 : (hover / Math.max(1, values.length - 1)) * w;
  const hy = hover == null ? 0 : h - (values[hover] / max) * (h - 12);
  const label = hover == null ? '' : (() => {
    const ts = stamps[hover];
    const t = ts ? new Date(ts * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) : '';
    return `${t} · ${fmtT(values[hover])} tokens`;
  })();
  return (
    <div style={{ position: 'relative' }}>
      <svg
        viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="trend-svg"
        role="img" aria-label="请求趋势"
        onMouseMove={onMove} onMouseLeave={() => setHover(null)}
      >
        <defs>
          <linearGradient id="trendFill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--brand)" stopOpacity="0.35" />
            <stop offset="100%" stopColor="var(--brand)" stopOpacity="0.02" />
          </linearGradient>
        </defs>
        <path d={d} fill="url(#trendFill)" stroke="var(--brand)" strokeWidth="1.5" />
        {hover != null && (
          <g>
            <line x1={hx} y1={0} x2={hx} y2={h} stroke="var(--brand)" strokeWidth="1" strokeDasharray="4 3" opacity="0.6" />
            <circle cx={hx} cy={hy} r="4" fill="var(--brand)" stroke="var(--surface)" strokeWidth="2" />
          </g>
        )}
      </svg>
      {hover != null && label && (
        <div className="trend-tip mono" style={{ left: `${(hx / w) * 100}%` }}>{label}</div>
      )}
    </div>
  );
}

export function UsagePage() {
  const us = useAsync<Usage>(() => api.get('/api/usage'), []);
  const hist = useAsync<{ recent: Array<{ ts?: number; total_tokens?: number; tokensOut?: number }> }>(
    () => api.get('/api/requests?limit=60'), [],
  );
  if (us.error) return <ErrorBox message={us.error} onRetry={us.reload} />;
  if (us.loading || !us.data) return <Loading />;

  const d = us.data;
  const realReq = Math.max(0, d.requests - d.cancelled);
  const rate = realReq ? Math.round((d.success / realReq) * 100) : 100;
  const rows = d.byPlatform?.rows ?? [];
  const last = rows.slice(0, 8);
  const maxOut = Math.max(1, ...last.map((r) => r.requests));
  const histRows = hist.data?.recent ?? [];
  const trend = histRows.map((r) => r.total_tokens ?? r.tokensOut ?? 0).reverse();
  const stamps = histRows.map((r) => r.ts ?? 0).reverse();

  return (
    <>
      <div className="grid g4 mb-3">
        <Kpi label="请求数" value={fmt(d.requests)} />
        <Kpi label="成功" value={fmt(d.success)} tone="ok" />
        <Kpi label="失败" value={fmt(d.failed)} tone={d.failed ? 'err' : undefined} hint={`另有 ${fmt(d.cancelled)} 次客户端取消`} />
        <Kpi label="成功率" value={`${rate}%`} tone={rate >= 90 ? 'ok' : 'warn'} />
      </div>

      <div className="grid g2">
        <Card title="首字节 TTFB">
          <div className="vl">{ms(d.ttfb?.p50)}</div>
          <div className="faint small">p95 {ms(d.ttfb?.p95)} · {fmt(d.ttfb?.samples ?? 0)} 次流式样本</div>
        </Card>
        <Card title="整轮耗时">
          <div className="vl">{ms(d.latency?.p50)}</div>
          <div className="faint small">p95 {ms(d.latency?.p95)} · {fmt(d.latency?.samples ?? 0)} 次样本</div>
        </Card>
      </div>

      <Card title="输出 tokens 趋势" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>最近 {trend.length} 条 · 悬停看单条明细</span>}>
        <Trend values={trend} stamps={stamps} />
      </Card>

      <Card title="平台输出榜">
        {last.length === 0 ? <div className="faint small">暂无数据</div> : last.map((r) => (
          <div className="rk-row" key={r.key}>
            <span className="rk-name mono">{r.key}</span>
            <div className="rk-bar"><i style={{ width: `${Math.round((r.requests / maxOut) * 100)}%` }} /></div>
            <span className="rk-num">{fmt(r.requests)}</span>
          </div>
        ))}
      </Card>

      <Card title="token 用量">
        <div className="mini-grid">
          <div className="mini"><b>输入</b><span className="mono">{fmt(d.tokensIn)}</span></div>
          <div className="mini"><b>输出</b><span className="mono">{fmt(d.tokensOut)}</span></div>
          <div className="mini"><b>合计</b><span className="mono">{fmt(d.tokensIn + d.tokensOut)}</span></div>
        </div>
      </Card>
    </>
  );
}
