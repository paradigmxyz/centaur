import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Arkham's REST API (https://api.arkm.com). Responses are Arkham's own
// objects, passed through; the interfaces list the fields worth knowing and
// Arkham sends more. Single-object endpoints return that object, list
// endpoints wrap their array. Amounts are USD unless a field says otherwise.

const DEFAULT_BASE = "https://api.arkm.com";

/** An Arkham entity: an exchange, fund, person, protocol and so on. */
export interface ArkhamEntity {
  id?: string;
  name?: string;
  type?: string;
  website?: string;
  twitter?: string;
  description?: string;
  [field: string]: unknown;
}

/** A human-readable label Arkham put on an address. */
export interface ArkhamLabel {
  name?: string;
  [field: string]: unknown;
}

/** A tag id plus its display label. */
export interface PopulatedTag {
  id?: string;
  label?: string;
  [field: string]: unknown;
}

/** An address as Arkham embeds it in transfers, portfolios and counterparties. */
export interface AddressInfo {
  address?: string;
  chain?: string;
  arkhamEntity?: ArkhamEntity | null;
  arkhamLabel?: ArkhamLabel | null;
  populatedTags?: PopulatedTag[];
  contract?: boolean;
  [field: string]: unknown;
}

/** Entity intelligence; `addresses` holds the entity's known addresses when present. */
export interface EntityInfo extends ArkhamEntity {
  populatedTags?: PopulatedTag[];
  addresses?: AddressInfo[];
  [field: string]: unknown;
}

export interface EntitySummary {
  numAddresses?: number;
  volumeUsd?: number;
  balanceUsd?: number;
  /** ISO timestamp of the earliest transfer. */
  firstTx?: string;
  /** ISO timestamp of the latest transfer. */
  lastTx?: string;
  [field: string]: unknown;
}

export interface Token {
  id?: string;
  name?: string;
  symbol?: string;
  price?: number;
  [field: string]: unknown;
}

export interface Transfer {
  /** ISO timestamp. */
  blockTimestamp?: string;
  fromAddress?: AddressInfo;
  toAddress?: AddressInfo;
  /** USD value of the transfer. */
  unitValue?: number;
  tokenSymbol?: string;
  chain?: string;
  transactionHash?: string;
  [field: string]: unknown;
}

export interface TransfersPage {
  transfers: Transfer[];
  [field: string]: unknown;
}

export interface Swap {
  chain?: string;
  usdValue?: number;
  [field: string]: unknown;
}

export interface SwapsPage {
  swaps: Swap[];
  [field: string]: unknown;
}

export interface TransactionInfo {
  hash?: string;
  blockNumber?: number;
  /** ISO timestamp. */
  blockTimestamp?: string;
  fromAddress?: string;
  toAddress?: string;
  usdValue?: number;
  fee?: string;
  [field: string]: unknown;
}

export interface Holding {
  token?: Token;
  balance?: number;
  /** USD value of the holding. */
  usd?: number;
  chain?: string;
  [field: string]: unknown;
}

export interface Portfolio {
  holdings: Holding[];
  totalUsd?: number;
  [field: string]: unknown;
}

export interface Balances {
  balances?: Holding[];
  totalUsd?: number;
  [field: string]: unknown;
}

export interface Counterparty {
  address?: AddressInfo;
  /** USD volume with the queried address or entity. */
  usd?: number;
  transactionCount?: number;
  flow?: string;
  [field: string]: unknown;
}

export interface CounterpartiesPage {
  counterparties: Counterparty[];
  [field: string]: unknown;
}

export interface TokenHolder {
  address?: AddressInfo;
  balance?: number;
  usd?: number;
  [field: string]: unknown;
}

export interface TokenHoldersPage {
  holders: TokenHolder[];
  [field: string]: unknown;
}

export interface TrendingToken {
  id?: string;
  name?: string;
  symbol?: string;
  price?: number;
  /** Price 24 hours ago, for computing the change. */
  price24hAgo?: number;
  volume24h?: number;
  [field: string]: unknown;
}

export interface NetworkStatus {
  chain?: string;
  active?: boolean;
  price?: number;
  priceChange24hPercent?: number;
  marketCap?: number;
  [field: string]: unknown;
}

const seg = (value: string): string => encodeURIComponent(value.trim());

