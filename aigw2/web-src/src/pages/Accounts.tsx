import { useEffect, useMemo, useState } from 'react';
import { Card, Note, Loading, ErrorBox, Empty, Pill, useAsync } from '../components';
import {
  api,
  type AccountsResponse,
  type CheckinConfigResponse,
  type TaskRecordsResponse,
  type TaskRecord,
} from '../api';

const fmt = (n: number) => Number(n || 0).toLocaleString('en-US');
const pad = (n: number) => String(n).padStart(2, '0');
const hhmm = (h: number, m: number) => `${pad(h)}:${pad(m)}`;

type Tab = 'center' | 'schedule' | 'records';
const TABS: Array<[Tab, string]> = [
  ['center', '签到中心'],
  ['schedule', '定时签到'],
  ['records', '签到记录'],
];

/** 活动按钮的后端动作名必须与 wb_checkin::GROWTH_ACTIONS 一致。 */
const ACTIVITIES: Array<[string, string, string]> = [
  ['growth_profile', '▤ 成长概览', '等级 / 完成任务 / 能量 / 活跃地图'],
  ['streak_redeem', '◈ 连登奖励兑换', '逐档兑换本月可领的连登奖励'],
  ['lottery_draw', '✦ 限时抽奖', '用免费次数抽奖（409 重复视为已处理）'],
  ['travel_depart', '✈ 猫猫旅行', '出发旅行，随后可领取奖励'],
  ['travel_claim', '↩ 领取旅行奖励', '领回已完成旅行的奖励'],
  ['buddy_quota', '◻ 盲盒配额', '查能量余额与可开盒次数'],
  ['buddy_open', '🎁 开盲盒', '按可开次数最多连开 5 次'],
];

const SOURCE_LABEL: Record<string, string> = {
  manual: '手动', schedule: '定时', activity: '活动', startup: '补签',
};
const OUTCOME_LABEL: Record<string, string> = {
  success: '成功领取', already: '今日已领', failed: '失败',
};

