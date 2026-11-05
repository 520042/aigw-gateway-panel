import { useEffect, useState, type ReactNode } from 'react';

export function Card({ title, extra, children, className = '' }: {
  title?: ReactNode; extra?: ReactNode; children: ReactNode; className?: string;
}) {
  return (
    <section className={'card ' + className}>
      {title && <div className="card-t">{title}{extra}</div>}
      {children}
    </section>
  );
}

export function Kpi({ label, value, hint, tone }: {
  label: string; value: ReactNode; hint?: ReactNode; tone?: 'ok' | 'err' | 'warn';
}) {
  return (
    <div className="card kpi">
      <div className="lb">{label}</div>
      <div className={'vl' + (tone ? ' v-' + tone : '')}>{value}</div>
      {hint && <div className="ex">{hint}</div>}
    </div>
  );
}

export function Pill({ tone = 'mut', children }: {
  tone?: 'ok' | 'warn' | 'err' | 'mut' | 'brand'; children: ReactNode;
}) {
  return <span className={'pill ' + tone}>{children}</span>;
}

export function Note({ tone, children }: {
  tone?: 'ok' | 'warn' | 'err'; children: ReactNode;
}) {
  return <div className={'note' + (tone ? ' ' + tone + 'note' : '')}>{children}</div>;
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="empty">{children}</div>;
}

export function Loading({ label = '加载中…' }: { label?: string }) {
  return <div className="faint loading">{label}</div>;
}

export function ErrorBox({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <Note tone="err">
      加载失败：{message}{onRetry && <button className="btn sm ml-2" onClick={onRetry}>重试</button>}
    </Note>
  );
}

/**
 * 模块级快取：view 切换 = 卸载重挂，useAsync 每次挂载都会重新 fetch。
 * 对轻接口（<10ms）无所谓；对「要读 6 倍日志窗口」的 /api/logs、要扫台账的
 * /api/usage 来说，来回切两次页就是四趟白跑。挂载时命中未过期快取就先摆旧数据
 * （不闪加载态），后台照常刷新一次（stale-while-revalidate）。
 *
 * ★ 缓存键必须含 **函数体**：多个页面都用 `[]` 做 deps（总览的 usage 与
 *   账号页的 accounts 完全同键），只按 deps 做键会互相串数据 —— 账号页拿到
 *   usage 的形状，`rows` 是 undefined，`.filter` 直接崩（实测过）。
 *   `fn.toString()` 对同端点的内联箭头函数是稳定且互异的，够用。
 *
 * reload()（tick>0）绕过缓存强制重取；写入/轮询后都走它。
 */
const SNAPSHOT_TTL_MS = 15_000;
const snapshotCache = new Map<string, { at: number; data: unknown }>();

export function useAsync<T>(fn: () => Promise<T>, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let alive = true;
    // 缓存键：函数体 + deps。tick 不进键 —— reload 是「强制刷新」，永远直取。
    const baseKey = fn.toString() + '|' + JSON.stringify(deps);
    let seeding = false;
    if (tick === 0) {
      const hit = snapshotCache.get(baseKey);
      if (hit && Date.now() - hit.at < SNAPSHOT_TTL_MS) {
        setData(hit.data as T);
        setError(null);
        setLoading(false);
        seeding = true;
      }
    }
    if (!seeding) setLoading(true);
    fn()
      .then((d) => {
        if (!alive) return;
        snapshotCache.set(baseKey, { at: Date.now(), data: d });
        // 上限兜底：正常页面数 × 端点数远到不了 40，到了就整体清（都是可重取数据）
        if (snapshotCache.size > 40) snapshotCache.clear();
        setData(d);
        setError(null);
      })
      .catch((e: unknown) => { if (alive && !seeding) setError(e instanceof Error ? e.message : String(e)); })
      .finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
    // fn 每次渲染都是新闭包，依赖只认显式 deps + tick（与旧版 render() 语义一致）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);

  return { data, error, loading, reload: () => setTick((n) => n + 1) };
}
