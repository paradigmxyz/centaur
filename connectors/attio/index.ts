import type { Ctx } from "../centaur";
import { query, type Params } from "../_lib/http";

// Attio's REST API answers `{ data, pagination? }` envelopes; results here are
// Attio's own objects with the envelope unwrapped, so field names and value
// shapes are Attio's. `values` maps attribute slug -> list of typed value
// objects, whose keys depend on the attribute type; the interfaces list the
// fields worth knowing and Attio sends more.

const DEFAULT_BASE = "https://api.attio.com/v2";
const JSON_RETRIES = 2;
/** Longest `Retry-After` this connector will wait out, in seconds (a call is capped at 60 s). */
const MAX_RETRY_DELAY = 15;

export interface Workspace {
  id?: string;
  name?: string;
  slug?: string;
  logo_url?: string | null;
  [field: string]: unknown;
}

/** The token's workspace and actor, from `GET /self`. */
export interface Self {
  workspace?: Workspace;
  [field: string]: unknown;
}

export interface AttioId {
  workspace_id?: string;
  object_id?: string;
  record_id?: string;
  list_id?: string;
  entry_id?: string;
  attribute_id?: string;
  note_id?: string;
  thread_id?: string;
  meeting_id?: string;
  task_id?: string;
  workspace_member_id?: string;
  [field: string]: unknown;
}

export interface AttioObject {
  id?: AttioId;
  /** Use this as `object_slug`. */
  api_slug: string;
  singular_noun?: string;
  plural_noun?: string;
  created_at?: string;
  /** True for Attio's built-in objects (people, companies, deals), false for custom ones. */
  is_system?: boolean;
  [field: string]: unknown;
}

export interface Attribute {
  id?: AttioId;
  /** Use this as the key in a filter or in `values`. */
  api_slug: string;
  title?: string;
  description?: string | null;
  /** text, number, currency, select, status, multi-select, date, timestamp, record-reference, email-address, domain, phone-number, personal-name, checkbox, rating, location, interaction, actor-reference, ... */
  type?: string;
  is_required?: boolean;
  is_unique?: boolean;
  is_multiselect?: boolean;
  is_writable?: boolean;
  is_archived?: boolean;
  config?: { [field: string]: unknown };
  created_at?: string;
  [field: string]: unknown;
}

/**
 * One value of a record attribute. Keys depend on the attribute type: `value` (text, select, number,
 * currency), `full_name`/`first_name`/`last_name` (personal-name), `email_address`, `domain`,
 * `phone_number`, `option` (status), `target_object`/`target_record_id` (record-reference),
 * `currency_value`, `original_phone_number`, ...; `active_from`/`active_until` frame its validity.
 */
export interface AttioValue {
  active_from?: string;
  active_until?: string | null;
  created_by_actor?: { type?: string; id?: string | null; [field: string]: unknown };
  [field: string]: unknown;
}

export interface AttioRecord {
  id?: AttioId;
  created_at?: string;
  web_url?: string;
  /** Attribute slug -> values, in the order Attio stores them (first is usually current). */
  values?: { [attribute_slug: string]: AttioValue[] };
  [field: string]: unknown;
}

/**
 * A record filter: attribute slug -> condition, with `$and`/`$or` for compound filters. Conditions
 * depend on the attribute type: `{ value: "exact" }` (text, domain, email), `{ gt: 10 }` / `{ lte: 5 }`
 * (number), `{ value: "option-slug" }` (select), `{ domain: "acme.com" }` (domain),
 * `{ email_address: "a@b.com" }` (email-address), `{ target_object: "companies", target_record_id: "…" }`
 * (record-reference), `{ value: true }` (checkbox), `[{ value: "a" }, { value: "b" }]` (multi-select).
 * Do not wrap it in a `filter` key; the connector does that.
 */
export interface Filter {
  [attribute_slug: string]: unknown;
}

export interface Sort {
  attribute: string;
  direction?: "asc" | "desc";
  [field: string]: unknown;
}

