import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Polymarket's two public APIs: Gamma (market and event metadata) and CLOB (prices,
// orderbooks, history). No key is needed for any of the functions here except `get_trades`,
// which the CLOB requires L2 authentication for. Results are Polymarket's own JSON, passed
// through; several Gamma fields are JSON-encoded strings (`outcomes`, `outcomePrices`,
// `clobTokenIds`).

const DEFAULT_GAMMA = "https://gamma-api.polymarket.com";
const DEFAULT_CLOB = "https://clob.polymarket.com";

/** A Gamma market. `outcomes`, `outcomePrices` and `clobTokenIds` are JSON strings. */
export interface Market {
  id: string;
  question?: string;
  slug?: string;
  conditionId?: string;
  description?: string;
  /** JSON string such as `["Yes", "No"]`. */
  outcomes?: string;
  /** Probabilities as strings aligned with `outcomes`, such as `["0.075", "0.925"]`. */
  outcomePrices?: string;
  /** CLOB token ids as a JSON string, one per outcome; pass them to the CLOB functions. */
  clobTokenIds?: string;
  /** USDC volume; the `*Num` variants are numbers instead of strings. */
  volume?: string;
  volumeNum?: number;
  volume24hr?: number;
  liquidity?: string;
  liquidityNum?: number;
  active?: boolean;
  closed?: boolean;
  archived?: boolean;
  /** ISO timestamp. */
  startDate?: string;
  endDate?: string;
  groupItemTitle?: string;
  [field: string]: unknown;
}

/** A Gamma event: a group of related markets. */
export interface Event {
  id: string;
  ticker?: string;
  slug?: string;
  title?: string;
  description?: string;
  closed?: boolean;
  active?: boolean;
  volume?: number;
  volume24hr?: number;
  liquidity?: number;
  markets?: Market[];
  [field: string]: unknown;
}

/** Gamma's `public-search` answer. */
export interface SearchResult {
  events?: Event[];
  pagination?: { hasMore?: boolean; totalResults?: number; [field: string]: unknown };
  [field: string]: unknown;
}

/** One CLOB order book level; `price` and `size` are strings. */
export interface Order {
  price: string;
  size: string;
  [field: string]: unknown;
}

/** A CLOB order book. `bids` ascend and `asks` descend by price. */
export interface Book {
  market?: string;
  asset_id?: string;
  timestamp?: string;
  hash?: string;
  bids?: Order[];
  asks?: Order[];
  [field: string]: unknown;
}

/** One point of CLOB price history: `t` is unix seconds, `p` is the probability. */
export interface PricePoint {
  t: number;
  p: number;
  [field: string]: unknown;
}

/** A CLOB trade (only returned with L2 authentication). */
export interface Trade {
  id?: string;
  market?: string;
  asset_id?: string;
  side?: string;
  size?: string;
  price?: string;
  outcome?: string;
  status?: string;
  /** Unix seconds. */
  match_time?: string;
  [field: string]: unknown;
}

function gamma(ctx: Ctx): string {
  return ctx.env.POLYMARKET_GAMMA_API_URL || DEFAULT_GAMMA;
}

function clob(ctx: Ctx): string {
  return ctx.env.POLYMARKET_CLOB_API_URL || DEFAULT_CLOB;
}

async function get(ctx: Ctx, url: string, params: Record<string, string | number | boolean | undefined> = {}, label = "Polymarket"): Promise<any> {
  const res = await ctx.fetch(url + query(params), { headers: { accept: "application/json" } });
  return readJson(res, label);
}

/**
 * Markets from Gamma, most liquid first by default. `order` is a market field such as
 * `volumeNum`, `volume24hr` or `liquidityNum`; `ascending` flips the direction; `closed: true`
 * includes resolved markets; paginate with `offset` and `limit` (pass the same `limit` each
 * page). Each market carries `outcomePrices` and `clobTokenIds` as JSON strings.
 */
export async function list_markets(
  args: {
    limit?: number;
    offset?: number;
    closed?: boolean;
    order?: string;
    ascending?: boolean;
  },
  ctx: Ctx,
): Promise<{ markets: Market[] }> {
  const markets = await get(ctx, `${gamma(ctx)}/markets`, {
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
    closed: args.closed ?? false,
    order: args.order ?? "volumeNum",
    ascending: args.ascending ?? false,
  });
  return { markets: markets as Market[] };
}

/**
 * One market by numeric id or by slug (all-digit arguments are treated as ids, anything else as
 * a slug): question, description, outcome prices, CLOB token ids, volume, liquidity, status and
 * end date.
 */
export async function get_market(args: { market_id: string }, ctx: Ctx): Promise<{ market: Market }> {
  const isId = /^\d+$/.test(args.market_id);
  const path = isId ? `/markets/${args.market_id}` : `/markets/slug/${encodeURIComponent(args.market_id)}`;
  return { market: (await get(ctx, gamma(ctx) + path)) as Market };
}

/**
 * Events (groups of related markets) from Gamma, ordered by `order` (`id` by default) and
 * pageable with `offset`/`limit`. `closed: true` includes resolved events.
 */
