import type { Ctx } from "../centaur";
import { query } from "../_lib/http";

// Results are Datadog's own API JSON, passed through: single-object endpoints are
// wrapped in a named field, list endpoints in a named array. Field names are
// Datadog's (https://docs.datadoghq.com/api/latest/).

const DEFAULT_SITE = "https://api.datadoghq.com";

/** One log event from the v2 log search API. */
export interface LogEvent {
  id?: string;
  type?: string;
  attributes?: {
    message?: string;
    /** ISO-8601. */
    timestamp?: string;
    status?: string;
    service?: string;
    host?: string;
    tags?: string[];
    /** The event's custom (non-reserved) attributes. */
    attributes?: Record<string, unknown>;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** `POST /api/v2/logs/events/search`: the page of events plus its cursor metadata. */
export interface LogSearchResponse {
  data?: LogEvent[];
  meta?: { page?: { after?: string; [field: string]: unknown }; [field: string]: unknown };
  links?: { next?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** One series of an `/api/v1/query` metric response. */
export interface MetricSeries {
  metric?: string;
  display_name?: string;
  /** `[epoch seconds, value]` pairs; the value is null for missing points. */
  pointlist?: (number | null)[][];
  interval?: number;
  unit?: { name?: string; short_name?: string; [field: string]: unknown }[] | string;
  scope?: string;
  expression?: string;
  tag_set?: string[];
  [field: string]: unknown;
}

/** `GET /api/v1/query` metric response. */
export interface MetricQueryResponse {
  status?: string;
  query?: string;
  from_date?: number;
  to_date?: number;
  series?: MetricSeries[];
  [field: string]: unknown;
}

/** A Datadog monitor object. */
export interface Monitor {
  id?: number;
  name?: string;
  /** OK, ALERT, WARN, NO DATA or IGNORED (also per group). */
  overall_state?: string;
  type?: string;
  query?: string;
  message?: string;
  tags?: string[];
  multi?: boolean;
  created?: string;
  modified?: string;
  creator?: { email?: string; handle?: string; name?: string; [field: string]: unknown };
  options?: Record<string, unknown>;
  state?: Record<string, unknown>;
  [field: string]: unknown;
}

/** One monitor-search result (`/api/v1/monitor/search`). */
export interface MonitorSearchResult {
  id?: number;
  name?: string;
  /** The matched field: name, query or tags. */
  matched?: string;
  status?: string;
  type?: string;
  tags?: string[];
  [field: string]: unknown;
}

/** `GET /api/v1/monitor/search`. */
export interface MonitorSearchResponse {
  counts?: { status?: Record<string, number>; type?: Record<string, number>; [field: string]: unknown };
  monitors?: MonitorSearchResult[];
  metadata?: { page?: number; per_page?: number; total_count?: number; [field: string]: unknown };
  [field: string]: unknown;
}

/** A host from `/api/v1/hosts`. */
export interface Host {
  name?: string;
  id?: number;
  aliases?: string[];
  apps?: string[];
  is_muted?: boolean;
  last_reported_time?: number;
  up?: boolean;
  tags_by_source?: Record<string, string[]>;
  [field: string]: unknown;
}

/** `GET /api/v1/hosts`. */
export interface HostListResponse {
  host_list?: Host[];
  total_matching?: number;
  total_returned?: number;
  [field: string]: unknown;
}

/** A dashboard summary from `/api/v1/dashboard` (search). */
export interface DashboardSummary {
  id?: string;
  title?: string;
  description?: string;
  layout_type?: string;
  url?: string;
  author?: { handle?: string; name?: string; [field: string]: unknown };
  created_at?: string;
  modified_at?: string;
  [field: string]: unknown;
}

/** `GET /api/v1/dashboard`: the dashboard list. */
export interface DashboardListResponse {
  dashboards?: DashboardSummary[];
  [field: string]: unknown;
}

/** A full dashboard as `get_dashboard` returns it. */
export interface Dashboard {
  id?: string;
  title?: string;
  description?: string;
  layout_type?: string;
  widgets?: Record<string, unknown>[];
  template_variables?: { name: string; default?: string; prefix?: string; available_values?: string[] }[];
  url?: string;
  [field: string]: unknown;
}

function baseUrl(ctx: Ctx): string {
  const site = (ctx.env.DD_SITE || DEFAULT_SITE).trim().replace(/\/+$/, "");
  if (!site) return DEFAULT_SITE;
  return /^https?:\/\//.test(site) ? site : `https://${site}`;
}

function headers(ctx: Ctx): Record<string, string> {
  // Placeholders by default: the proxy swaps them for the real keys on the wire.
  return {
    accept: "application/json",
    "DD-API-KEY": ctx.env.DD_API_KEY || "DD_API_KEY",
    "DD-APPLICATION-KEY": ctx.env.DD_APP_KEY || "DD_APP_KEY",
  };
}

async function request(
  ctx: Ctx,
  method: string,
  path: string,
  params: Record<string, string | number | boolean | undefined> = {},
  body?: unknown,
): Promise<any> {
  const all = headers(ctx);
  if (body !== undefined) all["content-type"] = "application/json";
  const res = await ctx.fetch(`${baseUrl(ctx)}${path}${query(params)}`, {
    method,
    headers: all,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Datadog API error (HTTP ${res.status}): ${text.slice(0, 1000)}`);
  return text ? JSON.parse(text) : {};
}

const UNITS: Record<string, number> = { s: 1, m: 60, h: 3600, d: 86400, w: 604800 };

/**
 * A time argument to epoch milliseconds: an ISO-8601 time (UTC when no offset), epoch seconds or
 * milliseconds, or a relative lookback such as `15m`, `2h`, `7d`, `1w`. Omitted means now.
 */
function resolveTime(value: string | undefined): number {
  if (!value) return Date.now();
  const raw = value.trim();
  if (/^\d+$/.test(raw)) {
    const epoch = Number(raw);
    return epoch > 10_000_000_000 ? epoch : epoch * 1000;
  }
  const relative = /^(\d+)([smhdw])$/.exec(raw);
  if (relative) return Date.now() - Number(relative[1]) * UNITS[relative[2]] * 1000;
  const iso = /(?:Z|[+-]\d{2}:?\d{2})$/.test(raw) ? raw : `${raw}Z`;
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) {
    throw new Error(`time ${JSON.stringify(value)} is not ISO-8601, epoch seconds/millis or 15m/2h/7d`);
  }
  return ms;
}

/** Validate the API and application keys. */
export async function validate(args: {}, ctx: Ctx): Promise<{ valid: boolean }> {
  const body = await request(ctx, "GET", "/api/v1/validate");
  return { valid: Boolean(body.valid) };
}

/**
 * Search logs with Datadog log query syntax, for example `service:api status:error` or
 * `@http.status_code:>=500`. `start`/`end` are ISO-8601, epoch seconds/millis or a relative
 * lookback such as `15m` or `2h` (`start` defaults to 15m ago, `end` to now); `limit` is 1-1000;
 * `sort` is timestamp (oldest first) or -timestamp (newest first). For more than one page, pass the
 * previous response's `meta.page.after` as `cursor`.
 */
export async function search_logs(
  args: {
    query: string;
    start?: string;
    end?: string;
    limit?: number;
    sort?: "timestamp" | "-timestamp";
    cursor?: string;
  },
  ctx: Ctx,
): Promise<LogSearchResponse> {
  return request(ctx, "POST", "/api/v2/logs/events/search", {}, {
    filter: {
      query: args.query,
      from: new Date(resolveTime(args.start ?? "15m")).toISOString(),
      to: new Date(resolveTime(args.end)).toISOString(),
    },
    sort: args.sort ?? "-timestamp",
    page: { limit: Math.max(1, Math.min(args.limit ?? 50, 1000)), cursor: args.cursor },
  });
}

/**
 * Query metrics with a Datadog metric query, for example `avg:system.cpu.user{*}` or
 * `sum:trace.http.request.errors{service:api}.as_count()`. `start`/`end` are ISO-8601, epoch
 * seconds/millis or a relative lookback such as `1h` or `7d` (`start` defaults to 1h ago, `end` to
 * now). Returns one `series` entry per group with `pointlist` of `[epoch seconds, value]` pairs.
 */
export async function query_metrics(
  args: { query: string; start?: string; end?: string },
  ctx: Ctx,
): Promise<MetricQueryResponse> {
  return request(ctx, "GET", "/api/v1/query", {
    query: args.query,
    from: Math.floor(resolveTime(args.start ?? "1h") / 1000),
    to: Math.floor(resolveTime(args.end) / 1000),
  });
}

/**
 * Monitors, optionally filtered by `query` (matches the monitor name), `group_states`
 * (comma-separated, for example `alert,warn`), `tags` (comma-separated monitor tags) and `limit`
 * (1-1000). Prefer `search_monitors` for richer server-side search.
 */
export async function list_monitors(
  args: { query?: string; group_states?: string; tags?: string; limit?: number },
  ctx: Ctx,
): Promise<{ monitors: Monitor[] }> {
  const body = await request(ctx, "GET", "/api/v1/monitor", {
    monitor_tags: args.tags,
    group_states: args.group_states,
    name: args.query,
    page_size: Math.max(1, Math.min(args.limit ?? 100, 1000)),
  });
  return { monitors: body };
}

/**
 * Search monitors with Datadog monitor search syntax, for example `status:alert`, `tag:env:prod`
 * or `type:metric`. `page` is zero-based, `per_page` is 1-100. Returns the matching monitors,
 * per-status/per-type `counts` and `metadata.total_count`.
 */
export async function search_monitors(
  args: { query?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<MonitorSearchResponse> {
  return request(ctx, "GET", "/api/v1/monitor/search", {
    query: args.query ?? "",
    page: args.page ?? 0,
    per_page: Math.max(1, Math.min(args.per_page ?? 30, 100)),
  });
}

/** One monitor by numeric ID, including its query, thresholds, state and message. */
export async function get_monitor(
  args: { monitor_id: number },
  ctx: Ctx,
): Promise<{ monitor: Monitor }> {
  return { monitor: await request(ctx, "GET", `/api/v1/monitor/${args.monitor_id}`) };
}

/**
 * Hosts reporting to Datadog. `filter` is a host filter query (for example `env:prod`),
 * `sort_field` defaults to last_reported_time and `sort_dir` is asc or desc; `count` is 1-1000.
 * Returns `host_list` with `total_matching` and `total_returned`.
 */
export async function list_hosts(
  args: { filter?: string; sort_field?: string; sort_dir?: "asc" | "desc"; count?: number },
  ctx: Ctx,
): Promise<HostListResponse> {
  return request(ctx, "GET", "/api/v1/hosts", {
    filter: args.filter,
    sort_field: args.sort_field ?? "last_reported_time",
    sort_dir: args.sort_dir ?? "desc",
    count: Math.max(1, Math.min(args.count ?? 100, 1000)),
  });
}

/** Dashboards whose title matches `query` (case-insensitive substring), up to `limit` (1-1000). */
export async function search_dashboards(
  args: { query?: string; limit?: number },
  ctx: Ctx,
): Promise<DashboardListResponse> {
  return request(ctx, "GET", "/api/v1/dashboard", {
    "filter[query]": args.query,
    count: Math.max(1, Math.min(args.limit ?? 100, 1000)),
  });
}

/** One dashboard by ID (`abc-def-ghi`), including its widgets and template variables. */
export async function get_dashboard(
  args: { dashboard_id: string },
  ctx: Ctx,
): Promise<{ dashboard: Dashboard }> {
  const id = encodeURIComponent(args.dashboard_id.trim());
  return { dashboard: await request(ctx, "GET", `/api/v1/dashboard/${id}`) };
}
