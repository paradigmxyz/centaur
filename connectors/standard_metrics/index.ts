import type { Ctx } from "../centaur";
import { encode, readJson, type Params } from "../_lib/http";

// Results are Standard Metrics' own objects, passed through: field names and
// types are theirs. Every list endpoint returns one DRF page
// (`count`, `next`, `previous`, `results`); page with `page`/`page_size` or by
// following `next`. The client authenticates with an OAuth2 client-credentials
// token that is minted per call.

const DEFAULT_BASE = "https://api.standardmetrics.io/v1";
const DEFAULT_TOKEN_URL = "https://api.standardmetrics.io/o/token/";

/** A portfolio company. */
export interface Company {
  id: string;
  name: string;
  slug?: string;
  status?: string;
  description?: string;
  city?: string;
  sector?: string;
  firm_sector?: string;
  website?: string;
  logo_url?: string;
  fiscal_year_end?: string;
  invested_fund_ids?: string[];
  investment_lead_id?: string | null;
  reporting_currency?: string;
  unique_ref?: string;
  [field: string]: unknown;
}

/** One metric data point: `value` in `currency`, for the period starting at `date`. */
export interface Metric {
  id: string;
  company_id: string;
  category: string;
  category_type?: string;
  category_id?: string;
  custom_metric?: string;
  date: string;
  value: number | string;
  currency?: string;
  converted_value?: number | string;
  preferred_currency?: string;
  fx_rate?: number;
  cadence?: string;
  is_budget_metric?: boolean;
  budget_id?: string | null;
  detailed_source?: string;
  comments?: unknown[];
  archived?: boolean;
  deleted_at?: string | null;
  created_at?: string;
  updated_at?: string;
  [field: string]: unknown;
}

/** A budget scenario a company's metrics can be reported against. */
export interface Budget {
  id: string;
  name: string;
  company: string;
  company_slug?: string;
  color?: string;
  description?: string;
  date?: string;
  [field: string]: unknown;
}

/** A document uploaded for a company (board deck, financial model, ...). */
export interface Document {
  id: string;
  name: string;
  company_id: string;
  link?: string;
  source?: string;
  parse_state?: string;
  parsed_at?: string | null;
  uploaded_at?: string;
  submitted_date?: string;
  information_request_id?: string | null;
  [field: string]: unknown;
}

/** An investment fund. */
export interface Fund {
  id: string;
  name: string;
  size?: string;
  vintage?: number;
  currency?: string;
  [field: string]: unknown;
}

/** A note on a company. */
export interface Note {
  id: string;
  company_id: string;
  note: string;
  author?: string;
  author_id?: string;
  email?: string;
  created_at?: string;
  [field: string]: unknown;
}

/** One page of a Standard Metrics list endpoint. */
export interface Paged<T> {
  count: number;
  next: string | null;
  previous: string | null;
  results: T[];
}

interface TokenResponse {
  access_token?: string;
  token_type?: string;
  expires_in?: number;
  scope?: string;
}

async function accessToken(ctx: Ctx): Promise<string> {
  // Both placeholders are swapped for the real credentials by iron-proxy.
  const clientId = ctx.env.STANDARD_METRICS_CLIENT_ID;
  const clientSecret = ctx.env.STANDARD_METRICS_CLIENT_SECRET;
  if (!clientId || !clientSecret) {
    throw new Error(
      "Standard Metrics: STANDARD_METRICS_CLIENT_ID and STANDARD_METRICS_CLIENT_SECRET are not set.",
    );
  }
  const res = await ctx.fetch(ctx.env.STANDARD_METRICS_TOKEN_URL || DEFAULT_TOKEN_URL, {
    method: "POST",
    headers: {
      authorization: `Basic ${ctx.base64.encode(`${clientId}:${clientSecret}`)}`,
      "content-type": "application/x-www-form-urlencoded",
      accept: "application/json",
    },
    body: "grant_type=client_credentials",
  });
  const token = (await readJson(res, "Standard Metrics token endpoint")) as TokenResponse;
  if (!token.access_token) throw new Error("Standard Metrics: token endpoint returned no access_token.");
  return token.access_token;
}

async function request(
  ctx: Ctx,
  path: string,
  options: { method?: string; params?: Params; ids?: string[] } = {},
): Promise<any> {
  const token = await accessToken(ctx);
  const parts = [encode(options.params ?? {})];
  // `ids[]` is repeated, which `encode` (a flat map) cannot express.
  for (const id of options.ids ?? []) parts.push(`ids%5B%5D=${encodeURIComponent(id)}`);
  const search = parts.filter(Boolean).join("&");
  const res = await ctx.fetch(
    (ctx.env.STANDARD_METRICS_API_URL || DEFAULT_BASE) + path + (search ? `?${search}` : ""),
    {
      method: options.method || "GET",
      headers: { authorization: `Bearer ${token}`, accept: "application/json" },
    },
  );
  return readJson(res, "Standard Metrics");
}

