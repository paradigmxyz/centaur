import type { Ctx, Response } from "../centaur";
import { query, type Params } from "../_lib/http";

// SEC EDGAR public data. No credential: SEC only asks for a descriptive
// User-Agent (override with EDGAR_USER_AGENT) and at most 10 requests/second
// (https://www.sec.gov/os/accessing-edgar-data).
//
// Everything except filing document bodies comes from SEC's JSON endpoints.
// Filing rows and XBRL facts are returned with SEC's field names; the small
// number of renamed fields (accession, filed, report_date, ...) are EDGAR's own
// column names spelled the way this connector spells them, as in the Python tool.

const DEFAULT_USER_AGENT = "Centaur EDGAR tool admin@example.com";
const WWW_URL = "https://www.sec.gov";
const DATA_URL = "https://data.sec.gov";
const SEARCH_URL = "https://efts.sec.gov/LATEST/search-index";
const ARCHIVES_URL = `${WWW_URL}/Archives/edgar/data`;

// SEC allows 10 requests/second; stay a little under it.
const MIN_REQUEST_INTERVAL_SECONDS = 0.12;
const RETRY_STATUSES: readonly number[] = [429, 500, 502, 503, 504];
const MAX_ATTEMPTS = 4;
const SEARCH_PAGE_SIZE = 100;
const DEFAULT_MAX_CHARS = 50_000;
const ACCESSION_RE = /^(\d{10})-?(\d{2})-?(\d{6})$/;

/** One row of SEC's ticker -> CIK mapping. */
export interface Ticker {
  cik: number;
  ticker: string;
  title: string;
}

/** One filing from a company's filing history. */
export interface Filing {
  /** Accession number, dashed (`0000320193-24-000123`). */
  accession: string;
  form: string | null;
  /** Filing date, YYYY-MM-DD. */
  filed: string | null;
  /** Period of report, YYYY-MM-DD, when the form has one. */
  report_date: string | null;
  description: string | null;
  /** 8-K/6-K item numbers, such as `2.02`. */
  items: string | null;
  primary_document: string | null;
  /** Archive URL of `primary_document`. */
  url: string | null;
}

/** The company's metadata plus its matching filings. */
export interface CompanyFilings {
  cik: number;
  name: string | null;
  tickers: string[];
  exchanges: string[];
  sic: string | null;
  sic_description: string | null;
  fiscal_year_end: string | null;
  filings: Filing[];
}

/** SEC's `submissions/CIK##########.json` document, as SEC sends it. */
export interface SubmissionDocument {
  cik: string;
  name: string;
  tickers?: string[];
  exchanges?: string[];
  sic?: string;
  sicDescription?: string;
  fiscalYearEnd?: string;
  /** `recent` holds columnar filing arrays; `files` lists older paged files. */
  filings?: {
    recent?: Record<string, unknown>;
    files?: { name: string; filingCount?: number; filingFrom?: string; filingTo?: string }[];
  };
  [field: string]: unknown;
}

/** One full-text search hit. */
export interface FilingSearchHit {
  accession: string;
  /** Document filename inside the filing. */
  filename: string;
  form: string | null;
  /** Filing date, YYYY-MM-DD. */
  filed: string | null;
  /** Period of report, YYYY-MM-DD. */
  period_ending: string | null;
  /** Display names such as `APPLE INC  (AAPL)  (CIK 0000320193)`. */
  companies: string[];
  /** Zero-padded CIKs, as SEC sends them. */
  ciks: string[];
  url: string | null;
}

/** One document in a filing. */
export interface FilingFile {
  name: string;
  size: number | null;
  url: string;
}

/** Extracted text of one filing document. */
export interface FilingDocument {
  cik: number;
  accession: string;
  filename: string;
  url: string;
  /** Characters in the whole (extracted) document, not just this chunk. */
  length: number;
  offset: number;
  truncated: boolean;
  text: string;
}

