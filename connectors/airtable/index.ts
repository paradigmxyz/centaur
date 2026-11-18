import type { Ctx } from "../centaur";
import { query } from "../_lib/http";

// Results are Airtable's own objects, passed through. Reads unwrap only the
// `{ records, offset }` / `{ tables }` envelopes; nothing is renamed or
// simplified. Field names and value shapes are Airtable's
// (https://airtable.com/developers/web/api/introduction).

const BASE_URL = "https://api.airtable.com/v0";
const META_URL = `${BASE_URL}/meta`;
const URL_MESSAGE =
  "Airtable URL must use airtable.com and include an app/base ID and table ID, e.g. " +
  "https://airtable.com/app.../tbl.../viw...";

/** A record as Airtable sends it: `fields` holds the table's field names and cell values. */
export interface AirtableRecord {
  id: string;
  createdTime?: string;
  fields: { [field: string]: unknown };
  [field: string]: unknown;
}

/** A base. `permissionLevel` is the token's grant: none, read, comment, edit or create. */
export interface AirtableBase {
  id: string;
  name: string;
  permissionLevel?: string;
  [field: string]: unknown;
}

/** A schema field. `options` is Airtable's per-type configuration (choices, formula, precision, ...). */
export interface AirtableField {
  id?: string;
  name: string;
  type: string;
  description?: string;
  options?: unknown;
  [field: string]: unknown;
}

/** A table view. `type` is grid, form, calendar, gallery, kanban, timeline, block or list. */
export interface AirtableView {
  id: string;
  name: string;
  type?: string;
  [field: string]: unknown;
}

/** A table with its fields and views. */
export interface AirtableTable {
  id: string;
  name: string;
  description?: string;
  primaryFieldId?: string;
  fields: AirtableField[];
  views: AirtableView[];
  [field: string]: unknown;
}

/** `GET /v0/meta/whoami`: the token's owner. */
export interface AirtableWhoami {
  id: string;
  scopes?: string[];
  [field: string]: unknown;
}

/** The result of `DELETE` on a record. */
export interface DeletedRecord {
  id: string;
  deleted: boolean;
  [field: string]: unknown;
}

/** An Airtable URL split into IDs; each is `null` when absent from the URL. */
export interface ParsedUrl {
  url: string;
  host: string;
  base_id: string | null;
  table_id: string | null;
  view_id: string | null;
  page_id: string | null;
  record_id: string | null;
}

/** Privacy-minimized token owner: presence, ID and scope count, never the email. */
export interface PreflightIdentity {
  identity_present: boolean;
  id?: string | null;
  scopes_count?: number | null;
}

/** One check (`auth` or `probe`) of `preflight_access`. */
export interface PreflightStep {
  attempted: boolean;
  ok: boolean;
  /** not_run, ok, invalid_token, auth_error, missing_base_scope, bad_url, bad_target, probe_error. */
  status: string;
  type?: string;
  identity?: PreflightIdentity;
  http_status?: number;
  error_type?: string | null;
  error_message?: string | null;
  details?: { [field: string]: unknown };
}

/** `preflight_access`'s verdict. `status` is ok, bad_url, bad_target, invalid_token, auth_error, missing_base_scope or probe_error. */
export interface PreflightReport {
  ok: boolean;
  status: string;
  message: string;
  target: { [field: string]: unknown };
  auth: PreflightStep;
  probe: PreflightStep;
}

type QueryValue = string | number | boolean | string[] | undefined;

/** `?a=1&b=2`, repeating the key for array values; skips undefined and empty values. */
function qs(pairs: [string, QueryValue][]): string {
  const parts: string[] = [];
  for (const [key, value] of pairs) {
    if (value === undefined || value === "") continue;
    for (const item of Array.isArray(value) ? value : [value]) {
      if (item === "") continue;
      parts.push(`${encodeURIComponent(key)}=${encodeURIComponent(String(item))}`);
    }
  }
  return parts.length ? `?${parts.join("&")}` : "";
}

const seg = (value: string): string => encodeURIComponent(value.trim());

interface Reply {
  status: number;
  ok: boolean;
  data: any;
  text: string;
}

async function send(ctx: Ctx, method: string, url: string, body?: unknown): Promise<Reply> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected on the wire; locally it comes from the env.
  if (ctx.env.AIRTABLE_API_KEY) headers.authorization = `Bearer ${ctx.env.AIRTABLE_API_KEY}`;
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await ctx.fetch(url, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let data: any = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
  }
  return { status: res.status, ok: res.ok, data, text };
}

