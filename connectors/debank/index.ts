import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are DeBank's own objects, passed through. Field names are DeBank's
// (https://docs.cloud.debank.com/en/readme/api-pro-reference). USD values are in
// `usd_value`, `net_usd_value`, `asset_usd_value`, `debt_usd_value`; token amounts are decimal
// numbers already scaled by `decimals`, not raw integer units.

const DEFAULT_BASE = "https://pro-openapi.debank.com";

/** A chain DeBank supports. `id` (`eth`, `bsc`, `arb`, ...) is what the other functions take. */
export interface Chain {
  id: string;
  name: string;
  logo_url?: string;
  native_token_id?: string;
  is_support_pre_exec?: boolean;
  [field: string]: unknown;
}

/** One chain's share of a wallet's balance. */
export interface ChainBalance {
  id: string;
  name?: string;
  usd_value: number;
  [field: string]: unknown;
}

/** `/v1/user/total_balance`. */
export interface TotalBalance {
  total_usd_value: number;
  chain_list: ChainBalance[];
  [field: string]: unknown;
}

/** A token holding. `price` is USD per token; `amount * price` is its USD value. */
export interface Token {
  id: string;
  chain: string;
  name?: string;
  symbol?: string;
  optimized_symbol?: string;
  decimals?: number;
  price?: number;
  amount?: number;
  usd_value?: number;
  logo_url?: string;
  is_core?: boolean;
  is_verified?: boolean;
  is_wallet?: boolean;
  time_at?: number;
  [field: string]: unknown;
}

/** Token metadata from `/v1/token` and `/v1/token/list_by_ids`. */
export interface TokenInfo {
  id: string;
  chain: string;
  name?: string;
  symbol?: string;
  optimized_symbol?: string;
  decimals?: number;
  price?: number;
  logo_url?: string;
  is_core?: boolean;
  is_verified?: boolean;
  [field: string]: unknown;
}

/** An NFT held by a wallet. */
export interface Nft {
  id: string;
  chain?: string;
  name?: string;
  contract_id?: string;
  contract_name?: string;
  description?: string;
  logo_url?: string;
  thumbnail_url?: string;
  content_url?: string;
  detail_url?: string;
  usd_price?: number;
  collection_id?: string;
  is_core?: boolean;
  [field: string]: unknown;
}

/** One token leg of a DeFi position (supplied, borrowed or reward). */
export interface PositionToken {
  id: string;
  chain?: string;
  name?: string;
  symbol?: string;
  optimized_symbol?: string;
  decimals?: number;
  price?: number;
  amount?: number;
  [field: string]: unknown;
}

/** The token lists of one DeFi position; DeBank also sends `*_token_list` variants. */
export interface PositionDetail {
  supply_token_list?: PositionToken[];
  reward_token_list?: PositionToken[];
  borrow_token_list?: PositionToken[];
  [field: string]: unknown;
}

