import type { Ctx } from "../centaur";
import { query, readJson } from "../_lib/http";

// X API v2 objects are passed through as the API sends them; the interfaces list
// fields worth knowing. Expanded objects (authors, media, polls, places,
// referenced tweets) stay in the response's `includes` map, keyed as X returns
// them; join on IDs yourself.

const DEFAULT_BASE = "https://api.x.com/2";

const USER_FIELDS =
  "created_at,description,entities,id,location,name,pinned_tweet_id," +
  "profile_image_url,protected,public_metrics,url,username,verified,verified_type";
const TWEET_FIELDS =
  "article,attachments,author_id,conversation_id,created_at,entities,geo,id,in_reply_to_user_id," +
  "lang,note_tweet,possibly_sensitive,public_metrics,referenced_tweets,reply_settings,source,text";
const MEDIA_FIELDS =
  "alt_text,duration_ms,height,media_key,preview_image_url,public_metrics,type,url,variants,width";
const POLL_FIELDS = "duration_minutes,end_datetime,id,options,voting_status";
const PLACE_FIELDS = "contained_within,country,country_code,full_name,geo,id,name,place_type";
const DEFAULT_EXPANSIONS =
  "article.cover_media,article.media_entities,attachments.media_keys,attachments.poll_ids," +
  "author_id,geo.place_id,referenced_tweets.id,referenced_tweets.id.attachments.media_keys," +
  "referenced_tweets.id.author_id";

/** Query parameters: `query` skips undefined, null and empty values. */
type Params = Record<string, string | number | boolean | undefined | null>;

/** A user's public counters. */
export interface UserPublicMetrics {
  followers_count?: number;
  following_count?: number;
  tweet_count?: number;
  listed_count?: number;
  like_count?: number;
  [field: string]: unknown;
}

/** An X user object. */
export interface XUser {
  id: string;
  username: string;
  name: string;
  created_at?: string;
  description?: string;
  location?: string;
  url?: string;
  protected?: boolean;
  verified?: boolean;
  verified_type?: string;
  pinned_tweet_id?: string;
  profile_image_url?: string;
  public_metrics?: UserPublicMetrics;
  [field: string]: unknown;
}

/** A tweet's public counters. */
export interface TweetPublicMetrics {
  retweet_count?: number;
  reply_count?: number;
  like_count?: number;
  quote_count?: number;
  bookmark_count?: number;
  impression_count?: number;
  [field: string]: unknown;
}

/** An X post (tweet) object. */
export interface XTweet {
  id: string;
  text: string;
  author_id?: string;
  created_at?: string;
  conversation_id?: string;
  lang?: string;
  note_tweet?: { text?: string; entities?: unknown; [field: string]: unknown };
  attachments?: { media_keys?: string[]; poll_ids?: string[]; [field: string]: unknown };
  public_metrics?: TweetPublicMetrics;
  referenced_tweets?: { type: string; id: string; [field: string]: unknown }[];
  [field: string]: unknown;
}

/** Expanded media object (`media_key` joins to `attachments.media_keys`). */
export interface XMedia {
  media_key: string;
  type: string;
  [field: string]: unknown;
}

/** Expanded poll object (`id` joins to `attachments.poll_ids`). */
export interface XPoll {
  id: string;
  [field: string]: unknown;
}

/** Expanded place object (`id` joins to `geo.place_id`). */
export interface XPlace {
  id: string;
  [field: string]: unknown;
}

/** Expanded users, posts, media, polls and places carried beside the primary data. */
export interface XIncludes {
  users?: XUser[];
  tweets?: XTweet[];
  media?: XMedia[];
  polls?: XPoll[];
  places?: XPlace[];
  [field: string]: unknown;
}

/** Pagination metadata assembled across fetched pages. */
export interface PageMeta {
  result_count?: number;
  newest_id?: string;
  oldest_id?: string;
  next_token?: string;
  previous_token?: string;
  [field: string]: unknown;
}

