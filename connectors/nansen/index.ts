import type { Ctx } from "../centaur";

// Nansen's API (https://api.nansen.ai). Answers wrap the payload in `Response`; that envelope is
// unwrapped and the rows are passed through with Nansen's own field names.

const DEFAULT_BASE = "https://api.nansen.ai";

/** A Nansen label attached to an address. */
export interface Label {
  label?: string;
  category?: string;
  /** Entity fullname, when the address belongs to one. */
  fullname?: string;
  definition?: string;
  [field: string]: unknown;
}

/** One token balance of an address or entity. */
export interface Balance {
  token_symbol?: string;
  token_name?: string;
  token_address?: string;
  /** Token amount, not USD. */
  balance?: number;
  price_usd?: number;
  value_usd?: number;
  chain?: string;
  [field: string]: unknown;
}

/** One token in the Smart Money holdings table. */
export interface SmartMoneyHolding {
  token_symbol?: string;
  token_address?: string;
  chain?: string;
  /** Number of Smart Money addresses holding the token. */
  smart_money_count?: number;
  total_value_usd?: number;
  [field: string]: unknown;
}

/** One Smart Money net-flow row: what the tracked wallets bought and sold. */
export interface Netflow {
  token_symbol?: string;
  token_address?: string;
  chain?: string;
  /** USD bought minus sold; positive is net buying. */
  netflow_usd?: number;
  buyers?: number;
  sellers?: number;
  [field: string]: unknown;
}

/** One Smart Money DEX trade, within the last 24 hours. */
export interface DexTrade {
  trader_address?: string;
  /** `buy` or `sell`. */
  action?: string;
  token_symbol?: string;
  token_address?: string;
  value_usd?: number;
  chain?: string;
  [field: string]: unknown;
}

/** One token screener row. */
export interface ScreenedToken {
  token_symbol?: string;
  token_name?: string;
  token_address?: string;
  market_cap?: number;
  smart_money_holders?: number;
  [field: string]: unknown;
}

/** One holder of a token. */
export interface TokenHolder {
  address?: string;
  entity_name?: string;
  balance?: number;
  value_usd?: number;
  labels?: string[];
  [field: string]: unknown;
}

/** Inflows and outflows for one entity type. */
export interface TokenFlow {
  inflow_usd?: number;
  outflow_usd?: number;
  [field: string]: unknown;
}

/** Token flows keyed by entity type, such as funds or Smart Traders. */
export interface TokenFlows {
  [entity_type: string]: TokenFlow;
}

/** A related wallet of an address. */
export interface RelatedWallet {
  address?: string;
  relation_type?: string;
  entity_name?: string;
  [field: string]: unknown;
}

/** An entity matched by `search_entity`. */
export interface Entity {
  entity_name?: string;
  wallet_count?: number;
  [field: string]: unknown;
}

/** Nansen's page wrapper: `data` holds the rows, `pagination` the cursor for the next page. */
export interface Page<T> {
  data?: T[];
  pagination?: { page?: number; recordsPerPage?: number; [field: string]: unknown };
  [field: string]: unknown;
}

interface NansenError {
  error?: string | { message?: string; [field: string]: unknown };
  message?: string;
  [field: string]: unknown;
}

function base(ctx: Ctx): string {
  return ctx.env.NANSEN_API_URL || DEFAULT_BASE;
}

