import { useState } from 'react';
import { Card, Kpi, Loading, ErrorBox, useAsync } from '../components';
import { api, type Usage } from '../api';

const fmt = (n: number) => Number(n || 0).toLocaleString('en-US');
/** 自定义接入源的 id 是 custom-<16位hash>-<序号>，直接铺在榜上像乱码。
 *  截成「自定义 · 前6位」既认得出是自己加的，又能对上接入源页里的条目。 */
const prettyKey = (k: string) => k.startsWith('custom-') ? `自定义 · ${k.slice(7, 13)}` : k;
const ms = (v: number | undefined) => v == null ? '—' : v >= 1000 ? (v / 1000).toFixed(1) + 's' : Math.round(v) + 'ms';

/** 零依赖 SVG 面积图（与旧版 trendSVG 同形状，但作为 React 组件可复用）。
 *  悬停 tooltip（借鉴 sub2api TokenUsageTrend 的 index-mode 交互）：
 *  鼠标横移按 x 定位到最近的样本点，显示该请求的 tokens 与时间。
 *
 *  ★ 2026-11-12 修两个实测问题：
 *  ① 数据源字段是 `tout`（/api/requests 的 recent 项），原代码读
 *     `total_tokens ?? tokensOut` —— 两个都不存在，values 全是 0，
 *     折线贴在底边上，看起来就是「一个空白框加一条横线」。
 *  ② 全 0 时不再画贴底直线，直接说明为什么没有数。 */
function Trend({ values, stamps }: { values: number[]; stamps: number[] }) {
  const [hover, setHover] = useState<number | null>(null);
  if (!values.length) return <div className="faint small">暂无样本</div>;
  const w = 760, h = 160, max = Math.max(1, ...values);
  const allZero = values.every((v) => !v);
  if (allZero) {
    return (
      <div className="faint small" style={{ padding: '28px 0' }}>
        这批请求没有回传 token 计数（流式响应或上游未计费）。换个模型或看「平台输出榜」的请求量。
      </div>
    );
  }
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
  const stamp = (t?: number) => t ? new Date(t * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) : '';
  const label = hover == null ? '' : (() => {
    const ts = stamps[hover];
    const t = ts ? new Date(ts * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) : '';
    return `${t} · ${fmtT(values[hover])} tokens`;
  })();
  return (
    <div style={{ position: 'relative' }}>
      {/* 峰值与起止时间：原先图上没有任何刻度，只能靠悬停猜量级。 */}
      <div className="trend-axis">
        <span className="faint fs-xs">峰值 {fmtT(max)} tokens</span>
        <span className="faint fs-xs">{stamp(stamps[0])} → {stamp(stamps[stamps.length - 1])}</span>
      </div>
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
        {[0.25, 0.5, 0.75].map((r) => (
          <line key={r} x1={0} y1={h * r} x2={w} y2={h * r} stroke="var(--border)" strokeWidth="1" strokeDasharray="3 4" />
        ))}
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
  // ★ /api/requests 的 recent 项字段是 tin / tout（不是 total_tokens / tokensOut，
  //   也不是 camelCase 的 tokensIn/Out）——类型漏了真字段名，图才会全 0。
  const hist = useAsync<{ recent: Array<{ ts?: number; tin?: number; tout?: number; total_tokens?: number; tokensOut?: number }> }>(
    () => api.get('/api/requests?limit=60'), [],
  );
  // ★ 请求明细下钻（借鉴 axonhub requests drawer）：点平台输出榜的行，
  //   展开该平台最近的请求表。后端 /api/requests 支持可选 platform 参数。
  const [drill, setDrill] = useState<string | null>(null);
  const drillQ = useAsync<{ recent: Array<{ ts: number; model: string; kind: string; ok: boolean; ms: number; tin: number; tout: number; cancelled: boolean }>; matched: number }>(
    () => api.get('/api/requests?limit=30&platform=' + encodeURIComponent(drill ?? '')),
    [drill],
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
  const trend = histRows.map((r) => r.tout ?? r.total_tokens ?? r.tokensOut ?? 0).reverse();
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

      <Card title="平台输出榜" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>条长 = 请求数 · 点行看该平台的请求明细</span>}>
        {last.length === 0 ? <div className="faint small">暂无数据</div> : last.map((r) => (
          <div
            className={'rk-row' + (drill === r.key ? ' on' : '')}
            key={r.key}
            style={{ cursor: 'pointer' }}
            title="点击查看该平台的请求明细"
            onClick={() => setDrill((v) => (v === r.key ? null : r.key))}
          >
            <span className="rk-name mono" title={r.key}>{prettyKey(r.key)}</span>
            <div className="rk-bar"><i style={{ width: `${Math.round((r.requests / maxOut) * 100)}%` }} /></div>
            <span className="rk-num">{fmt(r.requests)}</span>
          </div>
        ))}
      </Card>

      {drill && (
        <Card title={`请求明细 · ${prettyKey(drill)}`} extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
          {drillQ.data ? `共 ${fmt(drillQ.data.matched)} 条 · 最近 ${drillQ.data.recent.length} 条` : ''}
        </span>}>
          {drillQ.error ? <ErrorBox message={drillQ.error} onRetry={drillQ.reload} />
            : drillQ.loading && !drillQ.data ? <Loading />
              : (drillQ.data?.recent ?? []).length === 0 ? <div className="faint small">该平台在台账窗口内没有请求记录</div>
                : (
                  <div className="table-scroll">
                    <table>
                      <thead><tr><th>时间</th><th>模型 / 动作</th><th>状态</th><th>耗时</th><th className="mono">tokens in/out</th></tr></thead>
                      <tbody>
                        {drillQ.data!.recent.map((r, i) => (
                          <tr key={i}>
                            <td className="mono fs-xs">{new Date(r.ts * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' })}</td>
                            <td>{r.model}{r.kind !== 'chat' && <span className="pill mut ml-2">{r.kind}</span>}</td>
                            <td>{r.cancelled
                              ? <span className="pill mut">客户端取消</span>
                              : r.ok ? <span className="pill ok">成功</span> : <span className="pill err">失败</span>}</td>
                            <td className="mono fs-xs">{ms(r.ms)}</td>
                            <td className="mono fs-xs">{fmt(r.tin)} / {fmt(r.tout)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}
        </Card>
      )}

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
