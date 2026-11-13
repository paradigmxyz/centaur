import type { Ctx, Response } from "../centaur";
import { query, readJson } from "../_lib/http";

// Synoptic's `/twttr-api/*` objects are passed through as the API sends them.
// The interfaces list fields worth knowing; Synoptic sends more.

const DEFAULT_BASE = "https://api.synoptic.com";
const FXTWITTER_BASE = "https://api.fxtwitter.com";

const ARTICLE_URL_RE = /x\.com\/i\/article\/(\d+)/;
const TWEET_URL_RE = /(?:x|twitter)\.com\/([^/]+)\/status\/(\d+)/;

/** A Synoptic user object. */
export interface User {
  user_id?: string;
  screen_name?: string;
  name?: string;
  description?: string;
  followers_count?: number;
  following_count?: number;
  statuses_count?: number;
  [field: string]: unknown;
}

/** A Synoptic tweet object. */
export interface Tweet {
  tweet_id?: string;
  screen_name?: string;
  text?: string;
  published_at?: number;
  like_count?: number;
  retweet_count?: number;
  reply_count?: number;
  view_count?: number;
  urls?: string[];
  [field: string]: unknown;
}

/** Per-response unit accounting; every list/search call returns it as `meta`. */
export interface UsageMeta {
  consumed_units?: number;
  remaining_units?: number;
  returned_items?: number;
  price_per_item?: number;
  [field: string]: unknown;
}

/** One Draft.js-style block in a long-form article. */
export interface ArticleBlock {
  text?: string;
  type?: string;
  [field: string]: unknown;
}

/** An X long-form article as fxtwitter reports it. */
export interface Article {
  title?: string;
  preview_text?: string;
  content?: { blocks?: ArticleBlock[]; [field: string]: unknown };
  [field: string]: unknown;
}

/** A long-form article with the tweet it belongs to. */
export interface ArticleResult {
  article: Article;
  tweet_url: string;
  author: string;
}

/** A followers/following result: IDs always, full users unless `ids_only` was set. */
export interface FollowersResult {
  ids: string[];
  users: User[];
  next_cursor: string | null;
  meta: UsageMeta;
}

interface FollowArgs {
  handle: string;
  limit?: number;
  ids_only?: boolean;
  cursor?: string;
  max_results?: number;
}

function normalizeHandle(handle: string): string {
  return handle.trim().replace(/^@+/, "");
}

