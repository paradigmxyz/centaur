import type { Ctx } from "../centaur";
import { query as queryString, readJson } from "../_lib/http";

// Responses are VictoriaMetrics' Prometheus-compatible payloads, passed through:
// `{status, data: {resultType, result}}`, with `result` still holding
// `{metric, value}` or `{metric, values}` entries as the API sends them.

const DEFAULT_URL = "http://victoriametrics:8428";

/** A metric's label set, including `__name__`. */
export interface MetricLabels {
  __name__?: string;
  [label: string]: string | undefined;
}

/** One instant-query sample: `value` is `[unix seconds, value as a string]`. */
export interface VectorSample {
  metric: MetricLabels;
  value: [number, string];
}

/** One range-query series: `values` is a list of `[unix seconds, value as a string]`. */
export interface MatrixSeries {
  metric: MetricLabels;
  values: [number, string][];
}

/**
 * A Prometheus-compatible query response. `data.resultType` says how to read `data.result`:
 * `vector` is `VectorSample[]`, `matrix` is `MatrixSeries[]`, `scalar` is a single
 * `[unix seconds, value]` pair and `string` is a single `[unix seconds, value]` pair too.
 */
export interface QueryResponse {
  status: string;
  data: {
    resultType: string;
    result: unknown;
    [field: string]: unknown;
  };
  warnings?: string[];
  [field: string]: unknown;
}

/** The checks behind `health`. */
export interface HealthDetails {
  ready: boolean;
  query_ok: boolean;
  metric_names_count: number;
  series_count: number;
  query_result_count?: number;
  [field: string]: unknown;
}

/** `VICTORIAMETRICS_URL` (a URL or host) as a base URL. */
function baseUrl(ctx: Ctx): string {
  let url = (ctx.env.VICTORIAMETRICS_URL || DEFAULT_URL).trim().replace(/\/+$/, "");
  if (url && !/^https?:\/\//.test(url)) url = `http://${url}`;
  return url;
}

async function request(
  ctx: Ctx,
  path: string,
  params: Record<string, string | number | undefined>,
  label: string,
): Promise<any> {
  const res = await ctx.fetch(`${baseUrl(ctx)}${path}${queryString(params)}`, {
    headers: { accept: "application/json" },
  });
  const body = await readJson(res, label);
  // The API can answer 200 with `{status: "error", error}`.
  if (body?.status === "error") throw new Error(`${label}: ${body.error ?? JSON.stringify(body)}`);
  return body;
}

/**
 * Instant PromQL/MetricsQL query, evaluated now or at `time` (RFC3339 or Unix seconds).
 * Returns the API response unchanged: `data.resultType` is `vector`/`scalar`/`string` and
 * `data.result` holds `{metric: {__name__, ...labels}, value: [unix seconds, "value"]}` entries
 * (`scalar`/`string` hold one `[unix seconds, "value"]` pair instead). Examples:
 * `sum(rate(http_requests_total[5m])) by (service)`, `count({__name__=~".+"})`.
 */
export async function query(
  args: { expr: string; time?: string },
  ctx: Ctx,
): Promise<QueryResponse> {
  return request(ctx, "/api/v1/query", { query: args.expr, time: args.time }, "VictoriaMetrics query");
}

/**
 * Range PromQL/MetricsQL query over `start`..`end` (RFC3339 or Unix seconds; `end` defaults to
 * now) at `step` (such as `15s`, `1m`, `1h`; default `60s`). Returns `data.resultType: "matrix"`
 * with `data.result` as `{metric, values: [[unix seconds, "value"], ...]}` series. Keep the point
 * count low (a day at `1m` is 1440 points per series).
 */
export async function query_range(
  args: { expr: string; start: string; end?: string; step?: string },
  ctx: Ctx,
): Promise<QueryResponse> {
  return request(
    ctx,
    "/api/v1/query_range",
    { query: args.expr, start: args.start, end: args.end, step: args.step ?? "60s" },
    "VictoriaMetrics range query",
  );
}

/**
 * Time series matching a Prometheus selector such as `{__name__=~"agent_.*"}` or
 * `{service="api"}`, optionally within `start`..`end` (RFC3339 or Unix seconds). Each result is
 * that series' full label set. `limit` keeps only the first N label sets client-side; raise the
 * selector's specificity instead of paging — the API has no cursor.
 */
export async function series(
  args: { match: string; start?: string; end?: string; limit?: number },
  ctx: Ctx,
): Promise<{ series: MetricLabels[] }> {
  if (args.limit !== undefined && (!Number.isInteger(args.limit) || args.limit < 0)) {
    throw new Error("limit must be a non-negative integer");
  }
  const body = await request(
    ctx,
    "/api/v1/series",
    { "match[]": args.match, start: args.start, end: args.end },
    "VictoriaMetrics series query",
  );
  const all: MetricLabels[] = body.data ?? [];
  return { series: args.limit === undefined ? all : all.slice(0, args.limit) };
}

/**
 * All values of one label, sorted as VictoriaMetrics returns them. `__name__` lists every metric
 * name, `service`/`job`/`instance`/`namespace` list the values in use.
 */
export async function label_values(args: { label: string }, ctx: Ctx): Promise<{ values: string[] }> {
  const path = `/api/v1/label/${encodeURIComponent(args.label)}/values`;
  const body = await request(ctx, path, {}, "VictoriaMetrics label values");
  return { values: body.data ?? [] };
}

/**
 * Metric names, from `__name__` label values. `prefix` keeps names that start with it (default
 * `agent_`; pass `""` for all names). Use it to discover what a deployment exports before
 * querying.
 */
export async function metric_names(
  args: { prefix?: string },
  ctx: Ctx,
): Promise<{ metrics: string[] }> {
  const prefix = args.prefix ?? "agent_";
  const { values } = await label_values({ label: "__name__" }, ctx);
  return { metrics: prefix ? values.filter((name) => name.startsWith(prefix)) : values };
}

/** Whether `GET /health` answers 200. */
export async function ready(args: {}, ctx: Ctx): Promise<{ ready: boolean }> {
  try {
    const res = await ctx.fetch(`${baseUrl(ctx)}/health`);
    return { ready: res.status === 200 };
  } catch {
    return { ready: false };
  }
}

/**
 * Readiness plus a working check: `/health`, one instant query (`count({__name__=~".+"})`),
 * the metric-name count and one bounded series selector. Returns `ok` and, when a check failed,
 * `error`; `details` keeps the individual results.
 */
export async function health(
  args: {},
  ctx: Ctx,
): Promise<{ ok: boolean; error: string | null; details: HealthDetails }> {
  const details: HealthDetails = { ready: false, query_ok: false, metric_names_count: 0, series_count: 0 };
  try {
    const res = await ctx.fetch(`${baseUrl(ctx)}/health`);
    details.ready = res.status === 200;
    if (!details.ready) {
      return { ok: false, error: `VictoriaMetrics health returned HTTP ${res.status}`, details };
    }
    const result = await query({ expr: 'count({__name__=~".+"})' }, ctx);
    details.query_ok = result.status === "success";
    const data = result.data.result;
    details.query_result_count = Array.isArray(data) ? data.length : 0;

    const { metrics } = await metric_names({ prefix: "" }, ctx);
    details.metric_names_count = metrics.length;

    const { series: up } = await series({ match: '{__name__="up"}', limit: 1 }, ctx);
    details.series_count = up.length;

    const ok = Boolean(details.query_ok && details.metric_names_count && details.series_count);
    return {
      ok,
      error: ok ? null : "VictoriaMetrics checks returned no query, metric, or series data",
      details,
    };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error), details };
  }
}
