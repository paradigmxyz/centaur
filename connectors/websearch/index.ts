import type { Ctx } from "../centaur";
import { readJson } from "../_lib/http";

// One call, one vendor, chosen with `backend` or `$WEBSEARCH_BACKEND` (then Parallel).
// Each vendor answers with a different shape; results are normalized to
// {title, url, snippet, published_date} so a cell can switch backends unchanged.

const DEFAULT_BACKEND: SearchBackend = "parallel";
const MODES: SearchMode[] = ["basic", "advanced"];

const PARALLEL_API_URL = "https://api.parallel.ai/v1/search";
const PARALLEL_MCP_URL = "https://search.parallel.ai/mcp";
const PARALLEL_ATTRIBUTION =
  "Search powered by the free Parallel Web Search MCP (https://parallel.ai). " +
  "See https://parallel.ai/customer-terms.";
const EXA_API_URL = "https://api.exa.ai/search";
const TAKO_API_URL = "https://tako.com/api/v3/search";

const PARALLEL_MAX_RESULTS = 40;
const EXA_MAX_RESULTS = 100;
const TAKO_MAX_WEB_RESULTS = 20;
const TAKO_DATA_CARDS = 2;

const EXA_TYPES: Record<SearchMode, string> = { basic: "fast", advanced: "auto" };
const TAKO_EFFORTS: Record<SearchMode, string> = { basic: "instant", advanced: "fast" };

export type SearchBackend = "parallel" | "exa" | "tako";
export type SearchMode = "basic" | "advanced";

/** Which vendor answered: `parallel:api`, the anonymous `parallel:free` fallback, `exa` or `tako`. */
export type SearchBackendLabel = "parallel:api" | "parallel:free" | "exa" | "tako";

/** One normalized result. `published_date` is the vendor's date string, or null when absent. */
export interface SearchResult {
  title: string;
  url: string;
  snippet: string;
  published_date: string | null;
}

/**
 * `warnings` lists options the chosen backend cannot honor. Free Parallel results carry
 * `attribution`, which must be kept when redistributing them.
 */
export interface SearchMeta {
  backend: SearchBackendLabel;
  request_id: string | null;
  warnings: string[];
  attribution?: string;
}

export interface SearchResponse {
  query: string;
  results: SearchResult[];
  meta: SearchMeta;
}

interface Query {
  text: string;
  numResults: number;
  includeDomains: string[];
  excludeDomains: string[];
  maxAgeHours: number | null;
  mode: SearchMode | null;
  maxCharsTotal: number | null;
  sessionId: string | null;
}

interface Page {
  backend: SearchBackendLabel;
  requestId: string | null;
  results: SearchResult[];
  warnings: string[];
  attribution: string | null;
}

class KeyRejected extends Error {}

function backendName(ctx: Ctx, requested: string | undefined): SearchBackend {
  const configured = ctx.env.WEBSEARCH_BACKEND;
  const name = (requested || configured || DEFAULT_BACKEND).trim().toLowerCase();
  if (name !== "parallel" && name !== "exa" && name !== "tako") {
    throw new Error(`backend must be one of parallel, exa, tako (got '${name}').`);
  }
  return name;
}

/** Lower bound of the recency window, computed at call time. Free Parallel ignores it. */
function publishedAfter(hours: number | null): Date | null {
  return hours ? new Date(Date.now() - hours * 3600_000) : null;
}

const utcDate = (date: Date): string => date.toISOString().slice(0, 10);

/** Python's `datetime.isoformat(timespec="seconds")`: `2026-01-01T12:00:00+00:00`. */
function isoSeconds(date: Date): string {
  return date.toISOString().replace(/\.\d{3}Z$/, "+00:00");
}

function result(title: string | null | undefined, url: string, snippet: string | null | undefined, published: string | null | undefined): SearchResult {
  return {
    title: title || url,
    url,
    snippet: snippet || "",
    published_date: published || null,
  };
}

function ignored(backend: string, used: [name: string, present: boolean][]): string[] {
  const names = used.filter(([, present]) => present).map(([name]) => name);
  return names.length ? [`${backend} ignores ${names.join(", ")}.`] : [];
}

async function postJson(
  ctx: Ctx,
  url: string,
  keyName: string,
  header: string,
  body: unknown,
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json", "content-type": "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  const key = ctx.env[keyName];
  if (key) headers[header] = key;
  const res = await ctx.fetch(url, { method: "POST", headers, body: JSON.stringify(body) });
  if (res.status === 401 || res.status === 403) {
    throw new KeyRejected(`${keyName} was rejected (${res.status}); configure and grant it for this backend.`);
  }
  return readJson(res, url);
}

function parallelResults(data: any): SearchResult[] {
  return (data.results ?? []).map((item: any) =>
    result(item.title, item.url, (item.excerpts ?? []).join("\n\n"), item.publish_date),
  );
}

