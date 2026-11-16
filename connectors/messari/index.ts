import type { Ctx } from "../centaur";
import { query } from "../_lib/http";

// Messari's Metrics API (https://api.messari.io/metrics/v1). Every answer is `{ "data": ... }`;
// that envelope is unwrapped and the asset or timeseries object is passed through with
// Messari's own field names, which differ by subscription tier.

const DEFAULT_BASE = "https://api.messari.io/metrics";

/** Price and volume metrics for an asset. */
export interface MarketData {
  price_usd?: number;
  real_volume_last_24_hours?: number;
  percent_change_usd_last_24_hours?: number;
  [field: string]: unknown;
}

/** Market capitalization metrics for an asset. */
export interface Marketcap {
  current_marketcap_usd?: number;
  [field: string]: unknown;
}

/** The metric groups Messari returns for an asset. */
export interface AssetMetrics {
  market_data?: MarketData;
  marketcap?: Marketcap;
  [field: string]: unknown;
}

/** An asset, as Messari sends it. `metrics` holds what the list endpoint returns; the single
 * asset endpoint also fills `profile` and may inline `market_data` and `marketcap`. */
export interface Asset {
  id?: string;
  slug?: string;
  symbol?: string;
  name?: string;
  /** Metrics grouped per metric group, with `market_data` and `marketcap` among them. */
  metrics?: AssetMetrics;
  market_data?: MarketData;
  marketcap?: Marketcap;
  profile?: Record<string, unknown>;
  [field: string]: unknown;
}

/** Points of a metric time series: `[timestamp in milliseconds, value]`. */
export interface TimeSeries {
  values?: [number, number][];
  [field: string]: unknown;
}

interface ErrorBody {
  error?: string;
  status?: { error_message?: string; [field: string]: unknown };
  [field: string]: unknown;
}

async function request(
  ctx: Ctx,
  path: string,
  options: { version?: number; params?: Record<string, string | number | boolean | undefined>; method?: string; body?: string } = {},
): Promise<any> {
  const base = ctx.env.MESSARI_API_URL || DEFAULT_BASE;
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.MESSARI_API_KEY) headers["x-messari-api-key"] = ctx.env.MESSARI_API_KEY;
  if (options.body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(
    `${base}/v${options.version ?? 1}${path}${query(options.params ?? {})}`,
    { method: options.method ?? "GET", headers, body: options.body },
  );
  const text = await res.text();
  if (!res.ok) {
    let message = text.slice(0, 1000);
    try {
      const body = JSON.parse(text) as ErrorBody;
      message = body.error ?? body.status?.error_message ?? message;
    } catch {
      // Not JSON: keep the raw text.
    }
    throw new Error(`Messari API error (${res.status}): ${message}`);
  }
  return JSON.parse(text);
}

/**
 * Assets known to the Messari metrics API, with id, slug, symbol, name and metrics such as
 * `market_data.price_usd`. `limit` caps the number returned (no cursor: ask for more instead of
 * paging).
 */
export async function list_assets(
  args: { limit?: number },
  ctx: Ctx,
): Promise<{ assets: Asset[] }> {
  const body = await request(ctx, "/assets", { params: { limit: args.limit ?? 20 } });
  return { assets: (Array.isArray(body.data) ? body.data : []) as Asset[] };
}

/**
 * One asset by slug or id (for example `bitcoin` or `ethereum`): name, symbol, slug, plus the
 * metrics, profile and market fields the API returns for the subscription tier.
 */
export async function get_asset(args: { asset_key: string }, ctx: Ctx): Promise<{ asset: Asset }> {
  const body = await request(ctx, `/assets/${encodeURIComponent(args.asset_key)}`);
  return { asset: (body.data ?? {}) as Asset };
}

/**
 * Historical values of one metric for one asset, such as `price`, `mcap` or `vol`, as
 * `[timestamp in milliseconds, value]` points. `start` and `end` are dates (YYYY-MM-DD);
 * omit them for the API default range.
 */
export async function get_timeseries(
  args: { asset_key: string; metric: string; start?: string; end?: string },
  ctx: Ctx,
): Promise<{ timeseries: TimeSeries }> {
  const body = await request(
    ctx,
    `/assets/${encodeURIComponent(args.asset_key)}/metrics/${encodeURIComponent(args.metric)}/time-series`,
    { params: { start: args.start, end: args.end } },
  );
  return { timeseries: (body.data ?? {}) as TimeSeries };
}

/**
 * Escape hatch for any other Messari endpoint: `endpoint` is the path after the version prefix
 * (for example `/assets/bitcoin/metrics/price/time-series`), `version` is 1 (default) or 2,
 * `params` are query parameters. Returns the whole parsed response body, envelope included.
 */
export async function request_raw(
  args: { endpoint: string; version?: number; params?: Record<string, string | number | boolean> },
  ctx: Ctx,
): Promise<{ result: Record<string, unknown> }> {
  const path = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  return { result: (await request(ctx, path, { version: args.version, params: args.params })) as Record<string, unknown> };
}
