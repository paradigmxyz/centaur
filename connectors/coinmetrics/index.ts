import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are Coin Metrics' own objects, passed through; only the `data` envelope and
// pagination are handled here. Field names are Coin Metrics' v4 API's
// (https://docs.coinmetrics.io/api/v4). Times are ISO 8601 UTC strings with nanosecond
// precision (`2024-01-01T00:00:00.000000000Z`); metric values are strings or null, as sent.

const DEFAULT_BASE = "https://api.coinmetrics.io/v4";

/** One `/reference-data/assets` row. */
export interface Asset {
  asset: string;
  full_name?: string;
  asset_class?: string;
  type?: string;
  [field: string]: unknown;
}

/** One `/reference-data/asset-metrics` row: metadata for a metric. */
export interface Metric {
  metric: string;
  full_name?: string;
  description?: string;
  category?: string;
  type?: string;
  unit?: string;
  precision?: number;
  [field: string]: unknown;
}

/** One `/reference-data/exchanges` row. */
export interface Exchange {
  exchange: string;
  full_name?: string;
  exchange_type?: string;
  [field: string]: unknown;
}

/** One `/reference-data/markets` row. */
export interface Market {
  market: string;
  exchange?: string;
  type?: string;
  base?: string;
  quote?: string;
  asset?: string;
  [field: string]: unknown;
}

/** One entry of `/catalog-v2/asset-metrics` `data`: an asset and the metrics it has. */
export interface CatalogAsset {
  asset: string;
  metrics: {
    metric: string;
    frequencies: { frequency: string; min_time?: string; max_time?: string; community?: boolean }[];
    [field: string]: unknown;
  }[];
  [field: string]: unknown;
}

/** One `/timeseries/asset-metrics` row: `asset`, `time`, then one key per requested metric. */
export interface AssetMetric {
  asset: string;
  time: string;
  [metric: string]: string | number | null | undefined;
}

/** One `/timeseries/market-candles` row (OHLCV). */
export interface Candle {
  market: string;
  time: string;
  price_open?: string;
  price_high?: string;
  price_low?: string;
  price_close?: string;
  vwap?: string;
  volume?: string;
  candle_usd_volume?: string;
  trades_count?: number;
  [field: string]: unknown;
}

/** One `/timeseries/market-trades` row. */
export interface Trade {
  market: string;
  time: string;
  price?: string;
  amount?: string;
  side?: string;
  [field: string]: unknown;
}

/** A page of results; pass `next_page_token` back as the argument of the same name for more. */
export interface Page<T> {
  data: T[];
  /** Absent on the last page. */
  next_page_token?: string;
}

async function request(ctx: Ctx, path: string, params: Params = {}): Promise<any> {
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.COINMETRICS_API_KEY) params = { ...params, api_key: ctx.env.COINMETRICS_API_KEY };
  const base = (ctx.env.COINMETRICS_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
  const res = await ctx.fetch(`${base}${path}${query(params)}`, {
    headers: { accept: "application/json" },
  });
  const label = `Coin Metrics ${path}`;
  const text = await res.text();
  let body: any;
  try {
    body = text ? JSON.parse(text) : undefined;
  } catch {
    body = undefined;
  }
  // Errors are `{ error: { type, message } }`, sometimes with HTTP 200.
  const message = body?.error ? (body.error.message ?? body.error.type) : undefined;
  if (!res.ok) {
    throw new Error(`${label} failed (HTTP ${res.status}): ${message ?? text.slice(0, 1000)}`);
  }
  if (message) throw new Error(`${label} failed: ${message}`);
  return body;
}

/** Unwrap `{ data, next_page_token }` without renaming the rows. */
function page<T>(body: any): Page<T> {
  return { data: body.data ?? [], next_page_token: body.next_page_token };
}

/**
 * Assets Coin Metrics covers (thousands), as `{ asset, full_name, asset_class? }`. `asset` is the
 * lowercase ticker the timeseries functions take (`btc`, `eth`, `usdc`). Page with
 * `next_page_token`; `page_size` defaults to 100 and is capped at 10000.
 */
export async function assets(
  args: { page_size?: number; next_page_token?: string },
  ctx: Ctx,
): Promise<Page<Asset>> {
  const body = await request(ctx, "/reference-data/assets", {
    page_size: args.page_size,
    next_page_token: args.next_page_token,
  });
  return page<Asset>(body);
}

/**
 * Every metric Coin Metrics publishes, with full name, category, type and unit — use it to find
 * metric ids such as `PriceUSD`, `AdrActCnt`, `CapMrktCurUSD`, `TxCnt`. Page with
 * `next_page_token`; `page_size` defaults to 100.
 */
export async function metrics(
  args: { page_size?: number; next_page_token?: string },
  ctx: Ctx,
): Promise<Page<Metric>> {
  const body = await request(ctx, "/reference-data/asset-metrics", {
    page_size: args.page_size,
    next_page_token: args.next_page_token,
  });
  return page<Metric>(body);
}

/**
 * Exchanges Coin Metrics covers, as `{ exchange, full_name }`. The `exchange` value is the prefix
 * of market ids (`coinbase-btc-usd-spot`). Page with `next_page_token`.
 */
