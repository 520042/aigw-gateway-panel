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

/** 异步数据状态的一处收敛：所有页面用同一个 hook，不再各写一遍 loading/error。 */
export function useAsync<T>(fn: () => Promise<T>, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    fn()
      .then((d) => { if (alive) { setData(d); setError(null); } })
      .catch((e: unknown) => { if (alive) setError(e instanceof Error ? e.message : String(e)); })
      .finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
    // fn 每次渲染都是新闭包，依赖只认显式 deps + tick（与旧版 render() 语义一致）。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);

  return { data, error, loading, reload: () => setTick((n) => n + 1) };
}