export interface AttioList {
  id?: AttioId;
  api_slug?: string;
  name?: string;
  /** Slugs of the object whose records this list holds. */
  parent_object?: string[];
  created_at?: string;
  [field: string]: unknown;
}

export interface Entry {
  id?: AttioId;
  parent_record_id?: string;
  /** Attribute slug -> values, like a record's `values`. */
  entry_values?: { [attribute_slug: string]: AttioValue[] };
  created_at?: string;
  [field: string]: unknown;
}

export interface Note {
  id?: AttioId;
  parent_object?: string;
  parent_record_id?: string;
  title?: string;
  format?: string;
  content_plaintext?: string | null;
  content_markdown?: string | null;
  created_at?: string;
  created_by_actor?: { type?: string; id?: string | null; [field: string]: unknown };
  [field: string]: unknown;
}

export interface Thread {
  id?: AttioId;
  title?: string | null;
  comments?: Comment[];
  created_at?: string;
  [field: string]: unknown;
}

export interface Comment {
  id?: AttioId;
  content_plaintext?: string | null;
  created_at?: string;
  author?: { type?: string; id?: string | null; [field: string]: unknown };
  [field: string]: unknown;
}

export interface Pagination {
  next_cursor?: string | null;
  has_more?: boolean;
  [field: string]: unknown;
}

export interface Meeting {
  id?: AttioId;
  title?: string;
  start_at?: string;
  end_at?: string;
  participants?: { email_address?: string; [field: string]: unknown }[];
  [field: string]: unknown;
}

export interface CallRecording {
  id?: AttioId;
  title?: string | null;
  created_at?: string;
  [field: string]: unknown;
}

/** A transcript body as Attio sends it (segments and cursor pagination), passed through. */
export interface Transcript {
  [field: string]: unknown;
}

export interface LinkedRecord {
  target_object: string;
  target_record_id: string;
  [field: string]: unknown;
}

export interface Task {
  id?: AttioId;
  content_plaintext?: string;
  content_markdown?: string | null;
  is_completed?: boolean;
  deadline_at?: string | null;
  linked_records?: LinkedRecord[];
  assignees?: { referenced_actor_type?: string; referenced_actor_id?: string; [field: string]: unknown }[];
  created_at?: string;
  [field: string]: unknown;
}

export interface WorkspaceMember {
  id?: AttioId;
  first_name?: string;
  last_name?: string;
  email_address?: string;
  access_level?: string;
  avatar_url?: string | null;
  [field: string]: unknown;
}

export interface AttioFile {
  id?: { file_id?: string; [field: string]: unknown };
  name?: string;
  [field: string]: unknown;
}

/** A whole Attio response body, envelope included, for `raw_request`. */
export interface AttioResponse {
  data?: unknown;
  pagination?: Pagination;
  [field: string]: unknown;
}

interface FormFile {
  filename: string;
  content_type: string;
  bytes: Uint8Array;
}

interface Form {
  fields: [string, string][];
  file: FormFile;
}

interface RequestOptions {
  query?: Params;
  json?: unknown;
  form?: Form;
}

/** UTF-8 bytes; `TextEncoder` exists at runtime but not in the `es2023` type lib. */
function utf8(text: string): Uint8Array {
  const out: number[] = [];
  for (let i = 0; i < text.length; i += 1) {
    let code = text.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
      const next = text.charCodeAt(i + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        code = 0x10000 + ((code - 0xd800) << 10) + (next - 0xdc00);
        i += 1;
      }
    }
    if (code < 0x80) out.push(code);
    else if (code < 0x800) out.push(0xc0 | (code >> 6), 0x80 | (code & 0x3f));
    else if (code < 0x10000) out.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
    else out.push(0xf0 | (code >> 18), 0x80 | ((code >> 12) & 0x3f), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
  }
  return new Uint8Array(out);
}

