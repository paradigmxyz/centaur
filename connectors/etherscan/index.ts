import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Etherscan's own objects, passed through from the `result` field.
// Field names and formats are Etherscan's V2 API (https://docs.etherscan.io/):
// numbers such as wei amounts, block numbers and gas prices are decimal strings.

const BASE = "https://api.etherscan.io/v2/api";

/** A normal (native-currency) transaction. */
export interface Transaction {
  /** Block number, decimal string. */
  blockNumber: string;
  /** Unix seconds, as a string. */
  timeStamp: string;
  hash: string;
  nonce: string;
  blockHash: string;
  transactionIndex: string;
  from: string;
  /** Empty for contract creation; the new contract is in `contractAddress`. */
  to: string;
  /** Value in wei, as a decimal string. */
  value: string;
  gas: string;
  gasPrice: string;
  /** "1" when the transaction failed, otherwise "0". */
  isError: string;
  /** "1" success, "0" failed (pre-Byzantium transactions have no receipt status). */
  txreceipt_status: string;
  input: string;
  contractAddress: string;
  cumulativeGasUsed: string;
  gasUsed: string;
  confirmations: string;
  methodId: string;
  functionName: string;
  [field: string]: unknown;
}

/** An ERC-20 transfer event. */
export interface TokenTransfer {
  blockNumber: string;
  timeStamp: string;
  hash: string;
  nonce: string;
  from: string;
  contractAddress: string;
  to: string;
  /** Raw token amount, decimal string; divide by 10^tokenDecimal. */
  value: string;
  tokenName: string;
  tokenSymbol: string;
  tokenDecimal: string;
  transactionIndex: string;
  gas: string;
  gasPrice: string;
  gasUsed: string;
  cumulativeGasUsed: string;
  input: string;
  confirmations: string;
  [field: string]: unknown;
}

/** A verified contract's source (one object per contract). */
export interface ContractSource {
  /** Solidity source; JSON with all files when the contract uses imports. */
  SourceCode: string;
  /** ABI as a JSON string. */
  ABI: string;
  ContractName: string;
  CompilerVersion: string;
  OptimizationUsed: string;
  Runs: string;
  ConstructorArguments: string;
  EVMVersion: string;
  Library: string;
  LicenseType: string;
  Proxy: string;
  Implementation: string;
  SwarmSource: string;
  [field: string]: unknown;
}

/** ETH reference prices. */
export interface EthPrice {
  ethbtc: string;
  ethusd: string;
  /** Unix seconds, as a string. */
  ethbtc_timestamp: string;
  /** Unix seconds, as a string. */
  ethusd_timestamp: string;
  [field: string]: unknown;
}

/** One event log. */
export interface Log {
  address: string;
  topics: string[];
  data: string;
  blockNumber: string;
  timeStamp: string;
  gasPrice: string;
  gasUsed: string;
  logIndex: string;
  transactionHash: string;
  transactionIndex: string;
  [field: string]: unknown;
}

/**
 * Etherscan V2 serves every supported chain from one host; `chainid` picks the chain. `result` is
 * the API's payload, `status`/`message` its error envelope.
 */
async function request(
  ctx: Ctx,
  chainId: number,
  params: Record<string, string | number>,
): Promise<any> {
  const queryParams: Record<string, string | number> = { ...params, chainid: chainId };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.ETHERSCAN_API_KEY) queryParams.apikey = ctx.env.ETHERSCAN_API_KEY;
  const res = await ctx.fetch(`${BASE}${query(queryParams)}`, { headers: { accept: "application/json" } });
  const body = await readJson(res, "Etherscan");
  if (body && body.status === "0") {
    const message = String(body.message ?? "");
    const result = body.result;
    // Empty result sets are `status: "0"` with a "No transactions found" message, not errors.
    if (/no (transactions|records|logs) found/i.test(message) || /no (transactions|records|logs) found/i.test(String(result))) {
      return [];
    }
    const detail = typeof result === "string" ? result : message || JSON.stringify(result);
    throw new Error(`Etherscan error: ${detail}`);
  }
  return body && body.result !== undefined ? body.result : body;
}

/**
 * Native-currency (for example ETH) balance of an address, in wei as a decimal string. `chain_id`
 * is the Etherscan V2 chain id: 1 Ethereum (default), 8453 Base, 42161 Arbitrum One, 137 Polygon,
 * 10 OP Mainnet, 56 BNB Chain, and so on.
 */
export async function get_balance(
  args: { address: string; chain_id?: number },
  ctx: Ctx,
): Promise<{ balance: string }> {
  const balance = await request(ctx, args.chain_id ?? 1, {
    module: "account",
    action: "balance",
    address: args.address.trim(),
    tag: "latest",
  });
  return { balance };
}

