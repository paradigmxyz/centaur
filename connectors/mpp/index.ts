import type { Ctx } from "../centaur";

// Two halves. The service catalog at mpp.dev is public and free. Everything else is a
// pay-per-query MPP endpoint: the request carries the Tempo wallet key from `MPP_PRIVATE_KEY`
// (the runner's environment) in `X-MPP-Key` and the gateway settles the price on the wire, so
// those functions throw unless that variable is set. Which upstream is queried is per vendor, so
// vendor payloads are passed through as the vendor sends them.

const DEFAULT_CATALOG_URL = "https://mpp.dev/api/services";
const PARALLEL_URL = "https://parallelmpp.dev/api/search";
const COINGECKO_URL = "https://coingecko.mpp.paywithlocus.com/coingecko";
const CODEX_URL = "https://graph.codex.io/graphql";
const ALLIUM_URL = "https://agents.allium.so/api/v1/developer/wallet/balances";
const DUNE_URL = "https://api.dune.com/api/v1";

const MAX_SERVICE_LIMIT = 100;
const DEFAULT_SERVICE_LIMIT = 20;

// Token symbols and full names that resolve to a CoinGecko id without a paid search.
const COINGECKO_ID_MAP: Record<string, string> = {
  BTC: "bitcoin", ETH: "ethereum", SOL: "solana", ADA: "cardano", DOT: "polkadot",
  AVAX: "avalanche-2", LINK: "chainlink", MATIC: "matic-network", LTC: "litecoin",
  DOGE: "dogecoin", XRP: "ripple", TON: "the-open-network", UNI: "uniswap", AAVE: "aave",
  TIA: "celestia", ARB: "arbitrum", OP: "optimism", APT: "aptos", SUI: "sui",
  SEI: "sei-network", HYPE: "hyperliquid", JUP: "jupiter-exchange-solana",
  JTO: "jito-governance-token", RAY: "raydium", PENDLE: "pendle", EIGEN: "eigenlayer",
  STRK: "starknet", NEAR: "near", ATOM: "cosmos", FIL: "filecoin",
  INJ: "injective-protocol", TRX: "tron", PEPE: "pepe", SHIB: "shiba-inu", BONK: "bonk",
};

const TOKEN_NAME_MAP: Record<string, string> = {
  SOLANA: "SOL", BITCOIN: "BTC", ETHEREUM: "ETH", CARDANO: "ADA", POLKADOT: "DOT",
  AVALANCHE: "AVAX", CHAINLINK: "LINK", POLYGON: "MATIC", LITECOIN: "LTC", DOGECOIN: "DOGE",
  RIPPLE: "XRP", TONCOIN: "TON", UNISWAP: "UNI", CELESTIA: "TIA", ARBITRUM: "ARB",
  OPTIMISM: "OP", APTOS: "APT", HYPERLIQUID: "HYPE", JUPITER: "JUP", JITO: "JTO",
  RAYDIUM: "RAY", EIGENLAYER: "EIGEN", STARKNET: "STRK", MONAD: "MON", NOBLE: "NOBLE",
};

/** One service as the catalog summary endpoint returns it. */
export interface ServiceSummary {
  id: string;
  name: string;
  description: string;
  /** Base URL of the paid service. */
  service_url: string;
  categories: string[];
  tags: string[];
  status: string;
  /** How many of the service's endpoints are paid. */
  paid_endpoints: number;
  [field: string]: unknown;
}

/** One endpoint of a catalog service, with the price the gateway charges for it. */
export interface ServiceEndpoint {
  method?: string;
  path?: string;
  /** Absent for free endpoints; `amount` is in the payment token's smallest unit. */
  payment?: { intent?: string; method?: string; amount?: string; [field: string]: unknown } | null;
  [field: string]: unknown;
}

/** A complete catalog record, as `get_service` returns it. */
export interface Service {
  id?: string;
  name?: string;
  description?: string;
  url?: string;
  serviceUrl?: string;
  icon?: string;
  categories?: string[];
  tags?: string[];
  status?: string;
  paidEndpoints?: number;
  endpoints?: ServiceEndpoint[];
  [field: string]: unknown;
}

/** A CoinGecko price row; `usd_*` fields appear when they were requested. */
export interface CoinGeckoPrice {
  usd?: number;
  usd_24h_change?: number;
  usd_24h_vol?: number;
  usd_market_cap?: number;
  [field: string]: unknown;
}

/** A CoinGecko search match. */
export interface CoinGeckoCoin {
  id?: string;
  name?: string;
  symbol?: string;
  market_cap_rank?: number;
  [field: string]: unknown;
}

