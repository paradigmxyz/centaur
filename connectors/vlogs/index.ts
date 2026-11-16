import type { Ctx } from "../centaur";
import { encode, query as queryString } from "../_lib/http";

// Log entries are VictoriaLogs' own fields, passed through: `_time`, `_msg`,
// `_stream` and whatever the stream's log lines carry. The analytics helpers
// aggregate those entries; use `query` for anything they don't cover.

const DEFAULT_URL = "http://victorialogs:9428";
// `1h`, `30m`, `7d`: a relative range LogsQL spells inside the query.
const DURATION_RE = /^\d+[smhdw]$/;
const DURATION_SECONDS: Record<string, number> = { s: 1, m: 60, h: 3600, d: 86400, w: 604800 };

/** A log entry: `_time` (RFC3339), `_msg`, and the stream's fields. */
export interface LogEntry {
  _time: string;
  _msg: string;
  _stream?: string;
  _stream_id?: string;
  [field: string]: unknown;
}

/** One series of `/select/logsql/hits`: bucket timestamps and counts, or a single `total`. */
export interface HitSeries {
  fields?: Record<string, string>;
  timestamps?: string[];
  values?: number[];
  total?: number;
  [field: string]: unknown;
}

/** The `/select/logsql/hits` payload. */
export interface HitsResponse {
  hits?: HitSeries[];
  [field: string]: unknown;
}

/** One log stream and its hit count. */
export interface Stream {
  value: string;
  hits: number;
  [field: string]: unknown;
}

/** A `tool_call_completed` entry as `tool_calls` projects it. */
export interface ToolCall {
  _time?: string;
  tool_name?: string;
  tool_method?: string;
  duration_ms?: string | number;
  success?: string | boolean;
  tool_args?: unknown;
  tool_args_count?: string | number;
  tool_args_truncated?: string | boolean;
  thread_key?: string;
  [field: string]: unknown;
}

/** Per-tool usage, from `tool_analytics`. */
export interface ToolStats {
  tool: string;
  calls: number;
  failures: number;
  failure_rate_pct: number;
  avg_duration_ms: number;
  unique_threads: number;
  /** How often each argument label was used. */
  args: Record<string, number>;
  methods: Record<string, number>;
}

/** Tool calls per thread, from `tool_usage_by_thread` without a `thread_key`. */
export interface ThreadToolUsage {
  thread_key: string;
  tool_calls: number;
}

/** An `execution_summary` entry as `execution_summaries` projects it. */
export interface ExecutionSummary {
  _time?: string;
  execution_id?: string;
  thread_key?: string;
  harness?: string;
  engine?: string;
  persona_id?: string;
  prompt_ref?: string;
  prompt_sha?: string;
  status?: string;
  terminal_reason?: string;
  duration_s?: string | number;
  ttft_ms?: string | number;
  execution_sequence?: string | number;
  user_id?: string;
  total_tokens?: string | number;
  cost_usd?: string | number;
  models?: string[] | string;
  [field: string]: unknown;
}

/** Outcomes per prompt lineage, from `prompt_analytics`. */
export interface PromptStats {
  prompt_ref: string;
  prompt_sha: string;
  runs: number;
  completed: number;
  failed: number;
  success_rate_pct: number;
  avg_duration_s: number;
  avg_tokens: number;
  avg_cost_usd: number;
  models: string[];
  harnesses: string[];
  terminal_reasons: Record<string, number>;
}

/** Token and cost totals per harness/model, from `model_analytics`. */
export interface ModelStats {
  harness: string;
  model: string;
  calls: number;
  input_tokens: number;
  output_tokens: number;
  cache_creation_input_tokens: number;
  cache_read_input_tokens: number;
  total_tokens: number;
  cost_usd: number;
  avg_cost_usd: number;
}

/** Error and total counts for one service, from `service_health`. */
export interface ServiceHealth {
  total_count: number;
  error_count: number;
}

