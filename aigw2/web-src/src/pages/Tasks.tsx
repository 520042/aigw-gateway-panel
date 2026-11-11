import { useEffect, useMemo, useState } from 'react';
import { Card, Note, Loading, ErrorBox, Empty, Pill, useAsync } from '../components';
import {
  api,
  type AccountsResponse,
  type CheckinConfigResponse,
  type TaskRecordsResponse,
  type TaskRecord,
} from '../api';

/* WB 日常任务的完整结果（对齐 workbuddy2api growth_plan / Agent2API 结果面板）：
   每账号一组汇总 + 步骤链 + 18 项任务各自的完成状态。 */
interface GrowthStep {
  step?: string; ok?: boolean; message?: string; action?: string;
  tasks?: GrowthTask[]; todo?: Array<{ code: string; name: string; current?: number; target?: number }>;
  errors?: Array<{ code: string; name: string; error: string }>;
  [k: string]: unknown;
}
interface GrowthTask { code: string; name: string; status: string; credit?: number | null }
interface GrowthAccount {
  account: string;
  result: {
    ok?: boolean; message?: string; summary?: string;
    total_tasks?: number; pending?: number; lit_groups?: number;
    claimed?: number; credit?: number; energy?: number;
    manual_tasks?: Array<{ code: string; name: string }>;
    steps?: GrowthStep[];
  };
}
interface GrowthResp { ok?: boolean; message?: string; accounts?: GrowthAccount[] }

const STEP_LABEL: Record<string, string> = {
  list: '读取任务清单', accept: '接受任务', light_events: '事件链点亮', claim: '逐项领奖',
};
const TASK_STATUS: Record<string, string> = {
  claimed: '已领取', completed: '已完成', pending: '待完成',
};

function GrowthResult({ resp, onClose }: { resp: GrowthResp; onClose: () => void }) {
  const accounts = resp.accounts ?? [];
  const total = accounts.reduce((n, a) => n + (a.result.total_tasks ?? 0), 0);
  const done = accounts.reduce((n, a) => n + (a.result.claimed ?? 0), 0);
  const credit = accounts.reduce((n, a) => n + (a.result.credit ?? 0), 0);
  const failed = accounts.filter((a) => a.result.ok === false).length;
  return (
    <div className="growth-result">
      <div className="growth-result-hd">
        <b>WB 日常任务 · 执行明细</b>
        <button className="btn sm gh" onClick={onClose}>关闭</button>
      </div>
      <div className="growth-summary">
        <span>账号 <b>{accounts.length}</b></span>
        <span>每账号任务 <b>{accounts.length ? Math.round(total / accounts.length) : 0}</b> 项</span>
        <span>本次领取 <b>{done}</b> 项</span>
        <span>获得 <b>{credit}</b> 积分</span>
        {failed > 0 && <span className="c-err">失败 {failed} 个账号</span>}
      </div>
      {accounts.map((a) => {
        const r = a.result;
        const listStep = (r.steps ?? []).find((s) => s.action === 'list' || Array.isArray(s.tasks));
        const claimStep = (r.steps ?? []).find((s) => s.action === 'claim');
        const tasks = listStep?.tasks ?? [];
        const todo = claimStep?.todo ?? [];
        const errors = claimStep?.errors ?? [];
        return (
          <details className="growth-acc" key={a.account} open={accounts.length === 1}>
            <summary>
              <span className={r.ok === false ? 'c-err' : 'c-ok'}>{r.ok === false ? '✗' : '✓'}</span>
              <b>{a.account}</b>
              <span className="faint fs-xs">{r.summary ?? r.message ?? '执行完成'}</span>
            </summary>
            <div className="growth-steps">
              {(r.steps ?? []).filter((s) => s.action !== 'list').map((s, i) => (
                <div className="growth-step" key={i}>
                  <span className={s.ok ? 'c-ok' : 'c-warn'}>{s.ok ? '✓' : '·'}</span>
                  <b>{s.step ?? STEP_LABEL[s.action ?? ''] ?? s.action ?? `步骤 ${i + 1}`}</b>
                  <span className="faint fs-xs">{s.message ?? ''}</span>
                </div>
              ))}
            </div>
            {tasks.length > 0 && (
              <div className="growth-tasks">
                <div className="faint fs-xs mb-2">任务清单（{tasks.length} 项）</div>
                <div className="growth-task-grid">
                  {tasks.map((t) => (
                    <div className="growth-task" key={t.code || t.name} title={t.code}>
                      <span className={'pill ' + (t.status === 'pending' ? 'warn' : 'ok')}>
                        {TASK_STATUS[t.status] ?? t.status}
                      </span>
                      <span>{t.name}</span>
                      {t.credit != null && <span className="faint fs-xs">+{t.credit}</span>}
                    </div>
                  ))}
                </div>
              </div>
            )}
            {/* 参照 WorkBuddy-Daily 的「待办分布 + 失败原因」：用户不用翻日志 */}
            {(todo.length > 0 || errors.length > 0 || (r.manual_tasks ?? []).length > 0) && (
              <div className="growth-tasks">
                {(r.manual_tasks ?? []).length > 0 && <>
                  <div className="faint fs-xs mb-2">
                    需人工完成（{(r.manual_tasks ?? []).length} 项，脚本做不了）
                  </div>
                  <div className="growth-task-grid">
                    {(r.manual_tasks ?? []).map((t) => (
                      <div className="growth-task" key={t.code || t.name}>
                        <span className="pill mut">需手动</span>
                        <span>{t.name}</span>
                      </div>
                    ))}
                  </div>
                </>}
                {todo.length > 0 && <>
                  <div className="faint fs-xs mb-2">待完成（{todo.length} 项，进度未达标）</div>
                  <div className="growth-task-grid">
                    {todo.map((t) => (
                      <div className="growth-task" key={t.code || t.name}>
                        <span className="pill warn">待完成</span>
                        <span>{t.name}</span>
                        {t.current != null && t.target != null &&
                          <span className="faint fs-xs">{t.current}/{t.target}</span>}
                      </div>
                    ))}
                  </div>
                </>}
                {errors.length > 0 && <div className="note errnote mt-2">
                  {errors.map((x) => `${x.name}：${x.error}`).join('；')}
                </div>}
              </div>
            )}
          </details>
        );
      })}
    </div>
  );
}

