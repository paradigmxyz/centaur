import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Kalshi's own objects, passed through. Field names are Kalshi's
// (https://docs.kalshi.com/api-reference). Public market data needs no key.
// Kalshi migrated prices to dollar strings (`yes_ask_dollars`: "0.0900") and
// counts to fixed-point strings (`volume_fp`); older cent/integer fields can
// still appear, so both are listed where the API has both.

const BASE = "https://api.elections.kalshi.com/trade-api/v2";

/** A tradable market (a yes/no contract on one outcome). */
export interface Market {
  ticker: string;
  event_ticker?: string;
  market_type?: string;
  title?: string;
  subtitle?: string;
  yes_sub_title?: string;
  no_sub_title?: string;
  /** Kalshi's own status, for example active, initialized, closed or settled. */
  status?: string;
  /** ISO 8601 timestamps. */
  open_time?: string;
  close_time?: string;
  expected_expiration_time?: string;
  latest_expiration_time?: string;
  expiration_value?: string;
  /** yes, no or empty while unresolved. */
  result?: string;
  can_close_early?: boolean;
  /** Cents, 1–99, in responses that still use integer prices. */
  yes_bid?: number;
  yes_ask?: number;
  no_bid?: number;
  no_ask?: number;
  last_price?: number;
  previous_price?: number;
  /** Dollars per contract, for example "0.0900". */
  yes_bid_dollars?: string;
  yes_ask_dollars?: string;
  no_bid_dollars?: string;
  no_ask_dollars?: string;
  last_price_dollars?: string;
  previous_price_dollars?: string;
  /** Contracts traded. */
  volume?: number;
  volume_24h?: number;
  /** Contracts outstanding. */
  open_interest?: number;
  liquidity?: number;
  /** Fixed-point strings with two decimals of fractional contracts. */
  volume_fp?: string;
  volume_24h_fp?: string;
  open_interest_fp?: string;
  yes_bid_size_fp?: string;
  yes_ask_size_fp?: string;
  [field: string]: unknown;
}

/** An event: a group of related markets. */
export interface Event {
  event_ticker: string;
  series_ticker?: string;
  title?: string;
  sub_title?: string;
  category?: string;
  mutually_exclusive?: boolean;
  /** Settlement source names and URLs (Kalshi serializes these as strings in some fields). */
  settlement_sources?: unknown[];
  collateral_return_type?: string;
  /** ISO 8601. */
  last_updated_ts?: string;
  strike_period?: string | null;
  [field: string]: unknown;
}

/** One fill. */
export interface Trade {
  trade_id: string;
  ticker: string;
  /** ISO 8601. */
  created_time?: string;
  /** Cents, in responses that still use integer prices. */
  yes_price?: number;
  no_price?: number;
  /** Dollars per contract, for example "0.7800". */
  yes_price_dollars?: string;
  no_price_dollars?: string;
  /** Number of contracts, or its fixed-point string form (`count_fp`). */
  count?: number;
  count_fp?: string;
  /** Which side the taker took: yes or no. */
  taker_side?: string;
  taker_book_side?: string;
  taker_outcome_side?: string;
  is_block_trade?: boolean;
  [field: string]: unknown;
}

/** Open/high/low/close of one price series in a candlestick. */
export interface CandlestickPrice {
  open_dollars?: string;
  high_dollars?: string;
  low_dollars?: string;
  close_dollars?: string;
  mean_dollars?: string;
  previous_dollars?: string;
  /** Cents, in older responses. */
  open?: number;
  high?: number;
  low?: number;
  close?: number;
  [field: string]: unknown;
}

/** One OHLC candle for a market. */
export interface Candlestick {
  /** Candlestick end time, Unix seconds. */
  end_period_ts: number;
  /** Trade prices (the last/mark price series). */
  price?: CandlestickPrice;
  yes_bid?: CandlestickPrice;
  yes_ask?: CandlestickPrice;
  /** Contracts traded in the period. */
  volume?: number;
  volume_fp?: string;
  open_interest?: number;
  open_interest_fp?: string;
  [field: string]: unknown;
}

/** A series: the recurring product a market belongs to (for example KXBTC). */
export interface Series {
  ticker: string;
  title?: string;
  category?: string;
  categories?: string[];
  tags?: string[];
  frequency?: string;
  fee_type?: string;
  /** Serialized as a string by Kalshi. */
  fee_multiplier?: string;
  contract_url?: string;
  contract_terms_url?: string;
  additional_prohibitions?: string[];
  product_metadata?: Record<string, unknown>;
  /** ISO 8601. */
  last_updated_ts?: string;
  [field: string]: unknown;
}

/** An object whose fields this connector does not pin down. */
export type JsonObject = Record<string, unknown>;

async function request(
  ctx: Ctx,
  endpoint: string,
  params: Record<string, string | number | boolean | undefined> = {},
): Promise<any> {
  if (!endpoint.startsWith("/")) throw new Error(`endpoint must start with "/": ${JSON.stringify(endpoint)}`);
  const res = await ctx.fetch(`${BASE}${endpoint}${query(params)}`, {
    headers: { accept: "application/json" },
  });
  return readJson(res, "Kalshi");
}

/**
 * One page of markets, newest listings first, with prices, volume, open interest, status and key
 * dates. The `status` filter is unopened, open, closed or settled (market objects report their own
 * finer status, such as active); `event_ticker` and `series_ticker` narrow to one event or series
 * (for example "KXBTC"). `limit` is at most 1000
 * (default 100). Paginate by passing the returned `cursor` back; stop when it is empty.
 */
