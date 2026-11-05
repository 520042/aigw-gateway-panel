import { useState } from 'react';
import { Card, Note, useAsync, Loading, ErrorBox } from '../components';
import { api, type Health } from '../api';

interface UpdateCheck {
  ok: boolean;
  note?: string;
  local: { version: string; milestone: string; exe_built_ts: number; data_dir: string };
  remote: { reachable: boolean; sha: string; date: string };
}

const fmtBytes = (n: number) =>
  n >= 1 << 20 ? (n / (1 << 20)).toFixed(1) + ' MiB' : n >= 1 << 10 ? (n / (1 << 10)).toFixed(1) + ' KiB' : n + ' B';

export function About() {
  const h = useAsync<Health>(() => api.get('/api/health'), []);
  const up = useAsync<UpdateCheck>(() => api.get('/api/update-check'), []);
  const [busy, setBusy] = useState('');

  if (h.error) return <ErrorBox message={h.error} onRetry={h.reload} />;
  if (!h.data) return <Loading />;
  const d = h.data;

  const backup = async () => {
    setBusy('正在导出…');
    try {
      const blob = await api.post<unknown>('/api/backup/export', {});
      const a = document.createElement('a');
      a.href = URL.createObjectURL(new Blob([JSON.stringify(blob, null, 2)], { type: 'application/json' }));
      a.download = `aigw-backup-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(a.href);
      setBusy('✔ 备份已下载');
    } catch (e) { setBusy('导出失败：' + (e instanceof Error ? e.message : String(e))); }
  };

  const restore = async () => {
    const inp = document.createElement('input');
    inp.type = 'file';
    inp.accept = '.json';
    inp.onchange = async () => {
      const f = inp.files?.[0];
      if (!f) return;
      if (!confirm(`用 ${f.name} 恢复数据？现有同名文件会先留 .bak 副本。`)) return;
      setBusy('正在恢复…');
      try {
        const text = await f.text();
        JSON.parse(text);
        const r = await api.post<{ ok?: boolean; message?: string; restart_required?: boolean }>('/api/backup', {
          files: [{ name: f.name.replace(/\.json$/i, '') + '.json', bytes: btoa(unescape(encodeURIComponent(text))) }],
        });
        setBusy(r?.ok
          ? `✔ 恢复完成${r.restart_required ? '（数据库文件将在下次重启时生效）' : ''}`
          : '✗ ' + (r?.message ?? '恢复失败'));
      } catch (e) { setBusy('恢复失败：' + (e instanceof Error ? e.message : String(e))); }
    };
    inp.click();
  };

  const checkUpdate = () => up.reload();

  const remote = up.data?.remote;
  const local = up.data?.local;
  const remoteTxt = remote?.reachable
    ? `${remote.sha.slice(0, 8)}（${remote.date || ''}）`
    : '远端不可达（离线）';
  const behind = remote?.reachable && remote.sha && local?.exe_built_ts
    ? undefined // 后端只给 sha 不给本地 sha；此处只展示，不做「落后几个提交」的假算术
    : undefined;
  void behind;

  const lg = d.logs, ar = d.archives, db = d.db;

  return (
    <>
      {d.persist_error && (
        <Card className="card card-err" title={<b className="c-err">上次写盘失败</b>}>
          <div className="small">文件 <code className="mono">{d.persist_error.file}</code>（{d.persist_error.s_ago}s 前）</div>
          <div className="small faint mt-2">{d.persist_error.error}</div>
          <div className="small mt-2">这次改动<b>没有</b>写进磁盘，重启后会回退到旧状态。多半是磁盘满、数据目录只读或文件被占用；处理后再改一次即可。</div>
        </Card>
      )}

      <Card title={`aigw v2 · ${d.service}（一包多程）`}>
        <div className="kv">
          <div><span>本地版本</span><b>{d.version} · {d.milestone}</b></div>
          <div><span>远端最新 commit</span><b>{remoteTxt}</b></div>
          <div><span>数据目录</span><code className="mono">{local?.data_dir ?? '—'}</code></div>
        </div>
        <div className="row mt-3">
          <button className="btn" onClick={checkUpdate}>⟳ 检查更新</button>
          <button className="btn pri" disabled={!!busy} onClick={backup}>⬇ 备份数据</button>
          <button className="btn" disabled={!!busy} onClick={restore}>⬆ 恢复备份</button>
        </div>
        {busy && <Note tone={busy.startsWith('✔') ? 'ok' : undefined}>{busy}</Note>}
        {up.data?.note && <div className="faint small mt-2">{up.data.note}</div>}
      </Card>

      <Card title="数据占用" extra={<span className="faint small" style={{ fontWeight: 400, marginLeft: 'auto' }}>全部在本机数据目录，不上传</span>}>
        <div className="kv">
          <div><span>用量台账（{db.backend}）</span><b>{db.bytes != null ? `${fmtBytes(db.bytes)} · ${db.events} 条事件` : `${db.events} 条事件`}</b></div>
          <div><span>运行日志</span><b>{lg ? `${fmtBytes(lg.bytes)} · ${lg.files} 个文件` : '—'}</b></div>
          <div><span>备份留档</span><b>{ar ? `${fmtBytes(ar.bytes)} · ${ar.files} 个文件` : '—'}</b></div>
        </div>
        <div className="faint small mt-2">自动上限：台账保留 90 天 / 最多 5000 条；日志 4MB×5 滚动；留档每族最多 6 份。超出会自动清理，无需手动维护。</div>
        {db && db.backend !== 'sqlite' && (
          <Note tone="err">台账降级为 JSON 模式（SQLite 打不开）：建议检查数据目录权限后重启。</Note>
        )}
      </Card>

      <Card title="模型链">
        <Note>
          多模型按顺序组成候选链，前一个挂了自动换下一个。写法：<code>模型名@平台</code>，
          或在清单内直接写裸模型名（网关按启用的平台自动路由）。
        </Note>
      </Card>

      <Card title="使用说明">
        <Note>
          ① 在「接入源」页选平台并登录 ② 客户端 <code>base_url</code> 填 <code>{location.origin}/v1</code>、<code>api_key</code> 填 <code>admin</code>
          ③ <code>model</code> 填 <code>模型id@平台</code> 或清单内模型。<br />
          面板只监听本机回环；凭据落盘用 DPAPI 加密，日志里不会写入 token。
        </Note>
      </Card>

      <Card title="在线更新">
        <div className="faint small">
          本地为 Rust 工程。更新流程：<code className="mono">git pull</code> → <code className="mono">npm run build</code>（web-src）→
          <code className="mono"> cargo build --release</code> → 用新 aigw.exe 与 providers/ 覆盖部署目录 → 重启计划任务 Aigw2Service。
          更新前建议先「⬇ 备份数据」。
        </div>
      </Card>
    </>
  );
}
