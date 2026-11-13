import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are SimilarWeb's own objects, passed through. The interfaces list the
// fields worth knowing; SimilarWeb may send more.

const BASE = "https://api.similarweb.com";

/** Traffic-data granularity. */
export type Granularity = "daily" | "weekly" | "monthly";

/** Mobile app store. */
export type Store = "google" | "apple";

/** One period's visits. */
export interface VisitPoint {
  date: string;
  visits: number;
  [field: string]: unknown;
}

/** Total visits (desktop plus mobile) over time. */
export interface VisitsResponse {
  visits?: VisitPoint[];
  [field: string]: unknown;
}

/** A generic engagement metric response; the metric key varies by endpoint. */
export interface MetricResponse {
  [field: string]: unknown;
}

/** Visits, pages per visit, average visit duration (seconds) and bounce rate, as raw API responses. */
export interface TrafficOverview {
  domain: string;
  visits: VisitsResponse;
  pages_per_visit: MetricResponse;
  avg_visit_duration: MetricResponse;
  bounce_rate: MetricResponse;
}

/** Global rank response. */
export interface GlobalRankResponse {
  global_rank?: number;
  [field: string]: unknown;
}

/** Country rank response. */
export interface CountryRankResponse {
  country_rank?: number;
  [field: string]: unknown;
}

/** Industry/category rank response. */
export interface CategoryRankResponse {
  category?: string | { category?: string; rank?: number; [field: string]: unknown };
  category_rank?: number | { category?: string; rank?: number; [field: string]: unknown };
  rank?: number;
  [field: string]: unknown;
}

/** One country's share of a site's traffic. */
export interface GeoRecord {
  country?: string;
  country_name?: string;
  share?: number;
  traffic_share?: number;
  [field: string]: unknown;
}

/** Traffic by country. */
export interface GeographyResponse {
  records?: GeoRecord[];
  countries?: GeoRecord[];
  [field: string]: unknown;
}

/** Traffic-source channel shares. */
export interface TrafficSourcesResponse {
  overview?: Record<string, number>;
  [field: string]: unknown;
}

/** One referring site. */
export interface ReferralRecord {
  domain?: string;
  url?: string;
  share?: number;
  [field: string]: unknown;
}

/** Referring websites. */
export interface ReferralsResponse {
  records?: ReferralRecord[];
  [field: string]: unknown;
}

/** One similar/competitor site. */
export interface SimilarSite {
  url?: string;
  site?: string;
  score?: number;
  similarity?: number;
  [field: string]: unknown;
}

/** Similar/competitor sites. */
export interface SimilarSitesResponse {
  similar_sites?: SimilarSite[];
  sites?: SimilarSite[];
  [field: string]: unknown;
}

/** Website description/metadata. */
export interface DescriptionResponse {
  description?: string;
  [field: string]: unknown;
}

/** The top-sites category index: a nested mapping of category names to category ids. */
export interface TopSitesResponse {
  [field: string]: unknown;
}

/** One keyword and its share of traffic. */
export interface KeywordRecord {
  keyword?: string;
  search_term?: string;
  share?: number;
  traffic_share?: number;
  [field: string]: unknown;
}

/** Organic and paid search keywords. */
export interface KeywordsResponse {
  organic?: KeywordRecord[];
  paid?: KeywordRecord[];
  keywords?: KeywordRecord[];
  search_keywords?: KeywordRecord[];
  [field: string]: unknown;
}

/** Mobile app metadata. */
export interface AppRecord {
  title?: string;
  name?: string;
  app_id?: string;
  publisher?: string | { name?: string; [field: string]: unknown };
  developer?: string;
  category?: string;
  rating?: number;
  [field: string]: unknown;
}

/** One period's app downloads. */
export interface DownloadPoint {
  date?: string;
  downloads?: number;
  [field: string]: unknown;
}

/** App download estimates. */
export interface AppDownloadsResponse {
  downloads?: DownloadPoint[];
  [field: string]: unknown;
}

/** App store ranking. */
export interface AppRankResponse {
  rank?: number;
  [field: string]: unknown;
}

/** App search results. */
export interface AppsSearchResponse {
  apps: AppRecord[];
  [field: string]: unknown;
}

/** The `/v1/TopSites/categories` index: a nested mapping of category names to category ids. */
export interface CategoriesResponse {
  [field: string]: unknown;
}

function cleanSecret(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  if (!trimmed.includes("\n")) return trimmed;
  for (const line of trimmed.split("\n")) {
    const candidate = line.trim();
    if (candidate && !candidate.startsWith("===") && !candidate.startsWith("#")) {
      return candidate;
    }
  }
  return undefined;
}