export async function list_markets(
  args: { status?: string; event_ticker?: string; series_ticker?: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ markets: Market[]; cursor?: string }> {
  const body = await request(ctx, "/markets", {
    limit: Math.min(args.limit ?? 100, 1000),
    status: args.status,
    event_ticker: args.event_ticker,
    series_ticker: args.series_ticker,
    cursor: args.cursor,
  });
  return { markets: body.markets ?? [], cursor: body.cursor };
}

/**
 * One market by ticker (for example "KXBTC-24DEC31-99999"): full rules, prices, volume, open
 * interest, status, result and strike details.
 */
export async function get_market(args: { ticker: string }, ctx: Ctx): Promise<Market> {
  const body = await request(ctx, `/markets/${encodeURIComponent(args.ticker.trim())}`);
  return body.market ?? body;
}

/**
 * One page of trades, newest first, across markets or for one `ticker`. `min_ts`/`max_ts` are Unix
 * seconds (inclusive range); `limit` is at most 1000 (default 100). Pass the returned `cursor` back
 * to page; stop when it is empty.
 */
export async function get_trades(
  args: { ticker?: string; min_ts?: number; max_ts?: number; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ trades: Trade[]; cursor?: string }> {
  const body = await request(ctx, "/markets/trades", {
    limit: Math.min(args.limit ?? 100, 1000),
    ticker: args.ticker,
    min_ts: args.min_ts,
    max_ts: args.max_ts,
    cursor: args.cursor,
  });
  return { trades: body.trades ?? [], cursor: body.cursor };
}

/**
 * OHLC candles for one market, oldest first. `start_ts`/`end_ts` are Unix seconds bounding the
 * range (both required by Kalshi) and `period_interval` is the candle length in minutes: 1, 60 or
 * 1440 (default, daily). Each candle carries `price`, `yes_bid` and `yes_ask` open/high/low/close
 * dollar strings plus volume and open interest. `series_ticker` is not on the market object: read
 * it from the market's event (`get_event` returns `event.series_ticker`).
 */
export async function get_candlesticks(
  args: {
    series_ticker: string;
    ticker: string;
    start_ts: number;
    end_ts: number;
    period_interval?: 1 | 60 | 1440;
  },
  ctx: Ctx,
): Promise<{ candlesticks: Candlestick[] }> {
  const body = await request(
    ctx,
    `/series/${encodeURIComponent(args.series_ticker.trim())}/markets/${encodeURIComponent(args.ticker.trim())}/candlesticks`,
    {
      start_ts: Math.trunc(args.start_ts),
      end_ts: Math.trunc(args.end_ts),
      period_interval: args.period_interval ?? 1440,
    },
  );
  return { candlesticks: body.candlesticks ?? [] };
}

/**
 * One page of events (groups of markets) with ticker, title, category and series. `status` is
 * open, closed, settled or determined; `series_ticker` narrows to one series; `with_nested_markets`
 * embeds each event's markets. `limit` is at most 200 (default 100). Page with `cursor`.
 */
export async function list_events(
  args: { status?: string; series_ticker?: string; with_nested_markets?: boolean; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ events: Event[]; cursor?: string }> {
  const body = await request(ctx, "/events", {
    limit: Math.min(args.limit ?? 100, 200),
    status: args.status,
    series_ticker: args.series_ticker,
    with_nested_markets: args.with_nested_markets ? "true" : undefined,
    cursor: args.cursor,
  });
  return { events: body.events ?? [], cursor: body.cursor };
}

/**
 * One event by ticker, plus its markets (`markets`) when Kalshi returns them. Use it to go from an
 * event to every tradable market it contains.
 */
export async function get_event(
  args: { event_ticker: string },
  ctx: Ctx,
): Promise<{ event: Event; markets?: Market[] }> {
  const body = await request(ctx, `/events/${encodeURIComponent(args.event_ticker.trim())}`);
  return { event: body.event, markets: body.markets };
}

/**
 * Every series (recurring product, for example KXBTC or KXPRES) in one category, with title, tags,
 * fees, contract URLs and additional trading prohibitions. `category` is required and is
 * case-sensitive as Kalshi spells it ("Crypto", "Politics", "Sports", "Economics", ...); a value
 * Kalshi does not know returns an empty list. This endpoint has no pagination, so a large category
 * (Sports) returns a few thousand rows. Without a category it would return all ~16000 series, which
 * this runtime cannot hold, so the list cannot be requested unfiltered; use `get_series` by ticker.
 */
export async function list_series(args: { category: string }, ctx: Ctx): Promise<{ series: Series[] }> {
  const body = await request(ctx, "/series", { category: args.category.trim() });
  return { series: body.series ?? [] };
}

/** One series by ticker (for example "KXBTC"): title, category, tags, fees and contract links. */
export async function get_series(args: { series_ticker: string }, ctx: Ctx): Promise<Series> {
  const body = await request(ctx, `/series/${encodeURIComponent(args.series_ticker.trim())}`);
  return body.series ?? body;
}

/**
 * Escape hatch for any public trade-api/v2 endpoint not covered above (for example
 * `/exchange/status`, `/markets/{ticker}/orderbook`, `/series/{ticker}`). `endpoint` starts with
 * `/`; `params` become the query string. Returns `{ data }` exactly as the API sends it.
 */
export async function raw_request(
  args: { endpoint: string; params?: Record<string, string | number | boolean> },
  ctx: Ctx,
): Promise<{ data: JsonObject }> {
  return { data: await request(ctx, args.endpoint, args.params) };
}
