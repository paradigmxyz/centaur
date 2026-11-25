import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are Dune's own objects, passed through; only the endpoint bodies are rewrapped in a
// named field. Field names are Dune's (https://docs.dune.com/api-reference/overview/introduction).
// An execution is asynchronous: `execute_query` starts it, `execution_status` reports
// `state` (`QUERY_STATE_PENDING`, `QUERY_STATE_EXECUTING`, `QUERY_STATE_COMPLETED`,
// `QUERY_STATE_FAILED`, `QUERY_STATE_CANCELLED`), and `execution_results` returns the rows once it
// is completed. `run_query` does that loop for you within the 60 s call budget.

const DEFAULT_BASE = "https://api.dune.com/api/v1";

/** A started execution, from `execute_query`. */
export interface Execution {
  execution_id: string;
  state?: string;
  [field: string]: unknown;
}

/** One execution's status, from `execution_status` and `execution_results`. */
export interface ExecutionStatus {
  execution_id?: string;
  query_id?: number;
  state?: string;
  is_execution_finished?: boolean;
  queue_position?: number;
  submitted_at?: string;
  execution_started_at?: string;
  execution_ended_at?: string;
  expires_at?: string;
  [field: string]: unknown;
}

/** Dune's `result.metadata`: column order and types. */
export interface ResultMetadata {
  column_names?: string[];
  column_types?: string[];
  row_count?: number;
  execution_id?: string;
  result_set_bytes?: number;
  total_row_count?: number;
  datapoint_count?: number;
  pending_time_millis?: number;
  execution_time_millis?: number;
  [field: string]: unknown;
}

/** The rows and metadata of a completed execution; rows are objects keyed by column name. */
export interface QueryResult {
  rows: Record<string, unknown>[];
  metadata?: ResultMetadata;
  next_offset?: number;
  next_uri?: string;
  [field: string]: unknown;
}

/** `execution_results`: the execution status plus its `result`. */
export interface ExecutionResults extends ExecutionStatus {
  result?: QueryResult;
  [field: string]: unknown;
}

/** One query parameter definition, as stored on the query. */
export interface QueryParameter {
  key: string;
  type?: string;
  value?: unknown;
  description?: string;
  [field: string]: unknown;
}