function multipart(form: Form, boundary: string): Uint8Array {
  const parts: Uint8Array[] = [];
  for (const [name, value] of form.fields) {
    parts.push(utf8(`--${boundary}\r\nContent-Disposition: form-data; name="${name}"\r\n\r\n${value}\r\n`));
  }
  parts.push(
    utf8(
      `--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="${form.file.filename}"\r\n` +
        `Content-Type: ${form.file.content_type}\r\n\r\n`,
    ),
  );
  parts.push(form.file.bytes);
  parts.push(utf8(`\r\n--${boundary}--\r\n`));
  let length = 0;
  for (const part of parts) length += part.length;
  const body = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    body.set(part, offset);
    offset += part.length;
  }
  return body;
}

/** `Retry-After` as seconds: a delta-seconds number or an HTTP date. */
function retryDelay(header: string | undefined): number {
  if (!header) return 1;
  const delta = Number(header.trim());
  let seconds = Number.isFinite(delta) ? delta : Number.NaN;
  if (!Number.isFinite(seconds)) {
    const match = /(\d{1,2})\s+([A-Za-z]{3})\s+(\d{4})\s+(\d{2}):(\d{2}):(\d{2})/.exec(header);
    const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    const month = match ? months.indexOf(match[2][0].toUpperCase() + match[2].slice(1).toLowerCase()) : -1;
    seconds =
      match && month >= 0
        ? (Date.UTC(
            Number(match[3]),
            month,
            Number(match[1]),
            Number(match[4]),
            Number(match[5]),
            Number(match[6]),
          ) -
            Date.now()) /
          1000
        : 1;
  }
  return Math.min(Math.max(seconds, 0), MAX_RETRY_DELAY);
}

function errorDetail(text: string): string {
  try {
    const body = JSON.parse(text) as { message?: unknown; code?: unknown };
    if (typeof body.message === "string" && body.message) return body.message;
    if (typeof body.code === "string" && body.code) return body.code;
  } catch {
    // Not JSON; fall through to the raw text.
  }
  return text.slice(0, 1000);
}

async function request(
  ctx: Ctx,
  method: string,
  path: string,
  options: RequestOptions = {},
): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the token is injected on the wire; locally it comes from the env.
  const key = ctx.env.ATTIO_API_KEY;
  if (key) headers.authorization = `Bearer ${key}`;
  let body: string | Uint8Array | undefined;
  if (options.json !== undefined) {
    headers["content-type"] = "application/json";
    body = JSON.stringify(options.json);
  } else if (options.form) {
    const boundary = `----centaur${Math.random().toString(36).slice(2)}${Math.random().toString(36).slice(2)}`;
    headers["content-type"] = `multipart/form-data; boundary=${boundary}`;
    body = multipart(options.form, boundary);
  }
  const url = (ctx.env.ATTIO_API_URL || DEFAULT_BASE).replace(/\/+$/, "") + path + query(options.query ?? {});

  for (let attempt = 0; ; attempt += 1) {
    const res = await ctx.fetch(url, { method, headers, body });
    if (res.status === 429 && attempt < JSON_RETRIES) {
      await ctx.sleep(retryDelay(res.headers["retry-after"]) * 1000);
      continue;
    }
    const text = await res.text();
    if (!res.ok) throw new Error(`Attio ${method} ${path} returned HTTP ${res.status}: ${errorDetail(text)}`);
    return text ? JSON.parse(text) : {};
  }
}

const seg = (value: string): string => encodeURIComponent(value.trim());

/** Attio returns the mutated record; fail loudly when it returns a different one. */
function mutated(body: any, recordId: string): AttioRecord {
  const record = (body.data ?? {}) as AttioRecord;
  const returned = record.id?.record_id;
  if (returned !== recordId) {
    throw new Error(`Attio mutation response did not confirm record ${recordId}; got ${JSON.stringify(returned)}`);
  }
  return record;
}

async function collection<T>(
  ctx: Ctx,
  method: string,
  path: string,
  options: RequestOptions = {},
): Promise<T[]> {
  const body = await request(ctx, method, path, options);
  return Array.isArray(body.data) ? (body.data as T[]) : [];
}

/**
 * The workspace and actor behind the API token. Use it as an auth/health check and to learn the
 * workspace ID that appears in every Attio object ID.
 */