/** Airtable errors are `{ error: { type, message } }`, or `{ error, error_description }` on 401. */
function errorParts(reply: Reply): { type: string | null; message: string | null } {
  const error = reply.data?.error;
  if (typeof error === "string") {
    const description = reply.data?.error_description;
    return { type: error, message: typeof description === "string" ? description : null };
  }
  if (error && typeof error === "object") {
    return {
      type: typeof error.type === "string" ? error.type : null,
      message: typeof error.message === "string" ? error.message : null,
    };
  }
  return { type: null, message: null };
}

function failure(label: string, reply: Reply): Error {
  const { type, message } = errorParts(reply);
  const detail = message ?? (reply.text ? reply.text.slice(0, 300) : "no response body");
  const code = type ? ` ${type}` : "";
  const hint =
    reply.status === 401
      ? " (AIRTABLE_API_KEY is missing or invalid)"
      : reply.status === 403
        ? " (the token lacks access to this base, table or scope)"
        : reply.status === 404
          ? " (base, table, view or record not found)"
          : "";
  return new Error(`Airtable ${label} failed (HTTP ${reply.status}${code}): ${detail}${hint}`);
}

async function request(ctx: Ctx, label: string, method: string, url: string, body?: unknown): Promise<any> {
  const reply = await send(ctx, method, url, body);
  if (!reply.ok) throw failure(label, reply);
  return reply.data;
}

const recordsPath = (base_id: string, table: string): string =>
  `${BASE_URL}/${seg(base_id)}/${seg(table)}`;

const tablesPath = (base_id: string): string => `${META_URL}/bases/${seg(base_id)}/tables`;