function list<T>(body: unknown, keys: string[]): T[] {
  if (Array.isArray(body)) return body as T[];
  if (body && typeof body === "object") {
    for (const key of keys) {
      const value = (body as Record<string, unknown>)[key];
      if (Array.isArray(value)) return value as T[];
    }
  }
  return [];
}

function month(value: string): string {
  return value.length > 7 ? value.slice(0, 7) : value;
}

function monthString(value: Date): string {
  return `${value.getUTCFullYear()}-${String(value.getUTCMonth() + 1).padStart(2, "0")}`;
}

/** The Python CLI's default range: the three full months ending last month. */
function defaultDates(): { start: string; end: string } {
  const now = new Date();
  const end = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 0));
  const start = new Date(end.getTime() - 90 * 24 * 60 * 60 * 1000);
  return { start: monthString(start), end: monthString(end) };
}

/** Six monthly periods ending two months before today, as used by the app-downloads endpoint. */
function defaultAppDownloadWindow(): { start: string; end: string } {
  const now = new Date();
  const index = now.getUTCFullYear() * 12 + now.getUTCMonth() - 2;
  return { start: monthByIndex(index - 5), end: monthByIndex(index) };
}

function monthByIndex(index: number): string {
  const year = Math.floor(index / 12);
  const monthNumber = (index % 12) + 1;
  return `${year}-${String(monthNumber).padStart(2, "0")}`;
}

function window(
  args: { start_date?: string; end_date?: string },
): { start_date: string; end_date: string } {
  const fallback = defaultDates();
  return {
    start_date: month(args.start_date ?? fallback.start),
    end_date: month(args.end_date ?? fallback.end),
  };
}

async function request(ctx: Ctx, path: string, params: Params = {}): Promise<any> {
  const key = cleanSecret(ctx.env.SIMILARWEB_API_KEY);
  // Behind iron-proxy the key is injected as a query parameter on the wire; locally it comes
  // from the env.
  const withAuth = key ? { ...params, api_key: key } : params;
  const res = await ctx.fetch(`${BASE}${path}${query(withAuth)}`, {
    headers: { accept: "application/json" },
  });
  const text = await res.text();
  if (!res.ok) {
    let message = text;
    try {
      const body = JSON.parse(text);
      if (body && typeof body === "object") {
        for (const field of ["error_message", "message", "error", "detail"]) {
          const value = (body as Record<string, unknown>)[field];
          if (typeof value === "string" && value) {
            message = value;
            break;
          }
        }
      }
    } catch {
      // Keep the raw body when it is not JSON.
    }
    throw new Error(`SimilarWeb returned HTTP ${res.status}: ${message.slice(0, 1000)}`);
  }
  return JSON.parse(text);
}

function engagementParams(
  args: {
    start_date?: string;
    end_date?: string;
    country?: string;
    granularity?: Granularity;
    main_domain_only?: boolean;
  },
): Params {
  const dates = window(args);
  return {
    start_date: dates.start_date,
    end_date: dates.end_date,
    country: args.country ?? "world",
    granularity: args.granularity ?? "monthly",
    main_domain_only: String(args.main_domain_only ?? true).toLowerCase(),
  };
}

/**
 * Total visits (desktop plus mobile) over time. `domain` is a bare domain such as `google.com`.
 * Dates are `YYYY-MM` or `YYYY-MM-DD` and default to the three full months ending last month.
 * `country` is a country code or `world` (default `world`); `granularity` is `daily`, `weekly` or
 * `monthly` (default `monthly`); `main_domain_only` excludes subdomains (default true).
 */
export async function get_visits(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    granularity?: Granularity;
    main_domain_only?: boolean;
  },
  ctx: Ctx,
): Promise<VisitsResponse> {
  return request(
    ctx,
    `/v1/website/${args.domain}/total-traffic-and-engagement/visits`,
    engagementParams(args),
  );
}

/**
 * Visits plus pages per visit, average visit duration (seconds) and bounce rate for a domain. All
 * four raw metric responses are returned under `visits`, `pages_per_visit`, `avg_visit_duration`
 * and `bounce_rate`; arguments and defaults are the same as `get_visits`.
 */
export async function get_traffic_overview(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    granularity?: Granularity;
    main_domain_only?: boolean;
  },
  ctx: Ctx,
): Promise<TrafficOverview> {
  const params = engagementParams(args);
  const [visits, pages, duration, bounce] = await Promise.all([
    request(ctx, `/v1/website/${args.domain}/total-traffic-and-engagement/visits`, params),
    request(ctx, `/v1/website/${args.domain}/total-traffic-and-engagement/pages-per-visit`, params),
    request(
      ctx,
      `/v1/website/${args.domain}/total-traffic-and-engagement/average-visit-duration`,
      params,
    ),
    request(ctx, `/v1/website/${args.domain}/total-traffic-and-engagement/bounce-rate`, params),
  ]);
  return {
    domain: args.domain,
    visits,
    pages_per_visit: pages,
    avg_visit_duration: duration,
    bounce_rate: bounce,
  };
}