function timeText(ts: number): string {
  if (!Number.isFinite(ts)) return '—';
  const d = new Date(ts * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function Accounts() {
  const ac = useAsync<AccountsResponse>(() => api.get('/api/accounts'), []);
  const cfg = useAsync<CheckinConfigResponse>(
    () => api.post('/api/checkin', { action: 'config' }), [],
  );
  const [tab, setTab] = useState<Tab>('center');
  const [time, setTime] = useState('09:00');
  const [picked, setPicked] = useState<string[]>([]);
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);

  // 配置到位后同步本地表单；之后用户编辑不会被后台轮询覆盖。
  useEffect(() => {
    if (!cfg.data?.cfg) return;
    const c = cfg.data.cfg;
    setTime(hhmm(c.run_hour, c.run_minute ?? 0));
    setPicked(c.per_platform ? c.checkin_platforms : (cfg.data.platforms ?? []).map((p) => p.id));
  }, [cfg.data]);

  if (ac.error || cfg.error) {
    return <ErrorBox message={ac.error ?? cfg.error ?? ''} onRetry={() => { ac.reload(); cfg.reload(); }} />;
  }
  if (!ac.data || !cfg.data) return <Loading />;

  const wbN = ac.data.rows.filter((a) => a.balance_supported && !a.anon).length;
  const platforms = cfg.data.platforms ?? [];
  const currentCfg = cfg.data.cfg;

  const saveCfg = async () => {
    if (!/^\d{1,2}:\d{2}$/.test(time)) { setMsg('✗ 时刻必须是 HH:MM'); return; }
    setBusy(true);
    try {
      const r = await api.post<CheckinConfigResponse>('/api/checkin', {
        action: 'config', set_time: time, set_platforms: picked,
        set_daily_checkin: currentCfg.daily_checkin,
        set_daily_growth: currentCfg.daily_growth,
      });
      setMsg(`✔ 已保存：每天 ${hhmm(r.cfg.run_hour, r.cfg.run_minute)}（北京时间）自动执行 · ${picked.length} 个平台参与自动签到`);
      cfg.reload();
    } catch (e) {
      setMsg('保存失败：' + (e instanceof Error ? e.message : String(e)));
    } finally { setBusy(false); }
  };

  const toggle = async (kind: 'checkin' | 'growth', on: boolean) => {
    setBusy(true);
    try {
      const action = kind === 'checkin' ? 'schedule' : 'growth_schedule';
      await api.post('/api/checkin', { action, enabled: on });
      setMsg(`✔ 已${on ? '开启' : '关闭'}${kind === 'checkin' ? '每日自动签到' : '每日自动 WB 任务'}`);
      ac.reload();
      cfg.reload();
    } catch (e) { setMsg('设置失败：' + (e instanceof Error ? e.message : String(e))); }
    finally { setBusy(false); }
  };

  const runAction = async (payload: Record<string, unknown>, label: string) => {
    if (!confirm(`确认${label}？这是对上游的主动写操作。`)) return;
    setBusy(true);
    try {
      const r = await api.post('/api/checkin', payload) as {
        ok?: boolean; message?: string;
        /** claim 响应：逐账号结果（判据与后端 summarize_checkin 同源）。 */
        results?: Array<{ ok?: boolean; data?: { ok?: boolean; granted?: boolean; claimed?: boolean; code?: number; message?: string; status?: string; msg?: string } }>;
        accounts?: Array<{ result?: { ok?: boolean; message?: string } }>;
      };
      // ★ 「已执行」三个字不够：用户要的是「成几个、几个是今天已领过、
      //   失败几个失败在哪」。判据对齐 task_records::from_checkin：
      //   code=2/10001 与「已签/已领」文案是幂等跳过，不算成功也不算失败。
      const summarize = (): string => {
        const rows = r.results ?? [];
        if (!rows.length) {
          const g = r.accounts?.length
            ? r.accounts.map((a) => a.result?.message ?? '').filter(Boolean).join('；')
            : '';
          return g;
        }
        let okN = 0, alreadyN = 0, failN = 0, firstErr = '';
        for (const row of rows) {
          const d = row.data ?? {};
          const text = [d.message, d.status, d.msg].find((x) => typeof x === 'string') ?? '';
          const low = text.toLowerCase();
          const already = d.code === 2 || d.code === 10001
            || (row.ok && d.claimed === false)
            || (row.ok && d.granted === false)
            || low.includes('今日已') || low.includes('已领取') || low.includes('already');
          if (already) alreadyN++;
          else if (row.ok && (d.ok !== false)) okN++;
          else {
            failN++;
            if (!firstErr) firstErr = text.slice(0, 60);
          }
        }
        const parts = [`成功 ${okN}`];
        if (alreadyN) parts.push(`今日已领 ${alreadyN}`);
        if (failN) parts.push(`失败 ${failN}`);
        return parts.join(' · ') + (firstErr ? `；首个失败：${firstErr}` : '');
      };
      const detail = r.message ?? summarize();
      setMsg(r?.ok ? `✔ ${label}：${detail || '完成'}` : `✗ ${r?.message ?? '执行失败'}`);
    } catch (e) { setMsg('执行失败：' + (e instanceof Error ? e.message : String(e))); }
    finally { setBusy(false); }
  };

  const healthCheck = async () => {
    setBusy(true);
    try {
      await api.post('/api/accounts', { action: 'verify' });
      setMsg('✔ 健康检查已执行');
      ac.reload();
    } catch (e) { setMsg('执行失败：' + (e instanceof Error ? e.message : String(e))); }
    finally { setBusy(false); }
  };

  return (
    <>
      <Card
        title="WB 任务"
        extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
          WorkBuddy 账号运维 · {wbN} 个可操作账号
        </span>}
        className="wb-task-panel"
      >
        <div className="wb-tabs">
          {TABS.map(([k, label]) => (
            <button key={k} className={'wb-tab' + (tab === k ? ' on' : '')} onClick={() => setTab(k)}>
              {label}
            </button>
          ))}
          <span className="ml-auto">
            <Pill tone={currentCfg.daily_checkin ? 'ok' : 'mut'}>
              {currentCfg.daily_checkin ? '自动签到已开启' : '自动签到已关闭'}
            </Pill>
          </span>
        </div>

        {tab === 'center' && (
          <div className="wb-stats">
            <div className="wb-stat" role="button" tabIndex={0} onClick={() => setTab('schedule')}
              onKeyDown={(e) => { if (e.key === 'Enter') setTab('schedule'); }}>
              <div className="wb-stat-label">下次自动执行</div>
              <div className="wb-stat-value">
                {currentCfg.daily_checkin && currentCfg.next_run
                  ? <>{new Date(currentCfg.next_run * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })}</>
                  : <span className="faint">未开启</span>}
              </div>
              <div className="wb-stat-foot faint fs-xs">
                {currentCfg.daily_checkin
                  ? `每天 ${hhmm(currentCfg.run_hour, currentCfg.run_minute)}（北京时间）· 点此调整`
                  : '开启后按设定时刻自动执行 · 点此设置'}
              </div>
            </div>
            <div className="wb-stat" role="button" tabIndex={0} onClick={() => setTab('records')}
              onKeyDown={(e) => { if (e.key === 'Enter') setTab('records'); }}>
              <div className="wb-stat-label">可操作账号</div>
              <div className="wb-stat-value">{wbN}</div>
              <div className="wb-stat-foot faint fs-xs">签到 / 任务 / 健康检查都作用在这些账号</div>
            </div>
            <div className="wb-stat" role="button" tabIndex={0} onClick={() => setTab('records')}
              onKeyDown={(e) => { if (e.key === 'Enter') setTab('records'); }}>
              <div className="wb-stat-label">签到记录</div>
              <div className="wb-stat-value">查看</div>
              <div className="wb-stat-foot faint fs-xs">逐账号执行台账 · 可筛选时间与结果</div>
            </div>
          </div>
        )}

        {tab === 'center' && (
          <div className="wb-tab-body">
            {wbN === 0 ? (
              <div className="faint">还没有 WorkBuddy 系账号 —— 到「接入源」页接入一个 wb-gateway / 国际版 / CodeBuddy 账号后，这里会出现签到与任务入口。</div>
            ) : (
              <div className="wb-center-grid">
                <div className="wb-task-col">
                  <div className="wb-task-h">日常任务 <span className="faint fs-xs">可一键执行，也可设成每日自动</span></div>
                  <div className="wb-task-row">
                    <div className="wb-task-info"><b>每日签到</b><span className="faint fs-xs">全部可签到平台，当天已领过会幂等跳过</span></div>
                    <div className="acts">
                      <button className="btn sm pri" disabled={busy} onClick={() => runAction({ action: 'claim' }, '一键签到')}>立即签到</button>
                      <button className={'btn sm ' + (currentCfg.daily_checkin ? 'pri' : 'gh')} disabled={busy}
                        onClick={() => toggle('checkin', !currentCfg.daily_checkin)}>
                        自动：{currentCfg.daily_checkin ? '开 ✔' : '关'}
                      </button>
                    </div>
                  </div>
                  <div className="wb-task-row">
                    <div className="wb-task-info"><b>WB 日常任务</b><span className="faint fs-xs">17 项闭环：事件链上报 + 领奖，全新账号约 +1950 积分</span></div>
                    <div className="acts">
                      <button className="btn sm pri" disabled={busy} onClick={() => runAction({ action: 'growth', platform: 'wb-gateway' }, 'WB 任务')}>立即执行</button>
                      <button className={'btn sm ' + (currentCfg.daily_growth ? 'pri' : 'gh')} disabled={busy}
                        onClick={() => toggle('growth', !currentCfg.daily_growth)}>
                        自动：{currentCfg.daily_growth ? '开 ✔' : '关'}
                      </button>
                    </div>
                  </div>
                  <div className="wb-task-row">
                    <div className="wb-task-info"><b>一键健康检查</b><span className="faint fs-xs">逐账号验活：积分 / 模型 / 对话</span></div>
                    <div className="acts"><button className="btn sm" disabled={busy} onClick={healthCheck}>开始检查</button></div>
                  </div>
                </div>

                <div className="wb-task-col">
                  <div className="wb-task-h">活动 · 单次操作 <span className="faint fs-xs">按需点，不做自动化</span></div>
                  <div className="wb-task-acts">
                    {ACTIVITIES.map(([act, label, hint]) => (
                      <button key={act} className="btn sm" title={hint} disabled={busy}
                        onClick={() => runAction({ action: 'growth', platform: 'wb-gateway', growth_action: act }, label)}>
                        {label}
                      </button>
                    ))}
                  </div>
                  <Note>成长概览一次查询等级、能量、活跃地图；盲盒配额只查可开次数，「开盲盒」按次数最多连开 5 个。</Note>
                </div>
              </div>
            )}
          </div>
        )}

        {tab === 'schedule' && (
          <ScheduleTab
            platforms={platforms}
            time={time}
            setTime={setTime}
            picked={picked}
            setPicked={setPicked}
            dailyCheckin={currentCfg.daily_checkin}
            dailyGrowth={currentCfg.daily_growth}
            busy={busy}
            onSave={saveCfg}
            onToggle={toggle}
            onRun={() => runAction({ action: 'claim' }, '立即签到（当前勾选范围）')}
          />
        )}

        {tab === 'records' && <RecordsTab />}

        {msg && <div className="note mt-2">{msg}</div>}
      </Card>

      <Card title="账号凭据">
        {ac.data.rows.length === 0 ? (
          <Empty>还没有账号：到「接入源」页挑一个平台接入</Empty>
        ) : (
          <div className="table-scroll">
            <table>
              <thead>
                <tr><th>账号名</th><th>平台</th><th>凭据</th><th>状态</th><th>健康</th><th>积分</th></tr>
              </thead>
              <tbody>
                {ac.data.rows.map((a) => (
                  <tr key={a.id}>
                    <td><b>{a.name || a.id}</b><br /><span className="faint mono">{a.id}</span></td>
                    <td>{a.platform_name || a.platform}<br /><span className="faint mono fs-xs">{a.platform}</span></td>
                    <td className="faint">
                      {a.anon ? '无需凭据' : a.needs_relogin ? '本机解不开' : `${a.secret_len} 字符（掩码）`}
                    </td>
                    <td><Pill tone={a.enabled ? 'ok' : 'mut'}>{a.enabled ? '启用' : '停用'}</Pill></td>
                    <td>
                      {a.health?.label ? <span className={'pill ' + (a.health.status === 'ok' ? 'ok' : 'warn')}>{a.health.label}</span> : <span className="faint">未验活</span>}
                    </td>
                    <td className="mono">{a.credit != null ? fmt(a.credit) : <span className="faint">未查</span>}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {ac.data.rows.some((a) => a.needs_relogin) && (
        <Note tone="warn">有账号的凭据在本机解不开（DPAPI 绑定原 Windows 用户）。把 accounts.json 搬回原机器，或在当前机器重新登录一次。</Note>
      )}
    </>
  );
}

function ScheduleTab({
  platforms, time, setTime, picked, setPicked, dailyCheckin, dailyGrowth, busy, onSave, onToggle, onRun,
}: {
  platforms: Array<{ id: string; name: string }>;
  time: string; setTime: (s: string) => void;
  picked: string[]; setPicked: (ids: string[]) => void;
  dailyCheckin: boolean; dailyGrowth: boolean; busy: boolean;
  onSave: () => void;
  onToggle: (kind: 'checkin' | 'growth', on: boolean) => void;
  onRun: () => void;
}) {
  return (
    <div className="wb-tab-body">
      <div className="wb-sched-summary">
        <div>
          <b>每天 {time}（北京时间）</b>
          <div className="faint fs-xs">改动下一轮即生效，不用重启；错过时点后等到次日同一时刻。</div>
        </div>
        <Pill tone={dailyCheckin ? 'ok' : 'mut'}>{dailyCheckin ? '定时签到已开启' : '定时签到已关闭'}</Pill>
      </div>

      <div className="wb-sched-row">
        <div className="wb-sched-line">
          <span className="wb-set-k">执行时刻</span>
          <input id="wbHour" className="q mono" type="time" value={time} onChange={(e) => setTime(e.target.value)} />
          <span className="faint fs-xs">北京时间 · 分钟级，每天一次</span>
        </div>
        <div className="wb-sched-line">
          <span className="wb-set-k">作用任务</span>
          <button className={'btn sm ' + (dailyCheckin ? 'pri' : 'gh')} disabled={busy}
            onClick={() => onToggle('checkin', !dailyCheckin)}>
            每日签到：{dailyCheckin ? '开 ✔' : '关'}
          </button>
          <button className={'btn sm ' + (dailyGrowth ? 'pri' : 'gh')} disabled={busy}
            onClick={() => onToggle('growth', !dailyGrowth)}>
            WB 日常任务：{dailyGrowth ? '开 ✔' : '关'}
          </button>
        </div>
        <div className="wb-sched-line">
          <span className="wb-set-k">立即执行</span>
          <button className="btn sm" disabled={busy} onClick={onRun}>按当前配置签一次</button>
          <span className="faint fs-xs">手动执行会落进「签到记录」</span>
        </div>
      </div>

      <div className="wb-sched-row">
        <div className="wb-set-head">
          <b>自动一键签到的平台</b>
          <span className="faint fs-xs">已勾 {picked.length} / {platforms.length} 家</span>
          <button className="btn sm gh" disabled={busy} onClick={() => setPicked(platforms.map((p) => p.id))}>全选</button>
          <button className="btn sm gh" disabled={busy} onClick={() => setPicked([])}>清空</button>
        </div>
        <div className="wb-range">
          {platforms.map((p) => (
            <label className="wb-plat" key={p.id} title={p.id}>
              <input type="checkbox" className="w-auto" checked={picked.includes(p.id)}
                onChange={(e) => setPicked(e.target.checked ? [...picked, p.id] : picked.filter((x) => x !== p.id))} />
              <span>{p.name}</span>
            </label>
          ))}
        </div>
        {picked.length === 0 && <Note tone="warn">没勾任何平台：定时签到只会执行 WB 日常任务，不会打签到接口。</Note>}
        <div className="row mt-2">
          <button className="btn pri" disabled={busy} onClick={onSave}>保存</button>
          <span className="faint fs-xs">时刻与勾选范围一起落盘，重启后保持。</span>
        </div>
      </div>
    </div>
  );
}

function RecordsTab() {
  const [range, setRange] = useState('all');
  const [outcome, setOutcome] = useState('all');
  const [q, setQ] = useState('');
  const [page, setPage] = useState(0);
  const [draft, setDraft] = useState('');
  const pageSize = 20;

  useEffect(() => {
    const t = setTimeout(() => { setQ(draft); setPage(0); }, 300);
    return () => clearTimeout(t);
  }, [draft]);

  const query = useMemo(() => ({
    range, outcome, q, limit: pageSize, offset: page * pageSize,
  }), [range, outcome, q, page]);

  const rec = useAsync<TaskRecordsResponse>(
    () => api.get('/api/task-records?' + new URLSearchParams(
      Object.entries(query).map(([k, v]) => [k, String(v)]),
    ).toString()),
    [range, outcome, q, page],
  );

  const rows = rec.data?.rows ?? [];
  const total = rec.data?.total ?? 0;
  const pages = Math.max(1, Math.ceil(total / pageSize));

  return (
    <div className="wb-tab-body">
      <div className="wb-rec-filters">
        <select className="q w-auto" value={range} onChange={(e) => { setRange(e.target.value); setPage(0); }}>
          <option value="all">全部时间</option>
          <option value="today">今天</option>
          <option value="7">7 天</option>
          <option value="30">30 天</option>
        </select>
        <select className="q w-auto" value={outcome} onChange={(e) => { setOutcome(e.target.value); setPage(0); }}>
          <option value="all">全部结果</option>
          <option value="success">成功领取</option>
          <option value="already">今日已领</option>
          <option value="failed">失败</option>
        </select>
        <input className="q" placeholder="搜索账号 / 平台 / 说明…" value={draft} onChange={(e) => setDraft(e.target.value)} />
        <button className="btn sm gh" onClick={rec.reload}>刷新</button>
        <span className="faint fs-xs ml-auto">共 {total} 条</span>
      </div>

      {rec.error ? <ErrorBox message={rec.error} onRetry={rec.reload} /> : rec.loading && !rec.data ? <Loading /> : (
        <div className="table-scroll">
          <table className="wb-record-table">
            <thead>
              <tr><th>时间</th><th>任务</th><th>来源</th><th>账号 / 平台</th><th>结果</th><th>说明</th></tr>
            </thead>
            <tbody>
              {rows.length === 0 && <tr><td colSpan={6}><Empty>没有符合条件的执行记录</Empty></td></tr>}
              {rows.map((r: TaskRecord, i: number) => (
                <tr className="wb-record-row" key={`${r.ts}-${r.account}-${i}`}>
                  <td className="mono fs-xs">{timeText(r.ts)}</td>
                  <td>{r.task}</td>
                  <td>{SOURCE_LABEL[r.source] ?? r.source}</td>
                  <td><b>{r.account || '—'}</b><br /><span className="faint mono fs-xs">{r.platform || '—'}</span></td>
                  <td><span className={'pill ' + (r.outcome === 'success' ? 'ok' : r.outcome === 'already' ? 'brand' : 'err')}>
                    {OUTCOME_LABEL[r.outcome] ?? r.outcome}
                  </span></td>
                  <td className="faint">{r.detail || '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div className="wb-rec-foot">
        <span className="faint fs-xs">第 {page + 1} / {pages} 页 · 每页 {pageSize} 条</span>
        <button className="btn sm gh" disabled={page === 0} onClick={() => setPage((p) => Math.max(0, p - 1))}>上一页</button>
        <button className="btn sm gh" disabled={page + 1 >= pages} onClick={() => setPage((p) => p + 1)}>下一页</button>
      </div>
    </div>
  );
}
