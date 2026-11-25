import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Alchemy answers JSON-RPC on `https://<chain>.g.alchemy.com/v2/<key>` and the
// Prices API on `https://api.g.alchemy.com/prices/v1`. Results are Alchemy's
// own objects, passed through: hex quantities stay hex, field names are
// Alchemy's, and absent fields are absent.

const RPC_HOST = "g.alchemy.com";
const PRICES_BASE = "https://api.g.alchemy.com/prices/v1";

// Chain alias -> Alchemy network prefix. A name that is not an alias is used
// as the prefix verbatim (`eth-mainnet`, `arb-sepolia`, ...).
const SUPPORTED_CHAINS: Record<string, string> = {
  ethereum: "eth-mainnet",
  eth: "eth-mainnet",
  mainnet: "eth-mainnet",
  sepolia: "eth-sepolia",
  polygon: "polygon-mainnet",
  arbitrum: "arb-mainnet",
  arb: "arb-mainnet",
  optimism: "opt-mainnet",
  opt: "opt-mainnet",
  base: "base-mainnet",
  solana: "solana-mainnet",
  sol: "solana-mainnet",
  zksync: "zksync-mainnet",
  avalanche: "avax-mainnet",
  avax: "avax-mainnet",
  bnb: "bnb-mainnet",
  fantom: "fantom-mainnet",
  scroll: "scroll-mainnet",
  linea: "linea-mainnet",
  zora: "zora-mainnet",
  blast: "blast-mainnet",
  monad: "monad-mainnet",
};

export interface TokenBalance {
  /** Token contract address. */
  contractAddress: string;
  /** Raw balance as a hex quantity; `null` when Alchemy could not read it. */
  tokenBalance: string | null;
  error?: string | null;
  [field: string]: unknown;
}

export interface TokenBalances {
  address: string;
  tokenBalances: TokenBalance[];
  [field: string]: unknown;
}

export interface TokenMetadata {
  name: string | null;
  symbol: string | null;
  decimals: number | null;
  logo?: string | null;
  [field: string]: unknown;
}

export interface AssetTransfer {
  /** Block number as a hex quantity. */
  blockNum: string;
  uniqueId?: string;
  hash: string;
  from: string;
  to: string | null;
  /** Human-readable amount; `null` for unreadable or zero-decimal cases. */
  value: number | null;
  erc721TokenId?: string | null;
  erc1155Metadata?: { tokenId: string; value: string }[] | null;
  asset?: string | null;
  category: string;
  rawContract?: { value: string | null; address: string | null; decimal: string | null };
  metadata?: { blockTimestamp?: string; [field: string]: unknown } | null;
  [field: string]: unknown;
}

export interface AssetTransfers {
  transfers: AssetTransfer[];
  /** Continue a scan with `page_key` set to this. */
  pageKey?: string;
  [field: string]: unknown;
}

export interface Transaction {
  hash: string;
  from: string;
  to: string | null;
  value?: string;
  gas?: string;
  gasPrice?: string;
  maxFeePerGas?: string;
  maxPriorityFeePerGas?: string;
  nonce?: string;
  input?: string;
  blockNumber?: string | null;
  blockHash?: string | null;
  transactionIndex?: string | null;
  type?: string;
  chainId?: string;
  accessList?: unknown[];
  [field: string]: unknown;
}

export interface TransactionReceipt {
  transactionHash: string;
  transactionIndex: string;
  blockNumber: string;
  blockHash: string;
  from: string;
  to: string | null;
  cumulativeGasUsed: string;
  gasUsed: string;
  effectiveGasPrice?: string;
  contractAddress: string | null;
  /** `0x1` succeeded, `0x0` reverted. */
  status: string;
  logs: Log[];
  [field: string]: unknown;
}

export interface Log {
  address: string;
  topics: string[];
  data: string;
  blockNumber: string;
  blockHash: string;
  transactionHash: string;
  transactionIndex: string;
  logIndex: string;
  removed: boolean;
  [field: string]: unknown;
}

export interface PricePoint {
  currency: string;
  /** Decimal price as a string, as Alchemy sends it. */
  value: string;
  lastUpdatedAt: string;
  [field: string]: unknown;
}

export interface TokenPrice {
  symbol?: string;
  network?: string;
  address?: string;
  prices: PricePoint[];
  error: string | null;
  [field: string]: unknown;
}

export interface HistoricalPricePoint {
  /** Decimal price as a string. */
  value: string;
  timestamp: string;
  [field: string]: unknown;
}

export interface HistoricalPrices {
  symbol?: string;
  network?: string;
  address?: string;
  prices: HistoricalPricePoint[];
  [field: string]: unknown;
}

/** A JSON-RPC quantity ("0x1bc16d674ec80000") as an exact decimal string. */
function hexToDecimal(value: string | null | undefined): string {
  if (!value || value === "0x") return "0";
  return BigInt(value).toString();
}

