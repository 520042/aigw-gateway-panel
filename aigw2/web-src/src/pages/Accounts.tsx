import { Card, Note, Loading, ErrorBox, Empty, Pill, useAsync } from '../components';
import { api, type AccountsResponse } from '../api';

const fmt = (n: number) => Number(n || 0).toLocaleString('en-US');

/**
 * 账号凭据表。任务中心已拆到独立「任务中心」视图（2026-11-06）——
 * 签到覆盖 5 家平台（Lobster / 小浣熊非 WB 系），挂在「模型与用量」下
 * 名不副实，用户指出后拆出。本页只保留账号本体。
 */
export function Accounts() {
  const ac = useAsync<AccountsResponse>(() => api.get('/api/accounts'), []);

  if (ac.error) return <ErrorBox message={ac.error} onRetry={ac.reload} />;
  if (!ac.data) return <Loading />;

  return (
    <>
      <Card
        title="账号凭据"
        extra={
          <button className="btn sm gh" style={{ marginLeft: 'auto' }} disabled={ac.loading}
            onClick={async () => {
              try { await api.post('/api/accounts', { action: 'verify' }); ac.reload(); } catch { /* 刷新失败静默 */ }
            }}>
            逐账号验活
          </button>
        }
      >
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