async function searchParallel(ctx: Ctx, q: Query): Promise<Page> {
  const settings: Record<string, unknown> = { max_results: Math.min(q.numResults, PARALLEL_MAX_RESULTS) };
  const policy: Record<string, unknown> = {};
  if (q.includeDomains.length) policy.include_domains = q.includeDomains;
  if (q.excludeDomains.length) policy.exclude_domains = q.excludeDomains;
  const after = publishedAfter(q.maxAgeHours);
  if (after) policy.after_date = utcDate(after);
  if (Object.keys(policy).length) settings.source_policy = policy;
  const body: Record<string, unknown> = {
    objective: q.text,
    search_queries: [q.text],
    advanced_settings: settings,
  };
  if (q.mode) body.mode = q.mode;
  if (q.maxCharsTotal) body.max_chars_total = q.maxCharsTotal;
  if (q.sessionId) body.session_id = q.sessionId;
  try {
    const data = await postJson(ctx, PARALLEL_API_URL, "PARALLEL_API_KEY", "x-api-key", body);
    return {
      backend: "parallel:api",
      requestId: data.search_id ?? null,
      results: parallelResults(data),
      warnings: [],
      attribution: null,
    };
  } catch (error) {
    if (!(error instanceof KeyRejected)) throw error;
    return searchParallelFree(ctx, q);
  }
}

/** JSON-RPC over plain JSON, or the last `data:` line of an SSE response. */
function jsonrpcMessage(contentType: string, text: string): any {
  if (!contentType.includes("text/event-stream")) return JSON.parse(text);
  const dataLines = text
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trim());
  if (!dataLines.length) throw new Error("Parallel Search MCP returned an empty event stream.");
  return JSON.parse(dataLines[dataLines.length - 1]);
}