/** One XBRL concept a company reports. */
export interface XbrlConcept {
  /** `taxonomy:tag`, such as `us-gaap:Revenues`. */
  concept: string;
  label: string | null;
  /** Units the concept is reported in, such as `USD`, `shares`. */
  units: string[];
  /** Number of reported values across all units. */
  facts: number;
}

/** One reported XBRL value. */
export interface XbrlFact {
  start?: string;
  /** Period end, YYYY-MM-DD. */
  end?: string;
  val: number;
  accn?: string;
  fy?: number;
  fp?: string;
  form?: string;
  filed?: string;
  frame?: string;
  [field: string]: unknown;
}

/** One company's value for a concept in one XBRL frame. */
export interface XbrlFrameRow {
  cik: number;
  entityName: string;
  loc?: string;
  start?: string;
  end?: string;
  val: number;
  accn?: string;
  fy?: number;
  fp?: string;
  form?: string;
  filed?: string;
  frame?: string;
  [field: string]: unknown;
}

let lastRequestMillis = 0;

function userAgent(ctx: Ctx): string {
  return ctx.env.EDGAR_USER_AGENT?.trim() || DEFAULT_USER_AGENT;
}

function cik10(cik: number): string {
  return String(cik).padStart(10, "0");
}

function archiveUrl(cik: number, accession: string, filename = ""): string {
  return `${ARCHIVES_URL}/${cik}/${accession.replace(/-/g, "")}/${filename}`;
}

/** Return an accession number in dashed form (`0000320193-24-000123`). */
function normalizeAccession(accession: string): string {
  const match = ACCESSION_RE.exec(accession.trim());
  if (!match) throw new Error(`Invalid accession number: ${JSON.stringify(accession)}`);
  return match.slice(1).join("-");
}

async function throttle(ctx: Ctx): Promise<void> {
  const wait = MIN_REQUEST_INTERVAL_SECONDS * 1000 - (Date.now() - lastRequestMillis);
  if (wait > 0) await ctx.sleep(wait);
  lastRequestMillis = Date.now();
}

function statusError(status: number, body: string): Error {
  let message = `EDGAR returned HTTP ${status}: ${body.slice(0, 500)}`;
  if (status === 403) {
    message += " (SEC may be rate limiting or rejecting the User-Agent; set EDGAR_USER_AGENT to a descriptive contact string)";
  } else if (status === 404) {
    message += " (not found)";
  }
  return new Error(message);
}

/** Rate-limited GET with SEC's retry policy; returns the raw response. */
async function get(ctx: Ctx, url: string, params?: Params): Promise<{ res: Response; text: string }> {
  const headers: Record<string, string> = { "user-agent": userAgent(ctx), accept: "application/json" };
  let res: Response | undefined;
  for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt += 1) {
    await throttle(ctx);
    res = await ctx.fetch(url + query(params ?? {}), { headers });
    if (!RETRY_STATUSES.includes(res.status) || attempt === MAX_ATTEMPTS - 1) break;
    // SEC answers 429/5xx with Retry-After; otherwise back off exponentially.
    const retryAfter = res.headers["retry-after"] ?? "";
    const delay = /^\d+$/.test(retryAfter) ? Number(retryAfter) : 2 ** attempt;
    await ctx.sleep(Math.min(delay, 30) * 1000);
  }
  if (!res) throw new Error("EDGAR request was not attempted");
  const text = await res.text();
  if (!res.ok) throw statusError(res.status, text);
  return { res, text };
}

/** GET a JSON object; EDGAR's JSON endpoints always return objects. */
async function getJson(ctx: Ctx, url: string, params?: Params): Promise<Record<string, any>> {
  const { text } = await get(ctx, url, params);
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(`EDGAR returned a non-JSON response (${text.slice(0, 200)})`);
  }
  if (typeof body !== "object" || body === null || Array.isArray(body)) {
    throw new Error("EDGAR returned an unexpected response.");
  }
  return body as Record<string, any>;
}