async function request(
  ctx: Ctx,
  endpoint: string,
  data: Record<string, unknown> | undefined,
  method: "POST" | "GET" = "POST",
): Promise<unknown> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.NANSEN_API_KEY) headers.apiKey = ctx.env.NANSEN_API_KEY;
  let url = base(ctx) + endpoint;
  let body: string | undefined;
  if (method === "POST") {
    headers["content-type"] = "application/json";
    body = JSON.stringify(data ?? {});
  } else {
    // GET endpoints take the same fields as query parameters.
    const pairs = Object.entries(data ?? {})
      .filter(([, value]) => value !== undefined && value !== null && value !== "")
      .map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`);
    if (pairs.length) url += `?${pairs.join("&")}`;
  }
  const res = await ctx.fetch(url, { method, headers, body });
  const text = await res.text();
  if (!res.ok) {
    let message = text.slice(0, 1000);
    try {
      const parsed = JSON.parse(text) as NansenError;
      const nested = typeof parsed.error === "object" ? parsed.error?.message : parsed.error;
      message = parsed.message ?? nested ?? message;
    } catch {
      // Not JSON: keep the raw text.
    }
    throw new Error(`Nansen API error (${res.status}): ${message}`);
  }
  const parsed = JSON.parse(text) as { Response?: unknown };
  // Unwrap `Response`, keeping its field name; endpoints without it return the body as sent.
  return parsed && typeof parsed === "object" && "Response" in parsed ? parsed.Response : parsed;
}

function page(page_number?: number, per_page?: number): Record<string, unknown> {
  return { page: page_number ?? 1, recordsPerPage: per_page ?? 100 };
}

/**
 * Human and Nansen labels for an address: label, category, entity fullname and definition.
 * `chain` is a Nansen chain name such as `ethereum`, `solana`, `base`; address is hexadecimal
 * (or base58 on Solana/Sui). Page with `page`/`per_page` (100 rows per page by default).
 */
export async function get_address_labels(
  args: { address: string; chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Label[] }> {
  const response = await request(ctx, "/api/beta/profiler/address/labels", {
    parameters: { chain: args.chain ?? "ethereum", address: args.address },
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Label[] };
}

/**
 * Current token balances of an address or of a named entity (`entity_name`, for example
 * `Vitalik Buterin`; give one of the two). Rows carry token symbol and address, token amount,
 * USD price and value, and chain. `hide_spam` (default true) drops spam tokens; page with
 * `page`/`per_page`.
 */
export async function get_address_balance(
  args: {
    address?: string;
    entity_name?: string;
    chain?: string;
    hide_spam?: boolean;
    page?: number;
    per_page?: number;
  },
  ctx: Ctx,
): Promise<{ response: Page<Balance> }> {
  if (!args.address && !args.entity_name) {
    throw new Error("Either address or entity_name must be provided");
  }
  const data: Record<string, unknown> = {
    chain: args.chain ?? "ethereum",
    hide_spam_token: args.hide_spam ?? true,
    pagination: page(args.page, args.per_page),
  };
  if (args.address) data.address = args.address;
  if (args.entity_name) data.entity_name = args.entity_name;
  const response = await request(ctx, "/api/v1/profiler/address/current-balance", data);
  return { response: response as Page<Balance> };
}

/**
 * Transactions of an address on one chain, as Nansen sends them (block timestamp, hashes,
 * counterparties, USD values). Page with `page`/`per_page`.
 */
export async function get_address_transactions(
  args: { address: string; chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<Record<string, unknown>> }> {
  const response = await request(ctx, "/api/v1/profiler/address/transactions", {
    address: args.address,
    chain: args.chain ?? "ethereum",
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Page<Record<string, unknown>> };
}

/**
 * Profit and loss, realized and unrealized, and trade performance for an address on one chain.
 * Returns Nansen's PnL summary fields as sent. Page with `page`/`per_page`.
 */
export async function get_address_pnl(
  args: { address: string; chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Record<string, unknown> }> {
  const response = await request(ctx, "/api/v1/profiler/address/pnl", {
    address: args.address,
    chain: args.chain ?? "ethereum",
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Record<string, unknown> };
}

/**
 * Wallets Nansen associates with the same owner or entity as `address`, with relation type and
 * entity name. Page with `page`/`per_page`.
 */
export async function get_address_related_wallets(
  args: { address: string; chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<RelatedWallet> }> {
  const response = await request(ctx, "/api/v1/profiler/address/related-wallets", {
    address: args.address,
    chain: args.chain ?? "ethereum",
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Page<RelatedWallet> };
}

/**
 * Tokens held by Smart Money wallets: token, chain, number of Smart Money holders
 * (`smart_money_count`) and total USD value. `chains` defaults to `["ethereum"]`; `labels`
 * filters the wallet segments (`Fund`, `Smart Trader`, ...). Page with `page`/`per_page`.
 */
export async function get_smart_money_holdings(
  args: { chains?: string[]; labels?: string[]; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<SmartMoneyHolding> }> {
  const data: Record<string, unknown> = {
    chains: args.chains ?? ["ethereum"],
    pagination: page(args.page, args.per_page),
  };
  if (args.labels) data.labels = args.labels;
  const response = await request(ctx, "/api/v1/smart-money/holdings", data);
  return { response: response as Page<SmartMoneyHolding> };
}

/**
 * What Smart Money bought and sold over the last 24 hours, per token: `netflow_usd` (positive is
 * net buying), buyer and seller counts. `chains` defaults to `["ethereum"]`; `labels` filters
 * wallet segments. Page with `page`/`per_page`.
 */
export async function get_smart_money_netflows(
  args: { chains?: string[]; labels?: string[]; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<Netflow> }> {
  const data: Record<string, unknown> = {
    chains: args.chains ?? ["ethereum"],
    pagination: page(args.page, args.per_page),
  };
  if (args.labels) data.labels = args.labels;
  const response = await request(ctx, "/api/v1/smart-money/netflows", data);
  return { response: response as Page<Netflow> };
}

/**
 * Individual Smart Money DEX trades from the last 24 hours: trader address, `action` (`buy` or
 * `sell`), token, USD value and chain. `chains` defaults to `["ethereum"]`; `labels` filters
 * wallet segments. Page with `page`/`per_page`.
 */
export async function get_smart_money_dex_trades(
  args: { chains?: string[]; labels?: string[]; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<DexTrade> }> {
  const data: Record<string, unknown> = {
    chains: args.chains ?? ["ethereum"],
    pagination: page(args.page, args.per_page),
  };
  if (args.labels) data.labels = args.labels;
  const response = await request(ctx, "/api/v1/smart-money/dex-trades", data);
  return { response: response as Page<DexTrade> };
}

/**
 * Token screener: tokens with Smart Money activity on one chain, with market capitalization,
 * `smart_money_holders` and further token metrics. Page with `page`/`per_page`.
 */
export async function get_token_screener(
  args: { chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<ScreenedToken> }> {
  const response = await request(ctx, "/api/v1/tgm/token-screener", {
    chain: args.chain ?? "ethereum",
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Page<ScreenedToken> };
}

/**
 * Top holders of a token by contract address: holder address, entity name when known, balance,
 * USD value and labels. Page with `page`/`per_page`.
 */
export async function get_token_holders(
  args: { token_address: string; chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<TokenHolder> }> {
  const response = await request(ctx, "/api/v1/tgm/holders", {
    token_address: args.token_address,
    chain: args.chain ?? "ethereum",
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Page<TokenHolder> };
}

/**
 * Token inflows and outflows over 24 hours, keyed by entity type (funds, Smart Traders, whales,
 * ...), each with `inflow_usd` and `outflow_usd`.
 */
export async function get_token_flows(
  args: { token_address: string; chain?: string },
  ctx: Ctx,
): Promise<{ response: TokenFlows }> {
  const response = await request(ctx, "/api/v1/tgm/flows", {
    token_address: args.token_address,
    chain: args.chain ?? "ethereum",
  });
  return { response: response as TokenFlows };
}

/**
 * Recent buyers and sellers of a token, as Nansen sends them (addresses, entity names, amounts
 * and USD values), unaggregated.
 */
export async function get_token_who_bought_sold(
  args: { token_address: string; chain?: string },
  ctx: Ctx,
): Promise<{ response: Record<string, unknown> }> {
  const response = await request(ctx, "/api/v1/tgm/who-bought-sold", {
    token_address: args.token_address,
    chain: args.chain ?? "ethereum",
  });
  return { response: response as Record<string, unknown> };
}

/**
 * DEX trades for one token by contract address: trader, action, size and price. Page with
 * `page`/`per_page`.
 */
export async function get_token_dex_trades(
  args: { token_address: string; chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<Record<string, unknown>> }> {
  const response = await request(ctx, "/api/v1/tgm/dex-trades", {
    token_address: args.token_address,
    chain: args.chain ?? "ethereum",
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Page<Record<string, unknown>> };
}

/**
 * PnL leaderboard for one token: the traders with the best realized and unrealized profit on it,
 * as Nansen sends them. Page with `page`/`per_page`.
 */
export async function get_token_pnl_leaderboard(
  args: { token_address: string; chain?: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<Record<string, unknown>> }> {
  const response = await request(ctx, "/api/v1/tgm/pnl-leaderboard", {
    token_address: args.token_address,
    chain: args.chain ?? "ethereum",
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Page<Record<string, unknown>> };
}

/**
 * Search Nansen entities (funds, whales, projects) by name, returning entity name and wallet
 * count; use the name with `get_address_balance` (`entity_name`). Page with `page`/`per_page`.
 */
export async function search_entity(
  args: { query: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ response: Page<Entity> }> {
  const response = await request(ctx, "/api/beta/profiler/entity/search", {
    query: args.query,
    pagination: page(args.page, args.per_page),
  });
  return { response: response as Page<Entity> };
}

/**
 * Escape hatch for any other Nansen endpoint: `endpoint` is the full path (for example
 * `/api/v1/smart-money/holdings`), `data` the JSON request body, or query parameters for `get`.
 * Returns the `Response` payload, or the whole body when the endpoint has none.
 */
export async function request_raw(
  args: { endpoint: string; data?: Record<string, unknown>; method?: "POST" | "GET" },
  ctx: Ctx,
): Promise<{ response: unknown }> {
  const endpoint = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  return { response: await request(ctx, endpoint, args.data, args.method ?? "POST") };
}
