import type { Ctx } from "../centaur";
import { query } from "../_lib/http";

// Composio's REST API v3 (backend.composio.dev): discover tools per toolkit or by
// search, read a tool's input schema, and execute it against the connected
// account of a user. Tools are addressed by slug, such as
// GITHUB_LIST_REPOS_FOR_USER or HACKERNEWS_GET_TOP_STORIES.

const DEFAULT_BASE = "https://backend.composio.dev";

// Composio user_id scopes connected accounts. Agents in a shared deployment share
// this default scope; pass a distinct user_id when connected accounts must be
// isolated per user or per thread.
const DEFAULT_USER_ID = "centaur";

/** The toolkit (integration) a tool belongs to. */
export interface ToolkitRef {
  slug?: string;
  name?: string;
  logo?: string;
  [field: string]: unknown;
}

/** One Composio tool. */
export interface Tool {
  slug: string;
  name?: string;
  description?: string;
  toolkit?: ToolkitRef;
  /** Per-argument schema: `{ [argument]: { type, description, required, example } }`. */
  input_parameters?: Record<string, unknown>;
  /** True when the tool needs no connected account. */
  no_auth?: boolean;
  available_versions?: string[];
  version?: string;
  [field: string]: unknown;
}

/** A page of tools; `next_cursor` feeds the next call's `cursor`. */
export interface ToolList {
  tools: Tool[];
  next_cursor: string | null;
  total_items?: number;
  total_pages?: number;
  current_page?: number;
}

/** A tool execution: the vendor's `data` plus its success flag and error. */
export interface ExecutionResult {
  successful?: boolean;
  data?: Record<string, unknown>;
  error?: string | null;
  log_id?: string;
  [field: string]: unknown;
}

/** A connected account: which toolkit, which user, and whether auth still works. */
export interface ConnectedAccount {
  id?: string;
  toolkit?: ToolkitRef;
  status?: string;
  user_id?: string;
  auth_config?: { id?: string; auth_scheme?: string; [field: string]: unknown };
  created_at?: string;
  [field: string]: unknown;
}

/** A page of connected accounts; `next_cursor` feeds the next call's `cursor`. */
export interface ConnectedAccountList {
  accounts: ConnectedAccount[];
  next_cursor: string | null;
  total_items?: number;
  total_pages?: number;
  current_page?: number;
}

function base(ctx: Ctx): string {
  return (ctx.env.COMPOSIO_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx): Record<string, string> {
  const value: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.COMPOSIO_API_KEY) value["x-api-key"] = ctx.env.COMPOSIO_API_KEY;
  return value;
}

/** Composio errors are `{ error: { message, slug, status, suggested_fix } }`. */
function errorDetail(payload: any, text: string): string {
  const error = payload?.error;
  if (typeof error === "string" && error) return error;
  if (error && typeof error === "object") {
    const message = error.message || error.slug || JSON.stringify(error);
    return error.suggested_fix ? `${message} (${error.suggested_fix})` : message;
  }
  return text.trim();
}

async function call(
  ctx: Ctx,
  path: string,
  init: { method?: string; body?: unknown } = {},
): Promise<any> {
  const value = headers(ctx);
  if (init.body !== undefined) value["content-type"] = "application/json";
  const res = await ctx.fetch(`${base(ctx)}${path}`, {
    method: init.method || "GET",
    headers: value,
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
  });
  const text = await res.text();
  let payload: any;
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    payload = undefined;
  }
  if (!res.ok) {
    throw new Error(`Composio returned HTTP ${res.status}: ${errorDetail(payload, text)}`);
  }
  if (payload === undefined || payload === null) {
    throw new Error(`Composio returned non-JSON: ${text.slice(0, 1000)}`);
  }
  return payload;
}

/** Tool lists and account lists share the `{ items, next_cursor, total_* }` envelope. */
function page(payload: any, key: string): any {
  const result: Record<string, any> = {};
  result[key] = Array.isArray(payload.items) ? payload.items : [];
  result.next_cursor = payload.next_cursor ?? null;
  if (payload.total_items !== undefined) result.total_items = payload.total_items;
  if (payload.total_pages !== undefined) result.total_pages = payload.total_pages;
  if (payload.current_page !== undefined) result.current_page = payload.current_page;
  return result;
}

/**
 * A toolkit's tools (for example `github`, `gmail`, `slack`, `notion`, `linear`), as
 * `{ tools, next_cursor, total_items, total_pages, current_page }`. Each tool carries its
 * `slug`, `description` and `input_parameters` schema. `important` narrows to featured tools,
 * `limit` (max 1000) and `cursor` page through them, `include_deprecated` keeps retired tools.
 */