/** A CoinGecko trending entry; the coin itself is under `item`. */
export interface TrendingCoin {
  item?: {
    id?: string;
    coin_id?: number;
    name?: string;
    symbol?: string;
    market_cap_rank?: number;
    data?: Record<string, unknown>;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** CoinGecko market chart series: `[timestamp in milliseconds, value]` points. */
export interface CoinGeckoMarketChart {
  prices?: [number, number][];
  market_caps?: [number, number][];
  total_volumes?: [number, number][];
  [field: string]: unknown;
}

/** CoinGecko's global market data. */
export interface CoinGeckoGlobal {
  total_market_cap?: Record<string, number>;
  total_volume?: Record<string, number>;
  /** Share of total market cap per symbol, such as `btc`. */
  market_cap_percentage?: Record<string, number>;
  [field: string]: unknown;
}

/** One web search hit from Parallel. */
export interface WebSearchResult {
  title?: string;
  url?: string;
  text?: string;
  publishedDate?: string;
  [field: string]: unknown;
}

/** One Codex DEX pool/token match. */
export interface CodexTokenResult {
  token?: { name?: string; symbol?: string; address?: string; networkId?: number; [field: string]: unknown };
  priceUSD?: number | string;
  volume24?: number | string;
  liquidity?: number | string;
  marketCap?: number | string;
  [field: string]: unknown;
}

/** One token balance of a wallet from Allium. */
export interface WalletBalance {
  token_name?: string;
  token_symbol?: string;
  balance?: number | string;
  value_usd?: number | string;
  chain?: string;
  [field: string]: unknown;
}

/** Result of submitting a Dune SQL query; poll it with `dune_execution_results`. */
export interface DuneExecution {
  execution_id?: string;
  /** Such as `QUERY_STATE_PENDING`, `QUERY_STATE_EXECUTING`, `QUERY_STATE_COMPLETED`. */
  state?: string;
  [field: string]: unknown;
}

/** Result poll for a Dune execution. */
export interface DuneExecutionResult extends DuneExecution {
  is_execution_finished?: boolean;
  result?: {
    rows?: Record<string, unknown>[];
    metadata?: { column_names?: string[]; row_count?: number; [field: string]: unknown };
    [field: string]: unknown;
  };
  error?: unknown;
}

interface CatalogBody {
  services?: unknown;
}

function catalogUrl(ctx: Ctx): string {
  return ctx.env.MPP_SERVICE_CATALOG_URL || DEFAULT_CATALOG_URL;
}

/** Fetch the catalog, rejecting payloads that are not a list of service objects. */
async function catalog(ctx: Ctx): Promise<Record<string, unknown>[]> {
  const res = await ctx.fetch(catalogUrl(ctx), { headers: { accept: "application/json" } });
  if (!res.ok) throw new Error(`MPP service catalog returned HTTP ${res.status}`);
  const text = await res.text();
  let payload: CatalogBody;
  try {
    payload = JSON.parse(text) as CatalogBody;
  } catch {
    throw new Error("MPP service catalog returned invalid JSON");
  }
  const services = payload.services;
  if (!Array.isArray(services) || !services.every((service) => typeof service === "object" && service !== null)) {
    throw new Error("MPP service catalog has an invalid services list");
  }
  return services as Record<string, unknown>[];
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.map(String) : [];
}

function matches(
  service: Record<string, unknown>,
  filters: { query?: string; category?: string; tag?: string },
): boolean {
  if (filters.category !== undefined) {
    const categories = strings(service.categories);
    if (!categories.some((value) => value.toLowerCase() === filters.category!.toLowerCase())) return false;
  }
  if (filters.tag !== undefined) {
    const tags = strings(service.tags);
    if (!tags.some((value) => value.toLowerCase() === filters.tag!.toLowerCase())) return false;
  }
  if (filters.query === undefined) return true;
  const haystack: unknown[] = [
    service.id,
    service.name,
    service.description,
    ...strings(service.categories),
    ...strings(service.tags),
  ];
  const needle = filters.query.toLowerCase();
  return haystack.some(
    (value) => value !== undefined && value !== null && String(value).toLowerCase().includes(needle),
  );
}

function summarize(service: Record<string, unknown>): ServiceSummary {
  const endpoints = Array.isArray(service.endpoints) ? (service.endpoints as ServiceEndpoint[]) : [];
  const declared = service.paidEndpoints;
  const paid =
    typeof declared === "number"
      ? declared
      : endpoints.filter((endpoint) => endpoint && endpoint.payment !== undefined && endpoint.payment !== null).length;
  return {
    id: String(service.id ?? ""),
    name: String(service.name ?? ""),
    description: String(service.description ?? ""),
    service_url: String(service.serviceUrl ?? service.url ?? ""),
    categories: strings(service.categories),
    tags: strings(service.tags),
    status: String(service.status ?? ""),
    paid_endpoints: paid,
  };
}

function trimmed(value: string | undefined): string | undefined {
  const text = value?.trim();
  return text ? text : undefined;
}

function checkLimit(limit: number): number {
  if (!(limit >= 1 && limit <= MAX_SERVICE_LIMIT)) {
    throw new Error(`limit must be between 1 and ${MAX_SERVICE_LIMIT}`);
  }
  return limit;
}

/** A paid MPP request: the Tempo wallet key authorizes the charge the gateway collects. */
async function pay(
  ctx: Ctx,
  url: string,
  options: { method?: "GET" | "POST"; body?: unknown; label: string },
): Promise<any> {
  const key = ctx.env.MPP_PRIVATE_KEY;
  if (!key) {
    throw new Error(
      "MPP_PRIVATE_KEY is not set; paid MPP queries need a Tempo wallet key in the runner environment",
    );
  }
  const headers: Record<string, string> = { accept: "application/json", "X-MPP-Key": key };
  const payload = options.body === undefined ? undefined : JSON.stringify(options.body);
  if (payload !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(url, { method: options.method ?? "POST", headers, body: payload });
  const text = await res.text();
  if (!res.ok) {
    // MPP gateways answer a refused payment with an RFC 7807 problem body.
    let message = text.slice(0, 1000);
    try {
      const problem = JSON.parse(text) as { detail?: string; title?: string; error?: string; message?: string };
      message = problem.detail ?? problem.title ?? problem.error ?? problem.message ?? message;
    } catch {
      // Not JSON: keep the raw text.
    }
    throw new Error(`${options.label} returned HTTP ${res.status}: ${message}`);
  }
  return JSON.parse(text);
}

/**
 * Public MPP service catalog, filtered by free-form text (`query`, matched against id, name,
 * description, categories and tags) and exact `category` and `tag`. Returns a summary per
 * service: url, categories, tags, status and how many endpoints are paid. `limit` is 20 by
 * default and at most 100. Needs no key.
 */
export async function list_services(
  args: { query?: string; category?: string; tag?: string; limit?: number },
  ctx: Ctx,
): Promise<{ services: ServiceSummary[] }> {
  const limit = checkLimit(args.limit ?? DEFAULT_SERVICE_LIMIT);
  const filters = {
    query: trimmed(args.query),
    category: trimmed(args.category),
    tag: trimmed(args.tag),
  };
  const services = await catalog(ctx);
  return { services: services.filter((service) => matches(service, filters)).slice(0, limit).map(summarize) };
}

/**
 * Search the public MPP service catalog: same as `list_services` with a required `query`, plus
 * optional exact `category` and `tag` filters.
 */
export async function search_services(
  args: { query: string; category?: string; tag?: string; limit?: number },
  ctx: Ctx,
): Promise<{ services: ServiceSummary[] }> {
  return list_services(args, ctx);
}

/**
 * One complete catalog record by exact service id or unambiguous service name: endpoint paths,
 * the payment each endpoint charges (`payment.amount`, `payment.method`), and contact and status
 * metadata. Throws when the name is ambiguous or unknown.
 */
export async function get_service(args: { service: string }, ctx: Ctx): Promise<{ service: Service }> {
  const identifier = args.service.trim();
  if (!identifier) throw new Error("service id or name is required");
  const services = await catalog(ctx);
  const exact = services.find((item) => item.id === identifier);
  if (exact) return { service: exact as Service };
  const byName = services.filter(
    (item) => typeof item.name === "string" && item.name.toLowerCase() === identifier.toLowerCase(),
  );
  if (byName.length === 1) return { service: byName[0] as Service };
  if (byName.length > 1) throw new Error(`MPP service name "${identifier}" is ambiguous; use its id`);
  throw new Error(`MPP service "${identifier}" was not found`);
}

/**
 * Resolve a token symbol or name (`SOL`, `Solana`) to its CoinGecko id (`solana`) for
 * `coingecko_simple_price` and friends. Common tokens resolve from a built-in table for free; the
 * rest use one paid CoinGecko search and return `null` when nothing matches. Paid only for tokens
 * outside the built-in table.
 */
export async function coingecko_token_id(
  args: { token: string },
  ctx: Ctx,
): Promise<{ coin_id: string | null }> {
  const normalized = TOKEN_NAME_MAP[args.token.toUpperCase()] ?? args.token;
  const known = COINGECKO_ID_MAP[normalized.toUpperCase()];
  if (known) return { coin_id: known };
  const body = await pay(ctx, `${COINGECKO_URL}/search`, {
    body: { query: normalized },
    label: "MPP CoinGecko search",
  });
  const coins = (body.data?.coins ?? body.coins ?? []) as CoinGeckoCoin[];
  if (!coins.length) return { coin_id: null };
  const best = coins.reduce((left, right) =>
    (left.market_cap_rank ?? 9999) <= (right.market_cap_rank ?? 9999) ? left : right,
  );
  return { coin_id: best.id ?? null };
}

/**
 * Current USD price for one or more CoinGecko ids (`bitcoin`, `ethereum`, ...), comma-separated
 * or as an array, with 24h change, 24h volume and market cap. Returns a map of coin id to
 * `{ usd, usd_24h_change, usd_24h_vol, usd_market_cap }`; ids the API does not know are absent.
 * Use `coingecko_token_id` to turn a symbol into an id.
 *
 * Paid: one MPP query per call.
 */
export async function coingecko_simple_price(
  args: { ids: string | string[]; vs_currency?: string },
  ctx: Ctx,
): Promise<{ prices: Record<string, CoinGeckoPrice> }> {
  const ids = Array.isArray(args.ids) ? args.ids.join(",") : args.ids;
  const body = await pay(ctx, `${COINGECKO_URL}/simple-price`, {
    body: {
      ids,
      vs_currencies: args.vs_currency ?? "usd",
      include_24hr_change: true,
      include_24hr_vol: true,
      include_market_cap: true,
    },
    label: "MPP CoinGecko simple-price",
  });
  return { prices: (body.data ?? body) as Record<string, CoinGeckoPrice> };
}

/**
 * CoinGecko coin search: id, name, symbol and market cap rank per match. Use it to find the id a
 * price or chart call needs.
 *
 * Paid: one MPP query per call.
 */
export async function coingecko_search(
  args: { query: string },
  ctx: Ctx,
): Promise<{ coins: CoinGeckoCoin[] }> {
  const body = await pay(ctx, `${COINGECKO_URL}/search`, {
    body: { query: args.query },
    label: "MPP CoinGecko search",
  });
  return { coins: (body.data?.coins ?? body.coins ?? []) as CoinGeckoCoin[] };
}

/**
 * Historical price, market cap and volume series for one CoinGecko id: arrays of
 * `[timestamp in milliseconds, value]`. `days` is the lookback window (1, 7, 30, 90, 365, ...)
 * or `max`, 30 by default; `vs_currency` defaults to `usd`.
 *
 * Paid: one MPP query per call.
 */
export async function coingecko_market_chart(
  args: { id: string; days?: number | string; vs_currency?: string },
  ctx: Ctx,
): Promise<{ chart: CoinGeckoMarketChart }> {
  const body = await pay(ctx, `${COINGECKO_URL}/market-chart`, {
    body: { id: args.id, vs_currency: args.vs_currency ?? "usd", days: String(args.days ?? 30) },
    label: "MPP CoinGecko market-chart",
  });
  return { chart: (body.data ?? body) as CoinGeckoMarketChart };
}

/**
 * OHLC candles for one CoinGecko id: `[timestamp in milliseconds, open, high, low, close]`, in
 * USD. `days` is the lookback window (1, 7, 14, 30, 90, 180, 365, ...) or `max`, 30 by default;
 * the candle size grows with the window.
 *
 * Paid: one MPP query per call.
 */
export async function coingecko_ohlc(
  args: { id: string; days?: number | string },
  ctx: Ctx,
): Promise<{ candles: number[][] }> {
  const body = await pay(ctx, `${COINGECKO_URL}/ohlc`, {
    body: { id: args.id, vs_currency: "usd", days: String(args.days ?? 30) },
    label: "MPP CoinGecko ohlc",
  });
  const candles = body.data ?? body;
  return { candles: Array.isArray(candles) ? (candles as number[][]) : [] };
}

/**
 * CoinGecko's trending coins right now: each entry's `item` has id, name, symbol, market cap rank
 * and nested `data` with price and 24h change.
 *
 * Paid: one MPP query per call.
 */
export async function coingecko_trending(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<{ coins: TrendingCoin[] }> {
  const body = await pay(ctx, `${COINGECKO_URL}/trending`, { body: {}, label: "MPP CoinGecko trending" });
  return { coins: (body.data?.coins ?? body.coins ?? []) as TrendingCoin[] };
}

/**
 * CoinGecko global market data: total market cap and volume per currency, and `btc`/`eth` share
 * of market cap in `market_cap_percentage`. Useful for dominance in one call.
 *
 * Paid: one MPP query per call.
 */
export async function coingecko_global(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<{ global: CoinGeckoGlobal }> {
  const body = await pay(ctx, `${COINGECKO_URL}/global`, { body: {}, label: "MPP CoinGecko global" });
  return { global: (body.data ?? body) as CoinGeckoGlobal };
}

/**
 * Web search (Parallel, fast mode): title, url, text snippet and publication date per result.
 * `num_results` caps the list at 5 by default.
 *
 * Paid: one MPP query per call.
 */
export async function search_web(
  args: { query: string; num_results?: number },
  ctx: Ctx,
): Promise<{ results: WebSearchResult[] }> {
  const body = await pay(ctx, PARALLEL_URL, {
    body: { query: args.query, mode: "fast" },
    label: "MPP Parallel search",
  });
  const results = (body.results ?? body.data ?? []) as WebSearchResult[];
  return { results: results.slice(0, args.num_results ?? 5) };
}

/**
 * DEX pools matching a token phrase through Codex: token name, symbol, address and network id,
 * with USD price, 24h volume, liquidity and market cap. Good for tokens CoinGecko does not list.
 * `limit` defaults to 3; compare `liquidity` to pick the canonical pool.
 *
 * Paid: one MPP query per call.
 */
export async function codex_filter_tokens(
  args: { phrase: string; limit?: number },
  ctx: Ctx,
): Promise<{ results: CodexTokenResult[] }> {
  const query = `
    query FilterTokens($phrase: String!, $limit: Int!) {
      filterTokens(phrase: $phrase, limit: $limit) {
        results { token { name symbol address networkId } priceUSD volume24 liquidity marketCap }
      }
    }`;
  const body = await pay(ctx, CODEX_URL, {
    body: { query, variables: { phrase: args.phrase, limit: args.limit ?? 3 } },
    label: "MPP Codex",
  });
  const results = body.data?.filterTokens?.results ?? [];
  return { results: results as CodexTokenResult[] };
}

/**
 * Token balances of a wallet from Allium: token name and symbol, amount and USD value per token.
 * `chain` is a chain name such as `ethereum`, `base` or `solana` (one chain per call, `ethereum`
 * by default); sum `value_usd` for the portfolio total.
 *
 * Paid: one MPP query per call.
 */
export async function allium_wallet_balances(
  args: { address: string; chain?: string },
  ctx: Ctx,
): Promise<{ balances: WalletBalance[] }> {
  const body = await pay(ctx, ALLIUM_URL, {
    body: { address: args.address, chains: [args.chain ?? "ethereum"] },
    label: "MPP Allium wallet balances",
  });
  const balances = Array.isArray(body) ? body : (body.data ?? []);
  return { balances: (Array.isArray(balances) ? balances : []) as WalletBalance[] };
}

/**
 * Submit a Dune SQL query and return its `execution_id` immediately; then poll
 * `dune_execution_results` with it. Only for explicit data requests: queries are billed from
 * $0.05 up, depending on engine size and runtime.
 *
 * Paid: one MPP query per call.
 */
export async function dune_execute_sql(
  args: { sql: string },
  ctx: Ctx,
): Promise<{ execution: DuneExecution }> {
  const body = await pay(ctx, `${DUNE_URL}/sql/execute`, {
    body: { sql: args.sql },
    label: "MPP Dune execute",
  });
  return { execution: body as DuneExecution };
}

/**
 * Poll a Dune execution started by `dune_execute_sql`. While `is_execution_finished` is false,
 * `state` is `QUERY_STATE_PENDING` or `QUERY_STATE_EXECUTING`: call again after a few seconds.
 * When finished, `result.rows` holds the rows and `result.metadata.column_names` the columns;
 * a failed query carries `error`.
 *
 * Paid: one MPP query per call.
 */
export async function dune_execution_results(
  args: { execution_id: string },
  ctx: Ctx,
): Promise<{ execution: DuneExecutionResult }> {
  const body = await pay(ctx, `${DUNE_URL}/execution/${encodeURIComponent(args.execution_id)}/results`, {
    method: "GET",
    label: "MPP Dune results",
  });
  return { execution: body as DuneExecutionResult };
}
