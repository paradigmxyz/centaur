import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// Results are the newsapi.ai `getArticles` response, passed through: each article
// keeps its upstream field names (uri, body, source, authors, concepts, ...).
// Documented request parameters: https://newsapi.ai/documentation?tab=searchArticles

const DEFAULT_URL = "https://eventregistry.org/api/v1/article/getArticles";

/** A field that Event Registry accepts either once or as a list. */
export type StringOrList = string | string[];

/** One upstream news source, present when `includeSourceTitle` or similar is set. */
export interface Source {
  uri?: string;
  dataType?: string;
  title?: string;
  /** Source importance rank, 0 (lowest) to 1 (highest). */
  rank?: number;
  [field: string]: unknown;
}

export interface Author {
  uri?: string;
  name?: string;
  type?: string;
  isAgency?: string;
  [field: string]: unknown;
}

/** One article. Which fields are populated follows the `includeArticle*` request flags. */
export interface Article {
  uri?: string;
  /** ISO 639-3 language code, such as eng. */
  lang?: string;
  isDuplicated?: boolean;
  /** YYYY-MM-DD. */
  date?: string;
  /** HH:MM:SS in UTC. */
  time?: string;
  /** `date` and `time` as one value. */
  dateTime?: string;
  dateTimePub?: string;
  dataType?: string;
  /** Similarity to the query when ranked by `rel`. */
  sim?: number;
  url?: string;
  title?: string;
  body?: string;
  source?: Source;
  authors?: Author[];
  image?: string;
  eventUri?: string;
  sentiment?: number;
  wgt?: number;
  categories?: unknown[];
  concepts?: unknown[];
  location?: unknown;
  [field: string]: unknown;
}

/** The `articles` envelope: one page of results plus the match count. */
export interface ArticlesResult {
  results?: Article[];
  /** Total articles matching the query (up to the plan's cap). */
  totalResults?: number;
  page?: number;
  /** Articles in this page. */
  count?: number;
  pages?: number;
  [field: string]: unknown;
}

/** A documented `getArticles` request field; unset fields take the defaults on `get_articles`. */
export interface GetArticlesRequest {
  action?: "getArticles";
  resultType?: string;
  articlesPage?: number;
  articlesCount?: number;
  articlesSortBy?: string;
  articlesSortByAsc?: boolean;
  articleBodyLen?: number;
  dataType?: StringOrList;
  forceMaxDataTimeWindow?: number;
  query?: Record<string, unknown>;
  keyword?: StringOrList;
  conceptUri?: StringOrList;
  categoryUri?: StringOrList;
  sourceUri?: StringOrList;
  sourceLocationUri?: StringOrList;
  sourceGroupUri?: StringOrList;
  authorUri?: StringOrList;
  locationUri?: StringOrList;
  lang?: StringOrList;
  dateStart?: string;
  dateEnd?: string;
  dateMentionStart?: string;
  dateMentionEnd?: string;
  keywordLoc?: string;
  keywordOper?: string;
  keywordSearchMode?: string;
  conceptOper?: string;
  categoryOper?: string;
  ignoreKeyword?: StringOrList;
  ignoreConceptUri?: StringOrList;
  ignoreCategoryUri?: StringOrList;
  ignoreSourceUri?: StringOrList;
  ignoreSourceLocationUri?: StringOrList;
  ignoreSourceGroupUri?: StringOrList;
  ignoreLocationUri?: StringOrList;
  ignoreAuthorUri?: StringOrList;
  ignoreLang?: StringOrList;
  ignoreKeywordLoc?: string;
  startSourceRankPercentile?: number;
  endSourceRankPercentile?: number;
  minSentiment?: number;
  maxSentiment?: number;
  isDuplicateFilter?: string;
  eventFilter?: string;
  includeArticleTitle?: boolean;
  includeArticleBasicInfo?: boolean;
  includeArticleBody?: boolean;
  includeArticleEventUri?: boolean;
  includeArticleSocialScore?: boolean;
  includeArticleSentiment?: boolean;
  includeArticleConcepts?: boolean;
  includeArticleCategories?: boolean;
  includeArticleLocation?: boolean;
  includeArticleImage?: boolean;
  includeArticleAuthors?: boolean;
  includeArticleVideos?: boolean;
  includeArticleLinks?: boolean;
  includeArticleExtractedDates?: boolean;
  includeArticleDuplicateList?: boolean;
  includeArticleOriginalArticle?: boolean;
  includeSourceTitle?: boolean;
  includeSourceDescription?: boolean;
  includeSourceLocation?: boolean;
  includeSourceRanking?: boolean;
  includeConceptLabel?: boolean;
  includeConceptImage?: boolean;
  includeConceptSynonyms?: boolean;
  conceptLang?: string;
  includeCategoryParentUri?: boolean;
  includeLocationPopulation?: boolean;
  includeLocationGeoNamesId?: boolean;
  includeLocationCountryArea?: boolean;
  includeLocationCountryContinent?: boolean;
  includeLocationGeoLocation?: boolean;
}

