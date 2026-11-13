import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are DefiLlama's own objects, passed through. Endpoints are grouped by
// host: api.llama.fi (TVL, volumes, fees), stablecoins.llama.fi and
// bridges.llama.fi; pro endpoints use pro-api.llama.fi with the API key in the
// path. Field names are DefiLlama's (https://defillama.com/docs/api).
//
// `/protocols` (the full ~9000-row, 9.6 MB list) is not exposed: parsing it
// exceeds the Code Mode runtime's 64 MiB heap. Use `get_tvl`/`get_protocol` with
// a known slug, or the DEX, fee and open-interest overviews for discovery.

const MAIN_BASE = "https://api.llama.fi";
const STABLECOINS_BASE = "https://stablecoins.llama.fi";
const BRIDGES_BASE = "https://bridges.llama.fi";
const COINS_BASE = "https://coins.llama.fi";
const PRO_BASE = "https://pro-api.llama.fi";

/**
 * Which DefiLlama host a request goes to: `main` (api.llama.fi), `stablecoins`, `bridges`, or
 * `coins` (coins.llama.fi). `raw_request` only; typed functions pick their own host.
 */
export type Base = "main" | "stablecoins" | "bridges" | "coins";

const BASES: Record<Base, string> = {
  main: MAIN_BASE,
  stablecoins: STABLECOINS_BASE,
  bridges: BRIDGES_BASE,
  coins: COINS_BASE,
};

/** An upstream object whose fields this connector does not pin down. */
export type JsonObject = Record<string, unknown>;

/** A stablecoin with circulating supply per peg currency (USD under `peggedUSD`). */
export interface Stablecoin {
  id: string;
  name: string;
  symbol: string;
  gecko_id?: string | null;
  pegType?: string;
  pegMechanism?: string;
  price?: number;
  priceSource?: string;
  circulating?: Record<string, number>;
  circulatingPrevDay?: Record<string, number>;
  circulatingPrevWeek?: Record<string, number>;
  circulatingPrevMonth?: Record<string, number>;
  /** Chain name to that chain's circulating/bridged breakdown. */
  chainCirculating?: Record<string, unknown>;
  chains?: string[];
  [field: string]: unknown;
}

/** One daily data point of a chain's stablecoin balance history. */
export interface StablecoinChainBalancePoint {
  /** Unix seconds. */
  date: number;
  circulating: Record<string, number>;
  bridgedTo?: Record<string, unknown>;
  [field: string]: unknown;
}

/** A stablecoin's history on one chain (`chainBalances[chain]`). */
export interface StablecoinChainBalances {
  tokens?: StablecoinChainBalancePoint[];
  [field: string]: unknown;
}

/** One stablecoin by ID, with a per-chain historical breakdown. */
export interface StablecoinDetail extends Stablecoin {
  address?: string | null;
  url?: string;
  description?: string;
  /** Chain name to its `tokens` history (large for large coins). */
  chainBalances?: Record<string, StablecoinChainBalances>;
  currentChainBalances?: Record<string, unknown>;
  tokens?: unknown[];
  [field: string]: unknown;
}

/** One daily point of a stablecoin market-cap chart. */
export interface StablecoinChartPoint {
  /** Unix seconds, as a string. */
  date: string;
  totalCirculating?: Record<string, number>;
  totalCirculatingUSD?: Record<string, number>;
  totalMintedUSD?: number;
  totalBridgedToUSD?: number;
  [field: string]: unknown;
}

/** Stablecoin market cap of one chain. */
export interface StablecoinChain {
  name: string;
  totalCirculatingUSD?: Record<string, number>;
  [field: string]: unknown;
}

/** One daily point of the all-stablecoin price history. */
export interface StablecoinPricePoint {
  /** Unix seconds. */
  date: number;
  /** Stablecoin id or slug to its price in USD. */
  prices: Record<string, number>;
  [field: string]: unknown;
}