export async function get_self(args: {}, ctx: Ctx): Promise<{ self: Self }> {
  return { self: ((await request(ctx, "GET", "/self")).data ?? {}) as Self };
}

/** Every object in the workspace: `api_slug` (use it as `object_slug`), singular and plural nouns, `is_system`. */
export async function list_objects(args: {}, ctx: Ctx): Promise<{ objects: AttioObject[] }> {
  return { objects: await collection<AttioObject>(ctx, "GET", "/objects") };
}

/** One object by `api_slug` (people, companies, deals, or a custom slug) or object ID. */
export async function get_object(
  args: { object_slug: string },
  ctx: Ctx,
): Promise<{ object: AttioObject }> {
  return { object: ((await request(ctx, "GET", `/objects/${seg(args.object_slug)}`)).data ?? {}) as AttioObject };
}

/**
 * Every attribute of an object: `api_slug` (the key to use in filters and `values`), title, type,
 * required/unique/multiselect flags and per-type `config` (for example the options of a select). Call
 * this before writing a record to learn required fields and value shapes.
 */
export async function list_attributes(
  args: { object_slug: string },
  ctx: Ctx,
): Promise<{ attributes: Attribute[] }> {
  return { attributes: await collection<Attribute>(ctx, "GET", `/objects/${seg(args.object_slug)}/attributes`) };
}

/**
 * One page of records of an object, with values keyed by attribute slug. `filter` uses Attio's
 * attribute-slug syntax (see Filter); `sorts` orders server-side; `limit` is at most 500 and
 * `offset` pages the rest. Prefer `query_all_records` to walk every match.
 */
export async function query_records(
  args: {
    object_slug: string;
    filter?: Filter;
    sorts?: Sort[];
    limit?: number;
    offset?: number;
  },
  ctx: Ctx,
): Promise<{ records: AttioRecord[] }> {
  const body: Record<string, unknown> = {
    limit: Math.min(Math.max(Math.trunc(args.limit ?? 25), 1), 500),
    offset: Math.max(Math.trunc(args.offset ?? 0), 0),
  };
  if (args.filter && Object.keys(args.filter).length) body.filter = args.filter;
  if (args.sorts?.length) body.sorts = args.sorts;
  return {
    records: await collection<AttioRecord>(ctx, "POST", `/objects/${seg(args.object_slug)}/records/query`, { json: body }),
  };
}

/**
 * Every record matching `filter`, walking Attio's limit/offset pagination with pages of `page_size`
 * (1-500, default 500) until a short page. Each page is a request, so narrow the filter rather than
 * pulling a whole object.
 */
export async function query_all_records(
  args: { object_slug: string; filter?: Filter; sorts?: Sort[]; page_size?: number },
  ctx: Ctx,
): Promise<{ records: AttioRecord[] }> {
  const pageSize = Math.min(Math.max(Math.trunc(args.page_size ?? 500), 1), 500);
  const records: AttioRecord[] = [];
  for (let offset = 0; ; offset += pageSize) {
    const { records: page } = await query_records(
      { object_slug: args.object_slug, filter: args.filter, sorts: args.sorts, limit: pageSize, offset },
      ctx,
    );
    records.push(...page);
    if (page.length < pageSize) return { records };
  }
}

/** One record by object slug and record ID (`id.record_id`). */
export async function get_record(
  args: { object_slug: string; record_id: string },
  ctx: Ctx,
): Promise<{ record: AttioRecord }> {
  const body = await request(
    ctx,
    "GET",
    `/objects/${seg(args.object_slug)}/records/${seg(args.record_id)}`,
  );
  return { record: (body.data ?? {}) as AttioRecord };
}

/**
 * Create a record. `values` maps attribute slug -> list of typed value objects, for example
 * `{ name: [{ first_name: "Jane", last_name: "Doe" }] }` or
 * `{ name: [{ value: "Acme Inc" }], domains: [{ domain: "acme.com" }] }`. Multi-select attributes take
 * an array of options: `{ channel_source: [{ option: "inbound" }, { option: "referral" }] }`. Run
 * `list_attributes` first to see required fields and types.
 */