/** SEC's ticker -> CIK mapping, fetched on each call (no cache across calls). */
async function fetchTickers(ctx: Ctx): Promise<Ticker[]> {
  const raw = await getJson(ctx, `${WWW_URL}/files/company_tickers.json`);
  const rows: Ticker[] = [];
  for (const row of Object.values(raw)) {
    if (row && typeof row === "object") {
      const value = row as Record<string, any>;
      rows.push({ cik: Number(value.cik_str), ticker: String(value.ticker), title: String(value.title) });
    }
  }
  return rows;
}

/** Resolve a ticker or numeric CIK to an integer CIK. */
async function resolveCik(ctx: Ctx, identifier: string): Promise<number> {
  const value = identifier.trim();
  if (/^\d+$/.test(value)) return Number(value);
  const ticker = value.toUpperCase().replace(/\./g, "-");
  for (const row of await fetchTickers(ctx)) {
    if (row.ticker.toUpperCase() === ticker) return row.cik;
  }
  throw new Error(`Unknown ticker ${JSON.stringify(identifier)}; try lookup() or pass a CIK.`);
}

/** Turn EDGAR's columnar filing arrays into row objects. */
function rows(columns: Record<string, unknown>): Record<string, any>[] {
  const keys = Object.keys(columns).filter((key) => Array.isArray(columns[key]));
  const accession = Array.isArray(columns.accessionNumber) ? columns.accessionNumber.length : 0;
  const out: Record<string, any>[] = [];
  for (let index = 0; index < accession; index += 1) {
    const row: Record<string, any> = {};
    for (const key of keys) {
      const column = columns[key] as any[];
      if (index < column.length) row[key] = column[index];
    }
    out.push(row);
  }
  return out;
}

function filingRow(cik: number, row: Record<string, any>): Filing {
  const accession = String(row.accessionNumber ?? "");
  const primary = String(row.primaryDocument ?? "");
  return {
    accession,
    form: row.form ?? null,
    filed: row.filingDate ?? null,
    report_date: row.reportDate || null,
    description: row.primaryDocDescription || null,
    items: row.items || null,
    primary_document: primary || null,
    url: accession ? archiveUrl(cik, accession, primary) : null,
  };
}

function searchHit(hit: Record<string, any>): FilingSearchHit {
  const source = (hit._source ?? {}) as Record<string, any>;
  const id = String(hit._id ?? "");
  const accession = source.adsh || id.split(":")[0];
  const filename = id.split(":").slice(1).join(":");
  const ciks = (source.ciks ?? []) as string[];
  return {
    accession,
    filename,
    form: source.form ?? null,
    filed: source.file_date ?? null,
    period_ending: source.period_ending ?? null,
    companies: (source.display_names ?? []) as string[],
    ciks,
    url: ciks.length && accession ? archiveUrl(Number(ciks[0]), accession, filename) : null,
  };
}

/** Split `taxonomy:tag` (taxonomy defaults to us-gaap). */
function splitConcept(concept: string): { taxonomy: string; tag: string } {
  const separator = concept.lastIndexOf(":");
  const tag = separator < 0 ? concept : concept.slice(separator + 1);
  if (!tag) throw new Error(`Invalid XBRL concept: ${JSON.stringify(concept)}`);
  return { taxonomy: separator < 0 ? "us-gaap" : concept.slice(0, separator), tag };
}

const ENTITIES: Record<string, string> = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ", ensp: " ", emsp: " ",
  thinsp: " ", ndash: "\u2013", mdash: "\u2014", lsquo: "\u2018", rsquo: "\u2019",
  ldquo: "\u201c", rdquo: "\u201d", hellip: "\u2026", middot: "\u00b7", bull: "\u2022",
  copy: "\u00a9", reg: "\u00ae", trade: "\u2122", deg: "\u00b0", plusmn: "\u00b1",
  times: "\u00d7", frac12: "\u00bd", euro: "\u20ac", pound: "\u00a3", yen: "\u00a5",
  cent: "\u00a2", sect: "\u00a7", para: "\u00b6", prime: "\u2032", Prime: "\u2033",
};

