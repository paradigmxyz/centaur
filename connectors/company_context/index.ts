import type { Ctx } from "../centaur";
import { query } from "../_lib/http";
import { companyContextRequest as request, rfc3339 } from "../_lib/company_context";

// The company-context service's API (`POST /query`, `GET /documents/{id}`, and the custom
// documents endpoints). iron-proxy injects the principal's API JWT into requests to the service,
// so search results are limited to documents the calling principal can see, and custom document
// changes to the documents it created. Responses are passed through as the API sends them.

/** A data type the query API searches. */
export type DocumentType =
  | "slack_message"
  | "slack_file"
  | "drive_doc"
  | "calendar_event"
  | "granola_note"
  | "custom_doc";

/** A search result, or a document read by id (which has no `score`). */
export interface Document {
  /** Pass to `read_document`. */
  document_id: string;
  type: DocumentType;
  title: string | null;
  url: string | null;
  /** The full document chunk. */
  text: string;
  occurred_at: string | null;
  /** Fused rank score, comparable only within one hybrid response; absent in keyword mode. */
  score?: number;
  /**
   * Type-specific fields: `conversation_id` and `channel_name` for Slack messages, file ID and
   * type for files, calendar and event IDs for events, owner and attendees for Granola notes,
   * and the custom metadata plus `source`, `external_id` and `written_by` for custom documents.
   */
  metadata: Record<string, unknown>;
  [field: string]: unknown;
}

export interface SearchResponse {
  results: Document[];
  /** Present only in a keyword response with more results; pass back as `cursor`. */
  next_cursor?: string | null;
  [field: string]: unknown;
}

/** A custom document as the custom documents API returns it. */
export interface CustomDocument {
  source: string;
  /** The service-assigned id (a UUIDv7) used to update or delete the document. */
  external_id: string;
  title: string | null;
  body: string;
  occurred_at: string | null;
  readers: string[];
  metadata: Record<string, unknown>;
  /** The principal (`prn_...`) that created the document. */
  written_by: string | null;
  created_at: string;
  updated_at: string;
  /** `pending` until searchable, then `projected`, or `rejected` with `error`. */
  status: string;
  error: string | null;
  [field: string]: unknown;
}

/** Fields of a custom document a principal writes. */
export interface CustomDocumentInput {
  /** Custom document source (collection), such as `memory`: 1-64 lowercase letters, digits or hyphens. */
  source: string;
  /** Document text; required and not blank. Searched together with `title`. */
  body: string;
  title?: string;
  /** RFC 3339 time the content occurred, used by search time filters. */
  occurred_at?: string;
  /**
   * Who can read it: `public`, `principal:<your id>`, or `role:<one of your roles>`.
   * Defaults to the writer alone; an empty list hides the document from everyone.
   */
  readers?: string[];
  /** JSON object returned with the document and matched by the search `metadata` filter. */
  metadata?: Record<string, unknown>;
}

function required(value: string, name: string): string {
  const trimmed = (value ?? "").trim();
  if (!trimmed) throw new Error(`${name} cannot be empty`);
  return trimmed;
}

function documentsPath(source: string, externalId?: string): string {
  let path = `/custom-documents/${encodeURIComponent(required(source, "source"))}`;
  if (externalId !== undefined) path += `/${encodeURIComponent(required(externalId, "external_id"))}`;
  return path;
}

function documentBody(args: CustomDocumentInput): Record<string, unknown> {
  const document: Record<string, unknown> = { body: args.body };
  if (args.title) document.title = args.title;
  if (args.occurred_at) document.occurred_at = args.occurred_at;
  if (args.metadata && Object.keys(args.metadata).length > 0) document.metadata = args.metadata;
  // An empty list hides the document from everyone, so keep it.
  if (args.readers !== undefined) document.readers = args.readers;
  return document;
}