export async function create_record(
  args: { object_slug: string; values: { [attribute_slug: string]: AttioValue[] } },
  ctx: Ctx,
): Promise<{ record: AttioRecord }> {
  const body = await request(ctx, "POST", `/objects/${seg(args.object_slug)}/records`, {
    json: { data: { values: args.values } },
  });
  return { record: (body.data ?? {}) as AttioRecord };
}

/**
 * Update a record, appending to multi-value attributes instead of replacing them. `values` uses the
 * same shape as `create_record`. Returns the updated record; fails if Attio does not confirm it.
 */
export async function update_record(
  args: { object_slug: string; record_id: string; values: { [attribute_slug: string]: AttioValue[] } },
  ctx: Ctx,
): Promise<{ record: AttioRecord }> {
  const body = await request(
    ctx,
    "PATCH",
    `/objects/${seg(args.object_slug)}/records/${seg(args.record_id)}`,
    { json: { data: { values: args.values } } },
  );
  return { record: mutated(body, args.record_id) };
}

/**
 * Replace the supplied attributes of a record (instead of appending), including multi-value
 * relationships; an empty list clears an attribute. `values` uses the same shape as `create_record`.
 */
export async function replace_record_values(
  args: { object_slug: string; record_id: string; values: { [attribute_slug: string]: AttioValue[] } },
  ctx: Ctx,
): Promise<{ record: AttioRecord }> {
  const body = await request(ctx, "PUT", `/objects/${seg(args.object_slug)}/records/${seg(args.record_id)}`, {
    json: { data: { values: args.values } },
  });
  return { record: mutated(body, args.record_id) };
}

/** Delete a record permanently. */
export async function delete_record(
  args: { object_slug: string; record_id: string },
  ctx: Ctx,
): Promise<{ deleted: boolean }> {
  await request(ctx, "DELETE", `/objects/${seg(args.object_slug)}/records/${seg(args.record_id)}`);
  return { deleted: true };
}

/**
 * Create or update a record, matched on one attribute (`matching_attribute`, for example
 * `email_addresses` or `domains`) whose value must be present in `values`. `values` uses the same shape
 * as `create_record`. Attio updates the record that already matches.
 */
export async function assert_record(
  args: {
    object_slug: string;
    matching_attribute: string;
    values: { [attribute_slug: string]: AttioValue[] };
  },
  ctx: Ctx,
): Promise<{ record: AttioRecord }> {
  const body = await request(ctx, "PUT", `/objects/${seg(args.object_slug)}/records`, {
    query: { matching_attribute: args.matching_attribute },
    json: { data: { values: args.values } },
  });
  return { record: (body.data ?? {}) as AttioRecord };
}

/** Every list in the workspace: `api_slug` and name, the parent object slugs it holds, timestamps. */
export async function list_lists(args: {}, ctx: Ctx): Promise<{ lists: AttioList[] }> {
  return { lists: await collection<AttioList>(ctx, "GET", "/lists") };
}

/** One list by ID or slug. */
export async function get_list(
  args: { list_id: string },
  ctx: Ctx,
): Promise<{ list: AttioList }> {
  return { list: ((await request(ctx, "GET", `/lists/${seg(args.list_id)}`)).data ?? {}) as AttioList };
}

/**
 * One page of entries in a list, each with `entry_values` (attribute slug -> values) and the parent
 * record ID. `filter` uses Attio's attribute-slug syntax; `limit` is at most 500, `offset` pages.
 */
