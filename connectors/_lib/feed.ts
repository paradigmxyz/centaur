// A small, dependency-free RSS 2.0 / Atom parser for the feed connectors.
//
// It is a scanner, not a validating XML parser: it finds elements by local name
// (namespace prefixes are ignored), unwraps CDATA sections, decodes entities and
// leaves everything it does not know alone. It reads what news feeds actually
// send: RSS `<channel>/<item>` with `pubDate`, `description`, `guid`,
// `category`, `dc:creator` and `source`, and Atom `<feed>/<entry>` with `id`,
// `link href`, `published`/`updated`, `summary`/`content`, `category term` and
// `author/name`.

import type { Ctx } from "../centaur";

/** The publisher of an item, from RSS `<source url="...">Name</source>` (Google News sets it). */
export interface FeedSource {
  title: string;
  /** Empty when the element has no `url` attribute. */
  url: string;
}

/**
 * One item: `<item>` in RSS, `<entry>` in Atom. Elements the feed omits come back as
 * `""`/`[]`; no field is invented.
 */
export interface FeedItem {
  /** Item title as text: markup removed, entities decoded, whitespace collapsed. */
  title: string;
  /** The item URL. For Atom, the `rel="alternate"` link; for Google News, a news.google.com redirect. */
  link: string;
  /** The date the feed states, unchanged: RFC 822 for RSS (`Fri, 13 Nov 2026 17:05:43 +0000`), RFC 3339 for Atom. */
  published: string;
  /**
   * The summary/description element: RSS `<description>`, else Atom `<summary>`, else
   * `<content>`. CDATA is unwrapped and XML entities are decoded once, so escaped HTML
   * arrives as HTML; inline markup is otherwise kept as the feed sends it.
   */
  summary: string;
  /** `<guid>`/`<id>`, else RSS 1.0 `rdf:about`, unchanged; `""` when the feed has none. */
  guid: string;
  /** `<category>` terms: the element text, or the `term` attribute for Atom. */
  categories: string[];
  /** `<dc:creator>`, RSS `<author>`, or Atom `<author>.<name>`; `""` when absent. */
  author: string;
  /** Present only when the item has a `<source>` element. */
  source?: FeedSource;
}

/** A parsed feed: the channel/feed metadata plus its items in document order. */
export interface Feed {
  title: string;
  link: string;
  description: string;
  language: string;
  /** `lastBuildDate` for RSS, `updated` for Atom; unchanged. */
  updated: string;
  items: FeedItem[];
}

// The five XML entities plus the HTML ones news feeds actually send. Unknown
// names are left as written, not dropped.
const ENTITIES: Record<string, string> = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'",
  nbsp: "\u00a0", ensp: "\u2002", emsp: "\u2003", thinsp: "\u2009", shy: "\u00ad",
  mdash: "\u2014", ndash: "\u2013", minus: "\u2212", hellip: "\u2026",
  lsquo: "\u2018", rsquo: "\u2019", sbquo: "\u201a", ldquo: "\u201c", rdquo: "\u201d",
  bdquo: "\u201e", laquo: "\u00ab", raquo: "\u00bb", prime: "\u2032",
  copy: "\u00a9", reg: "\u00ae", trade: "\u2122", deg: "\u00b0", middot: "\u00b7",
  bull: "\u2022", dagger: "\u2020", sect: "\u00a7", para: "\u00b6", times: "\u00d7",
  euro: "\u20ac", pound: "\u00a3", yen: "\u00a5", cent: "\u00a2", dollar: "$",
  eacute: "\u00e9", egrave: "\u00e8", agrave: "\u00e0", aacute: "\u00e1",
  ccedil: "\u00e7", uuml: "\u00fc", ouml: "\u00f6", auml: "\u00e4", szlig: "\u00df",
  ntilde: "\u00f1", oslash: "\u00f8", aring: "\u00e5", aelig: "\u00e6",
};

/** Decodes named and numeric (`&#8217;`, `&#x2019;`) XML/HTML entities once. */
export function decodeXmlEntities(text: string): string {
  return text.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z][a-zA-Z0-9]*);/g, (match, body: string) => {
    if (body[0] === "#") {
      const code = body[1] === "x" || body[1] === "X"
        ? parseInt(body.slice(2), 16)
        : parseInt(body.slice(1), 10);
      return code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : match;
    }
    return ENTITIES[body] ?? match;
  });
}