/** One point of a protocol's historical TVL. */
export interface TvlPoint {
  /** Unix seconds. */
  date: number;
  /** TVL in USD. */
  totalLiquidityUSD: number;
  [field: string]: unknown;
}

/** One protocol by slug, with its historical TVL. */
export interface ProtocolDetail {
  id?: string;
  name: string;
  slug?: string;
  description?: string;
  url?: string;
  category?: string | null;
  chains?: string[];
  symbol?: string;
  twitter?: string;
  /** Daily TVL history. */
  tvl?: TvlPoint[];
  currentChainTvls?: Record<string, number>;
  /** Chain (and chain-token) name to its histories. */
  chainTvls?: Record<string, { tvl?: TvlPoint[]; tokensInUsd?: unknown[]; tokens?: unknown[]; [field: string]: unknown }>;
  tokens?: unknown[];
  mcap?: number | null;
  [field: string]: unknown;
}

/** A chain row from `/v2/chains`. */
export interface ChainSummary {
  name: string;
  /** Current chain TVL, USD. */
  tvl?: number;
  tokenSymbol?: string | null;
  gecko_id?: string | null;
  chainId?: number;
  cmcId?: string | null;
  [field: string]: unknown;
}

/** One daily point of a chain's historical TVL. */
export interface ChainTvlPoint {
  /** Unix seconds. */
  date: number;
  tvl: number;
  [field: string]: unknown;
}

/** One protocol row in a DEX/perp/fee/open-interest overview. */
export interface DimensionProtocol {
  name: string;
  displayName?: string;
  slug?: string;
  defillamaId?: string;
  category?: string | null;
  chains?: string[];
  protocolType?: string;
  /** USD volume, fees or open interest over the trailing window. */
  total24h?: number;
  total48hto24h?: number;
  total7d?: number;
  total30d?: number;
  total1y?: number;
  totalAllTime?: number;
  change_1d?: number;
  change_7d?: number;
  /** Chain to label to amount, when the request did not exclude breakdowns. */
  breakdown24h?: Record<string, Record<string, number>> | null;
  breakdown30d?: Record<string, Record<string, number>> | null;
  [field: string]: unknown;
}

/** Overview totals plus per-protocol rows for DEX volumes, perp volumes, fees or open interest. */
export interface DimensionsOverview {
  protocols: DimensionProtocol[];
  /** USD totals over the trailing window. */
  total24h?: number;
  total48hto24h?: number;
  total7d?: number;
  total30d?: number;
  total1y?: number;
  totalAllTime?: number;
  change_1d?: number;
  change_7d?: number;
  allChains?: string[];
  chain?: string | null;
  totalDataChart?: [number, number][];
  totalDataChartBreakdown?: unknown[];
  breakdown24h?: Record<string, Record<string, number>> | null;
  breakdown30d?: Record<string, Record<string, number>> | null;
  [field: string]: unknown;
}

/** One protocol's volume, fee or open-interest summary. */
export interface DimensionSummary {
  id?: string;
  name: string;
  slug?: string;
  category?: string | null;
  chains?: string[];
  protocolType?: string;
  /** USD totals over the trailing window. */
  total24h?: number;
  total48hto24h?: number;
  total7d?: number;
  total30d?: number;
  total1y?: number;
  totalAllTime?: number;
  change_1d?: number;
  change_7d?: number;
  totalDataChart?: [number, number][];
  totalDataChartBreakdown?: unknown[];
  [field: string]: unknown;
}

/** A bridge from the bridge list. */
export interface Bridge {
  id?: number | string;
  name?: string;
  displayName?: string;
  chains?: string[];
  destinationChain?: string;
  /** USD volumes; `lastDailyVolume` is the most recent day. */
  lastDailyVolume?: number;
  dayBeforeLastVolume?: number;
  weeklyVolume?: number;
  monthlyVolume?: number;
  volumePrevDay?: number;
  [field: string]: unknown;
}