function decodeEntities(text: string): string {
  return text.replace(/&(#x?[0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);?/g, (match: string, body: string) => {
    if (body.charAt(0) === "#") {
      const hex = body.charAt(1) === "x" || body.charAt(1) === "X";
      const code = hex ? parseInt(body.slice(2), 16) : parseInt(body.slice(1), 10);
      return Number.isFinite(code) && code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : match;
    }
    return ENTITIES[body] ?? match;
  });
}

/**
 * Readable text from an EDGAR HTML or inline-XBRL document. String-level
 * extraction only (no DOM): script/style/head/ix:header dropped, table cells
 * joined with `|`, block elements turned into line breaks, entities decoded.
 */
function htmlToText(html: string): string {
  let text = html
    .replace(/<!--[\s\S]*?-->/g, " ")
    .replace(/<(script|style|head)\b[^>]*>[\s\S]*?<\/\1\s*>/gi, " ")
    .replace(/<ix:header\b[^>]*>[\s\S]*?<\/ix:header\s*>/gi, " ")
    .replace(/<\/(td|th)\s*>/gi, " | ")
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<\/(p|div|tr|li|table|h[1-6]|section|article|ul|ol)\s*>/gi, "\n")
    .replace(/<[^>]*>/g, " ");
  text = decodeEntities(text).replace(/\u00a0/g, " ");
  const lines = text.split("\n").map((line) => {
    let value = line.replace(/[ \t\r\f\v]+/g, " ");
    // Financial tables split "$", "%" and ")" into their own cells.
    value = value.replace(/\$ \| /g, "$").replace(/ \| (%|\))/g, "$1");
    return value.trim().replace(/^\|+|\|+$/g, "").trim();
  });
  return lines.join("\n").replace(/\n{3,}/g, "\n\n").trim();
}

/**
 * SEC's full ticker -> CIK mapping (about 10,000 rows). Prefer `lookup` for one
 * company; this is for local joins. `cik` is an integer, `ticker` the symbol.
 */
export async function tickers(args: {}, ctx: Ctx): Promise<{ items: Ticker[] }> {
  void args;
  return { items: await fetchTickers(ctx) };
}

/**
 * Find companies by ticker, CIK or name fragment. `query` is upper-cased and `.`
 * becomes `-` in tickers; a numeric query matches a CIK exactly, otherwise exact
 * ticker matches come first, then companies whose name contains the fragment.
 * `limit` defaults to 10.
 */
export async function lookup(
  args: { query: string; limit?: number },
  ctx: Ctx,
): Promise<{ items: Ticker[] }> {
  const limit = args.limit ?? 10;
  const needle = args.query.trim().toUpperCase().replace(/\./g, "-");
  const all = await fetchTickers(ctx);
  if (/^\d+$/.test(needle)) {
    const cik = Number(needle);
    return { items: all.filter((row) => row.cik === cik).slice(0, limit) };
  }
  const exact = all.filter((row) => row.ticker.toUpperCase() === needle);
  const partial = all.filter(
    (row) => !exact.includes(row) && row.title.toUpperCase().replace(/\./g, "-").includes(needle),
  );
  return { items: [...exact, ...partial].slice(0, limit) };
}

/**
 * A company's raw `submissions/CIK##########.json` document: `filings.recent` is
 * a columnar set of arrays, `filings.files` lists older pages. Large (about 1 MB
 * for a frequent filer); `filings` is usually what you want.
 */
export async function submissions(
  args: { company: string },
  ctx: Ctx,
): Promise<{ cik: number; submissions: SubmissionDocument }> {
  const cik = await resolveCik(ctx, args.company);
  return {
    cik,
    submissions: (await getJson(ctx, `${DATA_URL}/submissions/CIK${cik10(cik)}.json`)) as SubmissionDocument,
  };
}