function settings(ctx: Ctx): { base: string; key: string } {
  const key = ctx.env.SYNOPTIC_API_KEY;
  if (!key) throw new Error("SYNOPTIC_API_KEY is not set");
  let base = ctx.env.SYNOPTIC_BASE_URL || DEFAULT_BASE;
  if (!/^https?:\/\//.test(base)) base = `https://${base}`;
  return { base: base.replace(/\/+$/, ""), key };
}

function retryDelay(res: Response, attempt: number): number {
  const header = Number(res.headers["retry-after"]);
  if (Number.isFinite(header) && header > 0) return Math.min(header * 1000, 10000);
  return Math.min(attempt * 1000, 10000);
}

async function get(
  ctx: Ctx,
  endpoint: string,
  params: Record<string, string | number | undefined> = {},
): Promise<any> {
  const { base, key } = settings(ctx);
  for (let attempt = 1; ; attempt++) {
    const res = await ctx.fetch(`${base}${endpoint}${query(params)}`, {
      headers: { accept: "application/json", "x-api-key": key },
    });
    if ((res.status === 429 || res.status >= 500) && attempt < 3) {
      await ctx.sleep(retryDelay(res, attempt));
      continue;
    }
    return readJson(res, "Synoptic");
  }
}

function usage(data: any): UsageMeta {
  return {
    consumed_units: data.consumed_units,
    remaining_units: data.remaining_units,
    returned_items: data.returned_items,
    price_per_item: data.price_per_item,
  };
}

async function followPage(
  ctx: Ctx,
  endpoint: string,
  key: string,
  handle: string,
  cursor: string | undefined,
  maxResults: number,
): Promise<{ ids: string[]; next_cursor: string | null; meta: UsageMeta }> {
  const data = await get(ctx, endpoint, {
    screen_name: normalizeHandle(handle),
    max_results: maxResults,
    cursor,
  });
  const page = data.data || {};
  const ids = (page[key] || []) as unknown[];
  const raw = page.next_cursor;
  const next =
    raw === undefined || raw === null || raw === 0 || raw === "0" || raw === "" ? null : String(raw);
  return { ids: ids.map((id) => String(id)), next_cursor: next, meta: usage(data) };
}

async function lookupByIds(ctx: Ctx, ids: string[]): Promise<User[]> {
  const users: User[] = [];
  for (let start = 0; start < ids.length; start += 50) {
    const data = await get(ctx, "/twttr-api/users/lookup", {
      user_ids: ids.slice(start, start + 50).join(","),
    });
    users.push(...((data.data || []) as User[]));
  }
  return users;
}

async function followList(
  ctx: Ctx,
  endpoint: string,
  key: string,
  args: FollowArgs,
): Promise<FollowersResult> {
  const limit = args.limit ?? 100;
  const ids: string[] = [];
  let cursor = args.cursor;
  let meta: UsageMeta = {};
  while (ids.length < limit) {
    const batch = Math.max(1, Math.min(args.max_results ?? 1000, limit - ids.length, 1000));
    const page = await followPage(ctx, endpoint, key, args.handle, cursor, batch);
    meta = page.meta;
    ids.push(...page.ids);
    cursor = page.next_cursor ?? undefined;
    if (!cursor || page.ids.length === 0) break;
  }
  const limited = ids.slice(0, limit);
  return {
    ids: limited,
    users: args.ids_only ? [] : await lookupByIds(ctx, limited),
    next_cursor: cursor ?? null,
    meta,
  };
}

async function fetchArticle(ctx: Ctx, screenName: string, tweetId: string): Promise<Article | null> {
  const res = await ctx.fetch(
    `${FXTWITTER_BASE}/${encodeURIComponent(screenName)}/status/${encodeURIComponent(tweetId)}`,
    { headers: { accept: "application/json" } },
  );
  if (!res.ok) return null;
  const text = await res.text();
  if (!text) return null;
  let body: any;
  try {
    body = JSON.parse(text);
  } catch {
    return null;
  }
  const article = body.tweet?.article;
  return article ? (article as Article) : null;
}

async function expandArticle(ctx: Ctx, tweet: Tweet): Promise<Tweet> {
  const urls = (tweet.urls || []) as unknown[];
  if (!urls.some((url) => typeof url === "string" && ARTICLE_URL_RE.test(url))) return tweet;
  const screenName = tweet.screen_name;
  const tweetId = tweet.tweet_id;
  if (!screenName || !tweetId) return tweet;
  const article = await fetchArticle(ctx, String(screenName), String(tweetId));
  return article ? { ...tweet, article } : tweet;
}

async function expandArticles(ctx: Ctx, tweets: Tweet[]): Promise<Tweet[]> {
  const expanded: Tweet[] = [];
  for (const tweet of tweets) expanded.push(await expandArticle(ctx, tweet));
  return expanded;
}

/**
 * One user profile by handle (`@` optional): `user_id`, `screen_name`, `name`, `description`,
 * `followers_count`, `following_count`, `statuses_count`, `created_at` and more. `user` is null
 * when Synoptic knows no such account.
 */
export async function get_user(args: { handle: string }, ctx: Ctx): Promise<{ user: User | null }> {
  const data = await get(ctx, "/twttr-api/users/lookup", {
    screen_name: normalizeHandle(args.handle),
  });
  const users = (data.data || []) as User[];
  return { user: users.length > 0 ? users[0] : null };
}

/**
 * Accounts following `handle`, in ID order. Fetches follower pages (up to 1000 IDs each) until
 * `limit` IDs (default 100), then resolves full user objects in batches of 50 unless `ids_only`.
 * `next_cursor` continues after the last fetched page; `meta` is from that page. Large limits cost
 * units for the extra lookups.
 */
export async function get_followers(
  args: { handle: string; limit?: number; ids_only?: boolean; cursor?: string; max_results?: number },
  ctx: Ctx,
): Promise<FollowersResult> {
  return followList(ctx, "/twttr-api/users/followers", "followers", args);
}

/**
 * Accounts `handle` follows, in ID order. Same paging and lookup rules as `get_followers`:
 * `limit` defaults to 100, `max_results` is the per-page size (at most 1000), `ids_only` skips
 * full profiles.
 */
export async function get_following(
  args: { handle: string; limit?: number; ids_only?: boolean; cursor?: string; max_results?: number },
  ctx: Ctx,
): Promise<FollowersResult> {
  return followList(ctx, "/twttr-api/users/followings", "following", args);
}

/** Full user objects for user IDs (not handles), 50 per request; batches automatically. */
export async function lookup_users(args: { ids: string[] }, ctx: Ctx): Promise<{ users: User[] }> {
  return { users: await lookupByIds(ctx, args.ids) };
}

/**
 * Search tweets with Synoptic's advanced query syntax (`from:user`, `since:YYYY-MM-DD`,
 * `min_faves:100`, quoted phrases). `search_type` is `latest` (chronological) or `top`; pages
 * until `limit` results (default 20) or the cursor runs out. `next_cursor` and unit `meta` are from
 * the last page.
 */
export async function search_tweets(
  args: { query: string; search_type?: "latest" | "top"; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ tweets: Tweet[]; next_cursor: string | null; meta: UsageMeta }> {
  const limit = args.limit ?? 20;
  const tweets: Tweet[] = [];
  let cursor = args.cursor;
  let meta: UsageMeta = {};
  while (tweets.length < limit) {
    const data = await get(ctx, "/twttr-api/tweets/search", {
      query: args.query,
      search_type: args.search_type ?? "latest",
      cursor,
    });
    const page = data.data || {};
    const results = (page.results || []) as Tweet[];
    meta = usage(data);
    tweets.push(...results);
    cursor = page.next_cursor ? String(page.next_cursor) : undefined;
    if (!cursor || results.length === 0) break;
  }
  return { tweets: tweets.slice(0, limit), next_cursor: cursor ?? null, meta };
}

/**
 * Look up tweets by ID, 100 per request (batches automatically). A tweet that links to an X
 * article gets the raw fxtwitter article object attached as `article` (title, preview_text,
 * content.blocks). Other fields are Synoptic's tweet objects.
 */
export async function lookup_tweets(args: { ids: string[] }, ctx: Ctx): Promise<{ tweets: Tweet[] }> {
  const tweets: Tweet[] = [];
  for (let start = 0; start < args.ids.length; start += 100) {
    const data = await get(ctx, "/twttr-api/tweets/lookup", {
      tweet_ids: args.ids.slice(start, start + 100).join(","),
    });
    tweets.push(...((data.data || []) as Tweet[]));
  }
  return { tweets: await expandArticles(ctx, tweets) };
}

/**
 * Fetch an X long-form article via fxtwitter. Pass the tweet URL
 * (`https://x.com/<user>/status/<id>`), not the article URL. Returns the raw article object
 * (`title`, `preview_text`, `content.blocks` as Draft.js-style blocks) plus `tweet_url` and
 * `author`. Throws when the URL is not a tweet URL or the tweet has no article.
 */
export async function get_article(args: { tweet_url: string }, ctx: Ctx): Promise<ArticleResult> {
  const match = TWEET_URL_RE.exec(args.tweet_url);
  if (!match) {
    if (ARTICLE_URL_RE.test(args.tweet_url)) {
      throw new Error("Please provide the tweet URL, not the article URL directly.");
    }
    throw new Error(`Could not parse tweet URL: ${args.tweet_url}`);
  }
  const screenName = match[1];
  const tweetId = match[2];
  const article = await fetchArticle(ctx, screenName, tweetId);
  if (!article) throw new Error("No article found for this tweet.");
  return { article, tweet_url: `https://x.com/${screenName}/status/${tweetId}`, author: screenName };
}

/**
 * A user's recent tweets, newest first: resolves the profile, then pages the user timeline until
 * `limit` tweets (default 20). Returns the profile (null when unknown), `tweets`, `next_cursor`
 * and unit `meta` from the last page. Best for "last N tweets by @user".
 */
export async function get_timeline(
  args: { handle: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ user: User | null; tweets: Tweet[]; next_cursor: string | null; meta: UsageMeta }> {
  const { user } = await get_user({ handle: args.handle }, ctx);
  if (!user) return { user: null, tweets: [], next_cursor: null, meta: {} };
  const userId = user.user_id;
  if (userId === undefined || userId === null) {
    return { user, tweets: [], next_cursor: null, meta: {} };
  }
  const limit = args.limit ?? 20;
  const tweets: Tweet[] = [];
  let cursor = args.cursor;
  let meta: UsageMeta = {};
  while (tweets.length < limit) {
    const data = await get(
      ctx,
      `/twttr-api/users/${encodeURIComponent(String(userId))}/timeline`,
      { cursor },
    );
    const page = data.data || {};
    const results = (page.results || []) as Tweet[];
    meta = usage(data);
    tweets.push(...results);
    cursor = page.next_cursor ? String(page.next_cursor) : undefined;
    if (!cursor || results.length === 0) break;
  }
  return { user, tweets: tweets.slice(0, limit), next_cursor: cursor ?? null, meta };
}

/**
 * Credit/unit probe: resolves @twitter and returns that response's `consumed_units`,
 * `remaining_units`, `returned_items` and `price_per_item`. Use it as a read-only auth and
 * connectivity check; the follower, search and timeline functions also return it as `meta`.
 */
export async function get_usage(args: {}, ctx: Ctx): Promise<{ usage: UsageMeta }> {
  const data = await get(ctx, "/twttr-api/users/lookup", { screen_name: "twitter" });
  return { usage: usage(data) };
}