function base(ctx: Ctx): string {
  return (ctx.env.ARKHAM_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function headers(ctx: Ctx): Record<string, string> {
  const out: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.ARKHAM_API_KEY) out["API-Key"] = ctx.env.ARKHAM_API_KEY;
  return out;
}

async function request(ctx: Ctx, endpoint: string, params: Params = {}): Promise<any> {
  const res = await ctx.fetch(`${base(ctx)}${endpoint}${query(params)}`, { headers: headers(ctx) });
  return readJson(res, "Arkham");
}

/**
 * Unauthenticated connectivity check: Arkham's `/health` answer as text (`ok` when the body is
 * empty), with the HTTP status. Needs no API key.
 */
export async function health(args: {}, ctx: Ctx): Promise<{ code: number; status: string }> {
  const res = await ctx.fetch(`${base(ctx)}/health`, { headers: { accept: "application/json, text/plain" } });
  const text = (await res.text()).trim();
  if (!res.ok) throw new Error(`Arkham health returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  const contentType = res.headers["content-type"] ?? "";
  if (!text || contentType.startsWith("text/")) return { code: res.status, status: text || "ok" };
  try {
    const body = JSON.parse(text);
    return { code: res.status, status: typeof body.status === "string" ? body.status : text };
  } catch {
    return { code: res.status, status: text };
  }
}

/** Blockchains Arkham covers, by the chain names its other endpoints accept (`ethereum`, `bitcoin`, ...). */
export async function chains(args: {}, ctx: Ctx): Promise<{ chains: string[] }> {
  return { chains: await request(ctx, "/chains") };
}

/**
 * Everything Arkham knows about one address on its chain: `arkhamEntity`, `arkhamLabel`,
 * `populatedTags`, `chain`, and `contract` set when it is a contract.
 */
export async function address_intelligence(
  args: { address: string },
  ctx: Ctx,
): Promise<AddressInfo> {
  return await request(ctx, `/intelligence/address/${seg(args.address)}`);
}

/** The same address intelligence aggregated across every chain the address is used on. */
export async function address_intelligence_all_chains(
  args: { address: string },
  ctx: Ctx,
): Promise<AddressInfo> {
  return await request(ctx, `/intelligence/address/${seg(args.address)}/all`);
}

/**
 * Enriched address intelligence: the same fields as `address_intelligence` plus Arkham's tags,
 * clusters and predicted entity. `include_tags` defaults to true, the other two to false.
 */
export async function address_enriched(
  args: { address: string; include_tags?: boolean; include_clusters?: boolean; include_entity_predictions?: boolean },
  ctx: Ctx,
): Promise<AddressInfo> {
  return await request(ctx, `/intelligence/address_enriched/${seg(args.address)}`, {
    includeTags: String(args.include_tags ?? true),
    includeClusters: String(args.include_clusters ?? false),
    includeEntityPredictions: String(args.include_entity_predictions ?? false),
  });
}

/** One entity by its Arkham id slug (`binance`, `vitalik-buterin`): name, type, links, tags, addresses. */
export async function entity(args: { entity: string }, ctx: Ctx): Promise<EntityInfo> {
  return await request(ctx, `/intelligence/entity/${seg(args.entity)}`);
}

/** Aggregate statistics for an entity: number of addresses, USD volume, USD balance, first and last transfer. */
export async function entity_summary(args: { entity: string }, ctx: Ctx): Promise<EntitySummary> {
  return await request(ctx, `/intelligence/entity/${seg(args.entity)}/summary`);
}

/** Addresses Arkham predicts belong to an entity but has not confirmed. */
export async function entity_predictions(
  args: { entity: string },
  ctx: Ctx,
): Promise<Record<string, unknown>> {
  return await request(ctx, `/intelligence/entity_predictions/${seg(args.entity)}`);
}

/** Contract intelligence for a contract address on a chain: verified name, token, deployer details. */
export async function contract(
  args: { chain: string; address: string },
  ctx: Ctx,
): Promise<Record<string, unknown>> {
  return await request(ctx, `/intelligence/contract/${seg(args.chain)}/${seg(args.address)}`);
}

/** Token intelligence by Arkham pricing id (`ethereum`, `bitcoin`, `usd-coin`): metadata and price. */
export async function token(args: { pricing_id: string }, ctx: Ctx): Promise<Token> {
  return await request(ctx, `/intelligence/token/${seg(args.pricing_id)}`);
}

/** Token intelligence by chain and contract address, for tokens without a pricing id. */
export async function token_by_address(
  args: { chain: string; address: string },
  ctx: Ctx,
): Promise<Token> {
  return await request(ctx, `/intelligence/token/${seg(args.chain)}/${seg(args.address)}`);
}

/**
 * Token transfers, newest first, filtered by the address or entity involved (`base`), `flow`
 * (in, out or all), `chain`, `token_id` (pricing id), a USD value range and an ISO time range.
 * Page with `limit` and `offset`; each transfer carries labelled `fromAddress`/`toAddress`,
 * `unitValue` in USD, `tokenSymbol`, `chain` and `blockTimestamp`.
 */
export async function transfers(
  args: {
    base?: string;
    flow?: "in" | "out" | "all";
    chain?: string;
    token_id?: string;
    usd_gte?: number;
    usd_lte?: number;
    time_gte?: string;
    time_lte?: string;
    limit?: number;
    offset?: number;
  },
  ctx: Ctx,
): Promise<TransfersPage> {
  return await request(ctx, "/transfers", {
    base: args.base,
    flow: args.flow,
    chain: args.chain,
    tokenId: args.token_id,
    usdGte: args.usd_gte,
    usdLte: args.usd_lte,
    timeGte: args.time_gte,
    timeLte: args.time_lte,
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}

/** DEX swaps, newest first, filtered by the address or entity involved (`base`) and `chain`; page with limit/offset. */
export async function swaps(
  args: { base?: string; chain?: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<SwapsPage> {
  return await request(ctx, "/swaps", {
    base: args.base,
    chain: args.chain,
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}

/** A transaction by hash, keyed by chain name, with hash, block, time, from/to, USD value and fee. */
export async function transaction(
  args: { tx_hash: string },
  ctx: Ctx,
): Promise<{ transactions: Record<string, TransactionInfo> }> {
  return { transactions: await request(ctx, `/tx/${seg(args.tx_hash)}`) };
}

/** Current token holdings of an address, with USD value per holding (`usd`) and a portfolio total; optionally one chain only. */
export async function portfolio_address(
  args: { address: string; chain?: string },
  ctx: Ctx,
): Promise<Portfolio> {
  return await request(ctx, `/portfolio/address/${seg(args.address)}`, { chain: args.chain });
}

/** Current token holdings of an entity, with USD value per holding; optionally one chain only. */
export async function portfolio_entity(
  args: { entity: string; chain?: string },
  ctx: Ctx,
): Promise<Portfolio> {
  return await request(ctx, `/portfolio/entity/${seg(args.entity)}`, { chain: args.chain });
}

/** Portfolio USD value over time for an address, as a list of points in Arkham's own shape. */
export async function portfolio_timeseries_address(
  args: { address: string; chain?: string },
  ctx: Ctx,
): Promise<{ points: unknown[] }> {
  return { points: await request(ctx, `/portfolio/timeSeries/address/${seg(args.address)}`, { chain: args.chain }) };
}

/** Portfolio USD value over time for an entity, as a list of points in Arkham's own shape. */
export async function portfolio_timeseries_entity(
  args: { entity: string; chain?: string },
  ctx: Ctx,
): Promise<{ points: unknown[] }> {
  return { points: await request(ctx, `/portfolio/timeSeries/entity/${seg(args.entity)}`, { chain: args.chain }) };
}

/** Historical USD value of an address over time, as a list of points in Arkham's own shape. */
export async function history_address(args: { address: string }, ctx: Ctx): Promise<{ points: unknown[] }> {
  return { points: await request(ctx, `/history/address/${seg(args.address)}`) };
}

/** Historical USD value of an entity over time, as a list of points in Arkham's own shape. */
export async function history_entity(args: { entity: string }, ctx: Ctx): Promise<{ points: unknown[] }> {
  return { points: await request(ctx, `/history/entity/${seg(args.entity)}`) };
}

/** Token balances of an address per chain, with `usd` per balance and a total; optionally one chain only. */
export async function balances_address(
  args: { address: string; chain?: string },
  ctx: Ctx,
): Promise<Balances> {
  return await request(ctx, `/balances/address/${seg(args.address)}`, { chain: args.chain });
}

/** Token balances of an entity per chain, with `usd` per balance and a total; optionally one chain only. */
export async function balances_entity(
  args: { entity: string; chain?: string },
  ctx: Ctx,
): Promise<Balances> {
  return await request(ctx, `/balances/entity/${seg(args.entity)}`, { chain: args.chain });
}

/** USD inflows and outflows of an address, broken down by chain and period in Arkham's own shape. */
export async function flow_address(args: { address: string; chain?: string }, ctx: Ctx): Promise<Record<string, unknown>> {
  return await request(ctx, `/flow/address/${seg(args.address)}`, { chain: args.chain });
}

/** USD inflows and outflows of an entity, broken down by chain and period in Arkham's own shape. */
export async function flow_entity(args: { entity: string; chain?: string }, ctx: Ctx): Promise<Record<string, unknown>> {
  return await request(ctx, `/flow/entity/${seg(args.entity)}`, { chain: args.chain });
}

/**
 * An address's top counterparties by USD volume: labelled `address`, `usd`, `transactionCount` and
 * `flow` (in or out). `flow` filters to one direction; `limit` defaults to 20.
 */
export async function counterparties_address(
  args: { address: string; flow?: "in" | "out" | "all"; chain?: string; limit?: number },
  ctx: Ctx,
): Promise<CounterpartiesPage> {
  return await request(ctx, `/counterparties/address/${seg(args.address)}`, {
    flow: args.flow,
    chain: args.chain,
    limit: args.limit ?? 20,
  });
}

/** An entity's top counterparties by USD volume, with the same fields as `counterparties_address`. */
export async function counterparties_entity(
  args: { entity: string; flow?: "in" | "out" | "all"; chain?: string; limit?: number },
  ctx: Ctx,
): Promise<CounterpartiesPage> {
  return await request(ctx, `/counterparties/entity/${seg(args.entity)}`, {
    flow: args.flow,
    chain: args.chain,
    limit: args.limit ?? 20,
  });
}

/** Top holders of a token by pricing id: labelled `address`, `balance` and USD value; page with limit/offset. */
export async function token_holders(
  args: { pricing_id: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<TokenHoldersPage> {
  return await request(ctx, `/token/holders/${seg(args.pricing_id)}`, {
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}

/** Top holders of a token by chain and contract address; page with limit/offset. */
export async function token_holders_by_address(
  args: { chain: string; address: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<TokenHoldersPage> {
  return await request(ctx, `/token/holders/${seg(args.chain)}/${seg(args.address)}`, {
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}

/** Tokens Arkham currently ranks as trending: name, symbol, `price`, `price24hAgo` and `volume24h`. */
export async function token_trending(args: { chain?: string }, ctx: Ctx): Promise<{ tokens: TrendingToken[] }> {
  return { tokens: await request(ctx, "/token/trending", { chain: args.chain }) };
}

/** Top addresses by flow of one token (`flow` in or out), newest period first; `limit` defaults to 20. */
export async function token_top_flow(
  args: { pricing_id: string; flow?: "in" | "out" | "all"; limit?: number },
  ctx: Ctx,
): Promise<Record<string, unknown>> {
  return await request(ctx, `/token/top_flow/${seg(args.pricing_id)}`, {
    flow: args.flow,
    limit: args.limit ?? 20,
  });
}

/** One token's trading volume over time; `interval` is a time bucket such as `1h` or `1d` (default `1h`). */
export async function token_volume(
  args: { pricing_id: string; interval?: string },
  ctx: Ctx,
): Promise<{ points: unknown[] }> {
  return { points: await request(ctx, `/token/volume/${seg(args.pricing_id)}`, { interval: args.interval ?? "1h" }) };
}

/** Tokens ranked by exchange movements: `sort_by` (default `volume`), `interval` (`24h` by default), `limit` (20). */
export async function token_top(
  args: { sort_by?: string; interval?: string; limit?: number },
  ctx: Ctx,
): Promise<Record<string, unknown>> {
  return await request(ctx, "/token/top", {
    sortBy: args.sort_by ?? "volume",
    interval: args.interval ?? "24h",
    limit: args.limit ?? 20,
  });
}

/** Every supported network with its native asset price, 24h change, market cap and `active` flag. */
export async function networks_status(args: {}, ctx: Ctx): Promise<{ networks: NetworkStatus[] }> {
  return { networks: await request(ctx, "/networks/status") };
}

/** Historical price quotes for one chain's native asset, as a list of points in Arkham's own shape. */
export async function network_history(args: { chain: string }, ctx: Ctx): Promise<{ points: unknown[] }> {
  return { points: await request(ctx, `/networks/history/${seg(args.chain)}`) };
}

/** Market data for Arkham's altcoin index, in Arkham's own shape. */
export async function altcoin_index(args: {}, ctx: Ctx): Promise<Record<string, unknown>> {
  return await request(ctx, "/marketdata/altcoin_index");
}

/** Loan and borrow positions of an address, optionally on one chain only, in Arkham's own shape. */
export async function loans_address(args: { address: string; chain?: string }, ctx: Ctx): Promise<Record<string, unknown>> {
  return await request(ctx, `/loans/address/${seg(args.address)}`, { chain: args.chain });
}

/** Loan and borrow positions of an entity, optionally on one chain only, in Arkham's own shape. */
export async function loans_entity(args: { entity: string; chain?: string }, ctx: Ctx): Promise<Record<string, unknown>> {
  return await request(ctx, `/loans/entity/${seg(args.entity)}`, { chain: args.chain });
}

/**
 * Escape hatch for endpoints without a wrapper: `endpoint` starts with `/` (`/token/trending`),
 * `params` are query parameters as strings. Returns the HTTP status and the JSON body untouched;
 * a non-2xx answer throws with Arkham's message.
 */
export async function raw(
  args: { endpoint: string; params?: Record<string, string> },
  ctx: Ctx,
): Promise<{ status: number; body: unknown }> {
  const endpoint = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  const res = await ctx.fetch(`${base(ctx)}${endpoint}${query(args.params ?? {})}`, { headers: headers(ctx) });
  return { status: res.status, body: await readJson(res, `Arkham ${endpoint}`) };
}