/** 单次动作的上游响应 → 一行人话（每个动作关心的字段不同）。 */
function singleActionText(act: string, res: Record<string, unknown>): string {
  const ok = res.ok !== false;
  const data = ((res.data ?? {}) as Record<string, unknown>);
  const inner = ((data.data ?? data) as Record<string, unknown>);
  const num = (v: unknown) => (typeof v === 'number' ? v : undefined);
  if (!ok) {
    const msg = (res.message ?? data.msg ?? '上游失败') as string;
    return `✗ ${String(msg).slice(0, 60)}`;
  }
  switch (act) {
    case 'growth_profile': {
      const lv = num(inner.level), done = num(inner.completed), total = num(inner.total);
      return `成长等级 Lv.${lv ?? '—'} · 任务 ${done ?? '—'}/${total ?? '—'}${inner.max_level ? '（已满级）' : ''}`;
    }
    case 'buddy_quota': {
      const bal = num(inner.balance), cost = num(inner.cost_per_open), aff = num(inner.affordable);
      return `能量 ${bal ?? '—'} · 可开 ${aff ?? 0} 次（每次 ${cost ?? '—'} 能量）`;
    }
    case 'buddy_open': {
      const n = Array.isArray(inner.results) ? inner.results.length : num(inner.opened);
      return `开盲盒 ${n ?? 0} 个`;
    }
    case 'streak_redeem': {
      const got = Array.isArray(res.redeemed) ? res.redeemed.length : 0;
      const tiers = Array.isArray(res.redeemed)
        ? (res.redeemed as Array<Record<string, unknown>>).filter((x) => x.ok !== false).length : 0;
      return got ? `兑换 ${tiers}/${got} 档成功` : '无可兑换档位';
    }
    case 'lottery_draw': {
      const p = inner.prize ?? inner.reward ?? inner.name;
      return p ? `抽到 ${String(p)}` : '抽奖已处理（次数为 0 或已抽过）';
    }
    case 'travel_depart': {
      if (res.empty) return '今天已经出发过了（上游每日一次限制）';
      const loc = (inner.location ?? {}) as Record<string, unknown>;
      if (loc.name) return `猫猫已出发前往 ${String(loc.name)}`;
      const arrive = inner.arrive_at ?? inner.arrive_time;
      return arrive ? `猫猫已出发，预计 ${String(arrive)} 到站` : '已出发旅行';
    }
    case 'travel_claim': {
      if (res.empty) return '没有可领的旅行奖励（猫还在路上或已领过）';
      const rw = inner.reward ?? inner.credit ?? inner.gift;
      return rw ? `领到 ${String(rw)}` : '旅行奖励已领取';
    }
    default:
      return '完成';
  }
}