const CHART_EXCLUDED: Record<string, string> = {
  excludeTotalDataChart: "true",
  excludeTotalDataChartBreakdown: "true",
};

/** Perpetuals venues that DefiLlama lists under derivatives, never under `/dexs`. */
const PERP_VENUES = new Set([
  "dydx", "dydx-v4", "dydx-chain", "dydx-v3", "hyperliquid", "hyperliquid-perps",
  "gmx", "gmx-v1", "gmx-v2", "aster", "lighter", "vertex", "vertex-perps",
  "synfutures", "kwenta", "drift", "drift-v2", "paradex", "mango-markets", "apex",
  "apex-pro", "level-finance",
]);

function looksLikePerps(slug: string): boolean {
  return PERP_VENUES.has((slug || "").trim().toLowerCase());
}

/** The pro key placeholder, present only where the runner/locally configured it. */
function apiKey(ctx: Ctx): string | undefined {
  const key = ctx.env.DEFILLAMA_API_KEY?.trim();
  return key ? key : undefined;
}

interface RequestOptions {
  /** A pro endpoint: the key goes into the URL path or the `apikey` query parameter. */
  pro?: boolean;
  /** Host to use; defaults to api.llama.fi. */
  base?: Base;
  params?: Record<string, string | number | boolean | undefined>;
}

async function request(ctx: Ctx, endpoint: string, options: RequestOptions = {}): Promise<any> {
  if (!endpoint.startsWith("/")) throw new Error(`endpoint must start with "/": ${JSON.stringify(endpoint)}`);
  const params: Record<string, string | number | boolean | undefined> = { ...options.params };
  let url: string;
  if (options.pro) {
    const key = apiKey(ctx);
    if (!key) throw new Error("DEFILLAMA_API_KEY is required for this DefiLlama pro endpoint");
    if (options.base) {
      url = `${BASES[options.base]}${endpoint}`;
      params.apikey = key;
    } else {
      url = `${PRO_BASE}/${key}${endpoint}`;
    }
  } else {
    url = `${options.base ? BASES[options.base] : MAIN_BASE}${endpoint}`;
  }
  const res = await ctx.fetch(`${url}${query(params)}`, { headers: { accept: "application/json" } });
  return readJson(res, "DefiLlama");
}

/**
 * Escape hatch for any DefiLlama endpoint not covered by a typed function, for example
 * `/overview/exchanges`, `/prices/current/coingecko:ethereum` (base "coins"), `/yields/pools`.
 * `endpoint` starts with `/`; `base` picks the host (main, stablecoins, bridges, coins) and
 * `pro` routes through pro-api.llama.fi with the key in the path. Returns `{ data }` exactly as
 * the API sends it.
 */
export async function raw_request(
  args: { endpoint: string; params?: Record<string, string | number | boolean>; pro?: boolean; base?: Base },
  ctx: Ctx,
): Promise<{ data: unknown }> {
  const data = await request(ctx, args.endpoint, { params: args.params, pro: args.pro, base: args.base });
  return { data };
}

/**
 * Every tracked stablecoin with symbol, peg type/mechanism, price and circulating supply per peg
 * currency (`circulating.peggedUSD` for USD). No pagination: the whole list is returned, so sort
 * or filter client-side.
 */
export async function list_stablecoins(args: {}, ctx: Ctx): Promise<{ stablecoins: Stablecoin[] }> {
  const body = await request(ctx, "/stablecoins", { base: "stablecoins" });
  return { stablecoins: body.peggedAssets ?? [] };
}

/**
 * One stablecoin by DefiLlama id (for example "1" for USDT), including `chainBalances`: chain name
 * to daily `tokens[]` points with circulating amounts. Large coins carry years of per-chain
 * history, so the response can be tens of MB and exceed this runtime's 10 MiB result limit; the
 * call then fails. Use `list_stablecoins` for current sizes.
 */
