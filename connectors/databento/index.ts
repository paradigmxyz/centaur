import type { Ctx } from "../centaur";
import { query } from "../_lib/http";

// Databento's Historical API. `criteria` are passed through unchanged except
// for the fixed dataset and JSON encoding; the response is newline-delimited
// JSON, one bar per line, returned as-is.

const DEFAULT_BASE = "https://hist.databento.com/v0";
/** Nasdaq TotalView-ITCH: US equities and ETFs, daily/hourly/minute bars. */
const DATASET = "XNAS.ITCH";

/**
 * One OHLCV bar as Databento's JSON encoding sends it. Prices and volume arrive as
 * fixed-point decimal strings (`"185.150000000"`); `ts_event` is RFC 3339 in UTC.
 */
export interface OhlcvBar {
  /** Bar start, UTC: `2026-01-02T00:00:00.000000000Z`. */
  ts_event: string;
  /** When Databento received the record. */
  ts_recv?: string;
  /** Record type (`33` for OHLCV-1d and friends). */
  rtype?: number;
  publisher_id?: number;
  instrument_id?: number;
  /** Nasdaq symbol when the request asked for symbol output. */
  symbol?: string;
  open: string;
  high: string;
  low: string;
  close: string;
  volume: string;
  [field: string]: unknown;
}

/** Databento errors are `{"detail": {"case", "message", ...}}`; surface the message. */
function errorMessage(text: string): string {
  try {
    const body = JSON.parse(text);
    const detail = body?.detail;
    if (typeof detail === "string") return detail;
    if (detail && typeof detail === "object" && typeof detail.message === "string") return detail.message;
    if (typeof body?.message === "string") return body.message;
    if (typeof body?.error === "string") return body.error;
  } catch {
    // Not JSON: fall through to the body itself.
  }
  return text.replace(/\s+/g, " ").trim().slice(0, 1000);
}

/**
 * OHLCV history for one US equity symbol from Databento's Historical API (dataset `XNAS.ITCH`,
 * Nasdaq TotalView). `symbol` is a raw Nasdaq symbol such as `AAPL` (comma-separated symbols
 * work too). `start_date`/`end_date` are `YYYY-MM-DD`: start inclusive, end exclusive, so one
 * day is `start_date=X`, `end_date=X+1`. `schema` is `ohlcv-1d` (default), `ohlcv-1h`,
 * `ohlcv-1m`, `ohlcv-1s` or `ohlcv-1ms`. Returns the bars Databento sends, oldest first, with
 * prices and volume as decimal strings; an empty range returns no records. Requests are billed
 * by Databento per symbol and range, so keep ranges narrow.
 */
export async function get_stock_prices(
  args: { symbol: string; start_date: string; end_date: string; schema?: string },
  ctx: Ctx,
): Promise<{ records: OhlcvBar[] }> {
  const base = (ctx.env.DATABENTO_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
  const params = query({
    dataset: DATASET,
    symbols: args.symbol,
    schema: args.schema || "ohlcv-1d",
    start: args.start_date,
    end: args.end_date,
    encoding: "json",
    stype_in: "raw_symbol",
  });
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire as Basic <base64(key:)>; locally it comes
  // from the environment.
  if (ctx.env.DATABENTO_API_KEY) {
    headers.authorization = `Basic ${ctx.base64.encode(`${ctx.env.DATABENTO_API_KEY}:`)}`;
  }
  const res = await ctx.fetch(`${base}/timeseries.get_range${params}`, { headers });
  const text = await res.text();
  if (!res.ok) throw new Error(`Databento returned HTTP ${res.status}: ${errorMessage(text)}`);
  const records: OhlcvBar[] = [];
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      records.push(JSON.parse(trimmed));
    } catch {
      throw new Error(`Databento returned a non-JSON line: ${trimmed.slice(0, 200)}`);
    }
  }
  return { records };
}
