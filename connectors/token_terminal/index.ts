import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are Token Terminal's own `v2` objects, passed through: field names and
// types are theirs. Data endpoints wrap their payload in `data`; the connector
// unwraps it under a named key. Project and metric IDs come from `list_projects`
// and the `metric_availability` of `get_project`.

const BASE_URL = "https://api.tokenterminal.com/v2";

/** A product of a project: an app, a chain or a token with its own metrics. */
export interface Product {
  product_id: string;
  product_name?: string;
  symbol?: string;
  url?: string;
  market_sectors?: string[];
  [field: string]: unknown;
}

/** A project (protocol, chain, app, ...) as listed by the API. */
export interface Project {
  project_id: string;
  name: string;
  symbol?: string | null;
  coingecko_id?: string | null;
  coinmarketcap_id?: string | null;
  url?: string;
  is_archived?: boolean;
  archived_reason?: string;
  maintenance_reason?: string;
  products?: Product[];
  [field: string]: unknown;
}

/** One project with the metrics it supports; `metric_availability` is keyed by metric ID. */
export interface ProjectDetails {
  project_id: string;
  name: string;
  symbol?: string | null;
  description?: unknown;
  market_sectors?: unknown;
  chains?: string[];
  products?: Product[];
  links?: { title?: string; url?: string; [field: string]: unknown }[];
  aggregate_by_options?: Record<string, unknown>;
  metric_availability?: Record<string, boolean>;
  metric_definitions?: { metric_id?: string; definition?: string; [field: string]: unknown }[];
  metric_sources?: { metric_id?: string; definition?: string; [field: string]: unknown }[];
  [field: string]: unknown;
}

/**
 * One row of a metric series: `timestamp` plus a column per requested metric ID (values are
 * numbers, or strings where the upstream formats them), broken down by chain, business line or
 * version when `aggregate_by` asks for it.
 */
export interface MetricRow {
  timestamp: string;
  project_name?: string;
  project_id?: string;
  chain?: string;
  business_line?: string;
  version?: string;
  [metric_id: string]: unknown;
}

/** A metric the request could not resolve, when other entities in it did succeed. */
export interface MetricError {
  code: string;
  field: string;
  value: string;
}

/** One row of a financial statement: a metric value for one period. */
export interface FinancialStatementRow {
  timestamp?: string;
  timestamp_label_heading?: string;
  timestamp_label_subheading?: string;
  timestamp_granularity?: string;
  project_name?: string;
  section?: string;
  metric_id?: string;
  value?: string;
  pop_change?: string;
  definition?: string;
  [field: string]: unknown;
}

/** A market sector, whose ID filters `get_market_sector_metrics` and `get_project_metrics`. */
export interface MarketSector {
  id: string;
  name: string;
  url?: string;
  [field: string]: unknown;
}

async function request(ctx: Ctx, path: string, params?: Params): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the bearer token is injected on the wire; locally it comes from the env.
  if (ctx.env.TOKEN_TERMINAL_API_KEY) {
    headers.authorization = `Bearer ${ctx.env.TOKEN_TERMINAL_API_KEY}`;
  }
  const res = await ctx.fetch(BASE_URL + path + query(params ?? {}), { headers });
  const text = await res.text();
  if (!res.ok) throw new Error(`Token Terminal returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return text ? JSON.parse(text) : null;
}

/** Comma-join an ID filter that the API takes as one query parameter. */
function csv(value: string | string[] | undefined): string | undefined {
  return Array.isArray(value) ? value.join(",") : value;
}

/**
 * Projects available on the API with ID, name, symbol, products and archive state. `limit` caps
 * how many are returned (the endpoint has no paging, so it is applied client-side).
 */
export async function list_projects(
  args: { limit?: number },
  ctx: Ctx,
): Promise<{ projects: Project[] }> {
  const body = await request(ctx, "/projects");
  const projects = (body?.data ?? []) as Project[];
  return { projects: args.limit === undefined ? projects : projects.slice(0, args.limit) };
}

/**
 * One project's details and metric availability: chains, products, links, and the
 * `metric_availability` map plus `metric_definitions` that say which metric IDs can be queried.
 */
export async function get_project(
  args: { project_id: string },
  ctx: Ctx,
): Promise<{ project: ProjectDetails }> {
  const body = await request(ctx, `/projects/${encodeURIComponent(args.project_id)}`);
  return { project: body?.data };
}

/**
 * A project's metric time series. `metric_ids` (for example `fees`, `revenue`, `tvl`) are
 * returned as columns; `chain_ids`, `product_ids` and `market_sector_ids` narrow the project's
 * rows, and `aggregate_by` (`chain`, `business_line`, `version`) splits them. `start`/`end` are
 * ISO dates (YYYY-MM-DD) bounding the series; `order_direction` is `asc` or `desc`.
 * `errors` lists requested IDs that were not found while the rest of the series still returns.
 */
export async function get_project_metrics(
  args: {
    project_id: string;
    metric_ids?: string | string[];
    chain_ids?: string | string[];
    product_ids?: string | string[];
    market_sector_ids?: string | string[];
    aggregate_by?: string;
    start?: string;
    end?: string;
    order_direction?: "asc" | "desc";
  },
  ctx: Ctx,
): Promise<{ rows: MetricRow[]; errors?: MetricError[] }> {
  const body = await request(ctx, `/projects/${encodeURIComponent(args.project_id)}/metrics`, {
    metric_ids: csv(args.metric_ids),
    chain_ids: csv(args.chain_ids),
    product_ids: csv(args.product_ids),
    market_sector_ids: csv(args.market_sector_ids),
    aggregate_by: args.aggregate_by,
    start: args.start,
    end: args.end,
    order_direction: args.order_direction,
  });
  return { rows: (body?.data ?? []) as MetricRow[], errors: body?.errors };
}

/**
 * A project's financial statement as rows of `{section, metric_id, value, pop_change}` per
 * period. `timestamp_granularity` is `year`, `quarter` (default), `month` or `week`; `interval`
 * is `1m`, `1y` (default), `2y`, `3y`, `5y` or `max`.
 */
export async function get_financial_statement(
  args: {
    project_id: string;
    timestamp_granularity?: "year" | "quarter" | "month" | "week";
    interval?: "1m" | "1y" | "2y" | "3y" | "5y" | "max";
  },
  ctx: Ctx,
): Promise<{ rows: FinancialStatementRow[] }> {
  const body = await request(
    ctx,
    `/projects/${encodeURIComponent(args.project_id)}/financial-statement`,
    { timestamp_granularity: args.timestamp_granularity, interval: args.interval },
  );
  return { rows: (body?.data ?? []) as FinancialStatementRow[] };
}

/** All market sectors with their ID and name; the IDs filter project metrics. */
export async function list_market_sectors(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ market_sectors: MarketSector[] }> {
  const body = await request(ctx, "/market-sectors");
  return { market_sectors: (body?.data ?? []) as MarketSector[] };
}

/**
 * A metric time series across every project in one market sector (`metric` is a single metric
 * ID such as `fees`). `start`/`end` are ISO dates (YYYY-MM-DD) bounding the series; `errors`
 * lists IDs that were not found while the rest of the series still returns.
 */
export async function get_market_sector_metrics(
  args: { sector: string; metric: string; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ rows: MetricRow[]; errors?: MetricError[] }> {
  const body = await request(ctx, `/metrics/${encodeURIComponent(args.metric)}`, {
    market_sector_ids: args.sector,
    start: args.start,
    end: args.end,
  });
  return { rows: (body?.data ?? []) as MetricRow[], errors: body?.errors };
}