/** A saved Dune query's metadata, from `query_metadata`. */
export interface Query {
  query_id: number;
  name?: string;
  description?: string;
  owner?: string;
  is_private?: boolean;
  is_unsaved?: boolean;
  is_archived?: boolean;
  tags?: string[];
  parameters?: QueryParameter[];
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

async function request(
  ctx: Ctx,
  method: string,
  path: string,
  options: { params?: Params; body?: unknown } = {},
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.DUNE_API_KEY) headers["X-Dune-API-Key"] = ctx.env.DUNE_API_KEY;
  if (options.body !== undefined) headers["content-type"] = "application/json";
  const base = (ctx.env.DUNE_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
  const res = await ctx.fetch(`${base}${path}${query(options.params ?? {})}`, {
    method,
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  const text = await res.text();
  let body: any;
  try {
    body = text ? JSON.parse(text) : undefined;
  } catch {
    body = undefined;
  }
  const label = `Dune ${method} ${path}`;
  // Errors are `{ error }`, or a problem+json body with `detail`.
  const message = body && typeof body === "object" ? (body.error ?? body.detail) : undefined;
  if (!res.ok) {
    throw new Error(`${label} failed (HTTP ${res.status}): ${message ?? text.slice(0, 1000)}`);
  }
  // Dune can answer 200 with `{ error }`.
  if (message) {
    throw new Error(`${label} failed: ${message}`);
  }
  return body;
}

function executionPath(execution_id: string): string {
  return `/execution/${encodeURIComponent(execution_id)}`;
}

/**
 * Execute a saved Dune query and return `{ execution: { execution_id, state } }` immediately.
 * `params` fills the query's `{{parameter}}` placeholders (`{ chain: "ethereum" }`); Dune only
 * accepts string, number and boolean values. Then poll `execution_status` and call
 * `execution_results`, or use `run_query`. Each call starts a new (billed) execution; re-read an
 * existing one with `execution_results`.
 */
export async function execute_query(
  args: { query_id: number; params?: Record<string, string | number | boolean> },
  ctx: Ctx,
): Promise<{ execution: Execution }> {
  const body: Record<string, unknown> = {};
  if (args.params) body.query_parameters = args.params;
  return {
    execution: await request(ctx, "POST", `/query/${encodeURIComponent(String(args.query_id))}/execute`, {
      body,
    }),
  };
}

/**
 * One execution's state: `QUERY_STATE_PENDING`, `QUERY_STATE_EXECUTING`, `QUERY_STATE_COMPLETED`,
 * `QUERY_STATE_FAILED` or `QUERY_STATE_CANCELLED`, plus `is_execution_finished`, `queue_position`
 * and timestamps. Cheap to poll; wait a few seconds between calls with `sleep`.
 */
export async function execution_status(
  args: { execution_id: string },
  ctx: Ctx,
): Promise<{ status: ExecutionStatus }> {
  return { status: await request(ctx, "GET", `${executionPath(args.execution_id)}/status`) };
}

/**
 * The rows of a completed execution: `result.rows` (objects keyed by column name),
 * `result.metadata` (`column_names`, `column_types`, `row_count`, timings) and `next_offset` /
 * `next_uri` when more rows exist. Page with `offset` and `limit` (Dune caps a page at its result
 * size, typically 1000 rows for the free plan). Rows are returned as sent, values uncoerced.
 */
export async function execution_results(
  args: { execution_id: string; limit?: number; offset?: number; columns?: string[] },
  ctx: Ctx,
): Promise<{ results: ExecutionResults }> {
  return {
    results: await request(ctx, "GET", `${executionPath(args.execution_id)}/results`, {
      params: {
        limit: args.limit,
        offset: args.offset,
        columns: args.columns?.join(","),
      },
    }),
  };
}

/** Cancel a pending or running execution; returns Dune's confirmation. */
export async function cancel_execution(
  args: { execution_id: string },
  ctx: Ctx,
): Promise<{ cancellation: { execution_id?: string; [field: string]: unknown } }> {
  return {
    cancellation: await request(ctx, "POST", `${executionPath(args.execution_id)}/cancel`),
  };
}

/**
 * A saved query's metadata: name, description, owner, tags and its parameter definitions (`key`,
 * `type`, `value`) — use the parameters to know what to pass to `execute_query`.
 */
export async function query_metadata(
  args: { query_id: number },
  ctx: Ctx,
): Promise<{ query: Query }> {
  return {
    query: await request(ctx, "GET", `/query/${encodeURIComponent(String(args.query_id))}`),
  };
}

/**
 * Execute a query and wait for its rows in one call: polls `execution_status` every
 * `poll_interval_seconds` (default 2) and returns the same shape as `execution_results`.
 * `timeout_seconds` defaults to 45 and must stay under 60 (the call limit); on timeout it throws
 * with the `execution_id`, so continue polling with `execution_status` rather than re-executing.
 * Throws if the execution fails or is cancelled.
 */
export async function run_query(
  args: {
    query_id: number;
    params?: Record<string, string | number | boolean>;
    poll_interval_seconds?: number;
    timeout_seconds?: number;
  },
  ctx: Ctx,
): Promise<{ results: ExecutionResults }> {
  const { execution } = await execute_query({ query_id: args.query_id, params: args.params }, ctx);
  const execution_id = execution.execution_id;
  if (!execution_id) throw new Error("Dune returned no execution_id");
  const timeout = Math.min(args.timeout_seconds ?? 45, 55);
  const interval = Math.max(args.poll_interval_seconds ?? 2, 0.5);
  let elapsed = 0;
  for (;;) {
    const { status } = await execution_status({ execution_id }, ctx);
    const state = status.state ?? "unknown";
    if (state === "QUERY_STATE_COMPLETED") break;
    if (state === "QUERY_STATE_FAILED" || state === "QUERY_STATE_CANCELLED") {
      throw new Error(`Dune query ${args.query_id} ${state} (execution ${execution_id})`);
    }
    if (elapsed >= timeout) {
      throw new Error(
        `Dune query ${args.query_id} still ${state} after ${elapsed}s (execution ${execution_id}); ` +
          `poll execution_status, then execution_results`,
      );
    }
    await ctx.sleep(interval * 1000);
    elapsed += interval;
  }
  return execution_results({ execution_id }, ctx);
}

/**
 * Call any Dune v1 endpoint, for paths without a typed function (for example `/query/{id}/results`,
 * `/datasets`, `/uploads`). `endpoint` is relative to `/api/v1` and starts with `/`; `body` is sent
 * as JSON. The response body is returned as `data`.
 */
export async function raw(
  args: {
    endpoint: string;
    method?: string;
    body?: Record<string, unknown>;
    params?: Record<string, string | number | boolean>;
  },
  ctx: Ctx,
): Promise<{ data: unknown }> {
  let endpoint = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  if (endpoint.startsWith("/api/v1/")) endpoint = endpoint.slice("/api/v1".length);
  const method = (args.method ?? "GET").toUpperCase();
  return { data: await request(ctx, method, endpoint, { body: args.body, params: args.params }) };
}