/**
 * Pages per visit over time. Arguments and defaults are the same as `get_visits`.
 */
export async function get_pages_per_visit(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    granularity?: Granularity;
    main_domain_only?: boolean;
  },
  ctx: Ctx,
): Promise<MetricResponse> {
  return request(
    ctx,
    `/v1/website/${args.domain}/total-traffic-and-engagement/pages-per-visit`,
    engagementParams(args),
  );
}

/**
 * Average visit duration in seconds over time. Arguments and defaults are the same as
 * `get_visits`.
 */
export async function get_avg_visit_duration(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    granularity?: Granularity;
    main_domain_only?: boolean;
  },
  ctx: Ctx,
): Promise<MetricResponse> {
  return request(
    ctx,
    `/v1/website/${args.domain}/total-traffic-and-engagement/average-visit-duration`,
    engagementParams(args),
  );
}

/**
 * Bounce rate over time. Arguments and defaults are the same as `get_visits`.
 */
export async function get_bounce_rate(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    granularity?: Granularity;
    main_domain_only?: boolean;
  },
  ctx: Ctx,
): Promise<MetricResponse> {
  return request(
    ctx,
    `/v1/website/${args.domain}/total-traffic-and-engagement/bounce-rate`,
    engagementParams(args),
  );
}

/** Get a domain's global SimilarWeb rank. */
export async function get_global_rank(
  args: { domain: string },
  ctx: Ctx,
): Promise<GlobalRankResponse> {
  return request(ctx, `/v1/website/${args.domain}/global-rank/global-rank`);
}

/** Get a domain's rank in one country; `country` is a lowercase country code (default `us`). */
export async function get_country_rank(
  args: { domain: string; country?: string },
  ctx: Ctx,
): Promise<CountryRankResponse> {
  return request(ctx, `/v1/website/${args.domain}/country-rank/country-rank`, {
    country: args.country ?? "us",
  });
}

/**
 * Get a domain's industry/category rank; `country` is a country code or `world` (default
 * `world`). The response includes the category and rank.
 */
export async function get_industry_rank(
  args: { domain: string; country?: string },
  ctx: Ctx,
): Promise<CategoryRankResponse> {
  return request(ctx, `/v1/website/${args.domain}/category-rank/category-rank`, {
    country: args.country ?? "world",
  });
}

/**
 * Traffic distribution by country. Dates default to the three full months ending last month;
 * `main_domain_only` excludes subdomains (default true). Returns country rows with their share.
 */
export async function get_geography(
  args: { domain: string; start_date?: string; end_date?: string; main_domain_only?: boolean },
  ctx: Ctx,
): Promise<GeographyResponse> {
  const dates = window(args);
  return request(ctx, `/v1/website/${args.domain}/geo/traffic-by-country`, {
    start_date: dates.start_date,
    end_date: dates.end_date,
    main_domain_only: String(args.main_domain_only ?? true).toLowerCase(),
  });
}

/**
 * Traffic sources broken down by marketing channel (organic search, paid search, direct,
 * referrals, social, mail, display ads). Dates default to the three full months ending last month;
 * `country` defaults to `world`; `main_domain_only` defaults to true.
 */
export async function get_traffic_sources(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    main_domain_only?: boolean;
  },
  ctx: Ctx,
): Promise<TrafficSourcesResponse> {
  const dates = window(args);
  return request(ctx, `/v1/website/${args.domain}/traffic-sources/overview`, {
    start_date: dates.start_date,
    end_date: dates.end_date,
    country: args.country ?? "world",
    main_domain_only: String(args.main_domain_only ?? true).toLowerCase(),
  });
}

/**
 * Referring websites and their share of traffic. Dates default to the three full months ending
 * last month; `country` defaults to `world`; `main_domain_only` defaults to true.
 */
export async function get_referrals(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    main_domain_only?: boolean;
  },
  ctx: Ctx,
): Promise<ReferralsResponse> {
  const dates = window(args);
  return request(ctx, `/v1/website/${args.domain}/traffic-sources/referrals`, {
    start_date: dates.start_date,
    end_date: dates.end_date,
    country: args.country ?? "world",
    main_domain_only: String(args.main_domain_only ?? true).toLowerCase(),
  });
}

