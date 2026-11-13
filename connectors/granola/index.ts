import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Granola has two backends behind one surface:
//
// - MCP (preferred, user-scoped): https://mcp.granola.ai/mcp, JSON-RPC over Streamable HTTP with
//   the OAuth token iron-proxy injects from the console consent flow. Sees only the connected
//   user's meetings, has no pagination, and returns its meetings as an XML text block.
// - REST (fallback, workspace-wide): the Enterprise public API at public-api.granola.ai with
//   GRANOLA_API_KEY.
//
// Which one a call uses is chosen per call from `GRANOLA_BACKEND=mcp|rest` when set, else MCP when
// the caller has a live Granola OAuth credential or no REST key is configured, else REST. Note
// shapes are REST-shaped on both backends; MCP's XML is parsed into them.

const DEFAULT_API_BASE = "https://public-api.granola.ai";
const DEFAULT_MCP_URL = "https://mcp.granola.ai/mcp";
const MAX_RATE_LIMIT_RETRIES = 2;
const MAX_PAGE_SIZE = 30;

const NOTE_ID_RE = /^not_[A-Za-z0-9]{14}$/;
const SHARE_ID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;
const MONTHS: Record<string, number> = {
  Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5, Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11,
};

/** A person on a note: the owner, or an attendee. */
export interface Person {
  name?: string;
  email?: string;
  [field: string]: unknown;
}

/** One transcript utterance; `speaker.source` names the speaker source. */
export interface Utterance {
  text: string;
  speaker?: { source?: string; diarized_speaker?: string; [field: string]: unknown };
  start_timestamp?: string;
  end_timestamp?: string;
  [field: string]: unknown;
}

/**
 * A meeting note as Granola returns it (the REST shape; the MCP backend is parsed into the same
 * fields): id, title, created_at, owner, attendees, summary_markdown, and optionally transcript.
 * `calendar_event`, `folder_membership` and other fields are passed through as they arrive.
 */
export interface Note {
  id: string;
  title?: string;
  created_at?: string;
  updated_at?: string;
  web_url?: string;
  owner?: Person;
  attendees?: Person[];
  summary_markdown?: string | null;
  summary_text?: string | null;
  calendar_event?: Record<string, unknown> | null;
  folder_membership?: unknown[];
  transcript?: Utterance[];
  [field: string]: unknown;
}

/** One page of notes: `notes`, `hasMore` and the `cursor` to pass back for the next page. */
export interface NotePage {
  notes: Note[];
  hasMore: boolean;
  cursor: string | null;
  [field: string]: unknown;
}

/** The MCP account payload: the connected account's email and active workspace. */
export interface GranolaAccount {
  email?: string;
  [field: string]: unknown;
}

/** Which Granola backend a call uses. */
export type Backend = "mcp" | "rest";
type Vars = Record<string, unknown>;

/** Which backend calls use for this caller (see the header comment). */
function backend(ctx: Ctx): Backend {
  const configured = (ctx.env.GRANOLA_BACKEND ?? "").trim().toLowerCase();
  if (configured === "mcp" || configured === "rest") return configured;
  const credentials = ctx.context?.sandbox?.oauth_credentials ?? [];
  const has_oauth = credentials.some((c) => c.provider === "granola" && c.status === "live");
  if (has_oauth || ctx.env.GRANOLA_MCP_TOKEN || !ctx.env.GRANOLA_API_KEY) return "mcp";
  return "rest";
}

function mcp_url(ctx: Ctx): string {
  return ctx.env.GRANOLA_MCP_URL || DEFAULT_MCP_URL;
}

function api_base(ctx: Ctx): string {
  return (ctx.env.GRANOLA_API_BASE || DEFAULT_API_BASE).replace(/\/+$/, "");
}