export async function list_events(
  args: {
    limit?: number;
    offset?: number;
    closed?: boolean;
    order?: string;
    ascending?: boolean;
  },
  ctx: Ctx,
): Promise<{ events: Event[] }> {
  const events = await get(ctx, `${gamma(ctx)}/events`, {
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
    closed: args.closed ?? false,
    order: args.order ?? "id",
    ascending: args.ascending ?? false,
  });
  return { events: events as Event[] };
}

/** One event by numeric id or by slug, including its `markets` array. */
export async function get_event(args: { event_id: string }, ctx: Ctx): Promise<{ event: Event }> {
  const isId = /^\d+$/.test(args.event_id);
  const path = isId ? `/events/${args.event_id}` : `/events/slug/${encodeURIComponent(args.event_id)}`;
  return { event: (await get(ctx, gamma(ctx) + path)) as Event };
}

/**
 * Keyword search over Gamma markets, events and profiles. `limit` is the number of results per
 * type; `closed: true` keeps closed markets in the result. Returns the events plus Gamma's
 * `pagination` block.
 */
export async function search(
  args: { query: string; limit?: number; closed?: boolean },
  ctx: Ctx,
): Promise<SearchResult> {
  return (await get(ctx, `${gamma(ctx)}/public-search`, {
    q: args.query,
    limit_per_type: args.limit ?? 20,
    keep_closed_markets: args.closed ? 1 : 0,
  })) as SearchResult;
}

/**
 * Current best price for one CLOB token id, as a probability string such as `"0.07"`. `side` is
 * `buy` (default) or `sell`.
 */
export async function get_price(
  args: { token_id: string; side?: "buy" | "sell" },
  ctx: Ctx,
): Promise<{ price: string }> {
  const body = await get(ctx, `${clob(ctx)}/price`, {
    token_id: args.token_id,
    side: args.side ?? "buy",
  });
  return body as { price: string };
}

/**
 * Best prices for several CLOB token ids at one side, in one request. Returns a map of token id
 * to `{ BUY | SELL: price }`, for example
 * `{"<token id>": {"BUY": "0.07"}}`; unanswered token ids are omitted.
 */
export async function get_prices(
  args: { token_ids: string[]; side?: "buy" | "sell" },
  ctx: Ctx,
): Promise<{ prices: Record<string, Record<string, string>> }> {
  // The CLOB only answers this endpoint to a POSTed array of {token_id, side}.
  const res = await ctx.fetch(`${clob(ctx)}/prices`, {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/json" },
    body: JSON.stringify(
      args.token_ids.map((token_id) => ({ token_id, side: args.side ?? "buy" })),
    ),
  });
  return { prices: (await readJson(res, "Polymarket")) as Record<string, Record<string, string>> };
}

/** Full order book for one CLOB token id: `bids` and `asks` levels with string price and size. */
export async function get_book(args: { token_id: string }, ctx: Ctx): Promise<{ book: Book }> {
  return { book: (await get(ctx, `${clob(ctx)}/book`, { token_id: args.token_id })) as Book };
}

/** Midpoint between best bid and ask for one CLOB token id, as a probability string (`mid`). */
export async function get_midpoint(
  args: { token_id: string },
  ctx: Ctx,
): Promise<{ mid: string }> {
  return (await get(ctx, `${clob(ctx)}/midpoint`, { token_id: args.token_id })) as { mid: string };
}

/**
 * Probability history for one CLOB token id: `history` of `{ t, p }` points, `t` in unix
 * seconds. `interval` is `1m`, `1w`, `1d`, `6h`, `1h` or `max`; `fidelity` is the resolution in
 * minutes (upstream requires at least 5 for `1w` and 10 for `1m`, which is why it defaults to 5).
 */
export async function get_price_history(
  args: { token_id: string; interval?: string; fidelity?: number },
  ctx: Ctx,
): Promise<{ history: PricePoint[] }> {
  const body = await get(ctx, `${clob(ctx)}/prices-history`, {
    market: args.token_id,
    interval: args.interval ?? "1w",
    fidelity: args.fidelity ?? 5,
  });
  return body as { history: PricePoint[] };
}

/**
 * Trades for a market condition id (`0x…`), or by `maker` address, optionally bounded by `before`
 * and `after` unix seconds. The public CLOB requires L2 API-key authentication here, so without
 * credentials this throws Polymarket's "Unauthorized/Invalid api key" error.
 */
export async function get_trades(
  args: { market?: string; maker?: string; before?: number; after?: number },
  ctx: Ctx,
): Promise<{ trades: Trade[] }> {
  const trades = await get(ctx, `${clob(ctx)}/data/trades`, {
    market: args.market,
    maker: args.maker,
    before: args.before,
    after: args.after,
  });
  return { trades: trades as Trade[] };
}

/**
 * Escape hatch for any other Gamma or CLOB endpoint: `path` is the path after the host (for
 * example `/tags`, `/series` or `/comments`), `api` picks `gamma` (default) or `clob`, `params`
 * are query parameters. Returns the parsed body as sent, which is an array or an object depending
 * on the endpoint.
 */
export async function request_raw(
  args: {
    path: string;
    api?: "gamma" | "clob";
    params?: Record<string, string | number | boolean>;
  },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  const path = args.path.startsWith("/") ? args.path : `/${args.path}`;
  const host = args.api === "clob" ? clob(ctx) : gamma(ctx);
  return { result: await get(ctx, host + path, args.params ?? {}) };
}
