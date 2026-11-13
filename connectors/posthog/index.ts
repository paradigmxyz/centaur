import type { Ctx } from "../centaur";
import { readJson } from "../_lib/http";

// Results are PostHog's own query response objects, passed through: `columns`
// names each column, `results` holds one array per row. The event helpers build
// a HogQL query and call the same endpoint as `query`.

const DEFAULT_HOST = "us.posthog.com";

/** PostHog's HogQL query response. `types` and `hogql` are present on most responses. */
export interface QueryResult {
  columns: string[];
  /** One array per row, in `columns` order. Values keep their JSON types. */
  results: unknown[][];
  types?: string[][];
  /** The SQL PostHog actually ran. */
  hogql?: string;
  [field: string]: unknown;
}

/** A project visible to the API key, from `GET /api/projects/`. */
export interface Project {
  id: number;
  name?: string;
  organization?: string;
  [field: string]: unknown;
}

/** True when a value is absent or still an unswapped secret placeholder. */
function isPlaceholder(value: string | undefined, name: string): boolean {
  const raw = value?.trim();
  if (!raw) return true;
  return raw === name || raw.startsWith("__CENTAUR_") || raw.startsWith("CENTAUR_SECRET_");
}

/** `POSTHOG_HOST` (a host name, `us.posthog.com` by default) as a URL base. */
function baseUrl(ctx: Ctx): string {
  const host = (ctx.env.POSTHOG_HOST || DEFAULT_HOST).trim().replace(/\/+$/, "");
  return /^https?:\/\//.test(host) ? host : `https://${host}`;
}