// Every field the API documents. Requests are rejected if they name anything else, so
// a typo fails here instead of being silently ignored upstream.
const KNOWN_FIELDS = [
  "action", "resultType", "articlesPage", "articlesCount", "articlesSortBy",
  "articlesSortByAsc", "articleBodyLen", "dataType", "forceMaxDataTimeWindow", "query",
  "keyword", "conceptUri", "categoryUri", "sourceUri", "sourceLocationUri",
  "sourceGroupUri", "authorUri", "locationUri", "lang", "dateStart", "dateEnd",
  "dateMentionStart", "dateMentionEnd", "keywordLoc", "keywordOper", "keywordSearchMode",
  "conceptOper", "categoryOper", "ignoreKeyword", "ignoreConceptUri", "ignoreCategoryUri",
  "ignoreSourceUri", "ignoreSourceLocationUri", "ignoreSourceGroupUri", "ignoreLocationUri",
  "ignoreAuthorUri", "ignoreLang", "ignoreKeywordLoc", "startSourceRankPercentile",
  "endSourceRankPercentile", "minSentiment", "maxSentiment", "isDuplicateFilter",
  "eventFilter", "includeArticleTitle", "includeArticleBasicInfo", "includeArticleBody",
  "includeArticleEventUri", "includeArticleSocialScore", "includeArticleSentiment",
  "includeArticleConcepts", "includeArticleCategories", "includeArticleLocation",
  "includeArticleImage", "includeArticleAuthors", "includeArticleVideos",
  "includeArticleLinks", "includeArticleExtractedDates", "includeArticleDuplicateList",
  "includeArticleOriginalArticle", "includeSourceTitle", "includeSourceDescription",
  "includeSourceLocation", "includeSourceRanking", "includeConceptLabel",
  "includeConceptImage", "includeConceptSynonyms", "conceptLang",
  "includeCategoryParentUri", "includeLocationPopulation", "includeLocationGeoNamesId",
  "includeLocationCountryArea", "includeLocationCountryContinent",
  "includeLocationGeoLocation",
] as const;

// Compile-time check that KNOWN_FIELDS lists every request field.
type UnlistedField = Exclude<keyof GetArticlesRequest, (typeof KNOWN_FIELDS)[number]>;
const allFieldsListed: UnlistedField extends never ? true : never = true;

const DEFAULTS: GetArticlesRequest = {
  action: "getArticles",
  keywordSearchMode: "exact",
  keywordLoc: "body,title",
  lang: "eng",
  dataType: ["news"],
  startSourceRankPercentile: 0,
  endSourceRankPercentile: 50,
  isDuplicateFilter: "skipDuplicates",
  forceMaxDataTimeWindow: 31,
  articlesCount: 50,
  articlesSortBy: "rel",
  articlesSortByAsc: false,
  includeArticleBody: true,
  articleBodyLen: 5000,
  includeArticleAuthors: true,
  includeSourceTitle: true,
  includeSourceRanking: true,
  resultType: "articles",
};

/**
 * Search news articles (newsapi.ai `getArticles`). `keyword` matches words, `conceptUri`,
 * `categoryUri`, `sourceUri`, `authorUri` and `locationUri` match Event Registry URIs, and
 * `dateStart`/`dateEnd` are YYYY-MM-DD. Defaults unless overridden: English news only,
 * keywords matched exactly in body and title, source rank percentile 0-50, duplicates skipped,
 * a 31-day window, 50 articles per page ranked by relevance, article body up to 5000 characters
 * plus authors, source title and source rank. Page with `articlesPage` (1-based) or set
 * `articlesCount` (up to 1000); `resultType: "articleInfo"` returns the sequence of
 * `articlesPage`/`articlesCount` batches. Unknown request fields are rejected.
 */
export async function get_articles(
  args: GetArticlesRequest,
  ctx: Ctx,
): Promise<{ articles: ArticlesResult }> {
  const unknown = Object.keys(args).filter(
    (key) => !(KNOWN_FIELDS as readonly string[]).includes(key),
  );
  if (unknown.length) {
    throw new Error(`Unsupported getArticles fields: ${unknown.sort().join(", ")}`);
  }
  if (args.action !== undefined && args.action !== "getArticles") {
    throw new Error("action must be getArticles");
  }

  const body: GetArticlesRequest = { ...DEFAULTS, ...args, action: "getArticles" };
  const url =
    (ctx.env.EVENTREGISTRY_API_URL || DEFAULT_URL) +
    // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
    query({ apiKey: ctx.env.EVENTREGISTRY_API_KEY });

  const data = await readJson(
    await ctx.fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body),
    }),
    "Event Registry",
  );
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    throw new Error("Event Registry returned an unexpected response.");
  }
  // getArticles reports errors in a 200 body.
  if (data.error) throw new Error(`Event Registry error: ${data.error}`);
  return { articles: data.articles };
}