export async function get_stablecoin(args: { asset_id: string }, ctx: Ctx): Promise<StablecoinDetail> {
  return request(ctx, `/stablecoin/${encodeURIComponent(args.asset_id)}`, { base: "stablecoins" });
}

/**
 * Daily stablecoin market-cap history: circulating per peg currency (`totalCirculating.peggedUSD`)
 * and in USD (`totalCirculatingUSD`). Without `chain` it is all chains combined (`/stablecoincharts/all`,
 * 2017 onward); with `chain` (for example "ethereum", "arbitrum") it is one chain.
 */
export async function get_stablecoin_charts(
  args: { chain?: string },
  ctx: Ctx,
): Promise<{ charts: StablecoinChartPoint[] }> {
  const path = args.chain ? `/stablecoincharts/${encodeURIComponent(args.chain)}` : "/stablecoincharts/all";
  return { charts: await request(ctx, path, { base: "stablecoins" }) };
}

/** Current stablecoin market cap for every chain, as `totalCirculatingUSD` per peg currency. */
export async function get_stablecoin_chains(args: {}, ctx: Ctx): Promise<{ chains: StablecoinChain[] }> {
  return { chains: await request(ctx, "/stablecoinchains", { base: "stablecoins" }) };
}

/**
 * Daily stablecoin price history, oldest first: each point holds `prices`, a map from stablecoin
 * id or slug to its USD price. Several MB of history.
 */
export async function get_stablecoin_prices(args: {}, ctx: Ctx): Promise<{ prices: StablecoinPricePoint[] }> {
  return { prices: await request(ctx, "/stablecoinprices", { base: "stablecoins" }) };
}

/**
 * One TVL protocol by slug (for example "aave", "uniswap"), with daily TVL history in `tvl`,
 * current TVL per chain in `currentChainTvls` and per-chain histories in `chainTvls`. Perpetuals
 * venues (hyperliquid, dydx-v4, gmx-v2, ...) have no TVL entry here; use `get_derivatives_summary`.
 * `tvl` points are `{ date, totalLiquidityUSD }`. Large protocols (aave, lido) return more than
 * 10 MiB and exceed this runtime's result limit; use `get_tvl` for their current TVL.
 */
export async function get_protocol(args: { slug: string }, ctx: Ctx): Promise<ProtocolDetail> {
  return request(ctx, `/protocol/${encodeURIComponent(args.slug.trim())}`);
}

/** Current TVL of a protocol in USD, from `/tvl/{slug}` (a single number upstream). */
export async function get_tvl(args: { protocol: string }, ctx: Ctx): Promise<{ tvl: number }> {
  return { tvl: await request(ctx, `/tvl/${encodeURIComponent(args.protocol.trim())}`) };
}

/** Every chain with its current DeFi TVL in USD, gas token symbol and chain id. */
export async function list_chains(args: {}, ctx: Ctx): Promise<{ chains: ChainSummary[] }> {
  return { chains: await request(ctx, "/v2/chains") };
}

/**
 * Daily TVL history for one chain, or for all chains combined when `chain` is omitted: points of
 * `{ date, tvl }` with `date` in Unix seconds, oldest first.
 */
export async function get_chain_tvl(
  args: { chain?: string },
  ctx: Ctx,
): Promise<{ history: ChainTvlPoint[] }> {
  const path = args.chain ? `/v2/historicalChainTvl/${encodeURIComponent(args.chain)}` : "/v2/historicalChainTvl";
  return { history: await request(ctx, path) };
}

/**
 * Protocol inflows and outflows over the day ending at `timestamp` (Unix seconds). Pro endpoint:
 * needs a DefiLlama API key with pro access and takes the key from DEFILLAMA_API_KEY.
 */
export async function get_protocol_inflows(
  args: { protocol: string; timestamp: number },
  ctx: Ctx,
): Promise<JsonObject> {
  return request(ctx, `/api/inflows/${encodeURIComponent(args.protocol.trim())}/${Math.trunc(args.timestamp)}`, {
    pro: true,
  });
}