/**
 * Portfolio companies, with id, name, slug, status, sector, website, description and funds.
 * `name` is a case-insensitive substring match; `ids` filters by company id. `page` is 1-based.
 */
export async function list_companies(
  args: { page?: number; page_size?: number; name?: string; ids?: string[] },
  ctx: Ctx,
): Promise<Paged<Company>> {
  return await request(ctx, "/companies/", {
    params: { page: args.page ?? 1, page_size: args.page_size ?? 100, name: args.name },
    ids: args.ids,
  });
}

/** One company by id, looked up through the list endpoint; throws when the id is unknown. */
export async function get_company(
  args: { company_id: string },
  ctx: Ctx,
): Promise<{ company: Company }> {
  const page = await request(ctx, "/companies/", { params: { page_size: 100 }, ids: [args.company_id] });
  const companies = (page.results ?? []) as Company[];
  if (!companies.length) throw new Error(`Standard Metrics: company not found: ${args.company_id}`);
  return { company: companies[0] };
}

/**
 * Metric data points for one company, given by `company_id` or `company_slug` (one is
 * required). `category` filters by metric category (revenue, arr, net_burn, runway,
 * cash_in_bank, gross_margin, ...); `from`/`to` are ISO 8601 dates (YYYY-MM-DD, inclusive);
 * `cadence` is `point_in_time`, `month`, `quarter` or `year`. Values are in the metric's
 * `currency`, with `converted_value` in the firm's preferred currency.
 */
export async function get_metrics(
  args: {
    company_id?: string;
    company_slug?: string;
    category?: string;
    from?: string;
    to?: string;
    cadence?: string;
    page?: number;
    page_size?: number;
  },
  ctx: Ctx,
): Promise<Paged<Metric>> {
  if (!args.company_id && !args.company_slug) {
    throw new Error("Standard Metrics: either company_id or company_slug is required.");
  }
  return await request(ctx, "/metrics/", {
    params: {
      page: args.page ?? 1,
      page_size: args.page_size ?? 100,
      company_id: args.company_id,
      company_slug: args.company_slug,
      category: args.category,
      from: args.from,
      to: args.to,
      cadence: args.cadence,
    },
  });
}

/**
 * Documents for a company, with name, source, parse state and upload time. `from`/`to` are
 * ISO 8601 dates bounding `uploaded_at`; `parse_state` is `not-started`, `in-progress`,
 * `completed` or `needs-review`; `source` is `information-request`, `implementation`,
 * `upload`, ...
 */
export async function get_documents(
  args: {
    company_id?: string;
    parse_state?: string;
    source?: string;
    from?: string;
    to?: string;
    page?: number;
    page_size?: number;
  },
  ctx: Ctx,
): Promise<Paged<Document>> {
  return await request(ctx, "/documents/", {
    params: {
      page: args.page ?? 1,
      page_size: args.page_size ?? 100,
      company_id: args.company_id,
      parse_state: args.parse_state,
      source: args.source,
      from: args.from,
      to: args.to,
    },
  });
}

/** Budget scenarios, optionally for one company, with name, color, description and date. */
export async function get_budgets(
  args: { company_id?: string; page?: number; page_size?: number },
  ctx: Ctx,
): Promise<Paged<Budget>> {
  return await request(ctx, "/budgets/", {
    params: { page: args.page ?? 1, page_size: args.page_size ?? 100, company_id: args.company_id },
  });
}

/** The firm's funds, with name, size, vintage year and currency. */
export async function get_funds(
  args: { page?: number; page_size?: number },
  ctx: Ctx,
): Promise<Paged<Fund>> {
  return await request(ctx, "/funds/", {
    params: { page: args.page ?? 1, page_size: args.page_size ?? 100 },
  });
}

/** Notes on a company. */
export async function get_notes(
  args: { company_id?: string; page?: number; page_size?: number },
  ctx: Ctx,
): Promise<Paged<Note>> {
  return await request(ctx, "/notes/", {
    params: { page: args.page ?? 1, page_size: args.page_size ?? 100, company_id: args.company_id },
  });
}

/**
 * Any other `v1` endpoint (`/whoami/`, `/firm-details/`, `/information-requests/`, ...) with
 * string query parameters; the JSON body is returned unchanged. Use the typed functions above
 * for the endpoints they cover.
 */
export async function raw_request(
  args: { endpoint: string; method?: string; params?: Record<string, string> },
  ctx: Ctx,
): Promise<{ result: unknown }> {
  return {
    result: await request(ctx, args.endpoint, { method: args.method || "GET", params: args.params }),
  };
}
