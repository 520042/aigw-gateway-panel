/** 后端 API 客户端：所有请求走这里，统一错误处理与面板令牌。 */
export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
    this.name = 'ApiError';
  }
}

function withToken(): RequestInit {
  // 面板令牌由旧版脚本存在 cookie / localStorage；新框架沿用，避免两套管控。
  return {};
}

async function request<T = unknown>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...withToken(),
    ...init,
    headers: {
      'Content-Type': 'application/json',
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const j = (await res.json()) as { message?: string; error?: { message?: string } };
      msg = j.message ?? j.error?.message ?? msg;
    } catch { /* 非 JSON 错误体，用状态码 */ }
    throw new ApiError(res.status, msg);
  }
  return (await res.json()) as T;
}

export const api = {
  get: <T = unknown>(path: string) => request<T>(path),
  post: <T = unknown>(path: string, body: unknown) =>
    request<T>(path, { method: 'POST', body: JSON.stringify(body) }),
};
/** 常用后端响应形状。 */
export interface Health {
  ok: boolean;
  service: string;
  version: string;
  milestone: string;
  build: { profile: string; uptime_s: number; target: string };
  platforms: number;
  token_required: boolean;
  tasks: Record<string, TaskStat>;
  credits: { total: number; accounts: number };
  db: { backend: string; events: number; bytes?: number };
  logs?: { files: number; bytes: number };
  archives?: { files: number; bytes: number };
  /** 最近一次落盘失败（各 Store 只 warn 不抛错，这里是唯一能看见它的地方）。 */
  persist_error?: { file: string; s_ago: number; error: string } | null;
}

export interface TaskStat {
  ok_runs: number;
  err_runs: number;
  last_ok: string | null;
  last_ok_s_ago: number | null;
  last_err: string | null;
  last_err_s_ago: number | null;
}

export interface Platform {
  id: string;
  name: string;
  enabled: boolean;
  auth: string;
  region?: string;
  accounts_n: number;
  usable: boolean;
  anon: boolean;
  placeholder: boolean;
  upstream_kind: string;
  models: string[];
  models_n: number;
  models_on: number;
  login_methods: string[];
  login_url: string;
  notes: string;
  credit_total: number | null;
  recent: { requests: number; success: number; failed: number; cancelled: number } | null;
  last_error: unknown;
  rates?: Record<string, number>;
}

export interface PlatformResponse {
  rows: Platform[];
  total: number;
  disabled_models: string[];
  disabled_platforms: string[];
}

export interface Account {
  id: string;
  name: string;
  platform: string;
  platform_name: string;
  enabled: boolean;
  kind: string;
  secret_len: number;
  anon: boolean;
  needs_relogin: boolean;
  balance_supported: boolean;
  health: { status?: string; label?: string; age_secs?: number } | null;
  credit?: number;
  cooling_s?: number;
  profile?: { name?: string; email?: string; plan?: string };
  token_limit?: number | null;
}

export interface AccountsResponse {
  rows: Account[];
  total: number;
  daily_checkin: boolean;
  daily_growth: boolean;
  checkin_task?: unknown;
}

export interface Usage {
  requests: number;
  success: number;
  failed: number;
  cancelled: number;
  tokensIn: number;
  tokensOut: number;
  latency: { avg: number; p50: number; p95: number; samples: number };
  ttfb: { p50: number; p95: number; samples: number };
  byPlatform: { rows: Array<{ key: string; requests: number; success: number; failed: number; cancelled: number }> };
}

export interface PoolState {
  cooling: unknown[];
  inflight: unknown[];
  max_inflight: number;
  sticky_entries: number;
  sticky: { total: number; truncated: boolean; ttl_s: number };
  waf: unknown[];
}

export interface AutoConfig {
  daily_checkin: boolean;
  daily_growth: boolean;
  run_hour: number;
  run_minute: number;
  per_platform: boolean;
  checkin_platforms: string[];
}

export interface CheckinConfigResponse {
  ok: boolean;
  cfg: AutoConfig;
  platforms: Array<{ id: string; name: string }>;
}

export interface TaskRecord {
  ts: number;
  source: string;
  task: string;
  platform: string;
  account: string;
  outcome: string;
  detail: string;
}

export interface TaskRecordsResponse {
  ok: boolean;
  rows: TaskRecord[];
  total: number;
  shown: number;
  limit: number;
  offset: number;
}