/**
 * Search indexed company history visible to the caller: Slack channel-day transcripts and
 * files, Google Docs and PDFs, Calendar events, Granola notes, and custom documents (archives,
 * notes, memory). Filters combine with AND; `channel_ids` applies only to Slack types,
 * `file_ids` to Slack and Drive files, and `sources`/`metadata` only to custom documents, so a
 * request whose `types` cannot satisfy every filter is rejected.
 *
 * Default (hybrid) mode ranks by keyword and embedding similarity and is not paged; narrow it
 * with filters. `keyword: true` returns every document containing all query terms, newest
 * first and unranked; pass `next_cursor` back as `cursor` (with the same query and filters) for
 * the next page. Use keyword mode with `channel_ids` and a time window to enumerate a channel's
 * history of a topic, and for exact identifiers or phrases.
 */
export async function search(
  args: {
    query: string;
    /** Max results; API default 10, at most 50. */
    limit?: number;
    /** Data types to search; defaults to every type. */
    types?: DocumentType[];
    /** Inclusive lower bound: RFC 3339 time, or YYYY-MM-DD (midnight UTC). */
    occurred_after?: string;
    /** Exclusive upper bound: RFC 3339 time, or YYYY-MM-DD (midnight UTC). */
    occurred_before?: string;
    /** Slack conversation IDs, at most 100. */
    channel_ids?: string[];
    /** Slack or Drive file IDs, at most 100. */
    file_ids?: string[];
    /** Custom document sources, at most 100. */
    sources?: string[];
    /** Only custom documents whose metadata contains this object (exact, case-sensitive). */
    metadata?: Record<string, unknown>;
    keyword?: boolean;
    /** `next_cursor` from the previous keyword page. */
    cursor?: string;
  },
  ctx: Ctx,
): Promise<SearchResponse> {
  const filters: Record<string, unknown> = {};
  const candidates: Record<string, unknown> = {
    types: args.types,
    occurred_after: rfc3339(args.occurred_after),
    occurred_before: rfc3339(args.occurred_before),
    channel_ids: args.channel_ids,
    file_ids: args.file_ids,
    sources: args.sources,
    metadata: args.metadata,
  };
  for (const [name, value] of Object.entries(candidates)) {
    if (value === undefined || value === null || value === "") continue;
    if (Array.isArray(value) && value.length === 0) continue;
    if (typeof value === "object" && !Array.isArray(value) && Object.keys(value).length === 0) continue;
    filters[name] = value;
  }
  const body: Record<string, unknown> = { query: required(args.query, "query"), filters };
  if (args.keyword) body.mode = "keyword";
  if (args.limit !== undefined) body.limit = args.limit;
  if (args.cursor) body.cursor = args.cursor;
  return request(ctx, "POST", "/query", body);
}

/** Read one visible document by a `document_id` from `search`, in the same shape without `score`. */
export async function read_document(args: { document_id: string }, ctx: Ctx): Promise<Document> {
  return request(ctx, "GET", `/documents/${encodeURIComponent(required(args.document_id, "document_id"))}`);
}

/**
 * Create a custom document in `source`, readable only by the caller unless `readers` is set.
 * Returns it with its new `external_id`; it is searchable once `status` is `projected`,
 * usually within seconds. In a channel session the caller is the channel's principal.
 */
export async function create_document(args: CustomDocumentInput, ctx: Ctx): Promise<CustomDocument> {
  return request(ctx, "POST", documentsPath(args.source), documentBody(args));
}

/** Replace all of a custom document the caller created; omitted fields are cleared or reset. */
export async function update_document(
  args: CustomDocumentInput & { external_id: string },
  ctx: Ctx,
): Promise<CustomDocument> {
  return request(ctx, "PUT", documentsPath(args.source, args.external_id), documentBody(args));
}

/** Delete a custom document the caller created, and its searchable copies, immediately. */
export async function delete_document(
  args: { source: string; external_id: string },
  ctx: Ctx,
): Promise<{ deleted: true; source: string; external_id: string }> {
  await request(ctx, "DELETE", documentsPath(args.source, args.external_id));
  return { deleted: true, source: args.source, external_id: args.external_id };
}

/** List the custom documents the caller created in `source`, newest first. */
export async function list_documents(
  args: {
    source: string;
    /** Max documents; API default 20, at most 100. */
    limit?: number;
    /** Only documents created before this `external_id`, to page back. */
    before?: string;
  },
  ctx: Ctx,
): Promise<{ documents: CustomDocument[] }> {
  return request(ctx, "GET", `${documentsPath(args.source)}${query({ limit: args.limit, before: args.before })}`);
}