async function mcp_rpc(ctx: Ctx, method: string, params: Vars): Promise<any> {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
  };
  // Behind iron-proxy the OAuth token is injected for mcp.granola.ai; locally it comes from the env.
  if (ctx.env.GRANOLA_MCP_TOKEN) headers.authorization = `Bearer ${ctx.env.GRANOLA_MCP_TOKEN}`;
  const res = await ctx.fetch(mcp_url(ctx), {
    method: "POST",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Granola MCP ${method} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  // Streamable HTTP answers either a JSON body or a one-message event stream.
  let payload = text;
  if ((res.headers["content-type"] ?? "").startsWith("text/event-stream")) {
    const events = text.split("\n").filter((line) => line.startsWith("data: "));
    if (events.length === 0) throw new Error(`Granola MCP returned an empty event stream for ${method}`);
    payload = events[events.length - 1].slice("data: ".length);
  }
  const message = JSON.parse(payload);
  if (message.error) {
    throw new Error(`Granola MCP error from ${method}: ${message.error.message ?? JSON.stringify(message.error)}`);
  }
  return message.result;
}

/** Call an MCP tool and return its concatenated text content. */
async function mcp_tool(ctx: Ctx, name: string, args: Vars): Promise<string> {
  const result = await mcp_rpc(ctx, "tools/call", { name, arguments: args });
  if (result?.isError) {
    throw new Error(`Granola MCP tool ${name} failed: ${JSON.stringify(result).slice(0, 1000)}`);
  }
  return (result?.content ?? [])
    .filter((content: { type?: string }) => content.type === "text")
    .map((content: { text?: string }) => content.text ?? "")
    .join("\n");
}

async function rest_get(ctx: Ctx, path: string, params?: Params): Promise<any> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the key is injected as `Bearer <key>`; locally it comes from the env.
  if (ctx.env.GRANOLA_API_KEY) headers.authorization = `Bearer ${ctx.env.GRANOLA_API_KEY}`;
  let res = await ctx.fetch(api_base(ctx) + path + query(params ?? {}), { headers });
  for (let attempt = 0; res.status === 429 && attempt < MAX_RATE_LIMIT_RETRIES; attempt++) {
    const seconds = Number(res.headers["retry-after"] ?? "1");
    const delay = Number.isFinite(seconds) ? Math.min(Math.max(seconds, 0), 60) : 1;
    await ctx.sleep(delay * 1000);
    res = await ctx.fetch(api_base(ctx) + path + query(params ?? {}), { headers });
  }
  return readJson(res, "Granola");
}

/** A Granola API note id (`not_…`) or meeting UUID, from an id, UUID or notes.granola.ai share link. */
function normalize_note_ref(note_ref: string): string {
  const value = note_ref
    .trim()
    .replace(/^</, "")
    .split("|")[0]
    .replace(/>$/, "")
    .trim();
  if (NOTE_ID_RE.test(value)) return value;
  const match = SHARE_ID_RE.exec(value);
  if (match) return match[0].toLowerCase();
  throw new Error("expected a Granola not_* ID, meeting UUID, or notes.granola.ai share link");
}

function decode_entities(value: string): string {
  return value.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|amp|lt|gt|quot|apos);/g, (whole, code: string) => {
    if (code === "amp") return "&";
    if (code === "lt") return "<";
    if (code === "gt") return ">";
    if (code === "quot") return '"';
    if (code === "apos") return "'";
    const point = code.startsWith("#x") ? parseInt(code.slice(2), 16) : parseInt(code.slice(1), 10);
    return Number.isFinite(point) ? String.fromCharCode(point) : whole;
  });
}

