import { useState } from 'react';
import { Kpi, Card, Note, Loading, ErrorBox, Pill, useAsync } from '../components';
import { api, type Usage, type AccountsResponse, type Health, type PlatformResponse } from '../api';

const fmt = (n: number) => Number(n || 0).toLocaleString('en-US');
const prettyKey = (k: string) => k.startsWith('custom-') ? `自定义 · ${k.slice(7, 13)}` : k;
const ms = (v: number | undefined) =>
  v == null ? '—' : v >= 1000 ? (v / 1000).toFixed(1) + 's' : Math.round(v) + 'ms';

/** 平台 × 小时健康网格响应（/api/pulse）。 */
interface PulseRow { platform: string; cells: Array<{ bucket: number; ok: number; fail: number }> }

const pad2 = (n: number) => String(n).padStart(2, '0');
const bucketLabel = (bucket: number) => {
  const d = new Date((bucket * 3600 - 8 * 3600) * 1000);
  return `${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${pad2(d.getHours())}:00`;
};

/**
 * 健康脉冲（借鉴 sub2api channel-monitor 的 RelayPulseMatrix，按我们台账裁剪）：
 * 每行一个平台，每格一小时；颜色 = 成功率档位（0-10 分，scoreToBand 同思路）。
 * 悬停 tooltip 给出该小时的具体数字。没流量的小时不画格子（时间轴上的空隙）。
 */