export async function query_entries(
  args: { list_id: string; filter?: Filter; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<{ entries: Entry[] }> {
  const body: Record<string, unknown> = {
    limit: Math.min(Math.max(Math.trunc(args.limit ?? 25), 1), 500),
    offset: Math.max(Math.trunc(args.offset ?? 0), 0),
  };
  if (args.filter && Object.keys(args.filter).length) body.filter = args.filter;
  const { data } = await request(ctx, "POST", `/lists/${seg(args.list_id)}/entries/query`, { json: body });
  return { entries: Array.isArray(data) ? (data as Entry[]) : [] };
}

/**
 * Add an existing record to a list. `values` fills list-specific attributes (`entry_values`), keyed by
 * attribute slug, same shape as a record's `values`; omit it to add the record with defaults.
 */
export async function create_entry(
  args: {
    list_id: string;
    parent_record_id: string;
    values?: { [attribute_slug: string]: AttioValue[] };
  },
  ctx: Ctx,
): Promise<{ entry: Entry }> {
  const data: Record<string, unknown> = { parent_record_id: args.parent_record_id };
  if (args.values) data.entry_values = args.values;
  const body = await request(ctx, "POST", `/lists/${seg(args.list_id)}/entries`, { json: { data } });
  return { entry: (body.data ?? {}) as Entry };
}

/**
 * One page of notes on a record, newest first, with title, plaintext and markdown content, author and
 * timestamp. `limit` is at most 50; `offset` pages. Use `list_all_notes` to walk every page.
 */
export async function list_notes(
  args: { parent_object: string; parent_record_id: string; limit?: number; offset?: number },
  ctx: Ctx,
): Promise<{ notes: Note[] }> {
  return {
    notes: await collection<Note>(ctx, "GET", "/notes", {
      query: {
        parent_object: args.parent_object,
        parent_record_id: args.parent_record_id,
        limit: Math.min(Math.max(Math.trunc(args.limit ?? 50), 1), 50),
        offset: Math.max(Math.trunc(args.offset ?? 0), 0),
      },
    }),
  };
}

/** Every note on a record, walking 50-object pages (`page_size` may be 1-50). */
export async function list_all_notes(
  args: { parent_object: string; parent_record_id: string; page_size?: number },
  ctx: Ctx,
): Promise<{ notes: Note[] }> {
  const pageSize = Math.min(Math.max(Math.trunc(args.page_size ?? 50), 1), 50);
  const notes: Note[] = [];
  for (let offset = 0; ; offset += pageSize) {
    const { notes: page } = await list_notes(
      { parent_object: args.parent_object, parent_record_id: args.parent_record_id, limit: pageSize, offset },
      ctx,
    );
    notes.push(...page);
    if (page.length < pageSize) return { notes };
  }
}

/** Write: add a plaintext note to a record. Returns the created note with its ID. */
export async function create_note(
  args: { parent_object: string; parent_record_id: string; title: string; content: string },
  ctx: Ctx,
): Promise<{ note: Note }> {
  const body = await request(ctx, "POST", "/notes", {
    json: {
      data: {
        parent_object: args.parent_object,
        parent_record_id: args.parent_record_id,
        title: args.title,
        format: "plaintext",
        content: args.content,
      },
    },
  });
  return { note: (body.data ?? {}) as Note };
}

/**
 * Comment threads on a record or list entry, narrowed by the identifiers given (`object_slug` +
 * `record_id`, or `list_id` + `entry_id`). `limit` at most 50, `offset` pages. Read one with
 * `get_thread`.
 */
export async function list_threads(
  args: {
    object_slug?: string;
    record_id?: string;
    list_id?: string;
    entry_id?: string;
    limit?: number;
    offset?: number;
  },
  ctx: Ctx,
): Promise<{ threads: Thread[] }> {
  return {
    threads: await collection<Thread>(ctx, "GET", "/threads", {
      query: {
        object: args.object_slug,
        record_id: args.record_id,
        list: args.list_id,
        entry_id: args.entry_id,
        limit: Math.min(Math.max(Math.trunc(args.limit ?? 10), 1), 50),
        offset: Math.max(Math.trunc(args.offset ?? 0), 0),
      },
    }),
  };
}

/** One comment thread with its comments (author, plaintext content, timestamps). */
export async function get_thread(
  args: { thread_id: string },
  ctx: Ctx,
): Promise<{ thread: Thread }> {
  return {
    thread: ((await request(ctx, "GET", `/threads/${seg(args.thread_id)}`)).data ?? {}) as Thread,
  };
}

/**
 * Meetings Attio recorded or synced, one cursor page, optionally for one linked record
 * (`linked_object` + `linked_record_id`), by participant email or record ID, and by time window:
 * `ends_from` (ISO time) and `starts_before` (ISO time), interpreted in `timezone`. Pass
 * `pagination.next_cursor` back as `cursor` while it is set.
 */
export async function list_meetings(
  args: {
    limit?: number;
    cursor?: string;
    linked_object?: string;
    linked_record_id?: string;
    participants?: string[];
    sort?: string;
    ends_from?: string;
    starts_before?: string;
    timezone?: string;
  },
  ctx: Ctx,
): Promise<{ meetings: Meeting[]; pagination?: Pagination }> {
  const body = await request(ctx, "GET", "/meetings", {
    query: {
      limit: Math.min(Math.max(Math.trunc(args.limit ?? 50), 1), 50),
      cursor: args.cursor,
      linked_object: args.linked_object,
      linked_record_id: args.linked_record_id,
      participants: args.participants?.length ? args.participants.join(",") : undefined,
      sort: args.sort,
      ends_from: args.ends_from,
      starts_before: args.starts_before,
      timezone: args.timezone,
    },
  });
  return {
    meetings: Array.isArray(body.data) ? (body.data as Meeting[]) : [],
    pagination: body.pagination,
  };
}

/** One meeting by ID: title, start and end time, participants. */
export async function get_meeting(
  args: { meeting_id: string },
  ctx: Ctx,
): Promise<{ meeting: Meeting }> {
  return {
    meeting: ((await request(ctx, "GET", `/meetings/${seg(args.meeting_id)}`)).data ?? {}) as Meeting,
  };
}

/** Call recordings of a meeting, one cursor page (`limit` at most 50). */
export async function list_call_recordings(
  args: { meeting_id: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ call_recordings: CallRecording[]; pagination?: Pagination }> {
  const body = await request(ctx, "GET", `/meetings/${seg(args.meeting_id)}/call_recordings`, {
    query: { limit: Math.min(Math.max(Math.trunc(args.limit ?? 50), 1), 50), cursor: args.cursor },
  });
  return {
    call_recordings: Array.isArray(body.data) ? (body.data as CallRecording[]) : [],
    pagination: body.pagination,
  };
}

/**
 * The transcript of one call recording, as Attio sends it: `transcript` holds the segment list and any
 * cursor pagination. Pass `cursor` to continue.
 */
export async function get_call_transcript(
  args: { meeting_id: string; call_recording_id: string; cursor?: string },
  ctx: Ctx,
): Promise<{ transcript: Transcript }> {
  const body = await request(
    ctx,
    "GET",
    `/meetings/${seg(args.meeting_id)}/call_recordings/${seg(args.call_recording_id)}/transcript`,
    { query: { cursor: args.cursor } },
  );
  return { transcript: (body.data ?? body) as Transcript };
}

/**
 * Tasks, one page, optionally filtered by linked record (`linked_object` + `linked_record_id`) or
 * completion. Each has content, `is_completed`, `deadline_at`, assignees and linked records.
 */
export async function list_tasks(
  args: {
    linked_object?: string;
    linked_record_id?: string;
    is_completed?: boolean;
    limit?: number;
  },
  ctx: Ctx,
): Promise<{ tasks: Task[] }> {
  const params: Params = {
    limit: Math.min(Math.max(Math.trunc(args.limit ?? 25), 1), 500),
    linked_object: args.linked_object,
    linked_record_id: args.linked_record_id,
  };
  if (args.is_completed !== undefined) params.is_completed = String(args.is_completed);
  return { tasks: await collection<Task>(ctx, "GET", "/tasks", { query: params }) };
}

/**
 * Write: create a plaintext task. `deadline` is an ISO timestamp, `assignees` are workspace member IDs,
 * `linked_records` are `{ target_object, target_record_id }` references.
 */
export async function create_task(
  args: {
    content: string;
    deadline?: string;
    assignees?: string[];
    linked_records?: LinkedRecord[];
  },
  ctx: Ctx,
): Promise<{ task: Task }> {
  const data: Record<string, unknown> = { content: args.content, format: "plaintext" };
  if (args.deadline) data.deadline_at = args.deadline;
  if (args.assignees?.length) data.assignees = args.assignees.map((id) => ({ workspace_member_id: id }));
  if (args.linked_records?.length) data.linked_records = args.linked_records;
  const body = await request(ctx, "POST", "/tasks", { json: { data } });
  return { task: (body.data ?? {}) as Task };
}

/**
 * Update a task: completion, deadline, assignees (replaces them) or linked records. At least one field
 * is required.
 */
export async function update_task(
  args: {
    task_id: string;
    is_completed?: boolean;
    deadline?: string;
    assignees?: string[];
    linked_records?: LinkedRecord[];
  },
  ctx: Ctx,
): Promise<{ task: Task }> {
  const updates: Record<string, unknown> = {};
  if (args.is_completed !== undefined) updates.is_completed = args.is_completed;
  if (args.deadline !== undefined) updates.deadline_at = args.deadline;
  if (args.assignees !== undefined) {
    updates.assignees = args.assignees.map((id) => ({
      referenced_actor_type: "workspace-member",
      referenced_actor_id: id,
    }));
  }
  if (args.linked_records !== undefined) updates.linked_records = args.linked_records;
  if (!Object.keys(updates).length) throw new Error("At least one task field must be provided");
  const body = await request(ctx, "PATCH", `/tasks/${seg(args.task_id)}`, { json: { data: updates } });
  return { task: (body.data ?? {}) as Task };
}

/** Delete a task permanently. Pass `assignees: []` to `update_task` instead to just unassign it. */
export async function delete_task(args: { task_id: string }, ctx: Ctx): Promise<{ deleted: boolean }> {
  await request(ctx, "DELETE", `/tasks/${seg(args.task_id)}`);
  return { deleted: true };
}

/** Workspace members: name, email, `access_level` and member ID (the ID tasks and meetings reference). */
export async function list_workspace_members(args: {}, ctx: Ctx): Promise<{ members: WorkspaceMember[] }> {
  return { members: await collection<WorkspaceMember>(ctx, "GET", "/workspace_members") };
}

/**
 * Write: attach a file to a record by sending its bytes. Pass the content base64-encoded in
 * `content_base64` (there is no local filesystem here), plus the filename to show in Attio and its MIME
 * type (`content_type`, default `application/octet-stream`). Attio accepts files up to 50 MB.
 * `parent_folder_id` files it in an Attio folder.
 */
export async function upload_file(
  args: {
    object_slug: string;
    record_id: string;
    filename: string;
    content_base64: string;
    content_type?: string;
    parent_folder_id?: string;
  },
  ctx: Ctx,
): Promise<{ file: AttioFile }> {
  const bytes = ctx.base64.decode(args.content_base64);
  if (!bytes.length) throw new Error("content_base64 decoded to no bytes");
  const fields: [string, string][] = [
    ["object", args.object_slug],
    ["record_id", args.record_id],
  ];
  if (args.parent_folder_id) fields.push(["parent_folder_id", args.parent_folder_id]);
  const body = await request(ctx, "POST", "/files/upload", {
    form: {
      fields,
      file: {
        filename: args.filename,
        content_type: args.content_type || "application/octet-stream",
        bytes,
      },
    },
  });
  return { file: (body.data ?? {}) as AttioFile };
}

/**
 * Escape hatch for Attio v2 endpoints without a dedicated function. `endpoint` is a path relative to
 * `/v2` (`/webhooks`) or absolute (`/v2/webhooks`); `method` is uppercased; `json` is sent as the
 * request body and `params` as the query string. Returns the whole response body, envelope included.
 */
export async function raw_request(
  args: { method: string; endpoint: string; json?: unknown; params?: Params },
  ctx: Ctx,
): Promise<{ response: AttioResponse }> {
  const endpoint = (args.endpoint.startsWith("/") ? args.endpoint : `/${args.endpoint}`).replace(/^\/v2/, "");
  const response = await request(ctx, args.method.toUpperCase(), endpoint || "/", {
    query: args.params,
    json: args.json,
  });
  return { response: response as AttioResponse };
}
