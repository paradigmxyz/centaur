import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Allium exposes two surfaces: a REST API for saved Explorer queries
// (https://api.allium.so/api/v1/...) and an MCP JSON-RPC endpoint for ad-hoc
// SQL (https://mcp.allium.so). Both are reached with the same key. Rows are
// returned as Allium sends them, with parallel column/row results turned into
// objects and row limits applied; nothing else is reshaped.

const DEFAULT_API_BASE = "https://api.allium.so";
const DEFAULT_MCP_BASE = "https://mcp.allium.so";

/** A column of an MCP SQL result. */
export interface QueryColumn {
  name?: string;
  type?: string;
  [field: string]: unknown;
}

/** One result row, keyed by column name. */
export interface QueryRow {
  [column: string]: unknown;
}

/** A SQL run's state: `status` is Allium's (`pending`, `running`, `success`, `failed`, ...). */
export interface QueryRun {
  run_id: string;
  status: string;
  columns: QueryColumn[];
  /** Empty while the run has not finished; see `sql_run`. */
  rows: QueryRow[];
}

/** A saved query run's status, as Allium reports it. */
export interface QueryRunStatus {
  status?: string;
  error?: string;
  [field: string]: unknown;
}

/** A table's schema entry from Allium's semantic search; `content` is its markdown documentation. */
export interface TableSchema {
  id?: string;
  description?: string;
  columns?: QueryColumn[];
  content?: string;
  [field: string]: unknown;
}

/** MCP `get_query_run_results` blocks server-side; stay well inside the 60 s call limit. */
const MAX_WAIT_SECONDS = 50;
const DEFAULT_WAIT_SECONDS = 40;

let sequence = 0;

function apiBase(ctx: Ctx): string {
  return (ctx.env.ALLIUM_API_URL || DEFAULT_API_BASE).replace(/\/+$/, "");
}

function mcpBase(ctx: Ctx): string {
  return (ctx.env.ALLIUM_MCP_URL || DEFAULT_MCP_BASE).replace(/\/+$/, "");
}

function waitSeconds(value: number | undefined, fallback: number): number {
  return Math.min(Math.max(value ?? fallback, 1), MAX_WAIT_SECONDS);
}

/** Parse the `data:` payloads of a text/event-stream body. */
function parseSse(text: string): unknown[] {
  const events: unknown[] = [];
  let buffer: string[] = [];
  const flush = (): void => {
    if (buffer.length === 0) return;
    const payload = buffer.join("\n");
    buffer = [];
    try {
      events.push(JSON.parse(payload));
    } catch {
      // A non-JSON event is not one of the JSON-RPC frames we are after.
    }
  };
  for (const raw of text.split("\n")) {
    const line = raw.replace(/\r$/, "");
    if (line.startsWith("data:")) {
      buffer.push(line.slice("data:".length).replace(/^ /, ""));
    } else if (line === "") {
      flush();
    }
  }
  flush();
  return events;
}

function parseOrText(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return { text };
  }
}

