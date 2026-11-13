import type { Ctx } from "../centaur";
import { readFeed, type FeedItem } from "../_lib/feed";

// The Block's public WordPress RSS feed. It sends plain-text standfirsts and the
// reporter in `dc:creator`; the full article is behind the link (and the paywall
// for some stories). Like CoinDesk, one fetch serves both tools.

const FEED_URL = "https://www.theblock.co/rss.xml";
const DEFAULT_LIMIT = 20;
const HEADERS: Record<string, string> = {
  "user-agent":
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
};

/** One The Block story. */
export interface Article {
  title: string;
  /** Article URL on www.theblock.co. */
  link: string;
  /** Publication time as the feed states it, RFC 822 (`Fri, 13 Nov 2026 16:37:26 +0000`). */
  published: string;
  /** One- or two-sentence standfirst, plain text. */
  summary: string;
  author: string;
}

function article(item: FeedItem): Article {
  return {
    title: item.title,
    link: item.link,
    published: item.published,
    summary: item.summary,
    author: item.author,
  };
}

function limitOf(value: number | undefined): number {
  return value === undefined ? DEFAULT_LIMIT : Math.max(0, Math.trunc(value));
}

/** Fetches the whole feed once; both tools read it. */
async function articles(ctx: Ctx): Promise<Article[]> {
  const feed = await readFeed(ctx, FEED_URL, "The Block RSS", HEADERS);
  return feed.items.map(article);
}

/**
 * The latest The Block articles, newest first: title, link, publication time, standfirst and
 * reporter. Reads the public RSS feed, which carries only the recent stories, so there is no
 * deeper pagination; `limit` (default 20) just trims the list.
 */
export async function news(args: { limit?: number }, ctx: Ctx): Promise<{ articles: Article[] }> {
  const items = await articles(ctx);
  return { articles: items.slice(0, limitOf(args.limit)) };
}

/**
 * The Block articles matching `query`, newest first, filtered on the same fetch of the recent
 * feed: case-insensitive substring over title and standfirst. Not an archive search — only what
 * the current feed carries can match. `limit` defaults to 20.
 */
export async function search(
  args: { query: string; limit?: number },
  ctx: Ctx,
): Promise<{ articles: Article[] }> {
  const query = args.query.toLowerCase();
  const items = await articles(ctx);
  return {
    articles: items
      .filter(
        (item) =>
          item.title.toLowerCase().includes(query) || item.summary.toLowerCase().includes(query),
      )
      .slice(0, limitOf(args.limit)),
  };
}
