import { useEffect, useRef, useState } from 'react';
import { Card, Loading, ErrorBox, useAsync } from '../components';
import { api } from '../api';

interface LogLine { ts: string; level: string; module: string; msg: string }

function parseLine(raw: string): LogLine {
  const m = raw.match(/^(\S+)\s+(\w+)\s+(\S+?):\s?([\s\S]*)$/);
  if (!m) return { ts: '', level: '', module: '', msg: raw };
  return { ts: m[1], level: m[2].toUpperCase(), module: m[3], msg: m[4] };
}

const ROW_H = 24;      // 与 .log-row 的固定 height 一致（2px padding + 19px 内容）
const OVERSCAN = 10;   // 视口上下各多渲染 10 行，滚动时不露白边

const pad2 = (n: number) => String(n).padStart(2, '0');

/** tracing 落盘的是 UTC ISO（2026-11-10T03:26:38.938113Z）。
 *  ★ 2026-11-12：原样显示有两个问题——时区差 8 小时（用户对着"03:26"找
 *  不到当时在干什么），且 28 个字符挤掉正文位宽。这里转本地只留时分秒，
 *  跨天时补上 MM-DD。解析失败就回退原串，不吞信息。 */
function fmtTs(raw: string): string {
  const d = new Date(raw);
  if (Number.isNaN(d.getTime())) return raw;
  const now = new Date();
  const sameDay = d.getFullYear() === now.getFullYear()
    && d.getMonth() === now.getMonth()
    && d.getDate() === now.getDate();
  const hms = `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`;
  return sameDay ? hms : `${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${hms}`;
}

/**
 * 窗口化渲染（轻量虚拟滚动）：日志页选 1000 行时全量渲染会到 ~5000 个
 * DOM 节点，滚动与重渲染都变沉。这里只渲染可视区 ± OVERSCAN 行，
 * 上下占位 div 撑住总高度，滚动条行为与全量渲染一致。
 * 固定行高是前提——.log-row 用 min-height + nowrap，实际高度恒定。
 */
function VirtualLog({ lines }: { lines: string[] }) {
  const boxRef = useRef<HTMLDivElement>(null);
  const [range, setRange] = useState({ start: 0, end: 60 });

  const recompute = () => {
    const el = boxRef.current;
    if (!el) return;
    const start = Math.max(0, Math.floor(el.scrollTop / ROW_H) - OVERSCAN);
    const visible = Math.ceil(el.clientHeight / ROW_H) + OVERSCAN * 2;
    setRange({ start, end: Math.min(lines.length, start + visible) });
  };

  useEffect(() => {
    recompute();
    const el = boxRef.current;
    if (!el) return;
    el.addEventListener('scroll', recompute, { passive: true });
    const ro = new ResizeObserver(recompute);
    ro.observe(el);
    return () => { el.removeEventListener('scroll', recompute); ro.disconnect(); };
    // lines 变化（换档/过滤/刷新）后行数变，可视区也要重算
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lines.length]);

  const padTop = range.start * ROW_H;
  const padBottom = Math.max(0, (lines.length - range.end) * ROW_H);
  const view = lines.slice(range.start, range.end);

  return (
    <div className="log-box" ref={boxRef}>
      {lines.length === 0 && <div className="faint">没有匹配的日志行</div>}
      {padTop > 0 && <div style={{ height: padTop }} />}
      {view.map((raw, i) => {
        const l = parseLine(raw);
        const cls = l.level === 'ERROR' ? 'err' : l.level === 'WARN' ? 'warn' : l.level === 'INFO' ? 'info' : 'dbg';
        return (
          <div className={'log-row log-' + cls} key={range.start + i} title={raw}>
            <span className="log-ts">{fmtTs(l.ts)}</span>
            <span className="log-lv">{l.level || '—'}</span>
            <span className="log-mod">{l.module}</span>
            <span className="log-msg">{l.msg}</span>
          </div>
        );
      })}
      {padBottom > 0 && <div style={{ height: padBottom }} />}
    </div>
  );
}

export function Logs() {
  const [lines, setLines] = useState(300);
  const [level, setLevel] = useState('');
  // 关键词走 300ms 防抖：搜索框每敲一个字符都打一次 /api/logs 的话，
  // 一次过滤请求要后端多读 6 倍日志窗口，连打十个字就是十次白跑。
  const [draft, setDraft] = useState('');
  const [q, setQ] = useState('');
  // 排障时最想要的是「它自己动」：每 5s 拉一次尾部，不用反复点刷新。
  const [auto, setAuto] = useState(false);
  useEffect(() => {
    const t = setTimeout(() => setQ(draft.trim()), 300);
    return () => clearTimeout(t);
  }, [draft]);
  // ★ 参数名必须与后端 logs_view 对齐：它读 `grep` 和 `level`。
  //   原先发的是 `g` / `lv`（legacy 脚本的简写），后端一个都不认 ——
  //   级别筛选和关键词框看起来能选能打，实际每次都是全量返回，
  //   属于「点了没反应」里最隐蔽的一种。
  const q2 = new URLSearchParams({
    lines: String(lines),
    ...(level ? { level } : {}),
    ...(q ? { grep: q } : {}),
  });
  const { data, error, loading, reload } = useAsync<{ lines: string[]; file_size: number }>(
    () => api.get('/api/logs?' + q2.toString()), [lines, level, q],
  );
  // reload 每次渲染都是新闭包，用 ref 转发，避免 setInterval 被反复重建。
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  useEffect(() => {
    if (!auto) return;
    const t = setInterval(() => reloadRef.current(), 5000);
    return () => clearInterval(t);
  }, [auto]);
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <Card
      title={<span>运行日志 <span className="faint small" style={{ fontWeight: 400 }}>命中 {data?.lines.length ?? 0} 行</span></span>}
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
          <label className={'tog' + (auto ? ' on' : '')} title="每 5 秒自动拉取最新日志">
            <input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} />
            <i /><span>自动</span>
          </label>
          <button className="btn sm" onClick={reload}>刷新</button>
        </div>
      }
    >
      {loading ? <Loading /> : <VirtualLog lines={data?.lines ?? []} />}
      {data && (
        <div className="faint small mt-2">
          日志文件约 {Math.round((data.file_size ?? 0) / 1024)} KiB（尾部 256 KiB 窗口内查找）
        </div>
      )}
    </Card>
  );
}