/** Project tweet-usage counters. */
export interface Usage {
  cap_reset_day?: number;
  project_cap?: number;
  project_id?: string;
  project_usage?: number;
  [field: string]: unknown;
}

interface Page {
  items: any[];
  meta: PageMeta;
  includes: XIncludes;
}

function apiBase(ctx: Ctx): string {
  return (ctx.env.X_API_BASE_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function authHeaders(ctx: Ctx): Record<string, string> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy `Authorization: Bearer <token>` is injected on the wire;
  // locally a real bearer token comes from the env.
  if (ctx.env.X_API_KEY) headers.authorization = `Bearer ${ctx.env.X_API_KEY}`;
  return headers;
}

function normalizeHandle(handle: string): string {
  return handle.trim().replace(/^@+/, "");
}

async function request(ctx: Ctx, endpoint: string, params: Params): Promise<any> {
  const res = await ctx.fetch(`${apiBase(ctx)}${endpoint}${query(params)}`, {
    headers: authHeaders(ctx),
  });
  const body = await readJson(res, "X API");
  const errors = (body.errors || []) as { detail?: string; title?: string; [field: string]: unknown }[];
  if (errors.length > 0) {
    // X answers 200 with item-level errors alongside (or instead of) data.
    const details = errors
      .map((error) => String(error.detail || error.title || JSON.stringify(error)))
      .join("; ");
    throw new Error(`X API response contained errors: ${details}`);
  }
  return body;
}

function mergeIncludes(target: XIncludes, source: XIncludes | undefined): void {
  for (const key of Object.keys(source || {})) {
    const incoming = (source as Record<string, unknown>)[key];
    if (!Array.isArray(incoming)) {
      target[key] = incoming;
      continue;
    }
    const existing = target[key];
    target[key] = Array.isArray(existing) ? [...existing, ...incoming] : [...incoming];
  }
}

function chunks(values: string[], size: number): string[][] {
  const out: string[][] = [];
  for (let start = 0; start < values.length; start += size) {
    out.push(values.slice(start, start + size));
  }
  return out;
}

function tweetParams(): Params {
  return {
    "tweet.fields": TWEET_FIELDS,
    "user.fields": USER_FIELDS,
    "media.fields": MEDIA_FIELDS,
    "poll.fields": POLL_FIELDS,
    "place.fields": PLACE_FIELDS,
    expansions: DEFAULT_EXPANSIONS,
  };
}

async function paged(
  ctx: Ctx,
  endpoint: string,
  dataKey: string,
  limit: number,
  params: Params,
  options: { maxPageSize?: number; minPageSize?: number; tokenParam?: string } = {},
): Promise<Page> {
  const maxPageSize = options.maxPageSize ?? 100;
  const minPageSize = options.minPageSize ?? 1;
  const tokenParam = options.tokenParam ?? "pagination_token";
  const items: any[] = [];
  const includes: XIncludes = {};
  const meta: PageMeta = {};
  let token: string | undefined;
  while (items.length < limit) {
    const pageSize = Math.max(minPageSize, Math.min(maxPageSize, limit - items.length));
    const body = await request(ctx, endpoint, {
      ...params,
      max_results: pageSize,
      [tokenParam]: token,
    });
    const pageMeta = (body.meta || {}) as PageMeta;
    if (meta.newest_id === undefined && pageMeta.newest_id !== undefined) {
      meta.newest_id = pageMeta.newest_id;
    }
    if (pageMeta.oldest_id !== undefined) meta.oldest_id = pageMeta.oldest_id;
    for (const [key, value] of Object.entries(pageMeta)) {
      if (key !== "newest_id" && key !== "oldest_id" && key !== "next_token" && key !== "result_count") {
        meta[key] = value;
      }
    }
    mergeIncludes(includes, body.includes as XIncludes | undefined);
    const pageItems = (body[dataKey] || []) as any[];
    items.push(...pageItems);
    token = pageMeta.next_token;
    if (!token || pageItems.length === 0) break;
  }
  const limited = items.slice(0, limit);
  meta.result_count = limited.length;
  if (token) meta.next_token = token;
  return { items: limited, meta, includes };
}

async function lookupUsers(ctx: Ctx, ids: string[]): Promise<XUser[]> {
  const users: XUser[] = [];
  for (const chunk of chunks(ids, 100)) {
    const body = await request(ctx, "/users", { ids: chunk.join(","), "user.fields": USER_FIELDS });
    users.push(...((body.data || []) as XUser[]));
  }
  return users;
}

/**
 * A user profile by handle (`@` optional): id, username, name, description, location, url,
 * created_at, protected, verified/verified_type and `public_metrics`. An unknown handle is an X
 * 404 error.
 */
export async function get_user(
  args: { handle: string },
  ctx: Ctx,
): Promise<{ user: XUser | null }> {
  const body = await request(ctx, `/users/by/username/${encodeURIComponent(normalizeHandle(args.handle))}`, {
    "user.fields": USER_FIELDS,
  });
  return { user: (body.data as XUser | undefined) ?? null };
}

/** A user profile by numeric X user ID; an unknown ID is an X 404 error. */
export async function get_user_by_id(
  args: { user_id: string },
  ctx: Ctx,
): Promise<{ user: XUser | null }> {
  const body = await request(ctx, `/users/${encodeURIComponent(args.user_id)}`, {
    "user.fields": USER_FIELDS,
  });
  return { user: (body.data as XUser | undefined) ?? null };
}

/** Full user objects for up to 100 user IDs per request (numeric IDs, not handles); batches automatically. */
export async function lookup_users(args: { ids: string[] }, ctx: Ctx): Promise<{ users: XUser[] }> {
  return { users: await lookupUsers(ctx, args.ids) };
}

/** Full user objects for up to 100 handles per request (`@` optional); batches automatically. */
export async function lookup_users_by_usernames(
  args: { usernames: string[] },
  ctx: Ctx,
): Promise<{ users: XUser[] }> {
  const users: XUser[] = [];
  for (const chunk of chunks(args.usernames.map(normalizeHandle), 100)) {
    const body = await request(ctx, "/users/by", {
      usernames: chunk.join(","),
      "user.fields": USER_FIELDS,
    });
    users.push(...((body.data || []) as XUser[]));
  }
  return { users };
}

/**
 * Accounts following `handle`, with `public_metrics`. Fetches pages until `limit` users (default
 * 100), then returns their IDs in `ids` and full objects in `users` (empty when `ids_only`).
 * Paginate from `meta.next_token`.
 */
export async function get_followers(
  args: { handle: string; limit?: number; ids_only?: boolean },
  ctx: Ctx,
): Promise<{ users: XUser[]; ids: string[]; meta: PageMeta }> {
  const { user } = await get_user({ handle: args.handle }, ctx);
  if (!user) return { users: [], ids: [], meta: {} };
  const page = await paged(ctx, `/users/${encodeURIComponent(user.id)}/followers`, "data", args.limit ?? 100, {
    "user.fields": USER_FIELDS,
  });
  const users = page.items as XUser[];
  return {
    users: args.ids_only ? [] : users,
    ids: users.map((item) => item.id),
    meta: page.meta,
  };
}

/** Accounts `handle` follows. Same paging and return shape as `get_followers`. */
export async function get_following(
  args: { handle: string; limit?: number; ids_only?: boolean },
  ctx: Ctx,
): Promise<{ users: XUser[]; ids: string[]; meta: PageMeta }> {
  const { user } = await get_user({ handle: args.handle }, ctx);
  if (!user) return { users: [], ids: [], meta: {} };
  const page = await paged(ctx, `/users/${encodeURIComponent(user.id)}/following`, "data", args.limit ?? 100, {
    "user.fields": USER_FIELDS,
  });
  const users = page.items as XUser[];
  return {
    users: args.ids_only ? [] : users,
    ids: users.map((item) => item.id),
    meta: page.meta,
  };
}

/**
 * Search posts. `search_type`: `latest`/`recent` (recent search, recency), `top` (recent search,
 * relevancy) or `all` (full-archive search, recency). `query` uses X search syntax (`from:`, `to:`,
 * `#tag`, `has:links`, `-is:retweet`, `since:`, `until:`). Fetches pages until `limit` (default 20)
 * and returns raw tweets, expanded `includes` and `meta`; continue from `meta.next_token`.
 */
export async function search_tweets(
  args: { query: string; search_type?: "latest" | "top" | "recent" | "all"; limit?: number },
  ctx: Ctx,
): Promise<{ tweets: XTweet[]; includes: XIncludes; meta: PageMeta }> {
  const searchType = args.search_type ?? "latest";
  const endpoint = searchType === "all" ? "/tweets/search/all" : "/tweets/search/recent";
  const page = await paged(
    ctx,
    endpoint,
    "data",
    args.limit ?? 20,
    {
      ...tweetParams(),
      query: args.query,
      sort_order: searchType === "top" ? "relevancy" : "recency",
    },
    { maxPageSize: 100, minPageSize: 10, tokenParam: "next_token" },
  );
  return { tweets: page.items as XTweet[], includes: page.includes, meta: page.meta };
}

/**
 * Look up posts by ID, up to 100 per request (batches automatically). Returns raw tweets and the
 * merged `includes` map (`users`, `tweets`, `media`, `polls`, `places`).
 */
export async function lookup_tweets(
  args: { ids: string[] },
  ctx: Ctx,
): Promise<{ tweets: XTweet[]; includes: XIncludes }> {
  const tweets: XTweet[] = [];
  const includes: XIncludes = {};
  for (const chunk of chunks(args.ids, 100)) {
    const body = await request(ctx, "/tweets", { ids: chunk.join(","), ...tweetParams() });
    tweets.push(...((body.data || []) as XTweet[]));
    mergeIncludes(includes, body.includes as XIncludes | undefined);
  }
  return { tweets, includes };
}

/** One post by ID, with its `includes` map. `tweet` is null when X returns no data. */
export async function get_tweet(
  args: { tweet_id: string },
  ctx: Ctx,
): Promise<{ tweet: XTweet | null; includes: XIncludes }> {
  const body = await request(ctx, `/tweets/${encodeURIComponent(args.tweet_id)}`, tweetParams());
  return {
    tweet: (body.data as XTweet | undefined) ?? null,
    includes: (body.includes as XIncludes | undefined) ?? {},
  };
}

/**
 * A user's authored posts (their timeline), newest first, excluding retweets. Resolves the profile
 * first; returns `user` (null when X knows no such handle), raw `tweets`, `includes` and `meta`.
 */
export async function get_timeline(
  args: { handle: string; limit?: number },
  ctx: Ctx,
): Promise<{ user: XUser | null; tweets: XTweet[]; includes: XIncludes; meta: PageMeta }> {
  const { user } = await get_user({ handle: args.handle }, ctx);
  if (!user) return { user: null, tweets: [], includes: {}, meta: {} };
  const page = await paged(ctx, `/users/${encodeURIComponent(user.id)}/tweets`, "data", args.limit ?? 20, {
    ...tweetParams(),
    exclude: "retweets",
  });
  return { user, tweets: page.items as XTweet[], includes: page.includes, meta: page.meta };
}

/**
 * Recent posts mentioning `handle`, newest first. Resolves the profile first; returns raw tweets,
 * `includes` and `meta` (all empty when the account is unknown).
 */
export async function get_mentions(
  args: { handle: string; limit?: number },
  ctx: Ctx,
): Promise<{ tweets: XTweet[]; includes: XIncludes; meta: PageMeta }> {
  const { user } = await get_user({ handle: args.handle }, ctx);
  if (!user) return { tweets: [], includes: {}, meta: {} };
  const page = await paged(ctx, `/users/${encodeURIComponent(user.id)}/mentions`, "data", args.limit ?? 20, tweetParams());
  return { tweets: page.items as XTweet[], includes: page.includes, meta: page.meta };
}

/** Users who liked a post, with `public_metrics`; paginate from `meta.next_token`. */
export async function get_liking_users(
  args: { tweet_id: string; limit?: number },
  ctx: Ctx,
): Promise<{ users: XUser[]; meta: PageMeta }> {
  const page = await paged(ctx, `/tweets/${encodeURIComponent(args.tweet_id)}/liking_users`, "data", args.limit ?? 100, {
    "user.fields": USER_FIELDS,
  });
  return { users: page.items as XUser[], meta: page.meta };
}

/** Users who reposted a post, with `public_metrics`; paginate from `meta.next_token`. */
export async function get_retweeted_by(
  args: { tweet_id: string; limit?: number },
  ctx: Ctx,
): Promise<{ users: XUser[]; meta: PageMeta }> {
  const page = await paged(ctx, `/tweets/${encodeURIComponent(args.tweet_id)}/retweeted_by`, "data", args.limit ?? 100, {
    "user.fields": USER_FIELDS,
  });
  return { users: page.items as XUser[], meta: page.meta };
}

/** Posts that quote a post, newest first, with expanded `includes`; paginate from `meta.next_token`. */
export async function get_quote_tweets(
  args: { tweet_id: string; limit?: number },
  ctx: Ctx,
): Promise<{ tweets: XTweet[]; includes: XIncludes; meta: PageMeta }> {
  const page = await paged(
    ctx,
    `/tweets/${encodeURIComponent(args.tweet_id)}/quote_tweets`,
    "data",
    args.limit ?? 20,
    tweetParams(),
    { maxPageSize: 100, minPageSize: 10 },
  );
  return { tweets: page.items as XTweet[], includes: page.includes, meta: page.meta };
}

/** Posts in a List, newest first, with expanded `includes`; paginate from `meta.next_token`. */
export async function get_list_tweets(
  args: { list_id: string; limit?: number },
  ctx: Ctx,
): Promise<{ tweets: XTweet[]; includes: XIncludes; meta: PageMeta }> {
  const page = await paged(ctx, `/lists/${encodeURIComponent(args.list_id)}/tweets`, "data", args.limit ?? 20, tweetParams());
  return { tweets: page.items as XTweet[], includes: page.includes, meta: page.meta };
}

/** Members of a List with `public_metrics`; paginate from `meta.next_token`. */
export async function get_list_members(
  args: { list_id: string; limit?: number },
  ctx: Ctx,
): Promise<{ users: XUser[]; meta: PageMeta }> {
  const page = await paged(ctx, `/lists/${encodeURIComponent(args.list_id)}/members`, "data", args.limit ?? 100, {
    "user.fields": USER_FIELDS,
  });
  return { users: page.items as XUser[], meta: page.meta };
}

/** Followers of a List with `public_metrics`; paginate from `meta.next_token`. */
export async function get_list_followers(
  args: { list_id: string; limit?: number },
  ctx: Ctx,
): Promise<{ users: XUser[]; meta: PageMeta }> {
  const page = await paged(ctx, `/lists/${encodeURIComponent(args.list_id)}/followers`, "data", args.limit ?? 100, {
    "user.fields": USER_FIELDS,
  });
  return { users: page.items as XUser[], meta: page.meta };
}

/**
 * Project tweet-usage counters for this credential: `project_usage`, `project_cap`, `cap_reset_day`
 * and `project_id`. Read-only auth/connectivity check.
 */
export async function get_usage(args: {}, ctx: Ctx): Promise<{ usage: Usage }> {
  const body = await request(ctx, "/usage/tweets", {});
  return { usage: (body.data as Usage | undefined) ?? {} };
}