export async function list_tools(
  args: {
    toolkit: string;
    important?: boolean;
    limit?: number;
    cursor?: string;
    include_deprecated?: boolean;
  },
  ctx: Ctx,
): Promise<ToolList> {
  if (!args.toolkit || !args.toolkit.trim()) throw new Error("toolkit is required");
  const payload = await call(
    ctx,
    `/api/v3/tools${query({
      toolkit_slug: args.toolkit.trim(),
      important: args.important ? "true" : undefined,
      limit: args.limit,
      cursor: args.cursor,
      include_deprecated: args.include_deprecated ? "true" : undefined,
    })}`,
  );
  return page(payload, "tools");
}

/**
 * Full-text search for tools across every toolkit by name, slug or description, as
 * `{ tools, next_cursor, total_items, ... }`. `limit` defaults to 20, max 1000; `cursor` pages.
 * Use it when you know the action ("create a github issue") but not the slug, then `get_tool`
 * for its arguments.
 */export async function search_tools(
  args: { query: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<ToolList> {
  if (!args.query || !args.query.trim()) throw new Error("query is required");
  const payload = await call(
    ctx,
    `/api/v3/tools${query({
      query: args.query.trim(),
      limit: args.limit ?? 20,
      cursor: args.cursor,
    })}`,
  );
  return page(payload, "tools");
}

/**
 * One tool definition by slug, as `{ tool }`: description, toolkit, `input_parameters` schema
 * (each argument's type, description and whether it is required), `no_auth` and
 * `available_versions`. `version` pins an older version; `include_pricing` adds published
 * pricing when Composio has it.
 */
export async function get_tool(
  args: { tool_slug: string; version?: string; include_pricing?: boolean },
  ctx: Ctx,
): Promise<{ tool: Tool }> {
  const slug = (args.tool_slug || "").trim();
  if (!slug) throw new Error("tool_slug is required");
  const payload = await call(
    ctx,
    `/api/v3/tools/${encodeURIComponent(slug)}${query({
      version: args.version,
      include_pricing: args.include_pricing ? true : undefined,
    })}`,
  );
  return { tool: payload };
}

/**
 * Execute a tool by slug and get its output as `{ successful, data, error, log_id }`. `arguments`
 * are the tool's input parameters (see `get_tool`); either `arguments` or a natural-language
 * `text` description is expected. `user_id` (default `centaur`) picks whose connected account is
 * used; `connected_account_id` overrides it with one specific account (`instant_account` runs on
 * Composio's instant account, failing rather than falling back). Hitting the upstream API without
 * a working connection throws the vendor's error instead of returning `successful: false`.
 */
export async function execute(
  args: {
    tool_slug: string;
    arguments?: Record<string, unknown>;
    text?: string;
    user_id?: string;
    connected_account_id?: string;
    version?: string;
  },
  ctx: Ctx,
): Promise<ExecutionResult> {
  const slug = (args.tool_slug || "").trim();
  if (!slug) throw new Error("tool_slug is required");
  const body: Record<string, unknown> = {
    user_id: args.user_id || DEFAULT_USER_ID,
    arguments: args.arguments ?? {},
  };
  if (args.text) body.text = args.text;
  if (args.connected_account_id) body.connected_account_id = args.connected_account_id;
  if (args.version) body.version = args.version;

  const payload = await call(ctx, `/api/v3/tools/execute/${encodeURIComponent(slug)}`, {
    method: "POST",
    body,
  });
  if (payload.successful === false) {
    throw new Error(`Composio ${slug} failed: ${payload.error || "tool execution failed"}`);
  }
  return payload;
}

/**
 * Connected accounts, as `{ accounts, next_cursor, total_items, ... }`, each with its toolkit,
 * `user_id` and `status` (ACTIVE, EXPIRED, REVOKED, ...). Filter by `toolkits`, `user_ids` or
 * `statuses`; `limit` and `cursor` page. Check this when `execute` says no account is connected.
 */
export async function list_connected_accounts(
  args: {
    toolkits?: string[];
    user_ids?: string[];
    statuses?: string[];
    limit?: number;
    cursor?: string;
  },
  ctx: Ctx,
): Promise<ConnectedAccountList> {
  const payload = await call(
    ctx,
    `/api/v3/connected_accounts${query({
      toolkit_slugs: args.toolkits?.length ? args.toolkits.join(",") : undefined,
      user_ids: args.user_ids?.length ? args.user_ids.join(",") : undefined,
      statuses: args.statuses?.length ? args.statuses.join(",") : undefined,
      limit: args.limit,
      cursor: args.cursor,
    })}`,
  );
  return page(payload, "accounts");
}