interface Tag {
  /** Lowercase local name, prefix removed. */
  name: string;
  /** Raw attribute text, including leading whitespace. */
  attrs: string;
  closing: boolean;
  selfClosing: boolean;
  /** Index just past the tag. */
  end: number;
}

interface Element {
  attrs: string;
  inner: string;
}

/** Reads the tag whose `<` is at `at`, or `null` when it is not a well-formed tag start. */
function tagAt(xml: string, at: number): Tag | null {
  let i = at + 1;
  const closing = xml[i] === "/";
  if (closing) i++;
  const start = i;
  while (i < xml.length && !/[\s/>]/.test(xml[i])) i++;
  if (i === start) return null;
  const raw = xml.slice(start, i);
  const colon = raw.indexOf(":");
  const name = (colon >= 0 ? raw.slice(colon + 1) : raw).toLowerCase();
  const attrsStart = i;
  let quote = "";
  while (i < xml.length) {
    const ch = xml[i];
    if (quote) {
      if (ch === quote) quote = "";
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === ">") {
      break;
    }
    i++;
  }
  if (i >= xml.length) return null;
  let attrsEnd = i;
  let selfClosing = false;
  if (xml[attrsEnd - 1] === "/") {
    selfClosing = true;
    attrsEnd--;
  }
  return { name, attrs: xml.slice(attrsStart, attrsEnd), closing, selfClosing, end: i + 1 };
}

/** Index just past the CDATA section starting at `at` (`<![CDATA[`). */
function cdataEnd(xml: string, at: number): number {
  const end = xml.indexOf("]]>", at + 9);
  return end < 0 ? xml.length : end + 3;
}

/** Every element with one of `names`, in document order, with its inner XML. */
function elements(xml: string, names: string[]): Element[] {
  const found: Element[] = [];
  let i = 0;
  while (i < xml.length) {
    const lt = xml.indexOf("<", i);
    if (lt < 0) break;
    if (xml.startsWith("<![CDATA[", lt)) {
      i = cdataEnd(xml, lt);
      continue;
    }
    const tag = tagAt(xml, lt);
    if (!tag) {
      i = lt + 1;
      continue;
    }
    if (tag.closing || !names.includes(tag.name)) {
      i = tag.end;
      continue;
    }
    if (tag.selfClosing) {
      found.push({ attrs: tag.attrs, inner: "" });
      i = tag.end;
      continue;
    }
    let depth = 1;
    let j = tag.end;
    let closeStart = xml.length;
    let closeEnd = xml.length;
    while (j < xml.length) {
      const next = xml.indexOf("<", j);
      if (next < 0) break;
      if (xml.startsWith("<![CDATA[", next)) {
        j = cdataEnd(xml, next);
        continue;
      }
      const nested = tagAt(xml, next);
      if (!nested) {
        j = next + 1;
        continue;
      }
      if (nested.name === tag.name) {
        if (nested.closing) {
          depth--;
          if (depth === 0) {
            closeStart = next;
            closeEnd = nested.end;
            break;
          }
        } else if (!nested.selfClosing) {
          depth++;
        }
      }
      j = nested.end;
    }
    found.push({ attrs: tag.attrs, inner: xml.slice(tag.end, closeStart) });
    i = closeEnd;
  }
  return found;
}

/** The first element matching `names`, trying the names in the given priority order. */
function firstElement(xml: string, names: string[]): Element | null {
  for (const name of names) {
    const found = elements(xml, [name]);
    if (found.length) return found[0];
  }
  return null;
}

