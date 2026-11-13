import type { Ctx, Response } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are the GA4 Data API (v1beta) and Admin API (v1beta) resources as
// they are sent: dimension and metric values are strings, in the order of
// `dimensionHeaders` and `metricHeaders`. The property id comes from
// `property_id` or the `GA_PROPERTY_ID` environment variable. Authentication is
// injected on the wire by iron-proxy's oauth_token transform;
// `GOOGLE_ACCESS_TOKEN` is a local-testing fallback only.

const DEFAULT_DATA_BASE = "https://analyticsdata.googleapis.com/v1beta";
const DEFAULT_ADMIN_BASE = "https://analyticsadmin.googleapis.com/v1beta";

/** One ordered column in a report; `desc` sorts high-to-low. */
export interface OrderBy {
  desc?: boolean;
  /** Sort by a dimension, such as `date` or `sessionSourceMedium`. */
  dimension?: { dimensionName?: string; orderType?: string };
  /** Sort by a metric, such as `sessions` or `screenPageViews`. */
  metric?: { metricName?: string };
  [field: string]: unknown;
}

/** A dimension column header. */
export interface DimensionHeader {
  name: string;
  [field: string]: unknown;
}

/** A metric column header (`type` is `TYPE_INTEGER`, `TYPE_FLOAT`, `TYPE_CURRENCY`, ...). */
export interface MetricHeader {
  name: string;
  type?: string;
  [field: string]: unknown;
}

/** A dimension cell; `value` is always a string. */
export interface DimensionValue {
  value?: string;
  [field: string]: unknown;
}

/** A metric cell; `value` is always a string (parse it before doing arithmetic). */
export interface MetricValue {
  value?: string;
  [field: string]: unknown;
}

/** One report row: cells aligned with the response's dimension and metric headers. */
export interface Row {
  dimensionValues?: DimensionValue[];
  metricValues?: MetricValue[];
  [field: string]: unknown;
}

/** A Data API report response (`kind: "analyticsData#runReport"`). */
export interface RunReportResponse {
  kind?: string;
  dimensionHeaders?: DimensionHeader[];
  metricHeaders?: MetricHeader[];
  rows?: Row[];
  totals?: Row[];
  maximums?: Row[];
  minimums?: Row[];
  rowCount?: number;
  metadata?: { currencyCode?: string; timeZone?: string; [field: string]: unknown };
  propertyQuota?: Record<string, unknown>;
  [field: string]: unknown;
}

/** A GA4 property summary from the Admin API. */
export interface PropertySummary {
  property: string;
  displayName?: string;
  propertyType?: string;
  parent?: string;
  [field: string]: unknown;
}

/** One Admin API account summary with its properties. */
export interface AccountSummary {
  name: string;
  account?: string;
  displayName?: string;
  propertySummaries?: PropertySummary[];
  [field: string]: unknown;
}

interface RequestOptions {
  method?: string;
  body?: unknown;
}

function dataBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_ANALYTICS_DATA_API_URL || DEFAULT_DATA_BASE;
}

function adminBase(ctx: Ctx): string {
  return ctx.env.GOOGLE_ANALYTICS_ADMIN_API_URL || DEFAULT_ADMIN_BASE;
}

/** iron-proxy adds the bearer token on the wire; locally the env may hold one. */
function authHeaders(ctx: Ctx): Record<string, string> {
  const headers: Record<string, string> = {};
  if (ctx.env.GOOGLE_ACCESS_TOKEN) headers.authorization = `Bearer ${ctx.env.GOOGLE_ACCESS_TOKEN}`;
  return headers;
}