/**
 * Spot DEX volume overview: totals (`total24h`, `total7d`, `total30d`, `total1y`, changes) and a
 * `protocols` row per DEX with the same windows. `chain` (for example "ethereum", "solana") narrows
 * it to one chain. Perp venues (hyperliquid, dydx, gmx, ...) are not here; use
 * `get_derivatives_volumes`. Chart series are excluded by default to fit the response limit; set
 * `include_chart` for the full `totalDataChart` (tens of MB, likely too large to return).
 */
export async function get_dex_volumes(
  args: { chain?: string; include_chart?: boolean },
  ctx: Ctx,
): Promise<DimensionsOverview> {
  const path = args.chain ? `/overview/dexs/${encodeURIComponent(args.chain)}` : "/overview/dexs";
  return request(ctx, path, { params: args.include_chart ? {} : CHART_EXCLUDED });
}

/**
 * Volume summary for one spot DEX (for example "uniswap", "curve-dex"): 24h/7d/30d/1y volume,
 * change, chains and `totalDataChart`. Perp venues return "not found" here; use
 * `get_derivatives_summary` instead (the error says so when the slug is a known perp venue).
 */
export async function get_dex_summary(args: { protocol: string }, ctx: Ctx): Promise<DimensionSummary> {
  const slug = args.protocol.trim();
  try {
    return await request(ctx, `/summary/dexs/${encodeURIComponent(slug)}`);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (/not found/i.test(message) && looksLikePerps(slug)) {
      throw new Error(
        `${message}\nHint: \`${slug}\` looks like a perpetuals venue, which DefiLlama lists under ` +
          `/derivatives, not /dexs. Use get_derivatives_summary({ protocol: "${slug}" }) instead.`,
      );
    }
    throw error;
  }
}

/**
 * Perpetual-futures venue volume overview: totals and a `protocols` row per venue (dYdX,
 * Hyperliquid, GMX, Aster, Lighter, Drift, Vertex, ...) with 24h/7d/30d/1y volume and changes.
 * `chain` narrows to one chain. Chart series are excluded by default; `include_chart` requests the
 * full `totalDataChart`. The free DefiLlama plan currently answers these endpoints with HTTP 402.
 */
export async function get_derivatives_volumes(
  args: { chain?: string; include_chart?: boolean },
  ctx: Ctx,
): Promise<DimensionsOverview> {
  const path = args.chain
    ? `/overview/derivatives/${encodeURIComponent(args.chain)}`
    : "/overview/derivatives";
  return request(ctx, path, { params: args.include_chart ? {} : CHART_EXCLUDED });
}

/**
 * Volume summary for one perpetuals venue (for example "hyperliquid", "lighter", "gmx-v2",
 * "dydx-v4"): 24h/7d/30d/1y volume, change, chains and `totalDataChart`.
 */
export async function get_derivatives_summary(
  args: { protocol: string },
  ctx: Ctx,
): Promise<DimensionSummary> {
  return request(ctx, `/summary/derivatives/${encodeURIComponent(args.protocol.trim())}`);
}

/**
 * Perpetuals open-interest overview: totals (USD) and a `protocols` row per venue with 24h/30d/1y
 * open interest and changes. `chain` narrows to one chain. Chart series are excluded by default;
 * `include_chart` requests the full `totalDataChart`.
 */
export async function get_open_interest_overview(
  args: { chain?: string; include_chart?: boolean },
  ctx: Ctx,
): Promise<DimensionsOverview> {
  const path = args.chain
    ? `/overview/open-interest/${encodeURIComponent(args.chain)}`
    : "/overview/open-interest";
  return request(ctx, path, { params: args.include_chart ? {} : CHART_EXCLUDED });
}

/**
 * Open-interest summary for one perpetuals venue (for example "hyperliquid", "lighter",
 * "dydx-v4"): 24h/7d/30d/1y open interest, change, chains and `totalDataChart`.
 */