/** Get similar/competitor websites for a domain, with SimilarWeb's similarity score. */
export async function get_similar_sites(
  args: { domain: string },
  ctx: Ctx,
): Promise<SimilarSitesResponse> {
  return request(ctx, `/v1/website/${args.domain}/similar-sites/similarsites`);
}

/** Get a website's description and general metadata. */
export async function get_website_description(
  args: { domain: string },
  ctx: Ctx,
): Promise<DescriptionResponse> {
  return request(ctx, `/v1/website/${args.domain}/general-data/description`);
}

/**
 * Return SimilarWeb's top-sites category index (`/v1/TopSites/categories`), a nested mapping of
 * category names to category ids. `category` and `country` are passed through as filters; the
 * index is the same public response that `get_categories` returns.
 */
export async function get_top_sites(
  args: { category?: string; country?: string },
  ctx: Ctx,
): Promise<TopSitesResponse> {
  return request(ctx, "/v1/TopSites/categories", {
    country: args.country ?? "world",
    category: args.category,
  });
}

/**
 * Organic and paid search keywords for a website. Dates default to the three full months ending
 * last month; `country` defaults to `world`; `limit` caps the number of keywords (default 100).
 */
export async function get_keywords(
  args: {
    domain: string;
    start_date?: string;
    end_date?: string;
    country?: string;
    limit?: number;
  },
  ctx: Ctx,
): Promise<KeywordsResponse> {
  const dates = window(args);
  return request(ctx, `/v1/website/${args.domain}/search-keywords/keywords`, {
    start_date: dates.start_date,
    end_date: dates.end_date,
    country: args.country ?? "world",
    limit: args.limit ?? 100,
  });
}

/**
 * Get mobile app metadata. `app_id` is a package name for Android or a numeric id for iOS;
 * `store` is `google` or `apple` (default `google`).
 */
export async function get_app_details(
  args: { app_id: string; store?: Store },
  ctx: Ctx,
): Promise<AppRecord> {
  return request(ctx, `/v1/app/${args.store ?? "google"}/${args.app_id}/details`);
}

/**
 * App download estimates over monthly periods. `app_id` is a package name for Android or a
 * numeric id for iOS; `store` is `google` or `apple` (default `google`). If `start_date` and
 * `end_date` are omitted the default is six monthly periods ending two months before today.
 * `country` defaults to `world` and `granularity` to `monthly`.
 */
export async function get_app_downloads(
  args: {
    app_id: string;
    store?: Store;
    start_date?: string;
    end_date?: string;
    country?: string;
    granularity?: Granularity;
  },
  ctx: Ctx,
): Promise<AppDownloadsResponse> {
  const fallback = defaultAppDownloadWindow();
  return request(ctx, `/v5/apps/${args.store ?? "google"}/downloads`, {
    app_id: args.app_id,
    country: args.country ?? "world",
    granularity: args.granularity ?? "monthly",
    start_date: month(args.start_date ?? fallback.start),
    end_date: month(args.end_date ?? fallback.end),
  });
}

/**
 * Get an app's store ranking. `app_id` is a package name for Android or a numeric id for iOS;
 * `store` is `google` or `apple` (default `google`); `country` defaults to `us`.
 */
export async function get_app_rank(
  args: { app_id: string; store?: Store; country?: string },
  ctx: Ctx,
): Promise<AppRankResponse> {
  return request(ctx, `/v1/app/${args.store ?? "google"}/${args.app_id}/rank`, {
    country: args.country ?? "us",
  });
}

/**
 * Search mobile apps by term in one store. `store` is `google` or `apple` (default `google`).
 */
export async function search_apps(
  args: { query: string; store?: Store },
  ctx: Ctx,
): Promise<AppsSearchResponse> {
  const body = await request(ctx, `/v1/app/${args.store ?? "google"}/search`, {
    term: args.query,
  });
  return { apps: list<AppRecord>(body, ["apps", "data", "results"]) };
}

/** Return SimilarWeb's industry category index (nested category names to ids). */
export async function get_categories(
  _args: Record<string, never>,
  ctx: Ctx,
): Promise<CategoriesResponse> {
  return request(ctx, "/v1/TopSites/categories");
}

/**
 * Call any SimilarWeb API endpoint with the connector's authentication. `endpoint` is a path such
 * as `/v1/website/google.com/global-rank/global-rank`, and `params` are query parameters. Use this
 * only for endpoints the typed functions do not cover.
 */
export async function raw(
  args: { endpoint: string; params?: Params },
  ctx: Ctx,
): Promise<{ data: unknown }> {
  return { data: await request(ctx, args.endpoint, args.params ?? {}) };
}
