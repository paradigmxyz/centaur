import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are Tenderly's own objects, passed through; interfaces list the fields
// worth knowing. The account and project slugs (and the access key) are
// placeholders that iron-proxy swaps on the wire: the slugs appear in the path
// of every project-scoped call, the key in an `X-Access-Key` header. Virtual
// TestNet calls go to the vnet's own admin RPC URL, which Tenderly returns from
// `get_vnet`.

const API_BASE = "https://api.tenderly.co/api/v1";

/** The authenticated Tenderly user. */
export interface User {
  id?: string;
  username?: string;
  email?: string;
  [field: string]: unknown;
}

/** A Tenderly project. */
export interface Project {
  id: string;
  name: string;
  slug: string;
  [field: string]: unknown;
}

/** A public EVM network Tenderly supports. */
export interface Network {
  id: string;
  name: string;
  slug: string;
  native_currency?: { symbol?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** A verified contract: name, ABI, compiler settings and prepared source. */
export interface Contract {
  contract_name?: string;
  address?: string;
  network_id?: string;
  compiler?: string;
  abi?: unknown;
  source?: unknown;
  [field: string]: unknown;
}

/** One transaction to simulate; `value` and `gas` are decimal wei, `input` is hex calldata. */
export interface SimulationRequest {
  network_id?: string | number;
  from: string;
  to: string;
  input?: string;
  value?: number;
  gas?: number;
  gas_price?: number;
  gas_price_raw?: string;
  block_number?: number;
  simulation_type?: string;
  state_objects?: Record<string, unknown>;
  [field: string]: unknown;
}

/** A Tenderly simulation result, including transaction info and the nested call trace. */
export interface Simulation {
  transaction?: {
    status?: boolean;
    gas_used?: number;
    block_number?: number;
    error_message?: string | null;
    transaction_info?: { call_trace?: CallTrace[]; [field: string]: unknown };
    [field: string]: unknown;
  };
  simulation?: { id?: string; [field: string]: unknown };
  contracts?: unknown[];
  [field: string]: unknown;
}

/** One node of a decoded call trace; `calls` are the child calls. */
export interface CallTrace {
  call_type?: string;
  type?: string;
  function_name?: string;
  contract_name?: string;
  from?: string;
  to?: string;
  input?: string;
  gas_used?: number;
  error?: string;
  error_reason?: string;
  calls?: CallTrace[];
  [field: string]: unknown;
}

/** A Virtual TestNet. */
export interface Vnet {
  id: string;
  slug: string;
  display_name?: string;
  status?: string;
  fork_config?: { network_id?: number; block_number?: number | string; [field: string]: unknown };
  rpcs?: { name: string; url: string; [field: string]: unknown }[];
  [field: string]: unknown;
}

/** One transaction executed on a Virtual TestNet. */
export interface VnetTransaction {
  hash?: string;
  block_number?: number;
  from?: string;
  to?: string;
  status?: boolean;
  gas_used?: number;
  [field: string]: unknown;
}

function accessKey(ctx: Ctx): string {
  const key = ctx.env.TENDERLY_ACCESS_KEY;
  if (!key) throw new Error("Tenderly: TENDERLY_ACCESS_KEY is not set.");
  return key;
}

function accountSlug(ctx: Ctx): string {
  const account = ctx.env.TENDERLY_ACCOUNT_SLUG;
  if (!account) throw new Error("Tenderly: TENDERLY_ACCOUNT_SLUG is not set.");
  return account;
}

function projectSlug(ctx: Ctx): string {
  const project = ctx.env.TENDERLY_PROJECT_SLUG;
  if (!project) throw new Error("Tenderly: TENDERLY_PROJECT_SLUG is not set.");
  return project;
}

/** `/account/{account}/project/{project}`, the prefix of every project-scoped call. */
function projectPath(ctx: Ctx): string {
  return `/account/${accountSlug(ctx)}/project/${projectSlug(ctx)}`;
}

async function request(
  ctx: Ctx,
  method: string,
  path: string,
  options: { body?: unknown; params?: Params } = {},
): Promise<any> {
  const headers: Record<string, string> = {
    "X-Access-Key": accessKey(ctx),
    accept: "application/json",
  };
  if (options.body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(API_BASE + path + query(options.params ?? {}), {
    method,
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Tenderly returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return text ? JSON.parse(text) : null;
}

async function rpc(ctx: Ctx, vnetId: string, method: string, params?: unknown[]): Promise<any> {
  const { vnet } = await get_vnet({ vnet_id: vnetId }, ctx);
  const rpcs = (vnet.rpcs ?? []) as { name: string; url: string }[];
  const rpc = rpcs.find((entry) => entry.name === "Admin RPC") ?? rpcs[0];
  if (!rpc) throw new Error(`Tenderly: no RPC endpoints found for vnet ${vnetId}.`);
  const res = await ctx.fetch(rpc.url, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", method, params: params ?? [], id: 1 }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Tenderly RPC returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  const body = JSON.parse(text);
  if (body.error) throw new Error(`Tenderly RPC error: ${JSON.stringify(body.error)}`);
  return body.result;
}

/** Info about the authenticated Tenderly user. */
export async function get_user(args: Record<string, never>, ctx: Ctx): Promise<{ user: User }> {
  return { user: await request(ctx, "GET", "/user") };
}

/** Projects in the configured account. */
export async function list_projects(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ projects: Project[] }> {
  const body = await request(ctx, "GET", `/account/${accountSlug(ctx)}/projects`);
  return { projects: (Array.isArray(body) ? body : (body?.projects ?? [])) as Project[] };
}

/** Public EVM networks Tenderly supports, with id (network ID), name and slug. */
export async function get_networks(
  args: Record<string, never>,
  ctx: Ctx,
): Promise<{ networks: Network[] }> {
  const body = await request(ctx, "GET", "/public-networks");
  return { networks: (Array.isArray(body) ? body : (body?.networks ?? [])) as Network[] };
}

/** Metadata (name, ABI, compiler, source) for a verified contract on a public network. */
export async function get_contract(
  args: { network_id: string; address: string },
  ctx: Ctx,
): Promise<{ contract: Contract }> {
  return {
    contract: await request(
      ctx,
      "GET",
      `/public-contract/${args.network_id}/${args.address.toLowerCase()}`,
    ),
  };
}

/**
 * Simulate a transaction on a public network. `value`/`gas`/`gas_price` are wei (decimal),
 * `input` is hex calldata, `block_number` pins a historical block (default latest), and
 * `save` stores the simulation in Tenderly. The result carries `transaction.status`,
 * `transaction.gas_used`, `transaction.error_message` and the call trace under
 * `transaction.transaction_info.call_trace`.
 */
export async function simulate(
  args: {
    network_id: string;
    from_address: string;
    to_address: string;
    input_data?: string;
    value?: number;
    gas?: number;
    gas_price?: number;
    block_number?: number;
    simulation_type?: string;
    state_objects?: Record<string, unknown>;
    save?: boolean;
  },
  ctx: Ctx,
): Promise<{ simulation: Simulation }> {
  const payload: Record<string, unknown> = {
    network_id: String(args.network_id),
    from: args.from_address,
    to: args.to_address,
    input: args.input_data ?? "0x",
    value: args.value ?? 0,
    gas: args.gas ?? 8_000_000,
    simulation_type: args.simulation_type ?? "full",
    save: args.save ?? false,
    save_if_fails: args.save ?? false,
  };
  if (args.gas_price !== undefined) payload.gas_price = String(args.gas_price);
  if (args.block_number !== undefined) payload.block_number = args.block_number;
  if (args.state_objects) payload.state_objects = args.state_objects;
  return { simulation: await request(ctx, "POST", `${projectPath(ctx)}/simulate`, { body: payload }) };
}

/**
 * Simulate transactions executed in sequence in one shared state. Each entry is a
 * simulation request (`from`, `to`, `input`, `value`, `gas`, ...); the response's
 * `simulation_results` holds one result per entry, in order.
 */
export async function simulate_bundle(
  args: { simulations: SimulationRequest[] },
  ctx: Ctx,
): Promise<{ simulation_results: Simulation[]; [field: string]: unknown }> {
  return await request(ctx, "POST", `${projectPath(ctx)}/simulate-bundle`, {
    body: { simulations: args.simulations },
  });
}

/**
 * The decoded execution trace of an on-chain transaction: the call tree with function names,
 * gas used and per-call error reasons. Navigate it with `flatten_trace`, `find_failures` or
 * `error_path`.
 */
export async function trace_transaction(
  args: { network_id: string; tx_hash: string },
  ctx: Ctx,
): Promise<{ trace: Record<string, unknown> }> {
  return {
    trace: await request(
      ctx,
      "GET",
      `/public-contract/${args.network_id}/trace/${args.tx_hash}`,
    ),
  };
}

/**
 * Create a Virtual TestNet by forking a public network at `block_number` (`latest` by
 * default). `slug` is the vnet's unique name in the project; `chain_id` defaults to the
 * forked network's ID. The response includes the vnet `id` and its RPC URLs.
 */
export async function create_vnet(
  args: {
    network_id: string;
    slug: string;
    display_name?: string;
    block_number?: number | string;
    chain_id?: number;
  },
  ctx: Ctx,
): Promise<{ vnet: Vnet }> {
  const body = {
    slug: args.slug,
    display_name: args.display_name ?? args.slug,
    fork_config: {
      network_id: Number(args.network_id),
      block_number: args.block_number ?? "latest",
    },
    virtual_network_config: {
      chain_config: {
        chain_id: args.chain_id ?? Number(args.network_id),
      },
    },
    sync_state_config: { enabled: false },
    explorer_page_config: { enabled: false, verification_visibility: "bytecode" },
  };
  return { vnet: await request(ctx, "POST", `${projectPath(ctx)}/vnets`, { body }) };
}

/** Virtual TestNets in the project, paginated by `page`/`per_page`. */
export async function list_vnets(
  args: { page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ vnets: Vnet[] }> {
  const body = await request(ctx, "GET", `${projectPath(ctx)}/vnets`, {
    params: { page: args.page ?? 1, perPage: args.per_page ?? 20 },
  });
  return { vnets: (Array.isArray(body) ? body : (body?.vnets ?? [])) as Vnet[] };
}

/** Full details of one Virtual TestNet, including its RPC URLs and fork configuration. */
export async function get_vnet(args: { vnet_id: string }, ctx: Ctx): Promise<{ vnet: Vnet }> {
  return { vnet: await request(ctx, "GET", `${projectPath(ctx)}/vnets/${args.vnet_id}`) };
}

/** Delete a Virtual TestNet. */
export async function delete_vnet(
  args: { vnet_id: string },
  ctx: Ctx,
): Promise<{ deleted: boolean; vnet_id: string }> {
  await request(ctx, "DELETE", `${projectPath(ctx)}/vnets/${args.vnet_id}`);
  return { deleted: true, vnet_id: args.vnet_id };
}

/** Transactions executed on a Virtual TestNet, paginated by `page`/`per_page`. */
export async function list_vnet_transactions(
  args: { vnet_id: string; page?: number; per_page?: number },
  ctx: Ctx,
): Promise<{ transactions: VnetTransaction[] }> {
  const body = await request(ctx, "GET", `${projectPath(ctx)}/vnets/${args.vnet_id}/transactions`, {
    params: { page: args.page ?? 1, perPage: args.per_page ?? 20 },
  });
  return {
    transactions: (Array.isArray(body) ? body : (body?.transactions ?? [])) as VnetTransaction[],
  };
}

/**
 * Simulate a transaction against a Virtual TestNet without changing its state; the result has
 * the same shape as `simulate`.
 */
export async function simulate_vnet_transaction(
  args: {
    vnet_id: string;
    from_address: string;
    to_address: string;
    input_data?: string;
    value?: number;
    gas?: number;
    block_number?: number;
  },
  ctx: Ctx,
): Promise<{ simulation: Simulation }> {
  const payload: Record<string, unknown> = {
    callArgs: {
      from: args.from_address,
      to: args.to_address,
      data: args.input_data ?? "0x",
      value: `0x${(args.value ?? 0).toString(16)}`,
      gas: `0x${(args.gas ?? 8_000_000).toString(16)}`,
    },
  };
  if (args.block_number !== undefined) payload.blockNumber = `0x${args.block_number.toString(16)}`;
  return {
    simulation: await request(
      ctx,
      "POST",
      `${projectPath(ctx)}/vnets/${args.vnet_id}/transactions/simulate`,
      { body: payload },
    ),
  };
}

/** Make an arbitrary JSON-RPC call against a Virtual TestNet's admin RPC; returns `result`. */
export async function vnet_rpc(
  args: { vnet_id: string; method: string; params?: unknown[] },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  return { result: await rpc(ctx, args.vnet_id, args.method, args.params) };
}

/**
 * Send an unsigned, impersonated transaction (`eth_sendTransaction`) on a Virtual TestNet.
 * `value` and `gas` are wei; no key or signature is needed.
 */
export async function send_vnet_transaction(
  args: {
    vnet_id: string;
    from_address: string;
    to_address: string;
    input_data?: string;
    value?: number;
    gas?: number;
  },
  ctx: Ctx,
): Promise<{ transaction_hash: string }> {
  const tx: Record<string, unknown> = {
    from: args.from_address,
    to: args.to_address,
    data: args.input_data ?? "0x",
    value: `0x${(args.value ?? 0).toString(16)}`,
  };
  if (args.gas !== undefined) tx.gas = `0x${args.gas.toString(16)}`;
  return {
    transaction_hash: await rpc(ctx, args.vnet_id, "eth_sendTransaction", [tx]),
  };
}

/**
 * Read-only `eth_call` on a Virtual TestNet; returns the hex return data (decode it, for
 * example with a function's ABI).
 */
export async function vnet_call(
  args: {
    vnet_id: string;
    to_address: string;
    input_data: string;
    from_address?: string;
    block?: string;
  },
  ctx: Ctx,
): Promise<{ result: string }> {
  const call: Record<string, unknown> = { to: args.to_address, data: args.input_data };
  if (args.from_address) call.from = args.from_address;
  return {
    result: await rpc(ctx, args.vnet_id, "eth_call", [call, args.block ?? "latest"]),
  };
}

/** Set the native balance of one or more accounts; `amount_wei` is decimal wei. */
export async function set_balance(
  args: { vnet_id: string; addresses: string[]; amount_wei: number },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  return {
    result: await rpc(ctx, args.vnet_id, "tenderly_setBalance", [
      args.addresses,
      `0x${args.amount_wei.toString(16)}`,
    ]),
  };
}

/** Set an ERC-20 balance; `amount` is in the token's base units. */
export async function set_erc20_balance(
  args: { vnet_id: string; token: string; wallet: string; amount: number },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  return {
    result: await rpc(ctx, args.vnet_id, "tenderly_setErc20Balance", [
      args.token,
      args.wallet,
      `0x${args.amount.toString(16)}`,
    ]),
  };
}

/** Write a raw 32-byte value to a contract storage slot; `slot` and `value` are hex. */
export async function set_storage_at(
  args: { vnet_id: string; contract: string; slot: string; value: string },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  return {
    result: await rpc(ctx, args.vnet_id, "tenderly_setStorageAt", [
      args.contract,
      args.slot,
      args.value,
    ]),
  };
}

/** Save the current Virtual TestNet state; returns the snapshot ID for `revert`. */
export async function snapshot(
  args: { vnet_id: string },
  ctx: Ctx,
): Promise<{ snapshot_id: string }> {
  return { snapshot_id: await rpc(ctx, args.vnet_id, "evm_snapshot") };
}

/** Restore a Virtual TestNet to a snapshot created by `snapshot`. */
export async function revert(
  args: { vnet_id: string; snapshot_id: string },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  return { result: await rpc(ctx, args.vnet_id, "evm_revert", [args.snapshot_id]) };
}

/** Advance the Virtual TestNet clock by `seconds`. */
export async function increase_time(
  args: { vnet_id: string; seconds: number },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  return {
    result: await rpc(ctx, args.vnet_id, "evm_increaseTime", [
      `0x${args.seconds.toString(16)}`,
    ]),
  };
}

/** Mine `blocks` blocks (default 1) on a Virtual TestNet. */
export async function mine_blocks(
  args: { vnet_id: string; blocks?: number },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  const blocks = args.blocks ?? 1;
  if (blocks === 1) return { result: await rpc(ctx, args.vnet_id, "evm_mine") };
  return { result: await rpc(ctx, args.vnet_id, "evm_increaseBlocks", [`0x${blocks.toString(16)}`]) };
}

// --- Trace navigation (operates on the `call_trace` a simulate or trace returns) ---

function callLabel(node: CallTrace): string {
  const name = node.function_name || (node.input ?? "").slice(0, 10) || "fallback";
  const target = node.contract_name || node.to || "";
  return target ? `${target}.${name}` : name;
}

function walk(node: CallTrace | undefined, depth: number): { node: CallTrace; depth: number }[] {
  if (!node) return [];
  const entries = [{ node, depth }];
  for (const child of node.calls ?? []) entries.push(...walk(child, depth + 1));
  return entries;
}

/**
 * Flatten a call trace into `{depth, type, label, from, to, gas_used, error}` rows in
 * execution order.
 */
export async function flatten_trace(
  args: { trace: CallTrace },
  _ctx: Ctx,
): Promise<{ calls: Record<string, unknown>[] }> {
  return {
    calls: walk(args.trace, 0).map(({ node, depth }) => ({
      depth,
      type: node.call_type ?? node.type ?? "CALL",
      label: callLabel(node),
      from: node.from ?? "",
      to: node.to ?? "",
      gas_used: node.gas_used ?? 0,
      error: node.error || node.error_reason || "",
    })),
  };
}

/**
 * A compressed call tree: nodes down to `max_depth` with their immediate child count, so a
 * huge trace stays readable. `truncated` marks nodes whose children were cut off.
 */
export async function trace_skeleton(
  args: { trace: CallTrace; max_depth?: number },
  _ctx: Ctx,
): Promise<{ calls: Record<string, unknown>[] }> {
  const maxDepth = args.max_depth ?? 3;
  return {
    calls: walk(args.trace, 0)
      .filter(({ depth }) => depth <= maxDepth)
      .map(({ node, depth }) => {
        const children = node.calls ?? [];
        return {
          depth,
          label: callLabel(node),
          gas_used: node.gas_used ?? 0,
          error: node.error ?? "",
          children: children.length,
          truncated: depth === maxDepth && children.length > 0,
        };
      }),
  };
}

/** Every errored call anywhere in the trace, with its depth, label, endpoints and reason. */
export async function find_failures(
  args: { trace: CallTrace },
  _ctx: Ctx,
): Promise<{ failures: Record<string, unknown>[] }> {
  return {
    failures: walk(args.trace, 0)
      .filter(({ node }) => node.error || node.error_reason)
      .map(({ node, depth }) => ({
        depth,
        label: callLabel(node),
        from: node.from ?? "",
        to: node.to ?? "",
        error: node.error || node.error_reason || "",
      })),
  };
}

/**
 * The blame chain from the root call down to the deepest errored call, which is usually where
 * the revert originated. Empty when nothing failed.
 */
export async function error_path(
  args: { trace: CallTrace },
  _ctx: Ctx,
): Promise<{ path: Record<string, unknown>[] }> {
  const path: Record<string, unknown>[] = [];
  const root: CallTrace | undefined = args.trace;
  if (!root || !(root.error || root.error_reason)) return { path };
  let current: CallTrace = root;
  let depth = 0;
  path.push({ depth, label: callLabel(current), error: current.error || current.error_reason });
  for (;;) {
    const calls: CallTrace[] = current.calls ?? [];
    const errored: CallTrace[] = calls.filter((child) => child.error || child.error_reason);
    if (!errored.length) return { path };
    current = errored[errored.length - 1];
    depth += 1;
    path.push({ depth, label: callLabel(current), error: current.error || current.error_reason });
  }
}