const pad = (n: number) => String(n).padStart(2, '0');
const hhmm = (h: number, m: number) => `${pad(h)}:${pad(m)}`;

type Tab = 'center' | 'schedule' | 'records' | 'accounts';
const TABS: Array<[Tab, string]> = [
  ['center', '签到中心'],
  ['schedule', '定时签到'],
  ['records', '签到记录'],
  ['accounts', '账号'],
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

/**
 * 任务中心（独立视图，2026-11-06 从「模型与用量」拆出）。
 *
 * 为什么拆：这卡片覆盖 5 家平台的签到（Lobster / 小浣熊不是 WB 系），
 * 挂在「模型与用量」下名不副实；用户也指出「标题叫 WB 任务但里面不只
 * WB」——卡片改名「签到与任务」，视图独立成「任务中心」进侧栏。
 */
export function Tasks() {
  const ac = useAsync<AccountsResponse>(() => api.get('/api/accounts'), []);
  const cfg = useAsync<CheckinConfigResponse>(
    () => api.post('/api/checkin', { action: 'config' }), [],
  );
  const [tab, setTab] = useState<Tab>('center');
  const [time, setTime] = useState('09:00');
  const [picked, setPicked] = useState<string[]>([]);
  const [msg, setMsg] = useState('');
  const [growthResult, setGrowthResult] = useState<GrowthResp | null>(null);
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
    // ★ growth「一键全做」走完整结果面板，不再压缩成一句话：
    //   后端返回逐账号 + 逐步骤 + 18 项任务清单（对齐 workbuddy2api growth_plan）。
    const isGrowth = payload.action === 'growth' && payload.growth_action === undefined;
    if (isGrowth) { setGrowthResult(null); setMsg('正在执行 WB 日常任务（事件链 + 逐项领奖）…'); }
    try {
      const r = await api.post('/api/checkin', payload) as {
        ok?: boolean; message?: string;
        /** claim 响应：逐账号结果（判据与后端 summarize_checkin 同源）。 */
        results?: Array<{ ok?: boolean; data?: { ok?: boolean; granted?: boolean; claimed?: boolean; code?: number; message?: string; status?: string; msg?: string } }>;
        accounts?: Array<{ result?: { ok?: boolean; message?: string } }>;
      };
      // ★ 「已执行」三个字不够：用户要的是「成几个、几个是今天已领过、
      //   失败几个失败在哪」。判据对齐 task_records::from_checkin。
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
      if (isGrowth) {
        const g = r as GrowthResp;
        setGrowthResult(g);
        const accs = g.accounts ?? [];
        const done = accs.reduce((n, a) => n + (a.result.claimed ?? 0), 0);
        const perAccount = accs.length ? (accs[0].result.total_tasks ?? 0) : 0;
        setMsg(accs.length
          ? `✔ ${label}完成：${accs.length} 个账号 / 每账号 ${perAccount} 项任务 / 本次领取 ${done} 项（明细见下方）`
          : `✗ ${r.message ?? '没有可执行的 WB 账号'}`);
      } else if (payload.growth_action !== undefined) {
        // ★ 单次动作（成长概览 / 盲盒配额 / 抽奖 / 旅行…）：每个都有可读数据，
        //   不能只回一句「已执行」。按动作把关键字段翻成人话（对齐 MiraiHub
        //   growth_center 的统计卡与 state 映射）。
        const act = String(payload.growth_action);
        const rows = (r.accounts ?? []) as Array<{ account?: string; result?: Record<string, unknown> }>;
        setMsg(`✔ ${label}：${rows.map((a) => `${(a.account ?? '').slice(0, 10)} ${singleActionText(act, a.result ?? {})}`).join('；') || '完成'}`);
      } else {
        const detail = r.message ?? summarize();
        setMsg(r?.ok ? `✔ ${label}：${detail || '完成'}` : `✗ ${r?.message ?? '执行失败'}`);
      }
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
    <Card
      title="签到与任务"
      extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>
        {wbN} 个 WB 系可操作账号 · 覆盖 {platforms.length} 家平台签到
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
        <>
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
              <div className="wb-stat-foot faint fs-xs">WB 系账号的任务与检查</div>
            </div>
            <div className="wb-stat" role="button" tabIndex={0} onClick={() => setTab('records')}
              onKeyDown={(e) => { if (e.key === 'Enter') setTab('records'); }}>
              <div className="wb-stat-label">签到记录</div>
              <div className="wb-stat-value">查看</div>
              <div className="wb-stat-foot faint fs-xs">逐账号执行台账 · 可筛选时间与结果</div>
            </div>
          </div>

          <div className="wb-center-grid">
            <div className="wb-task-col">
              <div className="wb-task-h">日常任务 <span className="faint fs-xs">可一键执行，也可设成每日自动</span></div>
              <div className="wb-task-row">
                <div className="wb-task-info"><b>每日签到</b><span className="faint fs-xs">覆盖全部勾选平台（WB / CodeBuddy / Lobster / 小浣熊）</span></div>
                <div className="acts">
                  <button className="btn sm pri" disabled={busy} onClick={() => runAction({ action: 'claim' }, '一键签到')}>立即签到</button>
                  <button className={'btn sm ' + (currentCfg.daily_checkin ? 'pri' : 'gh')} disabled={busy}
                    onClick={() => toggle('checkin', !currentCfg.daily_checkin)}>
                    自动：{currentCfg.daily_checkin ? '开 ✔' : '关'}
                  </button>
                </div>
              </div>
              <div className="wb-task-row">
                <div className="wb-task-info"><b>WB 日常任务</b><span className="faint fs-xs">仅 WB 系账号 · 逐项接受 + 事件链点亮 + 逐项领奖</span></div>
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
              <div className="wb-task-h">WB 活动 · 单次操作 <span className="faint fs-xs">仅 WB 系 · 按需点</span></div>
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
        </>
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
          nextRun={currentCfg.next_run}
          busy={busy}
          onSave={saveCfg}
          onToggle={toggle}
          onRun={() => runAction({ action: 'claim' }, '立即签到（当前勾选范围）')}
        />
      )}

      {tab === 'records' && <RecordsTab />}

      {tab === 'accounts' && <AccountsTab ac={ac} />}

      {msg && <div className="note mt-2">{msg}</div>}
      {growthResult && <GrowthResult resp={growthResult} onClose={() => setGrowthResult(null)} />}
    </Card>
  );
}

