import type { Ctx } from "../centaur";

// Mercator tools are called over its MCP endpoint and the tool's
// `structuredContent` is returned unchanged. Shapes below follow the public
// docs; `inputSchema` and job results are vendor-defined and left as objects.

const DEFAULT_MCP_URL = "https://mercator.sh/mcp/auth";
const DEFAULT_SEARCH_LIMIT = 8;

/** One catalogued endpoint returned by `search`. */
export interface ServiceEndpoint {
  rank?: number;
  score?: number;
  serviceId?: string;
  method?: string;
  path?: string;
  requiredArguments?: string[];
  nextTool?: string;
  [field: string]: unknown;
}

/** Result of `search`: ranked endpoints and the search id to pass to quote/submit. */
export interface SearchResult {
  id?: string;
  resolution?: string;
  endpoints?: ServiceEndpoint[];
  nextTool?: string;
  next_action?: string;
  [field: string]: unknown;
}

/** One endpoint's method, path and authoritative input schema. */
export interface DescribedEndpoint {
  method?: string;
  path?: string;
  inputSchema?: Record<string, unknown>;
  [field: string]: unknown;
}

/** Result of `describe`: a service and its endpoints (all, or the selected one). */
export interface ServiceDescription {
  service?: { id?: string; endpoints?: DescribedEndpoint[]; [field: string]: unknown };
  next_action?: string;
  [field: string]: unknown;
}

/** A live quote for a plan. `totalAmount` is a USD decimal string. */
export interface Quote {
  advisory?: boolean;
  currency?: string;
  totalAmount?: string;
  dynamic?: boolean;
  stages?: string[][];
  quotedAt?: string;
  quoteToken?: string;
  validUntil?: string;
  next_action?: string;
  [field: string]: unknown;
}

/** Result of `submit`: the accepted job and the next step. */
export interface JobSubmission {
  job?: { jobId?: string; status?: string; [field: string]: unknown };
  next_action?: string;
  [field: string]: unknown;
}

/** Result of `get_job`: status, results when terminal, usage and failures. */
export interface Job {
  job?: { jobId?: string; status?: string; [field: string]: unknown };
  jobId?: string;
  status?: string;
  result?: unknown;
  result_mode?: string;
  result_node_ids?: string[];
  summary?: Record<string, unknown>;
  failures?: unknown[];
  usage?: Record<string, unknown>;
  payments?: Record<string, unknown>;
  next_action?: string;
  [field: string]: unknown;
}

/** Authorization, wallet and spending-limit readiness. */
export interface ConnectionStatus {
  readiness?: Record<string, unknown>;
  account?: Record<string, unknown>;
  oauthAuthenticated?: boolean;
  paymentMode?: string;
  next_action?: string;
  [field: string]: unknown;
}

function isRecord(value: unknown): value is Record<string, any> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Compare non-negative decimal strings exactly, without floating point. */
function compareDecimal(left: string, right: string): number {
  const parse = (text: string): { int: string; frac: string } | null => {
    const match = /^(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/.exec(text.trim());
    if (!match || (!match[1] && !match[2] && !match[3])) return null;
    const exponent = match[3] ? Number(match[3]) : 0;
    if (!Number.isSafeInteger(exponent) || Math.abs(exponent) > 1000) return null;
    let digits = match[1] + (match[2] ?? "");
    let point = match[1].length + exponent;
    if (point < 0) {
      digits = "0".repeat(-point) + digits;
      point = 0;
    }
    if (point > digits.length) digits += "0".repeat(point - digits.length);
    return {
      int: (digits.slice(0, point) || "0").replace(/^0+(?=\d)/, ""),
      frac: digits.slice(point).replace(/0+$/, ""),
    };
  };
  const a = parse(left);
  const b = parse(right);
  if (!a || !b) throw new Error(`Mercator returned an invalid decimal amount: ${!a ? left : right}`);
  if (a.int.length !== b.int.length) return a.int.length < b.int.length ? -1 : 1;
  if (a.int !== b.int) return a.int < b.int ? -1 : 1;
  const length = Math.max(a.frac.length, b.frac.length);
  const leftFrac = a.frac.padEnd(length, "0");
  const rightFrac = b.frac.padEnd(length, "0");
  return leftFrac === rightFrac ? 0 : leftFrac < rightFrac ? -1 : 1;
}

/** The JSON-RPC response, from a JSON body or an MCP event stream. */
function message(contentType: string, text: string): Record<string, any> {
  if (!contentType.startsWith("text/event-stream")) {
    try {
      const body = JSON.parse(text);
      if (isRecord(body)) return body;
    } catch {
      // Fall through to a clearer error below.
    }
    throw new Error(`Mercator returned a non-JSON response: ${text.slice(0, 300)}`);
  }
  const events = text.replace(/\r\n/g, "\n").split("\n\n");
  for (let index = events.length - 1; index >= 0; index--) {
    const data = events[index]
      .split("\n")
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).replace(/^ /, ""))
      .join("\n");
    if (!data) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(data);
    } catch {
      continue;
    }
    if (isRecord(parsed) && ("result" in parsed || "error" in parsed)) return parsed;
  }
  throw new Error("Mercator returned an event stream without a response.");
}