export async function exchanges(
  args: { page_size?: number; next_page_token?: string },
  ctx: Ctx,
): Promise<Page<Exchange>> {
  const body = await request(ctx, "/reference-data/exchanges", {
    page_size: args.page_size,
    next_page_token: args.next_page_token,
  });
  return page<Exchange>(body);
}

/**
 * Markets (trading pairs per exchange), as `{ market, exchange, type, base, quote, asset? }`. The
 * `market` id (`coinbase-btc-usd-spot`) is what the candle and trade functions take. Page with
 * `next_page_token`; there are millions of rows, so filter locally by exchange/base/quote.
 */
export async function markets(
  args: { page_size?: number; next_page_token?: string },
  ctx: Ctx,
): Promise<Page<Market>> {
  const body = await request(ctx, "/reference-data/markets", {
    page_size: args.page_size,
    next_page_token: args.next_page_token,
  });
  return page<Market>(body);
}

/**
 * Timeseries of one or more on-chain / market metrics per asset: `{ data: [{ asset, time,
 * PriceUSD, ... }] }`. `assets` and `metrics` are comma-separated (`btc,eth` / `PriceUSD,AdrActCnt`);
 * `frequency` is one of 1s, 1m, 5m, 10m, 1h, 1d (default 1d); `start_time` and `end_time` are
 * ISO 8601 (`2024-01-01`) and inclusive. Rows are oldest first; for the newest values set a recent
 * `start_time` window rather than paging from the beginning. Page with `next_page_token`;
 * `page_size` defaults to 1000.
 */
export async function asset_metrics(
  args: {
    assets: string;
    metrics: string;
    frequency?: string;
    start_time?: string;
    end_time?: string;
    page_size?: number;
    next_page_token?: string;
  },
  ctx: Ctx,
): Promise<Page<AssetMetric>> {
  const body = await request(ctx, "/timeseries/asset-metrics", {
    assets: args.assets,
    metrics: args.metrics,
    frequency: args.frequency ?? "1d",
    start_time: args.start_time,
    end_time: args.end_time,
    page_size: args.page_size ?? 1000,
    next_page_token: args.next_page_token,
  });
  return page<AssetMetric>(body);
}

/**
 * Which metrics each asset has, and at which frequencies and time ranges: `{ data: [{ asset,
 * metrics: [{ metric, frequencies: [{ frequency, min_time, max_time, community }] }] }] }`.
 * `assets` is comma-separated; omit it for every asset (large). Page with `next_page_token`.
 */
export async function catalog_asset_metrics(
  args: { assets?: string; page_size?: number; next_page_token?: string },
  ctx: Ctx,
): Promise<Page<CatalogAsset>> {
  const body = await request(ctx, "/catalog-v2/asset-metrics", {
    assets: args.assets,
    page_size: args.page_size,
    next_page_token: args.next_page_token,
  });
  return page<CatalogAsset>(body);
}

/**
 * OHLCV candles for one or more markets: `{ data: [{ market, time, price_open, price_high,
 * price_low, price_close, vwap, volume, candle_usd_volume, trades_count }] }`. Prices and volumes
 * are strings. `markets` is comma-separated market ids; `frequency` is 1m, 5m, 10m, 15m, 30m, 1h,
 * 4h or 1d (default 1h); times are ISO 8601 and inclusive. Page with `next_page_token`.
 */
export async function market_candles(
  args: {
    markets: string;
    frequency?: string;
    start_time?: string;
    end_time?: string;
    page_size?: number;
    next_page_token?: string;
  },
  ctx: Ctx,
): Promise<Page<Candle>> {
  const body = await request(ctx, "/timeseries/market-candles", {
    markets: args.markets,
    frequency: args.frequency ?? "1h",
    start_time: args.start_time,
    end_time: args.end_time,
    page_size: args.page_size ?? 1000,
    next_page_token: args.next_page_token,
  });
  return page<Candle>(body);
}

/**
 * Individual trades for one or more markets: `{ data: [{ market, time, price, amount, side }] }`.
 * `markets` is comma-separated market ids; times are ISO 8601 and inclusive. Page with
 * `next_page_token`; `page_size` defaults to 1000.
 */
export async function market_trades(
  args: {
    markets: string;
    start_time?: string;
    end_time?: string;
    page_size?: number;
    next_page_token?: string;
  },
  ctx: Ctx,
): Promise<Page<Trade>> {
  const body = await request(ctx, "/timeseries/market-trades", {
    markets: args.markets,
    start_time: args.start_time,
    end_time: args.end_time,
    page_size: args.page_size ?? 1000,
    next_page_token: args.next_page_token,
  });
  return page<Trade>(body);
}

/**
 * GET any v4 path, for endpoints without a typed function (for example
 * `/timeseries/asset-metrics` with uncommon parameters, `/catalog/assets`,
 * `/indexes/...`). `endpoint` starts with `/`; `params` become query parameters. The response
 * body is returned as `data`.
 */
export async function raw(
  args: { endpoint: string; params?: Record<string, string | number | boolean> },
  ctx: Ctx,
): Promise<{ data: unknown }> {
  const endpoint = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  return { data: await request(ctx, endpoint, args.params ?? {}) };
}