async function posthog(ctx: Ctx, path: string, body?: unknown): Promise<unknown> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.POSTHOG_API_KEY) headers.authorization = `Bearer ${ctx.env.POSTHOG_API_KEY}`;
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(baseUrl(ctx) + path, {
    method: body === undefined ? "GET" : "POST",
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return readJson(res, "PostHog");
}

/** A HogQL string literal. */
function literal(value: string): string {
  return `'${value.replace(/\\/g, "\\\\").replace(/'/g, "\\'")}'`;
}

/** A validated `LIMIT` value; HogQL has no bind parameters, so numbers are required. */
function rowLimit(value: number | undefined, fallback: number, max = 10000): number {
  const limit = value === undefined ? fallback : value;
  if (!Number.isInteger(limit) || limit < 1 || limit > max) {
    throw new Error(`limit must be an integer between 1 and ${max}`);
  }
  return limit;
}

function nonNegativeInt(value: number | undefined, fallback: number, field: string): number {
  const result = value === undefined ? fallback : value;
  if (!Number.isInteger(result) || result < 0) throw new Error(`${field} must be a non-negative integer`);
  return result;
}

/** PostHog's project id: configured via the path placeholder, else the first visible project. */
async function projectId(ctx: Ctx): Promise<string> {
  const configured = ctx.env.POSTHOG_PROJECT_ID;
  if (!isPlaceholder(configured, "POSTHOG_PROJECT_ID")) return configured!.trim();
  const { projects } = await list_projects({}, ctx);
  const id = projects[0]?.id;
  if (id === undefined) {
    throw new Error("POSTHOG_PROJECT_ID is not set and no PostHog projects were visible");
  }
  return String(id);
}

async function runQuery(ctx: Ctx, sql: string, name: string): Promise<QueryResult> {
  const path = `/api/projects/${encodeURIComponent(await projectId(ctx))}/query/`;
  const body = await posthog(ctx, path, { query: { kind: "HogQLQuery", query: sql }, name });
  return body as QueryResult;
}

/**
 * Run a HogQL query (ClickHouse-flavoured SQL over PostHog's `events`, `persons`, `sessions`
 * and other tables; `now()`, `today()`, `toStartOfDay()` and `INTERVAL n DAY` work). Returns
 * PostHog's response: `columns`, `results` (row arrays in column order), `types` and `hogql`.
 * `name` only labels the query for PostHog's own logging. Cap rows with a `LIMIT` in `sql`.
 */
export async function query(args: { sql: string; name?: string }, ctx: Ctx): Promise<QueryResult> {
  return runQuery(ctx, args.sql, args.name ?? "query");
}

/**
 * Projects the API key can see (id, name, organization), from `GET /api/projects/`. Use it to
 * find the id the query helpers need, or to check that a key has access.
 */
export async function list_projects(args: {}, ctx: Ctx): Promise<{ projects: Project[] }> {
  const body = (await posthog(ctx, "/api/projects/")) as { results?: Project[] } | Project[];
  const projects = Array.isArray(body) ? body : (body.results ?? []);
  return { projects };
}

/**
 * Recent events, newest first, as a query response with columns `timestamp`, `event`,
 * `distinct_id`, `properties`. `event` filters by exact event name; `after`/`before` bound
 * `timestamp` (inclusive, ISO 8601 such as `2024-05-01` or `2024-05-01T12:00:00Z`). `properties`
 * adds exact-match filters on event properties; keys are property names (`$browser`, `$pathname`
 * or a nested `$set.plan`) and values are compared as strings unless a number or boolean.
 * `limit` defaults to 100 (max 10000).
 */
export async function events(
  args: {
    event?: string;
    properties?: Record<string, string | number | boolean>;
    limit?: number;
    after?: string;
    before?: string;
  },
  ctx: Ctx,
): Promise<QueryResult> {
  const conditions: string[] = [];
  if (args.event) conditions.push(`event = ${literal(args.event)}`);
  if (args.after) conditions.push(`timestamp >= ${literal(args.after)}`);
  if (args.before) conditions.push(`timestamp <= ${literal(args.before)}`);
  for (const [key, value] of Object.entries(args.properties ?? {})) {
    if (!/^[A-Za-z0-9_$.-]+$/.test(key)) throw new Error(`invalid property name ${JSON.stringify(key)}`);
    conditions.push(`properties.${key} = ${typeof value === "string" ? literal(value) : String(value)}`);
  }
  const where = conditions.length ? conditions.join(" AND ") : "1=1";
  const sql = `
            SELECT timestamp, event, distinct_id, properties
            FROM events
            WHERE ${where}
            ORDER BY timestamp DESC
            LIMIT ${rowLimit(args.limit, 100)}
        `;
  return runQuery(ctx, sql, "events_query");
}

/**
 * Event counts grouped by one property value, with percentage of the total. `property` is a
 * property name such as `$browser` or `$os` (`$` required for automatic properties); `event`
 * restricts to one event name; `days` is the look-back window in days (default 7) and `limit`
 * caps groups (default 20). Columns are `value`, `count`, `percentage`.
 */
export async function breakdown(
  args: { property?: string; event?: string; days?: number; limit?: number },
  ctx: Ctx,
): Promise<QueryResult> {
  const property = args.property ?? "$browser";
  const days = nonNegativeInt(args.days, 7, "days");
  const eventFilter = args.event ? `AND event = ${literal(args.event)}` : "";
  const sql = `
            SELECT
                properties.${property} AS value,
                count() AS count,
                round(count() * 100.0 / sum(count()) OVER (), 2) AS percentage
            FROM events
            WHERE timestamp >= now() - INTERVAL ${days} DAY
            ${eventFilter}
            GROUP BY value
            ORDER BY count DESC
            LIMIT ${rowLimit(args.limit, 20)}
        `;
  return runQuery(ctx, sql, `breakdown_${property}`);
}

/**
 * `$pageview` counts per URL over the last `days` days: columns `url`, `views`,
 * `unique_visitors` (distinct visitors), most viewed first. `url_pattern` keeps only URLs that
 * contain it (a substring, not a regex); `limit` defaults to 20.
 */
export async function pageviews(
  args: { url_pattern?: string; days?: number; limit?: number },
  ctx: Ctx,
): Promise<QueryResult> {
  const days = nonNegativeInt(args.days, 7, "days");
  const urlFilter = args.url_pattern
    ? `AND properties.$current_url LIKE ${literal(`%${args.url_pattern}%`)}`
    : "";
  const sql = `
            SELECT
                properties.$current_url AS url,
                count() AS views,
                uniq(distinct_id) AS unique_visitors
            FROM events
            WHERE event = '$pageview'
            AND timestamp >= now() - INTERVAL ${days} DAY
            ${urlFilter}
            GROUP BY url
            ORDER BY views DESC
            LIMIT ${rowLimit(args.limit, 20)}
        `;
  return runQuery(ctx, sql, "pageviews");
}

/**
 * Counts per browser and OS pair over the last `days` days for `event` (default `$pageview`):
 * columns `browser`, `os`, `count`, `percentage`. `url_pattern` keeps only URLs containing it;
 * `limit` defaults to 20.
 */
export async function user_agents(
  args: { url_pattern?: string; event?: string; days?: number; limit?: number },
  ctx: Ctx,
): Promise<QueryResult> {
  const days = nonNegativeInt(args.days, 7, "days");
  const event = args.event ?? "$pageview";
  const urlFilter = args.url_pattern
    ? `AND properties.$current_url LIKE ${literal(`%${args.url_pattern}%`)}`
    : "";
  const sql = `
            SELECT
                properties.$browser AS browser,
                properties.$os AS os,
                count() AS count,
                round(count() * 100.0 / sum(count()) OVER (), 2) AS percentage
            FROM events
            WHERE event = ${literal(event)}
            AND timestamp >= now() - INTERVAL ${days} DAY
            ${urlFilter}
            GROUP BY browser, os
            ORDER BY count DESC
            LIMIT ${rowLimit(args.limit, 20)}
        `;
  return runQuery(ctx, sql, "user_agents");
}
