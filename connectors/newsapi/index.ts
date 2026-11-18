import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Responses are NewsAPI's own bodies: `status`, `totalResults`/`articles` or
// `sources`. Interfaces name the fields worth knowing; NewsAPI sends more.

const DEFAULT_BASE = "https://newsapi.org/v2";

/** A news article. `content` is truncated by NewsAPI; fetch `url` for the full text. */
export interface Article {
  source: { id?: string | null; name?: string | null; [field: string]: unknown };
  author?: string | null;
  title?: string | null;
  description?: string | null;
  url?: string | null;
  urlToImage?: string | null;
  publishedAt?: string | null;
  content?: string | null;
  [field: string]: unknown;
}

/** An article list response. */
export interface NewsResponse {
  status: string;
  totalResults?: number;
  articles?: Article[];
  [field: string]: unknown;
}

/** A news source. */
export interface Source {
  id?: string | null;
  name?: string | null;
  description?: string | null;
  url?: string | null;
  category?: string | null;
  language?: string | null;
  country?: string | null;
  [field: string]: unknown;
}

/** A source list response. */
export interface SourcesResponse {
  status: string;
  sources?: Source[];
  [field: string]: unknown;
}

async function request(ctx: Ctx, path: string, params: Params): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.NEWSAPI_KEY) headers["X-Api-Key"] = ctx.env.NEWSAPI_KEY;
  const res = await ctx.fetch((ctx.env.NEWSAPI_API_URL || DEFAULT_BASE) + path + query(params), { headers });
  const body = await readJson(res, "NewsAPI");
  // NewsAPI can answer 200 with an error status.
  if (body && body.status === "error") {
    throw new Error(`NewsAPI error: ${body.message ?? body.code ?? "unknown error"}`);
  }
  return body;
}

/**
 * Top headlines. `country` is a two-letter code (default us); `category` is business, entertainment,
 * general, health, science, sports or technology; `sources` is a comma-separated list of source IDs.
 * NewsAPI does not allow `sources` together with `country` or `category`, so pass either `sources` or
 * the country/category pair. `query` filters by keywords. `page_size` is 1-100 (default 20); page
 * through with `page` (1-based).
 */
export async function headlines(
  args: {
    country?: string;
    category?: string;
    sources?: string;
    query?: string;
    page_size?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<NewsResponse> {
  const params: Params = { pageSize: args.page_size ?? 20, page: args.page ?? 1 };
  if (args.country) params.country = args.country;
  if (args.category) params.category = args.category;
  if (args.sources) params.sources = args.sources;
  if (args.query) params.q = args.query;
  return request(ctx, "/top-headlines", params);
}

/**
 * Search every article NewsAPI indexes. `query` supports AND/OR/NOT, quoted phrases and
 * `+`/`-` prefixes. `sources` and `domains` are comma-separated; `from`/`to` are ISO 8601 dates or
 * timestamps (for example 2024-01-01). `language` is a two-letter code (en, de, fr, ...) and
 * `sort_by` is relevancy, popularity or publishedAt. `page_size` is 1-100 (default 20); page through
 * with `page`. Read `totalResults` and `articles`.
 */
export async function search(
  args: {
    query: string;
    sources?: string;
    domains?: string;
    from?: string;
    to?: string;
    language?: string;
    sort_by?: string;
    page_size?: number;
    page?: number;
  },
  ctx: Ctx,
): Promise<NewsResponse> {
  const params: Params = {
    q: args.query,
    sortBy: args.sort_by ?? "publishedAt",
    pageSize: args.page_size ?? 20,
    page: args.page ?? 1,
  };
  if (args.sources) params.sources = args.sources;
  if (args.domains) params.domains = args.domains;
  if (args.from) params.from = args.from;
  if (args.to) params.to = args.to;
  if (args.language) params.language = args.language;
  return request(ctx, "/everything", params);
}

/**
 * Available news sources (id, name, description, url, category, language, country), optionally
 * filtered by `category`, two-letter `language` and two-letter `country`. Use a source `id` in
 * `headlines` or `search`.
 */
export async function sources(
  args: { category?: string; language?: string; country?: string },
  ctx: Ctx,
): Promise<SourcesResponse> {
  const params: Params = {};
  if (args.category) params.category = args.category;
  if (args.language) params.language = args.language;
  if (args.country) params.country = args.country;
  return request(ctx, "/top-headlines/sources", params);
}