/** Split an Airtable URL into its IDs (no network call). */
function parseUrl(url: string): ParsedUrl {
  const match = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/([^/?#]*)([^?#]*)/.exec(url.trim());
  const found: ParsedUrl = {
    url,
    host: match ? match[1] : "",
    base_id: null,
    table_id: null,
    view_id: null,
    page_id: null,
    record_id: null,
  };
  const path = match ? match[2] : url.split(/[?#]/)[0];
  for (const part of path.split("/")) {
    if (!part) continue;
    if (part.startsWith("app")) found.base_id = part;
    else if (part.startsWith("tbl")) found.table_id = part;
    else if (part.startsWith("viw")) found.view_id = part;
    else if (part.startsWith("pag")) found.page_id = part;
    else if (part.startsWith("rec")) found.record_id = part;
  }
  return found;
}

const isAirtableHost = (host: string): boolean => {
  const name = host.split(":", 1)[0].toLowerCase();
  return name === "airtable.com" || name === "www.airtable.com";
};

/** Resolve a table/view URL to its base and table IDs, or throw. */
function targetFromUrl(url: string): { base_id: string; table_id: string; view_id: string | null; parsed: ParsedUrl } {
  const parsed = parseUrl(url);
  if (!isAirtableHost(parsed.host) || !parsed.base_id || !parsed.table_id) throw new Error(URL_MESSAGE);
  return { base_id: parsed.base_id, table_id: parsed.table_id, view_id: parsed.view_id, parsed };
}

interface ListArgs {
  base_id: string;
  table: string;
  view?: string;
  max_records?: number;
  fields?: string[];
  filter_by_formula?: string;
}

async function listRecords(
  ctx: Ctx,
  args: ListArgs,
): Promise<{
  base_id: string;
  table: string;
  view: string | null;
  count: number;
  records: AirtableRecord[];
  has_more: boolean;
}> {
  const max = Math.max(1, Math.min(args.max_records ?? 100, 1000));
  const params: [string, QueryValue][] = [["pageSize", Math.min(max, 100)]];
  if (args.view) params.push(["view", args.view]);
  if (args.filter_by_formula) params.push(["filterByFormula", args.filter_by_formula]);
  if (args.fields?.length) params.push(["fields[]", args.fields]);

  const records: AirtableRecord[] = [];
  let offset: string | undefined;
  while (records.length < max) {
    const page = await request(
      ctx,
      "list records",
      "GET",
      `${recordsPath(args.base_id, args.table)}${qs(offset ? [...params, ["offset", offset]] : params)}`,
    );
    for (const record of (page?.records ?? []) as AirtableRecord[]) records.push(record);
    offset = page?.offset;
    if (!offset) break;
  }
  return {
    base_id: args.base_id,
    table: args.table,
    view: args.view ?? null,
    count: Math.min(records.length, max),
    records: records.slice(0, max),
    has_more: Boolean(offset),
  };
}

/** Search a cell value (string, number, list, object) for `needle`, already lowercased. */
function matches(value: unknown, needle: string): boolean {
  if (value === null || value === undefined) return false;
  if (Array.isArray(value)) return value.some((item) => matches(item, needle));
  if (typeof value === "object") {
    return Object.values(value as { [key: string]: unknown }).some((item) => matches(item, needle));
  }
  return String(value).toLowerCase().includes(needle);
}

/** Records may be a field mapping or `{ fields }`; normalize to `{ fields }`. */
function normalizeCreateRecords(records: { fields?: unknown; [field: string]: unknown }[]): { fields: { [field: string]: unknown } }[] {
  return records.map((record) => {
    const fields = record && "fields" in record ? record.fields : record;
    if (!fields || typeof fields !== "object" || Array.isArray(fields)) {
      throw new Error("Each record must be a field mapping or an object with a 'fields' mapping.");
    }
    return { fields: fields as { [field: string]: unknown } };
  });
}

/** Updates must be `{ id, fields }`. */
function normalizeUpdateRecords(
  records: { id?: unknown; fields?: unknown; [field: string]: unknown }[],
): { id: string; fields: { [field: string]: unknown } }[] {
  return records.map((record) => {
    const id = record?.id;
    const fields = record?.fields;
    if (typeof id !== "string" || !id) throw new Error("Each record update must include a non-empty 'id'.");
    if (!fields || typeof fields !== "object" || Array.isArray(fields)) {
      throw new Error("Each record update must include a 'fields' mapping.");
    }
    return { id, fields: fields as { [field: string]: unknown } };
  });
}

const batch = <T>(items: T[], size = 10): T[][] => {
  const groups: T[][] = [];
  for (let index = 0; index < items.length; index += size) groups.push(items.slice(index, index + size));
  return groups;
};

/** The token's identity: user ID and granted scopes (`schema.bases:read`, `data.records:write`, ...). */
export async function whoami(args: {}, ctx: Ctx): Promise<{ user: AirtableWhoami }> {
  return { user: await request(ctx, "whoami", "GET", `${META_URL}/whoami`) };
}

/**
 * Read-only auth check: the authenticated user ID, or a thrown error naming the vendor failure when
 * the token is missing or invalid. Useful as the first call before a larger fetch.
 */
export async function health(args: {}, ctx: Ctx): Promise<{ current_user: { id: string | null } }> {
  const user = await request(ctx, "whoami", "GET", `${META_URL}/whoami`);
  return { current_user: { id: user?.id ?? null } };
}

/**
 * Split an Airtable URL into base (app…), table (tbl…), view (viw…), page (pag…) and record (rec…)
 * IDs. No network call; returns `host` and nulls for the parts the URL does not carry.
 */
export async function parse_url(args: { url: string }, ctx: Ctx): Promise<ParsedUrl> {
  return parseUrl(args.url);
}

/**
 * Diagnose whether the token can read a base, table or view before a larger fetch: an identity check
 * (`/meta/whoami`) plus one cheap probe request, reported as `auth` and `probe` steps instead of
 * thrown errors. Pass either an Airtable table/view URL or `base_id` (+ optional `table` ID or name
 * and `view`), never both. Returns `status` ok, bad_url, bad_target, invalid_token, auth_error,
 * missing_base_scope or probe_error; `auth.identity` omits the email.
 */
export async function preflight_access(
  args: { url?: string; base_id?: string; table?: string; view?: string },
  ctx: Ctx,
): Promise<PreflightReport> {
  const notRun: PreflightStep = { attempted: false, ok: false, status: "not_run" };
  const refuse = (status: string, message: string, target: { [field: string]: unknown }): PreflightReport => ({
    ok: false,
    status,
    message,
    target,
    auth: { ...notRun },
    probe: { ...notRun },
  });

  if (args.url && (args.base_id || args.table || args.view)) {
    return refuse("bad_target", "Provide either an Airtable URL or base/table arguments, not both.", {
      url: args.url,
      base_id: args.base_id ?? null,
      table: args.table ?? null,
      view: args.view ?? null,
    });
  }

  let base_id = args.base_id;
  let table = args.table;
  let view = args.view;
  let parsed: ParsedUrl | null = null;
  if (args.url) {
    const target = (() => {
      try {
        return targetFromUrl(args.url!);
      } catch {
        return null;
      }
    })();
    if (!target) {
      return refuse("bad_url", URL_MESSAGE, { mode: "url", url: args.url, parsed: parseUrl(args.url) });
    }
    parsed = target.parsed;
    base_id = target.base_id;
    table = target.table_id;
    view = target.view_id ?? undefined;
  } else if (!base_id) {
    return refuse("bad_target", "Provide an Airtable URL or at least a base_id for preflight access checks.", {
      url: null,
      base_id: base_id ?? null,
      table: table ?? null,
      view: view ?? null,
    });
  }

  const target = {
    mode: args.url ? "url" : "ids",
    url: args.url ?? null,
    base_id: base_id ?? null,
    table: table ?? null,
    view: view ?? null,
    parsed,
  };

  const identity = (data: any): PreflightIdentity => ({
    identity_present: Boolean(data) && typeof data === "object",
    id: data?.id ?? null,
    scopes_count: Array.isArray(data?.scopes) ? data.scopes.length : null,
  });

  const auth = await send(ctx, "GET", `${META_URL}/whoami`);
  const authError = errorParts(auth);
  if (auth.status === 401) {
    return {
      ok: false,
      status: "invalid_token",
      message: "Airtable rejected AIRTABLE_API_KEY during the identity check.",
      target,
      auth: { attempted: true, ok: false, status: "invalid_token", error_type: authError.type, error_message: authError.message },
      probe: { ...notRun },
    };
  }
  if (!auth.ok) {
    return {
      ok: false,
      status: "auth_error",
      message: authError.message ?? "Airtable identity check failed.",
      target,
      auth: {
        attempted: true,
        ok: false,
        status: "auth_error",
        http_status: auth.status,
        error_type: authError.type,
        error_message: authError.message,
      },
      probe: { ...notRun },
    };
  }
  const whoamiData = auth.data;

  const probe = table
    ? await send(ctx, "GET", `${recordsPath(base_id!, table)}${query({ pageSize: 1, view })}`)
    : await send(ctx, "GET", tablesPath(base_id!));
  const probeError = errorParts(probe);
  const probeBase = {
    attempted: true,
    ok: probe.ok,
    type: table ? "records" : "schema",
    http_status: probe.status,
    error_type: probeError.type,
    error_message: probeError.message,
  };
  const authOk: PreflightStep = { attempted: true, ok: true, status: "ok", identity: identity(whoamiData) };
  const failed = (status: string, message: string): PreflightReport => ({
    ok: false,
    status,
    message,
    target,
    auth: authOk,
    probe: { ...probeBase, ok: false, status },
  });

  if (probe.status === 403) {
    return failed(
      "missing_base_scope",
      "Airtable auth succeeded, but the token cannot read the requested base or table.",
    );
  }
  if (probe.status === 400 || probe.status === 404 || probe.status === 422) {
    return failed(
      args.url ? "bad_url" : "bad_target",
      probeError.message ?? "Airtable could not resolve the requested base, table, or view.",
    );
  }
  if (probe.status === 401) return failed("invalid_token", "Airtable rejected AIRTABLE_API_KEY during the read probe.");
  if (!probe.ok) return failed("probe_error", probeError.message ?? "Airtable read probe failed unexpectedly.");

  const details = table
    ? { record_count: (probe.data?.records ?? []).length, has_more: Boolean(probe.data?.offset) }
    : { table_count: (probe.data?.tables ?? []).length };
  return {
    ok: true,
    status: "ok",
    message: "Airtable authentication and read access succeeded.",
    target,
    auth: authOk,
    probe: { ...probeBase, ok: true, status: "ok", details },
  };
}

/**
 * Bases the token can see, in Airtable's order, sliced to `limit` (1-1000, default 100). Airtable
 * pages this list: omit `offset` for the first page and pass the returned `offset` back for the next
 * one; the returned `offset` is null on the last page.
 */
export async function list_bases(
  args: { limit?: number; offset?: string },
  ctx: Ctx,
): Promise<{ bases: AirtableBase[]; offset: string | null }> {
  const limit = Math.max(1, Math.min(args.limit ?? 100, 1000));
  const body = await request(ctx, "list bases", "GET", `${META_URL}/bases${query({ offset: args.offset })}`);
  return { bases: (body?.bases ?? []).slice(0, limit), offset: body?.offset ?? null };
}

/**
 * Bases whose name or base ID contains `query` (case-insensitive), at most `limit` (1-100, default
 * 20). Searches the first page of `list_bases`; use a smaller `query` if a base is missing.
 */
export async function find_bases(
  args: { query: string; limit?: number },
  ctx: Ctx,
): Promise<{ bases: AirtableBase[] }> {
  const needle = args.query.toLowerCase();
  const limit = Math.max(1, Math.min(args.limit ?? 20, 100));
  const body = await request(ctx, "list bases", "GET", `${META_URL}/bases`);
  const bases = ((body?.bases ?? []) as AirtableBase[]).filter(
    (base) => String(base.name ?? "").toLowerCase().includes(needle) || String(base.id ?? "").toLowerCase().includes(needle),
  );
  return { bases: bases.slice(0, limit) };
}

/**
 * A base's full schema: every table with its fields (name, type, options) and views (ID, name, type).
 * Use the returned table `id` or `name` as `table` in the record functions.
 */
export async function schema(args: { base_id: string }, ctx: Ctx): Promise<{ tables: AirtableTable[] }> {
  const body = await request(ctx, "schema", "GET", tablesPath(args.base_id));
  return { tables: body?.tables ?? [] };
}

/**
 * Tables and views whose name or ID contains `query` (case-insensitive), at most `limit` (1-100,
 * default 20). Each match is the full table plus `matching_views`, the views that matched.
 */
export async function find_tables(
  args: { base_id: string; query: string; limit?: number },
  ctx: Ctx,
): Promise<{ tables: (AirtableTable & { matching_views: AirtableView[] })[] }> {
  const needle = args.query.toLowerCase();
  const limit = Math.max(1, Math.min(args.limit ?? 20, 100));
  const body = await request(ctx, "schema", "GET", tablesPath(args.base_id));
  const found: (AirtableTable & { matching_views: AirtableView[] })[] = [];
  for (const table of (body?.tables ?? []) as AirtableTable[]) {
    const tableHit =
      String(table.name ?? "").toLowerCase().includes(needle) || String(table.id ?? "").toLowerCase().includes(needle);
    const views = (table.views ?? []) as AirtableView[];
    const viewHits = views.filter(
      (view) =>
        String(view.name ?? "").toLowerCase().includes(needle) || String(view.id ?? "").toLowerCase().includes(needle),
    );
    if (tableHit || viewHits.length) found.push({ ...table, matching_views: viewHits });
  }
  return { tables: found.slice(0, limit) };
}

/**
 * Records from a table or view, following Airtable's 100-record pages up to `max_records` (1-1000,
 * default 100) in one call. `table` takes a table ID or name; `view` a view ID or name; `fields`
 * limits the returned cell values (Airtable still returns the field-less records); `filter_by_formula`
 * is Airtable formula syntax, e.g. `{Status}='Done'`. `has_more` says Airtable has another page
 * beyond the last one fetched.
 */
export async function list_records(
  args: {
    base_id: string;
    table: string;
    view?: string;
    max_records?: number;
    fields?: string[];
    filter_by_formula?: string;
  },
  ctx: Ctx,
): Promise<{
  base_id: string;
  table: string;
  view: string | null;
  count: number;
  records: AirtableRecord[];
  has_more: boolean;
}> {
  return listRecords(ctx, args);
}

/** One record by ID (`rec…`), with its `createdTime` and all cell values. */
export async function get_record(
  args: { base_id: string; table: string; record_id: string },
  ctx: Ctx,
): Promise<{ record: AirtableRecord }> {
  const record = await request(
    ctx,
    "get record",
    "GET",
    `${recordsPath(args.base_id, args.table)}/${seg(args.record_id)}`,
  );
  return { record };
}

/**
 * Read records from an Airtable table/view URL such as
 * `https://airtable.com/app…/tbl…/viw…`: the parsed IDs plus the same result as `list_records`
 * (table ID and view ID taken from the URL, `max_records` default 100). Throws for a non-Airtable
 * URL or one without an app and table ID.
 */
export async function records_from_url(
  args: { url: string; max_records?: number; fields?: string[] },
  ctx: Ctx,
): Promise<{
  parsed: ParsedUrl;
  base_id: string;
  table: string;
  view: string | null;
  count: number;
  records: AirtableRecord[];
  has_more: boolean;
}> {
  const target = targetFromUrl(args.url);
  const result = await listRecords(ctx, {
    base_id: target.base_id,
    table: target.table_id,
    view: target.view_id ?? undefined,
    max_records: args.max_records,
    fields: args.fields,
  });
  return { parsed: target.parsed, ...result };
}

/**
 * Case-insensitive substring search over cell values (strings, numbers, choices, links, attachments)
 * of up to `max_records` records (default 200). Airtable has no server-side text search, so this
 * fetches the records first and reports how many it searched; narrow with `view` and a small
 * `max_records`.
 */
export async function search_records(
  args: { base_id: string; table: string; query: string; view?: string; max_records?: number },
  ctx: Ctx,
): Promise<{
  base_id: string;
  table: string;
  view: string | null;
  query: string;
  searched: number;
  count: number;
  records: AirtableRecord[];
}> {
  const data = await listRecords(ctx, {
    base_id: args.base_id,
    table: args.table,
    view: args.view,
    max_records: args.max_records ?? 200,
  });
  const needle = args.query.toLowerCase();
  const found = data.records.filter((record) =>
    Object.values(record.fields ?? {}).some((value) => matches(value, needle)),
  );
  return {
    base_id: args.base_id,
    table: args.table,
    view: args.view ?? null,
    query: args.query,
    searched: data.count,
    count: found.length,
    records: found,
  };
}

/**
 * Create a table in a base. `fields` is Airtable's field schema array, e.g.
 * `[{ name: "Event", type: "singleLineText" }, { name: "Date", type: "date" }]`; when omitted the
 * required primary field is created as a single-line text field named "Name". Needs the
 * `schema.bases:write` scope. Returns the new table with its generated field and view IDs.
 */
export async function create_table(
  args: { base_id: string; name: string; fields?: { [field: string]: unknown }[]; description?: string },
  ctx: Ctx,
): Promise<{ table: AirtableTable }> {
  if (!args.name.trim()) throw new Error("Table name must not be empty.");
  const body: { [field: string]: unknown } = {
    name: args.name,
    fields: args.fields ?? [{ name: "Name", type: "singleLineText" }],
  };
  if (args.description !== undefined) body.description = args.description;
  return { table: await request(ctx, "create table", "POST", tablesPath(args.base_id), body) };
}

/**
 * Write: create one record. `fields` maps field names to values; `typecast` lets Airtable coerce
 * values (strings to dates, choices, attachments) instead of rejecting them. Needs the
 * `data.records:write` scope.
 */
export async function create_record(
  args: { base_id: string; table: string; fields: { [field: string]: unknown }; typecast?: boolean },
  ctx: Ctx,
): Promise<{ record: AirtableRecord }> {
  const record = await request(ctx, "create record", "POST", recordsPath(args.base_id, args.table), {
    fields: args.fields,
    typecast: args.typecast ?? false,
  });
  return { record };
}

/**
 * Write: create up to 10 records per request, batched automatically in one call. Items are field
 * mappings or `{ fields }` objects; `typecast` as in `create_record`. Returns the created records in
 * request order.
 */
export async function create_records(
  args: {
    base_id: string;
    table: string;
    records: ({ fields?: unknown; [field: string]: unknown })[];
    typecast?: boolean;
  },
  ctx: Ctx,
): Promise<{ base_id: string; table: string; count: number; records: AirtableRecord[] }> {
  const normalized = normalizeCreateRecords(args.records);
  const created: AirtableRecord[] = [];
  for (const group of batch(normalized)) {
    const body = await request(ctx, "create records", "POST", recordsPath(args.base_id, args.table), {
      records: group,
      typecast: args.typecast ?? false,
    });
    for (const record of (body?.records ?? []) as AirtableRecord[]) created.push(record);
  }
  return { base_id: args.base_id, table: args.table, count: created.length, records: created };
}

/**
 * Write: update one record's fields. Default PATCH changes only the fields given and leaves the
 * rest; `replace: true` uses PUT and clears every writable field not listed. `typecast` as in
 * `create_record`.
 */
export async function update_record(
  args: {
    base_id: string;
    table: string;
    record_id: string;
    fields: { [field: string]: unknown };
    typecast?: boolean;
    replace?: boolean;
  },
  ctx: Ctx,
): Promise<{ record: AirtableRecord }> {
  const record = await request(
    ctx,
    args.replace ? "replace record" : "update record",
    args.replace ? "PUT" : "PATCH",
    `${recordsPath(args.base_id, args.table)}/${seg(args.record_id)}`,
    { fields: args.fields, typecast: args.typecast ?? false },
  );
  return { record };
}

/**
 * Write: update up to 10 records per request, batched automatically in one call. Each item is
 * `{ id, fields }`; `replace` and `typecast` as in `update_record`. Returns the updated records.
 */
export async function update_records(
  args: {
    base_id: string;
    table: string;
    records: { id?: unknown; fields?: unknown; [field: string]: unknown }[];
    typecast?: boolean;
    replace?: boolean;
  },
  ctx: Ctx,
): Promise<{ base_id: string; table: string; count: number; records: AirtableRecord[] }> {
  const normalized = normalizeUpdateRecords(args.records);
  const updated: AirtableRecord[] = [];
  for (const group of batch(normalized)) {
    const body = await request(
      ctx,
      args.replace ? "replace records" : "update records",
      args.replace ? "PUT" : "PATCH",
      recordsPath(args.base_id, args.table),
      { records: group, typecast: args.typecast ?? false },
    );
    for (const record of (body?.records ?? []) as AirtableRecord[]) updated.push(record);
  }
  return { base_id: args.base_id, table: args.table, count: updated.length, records: updated };
}

/**
 * Write: create or update up to 10 records per request by matching `fields_to_merge_on` (for example
 * `["Email"]`), batched automatically in one call. Items are field mappings or `{ fields }` objects
 * and must include the merge fields. Returns the resulting records plus Airtable's `createdRecords`
 * and `updatedRecords` ID lists. Airtable caps an upsert at 10 records per request and requires the
 * merge field values to be unique.
 */
export async function upsert_records(
  args: {
    base_id: string;
    table: string;
    records: ({ fields?: unknown; [field: string]: unknown })[];
    fields_to_merge_on: string[];
    typecast?: boolean;
  },
  ctx: Ctx,
): Promise<{
  base_id: string;
  table: string;
  count: number;
  records: AirtableRecord[];
  createdRecords: string[];
  updatedRecords: string[];
}> {
  if (!args.fields_to_merge_on?.length) throw new Error("fields_to_merge_on must include at least one field name.");
  const normalized = normalizeCreateRecords(args.records);
  const upserted: AirtableRecord[] = [];
  const createdRecords: string[] = [];
  const updatedRecords: string[] = [];
  for (const group of batch(normalized)) {
    const body = await request(ctx, "upsert records", "PATCH", recordsPath(args.base_id, args.table), {
      records: group,
      typecast: args.typecast ?? false,
      performUpsert: { fieldsToMergeOn: args.fields_to_merge_on },
    });
    for (const record of (body?.records ?? []) as AirtableRecord[]) upserted.push(record);
    for (const id of (body?.createdRecords ?? []) as string[]) createdRecords.push(id);
    for (const id of (body?.updatedRecords ?? []) as string[]) updatedRecords.push(id);
  }
  return {
    base_id: args.base_id,
    table: args.table,
    count: upserted.length,
    records: upserted,
    createdRecords,
    updatedRecords,
  };
}

/** Write: delete one record. Airtable returns `{ id, deleted: true }`. */
export async function delete_record(
  args: { base_id: string; table: string; record_id: string },
  ctx: Ctx,
): Promise<{ record: DeletedRecord }> {
  const record = await request(
    ctx,
    "delete record",
    "DELETE",
    `${recordsPath(args.base_id, args.table)}/${seg(args.record_id)}`,
  );
  return { record };
}

/** Write: delete up to 10 records per request, batched automatically in one call. */
export async function delete_records(
  args: { base_id: string; table: string; record_ids: string[] },
  ctx: Ctx,
): Promise<{ base_id: string; table: string; count: number; records: DeletedRecord[] }> {
  const deleted: DeletedRecord[] = [];
  for (const group of batch(args.record_ids)) {
    const body = await request(
      ctx,
      "delete records",
      "DELETE",
      `${recordsPath(args.base_id, args.table)}${qs([["records[]", group]])}`,
    );
    for (const record of (body?.records ?? []) as DeletedRecord[]) deleted.push(record);
  }
  return { base_id: args.base_id, table: args.table, count: deleted.length, records: deleted };
}
