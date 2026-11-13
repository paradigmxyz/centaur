import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are EODHD's own objects, passed through. Field names are EODHD's
// (https://eodhd.com/financial-apis/).

const BASE = "https://eodhd.com/api";

/** A delayed quote for one ticker. */
export interface Quote {
  /** Ticker including the exchange suffix, for example "AAPL.US". */
  code: string;
  /** Quote time, Unix seconds. */
  timestamp: number;
  gmtoffset: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  previousClose?: number;
  change?: number;
  /** Percent change against the previous close. */
  change_p?: number;
  average_volume?: number;
  [field: string]: unknown;
}

/** One end-of-day OHLCV bar. */
export interface EodPrice {
  /** Trading date, YYYY-MM-DD. */
  date: string;
  open: number;
  high: number;
  low: number;
  close: number;
  adjusted_close: number;
  volume: number;
  [field: string]: unknown;
}

/**
 * EODHD wants the exchange suffix in the ticker. US tickers may omit it (`AAPL` becomes `AAPL.US`);
 * anything containing a dot is used as given (`VOD.LSE`, `BMW.XETRA`).
 */
function symbol(value: string): string {
  const ticker = value.trim().toUpperCase();
  if (!ticker) throw new Error("symbol is required, for example \"AAPL\"");
  return ticker.includes(".") ? ticker : `${ticker}.US`;
}

async function request(
  ctx: Ctx,
  path: string,
  params: Record<string, string | number | undefined> = {},
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  const queryParams: Record<string, string | number | undefined> = { ...params, fmt: "json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.EODHD_API_KEY) queryParams.api_token = ctx.env.EODHD_API_KEY;
  const res = await ctx.fetch(`${BASE}${path}${query(queryParams)}`, { headers });
  const body = await readJson(res, "EODHD");
  // EODHD reports some failures as a 200 with an `error` field.
  if (body && !Array.isArray(body) && typeof body === "object" && body.error) {
    const message = body.message ?? body.error;
    throw new Error(`EODHD error: ${typeof message === "string" ? message : JSON.stringify(message)}`);
  }
  return body;
}

/**
 * Latest delayed quote for one ticker: open, high, low, close, volume, change and percent change.
 * US tickers may omit the exchange suffix (`AAPL` becomes `AAPL.US`); pass a suffixed ticker for
 * other exchanges (`VOD.LSE`). Delayed by the exchange's quote delay, not real-time for all plans.
 */
export async function get_quote(args: { symbol: string }, ctx: Ctx): Promise<Quote> {
  return request(ctx, `/real-time/${encodeURIComponent(symbol(args.symbol))}`);
}

/**
 * Historical daily OHLCV bars for one ticker, oldest first. `from_date`/`to_date` are YYYY-MM-DD
 * and inclusive; omit both for the full history (the default period is daily). `adjusted_close`
 * adjusts for splits and dividends. Tickers take the exchange suffix; US tickers may omit it.
 */
export async function get_eod_prices(
  args: { symbol: string; from_date?: string; to_date?: string },
  ctx: Ctx,
): Promise<{ prices: EodPrice[] }> {
  const params: Record<string, string | undefined> = {};
  if (args.from_date) params.from = args.from_date;
  if (args.to_date) params.to = args.to_date;
  return { prices: await request(ctx, `/eod/${encodeURIComponent(symbol(args.symbol))}`, params) };
}
