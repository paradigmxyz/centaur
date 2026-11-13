import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are CoinGecko's own objects, passed through; only the endpoint envelopes are
// unwrapped. Field names are CoinGecko's (https://docs.coingecko.com/reference/).
// Coin ids are CoinGecko's slugs (`bitcoin`, `ethereum`, `usd-coin`), not symbols; resolve a
// symbol or name to an id with `search` first.

const DEFAULT_BASE = "https://pro-api.coingecko.com/api/v3";

/** One row of `/coins/markets`. Prices and values are in the requested `vs_currency`. */
export interface Coin {
  id: string;
  symbol: string;
  name: string;
  image?: string;
  current_price?: number;
  market_cap?: number;
  market_cap_rank?: number;
  fully_diluted_valuation?: number;
  total_volume?: number;
  high_24h?: number;
  low_24h?: number;
  price_change_24h?: number;
  price_change_percentage_24h?: number;
  market_cap_change_24h?: number;
  market_cap_change_percentage_24h?: number;
  circulating_supply?: number;
  total_supply?: number | null;
  max_supply?: number | null;
  ath?: number;
  ath_change_percentage?: number;
  ath_date?: string;
  atl?: number;
  atl_change_percentage?: number;
  atl_date?: string;
  last_updated?: string;
  [field: string]: unknown;
}

/** A currency-keyed map such as `{ usd: 1.0, eur: 0.92 }`. */
export type CurrencyValues = Record<string, number>;

/** `/coins/{id}` `market_data`; each price-like field is keyed by currency. */
export interface MarketData {
  current_price?: CurrencyValues;
  total_value_locked?: number | null;
  ath?: CurrencyValues;
  ath_change_percentage?: CurrencyValues;
  ath_date?: CurrencyValues;
  atl?: CurrencyValues;
  atl_change_percentage?: CurrencyValues;
  atl_date?: CurrencyValues;
  market_cap?: CurrencyValues;
  market_cap_rank?: number;
  fully_diluted_valuation?: CurrencyValues;
  total_volume?: CurrencyValues;
  high_24h?: CurrencyValues;
  low_24h?: CurrencyValues;
  price_change_24h?: number;
  price_change_percentage_24h?: number;
  price_change_percentage_7d?: number;
  price_change_percentage_14d?: number;
  price_change_percentage_30d?: number;
  price_change_percentage_60d?: number;
  price_change_percentage_200d?: number;
  price_change_percentage_1y?: number;
  market_cap_change_24h?: number;
  market_cap_change_percentage_24h?: number;
  circulating_supply?: number;
  total_supply?: number | null;
  max_supply?: number | null;
  last_updated?: string;
  [field: string]: unknown;
}

/** `/coins/{id}`, with localization, tickers, community and developer data left out. */
export interface CoinDetail {
  id: string;
  symbol: string;
  name: string;
  web_slug?: string;
  asset_platform_id?: string | null;
  /** Chain slug -> token contract address, for one entry per chain. */
  platforms?: Record<string, string>;
  /** Chain slug -> `{ decimal_place, contract_address }`. */
  detail_platforms?: Record<
    string,
    { decimal_place?: number; contract_address?: string; [field: string]: unknown }
  >;
  block_time_in_minutes?: number;
  hashing_algorithm?: string | null;
  categories?: string[];
  links?: Record<string, unknown>;
  image?: Record<string, string>;
  country_origin?: string;
  genesis_date?: string | null;
  sentiment_votes_up_percentage?: number;
  market_cap_rank?: number;
  market_data?: MarketData;
  last_updated?: string;
  [field: string]: unknown;
}

/** One entry of `/search/trending` (the `item` of each `coins[]` entry): a most-searched coin. */
export interface TrendingCoin {
  id: string;
  coin_id?: number;
  name: string;
  symbol: string;
  market_cap_rank?: number;
  thumb?: string;
  small?: string;
  large?: string;
  slug?: string;
  price_btc?: number;
  /** 0 is the most trending; add 1 for the displayed position. */
  score?: number;
  [field: string]: unknown;
}

/** One entry of `/search/trending` `nfts`. */
export interface TrendingNft {
  id: string;
  name: string;
  symbol?: string;
  thumb?: string;
  nft_contract_id?: number;
  native_currency_symbol?: string;
  floor_price?: number;
  floor_price_in_native_currency?: number;
  [field: string]: unknown;
}

