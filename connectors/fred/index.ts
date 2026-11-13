import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Results are the FRED API's own response objects, passed through; field names are
// FRED's (https://fred.stlouisfed.org/docs/api/fred/). Dates are YYYY-MM-DD.

const DEFAULT_BASE = "https://api.stlouisfed.org/fred";

/** Envelope fields FRED puts on paged responses. */
export interface FredEnvelope {
  realtime_start?: string;
  realtime_end?: string;
  count?: number;
  offset?: number;
  limit?: number;
  order_by?: string;
  sort_order?: string;
  [field: string]: unknown;
}

/** One economic data series. */
export interface Series {
  id: string;
  realtime_start?: string;
  realtime_end?: string;
  title?: string;
  observation_start?: string;
  observation_end?: string;
  frequency?: string;
  frequency_short?: string;
  units?: string;
  units_short?: string;
  seasonal_adjustment?: string;
  seasonal_adjustment_short?: string;
  last_updated?: string;
  popularity?: number;
  notes?: string;
  [field: string]: unknown;
}

export interface Observation {
  realtime_start?: string;
  realtime_end?: string;
  /** YYYY-MM-DD. */
  date: string;
  /** Missing observations are "." upstream. */
  value: string;
  [field: string]: unknown;
}

export interface Category {
  id: number;
  name?: string;
  parent_id?: number;
  notes?: string;
  [field: string]: unknown;
}

export interface Release {
  id: number;
  name?: string;
  press_release?: boolean;
  link?: string;
  notes?: string;
  [field: string]: unknown;
}

export interface ReleaseDate {
  release_id?: number;
  release_name?: string;
  realtime_start?: string;
  realtime_end?: string;
  /** YYYY-MM-DD. */
  date: string;
  [field: string]: unknown;
}

export interface Source {
  id: number;
  name?: string;
  link?: string;
  notes?: string;
  [field: string]: unknown;
}

export interface Tag {
  name: string;
  group_id?: string;
  notes?: string;
  created?: string;
  popularity?: number;
  series_count?: number;
  [field: string]: unknown;
}

export interface SeriesPage extends FredEnvelope {
  seriess: Series[];
}

export interface SeriesMetadata extends FredEnvelope {
  seriess: Series[];
}

export interface ObservationPage extends FredEnvelope {
  observations: Observation[];
}

export interface CategoryPage extends FredEnvelope {
  categories: Category[];
}

export interface ReleasePage extends FredEnvelope {
  releases: Release[];
}

export interface ReleaseDatePage extends FredEnvelope {
  release_dates: ReleaseDate[];
}

export interface SourcePage extends FredEnvelope {
  sources: Source[];
}

export interface TagPage extends FredEnvelope {
  tags: Tag[];
}

// FRED API keys are 32-character lowercase alphanumeric strings; keep them out of messages.
const KEY_LIKE = /\b[a-z0-9]{32}\b/g;

async function request(ctx: Ctx, path: string, params: Params): Promise<any> {
  const url =
    (ctx.env.FRED_API_URL || DEFAULT_BASE) +
    path +
    // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
    query({ ...params, api_key: ctx.env.FRED_API_KEY, file_type: "json" });

  const res = await ctx.fetch(url, { headers: { accept: "application/json" } });
  const text = await res.text();
  if (!res.ok) {
    // FRED reports bad parameters (an unknown series, say) as a JSON error_message.
    let message = "";
    try {
      const body = JSON.parse(text);
      if (body && typeof body.error_message === "string") message = body.error_message;
    } catch {
      // Not JSON; fall back to the body.
    }
    const detail = (message || text).replace(KEY_LIKE, "[redacted]").slice(0, 300);
    throw new Error(`FRED API error: HTTP ${res.status}${detail ? `: ${detail}` : ""}`);
  }

  const data = JSON.parse(text);
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    throw new Error("FRED returned an unexpected response.");
  }
  return data;
}

/**
 * Search series by keywords. `search_type` is `full_text` (default) or `series_id` (which supports
 * `*` wildcards, for example `GDP*`); `tag_names` is a semicolon-separated filter such as
 * `usa;inflation`; `order_by` is search_rank, series_id, title, units, frequency, popularity,
 * last_updated and similar, with `sort_order` asc or desc. `limit` is at most 1000.
 */
export async function search_series(
  args: {
    text: string;
    search_type?: "full_text" | "series_id";
    limit?: number;
    offset?: number;
    order_by?: string;
    sort_order?: "asc" | "desc";
    tag_names?: string;
  },
  ctx: Ctx,
): Promise<SeriesPage> {
  return request(ctx, "/series/search", {
    search_text: args.text,
    search_type: args.search_type ?? "full_text",
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
    order_by: args.order_by,
    sort_order: args.sort_order,
    tag_names: args.tag_names,
  });
}

/** One series' metadata by ID (for example GDP, UNRATE, CPIAUCSL): title, units, frequency, last update. */
export async function get_series(args: { series_id: string }, ctx: Ctx): Promise<SeriesMetadata> {
  return request(ctx, "/series", { series_id: args.series_id });
}

