import type { Ctx } from "../centaur";

// Laminar's read-only SQL endpoint over the `traces` and `spans` tables, as the
// Centaur platform exports them. Results are the endpoint's own rows, passed
// through: `attributes` (a JSON string in the row) is parsed into an object so
// its keys are usable, and nothing is renamed or truncated.

const DEFAULT_BASE_URL = "http://laminar-app-server.laminar.svc.cluster.local:8000";
const DEFAULT_EXTERNAL_URL = "http://prd-centaur-na-laminar.tail388b2e.ts.net";
const DEFAULT_PROJECT_ID = "202e8d91-1311-40f8-9217-ad375d3ab4df";
const MAX_LIMIT = 500;
const MAX_MINUTES = 60 * 24 * 14;

/** One row of `recent_traces`. */
export interface Trace {
  trace_id: string;
  start_time?: string;
  end_time?: string;
  duration?: number | string;
  status?: string;
  top_span_name?: string;
  session_id?: string;
  user_id?: string;
  total_tokens?: number;
  total_cost?: number | string;
  [field: string]: unknown;
}

/** One row of `find_traces`: a trace with the Centaur identifiers its spans carry. */
export interface FoundTrace {
  trace_id: string;
  start_time?: string;
  end_time?: string;
  duration_ms?: number | string;
  span_count?: number;
  /** Distinct span names in the trace. */
  span_names?: string[];
  /** Distinct `service.name` attribute values. */
  services?: string[];
  thread_key?: string;
  execution_id?: string;
  session_id?: string;
  [field: string]: unknown;
}

/** One span of `get_trace`. */
export interface Span {
  span_id?: string;
  parent_span_id?: string | null;
  name?: string;
  span_type?: string;
  start_time?: string;
  end_time?: string;
  duration_ms?: number | string;
  status?: string;
  model?: string;
  provider?: string;
  input_tokens?: number;
  output_tokens?: number;
  total_tokens?: number;
  total_cost?: number | string;
  /** The span's attributes, parsed from Laminar's JSON string. */
  attributes?: Record<string, unknown>;
  /** The span input, only with `include_io`. */
  input?: string;
  /** The span output, only with `include_io`. */
  output?: string;
  [field: string]: unknown;
}

/** One span of `errors` or `diagnose_thread`: a failed span with its Centaur identifiers. */
export interface ErrorSpan {
  trace_id?: string;
  span_id?: string;
  name?: string;
  start_time?: string;
  status?: string;
  thread_key?: string;
  execution_id?: string;
  service_name?: string;
  exception_message?: string;
  http_status?: string;
  [field: string]: unknown;
}

/** A slow span: the slowest spans of a thread, longest first. */
export interface SlowSpan {
  trace_id?: string;
  span_id?: string;
  name?: string;
  start_time?: string;
  duration_ms?: number | string;
  status?: string;
  [field: string]: unknown;
}

/** `diagnose_thread`: recent traces, failures and slowest spans for one Centaur thread. */
export interface ThreadDiagnosis {
  thread_key: string;
  minutes: number;
  trace_count: number;
  traces: FoundTrace[];
  error_count: number;
  errors: ErrorSpan[];
  slow_spans: SlowSpan[];
}

/** `health`: whether the app-server answers, and which endpoint/project is configured. */
export interface LaminarHealth {
  ready: boolean;
  base_url: string;
  external_url: string;
  project_id: string;
  auth_configured: boolean;
  /** The health response body, or the connection error. */
  response: string;
}

function config(ctx: Ctx): {
  baseUrl: string;
  externalUrl: string;
  apiKey: string;
  projectId: string;
} {
  const rawBase = (ctx.env.LAMINAR_BASE_URL || DEFAULT_BASE_URL).trim().replace(/\/+$/, "");
  const rawExternal = (ctx.env.LAMINAR_EXTERNAL_URL || DEFAULT_EXTERNAL_URL).trim().replace(/\/+$/, "");
  return {
    baseUrl: /^https?:\/\//.test(rawBase) ? rawBase : `http://${rawBase}`,
    externalUrl: rawExternal,
    apiKey: (ctx.env.LAMINAR_API_KEY || "").trim(),
    projectId: (ctx.env.LAMINAR_PROJECT_ID || DEFAULT_PROJECT_ID).trim(),
  };
}