async function call(ctx: Ctx, tool: string, args: Record<string, unknown>): Promise<Record<string, any>> {
  // Behind iron-proxy the OAuth bearer is injected on the wire; there is no key to read.
  const res = await ctx.fetch(ctx.env.MERCATOR_MCP_URL || DEFAULT_MCP_URL, {
    method: "POST",
    headers: { accept: "application/json, text/event-stream", "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: tool, arguments: args } }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Mercator ${tool} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  const response = message(res.headers["content-type"] ?? "", text);
  if (isRecord(response.error)) {
    throw new Error(`Mercator ${tool} failed: ${response.error.message ?? JSON.stringify(response.error)}`);
  }
  const result = isRecord(response.result) ? response.result : {};
  if (result.isError) {
    const content = Array.isArray(result.content) ? result.content : [];
    const detail = content
      .map((item) => (isRecord(item) && typeof item.text === "string" ? item.text : ""))
      .filter(Boolean)
      .join(" ");
    throw new Error(`Mercator ${tool} failed: ${detail}`);
  }
  if (!isRecord(result.structuredContent)) throw new Error("Mercator returned an unexpected response.");
  return result.structuredContent;
}

/**
 * Free check of the caller's Mercator connection: OAuth readiness, public wallet/access-key
 * details, balances and spending limits (`readiness`, `account`). Takes no arguments and creates no
 * payment.
 */
export async function connection_status(args: Record<string, never>, ctx: Ctx): Promise<ConnectionStatus> {
  return call(ctx, "get_connection_status", {});
}

/**
 * Free search of the service catalog for the complete outcome you need, in plain language.
 * Returns ranked `endpoints` (serviceId, method, path, required arguments) and a search `id`; pass
 * that id to `quote`/`submit` beside the plan, never inside it. `limit` is 1-25 (default 8).
 */
export async function search(
  args: { query: string; limit?: number },
  ctx: Ctx,
): Promise<SearchResult> {
  const limit = args.limit ?? DEFAULT_SEARCH_LIMIT;
  if (!Number.isInteger(limit) || limit < 1 || limit > 25) throw new Error("limit must be an integer from 1 to 25.");
  return call(ctx, "search_services", { query: args.query, limit });
}

/**
 * Free inspection of a service returned by `search`. Without `method`/`path` it lists the service's
 * endpoints; with both it returns that endpoint's authoritative `inputSchema`. Use the schema to fill
 * a plan node's `input`.
 */
export async function describe(
  args: { service_id: string; method?: string; path?: string },
  ctx: Ctx,
): Promise<ServiceDescription> {
  const arguments_: Record<string, unknown> = { service_id: args.service_id };
  if (args.method !== undefined) arguments_.method = args.method;
  if (args.path !== undefined) arguments_.path = args.path;
  return call(ctx, "describe_service", arguments_);
}

/**
 * Free live price for a plan, without paying. `plan` is `{ nodes: [...] }` with the exact serviceId,
 * method, path, input and dependsOn for each node (at most 10). Returns `totalAmount` (USD decimal
 * string) and the `quoteToken` that `submit` needs. Pass `search_id` beside the plan when available.
 */
export async function quote(
  args: { plan: Record<string, unknown>; search_id?: string },
  ctx: Ctx,
): Promise<Quote> {
  const payload: Record<string, unknown> = { plan: args.plan };
  if (args.search_id) payload.id = args.search_id;
  return call(ctx, "quote_plan", payload);
}

/**
 * Quote a plan and pay for it only if the live total is at most `max_spend` USD. Uses the unchanged
 * plan and a fresh `idempotency_key` (8-200 characters) per job; reuse the same key only to retry a
 * submit whose response was lost. Returns the accepted job; poll it with `get_job` until `status` is
 * succeeded, partially_succeeded or failed. Never resubmit a pending job.
 */
export async function submit(
  args: { plan: Record<string, unknown>; max_spend: number; idempotency_key: string; search_id?: string },
  ctx: Ctx,
): Promise<JobSubmission> {
  const quoted = await quote({ plan: args.plan, search_id: args.search_id }, ctx);
  const total = quoted.totalAmount;
  if (typeof total !== "string" || !total) throw new Error("Mercator quote did not include totalAmount; nothing was paid.");
  if (compareDecimal(total, String(args.max_spend)) > 0) {
    throw new Error(`Quoted total ${total} exceeds max spend ${args.max_spend}; nothing was paid.`);
  }
  const token = quoted.quoteToken;
  if (typeof token !== "string" || !token) {
    throw new Error("Mercator quote did not include a quoteToken; nothing was paid.");
  }
  const payload: Record<string, unknown> = {
    plan: args.plan,
    approved_total: total,
    idempotency_key: args.idempotency_key,
    quote_token: token,
  };
  if (args.search_id) payload.id = args.search_id;
  return call(ctx, "create_job", payload);
}

/**
 * Free status and results for a job returned by `submit`. Poll until `status` is succeeded,
 * partially_succeeded or failed; terminal responses include the node `result`, usage and failures.
 * Keep the job id and resume polling after a disconnect instead of resubmitting.
 */
export async function get_job(args: { job_id: string }, ctx: Ctx): Promise<Job> {
  return call(ctx, "get_job", { job_id: args.job_id });
}