/** One entry of `/search/trending` `categories`. */
export interface TrendingCategory {
  id: number;
  name: string;
  market_cap_1h_change?: number;
  slug?: string;
  coins_count?: number;
  [field: string]: unknown;
}

/** One `/search` match; `id` is what the other functions take as `coin_id`/`ids`. */
export interface SearchCoin {
  id: string;
  name: string;
  api_symbol?: string;
  symbol: string;
  market_cap_rank?: number | null;
  thumb?: string;
  large?: string;
  [field: string]: unknown;
}

/** One `/search` exchange match. */
export interface SearchExchange {
  id: string;
  name: string;
  market_type?: string;
  thumb?: string;
  large?: string;
  [field: string]: unknown;
}

/** One row of `/coins/categories`. Values are USD. */
export interface Category {
  id: string;
  name: string;
  market_cap?: number | null;
  market_cap_change_24h?: number | null;
  content?: string | null;
  top_3_coins?: string[];
  volume_24h?: number | null;
  updated_at?: string;
  [field: string]: unknown;
}

/** One row of `/exchanges`. */
export interface Exchange {
  id: string;
  name: string;
  year_established?: number | null;
  country?: string | null;
  description?: string;
  url?: string;
  image?: string;
  has_trading_incentive?: boolean;
  trust_score?: number;
  trust_score_rank?: number;
  trade_volume_24h_btc?: number;
  trade_volume_24h_btc_normalized?: number;
  [field: string]: unknown;
}

async function request(ctx: Ctx, path: string, params: Params = {}): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.COINGECKO_API_KEY) headers["x-cg-pro-api-key"] = ctx.env.COINGECKO_API_KEY;
  const base = (ctx.env.COINGECKO_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
  const res = await ctx.fetch(`${base}${path}${query(params)}`, { headers });
  const retry = res.headers["retry-after"];
  const label = `CoinGecko ${path}${retry ? ` (Retry-After: ${retry}s)` : ""}`;
  const text = await res.text();
  let body: any;
  try {
    body = text ? JSON.parse(text) : undefined;
  } catch {
    body = undefined;
  }
  // Errors are `{ status: { error_code, error_message } }` or `{ error }`.
  const message = body?.status?.error_message ?? body?.error;
  if (!res.ok) {
    throw new Error(`${label} failed (HTTP ${res.status}): ${message ?? text.slice(0, 1000)}`);
  }
  // Some CoinGecko endpoints answer 200 with an error body.
  if (typeof message === "string") throw new Error(`${label} failed: ${message}`);
  return body;
}

const bool = (value: boolean): string => String(value);

/**
 * Current price of one or more coins, keyed by coin id, with market cap, 24h volume and 24h
 * change per requested currency: `{ bitcoin: { usd: 67000, usd_market_cap: ..., usd_24h_vol: ...,
 * usd_24h_change: -1.2 } }`. `ids` is comma-separated CoinGecko coin ids. Unknown ids are
 * omitted from the result.
 */
export async function price(
  args: {
    ids: string;
    vs_currencies?: string;
    include_market_cap?: boolean;
    include_24hr_vol?: boolean;
    include_24hr_change?: boolean;
  },
  ctx: Ctx,
): Promise<{ prices: Record<string, CurrencyValues> }> {
  const body = await request(ctx, "/simple/price", {
    ids: args.ids,
    vs_currencies: args.vs_currencies ?? "usd",
    include_market_cap: bool(args.include_market_cap ?? true),
    include_24hr_vol: bool(args.include_24hr_vol ?? true),
    include_24hr_change: bool(args.include_24hr_change ?? true),
  });
  return { prices: body };
}

/**
 * Coins ranked by market cap, with price, market cap, 24h volume, 24h change, supply and ATH for
 * each in `vs_currency`. `order` is a CoinGecko sort key such as `market_cap_desc`,
 * `market_cap_asc`, `volume_desc`, `id_asc`, `gecko_desc`; `per_page` is at most 250. Page with
 * `page` starting at 1.
 */
export async function markets(
  args: { vs_currency?: string; order?: string; per_page?: number; page?: number; sparkline?: boolean },
  ctx: Ctx,
): Promise<{ coins: Coin[] }> {
  const body = await request(ctx, "/coins/markets", {
    vs_currency: args.vs_currency ?? "usd",
    order: args.order ?? "market_cap_desc",
    per_page: args.per_page ?? 100,
    page: args.page ?? 1,
    sparkline: bool(args.sparkline ?? false),
  });
  return { coins: body };
}