/** One DeFi position inside a protocol (a pool, a vault, a lending market, ...). */
export interface PortfolioItem {
  name?: string;
  detail?: PositionDetail;
  stats?: {
    asset_usd_value?: number;
    debt_usd_value?: number;
    net_usd_value?: number;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/**
 * A wallet's position in one protocol, in the "simple" shape: protocol ids, name, chain and USD
 * net/asset/debt values only.
 */
export interface SimpleProtocol {
  id: string;
  chain: string;
  name: string;
  logo_url?: string;
  site_url?: string;
  net_usd_value?: number;
  asset_usd_value?: number;
  debt_usd_value?: number;
  [field: string]: unknown;
}

/** A wallet's position in one protocol with every position spelled out. */
export interface ComplexProtocol extends SimpleProtocol {
  portfolio_item_list: PortfolioItem[];
}

/** Protocol metadata from `/v1/protocol` and `/v1/protocol/all_list`. */
export interface Protocol {
  id: string;
  chain: string;
  name: string;
  logo_url?: string;
  site_url?: string;
  tvl?: number;
  [field: string]: unknown;
}

/** One point of the net worth curve: `[unix_seconds, usd_value]`. */
export type NetCurvePoint = [number, number];

async function request(ctx: Ctx, path: string, params: Params = {}): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.DEBANK_API_KEY) headers.AccessKey = ctx.env.DEBANK_API_KEY;
  const base = (ctx.env.DEBANK_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
  const res = await ctx.fetch(`${base}${path}${query(params)}`, { headers });
  const label = `DeBank ${path}`;
  const text = await res.text();
  let body: any;
  try {
    body = text ? JSON.parse(text) : undefined;
  } catch {
    body = undefined;
  }
  // Errors are `{ message }` on HTTP errors and `{ error_code, error_msg }` on 200s.
  const message =
    body && typeof body === "object" ? (body.error_msg ?? body.message ?? body.error_code) : undefined;
  if (!res.ok) {
    throw new Error(`${label} failed (HTTP ${res.status}): ${message ?? text.slice(0, 1000)}`);
  }
  if (message) throw new Error(`${label} failed: ${message}`);
  return body;
}

/**
 * Chains DeBank supports, with their id (`eth`, `bsc`, `arb`, `base`, ...), display name and native
 * token id. Use the id as `chain_id`/`chain` in the other functions.
 */
export async function chains(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<{ chains: Chain[] }> {
  return { chains: await request(ctx, "/v1/chain/list") };
}

/**
 * A wallet's total USD balance across all chains plus the per-chain breakdown
 * (`chain_list[].usd_value`), highest first is not guaranteed — sort locally. `address` is an EVM
 * 0x address.
 */
export async function user_total_balance(
  args: { address: string },
  ctx: Ctx,
): Promise<{ balance: TotalBalance }> {
  return { balance: await request(ctx, "/v1/user/total_balance", { id: args.address }) };
}

/** A wallet's USD balance on one chain. */
export async function user_chain_balance(
  args: { address: string; chain_id: string },
  ctx: Ctx,
): Promise<{ balance: ChainBalance }> {
  return {
    balance: await request(ctx, "/v1/user/chain_balance", { id: args.address, chain_id: args.chain_id }),
  };
}

/**
 * A wallet's token holdings on one chain, with amount, USD price and core/verified flags.
 * `is_all` includes tokens DeBank does not count as core holdings (default true). Sort by
 * `amount * price` locally for the largest positions.
 */
export async function user_token_list(
  args: { address: string; chain_id: string; is_all?: boolean },
  ctx: Ctx,
): Promise<{ tokens: Token[] }> {
  const body = await request(ctx, "/v1/user/token_list", {
    id: args.address,
    chain_id: args.chain_id,
    is_all: String(args.is_all ?? true),
  });
  return { tokens: body };
}

/**
 * A wallet's token holdings across all chains (or only `chain_ids`, comma-separated), each row
 * carrying its `chain`. Large wallets return thousands of rows; filter and sort client-side.
 */
export async function user_all_token_list(
  args: { address: string; is_all?: boolean; chain_ids?: string },
  ctx: Ctx,
): Promise<{ tokens: Token[] }> {
  const body = await request(ctx, "/v1/user/all_token_list", {
    id: args.address,
    is_all: String(args.is_all ?? true),
    chain_ids: args.chain_ids,
  });
  return { tokens: body };
}

/**
 * A wallet's positions in one protocol (protocol id such as `uniswap3`, `aave3`, `curve`), with
 * every position's supplied/borrowed/reward token lists and USD stats.
 */
export async function user_protocol(
  args: { address: string; protocol_id: string },
  ctx: Ctx,
): Promise<{ protocol: ComplexProtocol }> {
  return {
    protocol: await request(ctx, "/v1/user/protocol", {
      id: args.address,
      protocol_id: args.protocol_id,
    }),
  };
}

/**
 * A wallet's DeFi protocol positions on one chain in the compact shape (id, name, chain, USD net /
 * asset / debt value per protocol). Use `user_complex_protocol_list` for the underlying positions.
 */
export async function user_simple_protocol_list(
  args: { address: string; chain_id: string },
  ctx: Ctx,
): Promise<{ protocols: SimpleProtocol[] }> {
  const body = await request(ctx, "/v1/user/simple_protocol_list", {
    id: args.address,
    chain_id: args.chain_id,
  });
  return { protocols: body };
}

/**
 * A wallet's DeFi protocol positions across all chains in the compact shape, each row carrying its
 * `chain`.
 */
export async function user_all_simple_protocol_list(
  args: { address: string },
  ctx: Ctx,
): Promise<{ protocols: SimpleProtocol[] }> {
  return { protocols: await request(ctx, "/v1/user/all_simple_protocol_list", { id: args.address }) };
}

/**
 * A wallet's DeFi protocol positions on one chain with every position: `portfolio_item_list[]`
 * with `name`, `detail` (supply/reward/borrow token lists) and `stats` USD values.
 */
export async function user_complex_protocol_list(
  args: { address: string; chain_id: string },
  ctx: Ctx,
): Promise<{ protocols: ComplexProtocol[] }> {
  const body = await request(ctx, "/v1/user/complex_protocol_list", {
    id: args.address,
    chain_id: args.chain_id,
  });
  return { protocols: body };
}

/**
 * A wallet's DeFi protocol positions across all chains with every position spelled out, each
 * protocol carrying its `chain`.
 */
export async function user_all_complex_protocol_list(
  args: { address: string },
  ctx: Ctx,
): Promise<{ protocols: ComplexProtocol[] }> {
  return { protocols: await request(ctx, "/v1/user/all_complex_protocol_list", { id: args.address }) };
}

/**
 * A wallet's NFTs on one chain, with collection (`contract_name`), token name, thumbnail and USD
 * price when DeBank has one. `is_all` includes collections DeBank does not treat as blue chip.
 */
export async function user_nft_list(
  args: { address: string; chain_id: string; is_all?: boolean },
  ctx: Ctx,
): Promise<{ nfts: Nft[] }> {
  const body = await request(ctx, "/v1/user/nft_list", {
    id: args.address,
    chain_id: args.chain_id,
    is_all: String(args.is_all ?? false),
  });
  return { nfts: body };
}

/**
 * A wallet's net worth history as `[unix_seconds, usd_value]` points, oldest first, across all
 * chains or only `chain_ids` (comma-separated).
 */
export async function user_total_net_curve(
  args: { address: string; chain_ids?: string },
  ctx: Ctx,
): Promise<{ total_net_curve: NetCurvePoint[] }> {
  const body = await request(ctx, "/v1/user/total_net_curve", {
    id: args.address,
    chain_ids: args.chain_ids,
  });
  // The Pro API returns `{ total_net_curve }`; older deployments returned the array itself.
  return { total_net_curve: Array.isArray(body) ? body : (body.total_net_curve ?? []) };
}

/**
 * One token's metadata and USD price. `token_id` is the token address, or the native token id
 * (`eth`, `bsc`) for a chain's gas token.
 */
export async function token(
  args: { chain_id: string; token_id: string },
  ctx: Ctx,
): Promise<{ token: TokenInfo }> {
  return {
    token: await request(ctx, "/v1/token", { chain_id: args.chain_id, id: args.token_id }),
  };
}

/**
 * Metadata and USD prices for several tokens on one chain. `token_ids` is comma-separated token
 * addresses (native token ids also work).
 */
export async function token_list_by_ids(
  args: { chain_id: string; token_ids: string },
  ctx: Ctx,
): Promise<{ tokens: TokenInfo[] }> {
  const body = await request(ctx, "/v1/token/list_by_ids", {
    chain_id: args.chain_id,
    ids: args.token_ids,
  });
  return { tokens: body };
}

/** One DeFi protocol's metadata: name, chain, site, TVL. `protocol_id` is a DeBank protocol id. */
export async function protocol(
  args: { protocol_id: string },
  ctx: Ctx,
): Promise<{ protocol: Protocol }> {
  return { protocol: await request(ctx, "/v1/protocol", { id: args.protocol_id }) };
}

/**
 * Protocols on one chain with id, name and TVL. Not sorted; sort by `tvl` locally and keep the top
 * rows.
 */
export async function protocol_list(
  args: { chain_id: string },
  ctx: Ctx,
): Promise<{ protocols: Protocol[] }> {
  return { protocols: await request(ctx, "/v1/protocol/list", { chain_id: args.chain_id }) };
}

/** Every protocol DeBank knows, across chains, with id, name, chain and TVL. Large; filter locally. */
export async function protocol_all_list(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<{ protocols: Protocol[] }> {
  return { protocols: await request(ctx, "/v1/protocol/all_list") };
}

/**
 * GET any v1 path, for endpoints without a typed function (for example `/v1/user/used_chains`,
 * `/v1/user/history_list`, `/v1/chain/gas_price`). `endpoint` starts with `/`; `params` become
 * query parameters. The response body is returned as `data`.
 */
export async function raw(
  args: { endpoint: string; params?: Record<string, string | number | boolean> },
  ctx: Ctx,
): Promise<{ data: unknown }> {
  const endpoint = args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`;
  return { data: await request(ctx, endpoint, args.params ?? {}) };
}