/** Attributes by lowercase local name (`href`, `term`, `url`), entities decoded. */
function attributes(attrs: string): Record<string, string> {
  const out: Record<string, string> = {};
  const pattern = /([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
  let match = pattern.exec(attrs);
  while (match !== null) {
    const key = match[1].toLowerCase();
    const colon = key.indexOf(":");
    const local = colon >= 0 ? key.slice(colon + 1) : key;
    if (!(local in out)) out[local] = decodeXmlEntities(match[2] ?? match[3] ?? "");
    match = pattern.exec(attrs);
  }
  return out;
}

/**
 * An element's inner XML as text: CDATA content verbatim, text nodes with entities decoded
 * once, and either the markup kept as-is or removed.
 */
function innerText(inner: string, stripMarkup: boolean): string {
  let out = "";
  let i = 0;
  while (i < inner.length) {
    const lt = inner.indexOf("<", i);
    if (lt < 0) {
      out += decodeXmlEntities(inner.slice(i));
      break;
    }
    if (lt > i) out += decodeXmlEntities(inner.slice(i, lt));
    if (inner.startsWith("<![CDATA[", lt)) {
      const end = inner.indexOf("]]>", lt + 9);
      out += inner.slice(lt + 9, end < 0 ? inner.length : end);
      i = end < 0 ? inner.length : end + 3;
      continue;
    }
    const tag = tagAt(inner, lt);
    if (!tag) {
      out += "<";
      i = lt + 1;
      continue;
    }
    if (!stripMarkup) out += inner.slice(lt, tag.end);
    i = tag.end;
  }
  return out;
}

/** Plain text of an element: markup removed, whitespace collapsed. */
function plain(element: Element): string {
  return innerText(element.inner, true).replace(/\s+/g, " ").trim();
}

/** The item or feed URL: RSS link text, or an Atom `rel="alternate"` link (the first link otherwise). */
function linkOf(block: Element): string {
  let fallback = "";
  for (const link of elements(block.inner, ["link"])) {
    const attrs = attributes(link.attrs);
    if (attrs.href === undefined) {
      const value = plain(link);
      if (value) return value;
      continue;
    }
    if (attrs.rel === undefined || attrs.rel === "" || attrs.rel === "alternate") return attrs.href;
    if (!fallback) fallback = attrs.href;
  }
  return fallback;
}

function itemOf(block: Element): FeedItem {
  const title = firstElement(block.inner, ["title"]);
  const date = firstElement(block.inner, ["pubdate", "published", "updated", "date"]);
  const summary = firstElement(block.inner, ["description", "summary", "content"]);
  const guid = firstElement(block.inner, ["guid", "id"]);
  const authorElement = firstElement(block.inner, ["creator", "author"]);
  const sourceElement = firstElement(block.inner, ["source"]);
  const about = attributes(block.attrs).about;

  let author = "";
  if (authorElement) {
    const name = firstElement(authorElement.inner, ["name"]);
    author = plain(name ?? authorElement);
  }

  const item: FeedItem = {
    title: title ? plain(title) : "",
    link: linkOf(block),
    published: date ? plain(date) : "",
    summary: summary ? innerText(summary.inner, false).trim() : "",
    guid: guid ? plain(guid) : about ?? "",
    categories: elements(block.inner, ["category"])
      .map((category) => {
        const attrs = attributes(category.attrs);
        return plain(category) || attrs.term || attrs.label || "";
      })
      .filter((category) => category !== ""),
    author,
  };
  if (sourceElement) {
    const attrs = attributes(sourceElement.attrs);
    item.source = { title: plain(sourceElement) || attrs.title || "", url: attrs.url ?? "" };
  }
  return item;
}

/**
 * Parses RSS 2.0, RSS 1.0 (RDF) or Atom into feed metadata and items. Throws when the
 * document is neither (an HTML or JSON error page), with the start of the body in the message.
 */
export function parseFeed(xml: string): Feed {
  const channel = firstElement(xml, ["channel", "feed"]);
  if (!channel) {
    throw new Error(`not an RSS or Atom feed: ${xml.replace(/\s+/g, " ").trim().slice(0, 200)}`);
  }
  const title = firstElement(channel.inner, ["title"]);
  const description = firstElement(channel.inner, ["description", "subtitle"]);
  const language = firstElement(channel.inner, ["language"]);
  const updated = firstElement(channel.inner, ["lastbuilddate", "updated", "pubdate"]);
  return {
    title: title ? plain(title) : "",
    link: linkOf(channel),
    description: description ? plain(description) : "",
    language: language ? plain(language) : "",
    updated: updated ? plain(updated) : "",
    items: elements(xml, ["item", "entry"]).map(itemOf),
  };
}

/**
 * Fetches `url` and parses it as a feed. `headers` are sent as given (feeds often want a
 * browser User-Agent); a non-2xx response throws `<label> returned HTTP <status>: <body>`.
 */
export async function readFeed(
  ctx: Ctx,
  url: string,
  label: string,
  headers: Record<string, string> = {},
): Promise<Feed> {
  const res = await ctx.fetch(url, {
    headers: { accept: "application/rss+xml, application/xml, text/xml, */*", ...headers },
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${label} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  try {
    return parseFeed(text);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`${label}: ${message}`);
  }
}