/**
 * Normal transactions of an address, newest first by default: block, time, from/to, value in wei,
 * gas, success flags and method. `start_block`/`end_block` bound the range (default 0 to the latest),
 * `page`/`offset` paginate and `sort` is "asc" or "desc". Etherscan caps `page * offset` at 10000,
 * so walk block ranges for deeper history. An empty range returns an empty list.
 */
export async function get_transactions(
  args: {
    address: string;
    start_block?: number;
    end_block?: number;
    page?: number;
    offset?: number;
    sort?: "asc" | "desc";
    chain_id?: number;
  },
  ctx: Ctx,
): Promise<{ transactions: Transaction[] }> {
  const transactions = await request(ctx, args.chain_id ?? 1, {
    module: "account",
    action: "txlist",
    address: args.address.trim(),
    startblock: args.start_block ?? 0,
    endblock: args.end_block ?? 99999999,
    page: args.page ?? 1,
    offset: args.offset ?? 20,
    sort: args.sort ?? "desc",
  });
  return { transactions };
}

/**
 * ERC-20 transfer events of an address, newest first by default: token contract, symbol, decimals,
 * raw value and both parties. `contract_address` narrows to one token. `page`/`offset` paginate
 * (up to 10000 records) and `sort` is "asc" or "desc". An empty range returns an empty list.
 */
export async function get_token_transfers(
  args: {
    address: string;
    contract_address?: string;
    page?: number;
    offset?: number;
    sort?: "asc" | "desc";
    chain_id?: number;
  },
  ctx: Ctx,
): Promise<{ transfers: TokenTransfer[] }> {
  const params: Record<string, string | number> = {
    module: "account",
    action: "tokentx",
    address: args.address.trim(),
    page: args.page ?? 1,
    offset: args.offset ?? 20,
    sort: args.sort ?? "desc",
  };
  if (args.contract_address) params.contractaddress = args.contract_address.trim();
  return { transfers: await request(ctx, args.chain_id ?? 1, params) };
}

/**
 * ABI of a verified contract, as the JSON string Etherscan returns (pass it to a decoder or
 * `JSON.parse` it). Unverified contracts fail with Etherscan's "Contract source code not verified".
 */
export async function get_contract_abi(
  args: { address: string; chain_id?: number },
  ctx: Ctx,
): Promise<{ abi: string }> {
  return {
    abi: await request(ctx, args.chain_id ?? 1, {
      module: "contract",
      action: "getabi",
      address: args.address.trim(),
    }),
  };
}

/**
 * Source code of a verified contract: files, ABI string, contract name, compiler version and
 * optimization settings, constructor arguments, EVM version, licence and proxy implementation.
 * `SourceCode` is JSON holding all files when the contract has imports.
 */
export async function get_contract_source(
  args: { address: string; chain_id?: number },
  ctx: Ctx,
): Promise<{ sources: ContractSource[] }> {
  return {
    sources: await request(ctx, args.chain_id ?? 1, {
      module: "contract",
      action: "getsourcecode",
      address: args.address.trim(),
    }),
  };
}

/** ETH price in BTC and USD, with timestamps (Unix seconds as strings). */
export async function get_eth_price(args: { chain_id?: number }, ctx: Ctx): Promise<EthPrice> {
  return request(ctx, args.chain_id ?? 1, { module: "stats", action: "ethprice" });
}

/**
 * Total supply of an ERC-20 token, in the token's smallest unit as a decimal string (divide by
 * 10^decimals for whole tokens).
 */
export async function get_token_supply(
  args: { contract_address: string; chain_id?: number },
  ctx: Ctx,
): Promise<{ supply: string }> {
  const supply = await request(ctx, args.chain_id ?? 1, {
    module: "stats",
    action: "tokensupply",
    contractaddress: args.contract_address.trim(),
  });
  return { supply };
}

/**
 * Current gas price as a hex string in wei per gas (for example "0x3b9aca00" is 1 gwei). The
 * `proxy` module answers in raw JSON-RPC form, so this is the `result` string only.
 */
export async function get_gas_price(args: { chain_id?: number }, ctx: Ctx): Promise<{ gas_price: string }> {
  const gas_price = await request(ctx, args.chain_id ?? 1, { module: "proxy", action: "eth_gasPrice" });
  return { gas_price };
}

/**
 * Event logs emitted by a contract address between two block numbers (inclusive), oldest first:
 * `topics`, `data`, block, transaction hash and log index. `topic0` filters by event signature hash.
 * Etherscan returns at most 1000 records per request; narrow the block range to page over more.
 */
export async function get_logs(
  args: { address: string; from_block: number; to_block: number; topic0?: string; chain_id?: number },
  ctx: Ctx,
): Promise<{ logs: Log[] }> {
  const params: Record<string, string | number> = {
    module: "logs",
    action: "getLogs",
    address: args.address.trim(),
    fromBlock: args.from_block,
    toBlock: args.to_block,
  };
  if (args.topic0) params.topic0 = args.topic0.trim();
  return { logs: await request(ctx, args.chain_id ?? 1, params) };
}