/** `VICTORIALOGS_URL` (a URL or host) as a base URL. */
function baseUrl(ctx: Ctx): string {
  let url = (ctx.env.VICTORIALOGS_URL || DEFAULT_URL).trim().replace(/\/+$/, "");
  if (url && !/^https?:\/\//.test(url)) url = `http://${url}`;
  return url;
}

async function post(
  ctx: Ctx,
  path: string,
  form: Record<string, string | number | undefined>,
): Promise<string> {
  const res = await ctx.fetch(`${baseUrl(ctx)}${path}`, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body: encode(form),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`VictoriaLogs API error (${res.status}): ${text.slice(0, 1000)}`);
  return text;
}

async function postJson(
  ctx: Ctx,
  path: string,
  form: Record<string, string | number | undefined>,
): Promise<any> {
  return JSON.parse(await post(ctx, path, form));
}

/** `/select/logsql/query` answers newline-delimited JSON. */
async function select(
  ctx: Ctx,
  q: string,
  limit: number,
  start?: string,
  end?: string,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const max = Math.trunc(limit);
  if (!Number.isFinite(max) || max < 1) throw new Error("limit must be a positive integer");
  const text = await post(ctx, "/select/logsql/query", {
    query: `${q} | limit ${max}`,
    ...timeParams(start),
    end,
  });
  const entries: LogEntry[] = [];
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (trimmed) entries.push(JSON.parse(trimmed));
  }
  return { entries, truncated: entries.length >= max };
}

