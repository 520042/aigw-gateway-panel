import { useEffect, useState } from 'react';
import type { Health, PlatformResponse } from './api';
import { api } from './api';
import { Overview } from './pages/Overview';
import { Sources } from './pages/Sources';
import { Accounts } from './pages/Accounts';
import { UsagePage } from './pages/UsagePage';
import { Logs } from './pages/Logs';
import { Runtime } from './pages/Runtime';
import { About } from './pages/About';

export type ViewId =
  | 'overview' | 'sources' | 'accounts' | 'usage' | 'logs' | 'runtime' | 'about';

interface NavGroup {
  label: string;
  items: Array<{ id: ViewId; label: string; icon: string }>;
}

/** 侧栏分组：参考项目的 nav-group-label，比原来 7 个平铺按钮更清楚。 */
const GROUPS: NavGroup[] = [
  {
    label: '概览',
    items: [
      { id: 'overview', label: '总览', icon: '◎' },
      { id: 'sources', label: '接入源', icon: '◈' },
    ],
  },
  {
    label: '资源与用量',
    items: [
      { id: 'accounts', label: '模型与用量', icon: '☰' },
      { id: 'usage', label: '用量明细', icon: '◔' },
    ],
  },
  {
    label: '系统',
    items: [
      { id: 'logs', label: '日志', icon: '≡' },
      { id: 'runtime', label: '运行配置', icon: '⚙' },
      { id: 'about', label: '关于', icon: 'ⓘ' },
    ],
  },
];

const TITLES: Record<ViewId, { title: string; sub: string }> = {
  overview: { title: '总览', sub: '免费 AI 额度 → 本地 OpenAI 兼容端点' },
  sources: { title: '接入源', sub: '官方 API · 订阅反代 · 客户端接入 · 公益站 · 自定义' },
  accounts: { title: '模型与用量', sub: '账号运维 · WB 任务 · 积分与限额' },
  usage: { title: '用量明细', sub: 'SQLite(WAL) 台账 · 延迟分位 · 平台归因' },
  logs: { title: '日志', sub: '落盘日志尾部：失败根因的最后一层' },
  runtime: { title: '运行配置', sub: '并发 · 超时 · 重试 · 限流（热更新，无需重启）' },
  about: { title: '关于', sub: '版本 · 备份恢复 · 使用说明' },
};

function viewFromHash(): ViewId {
  const h = location.hash.replace(/^#\/?/, '');
  const valid: ViewId[] = ['overview', 'sources', 'accounts', 'usage', 'logs', 'runtime', 'about'];
  return valid.includes(h as ViewId) ? (h as ViewId) : 'overview';
}

export function App() {
  const [view, setView] = useState<ViewId>(viewFromHash);
  const [health, setHealth] = useState<Health | null>(null);
  // ★ 平台清单提升到框架层：总览与接入源都要 167KB 的 /api/platforms，
  //   每次切视图整页卸载重建的话，两个页面来回切一次就拉两遍。
  //   放这里取一次传下去；子页仍可 reload（来自家机制）。
  const [platforms, setPlatforms] = useState<PlatformResponse | null>(null);
  const [theme, setTheme] = useState<'light' | 'dark' | 'auto'>(() => {
    const t = localStorage.getItem('aigw-theme');
    return t === 'light' || t === 'dark' ? t : 'auto';
  });

  useEffect(() => {
    let alive = true;
    api.get<PlatformResponse>('/api/platforms')
      .then((d) => { if (alive) setPlatforms(d); })
      .catch(() => {});
    return () => { alive = false; };
  }, []);

  useEffect(() => {
    const onHash = () => setView(viewFromHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  useEffect(() => {
    if (theme === 'auto') document.documentElement.removeAttribute('data-theme');
    else document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('aigw-theme', theme);
  }, [theme]);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api.get<Health>('/api/health').then((h) => { if (alive) setHealth(h); }).catch(() => {});
    };
    load();
    const t = setInterval(load, 15_000);
    return () => { alive = false; clearInterval(t); };
  }, []);

  const go = (id: ViewId) => { location.hash = '#/' + id; };
  const cycleTheme = () => setTheme((t) => (t === 'auto' ? 'light' : t === 'light' ? 'dark' : 'auto'));
  const meta = TITLES[view];

  return (
    <div className="wrap">
      <aside className="side">
        <div className="brand"><span className="logo">◈</span><span>aigw v2</span></div>
        <nav className="nav">
          {GROUPS.map((g) => (
            <div className="nav-group" key={g.label}>
              <div className="nav-group-label">{g.label}</div>
              {g.items.map((it) => (
                <button
                  key={it.id}
                  className={'navbtn' + (view === it.id ? ' on' : '')}
                  onClick={() => go(it.id)}
                >
                  <span className="ic">{it.icon}</span>{it.label}
                </button>
              ))}
            </div>
          ))}
        </nav>
        <div className="side-foot">
          BUILD {health?.version ?? '—'} · {health?.milestone ?? '—'}
          <br />{health ? `${health.build.uptime_s ? Math.round(health.build.uptime_s / 60) : 0} 分钟在线` : '连接中…'}
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <div className="topbar-ttl">
            <h1>{meta.title}</h1>
            <span className="sub">{meta.sub}</span>
          </div>
          <div className="topbar-right">
            {health && (
              <span className="status-chip" title={`${health.platforms} 个平台 · ${health.db.events} 条事件`}>
                <i className="dot-live" /> 运行中 · {health.platforms} 平台
              </span>
            )}
            <button className="btn sm" onClick={cycleTheme} title="切换主题：跟随系统 / 亮色 / 暗色">
              {theme === 'auto' ? '◐ 跟随系统' : theme === 'light' ? '☀ 亮色' : '☾ 暗色'}
            </button>
            <button className="btn sm" onClick={() => window.location.reload()}>↻ 刷新</button>
          </div>
        </header>

        <div className="view-wrap">
          {view === 'overview' && <Overview health={health} platforms={platforms} />}
          {view === 'sources' && <Sources platforms={platforms} />}
          {view === 'accounts' && <Accounts />}
          {view === 'usage' && <UsagePage />}
          {view === 'logs' && <Logs />}
          {view === 'runtime' && <Runtime />}
          {view === 'about' && <About />}
        </div>
      </main>
    </div>
  );
}