/**
 * Everything CoinGecko knows about one coin: contract addresses per chain (`detail_platforms`),
 * links, genesis date, categories, market cap rank and full `market_data` with per-currency prices,
 * ATH/ATL and 7d/14d/30d/200d/1y changes. `coin_id` is a CoinGecko id.
 */
export async function coin(args: { coin_id: string }, ctx: Ctx): Promise<{ coin: CoinDetail }> {
  const body = await request(ctx, `/coins/${encodeURIComponent(args.coin_id)}`, {
    localization: "false",
    tickers: "false",
    market_data: "true",
    community_data: "false",
    developer_data: "false",
  });
  return { coin: body };
}

/**
 * The coins and NFT collections CoinGecko users search for most in the last 24 hours, plus the
 * trending categories: each coin has the coin id, symbol, name, market cap rank and a `score`
 * (0 is the top position).
 */
export async function trending(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<{ coins: TrendingCoin[]; nfts: TrendingNft[]; categories: TrendingCategory[] }> {
  const body = await request(ctx, "/search/trending");
  // `coins` wraps each coin as `{ item }`; the other sections are already flat.
  return {
    coins: (body.coins ?? []).map((entry: { item: TrendingCoin }) => entry.item),
    nfts: body.nfts ?? [],
    categories: body.categories ?? [],
  };
}

/**
 * Search coins, exchanges, categories and NFT collections by name, symbol or contract address.
 * Use it to turn a symbol or name into the CoinGecko coin id the other functions need
 * (`coins[].id`); results are ordered by market cap rank, best match first.
 */
export async function search(
  args: { query: string },
  ctx: Ctx,
): Promise<{
  coins: SearchCoin[];
  exchanges: SearchExchange[];
  categories: TrendingCategory[];
  nfts: TrendingNft[];
}> {
  const body = await request(ctx, "/search", { query: args.query });
  return {
    coins: body.coins ?? [],
    exchanges: body.exchanges ?? [],
    categories: body.categories ?? [],
    nfts: body.nfts ?? [],
  };
}

/**
 * Historical price, market cap and 24h volume of one coin as `[unix_ms, value]` pairs:
 * `{ prices, market_caps, total_volumes }`. `days` is 1, 7, 14, 30, 90, 180, 365 or `max`;
 * CoinGecko picks the granularity (5-minute for 1 day, hourly up to 90 days, daily beyond).
 */
export async function market_chart(
  args: { coin_id: string; vs_currency?: string; days?: number | string },
  ctx: Ctx,
): Promise<{
  prices: [number, number][];
  market_caps: [number, number][];
  total_volumes: [number, number][];
}> {
  const body = await request(ctx, `/coins/${encodeURIComponent(args.coin_id)}/market_chart`, {
    vs_currency: args.vs_currency ?? "usd",
    days: String(args.days ?? 30),
  });
  return {
    prices: body.prices ?? [],
    market_caps: body.market_caps ?? [],
    total_volumes: body.total_volumes ?? [],
  };
}

/**
 * All coin categories (sectors such as `layer-1`, `meme-token`, `real-world-assets`) with USD
 * market cap, 24h market cap change, 24h volume and the top three coins by image URL. Not sorted;
 * sort or filter client-side.
 */
export async function categories(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<{ categories: Category[] }> {
  return { categories: await request(ctx, "/coins/categories") };
}

/**
 * Spot exchanges CoinGecko tracks, with trust score, trust score rank, 24h BTC volume (and its
 * normalized version), country and year established. `per_page` is at most 250; page with `page`.
 */
export async function exchanges(
  args: { per_page?: number; page?: number },
  ctx: Ctx,
): Promise<{ exchanges: Exchange[] }> {
  const body = await request(ctx, "/exchanges", {
    per_page: args.per_page ?? 100,
    page: args.page ?? 1,
  });
  return { exchanges: body };
}

/**
 * GET any CoinGecko Pro v3 path, for endpoints without a typed function (for example
 * `/coins/list`, `/global`, `/companies/public_treasury/bitcoin`). `endpoint` starts with `/`;
 * `params` become query parameters. The response body is returned as `data`.
 */
export async function raw(
  args: { endpoint: string; params?: Record<string, string | number | boolean> },
  ctx: Ctx,
): Promise<{ data: unknown }> {
  const endpoint = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  return { data: await request(ctx, endpoint, args.params ?? {}) };
}
