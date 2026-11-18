import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Artemis's market-data endpoint. The response is passed through unchanged:
// `data.symbols` is keyed as Artemis keys it (`btc`, `eth`, ...), values keep
// upstream nulls and error strings, and a symbol with no data is omitted.

const DEFAULT_BASE = "https://data-svc.artemisxyz.com";
const MARKET_DATA_ENDPOINT = "/data/api/";
// The metrics the endpoint path asks for. 24H_PRICE_CHG_PCT is a fraction:
// -0.0414 means -4.14 %.
const METRICS = ["PRICE", "24H_PRICE_CHG_PCT"];

/** One symbol's metrics; a field is absent, null or an error string when Artemis has no value. */
export interface SymbolMetrics {
  /** Latest price in USD. */
  PRICE?: number | string | null;
  /** Rolling 24-hour change as a fraction (-0.0414 is -4.14 %). */
  "24H_PRICE_CHG_PCT"?: number | string | null;
  [metric: string]: unknown;
}

/** Artemis's `data` object: `symbols` keyed by ticker as Artemis returns it (`btc`, `eth`, ...). */
export interface MarketData {
  symbols: Record<string, SymbolMetrics>;
  [field: string]: unknown;
}

/**
 * Latest USD price and rolling 24-hour percentage change for one or more asset tickers
 * (`BTC`, `ETH`, `SOL`, ...). `symbols` is sent to Artemis joined by commas, unchanged (case and
 * order kept). Artemis lowercases the keys it answers with (`BTC` comes back as `btc`) and
 * dedupes repeated tickers; a ticker it has no value for comes back with an error string such as
 * `"Metric not available for asset."` instead of a number rather than being zero.
 * Returns Artemis's `data` object plus its `metadata` when present.
 */
export async function market_data(
  args: { symbols: string[] },
  ctx: Ctx,
): Promise<{ data: MarketData; metadata?: Record<string, unknown> }> {
  const symbols = args.symbols.filter((symbol) => symbol.trim() !== "");
  if (symbols.length === 0) throw new Error("market_data needs at least one symbol");
  const base = (ctx.env.ARTEMIS_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.ARTEMIS_API_KEY) headers["X-API-Key"] = ctx.env.ARTEMIS_API_KEY;
  const url = `${base}${MARKET_DATA_ENDPOINT}${METRICS.join(",")}/${query({ symbols: symbols.join(",") })}`;
  const body = await readJson(await ctx.fetch(url, { headers }), "Artemis");
  return { data: body.data, metadata: body.metadata };
}