/**
 * A company's filings, newest first, with company metadata. `company` is a
 * ticker or CIK (numeric CIKs skip the extra ticker-map request); `forms`
 * filters form types (`10-K`, `8-K`, `S-1`, ...); `since`/`until` are
 * YYYY-MM-DD filing-date bounds (inclusive); `limit` defaults to 20. Older pages
 * of the filing history are followed automatically. Each filing carries its
 * primary document's archive `url`.
 */
export async function filings(
  args: { company: string; forms?: string[]; since?: string; until?: string; limit?: number },
  ctx: Ctx,
): Promise<CompanyFilings> {
  const limit = args.limit ?? 20;
  const cik = await resolveCik(ctx, args.company);
  const data = await getJson(ctx, `${DATA_URL}/submissions/CIK${cik10(cik)}.json`);
  const wanted = new Set((args.forms ?? []).map((form) => form.toUpperCase()));
  const found: Filing[] = [];

  const collect = (columns: Record<string, any>): boolean => {
    for (const row of rows(columns)) {
      const filed = String(row.filingDate ?? "");
      if (args.until && filed > args.until) continue;
      if (args.since && filed < args.since) return true;
      if (wanted.size && !wanted.has(String(row.form ?? "").toUpperCase())) continue;
      found.push(filingRow(cik, row));
      if (found.length >= limit) return true;
    }
    return false;
  };

  const history = data.filings ?? {};
  let done = collect(history.recent ?? {});
  for (const page of (history.files ?? []) as { name: string }[]) {
    if (done) break;
    done = collect(await getJson(ctx, `${DATA_URL}/submissions/${page.name}`));
  }

  return {
    cik,
    name: data.name ?? null,
    tickers: data.tickers ?? [],
    exchanges: data.exchanges ?? [],
    sic: data.sic ?? null,
    sic_description: data.sicDescription ?? null,
    fiscal_year_end: data.fiscalYearEnd ?? null,
    filings: found,
  };
}

/**
 * Full-text search over EDGAR filings (2001 onward). `query` takes EDGAR's
 * syntax, including quoted phrases (`"supply chain"`); `forms` filters form
 * types; `start`/`end` are YYYY-MM-DD bounds; `company` restricts to a
 * ticker or CIK; `limit` defaults to 20 (pages of 100 are fetched as needed) and
 * `offset` skips hits for paging. `total` is SEC's hit count for the query.
 */
export async function search(
  args: {
    query: string;
    forms?: string[];
    start?: string;
    end?: string;
    company?: string;
    limit?: number;
    offset?: number;
  },
  ctx: Ctx,
): Promise<{ total: number | null; offset: number; results: FilingSearchHit[] }> {
  const limit = args.limit ?? 20;
  const offset = args.offset ?? 0;
  const params: Params = { q: args.query };
  if (args.forms?.length) params.forms = args.forms.join(",");
  if (args.start || args.end) {
    params.dateRange = "custom";
    params.startdt = args.start || "2001-01-01";
    params.enddt = args.end || new Date().toISOString().slice(0, 10);
  }
  if (args.company) params.ciks = cik10(await resolveCik(ctx, args.company));

  const results: FilingSearchHit[] = [];
  let total: number | null = null;
  let position = offset;
  while (results.length < limit) {
    const data = await getJson(ctx, SEARCH_URL, { ...params, from: position });
    const hits = data.hits ?? {};
    total = hits.total?.value ?? total;
    const page = (hits.hits ?? []) as Record<string, any>[];
    for (const hit of page) results.push(searchHit(hit));
    position += page.length;
    if (page.length < SEARCH_PAGE_SIZE) break;
  }
  return { total, offset, results: results.slice(0, limit) };
}

/**
 * The documents in one filing, from the archive's `index.json`.
 * `accession` takes dashed or bare digits (`0000320193-24-000123` or
 * `000032019324000123`); `company` is a ticker or CIK. `size` is bytes (null
 * when unknown) and `url` is the archive URL.
 */
