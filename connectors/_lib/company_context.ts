// The company-context service's API (company context v2), shared by connectors that read it.
// iron-proxy injects the principal's API JWT into requests to the service, so results are limited
// to what the calling principal can see.
import type { Ctx } from "../centaur";

/** `COMPANY_CONTEXT_API_URL`, set only where the service is deployed; `null` when unset. */
export function companyContextUrl(ctx: Ctx): string | null {
  let url = (ctx.env.COMPANY_CONTEXT_API_URL || "").trim().replace(/\/+$/, "");
  if (!url) return null;
  if (!/^https?:\/\//.test(url)) url = `http://${url}`;
  return url;
}

/** One API request; returns the parsed body, or undefined for an empty (204) response. */
export async function companyContextRequest(ctx: Ctx, method: string, path: string, body?: unknown): Promise<any> {
  const base = companyContextUrl(ctx);
  if (!base) throw new Error("COMPANY_CONTEXT_API_URL is not configured; the company-context API is not deployed");
  const headers: Record<string, string> = { accept: "application/json" };
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(`${base}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!res.ok) {
    // The API reports failures as {"error": "..."}.
    let message = text.slice(0, 1000);
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed.error === "string") message = parsed.error;
    } catch {
      // Not JSON; keep the raw text.
    }
    throw new Error(`company-context API returned HTTP ${res.status}: ${message}`);
  }
  return text ? JSON.parse(text) : undefined;
}

const PLAIN_DATE = /^\d{4}-\d{2}-\d{2}$/;

/** A plain `YYYY-MM-DD` date becomes midnight UTC; other values pass through. */
export function rfc3339(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const trimmed = value.trim();
  return PLAIN_DATE.test(trimmed) ? `${trimmed}T00:00:00Z` : value;
}
