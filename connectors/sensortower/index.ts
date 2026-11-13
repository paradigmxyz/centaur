import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are SensorTower's own objects, passed through. The interfaces list the
// fields worth knowing; SensorTower may send more.

const BASE = "https://api.sensortower.com";

/** An app platform understood by SensorTower; aliases are normalized to `ios` or `android`. */
export type Platform = "ios" | "android" | "itunes" | "apple" | "google" | "play";

/** A top-chart ranking type. */
export type ChartType = "free" | "paid" | "grossing";

/** Sales-report date granularity. */
export type DateGranularity = "daily" | "weekly" | "monthly";

/** One day, week or month of download/revenue estimates. `iu` is units and `ir` is revenue. */
export interface SalesEstimate {
  d?: string;
  iu?: number;
  ir?: number;
  [field: string]: unknown;
}

/** One app in a top-chart ranking. */
export interface ChartApp {
  app_id?: string;
  id?: string;
  name?: string;
  app_name?: string;
  publisher?: { name?: string; [field: string]: unknown };
  publisher_name?: string;
  rating?: number;
  average_rating?: number;
  [field: string]: unknown;
}

/** An app returned by search, lookup or a publisher listing. */
export interface App {
  app_id?: string;
  id?: string;
  name?: string;
  app_name?: string;
  publisher?: { name?: string; [field: string]: unknown };
  publisher_name?: string;
  rating?: number;
  average_rating?: number;
  [field: string]: unknown;
}

/** A unified publisher and its apps when the API includes them. */
export interface Publisher {
  name?: string;
  publisher_name?: string;
  apps?: App[];
  [field: string]: unknown;
}

/** The unified publisher-apps response, which may be a list or a publisher object. */
export type PublisherResponse = Publisher | App[];

function normalizePlatform(platform: Platform): string {
  const value = platform.toLowerCase();
  if (value === "ios" || value === "itunes" || value === "apple") return "ios";
  if (value === "android" || value === "google" || value === "play") return "android";
  return value;
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

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

async function request(ctx: Ctx, path: string, params: Params = {}): Promise<any> {
  const token = ctx.env.SENSOR_TOWER_AUTH_TOKEN || ctx.env.SENSORTOWER_AUTH_TOKEN;
  // Behind iron-proxy the token is injected as a query parameter on the wire; locally it comes
  // from the env.
  const withAuth = token ? { ...params, auth_token: token } : params;
  const res = await ctx.fetch(`${BASE}${path}${query(withAuth)}`, {
    headers: { accept: "application/json" },
  });
  return readJson(res, "SensorTower");
}

/**
 * Download and revenue estimates for one or more apps over a date range. `app_ids` are iOS
 * numeric ids or Android package names; `platform` is `ios` or `android`. Dates are `YYYY-MM-DD`
 * and `date_granularity` is `daily`, `weekly` or `monthly` (default `daily`). `countries` are
 * ISO country codes (for example `["US", "GB"]`); omit for worldwide. Each result has `d` (date),
 * `iu` (units) and `ir` (revenue).
 */
export async function get_sales_estimates(
  args: {
    app_ids: string[];
    platform: Platform;
    start_date: string;
    end_date: string;
    countries?: string[];
    date_granularity?: DateGranularity;
  },
  ctx: Ctx,
): Promise<{ estimates: SalesEstimate[] }> {
  const os = normalizePlatform(args.platform);
  const body = await request(ctx, `/v1/${os}/sales_report_estimates`, {
    app_ids: args.app_ids.join(","),
    date_granularity: args.date_granularity ?? "daily",
    start_date: args.start_date,
    end_date: args.end_date,
    countries: args.countries?.join(","),
  });
  return { estimates: list<SalesEstimate>(body, ["estimates", "data", "results"]) };
}

/**
 * Top-chart rankings for a platform and country. `category` is a SensorTower category id (for
 * example `6014` for iOS Games); omit for overall apps. `chart_type` is `free`, `paid` or
 * `grossing` (default `free`), `country` defaults to `US`, `limit` is capped at 400 (default 100),
 * and `date` is `YYYY-MM-DD` (default today).
 */
export async function get_top_charts(
  args: {
    platform: Platform;
    category?: string;
    country?: string;
    chart_type?: ChartType;
    limit?: number;
    date?: string;
  },
  ctx: Ctx,
): Promise<{ apps: ChartApp[] }> {
  const chartTypes: Record<ChartType, string> = {
    free: "topfreeapplications",
    paid: "toppaidapplications",
    grossing: "topgrossingapplications",
  };
  const os = normalizePlatform(args.platform);
  const requested = args.chart_type ?? "free";
  const chartType = chartTypes[requested] ?? requested;
  const body = await request(ctx, `/v1/${os}/ranking`, {
    country: args.country ?? "US",
    chart_type: chartType,
    category: args.category ?? "0",
    limit: Math.min(args.limit ?? 100, 400),
    date: args.date ?? today(),
  });
  return { apps: list<ChartApp>(body, ["apps", "data", "results"]) };
}

/**
 * Get a unified publisher (24-character hex id) and its apps. This uses SensorTower's
 * `/v1/unified/publishers/apps` endpoint; `get_publisher_apps` is the same call.
 */
export async function get_publisher(
  args: { publisher_id: string },
  ctx: Ctx,
): Promise<{ publisher: PublisherResponse }> {
  const body = await request(ctx, "/v1/unified/publishers/apps", {
    unified_id: args.publisher_id,
  });
  return { publisher: body };
}

/**
 * List the apps published by a unified publisher id (24-character hex). Uses SensorTower's
 * `/v1/unified/publishers/apps` endpoint.
 */
export async function get_publisher_apps(
  args: { publisher_id: string },
  ctx: Ctx,
): Promise<{ apps: App[] }> {
  const body = await request(ctx, "/v1/unified/publishers/apps", {
    unified_id: args.publisher_id,
  });
  return { apps: list<App>(body, ["apps", "data", "results"]) };
}

/**
 * Search apps by name or keywords on one platform. `query` is the search term, `platform` is
 * `ios` or `android` (default `ios`), and `limit` is capped at 100 (default 50). Returns matching
 * app objects.
 */
export async function search_apps(
  args: { query: string; platform?: Platform; limit?: number },
  ctx: Ctx,
): Promise<{ apps: App[] }> {
  const os = normalizePlatform(args.platform ?? "ios");
  const body = await request(ctx, `/v1/${os}/search_entities`, {
    term: args.query,
    limit: Math.min(args.limit ?? 50, 100),
    entity_type: "app",
  });
  return { apps: list<App>(body, ["apps", "data", "results"]) };
}

/**
 * Get metadata for one app. `app_id` is an iOS numeric id or an Android package name; `platform`
 * is `ios` or `android` (default `ios`). Returns the app object (the first entry of the API's
 * `apps` list).
 */
export async function get_app_info(
  args: { app_id: string; platform?: Platform },
  ctx: Ctx,
): Promise<{ app: App }> {
  const os = normalizePlatform(args.platform ?? "ios");
  const body = await request(ctx, `/v1/${os}/apps`, { app_ids: args.app_id });
  const apps = list<App>(body, ["apps"]);
  return { app: apps[0] ?? body };
}

/**
 * Call any SensorTower API endpoint with the connector's authentication. `endpoint` is a path
 * such as `/v1/ios/apps`, and `params` are query parameters. Use this only for endpoints the
 * typed functions do not cover.
 */
export async function raw(
  args: { endpoint: string; params?: Params },
  ctx: Ctx,
): Promise<{ data: unknown }> {
  return { data: await request(ctx, args.endpoint, args.params ?? {}) };
}
