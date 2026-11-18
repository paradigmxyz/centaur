import type { Ctx } from "../centaur";
import { readFeed, type FeedItem } from "../_lib/feed";

// CoinDesk's public Arc RSS feed. Articles are CoinDesk's own; the summary is
// the standfirst and `categories` are the feed's category terms. Fetching the
// feed is the only cost, so `news` and `search` share it and slice/filter locally.

const FEED_URL = "https://www.coindesk.com/arc/outboundfeeds/rss/";
const DEFAULT_LIMIT = 20;
const HEADERS: Record<string, string> = {
  "user-agent":
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
  "accept-language": "en-US,en;q=0.9",
  referer: "https://www.coindesk.com/",
};

/** One CoinDesk story. */
export interface Article {
  title: string;
  /** Article URL on www.coindesk.com. */
  link: string;
  /** Publication time as the feed states it, RFC 822 (`Fri, 13 Nov 2026 17:05:43 +0000`). */
  published: string;
  /** Standfirst as HTML or plain text, as the feed sends it. */
  summary: string;
  author: string;
  /** Feed `<category>` terms, e.g. `Policy`, `Markets`, `Regulation` (the Python tool called these tags). */
  categories: string[];
}

function article(item: FeedItem): Article {
  return {
    title: item.title,
    link: item.link,
    published: item.published,
    summary: item.summary,
    author: item.author,
    categories: item.categories,
  };
}

function limitOf(value: number | undefined): number {
  return value === undefined ? DEFAULT_LIMIT : Math.max(0, Math.trunc(value));
}

/** Fetches the whole feed once; both tools read it. */
async function articles(ctx: Ctx): Promise<Article[]> {
  const feed = await readFeed(ctx, FEED_URL, "CoinDesk RSS", HEADERS);
  return feed.items.map(article);
}

/**
 * The latest CoinDesk articles, newest first: title, link, publication time, standfirst,
 * author and category terms. Reads the site-wide RSS feed, which carries only the recent
 * stories (about 20), so there is no deeper pagination; `limit` (default 20) just trims the
 * list.
 */
export async function news(args: { limit?: number }, ctx: Ctx): Promise<{ articles: Article[] }> {
  const items = await articles(ctx);
  return { articles: items.slice(0, limitOf(args.limit)) };
}

/**
 * CoinDesk articles matching `query`, newest first, filtered on the same fetch of the recent
 * feed: case-insensitive substring over title, standfirst and category terms. Not a full
 * archive search — only what the current feed carries can match. `limit` defaults to 20.
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
          item.title.toLowerCase().includes(query) ||
          item.summary.toLowerCase().includes(query) ||
          item.categories.some((category) => category.toLowerCase().includes(query)),
      )
      .slice(0, limitOf(args.limit)),
  };
}
