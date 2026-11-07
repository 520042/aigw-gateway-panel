import { useEffect, useState } from 'react';
import type { Health, PlatformResponse } from './api';
import { api } from './api';
import { Overview } from './pages/Overview';
import { Sources } from './pages/Sources';
import { Tasks } from './pages/Tasks';
import { UsagePage } from './pages/UsagePage';
import { Logs } from './pages/Logs';
import { Runtime } from './pages/Runtime';
import { About } from './pages/About';

export type ViewId =
  | 'overview' | 'sources' | 'tasks' | 'usage' | 'logs' | 'runtime' | 'about';

interface NavGroup {
  label: string;
  items: Array<{ id: ViewId; label: string; icon: string }>;
}

/** 侧栏分组。2026-11-06 精简：账号表并入任务中心的「账号」页签，
    「模型与用量」视图移除——名不副实且与任务中心重复。 */
const GROUPS: NavGroup[] = [
  {
    label: '概览',
    items: [
      { id: 'overview', label: '总览', icon: '◎' },
      { id: 'sources', label: '接入源', icon: '◈' },
    ],
  },
  {
    label: '用量',
    items: [
      { id: 'usage', label: '用量明细', icon: '◔' },
    ],
  },
  {
    label: '任务',
    items: [
      // 签到覆盖 5 家平台（Lobster / 小浣熊非 WB 系），独立成视图才名副其实。
      { id: 'tasks', label: '任务中心', icon: '✓' },
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
  tasks: { title: '任务中心', sub: '每日签到 · 定时执行 · WB 任务 · 账号凭据' },
  usage: { title: '用量明细', sub: 'SQLite(WAL) 台账 · 延迟分位 · 平台归因' },
  logs: { title: '日志', sub: '落盘日志尾部：失败根因的最后一层' },
  runtime: { title: '运行配置', sub: '并发 · 超时 · 重试 · 限流（热更新，无需重启）' },
  about: { title: '关于', sub: '版本 · 备份恢复 · 使用说明' },
};

function viewFromHash(): ViewId {
  const h = location.hash.replace(/^#\/?/, '');
  // #/accounts 是历史书签：并入任务中心「账号」页签后重定向。
  if (h === 'accounts') return 'tasks';
  const valid: ViewId[] = ['overview', 'sources', 'tasks', 'usage', 'logs', 'runtime', 'about'];
  return valid.includes(h as ViewId) ? (h as ViewId) : 'overview';
}

/**
 * localStorage 防御性读取（借鉴 Agent2API table-shell 的 readPageSize）：
 * · try/catch 全包 —— 隐私模式 / 存储被禁时 getItem 直接 throw，
 *   裸调会把整棵 React 树在 useState 初始化里炸掉（首屏白屏）。
 * · 存量值必须**还在 allowed 列表里**才采纳：合法值集合改动后（比如删掉
 *   一个主题档），旧存档值会让下拉框缺项而界面仍用它 —— 表现为「无法解释」。
 * · 刻意不提供写封装：写失败只影响下次打开，当场仍生效，不值得包一层。
 */
function loadPref(key: string, allowed: readonly string[], fallback: string): string {
  try {
    const saved = localStorage.getItem(key);
    return saved !== null && allowed.includes(saved) ? saved : fallback;
  } catch {
    return fallback;
  }
}

export function App() {
  const [view, setView] = useState<ViewId>(viewFromHash);
  const [health, setHealth] = useState<Health | null>(null);
  // ★ 平台清单提升到框架层：总览与接入源都要 167KB 的 /api/platforms，
  //   每次切视图整页卸载重建的话，两个页面来回切一次就拉两遍。
  //   放这里取一次传下去；子页仍可 reload（来自家机制）。
  const [platforms, setPlatforms] = useState<PlatformResponse | null>(null);
  // ★ 默认亮色（2026-11-06）：参考项目的设计语言是浅色专业控制台；
  //   'auto' 跟随系统在深色模式的 Windows 上会落进暗色，观感不如参考。
  //   index.html 的 FOUC 脚本已同步此默认。
  const [theme, setTheme] = useState<'light' | 'dark' | 'auto'>(() =>
    loadPref('aigw-theme', ['light', 'dark'], 'light') as 'light' | 'dark' | 'auto',
  );

  // ★ 供子页写操作后刷新平台清单（保存凭据 / 导入 / 启停平台都会改变它）。
  //   清单提升到框架层后，子页自己没有数据源，刷新必须走这个句柄。
  const refreshPlatforms = () => {
    api.get<PlatformResponse>('/api/platforms')
      .then((d) => setPlatforms(d))
      .catch(() => {});
  };

  useEffect(() => {
    refreshPlatforms();
  }, []);

  useEffect(() => {
    const onHash = () => setView(viewFromHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  useEffect(() => {
    // 'auto' 也显式落成亮色（跟随系统在深色 Windows 上观感差，见上注）。
    document.documentElement.setAttribute('data-theme', theme === 'auto' ? 'light' : theme);
    try { localStorage.setItem('aigw-theme', theme); } catch { /* 存不了只影响下次打开 */ }
  }, [theme]);

  useEffect(() => {
    let alive = true;
    const load = () => {
      api.get<Health>('/api/health').then((h) => { if (alive) setHealth(h); }).catch(() => {});
    };
    load();
    const t = setInterval(load, 15_000);
    const onVis = () => { if (document.visibilityState === 'visible') load(); };
    document.addEventListener('visibilitychange', onVis);
    return () => { alive = false; clearInterval(t); document.removeEventListener('visibilitychange', onVis); };
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
          {view === 'sources' && <Sources platforms={platforms} refresh={refreshPlatforms} />}
          {view === 'tasks' && <Tasks />}
          {view === 'usage' && <UsagePage />}
          {view === 'logs' && <Logs />}
          {view === 'runtime' && <Runtime />}
          {view === 'about' && <About />}
        </div>
      </main>
    </div>
  );
}