/** 账号页签：全部平台的凭据表（原「模型与用量」页并入于此，2026-11-06）。 */
function AccountsTab({ ac }: { ac: ReturnType<typeof useAsync<AccountsResponse>> }) {
  const fmt = (n: number) => Number(n || 0).toLocaleString('en-US');
  const [acctBusy, setAcctBusy] = useState(false);
  const [acctMsg, setAcctMsg] = useState('');

  /** 账号级写操作（rename / set_limit / delete / balance）。
   *  ★ 后端这四个 action 一直有，界面此前只接了 verify。 */
  const post = async (body: Record<string, unknown>, done: string) => {
    setAcctBusy(true);
    try {
      await api.post('/api/accounts', body);
      setAcctMsg('✔ ' + done);
      ac.reload();
    } catch (e) {
      setAcctMsg('失败：' + (e instanceof Error ? e.message : String(e)));
    } finally { setAcctBusy(false); }
  };

  const doRename = (id: string, old: string) => {
    const name = prompt(`给账号 ${id.slice(0, 10)} 起个新名字（当前：${old}）`, old);
    if (name === null) return;
    const trimmed = name.trim();
    if (!trimmed) return;
    void post({ action: 'rename', id, name: trimmed }, `已改名为「${trimmed}」`);
  };

  const doLimit = (id: string) => {
    const raw = prompt(
      `账号 ${id.slice(0, 10)} 每日请求上限（0 或留空 = 不限）`,
      '',
    );
    if (raw === null) return;
    const n = Number(raw.trim() || 0);
    if (!Number.isFinite(n) || n < 0) { setAcctMsg('上限必须是非负整数'); return; }
    void post({ action: 'set_limit', id, limit: Math.floor(n) },
      n === 0 ? '已取消该账号限额' : `已把上限设为 ${Math.floor(n)} 次/天`);
  };

  const doBalance = (id: string) => {
    void post({ action: 'balance', id }, '已刷新该账号积分');
  };

  const doDelete = (id: string, name: string) => {
    if (!confirm(`删除账号「${name}」？这会让它从号池里消失，且不可撤销。`)) return;
    void post({ action: 'delete', id }, `已删除账号「${name}」`);
  };

  if (ac.error) return <ErrorBox message={ac.error} onRetry={ac.reload} />;
  if (!ac.data) return <Loading />;
  return (
    <div className="wb-tab-body">
      <div className="wb-set-head">
        <b>账号凭据</b>
        <span className="faint fs-xs">{ac.data.rows.length} 个账号 · secret 仅后台使用</span>
        <button className="btn sm gh" style={{ marginLeft: 'auto' }} disabled={ac.loading}
          onClick={async () => {
            try { await api.post('/api/accounts', { action: 'verify' }); ac.reload(); } catch { /* 静默 */ }
          }}>
          逐账号验活
        </button>
      </div>
      {ac.data.rows.length === 0 ? (
        <Empty>还没有账号：到「接入源」页挑一个平台接入</Empty>
      ) : (
        <div className="table-scroll">
          <table>
            <thead>
              <tr><th>账号名</th><th>平台</th><th>凭据</th><th>状态</th><th>健康</th><th>积分</th><th>操作</th></tr>
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
                  <td>
                    {/* ★ 2026-11-10：rename / set_limit / delete / balance
                        四个 action 后端一直有，但界面只做了 verify ——
                        账号改名、设限额、删号都得靠 curl。这里补齐。 */}
                    <div className="acct-acts">
                      <button className="btn sm gh" disabled={acctBusy}
                        onClick={() => doRename(a.id, a.name || a.id)}>改名</button>
                      <button className="btn sm gh" disabled={acctBusy}
                        onClick={() => doLimit(a.id)}>限额</button>
                      <button className="btn sm gh" disabled={acctBusy}
                        onClick={() => doBalance(a.id)}>查积分</button>
                      <button className="btn sm gh danger" disabled={acctBusy}
                        onClick={() => doDelete(a.id, a.name || a.id)}>删除</button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {ac.data.rows.some((a) => a.needs_relogin) && (
        <Note tone="warn">有账号的凭据在本机解不开（DPAPI 绑定原 Windows 用户）。把 accounts.json 搬回原机器，或在当前机器重新登录一次。</Note>
      )}
      {acctMsg && <Note tone={acctMsg.startsWith('✔') ? 'ok' : 'err'}>{acctMsg}</Note>}
    </div>
  );
}

function ScheduleTab({
  platforms, time, setTime, picked, setPicked,
  dailyCheckin, dailyGrowth, nextRun, busy, onSave, onToggle, onRun,
}: {
  platforms: Array<{ id: string; name: string }>;
  time: string; setTime: (s: string) => void;
  picked: string[]; setPicked: (ids: string[]) => void;
  dailyCheckin: boolean; dailyGrowth: boolean; nextRun?: number | null; busy: boolean;
  onSave: () => void;
  onToggle: (kind: 'checkin' | 'growth', on: boolean) => void;
  onRun: () => void;
}) {
  const nextTxt = dailyCheckin && nextRun
    ? new Date(nextRun * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
    : null;
  return (
    <div className="wb-tab-body">
      <div className="wb-sched-summary">
        <div>
          <b>每天 {time}（北京时间）</b>
          <div className="faint fs-xs">改动下一轮即生效，不用重启；错过时点后等到次日同一时刻。</div>
          {nextTxt && <div className="faint fs-xs mt-2">下次自动执行：{nextTxt}</div>}
        </div>
        <Pill tone={dailyCheckin ? 'ok' : 'mut'}>{dailyCheckin ? '定时签到已开启' : '定时签到已关闭'}</Pill>
      </div>
      {/* 勾了平台但两个开关都关着时，勾选不会生效——不提示的话用户以为设置好了。 */}
      {!dailyCheckin && !dailyGrowth && picked.length > 0 && (
        <Note tone="warn">
          已勾选 {picked.length} 个平台，但「定时签到」和「定时 WB 任务」都处于关闭状态，
          到点不会执行。点上面的开关打开后才会按此时刻自动跑。
        </Note>
      )}

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
          <span className="faint fs-xs">签到作用于下面勾选的平台；WB 任务只作用 WB 系账号</span>
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

/** 台账 task 标识 → 人话（growth:xxx 是「WB 任务 · 某动作」）。 */
const TASK_LABEL: Record<string, string> = {
  checkin: '每日签到',
  'growth:all': 'WB 日常任务',
  'growth:tasks_list': 'WB · 任务清单',
  'growth:growth_profile': 'WB · 成长概览',
  'growth:streak_redeem': 'WB · 连登兑换',
  'growth:lottery_draw': 'WB · 限时抽奖',
  'growth:travel_depart': 'WB · 猫猫旅行',
  'growth:travel_claim': 'WB · 领旅行奖励',
  'growth:buddy_quota': 'WB · 盲盒配额',
  'growth:buddy_open': 'WB · 开盲盒',
  'growth:overview': 'WB · 概览',
};
const taskLabel = (t: string) => TASK_LABEL[t] ?? t;

function RecordRow({ r }: { r: TaskRecord }) {
  const steps = r.steps ?? [];
  return (
    <>
      <tr className="wb-record-row">
        <td className="mono fs-xs">{timeText(r.ts)}</td>
        <td title={r.task}>{taskLabel(r.task)}</td>
        <td>{SOURCE_LABEL[r.source] ?? r.source}</td>
        <td><b>{r.account || '—'}</b><br /><span className="faint mono fs-xs">{r.platform || '—'}</span></td>
        <td><span className={'pill ' + (r.outcome === 'success' ? 'ok' : r.outcome === 'already' ? 'brand' : 'err')}>
          {OUTCOME_LABEL[r.outcome] ?? r.outcome}
        </span></td>
        <td className="faint">
          {r.detail || '—'}
          {/* 展开逐动作明细：只对带 steps 的记录显示（新记录才有）。 */}
          {steps.length > 0 && (
            <details className="wb-rec-steps">
              <summary>查看 {steps.length} 项明细</summary>
              {steps.map((s, i) => (
                <div className="wb-rec-step" key={i}>
                  <span className={s.ok ? 'c-ok' : 'c-err'}>{s.ok ? '✓' : '!'}</span>
                  <b>{s.step || '步骤'}</b>
                  <span className="faint fs-xs">{s.message}{s.reward ? ` · +${s.reward} 积分` : ''}</span>
                </div>
              ))}
            </details>
          )}
        </td>
      </tr>
    </>
  );
}

function RecordsTab() {
  const [range, setRange] = useState('all');
  const [outcome, setOutcome] = useState('all');
  const [kind, setKind] = useState('all');
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
    // 后端一直支持 task 筛选，但面板此前没有入口 —— 补上（后端有心跳，前端接）。
    ...(kind === 'all' ? {} : { task: kind }),
  }), [range, outcome, q, page, kind]);

  const rec = useAsync<TaskRecordsResponse>(
    () => api.get('/api/task-records?' + new URLSearchParams(
      Object.entries(query).map(([k, v]) => [k, String(v)]),
    ).toString()),
    [range, outcome, q, page, kind],
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
        <select className="q w-auto" value={kind} onChange={(e) => { setKind(e.target.value); setPage(0); }}>
          <option value="all">全部任务</option>
          <option value="checkin">每日签到</option>
          <option value="growth:all">WB 日常任务</option>
          <option value="growth:growth_profile">WB · 成长概览</option>
          <option value="growth:buddy_quota">WB · 盲盒配额</option>
          <option value="growth:travel_depart">WB · 猫猫旅行</option>
          <option value="growth:travel_claim">WB · 领旅行奖励</option>
          <option value="growth:streak_redeem">WB · 连登兑换</option>
          <option value="growth:lottery_draw">WB · 限时抽奖</option>
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
                <RecordRow key={`${r.ts}-${r.account}-${i}`} r={r} />
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