function attributes(source: string): Record<string, string> {
  const found: Record<string, string> = {};
  const re = /([A-Za-z_:][-A-Za-z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
  let match = re.exec(source);
  while (match) {
    found[match[1]] = decode_entities(match[2] ?? match[3] ?? "");
    match = re.exec(source);
  }
  return found;
}

function child_text(source: string, name: string): string | null {
  const match = new RegExp(`<${name}\\b[^>]*>([\\s\\S]*?)</${name}>`, "i").exec(source);
  return match ? decode_entities(match[1]) : null;
}

/** Granola's display-name/email text ("Matt (note creator) from Acme <matt@example.com>, ..."). */
function parse_participants(text: string): Person[] {
  const participants: Person[] = [];
  const re = /([^,<]+?)\s*<([^>]+)>/g;
  let match = re.exec(text);
  while (match) {
    participants.push({ name: match[1].trim(), email: match[2].trim().toLowerCase() });
    match = re.exec(text);
  }
  return participants;
}

/** "Jul 8, 2026 5:30 PM GMT+2" to "2026-07-08T17:30:00+02:00"; anything else is returned as sent. */
function parse_meeting_date(raw: string): string {
  const suffix = /^(.*) GMT([+-]?\d+)?$/.exec(raw);
  if (!suffix) return raw;
  const parts = /^([A-Z][a-z]{2}) (\d{1,2}), (\d{4}) (\d{1,2}):(\d{2}) (AM|PM)$/.exec(suffix[1].trim());
  if (!parts) return raw;
  const offset_hours = Number(suffix[2] ?? "0");
  if (!Number.isFinite(offset_hours) || Math.abs(offset_hours) > 23) return raw;
  const month = MONTHS[parts[1]];
  if (month === undefined) return raw;
  const day = Number(parts[2]);
  const year = Number(parts[3]);
  let hour = Number(parts[4]) % 12;
  if (parts[6] === "PM") hour += 12;
  const minute = Number(parts[5]);
  if (day < 1 || day > 31 || hour > 23 || minute > 59) return raw;
  const pad = (value: number): string => String(value).padStart(2, "0");
  const sign = offset_hours < 0 ? "-" : "+";
  const offset = `${sign}${pad(Math.abs(offset_hours))}:00`;
  return `${year}-${pad(month + 1)}-${pad(day)}T${pad(hour)}:${pad(minute)}:00${offset}`;
}

/** Parse the MCP `meetings_data` XML text into REST-shaped notes. */
function parse_meetings(text: string): Note[] {
  const notes: Note[] = [];
  const re = /<meeting\b([^>]*)>([\s\S]*?)<\/meeting>/g;
  let match = re.exec(text);
  while (match) {
    const attrs = attributes(match[1]);
    if (attrs.id && attrs.title && attrs.date) {
      const attendees = parse_participants(child_text(match[2], "known_participants") ?? "");
      const creatorIndex = attendees.findIndex((attendee) => attendee.name?.includes("(note creator)"));
      const owner = creatorIndex >= 0 ? attendees[creatorIndex] : attendees[0];
      const summary = child_text(match[2], "summary");
      notes.push({
        id: attrs.id,
        title: attrs.title,
        created_at: parse_meeting_date(attrs.date),
        owner: owner
          ? {
              name: (owner.name ?? "").replace("(note creator)", "").split(" from ")[0].trim(),
              email: owner.email,
            }
          : {},
        attendees,
        summary_markdown: summary ? summary.trim() : null,
      });
    }
    match = re.exec(text);
  }
  return notes;
}

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

function one_year_ago(): string {
  const now = new Date();
  const then = new Date(now.getTime());
  then.setUTCFullYear(then.getUTCFullYear() - 1);
  return then.toISOString().slice(0, 10);
}

/** The REST page for the MCP backend: no pagination, the requested window only. */
async function mcp_notes(
  ctx: Ctx,
  page_size: number,
  created_after: string | undefined,
  created_before: string | undefined,
): Promise<NotePage> {
  const args: Vars =
    created_after || created_before
      ? {
          time_range: "custom",
          custom_start: (created_after ?? "2000-01-01").slice(0, 10),
          custom_end: (created_before ?? today()).slice(0, 10),
        }
      : { time_range: "last_30_days" };
  const text = await mcp_tool(ctx, "list_meetings", args);
  return { notes: parse_meetings(text).slice(0, page_size), hasMore: false, cursor: null };
}

async function list_page(
  ctx: Ctx,
  args: { page_size?: number; cursor?: string; created_before?: string; created_after?: string; updated_after?: string },
): Promise<NotePage> {
  const page_size = Math.max(1, Math.min(args.page_size ?? MAX_PAGE_SIZE, MAX_PAGE_SIZE));
  if (backend(ctx) === "mcp") return mcp_notes(ctx, page_size, args.created_after, args.created_before);
  const body = await rest_get(ctx, "/v1/notes", {
    page_size,
    cursor: args.cursor,
    created_before: args.created_before,
    created_after: args.created_after,
    updated_after: args.updated_after,
  });
  return { notes: body.notes ?? [], hasMore: Boolean(body.hasMore), cursor: body.cursor ?? null };
}

async function all_notes(
  ctx: Ctx,
  limit: number,
  created_after: string | undefined,
  updated_after: string | undefined,
): Promise<Note[]> {
  if (backend(ctx) === "mcp") {
    // MCP has no pagination and, without a window, covers the last year.
    const after = created_after || updated_after || one_year_ago();
    return (await mcp_notes(ctx, Math.max(1, limit), after, undefined)).notes.slice(0, limit);
  }
  const notes: Note[] = [];
  let cursor: string | undefined;
  while (notes.length < limit) {
    const page_size = Math.min(MAX_PAGE_SIZE, limit - notes.length);
    const body = await rest_get(ctx, "/v1/notes", {
      page_size,
      cursor,
      created_after,
      updated_after,
    });
    const received: Note[] = body.notes ?? [];
    notes.push(...received);
    if (!body.hasMore || !body.cursor) break;
    cursor = body.cursor;
  }
  return notes.slice(0, limit);
}

/** Resolve a meeting UUID to a REST note id by matching each note's share URL. */
async function resolve_note_id(ctx: Ctx, note_ref: string): Promise<string> {
  const normalized = normalize_note_ref(note_ref);
  if (NOTE_ID_RE.test(normalized)) return normalized;
  let cursor: string | undefined;
  for (;;) {
    const page_body = await rest_get(ctx, "/v1/notes", { page_size: MAX_PAGE_SIZE, cursor });
    const notes: Note[] = page_body.notes ?? [];
    for (const note of notes) {
      if (!note.id) continue;
      const candidate = note.web_url ? note : await rest_get(ctx, `/v1/notes/${encodeURIComponent(note.id)}`);
      let web_id: string;
      try {
        web_id = normalize_note_ref(candidate.web_url ?? "");
      } catch {
        continue;
      }
      if (web_id === normalized) return note.id;
    }
    const next = page_body.cursor ?? null;
    if (!page_body.hasMore || !next) break;
    cursor = next;
  }
  throw new Error(
    `meeting ${normalized} not found in accessible Granola notes; the note may require user-scoped Granola access`,
  );
}

/**
 * Check connectivity and auth with a safe read-only call, and report which backend answered:
 * `mcp` (user-scoped meetings, no pagination) or `rest` (Enterprise workspace-wide,
 * cursor-paginated). Throws with the vendor's error when the call fails.
 */
export async function health(args: {}, ctx: Ctx): Promise<{ backend: Backend; notes: Note[] }> {
  const page = await list_page(ctx, { page_size: 1 });
  return { backend: backend(ctx), notes: page.notes };
}

/** The connected Granola account (MCP backend only): email and active workspace. */
export async function get_account_info(args: {}, ctx: Ctx): Promise<{ account: GranolaAccount }> {
  if (backend(ctx) !== "mcp") {
    throw new Error("get_account_info needs MCP access (connect Granola through the OAuth consent flow); the REST API key has no account endpoint");
  }
  return { account: JSON.parse(await mcp_tool(ctx, "get_account_info", {})) };
}

/**
 * Meeting folders (MCP backend only): the MCP tool's text listing of folder ids, titles,
 * descriptions and note counts.
 */
export async function list_folders(args: {}, ctx: Ctx): Promise<{ text: string }> {
  if (backend(ctx) !== "mcp") {
    throw new Error("list_folders needs MCP access; the Granola REST API has no folders endpoint");
  }
  return { text: await mcp_tool(ctx, "list_meeting_folders", {}) };
}

/**
 * Ask Granola a natural-language question about the connected user's meetings and return its
 * answer as Markdown (MCP backend only). `document_ids` restricts the question to specific meeting
 * notes.
 */
export async function query_meetings(
  args: { query: string; document_ids?: string[] },
  ctx: Ctx,
): Promise<{ answer: string }> {
  if (backend(ctx) !== "mcp") {
    throw new Error("query_meetings needs MCP access (connect Granola through the OAuth consent flow); the REST API has no question endpoint");
  }
  const call: Vars = { query: args.query };
  if (args.document_ids?.length) call.document_ids = args.document_ids;
  return { answer: await mcp_tool(ctx, "query_granola_meetings", call) };
}

/**
 * One page of meeting notes. `page_size` is at most 30. `created_after`/`created_before`/
 * `updated_after` take ISO 8601 dates (`YYYY-MM-DD` or a timestamp). Pass the returned `cursor`
 * back for the next page; `hasMore` says whether one exists. On the MCP backend there is no
 * pagination (`hasMore` is false) and without a date filter the page covers the last 30 days.
 */
export async function list_notes(
  args: { page_size?: number; cursor?: string; created_before?: string; created_after?: string; updated_after?: string },
  ctx: Ctx,
): Promise<NotePage> {
  return list_page(ctx, args);
}

/**
 * Up to `limit` meeting notes (default 50), in the order the API returns them, paging internally
 * (30 per request on REST). `created_after` and `updated_after` are ISO 8601 dates. On the MCP
 * backend this covers the last year unless a date is given.
 */
export async function list_all_notes(
  args: { limit?: number; created_after?: string; updated_after?: string },
  ctx: Ctx,
): Promise<{ notes: Note[] }> {
  return { notes: await all_notes(ctx, args.limit ?? 50, args.created_after, args.updated_after) };
}

/**
 * One meeting note by `not_…` id, meeting UUID or notes.granola.ai share link, with title, owner,
 * attendees, summary and calendar/folder metadata. `include_transcript` adds the transcript
 * utterances.
 */
export async function get_note(
  args: { note_id: string; include_transcript?: boolean },
  ctx: Ctx,
): Promise<{ note: Note }> {
  if (backend(ctx) === "mcp") {
    const meeting_id = normalize_note_ref(args.note_id);
    const notes = parse_meetings(await mcp_tool(ctx, "get_meetings", { meeting_ids: [meeting_id] }));
    if (notes.length === 0) throw new Error(`meeting ${meeting_id} not found`);
    const note = notes[0];
    if (args.include_transcript) note.transcript = await mcp_transcript(ctx, meeting_id);
    return { note };
  }
  const note_id = await resolve_note_id(ctx, args.note_id);
  const body = await rest_get(ctx, `/v1/notes/${encodeURIComponent(note_id)}`, {
    include: args.include_transcript ? "transcript" : undefined,
  });
  return { note: body };
}

async function mcp_transcript(ctx: Ctx, meeting_id: string): Promise<Utterance[]> {
  const text = (await mcp_tool(ctx, "get_meeting_transcript", { meeting_id })).trim();
  return text ? [{ speaker: { source: "transcript" }, text }] : [];
}

/**
 * The transcript utterances of a meeting (id, UUID or share link). On the MCP backend the whole
 * transcript comes back as one block, so there is a single utterance with
 * `speaker.source = "transcript"`.
 */
export async function get_transcript(
  args: { note_id: string },
  ctx: Ctx,
): Promise<{ transcript: Utterance[] }> {
  if (backend(ctx) === "mcp") {
    return { transcript: await mcp_transcript(ctx, normalize_note_ref(args.note_id)) };
  }
  const note_id = await resolve_note_id(ctx, args.note_id);
  const body = await rest_get(ctx, `/v1/notes/${encodeURIComponent(note_id)}`, { include: "transcript" });
  return { transcript: body.transcript ?? [] };
}

/**
 * Notes whose title contains `query` (case-insensitive), up to `limit` (default 50). There is no
 * server-side search: this pages up to 200 recent notes and filters locally, so narrow the window
 * with `created_after` when the workspace is large.
 */
export async function search_notes(
  args: { query: string; limit?: number; created_after?: string },
  ctx: Ctx,
): Promise<{ notes: Note[] }> {
  const needle = args.query.toLowerCase();
  const candidates = await all_notes(ctx, 200, args.created_after, undefined);
  return {
    notes: candidates
      .filter((note) => (note.title ?? "").toLowerCase().includes(needle))
      .slice(0, args.limit ?? 50),
  };
}