function Pulse({ rows, hours }: { rows: PulseRow[]; hours: number }) {
  const nowBucket = Math.floor((Date.now() / 1000 + 8 * 3600) / 3600);
  const band = (ok: number, fail: number): string => {
    const total = ok + fail;
    const score = total ? Math.round((ok / total) * 10) : 5;
    return 'pulse-s' + Math.max(0, Math.min(10, score));
  };
  return (
    <div className="pulse-scroll">
      <table className="pulse-table">
        <tbody>
          {rows.map((r) => (
            <tr key={r.platform}>
              <td className="pulse-name mono" title={r.platform}>{r.platform}</td>
              {r.cells.map((c) => {
                const age = nowBucket - c.bucket;
                return (
                  <td key={c.bucket} className={'pulse-cell ' + band(c.ok, c.fail)}
                    title={`${r.platform} · ${bucketLabel(c.bucket)}\n成功 ${c.ok} · 失败 ${c.fail}${age > hours ? '\n（超出窗口）' : ''}`}>
                    {(c.ok + c.fail) > 0 ? '' : ''}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** 后台任务内部名 → 人话。/api/health 的 tasks 键就是 Rust 侧的函数名，
 *  直接铺 `credits_refresh` / `warm_models` 对用户零信息量；键名留 title 供排查。 */
const TASK_CN: Record<string, string> = {
  credits_refresh: '积分快照刷新',
  custom_refresh: '自定义 API 扫描',
  daily_checkin: '每日签到',
  daily_growth: 'WB 日常任务',
  free_refresh: '公益站刷新',
  warm_models: '模型清单预热',
  history_trim: '台账裁剪',
  backup: '自动备份',
};

export function Overview({ health, platforms }: { health: Health | null; platforms: PlatformResponse | null }) {
  // 平台清单由框架层拉取一次传入（167KB，不该每次切页重取）；
  // 这里只拉轻量的账号与用量。
  const ac = useAsync<AccountsResponse>(() => api.get('/api/accounts'), []);
  const us = useAsync<Usage>(() => api.get('/api/usage'), []);
  const pulse = useAsync<{ rows: PulseRow[] }>(() => api.get('/api/pulse?hours=48'), []);

  if (ac.error || us.error) {
    return <ErrorBox message={ac.error ?? us.error ?? ''} onRetry={() => { ac.reload(); us.reload(); }} />;
  }
  if (!platforms || !ac.data || !us.data) return <Loading />;

  const enabled = platforms.rows.filter((p) => p.enabled).length;
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
        <Kpi label="平台" value={`${enabled}/${platforms.total}`} hint={`启用 / 全部 spec · 可用 ${health?.platforms ?? '—'}`} />
        {/* ★ 与接入源页「账号凭据」同口径（非免密）：总览此前算的是
            /api/accounts 全部行（含免密车道的合成账号），接入源页算的是
            逐平台 accounts_n 求和 —— 同一个概念两个数，看着像有一页算错。 */}
        <Kpi label="账号"
          value={ac.data.rows.filter((r) => !r.anon).length}
          hint={`已导入 · 免密车道另计 ${ac.data.rows.filter((r) => r.anon).length} 个`} />
        <Kpi label="累计调用" value={fmt(us.data.requests)} hint={`成功 ${fmt(us.data.success)} · 失败 ${fmt(us.data.failed)}`} />
        <Kpi label="成功率" value={`${rate}%`} tone={rate >= 90 ? 'ok' : rate >= 60 ? 'warn' : 'err'} hint={`tokens ${fmt(us.data.tokensIn)} in / ${fmt(us.data.tokensOut)} out`} />
      </div>

      <Card title="运行状态">
        <div className="mini-grid">          <div className="mini"><b>{health?.service ?? 'aigw'} v{health?.version ?? '—'}</b><span className="pill ok">{health?.milestone ?? '—'}</span></div>
          <div className="mini"><b>存储</b><span className="pill ok">SQLite WAL</span></div>
          <div className="mini">
            <b>端点</b>
            {/* ★ 原来三个端点用 `A</code> · <code>B` 串成一行，窄列里换行时
                中间那个「·」会孤零零挂在行尾。改成一个一行；「模型路由」
                那句太长，挪到卡片底部整宽显示（见下）。 */}
            <div className="mono fs-xs">/v1/chat/completions</div>
            <div className="mono fs-xs">/v1/messages</div>
            <div className="mono fs-xs">/v1/responses</div>
          </div>
        </div>
        <div className="faint fs-xs mt-2">
          模型路由：清单匹配，或 <code>模型id@平台</code>（也可以直接在模型链里写裸模型名）
        </div>
      </Card>

      <Card title="健康脉冲 · 近 48 小时" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>每格一小时 · 绿好红坏 · 悬停看数字</span>}>
        {pulse.error ? <div className="faint small">加载失败：{pulse.error}</div>
          : !pulse.data || pulse.data.rows.length === 0
            ? <div className="empty-inline">
                最近 48 小时没有调用记录。
                {us.data.requests > 0 && <span className="faint"> 累计 {fmt(us.data.requests)} 次调用都发生在更早。</span>}
              </div>
            : <Pulse rows={pulse.data.rows} hours={48} />}
      </Card>

      <LaneProbeCard />

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
        <Card title="平台用量排行" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>条长 = 请求数 · 右边 = 成功率</span>}>
          {platRows.length === 0 ? <div className="faint small">还没有调用记录</div> : platRows.map((r) => {
            const denom = Math.max(1, r.requests - (r.cancelled ?? 0));
            const pct = Math.round((r.success / denom) * 100);
            return (
              <div className="rk-row" key={r.key}>
                <span className="rk-name mono" title={r.key}>{prettyKey(r.key)}</span>
                <div className="rk-bar"><i style={{ width: `${Math.round((r.requests / maxReq) * 100)}%` }} /></div>
                <span className="rk-num">{fmt(r.requests)}</span>
                <span className={'pill ' + (pct >= 90 ? 'ok' : pct >= 60 ? 'warn' : 'err')}>{pct}%</span>
              </div>
            );
          })}
        </Card>
      </div>

      <Card title="后台任务" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>每天自动执行 · 签到与 WB 任务见「任务中心」</span>}>
        {tasks.length === 0 ? <div className="faint small">暂无任务记录</div> : (
          <div className="task-ledger">
            {tasks.map(([k, t]) => {
              const stale = t.last_ok_s_ago == null && t.last_err_s_ago == null;
              return (
                <div className="task-ledger-row" key={k} title={k}>
                  <span className="task-name">{TASK_CN[k] ?? k}</span>
                  <span className={t.err_runs > 0 && (t.last_ok_s_ago == null || (t.last_err_s_ago ?? 0) < t.last_ok_s_ago) ? 'c-err' : 'c-ok'}>
                    成功 {t.ok_runs} · 失败 {t.err_runs}
                  </span>
                  <span className="faint small">{stale ? '还没跑过' : ago(t.last_ok_s_ago)}</span>
                  {t.last_ok
                    ? <span className="faint small task-msg" title={t.last_ok}>{t.last_ok}</span>
                    : <span className="faint small">—</span>}
                </div>
              );
            })}
          </div>
        )}
      </Card>

      {us.data.failed > 0 && (
        <Note tone="warn">
          近 24 小时有 {fmt(us.data.failed)} 次失败（成功率 {rate}%）。先到「接入源」页筛
          「有问题」确认是哪些平台在失败；失败会按平台归因在「用量明细」里。
        </Note>
      )}
    </>
  );
}

/** 免密车道实时探针（借鉴 OmniRoute 免费池诚实口径 + CliRelay 健康解释）。
 *
 * ★ 脉冲矩阵是小时级历史，回答不了「这条车道**现在**能不能用」——
 *   zen 池的 IP 额度说没就没（429 带换出口提示）。这个卡当场打一发
 *   真实请求（每条 max_tokens=16），把 ok / 延迟 / 失败原因摆在眼前。
 *   不自动轮询：探测消耗上游额度，必须显式点击。 */
interface ProbeRow {
  platform: string; name: string; model?: string;
  ok: boolean; ms?: number; code?: string | null;
  message?: string; reply?: string;
}
interface ProbeResp { ok: boolean; probed_at: number; results: ProbeRow[] }

function LaneProbeCard() {
  const [resp, setResp] = useState<ProbeResp | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');

  const probe = async () => {
    setBusy(true);
    setErr('');
    try {
      setResp(await api.post('/api/probe-lanes', {}) as ProbeResp);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally { setBusy(false); }
  };

  const okN = resp?.results.filter((r) => r.ok).length ?? 0;

  return (
    <Card
      title="免密车道 · 实时探针"
      extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
        {resp ? `上次探测 ${new Date(resp.probed_at * 1000).toLocaleTimeString('zh-CN')} · 可用 ${okN}/${resp.results.length}` : '未探测 · 脉冲矩阵看历史，这里看现在'}
      </span>}
    >
      <div className="row">
        <button className="btn pri" disabled={busy} onClick={probe}>
          {busy ? '探测中（逐条打真实请求）…' : '⟳ 立即探测'}
        </button>
        <span className="faint fs-xs">
          每条车道发一次 max_tokens=16 的真实请求 · 消耗少量上游额度 · 不自动轮询
        </span>
      </div>
      {err && <Note tone="err">探测失败：{err}</Note>}
      {resp && (
        <div className="lane-probe-rows mt-2">
          {resp.results.map((r) => (
            <div className="lane-probe-row" key={r.platform}>
              <Pill tone={r.ok ? 'ok' : 'err'}>{r.ok ? '✓ 可用' : '✗ 不可用'}</Pill>
              <b>{r.name}</b>
              {r.model && <span className="faint mono fs-xs">{r.model}</span>}
              <span className="lane-probe-msg">
                {r.ok
                  ? <>延迟 <b>{ms(r.ms)}</b>{r.reply && <> · 回包 <span className="mono">「{r.reply}」</span></>}</>
                  : <span className="c-err">{r.message || '失败'}</span>}
              </span>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}
