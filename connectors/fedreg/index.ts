import type { Ctx } from "../centaur";
import { encode, readJson, type Params } from "../_lib/http";

// Results are the Federal Register API's own objects, passed through; field names
// are the API's (https://www.federalregister.gov/developers/documentation/api/v1).

const DEFAULT_BASE = "https://www.federalregister.gov/api/v1";

export interface AgencyLogo {
  small_url?: string;
  medium_url?: string;
  thumb_url?: string;
  [field: string]: unknown;
}

/** One agency, as embedded in documents and returned by the agencies endpoints. */
export interface Agency {
  id?: number;
  name?: string;
  /** Present on agencies embedded in documents. */
  raw_name?: string;
  short_name?: string;
  slug?: string;
  url?: string;
  json_url?: string;
  agency_url?: string;
  description?: string;
  recent_articles_url?: string;
  parent_id?: number | null;
  child_ids?: number[];
  child_slugs?: string[];
  logo?: AgencyLogo | string | null;
  [field: string]: unknown;
}

/** One Federal Register document, such as a rule, proposed rule or notice. */
export interface Document {
  document_number?: string;
  title?: string;
  type?: string;
  abstract?: string | null;
  publication_date?: string;
  effective_on?: string | null;
  comments_close_on?: string | null;
  agencies?: Agency[];
  citation?: string;
  start_page?: number | string;
  end_page?: number | string;
  pdf_url?: string;
  html_url?: string;
  body_html_url?: string;
  raw_text_url?: string;
  public_inspection_pdf_url?: string;
  excerpts?: string | null;
  dates?: string | null;
  action?: string | null;
  [field: string]: unknown;
}

/** A page of documents. `count` is the total number of matches. */
export interface DocumentPage {
  count: number;
  total_pages?: number;
  next_page_url?: string | null;
  results: Document[];
  [field: string]: unknown;
}

/** One document on public inspection (filed, not yet published). */
export interface InspectionDocument extends Document {
  filed_at?: string;
  num_pages?: number;
  filing_type?: string;
  docket_numbers?: string[];
  subject_1?: string;
  subject_2?: string | null;
  subject_3?: string | null;
  page_views?: number;
  [field: string]: unknown;
}

export interface InspectionPage {
  count?: number;
  results: InspectionDocument[];
  [field: string]: unknown;
}

async function request(
  ctx: Ctx,
  path: string,
  params: Params = {},
  fields: string[] = [],
): Promise<any> {
  const parts = [encode(params)];
  for (const field of fields) {
    parts.push(`${encodeURIComponent("fields[]")}=${encodeURIComponent(field)}`);
  }
  const encoded = parts.filter(Boolean).join("&");
  const url =
    (ctx.env.FEDERAL_REGISTER_API_URL || DEFAULT_BASE) + path + (encoded ? `?${encoded}` : "");
  return readJson(await ctx.fetch(url, { headers: { accept: "application/json" } }), "Federal Register");
}

/**
 * Search published documents by full text. `type` is the request code RULE, PRORULE, NOTICE or
 * PRESDOCU (results render them as "Rule", "Proposed Rule", ...); `agency` is an agency slug such
 * as `securities-and-exchange-commission`; `order` is relevance, newest or oldest; `page` is
 * 1-based and `per_page` at most 1000 (default 20). Returns the matching documents with the total
 * `count` and `next_page_url`. Results carry the API's short default field set; `fields` replaces it
 * with the upstream fields you name, such as `comments_close_on`, `comment_url`, `citation` or
 * `effective_on` (name `document_number` and `title` too if you need them).
 */
export async function search_articles(
  args: {
    term: string;
    type?: string;
    agency?: string;
    page?: number;
    per_page?: number;
    order?: "relevance" | "newest" | "oldest";
    fields?: string[];
  },
  ctx: Ctx,
): Promise<DocumentPage> {
  return request(
    ctx,
    "/articles.json",
    {
      "conditions[term]": args.term,
      "conditions[type][]": args.type,
      "conditions[agencies][]": args.agency,
      page: args.page ?? 1,
      per_page: args.per_page ?? 20,
      order: args.order ?? "relevance",
    },
    args.fields,
  );
}

/**
 * One document by Federal Register document number (for example `2023-27280`): title, abstract,
 * type, publication date, agencies, citation, page range, PDF/HTML/raw-text URLs, dates and action.
 */
export async function get_article(
  args: { document_number: string },
  ctx: Ctx,
): Promise<{ document: Document }> {
  return { document: await request(ctx, `/articles/${encodeURIComponent(args.document_number)}.json`) };
}

/** All agencies the Federal Register tracks, with slug, name, short name and description. */
export async function get_agencies(args: {}, ctx: Ctx): Promise<{ agencies: Agency[] }> {
  return { agencies: await request(ctx, "/agencies.json") };
}

/** One agency by slug, including its description, recent-articles URL and child agency IDs and slugs. */
export async function get_agency(args: { slug: string }, ctx: Ctx): Promise<{ agency: Agency }> {
  return { agency: await request(ctx, `/agencies/${encodeURIComponent(args.slug)}.json`) };
}

/**
 * Documents currently on public inspection: filed with the Office of the Federal Register but not
 * yet published. Filter by `agency` slug and/or `type` (request code RULE, PRORULE, NOTICE or
 * PRESDOCU).
 */
export async function get_public_inspection(
  args: { agency?: string; type?: string },
  ctx: Ctx,
): Promise<InspectionPage> {
  return request(ctx, "/public-inspection-documents/current.json", {
    "conditions[agencies][]": args.agency,
    "conditions[type][]": args.type,
  });
}

/**
 * Documents whose comment period is still open: those with a comment close date on or after
 * `comment_date` (YYYY-MM-DD, default today). Filter by `agency` slug; `per_page` at most 1000
 * (default 20). Results carry the API's short default field set, which omits the close date; pass
 * `fields` (for example `["document_number", "title", "comments_close_on", "comment_url"]`) to
 * replace it with the fields you need.
 */
export async function search_open_comments(
  args: { agency?: string; per_page?: number; comment_date?: string; fields?: string[] },
  ctx: Ctx,
): Promise<DocumentPage> {
  const commentDate = args.comment_date ?? new Date().toISOString().slice(0, 10);
  return request(
    ctx,
    "/articles.json",
    {
      "conditions[comment_date][gte]": commentDate,
      "conditions[agencies][]": args.agency,
      per_page: args.per_page ?? 20,
    },
    args.fields,
  );
}