/** An Alchemy network prefix for a chain alias or a full prefix. */
function networkPrefix(chain: string): string {
  const normalized = chain.trim().toLowerCase().replace(/ /g, "-");
  return SUPPORTED_CHAINS[normalized] ?? normalized;
}

function apiKey(ctx: Ctx): string {
  const key = ctx.env.ALCHEMY_API_KEY;
  if (!key) throw new Error("ALCHEMY_API_KEY is not set; Alchemy needs it in the request path (RPC) or as a Bearer token (Prices)");
  return key;
}

/** POST one JSON-RPC call and return its `result`, throwing on an RPC error body. */
async function rpc(ctx: Ctx, method: string, params: unknown[], chain: string): Promise<any> {
  const url = `https://${networkPrefix(chain)}.${RPC_HOST}/v2/${apiKey(ctx)}`;
  const res = await ctx.fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", method, params, id: 1 }),
  });
  const body = await readJson(res, `Alchemy ${method}`);
  // Alchemy reports most failures as HTTP 200 with a JSON-RPC error object.
  if (body.error) throw new Error(`Alchemy ${method} failed: ${body.error.message ?? JSON.stringify(body.error)}`);
  return body.result;
}

/** GET the Prices API and return the response body. */
async function prices(ctx: Ctx, path: string): Promise<any> {
  const res = await ctx.fetch(PRICES_BASE + path, {
    headers: { authorization: `Bearer ${apiKey(ctx)}`, accept: "application/json" },
  });
  return readJson(res, "Alchemy Prices");
}

/**
 * Chain aliases Alchemy accepts here, mapped to their network prefix (`ethereum` and `eth` are
 * `eth-mainnet`, `arb` is `arb-mainnet`, ...). Every function that takes `chain` accepts an alias
 * from this list or a full prefix such as `arb-sepolia`; the default is `ethereum`.
 */
export async function chains(args: {}, ctx: Ctx): Promise<{ chains: { alias: string; network: string }[] }> {
  return {
    chains: Object.keys(SUPPORTED_CHAINS)
      .sort()
      .map((alias) => ({ alias, network: SUPPORTED_CHAINS[alias] })),
  };
}

/**
 * Native-token balance (ETH, MATIC, ...) at the latest block, in wei as an exact decimal string.
 * `address` is a 0x-prefixed account address.
 */
export async function balance(args: { address: string; chain?: string }, ctx: Ctx): Promise<{ balance_wei: string }> {
  return { balance_wei: hexToDecimal(await rpc(ctx, "eth_getBalance", [args.address, "latest"], args.chain ?? "ethereum")) };
}

/**
 * ERC-20 balances for an address. Without `token_addresses` Alchemy scans the wallet's tokens;
 * with them it reads exactly those contracts. Balances are raw hex quantities (apply the token's
 * `decimals` from `token_metadata`). Alchemy accepts a limited number of contract addresses per
 * call, so pass a batch rather than a whole wallet's token list.
 */
export async function token_balances(
  args: { address: string; token_addresses?: string[]; chain?: string },
  ctx: Ctx,
): Promise<TokenBalances> {
  const params = [args.address, args.token_addresses?.length ? args.token_addresses : "erc20"];
  return await rpc(ctx, "alchemy_getTokenBalances", params, args.chain ?? "ethereum");
}

/** Name, symbol, decimals and logo for one ERC-20 contract; `null` fields mean the token does not implement them. */
export async function token_metadata(
  args: { token_address: string; chain?: string },
  ctx: Ctx,
): Promise<{ metadata: TokenMetadata }> {
  return { metadata: await rpc(ctx, "alchemy_getTokenMetadata", [args.token_address], args.chain ?? "ethereum") };
}

/**
 * Native, internal and token transfers in or out of an address, newest block first: `from`/`to`
 * as Alchemy labels them, human-readable `value`, `asset` symbol, `category`
 * (external, internal, erc20, erc721, erc1155) and `metadata.blockTimestamp`. Blocks are hex
 * quantities (`0x0`..`latest`). `max_count` is Alchemy's page size, at most 1000. Continue with
 * `page_key` from `pageKey` until it is absent.
 */
export async function asset_transfers(
  args: {
    from_address?: string;
    to_address?: string;
    from_block?: string;
    to_block?: string;
    categories?: string[];
    max_count?: number;
    exclude_zero_value?: boolean;
    page_key?: string;
    chain?: string;
  },
  ctx: Ctx,
): Promise<AssetTransfers> {
  if (!args.from_address && !args.to_address) throw new Error("asset_transfers needs from_address and/or to_address");
  const params = {
    fromBlock: args.from_block ?? "0x0",
    toBlock: args.to_block ?? "latest",
    fromAddress: args.from_address,
    toAddress: args.to_address,
    category: args.categories?.length ? args.categories : ["external", "internal", "erc20"],
    maxCount: `0x${Math.min(Math.max(args.max_count ?? 100, 1), 1000).toString(16)}`,
    excludeZeroValue: args.exclude_zero_value ?? true,
    withMetadata: true,
    pageKey: args.page_key,
  };
  return await rpc(ctx, "alchemy_getAssetTransfers", [params], args.chain ?? "ethereum");
}