export async function files(
  args: { company: string; accession: string },
  ctx: Ctx,
): Promise<{ cik: number; accession: string; files: FilingFile[] }> {
  const cik = await resolveCik(ctx, args.company);
  const accession = normalizeAccession(args.accession);
  const data = await getJson(ctx, archiveUrl(cik, accession, "index.json"));
  const items = (data.directory?.item ?? []) as Record<string, any>[];
  return {
    cik,
    accession,
    files: items.map((item) => ({
      name: String(item.name),
      size: /^\d+$/.test(String(item.size ?? "")) ? Number(item.size) : null,
      url: archiveUrl(cik, accession, String(item.name)),
    })),
  };
}

/**
 * A filing document as text. `filename` defaults to the filing's primary
 * document; `company` is a ticker or CIK. HTML/inline-XBRL is converted to text
 * unless `raw` is true. Long filings are paged: `max_chars` (default 50000)
 * characters starting at `offset`, with `length` and `truncated` telling you
 * whether to fetch the next chunk. For financial numbers prefer `facts`/`frame`.
 */
export async function document(
  args: {
    company: string;
    accession: string;
    filename?: string;
    max_chars?: number;
    offset?: number;
    raw?: boolean;
  },
  ctx: Ctx,
): Promise<FilingDocument> {
  const cik = await resolveCik(ctx, args.company);
  const accession = normalizeAccession(args.accession);
  const filename = args.filename || (await primaryDocument(ctx, cik, accession));
  const url = archiveUrl(cik, accession, filename);
  const body = (await get(ctx, url)).text;
  const isHtml = /\.(htm|html|xml)$/i.test(filename) && !args.raw;
  const text = isHtml ? htmlToText(body) : body;
  const offset = args.offset ?? 0;
  const maxChars = args.max_chars ?? DEFAULT_MAX_CHARS;
  const chunk = text.slice(offset, offset + maxChars);
  return {
    cik,
    accession,
    filename,
    url,
    length: text.length,
    offset,
    truncated: offset + chunk.length < text.length,
    text: chunk,
  };
}

/**
 * Every XBRL concept a company reports, with its units and value count.
 * `taxonomy` keeps one taxonomy (`us-gaap`, `dei`, `ifrs-full`, ...); `grep`
 * keeps concepts whose name or label contains the substring (case-insensitive).
 */
export async function concepts(
  args: { company: string; taxonomy?: string; grep?: string },
  ctx: Ctx,
): Promise<{ cik: number; entity: string | null; concepts: XbrlConcept[] }> {
  const cik = await resolveCik(ctx, args.company);
  const data = await getJson(ctx, `${DATA_URL}/api/xbrl/companyfacts/CIK${cik10(cik)}.json`);
  const found: XbrlConcept[] = [];
  const needle = args.grep?.toLowerCase();
  for (const [taxonomy, tags] of Object.entries((data.facts ?? {}) as Record<string, any>)) {
    if (args.taxonomy && taxonomy !== args.taxonomy) continue;
    for (const [tag, body] of Object.entries((tags ?? {}) as Record<string, any>)) {
      const units = (body.units ?? {}) as Record<string, unknown[]>;
      const concept: XbrlConcept = {
        concept: `${taxonomy}:${tag}`,
        label: body.label ?? null,
        units: Object.keys(units).sort(),
        facts: Object.values(units).reduce((sum, values) => sum + (values?.length ?? 0), 0),
      };
      if (needle && !concept.concept.toLowerCase().includes(needle) && !String(concept.label ?? "").toLowerCase().includes(needle)) {
        continue;
      }
      found.push(concept);
    }
  }
  return { cik, entity: data.entityName ?? null, concepts: found };
}

/**
 * One XBRL concept's reported values for a company, newest first.
 * `concept` is `taxonomy:tag` (`us-gaap:Revenues`) or a bare tag (us-gaap).
 * `form` keeps one form type; `unit` keeps one unit (`USD`, `shares`,
 * `USD-per-shares`); `limit` caps values per unit (default 20). Values carry
 * `start`/`end` (YYYY-MM-DD), `val`, `accn`, `fy`, `fp`, `form`, `filed`, `frame`.
 */
