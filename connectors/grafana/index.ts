import type { Ctx, Response } from "../centaur";
import { encode, query, type Params } from "../_lib/http";

// Results are Grafana's own API JSON, passed through: list endpoints are wrapped
// in a named array, datasource-proxy queries keep the proxied backend's envelope.
// Field names are Grafana's (https://grafana.com/docs/grafana/latest/developers/http_api/).

/** One `/api/search` hit: a dashboard or a folder. */
export interface DashboardSearchResult {
  id?: number;
  uid?: string;
  title?: string;
  type?: "dash-db" | "dash-folder";
  tags?: string[];
  uri?: string;
  url?: string;
  slug?: string;
  folderId?: number;
  folderUid?: string;
  folderTitle?: string;
  isStarred?: boolean;
  sortMeta?: number;
  [field: string]: unknown;
}

/** A Grafana dashboard model (panels, templating, layout, version). */
export interface Dashboard {
  id?: number;
  uid?: string;
  title?: string;
  description?: string;
  tags?: string[];
  version?: number;
  schemaVersion?: number;
  panels?: Record<string, unknown>[];
  templating?: Record<string, unknown>;
  time?: Record<string, unknown>;
  timezone?: string;
  [field: string]: unknown;
}

/** `get_dashboard`: the dashboard model plus its metadata (folder, url, versions). */
export interface DashboardEnvelope {
  dashboard?: Dashboard;
  meta?: {
    folderId?: number;
    folderUid?: string;
    folderTitle?: string;
    url?: string;
    slug?: string;
    version?: number;
    canSave?: boolean;
    canEdit?: boolean;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** `save_dashboard` and `delete_dashboard` results. */
export interface DashboardSaveResult {
  id?: number;
  uid?: string;
  slug?: string;
  status?: string;
  url?: string;
  version?: number;
  message?: string;
  title?: string;
  [field: string]: unknown;
}

/** A configured datasource. */
export interface Datasource {
  id?: number;
  uid?: string;
  name?: string;
  type?: string;
  url?: string;
  access?: string;
  isDefault?: boolean;
  database?: string;
  readOnly?: boolean;
  [field: string]: unknown;
}

/** One series of a MetricsQL / Prometheus query result. */
export interface MetricsSeries {
  /** The series' label set; empty for a scalar. */
  metric?: Record<string, string>;
  /** Instant query sample: `[epoch seconds, value]`. */
  value?: [number, string];
  /** Range query samples: `[epoch seconds, value]` pairs. */
  values?: [number, string][];
  [field: string]: unknown;
}

/** A MetricsQL instant or range query result. */
export interface MetricsQueryResult {
  status?: string;
  data?: { resultType?: string; result?: MetricsSeries[]; [field: string]: unknown };
  error?: string;
  [field: string]: unknown;
}

/** One VictoriaLogs log line: its reserved fields plus the stream's other fields. */
export interface LogsQLLine {
  /** RFC3339 timestamp. */
  _time?: string;
  _stream?: string;
  /** The log message. */
  _msg?: string;
  [field: string]: unknown;
}

/** An active alert from Grafana's Prometheus-compatible alerts API. */
export interface Alert {
  labels?: Record<string, string>;
  annotations?: Record<string, string>;
  state?: string;
  /** ISO-8601. */
  activeAt?: string;
  value?: string;
  [field: string]: unknown;
}

/** An alert rule group. */
export interface AlertRuleGroup {
  name?: string;
  file?: string;
  folderUid?: string;
  interval?: number;
  rules?: Record<string, unknown>[];
  [field: string]: unknown;
}

/** A dashboard annotation. */
export interface Annotation {
  id?: number;
  dashboardId?: number;
  dashboardUID?: string;
  panelId?: number;
  /** Epoch milliseconds. */
  time?: number;
  timeEnd?: number;
  text?: string;
  tags?: string[];
  [field: string]: unknown;
}

/** `/api/health`. */
export interface Health {
  commit?: string;
  database?: string;
  version?: string;
  [field: string]: unknown;
}

/** A Grafana thread-debugger deep link built from Slack thread input. */
export interface ThreadDebugUrl {
  thread_input: string;
  thread_key: string;
  dashboard_uid: string;
  from: string;
  to: string;
  url: string;
}

function baseUrl(ctx: Ctx): string {
  const url = (ctx.env.GRAFANA_URL || "").trim().replace(/\/+$/, "");
  if (!url) throw new Error("GRAFANA_URL is not set");
  return /^https?:\/\//.test(url) ? url : `http://${url}`;
}

/** The browser-facing URL (GRAFANA_PUBLIC_URL, then GRAFANA_ROOT_URL, then GRAFANA_URL). */
function publicUrl(ctx: Ctx): string {
  const url = (ctx.env.GRAFANA_PUBLIC_URL || ctx.env.GRAFANA_ROOT_URL || "")
    .trim()
    .replace(/\/+$/, "");
  if (!url) return baseUrl(ctx);
  return /^https?:\/\//.test(url) ? url : `http://${url}`;
}

/**
 * A service-account token in `Authorization: Bearer …`, falling back to basic auth when only
 * GRAFANA_PASSWORD is configured. The default is the placeholder the proxy swaps for the token.
 */
function authorization(ctx: Ctx): string {
  const key = ctx.env.GRAFANA_API_KEY;
  if (key) return `Bearer ${key}`;
  const password = ctx.env.GRAFANA_PASSWORD;
  if (password) {
    const user = ctx.env.GRAFANA_USER || "admin";
    return `Basic ${ctx.base64.encode(`${user}:${password}`)}`;
  }
  return "Bearer GRAFANA_API_KEY";
}

interface RequestOptions {
  params?: Params;
  json?: unknown;
  form?: Params;
}

async function send(
  ctx: Ctx,
  method: string,
  path: string,
  options: RequestOptions = {},
): Promise<Response> {
  const headers: Record<string, string> = {
    accept: "application/json",
    authorization: authorization(ctx),
  };
  let body: string | undefined;
  if (options.json !== undefined) {
    headers["content-type"] = "application/json";
    body = JSON.stringify(options.json);
  } else if (options.form !== undefined) {
    headers["content-type"] = "application/x-www-form-urlencoded";
    body = encode(options.form);
  }
  const res = await ctx.fetch(`${baseUrl(ctx)}${path}${query(options.params ?? {})}`, {
    method,
    headers,
    body,
  });
  if (!res.ok) {
    const message = await res.text();
    throw new Error(`Grafana API error (HTTP ${res.status}): ${message.slice(0, 1000)}`);
  }
  return res;
}

async function json(
  ctx: Ctx,
  method: string,
  path: string,
  options: RequestOptions = {},
): Promise<any> {
  const res = await send(ctx, method, path, options);
  const raw = await res.text();
  return raw ? JSON.parse(raw) : {};
}

async function text(
  ctx: Ctx,
  method: string,
  path: string,
  options: RequestOptions = {},
): Promise<string> {
  const res = await send(ctx, method, path, options);
  return res.text();
}

const seg = (value: string): string => encodeURIComponent(value.trim());

/**
 * Dashboards (and folders) by title substring, tag and `type` (dash-db or dash-folder). Returns
 * uid, title, url, tags and folder for each hit, so a search can be followed by `get_dashboard`.
 */
export async function search_dashboards(
  args: { query?: string; tag?: string; type?: "dash-db" | "dash-folder"; limit?: number },
  ctx: Ctx,
): Promise<{ dashboards: DashboardSearchResult[] }> {
  const body = await json(ctx, "GET", "/api/search", {
    params: {
      type: args.type ?? "dash-db",
      limit: Math.max(1, args.limit ?? 50),
      query: args.query,
      tag: args.tag,
    },
  });
  return { dashboards: body };
}

/** One dashboard by UID: the full model (panels, templating) plus its `meta` (folder, url). */
export async function get_dashboard(
  args: { uid: string },
  ctx: Ctx,
): Promise<DashboardEnvelope> {
  return json(ctx, "GET", `/api/dashboards/uid/${seg(args.uid)}`);
}

/**
 * Create or update a dashboard from a dashboard model (or a `get_dashboard` result, whose
 * `dashboard`/`meta` envelope is unwrapped). Fetch a dashboard, edit it, and save it back:
 * `dashboard.version` is sent as-is, so Grafana rejects the save with HTTP 412 when someone else
 * changed it since, unless `overwrite` is true. Omit `dashboard.uid`/`dashboard.id` to create a new
 * dashboard. `folder_uid` defaults to the fetched dashboard's folder; `message` is the
 * version-history note. Returns the saved uid, version and url.
 */
export async function save_dashboard(
  args: {
    dashboard: Record<string, unknown>;
    folder_uid?: string;
    message?: string;
    overwrite?: boolean;
  },
  ctx: Ctx,
): Promise<DashboardSaveResult> {
  if (typeof args.dashboard !== "object" || args.dashboard === null) {
    throw new Error("dashboard must be a JSON object");
  }
  const inner = args.dashboard.dashboard;
  const meta = (args.dashboard.meta ?? {}) as Record<string, unknown>;
  const dashboard =
    typeof inner === "object" && inner !== null ? (inner as Record<string, unknown>) : args.dashboard;
  if (!dashboard.title) throw new Error("dashboard.title is required");
  const payload: Record<string, unknown> = {
    dashboard,
    overwrite: args.overwrite ?? false,
  };
  const folder = args.folder_uid ?? meta.folderUid;
  if (folder) payload.folderUid = folder;
  if (args.message) payload.message = args.message;
  return json(ctx, "POST", "/api/dashboards/db", { json: payload });
}

/** Delete a dashboard by UID. Grafana answers with a status message; the deletion is permanent. */
export async function delete_dashboard(
  args: { uid: string },
  ctx: Ctx,
): Promise<DashboardSaveResult> {
  return json(ctx, "DELETE", `/api/dashboards/uid/${seg(args.uid)}`);
}

/** Configured datasources, with the uid to pass as `datasource_uid` to the query functions. */
export async function list_datasources(args: {}, ctx: Ctx): Promise<{ datasources: Datasource[] }> {
  return { datasources: await json(ctx, "GET", "/api/datasources") };
}

/**
 * Run a MetricsQL (Prometheus-compatible) query through a datasource proxy. `expr` is the query,
 * for example `rate(http_requests_total[5m])` or `sum by (service) (avg_over_time(cpu[1h]))`.
 * `datasource_uid` defaults to `victoriametrics`. With `start` it is a range query from `start` to
 * `end` (default now) in steps of `step`; without `start` it is an instant query. `start`/`end` are
 * RFC3339 or Unix seconds, `step` is a duration such as 60s or 5m.
 */
export async function query_metrics(
  args: {
    expr: string;
    datasource_uid?: string;
    start?: string;
    end?: string;
    step?: string;
  },
  ctx: Ctx,
): Promise<MetricsQueryResult> {
  const uid = seg(args.datasource_uid ?? "victoriametrics");
  if (args.start) {
    return json(ctx, "GET", `/api/datasources/proxy/uid/${uid}/api/v1/query_range`, {
      params: { query: args.expr, start: args.start, step: args.step ?? "60s", end: args.end },
    });
  }
  return json(ctx, "GET", `/api/datasources/proxy/uid/${uid}/api/v1/query`, {
    params: { query: args.expr },
  });
}

/** Metric label names known to a MetricsQL datasource (`datasource_uid` defaults to victoriametrics). */
export async function metric_labels(
  args: { datasource_uid?: string },
  ctx: Ctx,
): Promise<{ labels: string[] }> {
  const uid = seg(args.datasource_uid ?? "victoriametrics");
  const body = await json(ctx, "GET", `/api/datasources/proxy/uid/${uid}/api/v1/labels`);
  return { labels: body.data ?? [] };
}

/** The values a label takes, for example every `service` or `pod` name. */
export async function metric_label_values(
  args: { label: string; datasource_uid?: string },
  ctx: Ctx,
): Promise<{ values: string[] }> {
  const uid = seg(args.datasource_uid ?? "victoriametrics");
  const path = `/api/datasources/proxy/uid/${uid}/api/v1/label/${seg(args.label)}/values`;
  const body = await json(ctx, "GET", path);
  return { values: body.data ?? [] };
}

/**
 * Run a LogsQL query through a VictoriaLogs datasource proxy: one JSON object per log line, with
 * the reserved fields `_time`, `_stream`, `_msg` plus the stream's other fields. `query` is LogsQL,
 * for example `_time:5m error` or `level:error AND service:api`; `limit` (default 100) is appended
 * as `| limit N`. `start`/`end` are RFC3339 or Unix nanoseconds; omit them for the last hour.
 * `datasource_uid` defaults to `victorialogs`.
 */
export async function query_victorialogs(
  args: {
    query: string;
    datasource_uid?: string;
    start?: string;
    end?: string;
    limit?: number;
  },
  ctx: Ctx,
): Promise<{ logs: LogsQLLine[] }> {
  const uid = seg(args.datasource_uid ?? "victorialogs");
  const body = await text(ctx, "POST", `/api/datasources/proxy/uid/${uid}/select/logsql/query`, {
    form: {
      query: `${args.query} | limit ${Math.max(1, args.limit ?? 100)}`,
      start: args.start,
      end: args.end,
    },
  });
  const logs: LogsQLLine[] = [];
  for (const line of body.split("\n")) {
    if (line.trim()) logs.push(JSON.parse(line));
  }
  return { logs };
}

/** Field names present in a VictoriaLogs datasource, for building LogsQL filters. */
export async function victorialogs_field_names(
  args: { datasource_uid?: string },
  ctx: Ctx,
): Promise<{ field_names: string[] }> {
  const uid = seg(args.datasource_uid ?? "victorialogs");
  const body = await json(ctx, "POST", `/api/datasources/proxy/uid/${uid}/select/logsql/field_names`, {
    form: { query: "*" },
  });
  return { field_names: (body.values ?? []).map((entry: { value: string }) => entry.value) };
}

/** The values a VictoriaLogs field takes, for example every `service` or `level`. */
export async function victorialogs_field_values(
  args: { field: string; datasource_uid?: string },
  ctx: Ctx,
): Promise<{ values: string[] }> {
  const uid = seg(args.datasource_uid ?? "victorialogs");
  const body = await json(
    ctx,
    "POST",
    `/api/datasources/proxy/uid/${uid}/select/logsql/field_values`,
    { form: { query: "*", field: args.field } },
  );
  return { values: (body.values ?? []).map((entry: { value: string }) => entry.value) };
}

/** Currently active alerts, with labels, annotations, state and the time they became active. */
export async function get_alerts(args: {}, ctx: Ctx): Promise<{ alerts: Alert[] }> {
  const body = await json(ctx, "GET", "/api/prometheus/grafana/api/v1/alerts");
  return { alerts: body.data?.alerts ?? [] };
}

/** All alert rule groups with their rules (state, health, evaluation interval, labels). */
export async function get_alert_rules(
  args: {},
  ctx: Ctx,
): Promise<{ groups: AlertRuleGroup[] }> {
  const body = await json(ctx, "GET", "/api/prometheus/grafana/api/v1/rules");
  return { groups: body.data?.groups ?? [] };
}

/**
 * Annotations, optionally for one dashboard (`dashboard_uid`) and a `from`/`to` range in epoch
 * milliseconds; `limit` defaults to 100.
 */
export async function list_annotations(
  args: { dashboard_uid?: string; from?: number; to?: number; limit?: number },
  ctx: Ctx,
): Promise<{ annotations: Annotation[] }> {
  const body = await json(ctx, "GET", "/api/annotations", {
    params: {
      limit: Math.max(1, args.limit ?? 100),
      dashboardUID: args.dashboard_uid,
      from: args.from,
      to: args.to,
    },
  });
  return { annotations: body };
}

/** Whether Grafana answers, and its version, commit and database status. */
export async function health(args: {}, ctx: Ctx): Promise<Health> {
  return json(ctx, "GET", "/api/health");
}

/** Slack archive, Slack-client and `C123:1700000000.123456` thread inputs. */
function parseThreadKey(value: string): string {
  const candidate = value.trim();
  if (!candidate) throw new Error("thread input is required");

  const url = /^[a-zA-Z][\w+.-]*:\/\/([^/?#]*)([^?#]*)(?:\?([^#]*))?/.exec(candidate);
  if (url) {
    const path = url[2] ?? "";
    const archive = /\/archives\/([A-Z0-9]+)\/p(\d{16})/.exec(path);
    if (archive) return `${archive[1]}:${archive[2].slice(0, 10)}.${archive[2].slice(10)}`;
    const client = /\/client\/(T[A-Z0-9]+)\/([A-Z0-9]+)\/thread\/\2-(\d+\.\d+)/.exec(path);
    if (client) return `${client[2]}:${client[3]}`;
    const fields = (url[3] ?? "").split("&");
    const field = (name: string): string =>
      fields.find((part) => part.startsWith(`${name}=`))?.slice(name.length + 1) ?? "";
    const channel = field("cid");
    const ts = field("thread_ts");
    if (channel && ts) return `${channel}:${ts}`;
  }

  if (candidate.startsWith("slack:")) {
    const parts = candidate.split(":");
    if (parts.length === 3 && parts[1] && parts[2]) return `${parts[1]}:${parts[2]}`;
  }
  if (candidate.split(":").length === 2) {
    const [channel, ts] = candidate.split(":");
    if (channel && ts) return `${channel}:${ts}`;
  }
  throw new Error(`cannot parse Slack thread input ${JSON.stringify(value)}`);
}

/**
 * Build a Grafana thread-debugger deep link (no request) from a Slack thread: a message permalink
 * `https://…/archives/C123/p1700000000123456`, a Slack-client thread URL, `slack:C123:1700000000.123456`
 * or `C123:1700000000.123456`. Returns the normalized thread key and the dashboard URL with
 * `var-thread_key` plus the `from`/`to` time range.
 */
export async function thread_debug_url(
  args: {
    thread: string;
    dashboard_uid?: string;
    from?: string;
    to?: string;
  },
  ctx: Ctx,
): Promise<ThreadDebugUrl> {
  const threadKey = parseThreadKey(args.thread);
  const dashboardUid = args.dashboard_uid ?? "thread-debugger";
  const from = args.from ?? "now-24h";
  const to = args.to ?? "now";
  const params = `var-thread_key=${encodeURIComponent(threadKey)}&from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`;
  return {
    thread_input: args.thread,
    thread_key: threadKey,
    dashboard_uid: dashboardUid,
    from,
    to,
    url: `${publicUrl(ctx)}/d/${encodeURIComponent(dashboardUid)}/${encodeURIComponent(dashboardUid)}?${params}`,
  };
}