/** Quote a LogsQL field value (thread keys contain `:` and `=`). */
function quoteValue(value: string): string {
  return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

function fieldExpr(field: string, value: string): string {
  return `${field}:${quoteValue(value)}`;
}

/** `_time:1h ` for a relative range; absolute ranges go in the `start` form field instead. */
function timePrefix(start?: string): string {
  return start && DURATION_RE.test(start) ? `_time:${start} ` : "";
}

function timeParams(start?: string): { start?: string } {
  return start && !DURATION_RE.test(start) ? { start } : {};
}

function clean(entry: LogEntry): LogEntry {
  const { _stream_id, _stream, ...rest } = entry;
  return rest as LogEntry;
}

function cleaned(entries: LogEntry[]): LogEntry[] {
  return entries.filter((entry) => !("_note" in entry)).map(clean);
}

function toNumber(value: unknown): number {
  if (typeof value === "number") return Number.isFinite(value) ? value : 0;
  if (typeof value === "string") {
    const parsed = Number.parseFloat(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  return 0;
}

/** A stable label for a tool call's arguments, used to group calls. */
function formatToolArgs(value: unknown, present: boolean): string {
  if (!present) return "(not captured)";
  if (value === null || value === undefined) return "(no args)";
  if (Array.isArray(value)) {
    if (!value.length) return "(no args)";
    return value
      .map((item) => {
        const text = typeof item === "string" ? item : JSON.stringify(item);
        return /[\s"']/.test(text) ? `'${text.replace(/'/g, `'\\''`)}'` : text;
      })
      .join(" ");
  }
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>).sort(([a], [b]) => a.localeCompare(b));
    if (!entries.length) return "(no args)";
    return JSON.stringify(Object.fromEntries(entries));
  }
  const text = String(value);
  return text || "(no args)";
}

/** A reasonable bucket size for `/select/logsql/hits`, from the relative range. */
function hitsStep(start?: string): string {
  if (!start || !DURATION_RE.test(start)) return "1h";
  const seconds = Number.parseInt(start.slice(0, -1), 10) * (DURATION_SECONDS[start.slice(-1)] ?? 1);
  if (seconds <= 3600) return "1m";
  if (seconds <= 6 * 3600) return "5m";
  if (seconds <= 86400) return "1h";
  if (seconds <= 7 * 86400) return "1d";
  return "1w";
}

/** Victorialogs reports counts as numbers or numeric strings; `total` may be absent. */
function hitsTotal(payload: HitsResponse): number {
  let total = 0;
  for (const series of payload.hits ?? []) {
    if (!series || typeof series !== "object") continue;
    if ("total" in series) {
      total += Math.trunc(toNumber(series.total));
      continue;
    }
    total += (series.values ?? []).reduce((sum, value) => sum + Math.trunc(toNumber(value)), 0);
  }
  return total;
}

/**
 * Run a LogsQL query and return matching log entries oldest first, each with `_time` (RFC3339),
 * `_msg` and the entry's fields. `query` is the LogsQL expression, for example
 * `_time:1h level:error` or `_stream:{service="api"} AND duration_ms:>500`. `limit` caps entries
 * (default 100) and `truncated` is true when the cap was hit; `start`/`end` are an absolute
 * RFC3339 or Unix range, or `start` may be relative (`1h`, `7d`), which is folded into the query.
 */
export async function query(
  args: { query: string; limit?: number; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const limit = args.limit ?? 100;
  const prefix = timePrefix(args.start);
  return select(ctx, `${prefix}${args.query}`, limit, args.start, args.end);
}

/**
 * Log counts over time for a LogsQL `query` (no `limit`; counts are aggregated). Returns
 * VictoriaLogs' `hits` payload: one series with `fields` (the grouping labels), `timestamps`,
 * `values` (counts per bucket) and `total`. `step` is the bucket size (`1m`, `1h`, `1d`, default
 * `1h`); `start`/`end` bound the range, `start` either relative (`24h`) or absolute.
 */
export async function hits(
  args: { query: string; start?: string; end?: string; step?: string },
  ctx: Ctx,
): Promise<HitsResponse> {
  return postJson(ctx, "/select/logsql/hits", {
    query: args.query,
    ...timeParams(args.start),
    end: args.end,
    step: args.step,
  });
}

/**
 * Field names present in the matching logs (default `query: "*"`), for discovering what a stream
 * carries before querying it. `start`/`end` optionally scope the search.
 */
export async function field_names(
  args: { query?: string; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ fields: string[] }> {
  const body = await postJson(ctx, "/select/logsql/field_names", {
    query: args.query ?? "*",
    ...timeParams(args.start),
    end: args.end,
  });
  return { fields: (body.values ?? []).map((value: { value: string }) => value.value) };
}

/**
 * Distinct values of one field (for example `service`, `container`, `thread_key`), optionally
 * scoped by a LogsQL `query` filter. `limit` caps values (default 100). Use it to find exact
 * service or stream labels before `_stream:{service="..."}` queries.
 */
export async function field_values(
  args: { field: string; query?: string; limit?: number; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ values: string[] }> {
  const body = await postJson(ctx, "/select/logsql/field_values", {
    query: args.query ?? "*",
    field: args.field,
    limit: args.limit ?? 100,
    ...timeParams(args.start),
    end: args.end,
  });
  return { values: (body.values ?? []).map((value: { value: string }) => value.value) };
}

/**
 * Log streams matching a LogsQL `query` (default `*`) with each stream's hit count: `value` is
 * the `{label="value",...}` stream selector. `start`/`end` optionally bound the range.
 */
export async function streams(
  args: { query?: string; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ streams: Stream[] }> {
  const body = await postJson(ctx, "/select/logsql/streams", {
    query: args.query ?? "*",
    ...timeParams(args.start),
    end: args.end,
  });
  return { streams: body.values ?? [] };
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
 * Every log line for one `thread_key` across all services, newest first, with `_stream` and
 * `_stream_id` stripped. `level` adds an exact log-level filter (`error`, `warn`); `start` is the
 * range start (relative such as `1h`, or RFC3339) and `end` the optional end; `limit` defaults to
 * 50. This is the first call when tracing one chat thread.
 */
export async function thread_logs(
  args: { thread_key: string; level?: string; limit?: number; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const parts = [fieldExpr("thread_key", args.thread_key)];
  if (args.level) parts.push(`level:${args.level}`);
  const q = `${timePrefix(args.start)}${parts.join(" AND ")}`;
  const result = await select(ctx, q, args.limit ?? 50, args.start, args.end);
  return { entries: cleaned(result.entries), truncated: result.truncated };
}

/**
 * The end-to-end lifecycle events of one thread across services, newest first: platform receipt,
 * session and execution state, sandbox spawn, turns and tool calls, and final delivery. `start`
 * is the range start (default `24h`, relative or RFC3339), `limit` defaults to 500.
 */
export async function thread_trace(
  args: { thread_key: string; start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const events = [
    "mention_received", "thread_history_backfilled", "assignment_ready", "spawn_completed",
    "message_buffered", "message_stored", "execute_start", "execute_queued", "execute_claimed",
    "execute_started", "sse_connect", "turn_start", "sandbox_spawned", "sandbox_attached",
    "sandbox_stdin_write", "turn_first_output", "assistant_tool_use_observed",
    "tool_call_started", "tool_call_completed", "tool_result_observed",
    "subagent_status_observed", "command_execution_observed", "assistant_text_observed",
    "usage_observed", "result_observed", "turn_done", "execution_summary", "execute_complete",
    "execute_completed", "final_delivery_ready", "final_delivery_claimed",
    "final_delivery_started", "final_delivery_delivered", "final_delivery_completed",
    "final_delivery_failed", "final_delivery_ack_failed", "final_delivery_post_failed",
    "final_delivery_suppressed", "sse_disconnect", "wire_reconnecting",
    "wire_reconnect_exhausted", "inflight_turn_replayed",
  ];
  const eventExpr = events.map((name) => `event:${name}`).join(" OR ");
  const q = `${timePrefix(args.start ?? "24h")}${fieldExpr("thread_key", args.thread_key)} AND (${eventExpr})`;
  const result = await select(ctx, q, args.limit ?? 500, args.start ?? "24h");
  return { entries: cleaned(result.entries), truncated: result.truncated };
}

/**
 * Error-level logs across services, newest first. `service` restricts to one service (exact
 * stream label), `thread_key` to one chat thread; `start` is the range start (default `1h`,
 * relative or RFC3339) and `limit` defaults to 50.
 */
export async function errors(
  args: { service?: string; thread_key?: string; start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const start = args.start ?? "1h";
  const parts = ["level:error"];
  if (args.service) parts.push(`_stream:{service="${args.service}"}`);
  if (args.thread_key) parts.push(fieldExpr("thread_key", args.thread_key));
  const result = await select(ctx, `${timePrefix(start)}${parts.join(" AND ")}`, args.limit ?? 50, start);
  return { entries: cleaned(result.entries), truncated: result.truncated };
}

/**
 * `http_request` logs slower than `threshold_ms` (default 5000 ms), slowest first, so entries
 * carry `path`, `method`, `status` and `duration_ms`. `start` is the range start (default `1h`)
 * and `limit` defaults to 50.
 */
export async function slow_requests(
  args: { threshold_ms?: number; start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const start = args.start ?? "1h";
  const threshold = Math.trunc(args.threshold_ms ?? 5000);
  const q = `${timePrefix(start)}event:http_request AND duration_ms:>${threshold}`;
  const result = await select(ctx, q, args.limit ?? 50, start);
  const entries = cleaned(result.entries).sort(
    (a, b) => toNumber(b.duration_ms) - toNumber(a.duration_ms),
  );
  return { entries, truncated: result.truncated };
}

/**
 * Completed tool calls, newest first, projected to `_time`, `tool_name`, `tool_method`,
 * `tool_args`, `tool_args_count`, `tool_args_truncated`, `duration_ms`, `success` and
 * `thread_key`. `tool_name` and `thread_key` filter exactly; `start` is the range start
 * (default `1h`) and `limit` defaults to 100.
 */
export async function tool_calls(
  args: { tool_name?: string; thread_key?: string; start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ entries: ToolCall[]; truncated: boolean }> {
  const start = args.start ?? "1h";
  const parts = ["event:tool_call_completed"];
  if (args.tool_name) parts.push(`tool_name:${args.tool_name}`);
  if (args.thread_key) parts.push(fieldExpr("thread_key", args.thread_key));
  const result = await select(ctx, `${timePrefix(start)}${parts.join(" AND ")}`, args.limit ?? 100, start);
  const keep = [
    "_time", "_msg", "duration_ms", "success", "tool_args", "tool_args_count",
    "tool_args_truncated", "tool_name", "tool_method", "thread_key",
  ];
  return { entries: cleaned(result.entries).map((entry) => project(entry, keep)), truncated: result.truncated };
}

function project<T extends Record<string, unknown>>(entry: LogEntry, keep: string[]): T {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(entry)) {
    if (keep.includes(key)) out[key] = value;
  }
  return out as unknown as T;
}

/**
 * Every log line carrying one `execution_id`, oldest first. Useful after finding the id in
 * `thread_trace`; at most 1000 entries are fetched.
 */
export async function execution_timeline(
  args: { execution_id: string },
  ctx: Ctx,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const result = await select(ctx, `execution_id:${args.execution_id}`, 1000);
  const entries = cleaned(result.entries).sort((a, b) =>
    String(a._time ?? "").localeCompare(String(b._time ?? "")),
  );
  return { entries, truncated: result.truncated };
}

/**
 * Per-service total and error counts over `start` (default `1h`), keyed by service name. One
 * `hits` request per service, so it costs a few round trips; use it for a fleet health check.
 */
export async function service_health(
  args: { start?: string },
  ctx: Ctx,
): Promise<{ services: Record<string, ServiceHealth> }> {
  const start = args.start ?? "1h";
  const prefix = timePrefix(start);
  const params = timeParams(start);
  const { values } = await field_values({ field: "service", query: `${prefix}*`, ...params }, ctx);
  const names = values.filter((name) => name.trim());
  const step = hitsStep(start);
  const counted = await Promise.all(
    names.map(async (name) => {
      const selector = `_stream:{service="${name}"}`;
      const total = hitsTotal(await hits({ query: selector, start, step }, ctx));
      const failed = hitsTotal(await hits({ query: `${selector} AND level:error`, start, step }, ctx));
      return [name, { total_count: total, error_count: failed }] as const;
    }),
  );
  return { services: Object.fromEntries(counted) };
}

/**
 * Sandbox container lifecycle events: everything from the `sandbox` service plus
 * `warm_container_claimed` and `sandbox_*` events. `start` is the range start (default `1h`),
 * `limit` defaults to 50.
 */
export async function sandbox_activity(
  args: { start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ entries: LogEntry[]; truncated: boolean }> {
  const start = args.start ?? "1h";
  const q =
    `${timePrefix(start)}_stream:{service="sandbox"} OR event:warm_container_claimed OR event:sandbox_*`;
  const result = await select(ctx, q, args.limit ?? 50, start);
  return { entries: cleaned(result.entries), truncated: result.truncated };
}

/**
 * Tool usage over `start` (default `24h`): call count, failures, failure rate, average duration,
 * distinct threads, and how often each argument label and method appeared, most used first.
 * `limit` caps tools (default 30). Fetches up to 10000 events first, so a long window costs a
 * large response.
 */
export async function tool_analytics(
  args: { start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ tools: ToolStats[] }> {
  const start = args.start ?? "24h";
  const result = await select(ctx, `${timePrefix(start)}event:tool_call_completed`, 10000, start);
  interface Acc {
    calls: number;
    failures: number;
    total_duration_ms: number;
    args: Map<string, number>;
    methods: Map<string, number>;
    threads: Set<string>;
  }
  const stats = new Map<string, Acc>();
  for (const entry of cleaned(result.entries)) {
    const tool = String(entry.tool_name ?? "unknown");
    const method = String(entry.tool_method ?? "unknown");
    const args = formatToolArgs(entry.tool_args, "tool_args" in entry);
    const success = String(entry.success ?? "true") === "true";
    const duration = Math.round(toNumber(entry.duration_ms));
    const thread = String(entry.thread_key ?? "");
    let acc = stats.get(tool);
    if (!acc) {
      acc = { calls: 0, failures: 0, total_duration_ms: 0, args: new Map(), methods: new Map(), threads: new Set() };
      stats.set(tool, acc);
    }
    acc.calls += 1;
    if (!success) acc.failures += 1;
    acc.total_duration_ms += duration;
    acc.args.set(args, (acc.args.get(args) ?? 0) + 1);
    acc.methods.set(method, (acc.methods.get(method) ?? 0) + 1);
    if (thread) acc.threads.add(thread);
  }
  const tools = [...stats.entries()]
    .sort(([, a], [, b]) => b.calls - a.calls)
    .slice(0, args.limit ?? 30)
    .map(([tool, acc]) => ({
      tool,
      calls: acc.calls,
      failures: acc.failures,
      failure_rate_pct: acc.calls ? Math.round((acc.failures / acc.calls) * 1000) / 10 : 0,
      avg_duration_ms: acc.calls ? Math.round(acc.total_duration_ms / acc.calls) : 0,
      unique_threads: acc.threads.size,
      args: Object.fromEntries([...acc.args.entries()].sort(([, a], [, b]) => b - a)),
      methods: Object.fromEntries(acc.methods),
    }));
  return { tools };
}

/**
 * With `thread_key`: that thread's completed tool calls, newest first, projected to `_time`,
 * `tool_name`, `tool_method`, `tool_args`, `tool_args_count`, `tool_args_truncated`,
 * `duration_ms` and `success`, limited by `limit` (default 200). Without `thread_key`: the
 * busiest threads by tool-call count (`threads`), also capped by `limit`. `start` defaults to
 * `24h`.
 */
export async function tool_usage_by_thread(
  args: { thread_key?: string; start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ entries?: ToolCall[]; threads?: ThreadToolUsage[] }> {
  const start = args.start ?? "24h";
  const limit = args.limit ?? 200;
  if (args.thread_key) {
    const q = `${timePrefix(start)}event:tool_call_completed AND ${fieldExpr("thread_key", args.thread_key)}`;
    const result = await select(ctx, q, limit, start);
    const keep = [
      "_time", "tool_args", "tool_args_count", "tool_args_truncated", "tool_name",
      "tool_method", "duration_ms", "success",
    ];
    return { entries: cleaned(result.entries).map((entry) => project<ToolCall>(entry, keep)) };
  }
  const result = await select(ctx, `${timePrefix(start)}event:tool_call_completed AND thread_key:*`, 10000, start);
  const counts = new Map<string, number>();
  for (const entry of cleaned(result.entries)) {
    const thread = String(entry.thread_key ?? "");
    if (!thread) continue;
    counts.set(thread, (counts.get(thread) ?? 0) + 1);
  }
  const threads = [...counts.entries()]
    .sort(([, a], [, b]) => b - a)
    .slice(0, limit)
    .map(([thread_key, tool_calls]) => ({ thread_key, tool_calls }));
  return { threads };
}

/**
 * Recent `execution_summary` events for one agent run each, projected to the improvement-loop
 * fields: `_time`, `execution_id`, `thread_key`, `harness`, `engine`, `persona_id`, `prompt_ref`,
 * `prompt_sha`, `status`, `terminal_reason`, `duration_s`, `ttft_ms`, `total_tokens`, `cost_usd`,
 * `models`, tool and command error counts, and the per-tool call/error maps. `start` defaults to
 * `24h`; `harness`, `status` and `prompt_ref` filter exactly; `limit` defaults to 100.
 */
export async function execution_summaries(
  args: { start?: string; harness?: string; status?: string; prompt_ref?: string; limit?: number },
  ctx: Ctx,
): Promise<{ entries: ExecutionSummary[]; truncated: boolean }> {
  const start = args.start ?? "24h";
  const parts = ["event:execution_summary"];
  if (args.harness) parts.push(`harness:${args.harness}`);
  if (args.status) parts.push(`status:${args.status}`);
  if (args.prompt_ref) parts.push(`prompt_ref:${args.prompt_ref}`);
  const result = await select(ctx, `${timePrefix(start)}${parts.join(" AND ")}`, args.limit ?? 100, start);
  const keep = [
    "_time", "execution_id", "thread_key", "harness", "engine", "persona_id", "prompt_ref",
    "prompt_sha", "status", "terminal_reason", "duration_s", "ttft_ms", "execution_sequence",
    "user_id", "total_tokens", "cost_usd", "models", "assistant_tool_use_events",
    "tool_error_events", "tool_retry_count", "tool_error_categories", "command_error_events",
    "subagent_failures", "tool_calls_by_name", "tool_errors_by_name",
  ];
  return {
    entries: cleaned(result.entries).map((entry) => project<ExecutionSummary>(entry, keep)),
    truncated: result.truncated,
  };
}

/**
 * Outcomes grouped by prompt lineage (`prompt_ref` + `prompt_sha`): runs, completed, failed,
 * success rate, average duration, tokens and cost, the models and harnesses seen, and the
 * terminal reasons. `start` defaults to `7d` and `limit` caps lineages (default 50); builds on up
 * to 10000 execution summaries, so widen the window with care.
 */
export async function prompt_analytics(
  args: { start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ prompts: PromptStats[] }> {
  const start = args.start ?? "7d";
  const { entries } = await execution_summaries({ start, limit: 10000 }, ctx);
  interface Acc {
    runs: number;
    completed: number;
    failed: number;
    total_duration_s: number;
    total_tokens: number;
    total_cost_usd: number;
    models: Set<string>;
    harnesses: Set<string>;
    terminal_reasons: Map<string, number>;
  }
  const stats = new Map<string, Acc>();
  for (const entry of entries) {
    const promptRef = String(entry.prompt_ref ?? "");
    const promptSha = String(entry.prompt_sha ?? "");
    if (!promptRef) continue;
    const key = `${promptRef}\u0000${promptSha}`;
    let acc = stats.get(key);
    if (!acc) {
      acc = {
        runs: 0, completed: 0, failed: 0, total_duration_s: 0, total_tokens: 0, total_cost_usd: 0,
        models: new Set(), harnesses: new Set(), terminal_reasons: new Map(),
      };
      stats.set(key, acc);
    }
    acc.runs += 1;
    if (entry.status === "completed") acc.completed += 1;
    else acc.failed += 1;
    acc.total_duration_s += toNumber(entry.duration_s);
    acc.total_tokens += Math.trunc(toNumber(entry.total_tokens));
    acc.total_cost_usd += toNumber(entry.cost_usd);
    if (entry.harness) acc.harnesses.add(String(entry.harness));
    const models = Array.isArray(entry.models) ? entry.models : typeof entry.models === "string" ? [entry.models] : [];
    for (const model of models) if (model) acc.models.add(String(model));
    const reason = String(entry.terminal_reason ?? "");
    if (reason) acc.terminal_reasons.set(reason, (acc.terminal_reasons.get(reason) ?? 0) + 1);
  }
  const prompts = [...stats.entries()]
    .map(([key, acc]) => {
      const [prompt_ref, prompt_sha] = key.split("\u0000");
      return {
        prompt_ref,
        prompt_sha,
        runs: acc.runs,
        completed: acc.completed,
        failed: acc.failed,
        success_rate_pct: acc.runs ? Math.round((acc.completed / acc.runs) * 1000) / 10 : 0,
        avg_duration_s: acc.runs ? Math.round((acc.total_duration_s / acc.runs) * 1000) / 1000 : 0,
        avg_tokens: acc.runs ? Math.round(acc.total_tokens / acc.runs) : 0,
        avg_cost_usd: acc.runs ? Math.round((acc.total_cost_usd / acc.runs) * 1e6) / 1e6 : 0,
        models: [...acc.models].sort(),
        harnesses: [...acc.harnesses].sort(),
        terminal_reasons: Object.fromEntries(acc.terminal_reasons),
      };
    })
    .sort((a, b) => b.runs - a.runs)
    .slice(0, args.limit ?? 50);
  return { prompts };
}

/**
 * Token and cost totals per harness/model pair from `usage_observed` events: calls, input,
 * output, cache creation and cache read tokens, total tokens and cost. `start` defaults to `24h`
 * and `limit` caps rows (default 50), most expensive first; builds on up to 10000 events.
 */
export async function model_analytics(
  args: { start?: string; limit?: number },
  ctx: Ctx,
): Promise<{ models: ModelStats[] }> {
  const start = args.start ?? "24h";
  const result = await select(ctx, `${timePrefix(start)}event:usage_observed`, 10000, start);
  interface Acc {
    calls: number;
    input_tokens: number;
    output_tokens: number;
    cache_creation_input_tokens: number;
    cache_read_input_tokens: number;
    cost_usd: number;
  }
  const stats = new Map<string, Acc>();
  for (const entry of cleaned(result.entries)) {
    const harness = String(entry.harness ?? "unknown");
    const model = String(entry.model ?? "unknown");
    const key = `${harness}\u0000${model}`;
    let acc = stats.get(key);
    if (!acc) {
      acc = {
        calls: 0, input_tokens: 0, output_tokens: 0,
        cache_creation_input_tokens: 0, cache_read_input_tokens: 0, cost_usd: 0,
      };
      stats.set(key, acc);
    }
    acc.calls += 1;
    acc.input_tokens += Math.trunc(toNumber(entry.input_tokens));
    acc.output_tokens += Math.trunc(toNumber(entry.output_tokens));
    acc.cache_creation_input_tokens += Math.trunc(toNumber(entry.cache_creation_input_tokens));
    acc.cache_read_input_tokens += Math.trunc(toNumber(entry.cache_read_input_tokens));
    acc.cost_usd += toNumber(entry.cost_usd);
  }
  const models = [...stats.entries()]
    .map(([key, acc]) => {
      const [harness, model] = key.split("\u0000");
      return {
        harness,
        model,
        calls: acc.calls,
        input_tokens: acc.input_tokens,
        output_tokens: acc.output_tokens,
        cache_creation_input_tokens: acc.cache_creation_input_tokens,
        cache_read_input_tokens: acc.cache_read_input_tokens,
        total_tokens:
          acc.input_tokens +
          acc.output_tokens +
          acc.cache_creation_input_tokens +
          acc.cache_read_input_tokens,
        cost_usd: Math.round(acc.cost_usd * 1e6) / 1e6,
        avg_cost_usd: acc.calls ? Math.round((acc.cost_usd / acc.calls) * 1e6) / 1e6 : 0,
      };
    })
    .sort((a, b) => b.cost_usd - a.cost_usd)
    .slice(0, args.limit ?? 50);
  return { models };
}