export async function facts(
  args: { company: string; concept: string; form?: string; unit?: string; limit?: number },
  ctx: Ctx,
): Promise<{
  cik: number;
  entity: string | null;
  concept: string;
  label: string | null;
  description: string | null;
  units: Record<string, XbrlFact[]>;
}> {
  const limit = args.limit ?? 20;
  const cik = await resolveCik(ctx, args.company);
  const { taxonomy, tag } = splitConcept(args.concept);
  const data = await getJson(ctx, `${DATA_URL}/api/xbrl/companyconcept/CIK${cik10(cik)}/${taxonomy}/${tag}.json`);
  const units: Record<string, XbrlFact[]> = {};
  for (const [unitName, values] of Object.entries((data.units ?? {}) as Record<string, XbrlFact[]>)) {
    if (args.unit && unitName !== args.unit) continue;
    const kept = values.filter((value) => !args.form || String(value.form ?? "").toUpperCase() === args.form.toUpperCase());
    kept.sort((a, b) => {
      const left = `${a.end ?? ""}|${a.filed ?? ""}`;
      const right = `${b.end ?? ""}|${b.filed ?? ""}`;
      return left < right ? 1 : left > right ? -1 : 0;
    });
    units[unitName] = kept.slice(0, limit);
  }
  return {
    cik,
    entity: data.entityName ?? null,
    concept: `${taxonomy}:${tag}`,
    label: data.label ?? null,
    description: data.description ?? null,
    units,
  };
}

/**
 * One concept across all companies that reported it in one period, largest value
 * first (or smallest with `ascending`). `period` is `CY2023` (annual),
 * `CY2023Q4` (quarter) or `CY2023Q4I` (instant); `unit` defaults to USD;
 * `limit` caps companies (default 20). `companies` is the number of companies in
 * the frame before the limit.
 */
export async function frame(
  args: { concept: string; period: string; unit?: string; limit?: number; ascending?: boolean },
  ctx: Ctx,
): Promise<{ concept: string; label: string | null; unit: string; period: string; companies: number; data: XbrlFrameRow[] }> {
  const limit = args.limit ?? 20;
  const unit = args.unit ?? "USD";
  const { taxonomy, tag } = splitConcept(args.concept);
  const data = await getJson(ctx, `${DATA_URL}/api/xbrl/frames/${taxonomy}/${tag}/${unit}/${args.period}.json`);
  const all = (data.data ?? []) as XbrlFrameRow[];
  const sorted = [...all].sort((a, b) => {
    const left = Number(a.val ?? 0);
    const right = Number(b.val ?? 0);
    return args.ascending ? left - right : right - left;
  });
  return {
    concept: `${taxonomy}:${tag}`,
    label: data.label ?? null,
    unit,
    period: args.period,
    companies: all.length,
    data: sorted.slice(0, limit),
  };
}

/** A filing's primary document name, from its submissions row or the archive listing. */
async function primaryDocument(ctx: Ctx, cik: number, accession: string): Promise<string> {
  const data = await getJson(ctx, `${DATA_URL}/submissions/CIK${cik10(cik)}.json`);
  for (const row of rows(data.filings?.recent ?? {})) {
    if (row.accessionNumber === accession && row.primaryDocument) return String(row.primaryDocument);
  }
  // Older filings are not in `recent`; fall back to the largest HTML file.
  const listing = (await files({ company: String(cik), accession }, ctx)).files;
  let candidates = listing.filter(
    (item) => /\.(htm|html)$/i.test(item.name) && !item.name.startsWith(accession),
  );
  if (!candidates.length) candidates = listing.filter((item) => item.name.endsWith(".txt"));
  if (!candidates.length) throw new Error(`No document found in filing ${accession}.`);
  let best = candidates[0];
  for (const candidate of candidates) {
    if ((candidate.size ?? 0) > (best.size ?? 0)) best = candidate;
  }
  return best.name;
}