/** Call one MCP tool and return its structured content (or its text content, parsed as JSON). */
async function mcpCall(ctx: Ctx, tool: string, args: Record<string, unknown>): Promise<any> {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
  };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.ALLIUM_API_KEY) headers["X-API-KEY"] = ctx.env.ALLIUM_API_KEY;
  sequence += 1;
  const payload = {
    jsonrpc: "2.0",
    id: `${Date.now()}-${sequence}`,
    method: "tools/call",
    params: { name: tool, arguments: args },
  };
  const res = await ctx.fetch(`${mcpBase(ctx)}/`, { method: "POST", headers, body: JSON.stringify(payload) });
  const text = await res.text();
  if (!res.ok) throw new Error(`Allium MCP ${tool} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);

  let frame: any = {};
  const contentType = (res.headers["content-type"] ?? "").toLowerCase();
  if (contentType.includes("text/event-stream") || text.trimStart().startsWith("data:")) {
    const events = parseSse(text);
    for (let index = events.length - 1; index >= 0; index -= 1) {
      const event = events[index] as Record<string, unknown>;
      if (event && ("result" in event || "error" in event)) {
        frame = event;
        break;
      }
    }
    if (!frame && events.length > 0) frame = events[events.length - 1];
  } else if (text.trimStart().startsWith("{")) {
    frame = JSON.parse(text);
  } else if (text.trim() !== "") {
    throw new Error(`Allium MCP ${tool} returned a non-JSON body: ${text.slice(0, 500)}`);
  }

  if (frame.error) {
    throw new Error(`Allium MCP ${tool} failed: ${JSON.stringify(frame.error)}`);
  }
  const inner = frame.result ?? {};
  // Tool-level failures arrive as isError plus text content, not as JSON-RPC errors.
  if (inner.isError) {
    const content = inner.content;
    const texts: string[] = Array.isArray(content)
      ? content.map((item: any) => (item && typeof item.text === "string" ? item.text : ""))
      : content && typeof content.text === "string"
        ? [content.text]
        : [];
    throw new Error(`Allium MCP tool ${tool} failed: ${texts.filter(Boolean).join("; ") || JSON.stringify(inner)}`);
  }
  if (inner.structuredContent !== undefined) return inner.structuredContent;
  if (typeof inner.content === "string") return parseOrText(inner.content);
  if (Array.isArray(inner.content) && inner.content.length > 0) {
    const first = inner.content[0];
    return first && typeof first.text === "string" ? parseOrText(first.text) : first;
  }
  if (inner.content && typeof inner.content.text === "string") return parseOrText(inner.content.text);
  return inner.content ?? {};
}

/** Find the run id in an MCP `run_sql_query` response. */
function runIdFrom(result: any): string {
  if (result && typeof result === "object") {
    for (const key of ["run_id", "query_run_id", "id"]) {
      const value = result[key];
      if (typeof value === "string" && value !== "") return value;
    }
    const text = result.text;
    if (typeof text === "string") {
      const match = /run[_ ]?id["'\s:=]+([\w-]+)/i.exec(text);
      if (match) return match[1];
    }
  }
  throw new Error(`Allium did not return a run id: ${JSON.stringify(result).slice(0, 500)}`);
}

/** Pull rows out of an MCP result, zipping parallel column/row arrays into objects. */
function rowsFrom(result: any, row_limit: number): { columns: QueryColumn[]; rows: QueryRow[] } {
  const keys = ["data", "rows", "results"];
  let found: unknown[] = [];
  let columns: QueryColumn[] = [];
  const pick = (value: any): unknown[] | null => {
    if (Array.isArray(value)) return value;
    if (value && typeof value === "object") {
      if (Array.isArray(value.columns) && columns.length === 0) columns = value.columns;
      for (const key of keys) {
        if (key in value) {
          const nested = pick(value[key]);
          if (nested) return nested;
        }
      }
    }
    return null;
  };
  const picked = pick(result);
  if (picked) found = picked;
  if (Array.isArray(result?.columns) && columns.length === 0) columns = result.columns;

  let rows: QueryRow[];
  if (found.length > 0 && Array.isArray(found[0]) && columns.length > 0) {
    const names = columns.map((column, index) =>
      column && typeof column === "object" && typeof column.name === "string" ? column.name : String(index),
    );
    rows = (found as unknown[][]).map((values) => {
      const row: QueryRow = {};
      names.forEach((name, index) => {
        row[name] = values[index];
      });
      return row;
    });
  } else {
    rows = found as QueryRow[];
  }
  return { columns, rows: rows.slice(0, row_limit) };
}

function classify(status: string): "ok" | "pending" | "failed" {
  const value = status.toLowerCase();
  if (["success", "succeeded", "completed", "complete", "finished"].includes(value)) return "ok";
  if (["failed", "error", "canceled", "cancelled"].includes(value)) return "failed";
  return "pending";
}

/** GET/POST the REST API and return the JSON body. */
async function rest(ctx: Ctx, method: string, path: string, body?: unknown): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  if (ctx.env.ALLIUM_API_KEY) headers["X-API-KEY"] = ctx.env.ALLIUM_API_KEY;
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(apiBase(ctx) + path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return readJson(res, `Allium ${method} ${path}`);
}

/**
 * Run ad-hoc SQL against Allium's on-chain dataset and return the rows it has within
 * `wait_seconds` (default 40, at most 50). When `status` is not a finished state the run is still
 * pending: keep the `run_id` and call `sql_run` until it returns rows. `row_limit` defaults to
 * 10000 (Allium's maximum is 250000). Rows are objects keyed by column name when Allium returns
 * column metadata. Use `search_schemas`/`describe_table` to discover tables; common ones are
 * `crosschain.stablecoin.transfers`, `crosschain.metrics.stablecoin_volume`,
 * `crosschain.dex.trades`, `ethereum.core.transactions` and `hyperliquid.raw.trades`. For a
 * freshness check, `SELECT <time column> AS last_update FROM <table> ORDER BY 1 DESC LIMIT 1`
 * is much faster on large tables than `MAX()`.
 */
export async function run_sql(
  args: { sql: string; row_limit?: number; wait_seconds?: number },
  ctx: Ctx,
): Promise<QueryRun> {
  const row_limit = args.row_limit ?? 10000;
  const run_id = runIdFrom(await mcpCall(ctx, "run_sql_query", { sql: args.sql }));
  const result = await mcpCall(ctx, "get_query_run_results", {
    run_id,
    poll_timeout_seconds: waitSeconds(args.wait_seconds, DEFAULT_WAIT_SECONDS),
    row_limit,
  });
  return finish(run_id, result, row_limit);
}

/** Start ad-hoc SQL and return its `run_id` immediately; fetch results with `sql_run`. */
export async function start_sql_run(args: { sql: string }, ctx: Ctx): Promise<{ run_id: string }> {
  return { run_id: runIdFrom(await mcpCall(ctx, "run_sql_query", { sql: args.sql })) };
}

/**
 * Wait for an ad-hoc SQL run and return its rows. Blocks on Allium's side for up to
 * `poll_seconds` (default 40, at most 50); an unfinished run comes back with a non-final `status`
 * and no rows, so call again. `row_limit` defaults to 10000.
 */
export async function sql_run(
  args: { run_id: string; row_limit?: number; poll_seconds?: number },
  ctx: Ctx,
): Promise<QueryRun> {
  const row_limit = args.row_limit ?? 10000;
  const result = await mcpCall(ctx, "get_query_run_results", {
    run_id: args.run_id,
    poll_timeout_seconds: waitSeconds(args.poll_seconds, DEFAULT_WAIT_SECONDS),
    row_limit,
  });
  return finish(args.run_id, result, row_limit);
}

function finish(run_id: string, result: any, row_limit: number): QueryRun {
  const status = result && typeof result === "object" ? String(result.status ?? "") : "";
  if (classify(status) === "failed") {
    const error = result.error ?? result.message ?? JSON.stringify(result);
    throw new Error(`Allium SQL run ${run_id} failed: ${String(error).slice(0, 1000)}`);
  }
  const { columns, rows } = classify(status) === "ok" ? rowsFrom(result, row_limit) : { columns: [] as QueryColumn[], rows: [] as QueryRow[] };
  return { run_id, status: status || "unknown", columns, rows };
}

/**
 * Semantic search over Allium's table catalog (`"erc20 token transfers"`, `"kalshi trades"`,
 * `"hyperliquid funding"`). Returns the matching table ids; pass one to `describe_table`.
 */
export async function search_schemas(args: { query: string }, ctx: Ctx): Promise<{ tables: string[] }> {
  const result = await mcpCall(ctx, "search_schemas", { query: args.query });
  const keys = ["hits", "tables", "results", "data", "matches"];
  const lists: unknown[] = [];
  const pick = (value: any, depth: number): void => {
    if (!value || typeof value !== "object" || depth > 1) return;
    for (const key of keys) {
      const nested = value[key];
      if (Array.isArray(nested)) lists.push(...nested);
      else if (nested && typeof nested === "object") pick(nested, depth + 1);
    }
  };
  pick(result, 0);
  if (lists.length > 0) {
    return {
      tables: lists.map((item: any) =>
        typeof item === "string" ? item : String(item?.id ?? item?.table ?? item?.name ?? JSON.stringify(item)),
      ),
    };
  }
  const text = result && typeof result === "object" ? result.text : undefined;
  if (typeof text === "string") {
    return { tables: text.split("\n").map((line) => line.trim().replace(/^[-`]+|`+$/g, "")).filter(Boolean) };
  }
  return { tables: [] };
}

/**
 * A table's schema: columns (name, type, description) when Allium has them, plus markdown
 * documentation in `content`. `table_id` is a full table name such as
 * `polygon.predictions.trades` or `ethereum.raw.token_transfers`.
 */
export async function describe_table(args: { table_id: string }, ctx: Ctx): Promise<TableSchema> {
  const result = await mcpCall(ctx, "search_schemas", { id: args.table_id });
  const keys = ["hits", "results", "data", "matches", "tables"];
  if (result && typeof result === "object") {
    for (const key of keys) {
      const hits = result[key];
      if (Array.isArray(hits) && hits.length > 0 && hits[0] && typeof hits[0] === "object") return hits[0];
    }
    return result;
  }
  return { id: args.table_id, content: String(result) };
}

/**
 * Start a saved Allium Explorer query and return its `run_id`; poll with `query_status` and read
 * with `query_results`. `parameters` fills the query's `{{placeholders}}`; `row_limit` (default
 * 10000) caps the rows the run produces.
 */
export async function run_query(
  args: { query_id: string; parameters?: Record<string, unknown>; row_limit?: number },
  ctx: Ctx,
): Promise<{ run_id: string }> {
  const body: Record<string, unknown> = { row_limit: args.row_limit ?? 10000 };
  if (args.parameters) body.parameters = args.parameters;
  const response = await rest(ctx, "POST", `/api/v1/explorer/queries/${encodeURIComponent(args.query_id)}/run-async`, body);
  if (typeof response.run_id !== "string") {
    throw new Error(`Allium did not return a run id: ${JSON.stringify(response).slice(0, 500)}`);
  }
  return { run_id: response.run_id };
}

/** Status of a saved-query run: `status` is pending, running, success or failed, with `error` when it failed. */
export async function query_status(args: { run_id: string }, ctx: Ctx): Promise<QueryRunStatus> {
  return await rest(ctx, "GET", `/api/v1/explorer/query-runs/${encodeURIComponent(args.run_id)}/status`);
}

/** Rows of a finished saved-query run, as Allium sends them (one object per row, plus a `columns` entry when the saved query carries one). */
export async function query_results(args: { run_id: string }, ctx: Ctx): Promise<{ rows: QueryRow[] }> {
  const body = await rest(
    ctx,
    "GET",
    `/api/v1/explorer/query-runs/${encodeURIComponent(args.run_id)}/results${query({ f: "json" })}`,
  );
  return { rows: Array.isArray(body.data) ? body.data : [] };
}

/**
 * Run a saved query and wait for it: polls every 2 s for up to `wait_seconds` (default 40, at most
 * 50) and returns the rows once the run succeeds. If it is still running at the deadline it throws
 * with the `run_id`, so call `query_status`/`query_results` with it rather than re-running. For long
 * queries prefer `run_query` plus polling. `parameters` fills the query's `{{placeholders}}`.
 */
export async function execute_query(
  args: { query_id: string; parameters?: Record<string, unknown>; wait_seconds?: number },
  ctx: Ctx,
): Promise<{ run_id: string; status: string; rows: QueryRow[] }> {
  const { run_id } = await run_query(args, ctx);
  const deadline = Date.now() + waitSeconds(args.wait_seconds, DEFAULT_WAIT_SECONDS) * 1000;
  for (;;) {
    const status = await query_status({ run_id }, ctx);
    const state = String(status.status ?? "unknown");
    if (classify(state) === "ok") return { run_id, status: state, rows: (await query_results({ run_id }, ctx)).rows };
    if (classify(state) === "failed") {
      throw new Error(`Allium query run ${run_id} failed: ${String(status.error ?? JSON.stringify(status)).slice(0, 1000)}`);
    }
    if (Date.now() >= deadline) {
      throw new Error(
        `Allium query run ${run_id} is still ${state} after ${waitSeconds(args.wait_seconds, DEFAULT_WAIT_SECONDS)}s; poll query_status/query_results with that run_id`,
      );
    }
    await ctx.sleep(2000);
  }
}

/**
 * Escape hatch for Allium REST endpoints without a wrapper (`GET /api/v1/explorer/queries` lists saved
 * queries): `method` is the HTTP verb, `endpoint` starts with `/api/v1/...`, and `body` is sent as
 * JSON. Returns the HTTP status and the parsed JSON body; a non-2xx answer throws with Allium's
 * message.
 */
export async function raw_request(
  args: { method: string; endpoint: string; body?: Record<string, unknown> },
  ctx: Ctx,
): Promise<{ status: number; body: unknown }> {
  const headers: Record<string, string> = { accept: "application/json" };
  if (ctx.env.ALLIUM_API_KEY) headers["X-API-KEY"] = ctx.env.ALLIUM_API_KEY;
  if (args.body !== undefined) headers["content-type"] = "application/json";
  const endpoint = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  const res = await ctx.fetch(apiBase(ctx) + endpoint, {
    method: args.method.toUpperCase(),
    headers,
    body: args.body === undefined ? undefined : JSON.stringify(args.body),
  });
  return { status: res.status, body: await readJson(res, `Allium ${args.method} ${endpoint}`) };
}