/**
 * Observations of one series: `{ date, value }` pairs (`value` is a string, "." when missing).
 * `start`/`end` are YYYY-MM-DD. `units` transforms values (lin, chg, ch1, pch, pc1, pca, cch, cca,
 * log); `frequency` resamples to a lower frequency (d, w, m, q, sa, a, ...) using
 * `aggregation_method` avg, sum or eop. `sort_order` is asc (default) or desc—use desc with
 * `limit` to read the newest observations first. `limit` is at most 100000; omit it for all.
 */
export async function get_observations(
  args: {
    series_id: string;
    start?: string;
    end?: string;
    units?: string;
    frequency?: string;
    aggregation_method?: string;
    limit?: number;
    sort_order?: "asc" | "desc";
  },
  ctx: Ctx,
): Promise<ObservationPage> {
  return request(ctx, "/series/observations", {
    series_id: args.series_id,
    observation_start: args.start,
    observation_end: args.end,
    units: args.units,
    frequency: args.frequency,
    aggregation_method: args.aggregation_method,
    limit: args.limit,
    sort_order: args.sort_order ?? "asc",
  });
}

/** The categories a series belongs to. */
export async function get_series_categories(
  args: { series_id: string },
  ctx: Ctx,
): Promise<CategoryPage> {
  return request(ctx, "/series/categories", { series_id: args.series_id });
}

/** The release a series belongs to. */
export async function get_series_release(
  args: { series_id: string },
  ctx: Ctx,
): Promise<ReleasePage> {
  return request(ctx, "/series/release", { series_id: args.series_id });
}

/** The tags attached to a series, with popularity and series counts. */
export async function get_series_tags(args: { series_id: string }, ctx: Ctx): Promise<TagPage> {
  return request(ctx, "/series/tags", { series_id: args.series_id });
}

/** Series updated most recently, with FRED's rankings and the update envelope. */
export async function get_series_updates(
  args: { limit?: number; offset?: number },
  ctx: Ctx,
): Promise<SeriesPage> {
  return request(ctx, "/series/updates", { limit: args.limit ?? 20, offset: args.offset ?? 0 });
}

/** One category by ID: 0 is the root of the category tree. */
export async function get_category(
  args: { category_id?: number },
  ctx: Ctx,
): Promise<CategoryPage> {
  return request(ctx, "/category", { category_id: args.category_id ?? 0 });
}

/** Child categories of a category; 0 lists the root's children. */
export async function get_category_children(
  args: { category_id?: number },
  ctx: Ctx,
): Promise<CategoryPage> {
  return request(ctx, "/category/children", { category_id: args.category_id ?? 0 });
}

/** Series in a category. `limit` is at most 1000; page with `offset`. */
export async function get_category_series(
  args: { category_id: number; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<SeriesPage> {
  return request(ctx, "/category/series", {
    category_id: args.category_id,
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}

/** All releases of economic data, alphabetical. `limit` is at most 1000; page with `offset`. */
export async function list_releases(
  args: { limit?: number; offset?: number },
  ctx: Ctx,
): Promise<ReleasePage> {
  return request(ctx, "/releases", { limit: args.limit ?? 20, offset: args.offset ?? 0 });
}

/** One release by ID, with its press-release link and notes. */
export async function get_release(args: { release_id: number }, ctx: Ctx): Promise<ReleasePage> {
  return request(ctx, "/release", { release_id: args.release_id });
}

/** Series in a release. `limit` is at most 1000; page with `offset`. */
export async function get_release_series(
  args: { release_id: number; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<SeriesPage> {
  return request(ctx, "/release/series", {
    release_id: args.release_id,
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}

/**
 * Release dates—the economic data calendar. With `release_id`, that release's dates; without it,
 * all releases' dates. `start`/`end` are YYYY-MM-DD, `include_future` adds scheduled dates that
 * have no data yet, `sort_order` is asc or desc, `limit` is at most 1000.
 */
export async function get_release_dates(
  args: {
    release_id?: number;
    start?: string;
    end?: string;
    limit?: number;
    include_future?: boolean;
    sort_order?: "asc" | "desc";
  },
  ctx: Ctx,
): Promise<ReleaseDatePage> {
  const params: Params = {
    realtime_start: args.start,
    realtime_end: args.end,
    limit: args.limit ?? 20,
    sort_order: args.sort_order,
    include_release_dates_with_no_data: args.include_future ? "true" : undefined,
  };
  if (args.release_id === undefined) return request(ctx, "/releases/dates", params);
  return request(ctx, "/release/dates", { ...params, release_id: args.release_id });
}

/** Data sources such as the Bureau of Labor Statistics. `limit` is at most 1000; page with `offset`. */
export async function list_sources(
  args: { limit?: number; offset?: number },
  ctx: Ctx,
): Promise<SourcePage> {
  return request(ctx, "/sources", { limit: args.limit ?? 20, offset: args.offset ?? 0 });
}

/** Tags, optionally filtered by `search_text`. `limit` is at most 1000; page with `offset`. */
export async function list_tags(
  args: { search_text?: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<TagPage> {
  return request(ctx, "/tags", {
    search_text: args.search_text,
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}

/** Series matching all of the semicolon-separated tags, for example `usa;inflation`. */
export async function get_tag_series(
  args: { tag_names: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<SeriesPage> {
  return request(ctx, "/tags/series", {
    tag_names: args.tag_names,
    limit: args.limit ?? 20,
    offset: args.offset ?? 0,
  });
}
