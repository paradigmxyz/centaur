import type { Ctx } from "../centaur";
import { readFeed, type FeedItem } from "../_lib/feed";

// Google News' public RSS endpoints. They need no credentials and no query for
// headlines; search is Google-side, so `search` can reach beyond the current
// headlines. Links are news.google.com redirects to the publisher.

const BASE_URL = "https://news.google.com/rss";
const DEFAULT_LIMIT = 20;

/** The topic sections Google News publishes RSS feeds for (`topic` accepts these, case-insensitive). */
export type Topic =
  | "WORLD"
  | "NATION"
  | "BUSINESS"
  | "TECHNOLOGY"
  | "ENTERTAINMENT"
  | "SPORTS"
  | "SCIENCE"
  | "HEALTH";

/** One Google News result. */
export interface Article {
  /** Headline, with the publisher appended as Google News does (`... - BBC`). */
  title: string;
  /** news.google.com RSS redirect to the publisher's article. */
  link: string;
  /** Publication time as the feed states it, RFC 822 (`Fri, 13 Nov 2026 15:40:08 GMT`). */
  published: string;
  /** Publisher name, e.g. `BBC`; `""` when the item has no source. */
  source: string;
}

function article(item: FeedItem): Article {
  return {
    title: item.title,
    link: item.link,
    published: item.published,
    source: item.source?.title ?? "",
  };
}

function limitOf(value: number | undefined): number {
  return value === undefined ? DEFAULT_LIMIT : Math.max(0, Math.trunc(value));
}

/** `quote_plus`: like `encodeURIComponent` but spaces become `+`, as Google's search URLs use. */
function quotePlus(text: string): string {
  return encodeURIComponent(text).replace(/%20/g, "+");
}

/** Country/language parameters shared by the headlines and topic feeds. */
function locale(country: string): string {
  return `hl=en-${country}&gl=${country}&ceid=${country}:en`;
}

async function articles(ctx: Ctx, url: string): Promise<Article[]> {
  const feed = await readFeed(ctx, url, "Google News RSS");
  return feed.items.map(article);
}

/**
 * Search Google News for `query` (Google search syntax works: quotes, `site:`,
 * `when:7d`, `OR`, ...). Returns one item per story with headline, publisher,
 * publication time and redirect link, most relevant first. Google decides how many
 * results the feed carries (usually tens); `limit` (default 20) trims the list.
 */
export async function search(
  args: { query: string; limit?: number },
  ctx: Ctx,
): Promise<{ articles: Article[] }> {
  const url = `${BASE_URL}/search?q=${quotePlus(args.query)}`;
  const items = await articles(ctx, url);
  return { articles: items.slice(0, limitOf(args.limit)) };
}

/**
 * Top stories for a country, newest first. `country` is a two-letter code as Google News uses
 * it (`US`, `GB`, `DE`, `IN`, ...; default `US`); the feed is English (`hl=en-<country>`).
 * `limit` defaults to 20.
 */
export async function headlines(
  args: { country?: string; limit?: number },
  ctx: Ctx,
): Promise<{ articles: Article[] }> {
  const url = `${BASE_URL}?${locale(args.country ?? "US")}`;
  const items = await articles(ctx, url);
  return { articles: items.slice(0, limitOf(args.limit)) };
}

/**
 * Headlines for one topic section: `WORLD`, `NATION`, `BUSINESS`, `TECHNOLOGY`,
 * `ENTERTAINMENT`, `SPORTS`, `SCIENCE` or `HEALTH` (case-insensitive). `country` is a
 * two-letter code (default `US`). `limit` defaults to 20.
 */
export async function topic(
  args: { topic: string; country?: string; limit?: number },
  ctx: Ctx,
): Promise<{ articles: Article[] }> {
  const name = args.topic.toUpperCase();
  const url = `${BASE_URL}/headlines/section/topic/${encodeURIComponent(name)}?${locale(args.country ?? "US")}`;
  const items = await articles(ctx, url);
  return { articles: items.slice(0, limitOf(args.limit)) };
}
