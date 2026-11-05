import { useState } from 'react';
import { Card, Note, useAsync, Loading, ErrorBox } from '../components';
import { api, type Health } from '../api';

export function About() {
  const h = useAsync<Health>(() => api.get('/api/health'), []);
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

  return (
    <>
      <Card title={`aigw v2 · ${d.service}（一包多程）`}>
        <div className="mini-grid">
          <div className="mini"><b>本地版本</b><span className="mono">{d.version} · {d.milestone}</span></div>
          <div className="mini"><b>构建</b><span className="mono">{d.build.profile} · {d.build.target}</span></div>
          <div className="mini"><b>台账</b><span className="mono">{d.db.backend} · {d.db.events} 条事件</span></div>
        </div>
        <div className="row mt-3">
          <button className="btn pri" disabled={!!busy} onClick={backup}>⬇ 备份数据</button>
          <button className="btn" onClick={() => window.location.reload()}>↻ 刷新</button>
        </div>
        {busy && <Note tone={busy.startsWith('✔') ? 'ok' : undefined}>{busy}</Note>}
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
    </>
  );
}