/** Parallel's anonymous Search MCP, used when PARALLEL_API_KEY is absent or rejected. */
async function searchParallelFree(ctx: Ctx, q: Query): Promise<Page> {
  const args: Record<string, unknown> = { objective: q.text, search_queries: [q.text] };
  if (q.sessionId) args.session_id = q.sessionId;
  const res = await ctx.fetch(PARALLEL_MCP_URL, {
    method: "POST",
    headers: { accept: "application/json, text/event-stream", "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "tools/call",
      params: { name: "web_search", arguments: args },
    }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Parallel Search MCP returned ${res.status}: ${text.slice(0, 1000)}`);
  const message = jsonrpcMessage(res.headers["content-type"] ?? "", text);
  if (message.error !== undefined) {
    throw new Error(`Parallel Search MCP error: ${JSON.stringify(message.error).slice(0, 1000)}`);
  }
  const call = message.result ?? {};
  if (call.isError) {
    throw new Error(`Parallel Search MCP tool error: ${JSON.stringify(call.content).slice(0, 1000)}`);
  }
  const contentText = Array.isArray(call.content) ? call.content[0]?.text : undefined;
  if (!call.structuredContent && !contentText) {
    throw new Error("Parallel Search MCP returned no structured content.");
  }
  const data = call.structuredContent ?? JSON.parse(contentText);
  return {
    backend: "parallel:free",
    requestId: data.search_id ?? null,
    results: parallelResults(data),
    warnings: ignored("Free Parallel search (PARALLEL_API_KEY was rejected)", [
      ["include_domains", q.includeDomains.length > 0],
      ["exclude_domains", q.excludeDomains.length > 0],
      ["max_age_hours", Boolean(q.maxAgeHours)],
      ["mode", q.mode !== null],
    ]),
    attribution: PARALLEL_ATTRIBUTION,
  };
}

async function searchExa(ctx: Ctx, q: Query): Promise<Page> {
  const body: Record<string, unknown> = {
    query: q.text,
    numResults: Math.min(q.numResults, EXA_MAX_RESULTS),
    contents: { highlights: true },
  };
  if (q.mode) body.type = EXA_TYPES[q.mode];
  if (q.includeDomains.length) body.includeDomains = q.includeDomains;
  if (q.excludeDomains.length) body.excludeDomains = q.excludeDomains;
  const after = publishedAfter(q.maxAgeHours);
  if (after) body.startPublishedDate = isoSeconds(after);
  const data = await postJson(ctx, EXA_API_URL, "EXA_API_KEY", "x-api-key", body);
  return {
    backend: "exa",
    requestId: data.requestId ?? null,
    results: (data.results ?? []).map((item: any) =>
      result(item.title, item.url, (item.highlights ?? []).join("\n\n") || item.text, item.publishedDate),
    ),
    warnings: ignored("Exa", [["session_id", Boolean(q.sessionId)]]),
    attribution: null,
  };
}

async function searchTako(ctx: Ctx, q: Query): Promise<Page> {
  const web: Record<string, unknown> = {
    count: Math.min(q.numResults, TAKO_MAX_WEB_RESULTS),
    highlights: true,
  };
  if (q.includeDomains.length) web.include_domains = q.includeDomains;
  if (q.excludeDomains.length) web.exclude_domains = q.excludeDomains;
  const after = publishedAfter(q.maxAgeHours);
  if (after) web.published_after = utcDate(after);
  const body: Record<string, unknown> = {
    query: q.text,
    sources: { data: { count: Math.min(q.numResults, TAKO_DATA_CARDS) }, web },
  };
  if (q.mode) body.effort = TAKO_EFFORTS[q.mode];
  const data = await postJson(ctx, TAKO_API_URL, "TAKO_API_KEY", "X-API-Key", body);
  const cards = (data.cards ?? [])
    .filter((card: any) => card.webpage_url)
    .map((card: any) =>
      result(card.title, card.webpage_url, card.description, card.data_freshness?.last_updated),
    );
  const webResults = (data.web_results ?? []).map((item: any) =>
    result(item.title, item.url, item.snippet, item.publish_date),
  );
  return {
    backend: "tako",
    requestId: data.request_id ?? null,
    results: [...cards, ...webResults],
    warnings: ignored("Tako", [["session_id", Boolean(q.sessionId)]]),
    attribution: null,
  };
}

/** Drop repeated URLs (first wins), cap the count, then cap total snippet characters. */
function dedupeAndTrim(results: SearchResult[], numResults: number, maxCharsTotal: number | null): SearchResult[] {
  const unique = new Map<string, SearchResult>();
  for (const item of results) if (!unique.has(item.url)) unique.set(item.url, item);
  const trimmed = [...unique.values()].slice(0, numResults);
  if (maxCharsTotal === null) return trimmed;
  let remaining = maxCharsTotal;
  for (const item of trimmed) {
    item.snippet = item.snippet.slice(0, Math.max(remaining, 0));
    remaining -= item.snippet.length;
  }
  return trimmed;
}

/**
 * Search the web and return normalized results (`title`, `url`, `snippet`, `published_date`)
 * plus `meta.backend`, `meta.request_id`, and `meta.warnings`.
 *
 * `backend` is `parallel` (default), `exa`, or `tako`; it falls back to `$WEBSEARCH_BACKEND`,
 * then `parallel`. Parallel needs no key: when its key is absent or rejected it uses the free
 * anonymous Search MCP (`meta.backend` = `parallel:free`, `meta.attribution` required when
 * redistributing), which ignores domain, recency and mode filters and reports them in
 * `meta.warnings`. Exa and Tako require their key.
 *
 * `num_results` defaults to 10 (caps: Parallel 40, Exa 100, Tako 20 web results plus up to 2
 * data cards). `include_domains`/`exclude_domains` take bare hostnames such as `docs.rs`.
 * `max_age_hours` keeps only recent results; Parallel and Tako round it down to a UTC date.
 * `mode` is `basic` (faster) or `advanced`. `max_chars_total` caps total snippet characters.
 * `session_id` is a stable ID reused across related searches (Parallel only; Exa and Tako
 * warn that they ignore it). Duplicate URLs are dropped and results are capped as requested.
 */
export async function search(
  args: {
    query: string;
    num_results?: number;
    include_domains?: string[];
    exclude_domains?: string[];
    max_age_hours?: number;
    mode?: SearchMode;
    max_chars_total?: number;
    session_id?: string;
    backend?: SearchBackend;
  },
  ctx: Ctx,
): Promise<SearchResponse> {
  const backend = backendName(ctx, args.backend);
  const text = args.query.trim();
  if (!text) throw new Error("query cannot be empty.");
  const numResults = args.num_results ?? 10;
  if (numResults < 1) throw new Error("num_results must be at least 1.");
  const mode = args.mode ?? null;
  if (mode !== null && !MODES.includes(mode)) {
    throw new Error(`mode must be one of ${MODES.join(", ")} (got '${mode}').`);
  }
  const q: Query = {
    text,
    numResults,
    includeDomains: args.include_domains ?? [],
    excludeDomains: args.exclude_domains ?? [],
    maxAgeHours: args.max_age_hours ?? null,
    mode,
    maxCharsTotal: args.max_chars_total ?? null,
    sessionId: args.session_id ?? null,
  };
  const page =
    backend === "parallel"
      ? await searchParallel(ctx, q)
      : backend === "exa"
        ? await searchExa(ctx, q)
        : await searchTako(ctx, q);
  const meta: SearchMeta = { backend: page.backend, request_id: page.requestId, warnings: page.warnings };
  if (page.attribution) meta.attribution = page.attribution;
  return { query: text, results: dedupeAndTrim(page.results, numResults, q.maxCharsTotal), meta };
}