/** Current block height on the chain. */
export async function block_number(args: { chain?: string }, ctx: Ctx): Promise<{ block_number: number }> {
  return { block_number: Number(hexToDecimal(await rpc(ctx, "eth_blockNumber", [], args.chain ?? "ethereum"))) };
}

/** Current gas price in wei as an exact decimal string (divide by 1e9 for gwei). */
export async function gas_price(args: { chain?: string }, ctx: Ctx): Promise<{ gas_price_wei: string }> {
  return { gas_price_wei: hexToDecimal(await rpc(ctx, "eth_gasPrice", [], args.chain ?? "ethereum")) };
}

/** A transaction by 0x hash, with hex quantities and unknown fields as Alchemy sends them; `transaction` is null when no node has seen it. */
export async function transaction(
  args: { tx_hash: string; chain?: string },
  ctx: Ctx,
): Promise<{ transaction: Transaction | null }> {
  return { transaction: await rpc(ctx, "eth_getTransactionByHash", [args.tx_hash], args.chain ?? "ethereum") };
}

/** A transaction's receipt, found only when it is mined; `status` is `0x1` (success) or `0x0` (reverted). */
export async function transaction_receipt(
  args: { tx_hash: string; chain?: string },
  ctx: Ctx,
): Promise<{ receipt: TransactionReceipt | null }> {
  return { receipt: await rpc(ctx, "eth_getTransactionReceipt", [args.tx_hash], args.chain ?? "ethereum") };
}

/**
 * Event logs matching an address and topics over a block range, oldest first. `topics` is the
 * positional filter array: `null` skips a position, a string matches it, an array of strings
 * matches any of them (topic0 is usually the event signature hash). Blocks are hex quantities;
 * keep ranges small or the node rejects the query.
 */
export async function logs(
  args: {
    address?: string | string[];
    topics?: (string | string[] | null)[];
    from_block?: string;
    to_block?: string;
    chain?: string;
  },
  ctx: Ctx,
): Promise<{ logs: Log[] }> {
  const filter: Record<string, unknown> = {
    fromBlock: args.from_block ?? "latest",
    toBlock: args.to_block ?? "latest",
  };
  if (args.address) filter.address = args.address;
  if (args.topics?.length) filter.topics = args.topics;
  return { logs: await rpc(ctx, "eth_getLogs", [filter], args.chain ?? "ethereum") };
}

/**
 * Spot prices by ticker (`ETH`, `BTC`, `USDC`, ...), one upstream item per symbol with `prices`
 * (currency, decimal-string `value`, `lastUpdatedAt`) and an `error` string when Alchemy has no
 * quote. At most 25 symbols per call; tickers are Pyth-style and not contract-specific.
 */
export async function prices_by_symbol(args: { symbols: string[] }, ctx: Ctx): Promise<{ data: TokenPrice[] }> {
  if (args.symbols.length === 0) throw new Error("prices_by_symbol needs at least one symbol");
  // Repeated `symbols` parameters: `?symbols=ETH&symbols=BTC`.
  const params = args.symbols.map((symbol) => `symbols=${encodeURIComponent(symbol)}`).join("&");
  return { data: (await prices(ctx, `/tokens/by-symbol?${params}`)).data };
}

/**
 * Spot prices by contract, so two tokens with the same ticker can be told apart. `network` is an
 * Alchemy prefix (`eth-mainnet`, `polygon-mainnet`, ...); `address` is the token contract. At most
 * 25 entries per call. Each result carries `prices` or an `error` string.
 */
export async function prices_by_address(
  args: { addresses: { network: string; address: string }[] },
  ctx: Ctx,
): Promise<{ data: TokenPrice[] }> {
  if (args.addresses.length === 0) throw new Error("prices_by_address needs at least one address");
  const res = await ctx.fetch(`${PRICES_BASE}/tokens/by-address`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${apiKey(ctx)}`,
      "content-type": "application/json",
      accept: "application/json",
    },
    body: JSON.stringify({ addresses: args.addresses }),
  });
  return { data: (await readJson(res, "Alchemy Prices")).data };
}

/**
 * Historical prices from a start time onward, as `{ prices: [{ timestamp, value }] }` where `value`
 * is a decimal string. Identify the asset by `symbol` or by `network` + `address`. `start_time` and
 * `end_time` are RFC 3339 timestamps (`2024-01-01T00:00:00Z`); the window is capped by the plan.
 */
export async function historical_prices(
  args: {
    symbol?: string;
    address?: string;
    network?: string;
    start_time?: string;
    end_time?: string;
  },
  ctx: Ctx,
): Promise<{ data: HistoricalPrices }> {
  if (!args.symbol && !args.address) throw new Error("historical_prices needs symbol or address");
  const path = `/tokens/historical${query({
    symbol: args.symbol,
    address: args.address,
    network: args.network,
    startTime: args.start_time,
    endTime: args.end_time,
  })}`;
  return { data: (await prices(ctx, path)).data };
}
