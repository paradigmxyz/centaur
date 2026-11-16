import type { Ctx } from "../centaur";
import { query } from "../_lib/http";

// Results are ethPandaOps' own `int_engine_new_payload` rows, passed through: field
// names and types are theirs. The API is public (no credentials) and paginated
// newest first; `slot_start_date_time_gte` and `order_by` are required by the
// upstream.

const BASE_URL = "https://lab.ethpandaops.io/api/v1";
const DEFAULT_NETWORK = "mainnet";

/** One execution payload, with the time its EL client took to validate it. */
export interface ExecutionPayload {
  slot: number;
  slot_start_date_time?: number;
  epoch?: number;
  block_number?: number;
  block_hash?: string;
  duration_ms: number;
  status?: string;
  gas_limit?: number;
  gas_used?: number;
  tx_count?: number;
  blob_count?: number;
  node_class?: string;
  meta_network_name?: string;
  meta_client_implementation?: string;
  meta_execution_implementation?: string;
  meta_execution_version?: string;
  meta_client_geo_city?: string;
  meta_client_geo_country?: string;
  [field: string]: unknown;
}

/** Per-client timing distribution over the fetched payloads, slowest average first. */
export interface ClientTiming {
  client: string;
  version: string;
  count: number;
  avg_ms: number;
  min_ms: number;
  max_ms: number;
  p50_ms: number;
  p90_ms: number;
  p99_ms: number;
}

/** Duration strings like `1h`, `6hr`, `24hr`, `7d` or `30m`; a bare number means hours. */
function parseDurationHours(duration: string): number {
  const value = duration.toLowerCase().trim();
  let seconds: number;
  if (value.endsWith("hr")) seconds = parseInt(value.slice(0, -2), 10) * 3600;
  else if (value.endsWith("h")) seconds = parseInt(value.slice(0, -1), 10) * 3600;
  else if (value.endsWith("d")) seconds = parseInt(value.slice(0, -1), 10) * 86400;
  else if (value.endsWith("m")) seconds = parseInt(value.slice(0, -1), 10) * 60;
  else seconds = parseInt(value, 10) * 3600;
  if (!Number.isFinite(seconds)) throw new Error(`reth: cannot parse duration: ${duration}`);
  return Math.max(1, Math.floor(seconds / 3600));
}

/**
 * Recent execution payloads with each client's `duration_ms`. `hours` (default 1) or a
 * `duration` such as `6hr`/`7d` sets the window; `page_size` (default 500) caps how many of the
 * newest payloads come back. `network` is the devnet name (default `mainnet`).
 */
export async function get_execution_timings(
  args: { hours?: number; duration?: string; page_size?: number; network?: string },
  ctx: Ctx,
): Promise<{ payloads: ExecutionPayload[]; network: string; since: number }> {
  const hours = args.duration ? parseDurationHours(args.duration) : (args.hours ?? 1);
  const network = args.network ?? DEFAULT_NETWORK;
  const since = Math.floor(Date.now() / 1000) - hours * 3600;
  const res = await ctx.fetch(
    `${BASE_URL}/${network}/int_engine_new_payload` +
      query({
        slot_start_date_time_gte: since,
        page_size: args.page_size ?? 500,
        order_by: "slot desc",
      }),
    { headers: { accept: "application/json" } },
  );
  const text = await res.text();
  if (!res.ok) throw new Error(`ethPandaOps returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  const body = JSON.parse(text);
  return { payloads: (body?.int_engine_new_payload ?? []) as ExecutionPayload[], network, since };
}

/**
 * Aggregate payload timings per execution client: count, average, min, max and the p50/p90/p99
 * percentiles, slowest average first. Takes the rows from `get_execution_timings`.
 */
export async function aggregate_timings(
  args: { payloads: ExecutionPayload[] },
  _ctx: Ctx,
): Promise<{ clients: ClientTiming[] }> {
  const durations = new Map<string, number[]>();
  const versions = new Map<string, string>();
  for (const payload of args.payloads) {
    const client = payload.meta_execution_implementation ?? "unknown";
    const version = payload.meta_execution_version ?? "";
    durations.set(client, [...(durations.get(client) ?? []), payload.duration_ms ?? 0]);
    if (!versions.has(client) || version.length > (versions.get(client) ?? "").length) {
      versions.set(client, version);
    }
  }
  const clients: ClientTiming[] = [];
  for (const [client, values] of durations) {
    const sorted = [...values].sort((a, b) => a - b);
    const count = sorted.length;
    clients.push({
      client,
      version: versions.get(client) ?? "",
      count,
      avg_ms: count ? Math.floor(values.reduce((sum, value) => sum + value, 0) / count) : 0,
      min_ms: count ? sorted[0] : 0,
      max_ms: count ? sorted[count - 1] : 0,
      p50_ms: count ? sorted[Math.floor(count / 2)] : 0,
      p90_ms: count ? sorted[Math.floor(count * 0.9)] : 0,
      p99_ms: count ? sorted[Math.floor(count * 0.99)] : 0,
    });
  }
  clients.sort((a, b) => a.avg_ms - b.avg_ms);
  return { clients };
}

/**
 * The slowest payloads above `threshold_ms` (default 500), longest first, capped at `limit`
 * (default 20). Takes the rows from `get_execution_timings`.
 */
export async function find_slow_payloads(
  args: { payloads: ExecutionPayload[]; threshold_ms?: number; limit?: number },
  _ctx: Ctx,
): Promise<{ payloads: ExecutionPayload[] }> {
  const threshold = args.threshold_ms ?? 500;
  const slow = args.payloads
    .filter((payload) => (payload.duration_ms ?? 0) >= threshold)
    .sort((a, b) => (b.duration_ms ?? 0) - (a.duration_ms ?? 0));
  return { payloads: slow.slice(0, args.limit ?? 20) };
}