async function send(ctx: Ctx, url: string, options: RequestOptions = {}): Promise<Response> {
  const headers: Record<string, string> = { accept: "application/json", ...authHeaders(ctx) };
  if (options.body !== undefined) headers["content-type"] = "application/json";
  return ctx.fetch(url, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
}

async function dataRequest(ctx: Ctx, path: string, options: RequestOptions = {}): Promise<any> {
  return readJson(await send(ctx, dataBase(ctx) + path, options), "Google Analytics Data API");
}

async function adminRequest(ctx: Ctx, path: string): Promise<any> {
  return readJson(
    await send(ctx, `${adminBase(ctx)}${path}`),
    "Google Analytics Admin API",
  );
}

/** A numeric property id, with a leading `properties/` stripped. */
function propertyId(args: { property_id?: string }, ctx: Ctx): string {
  const raw = args.property_id || ctx.env.GA_PROPERTY_ID;
  if (!raw) {
    throw new Error(
      "No GA4 property id: pass property_id, or set the GA_PROPERTY_ID environment variable " +
        "(Analytics Admin > Property settings). Use list_properties to find it.",
    );
  }
  const id = raw.startsWith("properties/") ? raw.slice("properties/".length) : raw;
  if (!/^[0-9]+$/.test(id)) throw new Error(`GA4 property id must be numeric, got ${raw}`);
  return id;
}

function metricOrder(metricName: string, desc: boolean): OrderBy[] {
  return [desc ? { desc: true, metric: { metricName } } : { metric: { metricName } }];
}

function dimensionOrder(dimensionName: string): OrderBy[] {
  return [{ dimension: { dimensionName } }];
}

/**
 * Run an arbitrary GA4 report over one property's data, for example dimensions
 * `["sessionSourceMedium"]` and metrics `["sessions","totalUsers"]`. Dates are `YYYY-MM-DD`,
 * `today`, `yesterday` or `NdaysAgo` (defaults `30daysAgo` to `today`). Rows are unsorted unless
 * `order_bys` says otherwise; `limit` (default 100) and `offset` page them, and dimension/metric
 * cell values are strings aligned with `dimensionHeaders`/`metricHeaders`.
 */
export async function run_report(
  args: {
    property_id?: string;
    dimensions: string[];
    metrics: string[];
    start_date?: string;
    end_date?: string;
    limit?: number;
    offset?: number;
    order_bys?: OrderBy[];
  },
  ctx: Ctx,
): Promise<RunReportResponse> {
  const id = propertyId(args, ctx);
  return dataRequest(ctx, `/properties/${encodeURIComponent(id)}:runReport`, {
    body: {
      dimensions: args.dimensions.map((name) => ({ name })),
      metrics: args.metrics.map((name) => ({ name })),
      dateRanges: [{ startDate: args.start_date ?? "30daysAgo", endDate: args.end_date ?? "today" }],
      limit: args.limit ?? 100,
      offset: args.offset,
      orderBys: args.order_bys,
    },
  });
}

/**
 * Run a realtime report over the last 30 minutes: dimensions and metrics as in `run_report`, with
 * `limit` (default 100) rows. Use it to check live traffic; the metrics available are the realtime
 * ones (`activeUsers`, `eventCount`, `screenPageViews`, ...).
 */
export async function run_realtime_report(
  args: {
    property_id?: string;
    dimensions: string[];
    metrics: string[];
    limit?: number;
    order_bys?: OrderBy[];
  },
  ctx: Ctx,
): Promise<RunReportResponse> {
  const id = propertyId(args, ctx);
  return dataRequest(ctx, `/properties/${encodeURIComponent(id)}:runRealtimeReport`, {
    body: {
      dimensions: args.dimensions.map((name) => ({ name })),
      metrics: args.metrics.map((name) => ({ name })),
      limit: args.limit ?? 100,
      orderBys: args.order_bys,
    },
  });
}

/**
 * The headline metrics for the property over a date range: `activeUsers`, `newUsers`, `sessions`,
 * `screenPageViews`, `bounceRate`, `averageSessionDuration` and `engagementRate`. The response has
 * one row whose `metricValues` follow that order.
 */
export async function get_summary(
  args: { property_id?: string; start_date?: string; end_date?: string },
  ctx: Ctx,
): Promise<RunReportResponse> {
  return run_report(
    {
      property_id: args.property_id,
      dimensions: [],
      metrics: [
        "activeUsers",
        "newUsers",
        "sessions",
        "screenPageViews",
        "bounceRate",
        "averageSessionDuration",
        "engagementRate",
      ],
      start_date: args.start_date,
      end_date: args.end_date,
      limit: 1,
    },
    ctx,
  );
}

/**
 * Sessions and users by source/medium (`sessionSourceMedium`, such as `google / organic`), most
 * sessions first, up to `limit` rows (default 20).
 */
export async function get_traffic_by_source(
  args: { property_id?: string; start_date?: string; end_date?: string; limit?: number },
  ctx: Ctx,
): Promise<RunReportResponse> {
  return run_report(
    {
      property_id: args.property_id,
      dimensions: ["sessionSourceMedium"],
      metrics: ["sessions", "activeUsers", "bounceRate", "averageSessionDuration"],
      start_date: args.start_date,
      end_date: args.end_date,
      limit: args.limit ?? 20,
      order_bys: metricOrder("sessions", true),
    },
    ctx,
  );
}

/**
 * Sessions and engaged sessions by default channel group (`sessionDefaultChannelGroup`, such as
 * `Organic Search`, `Direct`, `Paid Social`), most sessions first, up to `limit` rows (default 20).
 */
export async function get_traffic_by_channel(
  args: { property_id?: string; start_date?: string; end_date?: string; limit?: number },
  ctx: Ctx,
): Promise<RunReportResponse> {
  return run_report(
    {
      property_id: args.property_id,
      dimensions: ["sessionDefaultChannelGroup"],
      metrics: ["sessions", "activeUsers", "newUsers", "engagedSessions"],
      start_date: args.start_date,
      end_date: args.end_date,
      limit: args.limit ?? 20,
      order_bys: metricOrder("sessions", true),
    },
    ctx,
  );
}

/**
 * Page paths with views, users and average session duration, most viewed first, up to `limit` rows
 * (default 20).
 */
export async function get_top_pages(
  args: { property_id?: string; start_date?: string; end_date?: string; limit?: number },
  ctx: Ctx,
): Promise<RunReportResponse> {
  return run_report(
    {
      property_id: args.property_id,
      dimensions: ["pagePath"],
      metrics: ["screenPageViews", "activeUsers", "averageSessionDuration"],
      start_date: args.start_date,
      end_date: args.end_date,
      limit: args.limit ?? 20,
      order_bys: metricOrder("screenPageViews", true),
    },
    ctx,
  );
}

/**
 * Sessions, users and bounce rate by device category (`desktop`, `mobile`, `tablet`), most sessions
 * first.
 */
export async function get_traffic_by_device(
  args: { property_id?: string; start_date?: string; end_date?: string },
  ctx: Ctx,
): Promise<RunReportResponse> {
  return run_report(
    {
      property_id: args.property_id,
      dimensions: ["deviceCategory"],
      metrics: ["sessions", "activeUsers", "bounceRate"],
      start_date: args.start_date,
      end_date: args.end_date,
      limit: 10,
      order_bys: metricOrder("sessions", true),
    },
    ctx,
  );
}

/**
 * Sessions, users and new users by country, most sessions first, up to `limit` rows (default 20).
 */
export async function get_traffic_by_country(
  args: { property_id?: string; start_date?: string; end_date?: string; limit?: number },
  ctx: Ctx,
): Promise<RunReportResponse> {
  return run_report(
    {
      property_id: args.property_id,
      dimensions: ["country"],
      metrics: ["sessions", "activeUsers", "newUsers"],
      start_date: args.start_date,
      end_date: args.end_date,
      limit: args.limit ?? 20,
      order_bys: metricOrder("sessions", true),
    },
    ctx,
  );
}

/**
 * Active users, new users and sessions per day (`date` as `YYYYMMDD`), oldest first, for trend
 * lines over the date range.
 */
export async function get_daily_users(
  args: { property_id?: string; start_date?: string; end_date?: string },
  ctx: Ctx,
): Promise<RunReportResponse> {
  return run_report(
    {
      property_id: args.property_id,
      dimensions: ["date"],
      metrics: ["activeUsers", "newUsers", "sessions"],
      start_date: args.start_date,
      end_date: args.end_date,
      limit: 366,
      order_bys: dimensionOrder("date"),
    },
    ctx,
  );
}

/**
 * List every GA4 property the caller can see, grouped by account: account name/display name and,
 * per property, its resource name (`properties/123456789`) and display name. Use the numeric part as
 * `property_id`. Paginates through up to `max_results` accounts (default 200).
 */
export async function list_properties(
  args: { max_results?: number },
  ctx: Ctx,
): Promise<{ accountSummaries: AccountSummary[] }> {
  const max = args.max_results ?? 200;
  const accountSummaries: AccountSummary[] = [];
  let pageToken: string | undefined;
  do {
    const body = await adminRequest(
      ctx,
      `/accountSummaries${query({
        pageSize: Math.min(200, max - accountSummaries.length),
        pageToken,
      })}`,
    );
    for (const summary of body.accountSummaries ?? []) accountSummaries.push(summary);
    pageToken = body.nextPageToken;
  } while (pageToken && accountSummaries.length < max);
  return { accountSummaries: accountSummaries.slice(0, max) };
}

/**
 * Find the GA4 property id for a site by domain (such as `example.com`), using the Admin API: first
 * a property display name containing the domain, then each property's web data stream default URI.
 * Returns `property_id: null` when nothing matches (or when the caller cannot read the Admin API —
 * pass `property_id` explicitly then).
 */
export async function find_property(
  args: { site: string },
  ctx: Ctx,
): Promise<{ property_id: string | null; property?: string; display_name?: string }> {
  let domain = args.site.trim().toLowerCase();
  for (const prefix of ["http://", "https://"]) {
    if (domain.startsWith(prefix)) domain = domain.slice(prefix.length);
  }
  if (domain.startsWith("www.")) domain = domain.slice(4);
  domain = domain.replace(/\/+$/, "");
  if (!domain) throw new Error("site must not be empty");

  const { accountSummaries } = await list_properties({ max_results: 1000 }, ctx);
  const properties: PropertySummary[] = [];
  for (const summary of accountSummaries) {
    for (const property of summary.propertySummaries ?? []) properties.push(property);
  }
  for (const property of properties) {
    const displayName = (property.displayName ?? "").trim().toLowerCase();
    if (displayName && (displayName.includes(domain) || domain.includes(displayName))) {
      return {
        property_id: property.property.split("/").pop() ?? null,
        property: property.property,
        display_name: property.displayName,
      };
    }
  }
  for (const property of properties) {
    const id = property.property.split("/").pop() ?? "";
    let body: any;
    try {
      body = await adminRequest(ctx, `/${property.property}/dataStreams`);
    } catch {
      // Properties the caller cannot inspect are skipped, as in the Python tool.
      continue;
    }
    for (const stream of body.dataStreams ?? []) {
      const uri = (stream.webStreamData?.defaultUri ?? "").toLowerCase();
      if (uri && uri.includes(domain)) {
        return {
          property_id: id,
          property: property.property,
          display_name: property.displayName,
        };
      }
    }
  }
  return { property_id: null };
}
