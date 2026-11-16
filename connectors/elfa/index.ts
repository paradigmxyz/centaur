import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are Elfa's own objects, passed through. Field names are Elfa's
// (https://docs.elfa.ai/api/rest/get-event-summary-v-2/).

const BASE = "https://api.elfa.ai";

/** One event summary: prose plus the tweets it is based on. */
export interface EventSummary {
  summary: string;
  /** Tweet IDs behind the summary; fetch a tweet with `https://x.com/i/status/<id>`. */
  tweetIds?: string[];
  /** Links to the underlying tweets or articles. */
  sourceLinks?: string[];
  [field: string]: unknown;
}

/** Elfa's response envelope for a keyword lookup. */
export interface EventSummaryResponse {
  success: boolean;
  data: EventSummary[];
  [field: string]: unknown;
}

/**
 * Summarize recent posts about `keywords` (a ticker such as "ETH", a name, or a comma-separated
 * list of alternatives) with source links, as one or more event summaries. `time_window` is the
 * lookback and takes hour/day units: "1h", "24h" (default), "7d". Each call costs several API
 * credits (5) and the upstream lookup can take a while; the whole response is returned.
 */
export async function event_summary(
  args: { keywords: string; time_window?: string },
  ctx: Ctx,
): Promise<EventSummaryResponse> {
  const keywords = args.keywords.trim();
  if (!keywords) throw new Error("keywords is required, for example \"ETH\" or \"ETH,SOL\"");
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.ELFA_API_KEY) headers["x-elfa-api-key"] = ctx.env.ELFA_API_KEY;
  const timeWindow = args.time_window?.trim() || "24h";
  const res = await ctx.fetch(`${BASE}/v2/data/event-summary${query({ keywords, timeWindow })}`, {
    headers,
  });
  const body = await readJson(res, "Elfa");
  if (body && body.success === false) {
    const message = body.message ?? body.error ?? JSON.stringify(body);
    throw new Error(`Elfa event-summary failed: ${typeof message === "string" ? message : JSON.stringify(message)}`);
  }
  return body;
}
