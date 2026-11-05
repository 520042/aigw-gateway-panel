import { useEffect, useState } from 'react';
import { Card, Loading, ErrorBox, useAsync } from '../components';
import { api } from '../api';

interface LogLine { ts: string; level: string; module: string; msg: string }

function parseLine(raw: string): LogLine {
  const m = raw.match(/^(\S+)\s+(\w+)\s+(\S+?):\s?([\s\S]*)$/);
  if (!m) return { ts: '', level: '', module: '', msg: raw };
  return { ts: m[1], level: m[2].toUpperCase(), module: m[3], msg: m[4] };
}

export function Logs() {
  const [lines, setLines] = useState(300);
  const [level, setLevel] = useState('');
  // 关键词走 300ms 防抖：搜索框每敲一个字符都打一次 /api/logs 的话，
  // 一次过滤请求要后端多读 6 倍日志窗口，连打十个字就是十次白跑。
  const [draft, setDraft] = useState('');
  const [q, setQ] = useState('');
  useEffect(() => {
    const t = setTimeout(() => setQ(draft.trim()), 300);
    return () => clearTimeout(t);
  }, [draft]);
  const q2 = new URLSearchParams({ lines: String(lines), ...(level ? { lv: level } : {}), ...(q ? { g: q } : {}) });
  const { data, error, loading, reload } = useAsync<{ lines: string[]; file_size: number }>(
    () => api.get('/api/logs?' + q2.toString()), [lines, level, q],
  );
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <Card
      title={<span>运行日志 <span className="faint small" style={{ fontWeight: 400 }}>{data?.lines.length ?? 0} 行</span></span>}
      extra={
        <div className="row" style={{ margin: 0, marginLeft: 'auto', gap: 8 }}>
          <select className="q" style={{ width: 'auto' }} value={lines} onChange={(e) => setLines(Number(e.target.value))}>
            {[100, 300, 600, 1000].map((n) => <option key={n} value={n}>{n} 行</option>)}
          </select>
          <select className="q" style={{ width: 'auto' }} value={level} onChange={(e) => setLevel(e.target.value)}>
            <option value="">全部级别</option>
            <option value="error">ERROR</option>
            <option value="warn">WARN</option>
            <option value="info">INFO</option>
          </select>
          <input className="q" style={{ width: 180 }} placeholder="关键词过滤…" value={draft} onChange={(e) => setDraft(e.target.value)} />
          <button className="btn sm" onClick={reload}>刷新</button>
        </div>
      }
    >
      {loading ? <Loading /> : (
        <div className="log-box">
          {(data?.lines ?? []).map((raw, i) => {
            const l = parseLine(raw);
            const cls = l.level === 'ERROR' ? 'err' : l.level === 'WARN' ? 'warn' : l.level === 'INFO' ? 'info' : 'dbg';
            return (
              <div className={'log-row log-' + cls} key={i}>
                <span className="log-ts">{l.ts}</span>
                <span className="log-lv">{l.level || '—'}</span>
                <span className="log-mod">{l.module}</span>
                <span className="log-msg">{l.msg}</span>
              </div>
            );
          })}
          {data && data.lines.length === 0 && <div className="faint">没有匹配的日志行</div>}
        </div>
      )}
      {data && (
        <div className="faint small mt-2">
          日志文件约 {Math.round((data.file_size ?? 0) / 1024)} KiB（尾部 256 KiB 窗口内查找）
        </div>
      )}
    </Card>
  );
}