function clamp(value: number | undefined, fallback: number, minimum: number, maximum: number): number {
  const number = Math.trunc(value ?? fallback);
  return Math.max(minimum, Math.min(maximum, number));
}

/** Laminar returns `attributes` as a JSON string; parse it when it is one. */
function parseAttributes(value: unknown): Record<string, unknown> {
  if (typeof value === "object" && value !== null) return value as Record<string, unknown>;
  if (typeof value !== "string" || !value) return {};
  try {
    const parsed = JSON.parse(value) as unknown;
    return typeof parsed === "object" && parsed !== null ? (parsed as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function headers(ctx: Ctx): Record<string, string> {
  const { apiKey } = config(ctx);
  return apiKey ? { accept: "application/json", authorization: `Bearer ${apiKey}` } : { accept: "application/json" };
}

async function queryRows(
  ctx: Ctx,
  sql: string,
  parameters: Record<string, unknown> = {},
): Promise<Record<string, unknown>[]> {
  const { baseUrl, projectId, apiKey } = config(ctx);
  const path = projectId ? `/api/v1/projects/${encodeURIComponent(projectId)}/sql/query` : "/v1/sql/query";
  if (!projectId && !apiKey) {
    throw new Error("set LAMINAR_PROJECT_ID or LAMINAR_API_KEY to query Laminar traces");
  }
  const res = await ctx.fetch(`${baseUrl}${path}`, {
    method: "POST",
    headers: { ...headers(ctx), "content-type": "application/json" },
    body: JSON.stringify({ query: sql, parameters }),
  });
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`Laminar SQL API error (HTTP ${res.status}): ${text.slice(0, 1000)}`);
  }
  const payload = text ? JSON.parse(text) : {};
  if (Array.isArray(payload)) return payload;
  const rows = payload?.data ?? payload;
  if (!Array.isArray(rows)) {
    throw new Error(`unexpected Laminar SQL response: ${text.slice(0, 500)}`);
  }
  return rows;
}

/**
 * Whether the configured Laminar app-server answers, plus the base URL, external URL, project ID
 * and whether an API key is configured. Never throws: a failed request is reported as
 * `ready: false` with the error in `response`.
 */
export async function health(args: {}, ctx: Ctx): Promise<LaminarHealth> {
  const { baseUrl, externalUrl, projectId, apiKey } = config(ctx);
  const report = (ready: boolean, body: string): LaminarHealth => ({
    ready,
    base_url: baseUrl,
    external_url: externalUrl,
    project_id: projectId,
    auth_configured: Boolean(apiKey),
    response: body.slice(0, 200),
  });
  try {
    const res = await ctx.fetch(`${baseUrl}/health`, { headers: headers(ctx) });
    return report(res.status === 200, await res.text());
  } catch (error) {
    return report(false, String((error as Error).message ?? error));
  }
}

/**
 * Run a read-only Laminar SQL query over the `traces` and `spans` tables and return its rows.
 * Laminar validates the query and scopes it to LAMINAR_PROJECT_ID (or to LAMINAR_API_KEY when no
 * project id is set). Use `{name:Type}` placeholders in `sql` with `parameters`, for example
 * `SELECT * FROM spans WHERE trace_id = {trace_id:UUID} LIMIT 10` and `{ trace_id: "…" }`.
 */
export async function query(
  args: { sql: string; parameters?: Record<string, unknown> },
  ctx: Ctx,
): Promise<{ rows: Record<string, unknown>[] }> {
  return { rows: await queryRows(ctx, args.sql, args.parameters ?? {}) };
}

/**
 * Recent traces, newest first: trace id, start/end, duration, status, top span name, session and
 * user id, total tokens and cost. `minutes` is the look-back window (1-20160, default 60) and
 * `limit` the number of traces (1-500, default 20).
 */
export async function recent_traces(
  args: { minutes?: number; limit?: number },
  ctx: Ctx,
): Promise<{ traces: Trace[] }> {
  const minutes = clamp(args.minutes, 60, 1, MAX_MINUTES);
  const limit = clamp(args.limit, 20, 1, MAX_LIMIT);
  const rows = await queryRows(
    ctx,
    `
    SELECT
        id AS trace_id,
        start_time,
        end_time,
        duration,
        status,
        top_span_name,
        session_id,
        user_id,
        total_tokens,
        total_cost
    FROM traces
    WHERE start_time >= now() - INTERVAL ${minutes} MINUTE
    ORDER BY start_time DESC
    LIMIT ${limit}
    `,
  );
  return { traces: rows as unknown as Trace[] };
}

/**
 * Traces that carry a Centaur `thread_key`, `execution_id` or Laminar `session_id` (at least one
 * is required), newest first, with their span count, span names, services and duration.
 * `minutes` is the look-back window (1-20160, default 1440) and `limit` the number of traces
 * (1-500, default 20).
 */
export async function find_traces(
  args: {
    thread_key?: string;
    execution_id?: string;
    session_id?: string;
    minutes?: number;
    limit?: number;
  },
  ctx: Ctx,
): Promise<{ traces: FoundTrace[] }> {
  return { traces: await findTraces(ctx, args) };
}

async function findTraces(
  ctx: Ctx,
  args: { thread_key?: string; execution_id?: string; session_id?: string; minutes?: number; limit?: number },
): Promise<FoundTrace[]> {
  if (!args.thread_key && !args.execution_id && !args.session_id) {
    throw new Error("provide at least one of thread_key, execution_id or session_id");
  }
  const minutes = clamp(args.minutes, 60 * 24, 1, MAX_MINUTES);
  const limit = clamp(args.limit, 20, 1, MAX_LIMIT);
  const filters = [`s.start_time >= now() - INTERVAL ${minutes} MINUTE`];
  const parameters: Record<string, unknown> = {};
  if (args.thread_key) {
    filters.push("JSONExtractString(s.attributes, 'centaur.thread_key') = {thread_key:String}");
    parameters.thread_key = args.thread_key;
  }
  if (args.execution_id) {
    filters.push("JSONExtractString(s.attributes, 'centaur.execution_id') = {execution_id:String}");
    parameters.execution_id = args.execution_id;
  }
  if (args.session_id) {
    filters.push("t.session_id = {session_id:String}");
    parameters.session_id = args.session_id;
  }
  const rows = await queryRows(
    ctx,
    `
    SELECT
        s.trace_id,
        min(s.start_time) AS start_time,
        max(s.end_time) AS end_time,
        dateDiff('millisecond', min(s.start_time), max(s.end_time)) AS duration_ms,
        count() AS span_count,
        groupUniqArray(s.name) AS span_names,
        groupUniqArray(JSONExtractString(s.attributes, 'service.name')) AS services,
        any(JSONExtractString(s.attributes, 'centaur.thread_key')) AS thread_key,
        any(JSONExtractString(s.attributes, 'centaur.execution_id')) AS execution_id,
        any(t.session_id) AS session_id
    FROM spans AS s
    LEFT JOIN traces AS t ON s.trace_id = t.id
    WHERE ${filters.join(" AND ")}
    GROUP BY s.trace_id
    ORDER BY start_time DESC
    LIMIT ${limit}
    `,
    parameters,
  );
  return rows as unknown as FoundTrace[];
}

/**
 * Span-level detail for one trace ID (a UUID), oldest span first: name, type, timing, status,
 * model and provider, token and cost totals, and the span's attributes. Set `include_io` to add
 * each span's raw `input` and `output`. `limit` caps the spans (1-500, default 300). Includes
 * `trace_url` for the Laminar UI.
 */
export async function get_trace(
  args: { trace_id: string; include_io?: boolean; limit?: number },
  ctx: Ctx,
): Promise<{ trace_id: string; span_count: number; trace_url: string; spans: Span[] }> {
  const limit = clamp(args.limit, 300, 1, MAX_LIMIT);
  const rows = await queryRows(
    ctx,
    `
    SELECT
        span_id,
        parent_span_id,
        name,
        span_type,
        start_time,
        end_time,
        dateDiff('millisecond', start_time, end_time) AS duration_ms,
        status,
        model,
        provider,
        input_tokens,
        output_tokens,
        total_tokens,
        total_cost,
        attributes,
        input,
        output
    FROM spans
    WHERE trace_id = {trace_id:UUID}
    ORDER BY start_time ASC
    LIMIT ${limit}
    `,
    { trace_id: args.trace_id },
  );
  const spans: Span[] = [];
  for (const row of rows) {
    const { input, output, ...rest } = row;
    const span: Span = { ...rest, attributes: parseAttributes(row.attributes) };
    if (args.include_io) {
      span.input = input === null || input === undefined ? "" : String(input);
      span.output = output === null || output === undefined ? "" : String(output);
    }
    spans.push(span);
  }
  return {
    trace_id: args.trace_id,
    span_count: spans.length,
    trace_url: `${config(ctx).externalUrl}/traces/${args.trace_id}`,
    spans,
  };
}

/**
 * Everything worth seeing for one Centaur thread key: its recent traces, the failed spans
 * (error status, an exception message or a 5xx HTTP status) and the slowest spans, within
 * `minutes` (1-20160, default 1440). `trace_limit` and `slow_span_limit` are 1-100.
 */
export async function diagnose_thread(
  args: { thread_key: string; minutes?: number; trace_limit?: number; slow_span_limit?: number },
  ctx: Ctx,
): Promise<ThreadDiagnosis> {
  const minutes = clamp(args.minutes, 60 * 24, 1, MAX_MINUTES);
  const traceLimit = clamp(args.trace_limit, 10, 1, 100);
  const slowSpanLimit = clamp(args.slow_span_limit, 20, 1, 100);
  const traces = await findTraces(ctx, {
    thread_key: args.thread_key,
    minutes,
    limit: traceLimit,
  });
  const errors = await queryRows(
    ctx,
    `
    SELECT
        trace_id,
        span_id,
        name,
        start_time,
        status,
        JSONExtractString(attributes, 'exception.message') AS exception_message,
        JSONExtractString(attributes, 'http.response.status_code') AS http_status
    FROM spans
    WHERE start_time >= now() - INTERVAL ${minutes} MINUTE
      AND JSONExtractString(attributes, 'centaur.thread_key') = {thread_key:String}
      AND (
        lower(status) IN ('error', 'failed')
        OR JSONExtractString(attributes, 'exception.message') != ''
        OR toInt32OrZero(JSONExtractString(attributes, 'http.response.status_code')) >= 500
      )
    ORDER BY start_time DESC
    LIMIT 100
    `,
    { thread_key: args.thread_key },
  );
  const slowSpans = await queryRows(
    ctx,
    `
    SELECT
        trace_id,
        span_id,
        name,
        start_time,
        dateDiff('millisecond', start_time, end_time) AS duration_ms,
        status
    FROM spans
    WHERE start_time >= now() - INTERVAL ${minutes} MINUTE
      AND JSONExtractString(attributes, 'centaur.thread_key') = {thread_key:String}
    ORDER BY duration_ms DESC
    LIMIT ${slowSpanLimit}
    `,
    { thread_key: args.thread_key },
  );
  const errorSpans = errors as unknown as ErrorSpan[];
  return {
    thread_key: args.thread_key,
    minutes,
    trace_count: traces.length,
    traces,
    error_count: errorSpans.length,
    errors: errorSpans,
    slow_spans: slowSpans as unknown as SlowSpan[],
  };
}

/**
 * Recent failed spans across the whole project, newest first: error status, an exception message or
 * a 5xx HTTP status, with their thread key, execution id, service and exception message.
 * `minutes` is the look-back window (1-20160, default 60), `limit` the number of spans (1-500).
 */
export async function errors(
  args: { minutes?: number; limit?: number },
  ctx: Ctx,
): Promise<{ errors: ErrorSpan[] }> {
  const minutes = clamp(args.minutes, 60, 1, MAX_MINUTES);
  const limit = clamp(args.limit, 50, 1, MAX_LIMIT);
  const rows = await queryRows(
    ctx,
    `
    SELECT
        trace_id,
        span_id,
        name,
        start_time,
        status,
        JSONExtractString(attributes, 'centaur.thread_key') AS thread_key,
        JSONExtractString(attributes, 'centaur.execution_id') AS execution_id,
        JSONExtractString(attributes, 'service.name') AS service_name,
        JSONExtractString(attributes, 'exception.message') AS exception_message,
        JSONExtractString(attributes, 'http.response.status_code') AS http_status
    FROM spans
    WHERE start_time >= now() - INTERVAL ${minutes} MINUTE
      AND (
        lower(status) IN ('error', 'failed')
        OR JSONExtractString(attributes, 'exception.message') != ''
        OR toInt32OrZero(JSONExtractString(attributes, 'http.response.status_code')) >= 500
      )
    ORDER BY start_time DESC
    LIMIT ${limit}
    `,
  );
  return { errors: rows as unknown as ErrorSpan[] };
}