export async function get_open_interest_summary(
  args: { protocol: string },
  ctx: Ctx,
): Promise<DimensionSummary> {
  return request(ctx, `/summary/open-interest/${encodeURIComponent(args.protocol.trim())}`);
}

/**
 * All bridges with their chains and USD volumes (`lastDailyVolume` is the latest day). The whole
 * list is returned; sort and slice client-side. The free DefiLlama plan currently answers the
 * bridge endpoints with HTTP 402.
 */
export async function list_bridges(args: {}, ctx: Ctx): Promise<{ bridges: Bridge[] }> {
  const body = await request(ctx, "/bridges", { base: "bridges" });
  return { bridges: body.bridges ?? [] };
}

/**
 * One bridge by DefiLlama bridge id, with its volume history and chains. The free DefiLlama plan
 * currently answers the bridge endpoints with HTTP 402.
 */
export async function get_bridge(args: { bridge_id: string }, ctx: Ctx): Promise<Bridge> {
  return request(ctx, `/bridge/${encodeURIComponent(args.bridge_id.trim())}`, { base: "bridges" });
}

/**
 * Daily bridge volume history for one chain (for example "ethereum", "arbitrum"): upstream points
 * carrying deposits, withdrawals and volume figures. `chain` is required (the Python tool's
 * no-chain branch returned the whole bridge list, which `list_bridges` already covers). The free
 * DefiLlama plan currently answers the bridge endpoints with HTTP 402.
 */
export async function get_bridge_volumes(
  args: { chain: string },
  ctx: Ctx,
): Promise<{ volumes: JsonObject[] }> {
  return {
    volumes: await request(ctx, `/bridgevolume/${encodeURIComponent(args.chain.trim())}`, { base: "bridges" }),
  };
}

/**
 * Bridge deposits, withdrawals and volume for one UTC day and chain. `timestamp` is Unix seconds
 * for any time on that day; `chain` is a chain name such as "ethereum". The free DefiLlama plan
 * currently answers the bridge endpoints with HTTP 402.
 */
export async function get_bridge_day_stats(
  args: { timestamp: number; chain: string },
  ctx: Ctx,
): Promise<JsonObject> {
  return request(
    ctx,
    `/bridgedaystats/${Math.trunc(args.timestamp)}/${encodeURIComponent(args.chain.trim())}`,
    { base: "bridges" },
  );
}

/**
 * Transactions handled by one bridge, as the API sends them. Pro endpoint: needs a DefiLlama API
 * key with pro access; the key goes in the `apikey` query parameter.
 */
export async function get_bridge_transactions(
  args: { bridge_id: string },
  ctx: Ctx,
): Promise<JsonObject> {
  return request(ctx, `/transactions/${encodeURIComponent(args.bridge_id.trim())}`, {
    base: "bridges",
    pro: true,
  });
}

/**
 * Protocol fee and revenue overview: totals (USD, `total24h` is the latest day) and a `protocols`
 * row per protocol with `total24h`, `total7d`, `total30d`, `total1y`, `totalAllTime` and changes.
 * `chain` narrows to one chain. Chart series are excluded by default; `include_chart` requests the
 * full `totalDataChart` (about 30 MB, too large to return).
 */
export async function get_fees(
  args: { chain?: string; include_chart?: boolean },
  ctx: Ctx,
): Promise<DimensionsOverview> {
  const path = args.chain ? `/overview/fees/${encodeURIComponent(args.chain)}` : "/overview/fees";
  return request(ctx, path, { params: args.include_chart ? {} : CHART_EXCLUDED });
}

/**
 * Fees and revenue summary for one protocol (for example "uniswap", "aave"): 24h/7d/30d/1y/AllTime
 * totals, change, chains and `totalDataChart`.
 */
export async function get_protocol_fees(
  args: { protocol: string },
  ctx: Ctx,
): Promise<DimensionSummary> {
  return request(ctx, `/summary/fees/${encodeURIComponent(args.protocol.trim())}`);
}
