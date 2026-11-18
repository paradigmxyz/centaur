import type { Ctx, RequestInit, Response } from "../centaur";
import { query, readJson, encode, type Params } from "../_lib/http";
import { downloadTo, outputPath, requireFiles, safeFilename } from "../_lib/files";
import { companyContextRequest, companyContextUrl, rfc3339 } from "../_lib/company_context";

// Results are Slack's own objects, passed through. The interfaces list the
// fields worth knowing; Slack sends more.

const SLACK = "https://slack.com/api";
const FILES_HOST = "files.slack.com";
const DEFAULT_CENTAUR_API = "http://api:8000";

/**
 * How Slack is reached, chosen from the calling context. `direct`: slack.com with the linked
 * Slack user token, when the caller has a live `slack` OAuth credential. `centaur`: Centaur's
 * API, scoped to the caller's channel grants. Every function picks the path automatically.
 */
export type Access = "direct" | "centaur";

/**
 * Where `search_messages` searches. `company_context`: the indexed Slack history in company
 * context v2, scoped to what the calling principal can see; used for every principal other than a
 * Slack DM or MCP (Console user) principal wherever company context v2 is deployed. `slack`:
 * Slack's native `search.messages`, with direct access. `null`: search is unavailable.
 */
export type SearchBackend = "company_context" | "slack";

/** A Slack message object. */
export interface Message {
  ts: string;
  text: string;
  user?: string;
  bot_id?: string;
  subtype?: string;
  thread_ts?: string;
  reply_count?: number;
  reactions?: { name: string; count: number; users?: string[] }[];
  files?: { id: string; name?: string; mimetype?: string; size?: number }[];
  [field: string]: unknown;
}

/**
 * A conversation. Directly this is Slack's conversation object (`topic`/`purpose` are
 * `{ value }`, `num_members`); through the Centaur API it is Centaur's channel item (`topic` and
 * `purpose` are strings, `member_count`, and the caller's `can_*` grants).
 */
export interface Channel {
  id: string;
  name?: string;
  is_private?: boolean;
  is_im?: boolean;
  is_mpim?: boolean;
  is_member?: boolean;
  num_members?: number;
  member_count?: number;
  topic?: string | { value: string };
  purpose?: string | { value: string };
  can_read_history?: boolean;
  can_upload?: boolean;
  can_download?: boolean;
  [field: string]: unknown;
}

/** A Slack user object. */
export interface User {
  id: string;
  name: string;
  real_name?: string;
  tz?: string;
  is_bot?: boolean;
  deleted?: boolean;
  profile?: { email?: string; title?: string; display_name?: string; real_name?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** A Slack file object, as `files.info` and `files.completeUploadExternal` return it. */
export interface FileInfo {
  id: string;
  name: string | null;
  title: string | null;
  mimetype: string | null;
  size: number | null;
  user: string | null;
  permalink: string | null;
  url_private_download: string | null;
}

/** A file a connector function saved for the caller (see `_lib/files`). */
export interface SavedFile {
  /** Absolute path in the sandbox. */
  path: string;
  bytes: number;
  mime_type?: string;
}

/** A Slack file downloaded to the sandbox: where it landed, and Slack's metadata for it. */
export interface DownloadedFile extends SavedFile {
  file: FileInfo;
}

/** A completed upload: the new file IDs, and Slack's metadata for each. */
export interface UploadedFiles {
  file_ids: string[];
  files: FileInfo[];
}

/** An indexed Slack document from company context v2, passed through as the API sends it. */
export interface IndexedDocument {
  /** Pass to `company_context.read_document`. */
  document_id: string;
  /** `slack_message`: a channel-day transcript chunk. */
  type: string;
  title: string | null;
  url: string | null;
  text: string;
  occurred_at: string | null;
  /** Fused rank score in hybrid mode; absent in keyword (timestamp) mode. */
  score?: number;
  /** For Slack messages, `conversation_id` and `channel_name` among others. */
  metadata: Record<string, unknown>;
  [field: string]: unknown;
}

/** Native Slack search results. */
export interface SlackSearchResult {
  backend: "slack";
  total: number;
  matches: SearchMatch[];
  paging?: { count: number; total: number; page: number; pages: number };
}

/** Company context v2 search results; follow `next_cursor` as `cursor` in timestamp mode. */
export interface IndexedSearchResult {
  backend: "company_context";
  results: IndexedDocument[];
  next_cursor?: string | null;
}

/** One `search.messages` match. */
export interface SearchMatch {
  ts: string;
  text: string;
  user?: string;
  username?: string;
  channel?: { id: string; name?: string };
  permalink?: string;
  [field: string]: unknown;
}

type Body = Record<string, any>;

// Routing only: iron-proxy injects the user token and the API checks grants either way.
function access(ctx: Ctx): Access {
  const credentials = ctx.context?.sandbox?.oauth_credentials ?? [];
  return credentials.some((c) => c.provider === "slack" && c.status === "live") ? "direct" : "centaur";
}

// Principals that act for one person: a Slack DM's user and a Console (MCP) user.
const PERSONAL_PRINCIPAL_KINDS = ["slack_dm", "console_user"];

// Routing only: the company-context service scopes results to the principal either way.
function searchBackend(ctx: Ctx): SearchBackend | null {
  const kind = ctx.context?.sandbox?.principal?.kind;
  if (kind && !PERSONAL_PRINCIPAL_KINDS.includes(kind) && companyContextUrl(ctx)) return "company_context";
  return access(ctx) === "direct" ? "slack" : null;
}

function token(ctx: Ctx): string {
  const value = ctx.env.SLACK_BOT_TOKEN;
  if (!value) throw new Error("SLACK_BOT_TOKEN is not set for direct Slack access");
  return value;
}

/** Slack answers 200 with `{ ok: false, error }`; surface that as an error. */
async function slackResult(res: Response, method: string): Promise<Body> {
  const retry = res.headers["retry-after"];
  const body = await readJson(res, `Slack ${method}${retry ? ` (Retry-After: ${retry}s)` : ""}`);
  if (!body.ok) throw new Error(`Slack ${method} failed: ${body.error ?? JSON.stringify(body)}`);
  return body;
}

async function slackGet(ctx: Ctx, method: string, params: Params = {}): Promise<Body> {
  const res = await ctx.fetch(`${SLACK}/${method}${query(params)}`, {
    headers: { authorization: `Bearer ${token(ctx)}` },
  });
  return slackResult(res, method);
}

async function slackPost(ctx: Ctx, method: string, params: Params): Promise<Body> {
  const res = await ctx.fetch(`${SLACK}/${method}`, {
    method: "POST",
    headers: { authorization: `Bearer ${token(ctx)}`, "content-type": "application/x-www-form-urlencoded" },
    body: encode(params),
  });
  return slackResult(res, method);
}

function centaurBase(ctx: Ctx): string {
  return (ctx.env.CENTAUR_API_URL || DEFAULT_CENTAUR_API).replace(/\/+$/, "");
}

function centaurHeaders(ctx: Ctx): Record<string, string> {
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the API credential is injected for the API host.
  if (ctx.env.CENTAUR_API_BEARER_TOKEN) headers.authorization = `Bearer ${ctx.env.CENTAUR_API_BEARER_TOKEN}`;
  return headers;
}

async function centaur(ctx: Ctx, path: string, params: Params = {}, method = "GET"): Promise<Body> {
  const res = await ctx.fetch(`${centaurBase(ctx)}${path}${query(params)}`, { method, headers: centaurHeaders(ctx) });
  return readJson(res, `Centaur API ${path}`);
}

/** Content type for an upload, from the file name's extension; Slack uses it for previews. */
function mimeFor(filename: string): string {
  const extension = filename.split(".").pop()?.toLowerCase() ?? "";
  const known: Record<string, string> = {
    png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp",
    pdf: "application/pdf", csv: "text/csv", txt: "text/plain", md: "text/markdown",
    json: "application/json", html: "text/html", zip: "application/zip",
    mp4: "video/mp4", mov: "video/quicktime", mp3: "audio/mpeg", wav: "audio/wav",
  };
  return known[extension] ?? "application/octet-stream";
}

const seg = (value: string): string => encodeURIComponent(value.trim());
const nextCursor = (body: Body): string | null => body.response_metadata?.next_cursor || null;
const text = (value: unknown): string | null => (typeof value === "string" ? value : null);

/** Slack's file object, narrowed to the fields worth knowing. */
function fileInfo(raw: Body): FileInfo {
  return {
    id: text(raw.id) ?? "",
    name: text(raw.name),
    title: text(raw.title),
    mimetype: text(raw.mimetype),
    size: typeof raw.size === "number" ? raw.size : null,
    user: text(raw.user),
    permalink: text(raw.permalink),
    url_private_download: text(raw.url_private_download ?? raw.url_private),
  };
}

/** Through the Centaur API a file is authorized by the channel it is shared in. */
function requireChannel(channelId: string | undefined, what: string): string {
  if (!channelId) throw new Error(`${what} through the Centaur API needs \`channel\` (the conversation the file is shared in)`);
  return channelId;
}

/** Slack ts, YYYY-MM-DD (UTC midnight) or ISO time to a Slack ts. */
function slackTs(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const raw = value.trim();
  if (/^\d+(\.\d+)?$/.test(raw)) return raw;
  const millis = Date.parse(/^\d{4}-\d{2}-\d{2}$/.test(raw) ? `${raw}T00:00:00Z` : raw);
  if (Number.isNaN(millis)) throw new Error(`timestamp ${JSON.stringify(value)} is not a Slack ts, YYYY-MM-DD or ISO time`);
  return (millis / 1000).toFixed(6);
}

/** Which path Slack calls take for this caller (see `Access`). */
export async function get_access(
  args: {},
  ctx: Ctx,
): Promise<{ access: Access; search: SearchBackend | null; thread_key: string | null }> {
  return { access: access(ctx), search: searchBackend(ctx), thread_key: ctx.context?.thread_key ?? null };
}

/**
 * Search Slack messages; `backend` says where (see `SearchBackend`).
 *
 * `company_context`: indexed channel history visible to the caller, as channel-day transcript
 * chunks. Ranked by relevance by default; `sort: "timestamp"` returns every chunk containing all
 * query terms, newest first, paged with `cursor`. Slack modifiers are not parsed: filter with
 * `channel_ids` (conversation IDs) and `after`/`before`. `count` at most 50.
 *
 * `slack`: Slack's native search, needing direct access. `query` takes Slack search syntax
 * (`in:#channel`, `from:@user`, `before:`, `after:`, `has:link`); `count` at most 100, paged
 * with `page`.
 */
export async function search_messages(
  args: {
    query: string;
    sort?: "score" | "timestamp";
    sort_dir?: "asc" | "desc";
    count?: number;
    page?: number;
    /** company_context only: Slack conversation IDs, at most 100. */
    channel_ids?: string[];
    /** company_context only: inclusive lower bound, RFC 3339 time or YYYY-MM-DD (UTC). */
    after?: string;
    /** company_context only: exclusive upper bound, RFC 3339 time or YYYY-MM-DD (UTC). */
    before?: string;
    /** company_context only: `next_cursor` from the previous timestamp-sorted page. */
    cursor?: string;
  },
  ctx: Ctx,
): Promise<SlackSearchResult | IndexedSearchResult> {
  const backend = searchBackend(ctx);
  if (backend === "company_context") {
    if (!args.query?.trim()) throw new Error("query cannot be empty");
    const filters: Record<string, unknown> = { types: ["slack_message"] };
    if (args.channel_ids?.length) filters.channel_ids = args.channel_ids;
    if (args.after) filters.occurred_after = rfc3339(args.after);
    if (args.before) filters.occurred_before = rfc3339(args.before);
    const body: Record<string, unknown> = { query: args.query.trim(), filters };
    if (args.sort === "timestamp") body.mode = "keyword";
    if (args.count !== undefined) body.limit = Math.min(args.count, 50);
    if (args.cursor) body.cursor = args.cursor;
    const result = await companyContextRequest(ctx, "POST", "/query", body);
    return { backend, results: result?.results ?? [], next_cursor: result?.next_cursor ?? null };
  }
  if (backend !== "slack") {
    throw new Error("search_messages needs company context v2 or a linked Slack user token (direct access); neither is available to this caller");
  }
  const { messages } = await slackGet(ctx, "search.messages", {
    query: args.query, sort: args.sort, sort_dir: args.sort_dir, count: args.count, page: args.page,
  });
  return { backend, total: messages?.total ?? 0, matches: messages?.matches ?? [], paging: messages?.paging };
}

/**
 * Conversations. `readable_only` keeps those whose history can be read (the bot's memberships
 * directly; the caller's history grants through Centaur). `query` filters by name
 * (case-insensitive). Follow `next_cursor` as `cursor` for more; `limit` at most 200.
 */
export async function channels(
  args: { query?: string; readable_only?: boolean; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ channels: Channel[]; next_cursor: string | null }> {
  const limit = Math.min(args.limit ?? 100, 200);
  if (access(ctx) === "centaur") {
    const body = await centaur(ctx, "/api/slack/channels", {
      limit, cursor: args.cursor, query: args.query, history_only: args.readable_only ?? false,
    });
    return { channels: body.channels ?? [], next_cursor: nextCursor(body) };
  }
  // Slack has no name filter, so page until `limit` matches.
  const needle = args.query?.trim().toLowerCase();
  const method = args.readable_only ? "users.conversations" : "conversations.list";
  const found: Channel[] = [];
  let cursor = args.cursor;
  for (;;) {
    const body = await slackGet(ctx, method, {
      limit, cursor, types: "public_channel,private_channel", exclude_archived: true,
    });
    for (const c of (body.channels ?? []) as Channel[]) {
      if (!needle || c.name?.toLowerCase().includes(needle)) found.push(c);
    }
    cursor = nextCursor(body) ?? undefined;
    if (!cursor || found.length >= limit) return { channels: found.slice(0, limit), next_cursor: cursor ?? null };
  }
}

/**
 * One page of a conversation, newest first. `channel` is a conversation ID (C…, G… or D…).
 * `oldest`/`latest` take a Slack ts, YYYY-MM-DD or ISO time. Thread parents carry `reply_count`;
 * read replies with `thread_replies`. Follow `next_cursor` while `has_more`.
 */
export async function channel_history(
  args: { channel: string; limit?: number; cursor?: string; oldest?: string; latest?: string; inclusive?: boolean },
  ctx: Ctx,
): Promise<{ messages: Message[]; has_more: boolean; next_cursor: string | null }> {
  const params: Params = {
    limit: args.limit, cursor: args.cursor, oldest: slackTs(args.oldest), latest: slackTs(args.latest),
    inclusive: args.inclusive,
  };
  const body = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/channels/${seg(args.channel)}/history`, params)
    : await slackGet(ctx, "conversations.history", { ...params, channel: args.channel });
  return { messages: body.messages ?? [], has_more: Boolean(body.has_more), next_cursor: nextCursor(body) };
}

/**
 * A thread, oldest first, starting with the parent. From a permalink
 * `https://…/archives/C123/p1700000000123456`, channel is C123 and ts 1700000000.123456.
 */
export async function thread_replies(
  args: { channel: string; ts: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ messages: Message[]; has_more: boolean; next_cursor: string | null }> {
  const params: Params = { limit: args.limit, cursor: args.cursor };
  const body = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/channels/${seg(args.channel)}/threads/${seg(args.ts)}/replies`, params)
    : await slackGet(ctx, "conversations.replies", { ...params, channel: args.channel, ts: args.ts });
  return { messages: body.messages ?? [], has_more: Boolean(body.has_more), next_cursor: nextCursor(body) };
}

/**
 * Member user IDs of a conversation (C…, G… or D…), as Slack's `conversations.members` returns
 * them; resolve each with `user_details` or match against `users`. Through Centaur this needs
 * the caller's history grant on the channel. Follow `next_cursor` as `cursor`; `limit` at most
 * 1000.
 */
export async function channel_members(
  args: { channel: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ members: string[]; next_cursor: string | null }> {
  const params: Params = { limit: args.limit === undefined ? undefined : Math.min(args.limit, 1000), cursor: args.cursor };
  const body = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/channels/${seg(args.channel)}/members`, params)
    : await slackGet(ctx, "conversations.members", { ...params, channel: args.channel.trim() });
  return { members: body.members ?? [], next_cursor: nextCursor(body) };
}

/** A message's permalink by conversation ID and ts. */
export async function permalink(args: { channel: string; ts: string }, ctx: Ctx): Promise<{ permalink: string }> {
  const body = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/channels/${seg(args.channel)}/messages/${seg(args.ts)}/permalink`)
    : await slackGet(ctx, "chat.getPermalink", { channel: args.channel, message_ts: args.ts });
  return { permalink: body.permalink };
}

/**
 * Workspace users. `query` filters on handle, real name, display name, email or title
 * (case-insensitive), paging until `limit` matches; Slack has no server-side filter.
 */
export async function users(
  args: { query?: string; limit?: number; cursor?: string },
  ctx: Ctx,
): Promise<{ users: User[]; next_cursor: string | null }> {
  const limit = Math.min(args.limit ?? 100, 200);
  const needle = args.query?.trim().toLowerCase();
  const found: User[] = [];
  let cursor = args.cursor;
  for (;;) {
    const params: Params = { limit, cursor };
    const body = access(ctx) === "centaur"
      ? await centaur(ctx, "/api/slack/users", params)
      : await slackGet(ctx, "users.list", params);
    for (const u of (body.members ?? []) as User[]) {
      const hay = [u.name, u.real_name, u.profile?.display_name, u.profile?.email, u.profile?.title];
      if (!needle || hay.some((v) => v?.toLowerCase().includes(needle))) found.push(u);
    }
    cursor = nextCursor(body) ?? undefined;
    if (!cursor || found.length >= limit) return { users: found.slice(0, limit), next_cursor: cursor ?? null };
  }
}

/** One user by user ID (U… or W…). */
export async function user_details(args: { user: string }, ctx: Ctx): Promise<{ user: User }> {
  const body = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/users/${seg(args.user)}`)
    : await slackGet(ctx, "users.info", { user: args.user.trim() });
  return { user: body.user };
}

/**
 * Download a Slack file (F…) to the sandbox and return where it landed, plus Slack's metadata.
 * `channel` is the conversation the file is shared in, required through the Centaur API. `path`
 * defaults to `downloads/slack/<file name>`. Read it back with `files.read` or hand it to another
 * tool.
 *
 * @requires files
 */
export async function download_file(
  args: { file: string; channel?: string; path?: string },
  ctx: Ctx,
): Promise<DownloadedFile> {
  requireFiles(ctx, "slack.download_file");
  let info: FileInfo;
  let url: string;
  let init: RequestInit;
  if (access(ctx) === "centaur") {
    const channelId = requireChannel(args.channel, "download_file");
    const raw = await centaur(ctx, `/api/slack/files/${seg(args.file)}/info`, { channel_id: channelId });
    info = fileInfo(raw.file ?? {});
    url = `${centaurBase(ctx)}/api/slack/files/${seg(args.file)}/download?${encode({ channel_id: channelId })}`;
    init = { headers: centaurHeaders(ctx) };
  } else {
    info = fileInfo((await slackGet(ctx, "files.info", { file: args.file.trim() })).file ?? {});
    const downloadUrl = info.url_private_download;
    if (!downloadUrl) throw new Error(`file ${args.file} has no download URL`);
    if (!downloadUrl.startsWith(`https://${FILES_HOST}/`)) {
      throw new Error(`file ${args.file} downloads from ${downloadUrl}, not https://${FILES_HOST}; refusing to send the token there`);
    }
    url = downloadUrl;
    init = { headers: { authorization: `Bearer ${token(ctx)}` } };
  }
  const path = outputPath(args.path, "slack", safeFilename(info.name ?? args.file));
  return { ...(await downloadTo(ctx, url, path, `downloading Slack file ${args.file}`, init)), file: info };
}

/**
 * Write: post a message. `channel` is a conversation ID or a user ID (U…) to DM them from the
 * bot; `text` is Slack mrkdwn; `thread_ts` replies in a thread. Direct access only.
 */
export async function send_message(
  args: { channel: string; text: string; thread_ts?: string; unfurl_links?: boolean },
  ctx: Ctx,
): Promise<{ channel: string; ts: string; message: Message }> {
  if (access(ctx) !== "direct") {
    throw new Error("send_message needs direct Slack access; the Centaur API does not post messages");
  }
  const { channel, ts, message } = await slackPost(ctx, "chat.postMessage", args);
  return { channel, ts, message };
}

/** Write: add an emoji reaction (with or without colons) to a message. */
export async function add_reaction(
  args: { channel: string; ts: string; emoji: string },
  ctx: Ctx,
): Promise<{ changed: boolean }> {
  const name = args.emoji.trim().replace(/^:|:$/g, "");
  if (access(ctx) === "centaur") {
    const path = `/api/slack/channels/${seg(args.channel)}/messages/${seg(args.ts)}/reactions/${seg(name)}`;
    const body = await centaur(ctx, path, {}, "POST");
    return { changed: body.added !== false };
  }
  try {
    await slackPost(ctx, "reactions.add", { channel: args.channel, timestamp: args.ts, name });
    return { changed: true };
  } catch (error) {
    if (String((error as Error).message).includes("already_reacted")) return { changed: false };
    throw error;
  }
}

/**
 * Write: upload a file from the sandbox (`path`, for example one saved by `download_file` or
 * `files.write`) and share it into `channel`, optionally in a thread. `filename` defaults to the
 * path's last segment; the file is streamed from disk, not read into the cell.
 *
 * @requires files
 */
export async function upload_file(
  args: { channel: string; path: string; filename?: string; title?: string; initial_comment?: string; thread_ts?: string },
  ctx: Ctx,
): Promise<UploadedFiles> {
  const files = requireFiles(ctx, "slack.upload_file");
  const stat = await files.stat(args.path);
  if (!stat) throw new Error(`no such file: ${args.path}`);
  const filename = args.filename ?? args.path.split(/[\\/]/).pop() ?? "file";
  const title = args.title ?? filename;
  const content_type = mimeFor(filename);
  const channel = args.channel.trim();
  if (access(ctx) === "centaur") {
    const params: Params = {
      channel_id: channel, filename, title, content_type,
      initial_comment: args.initial_comment, thread_ts: args.thread_ts,
    };
    const res = await ctx.fetch(`${centaurBase(ctx)}/api/slack/files/upload${query(params)}`, {
      method: "POST",
      headers: { ...centaurHeaders(ctx), "content-type": content_type },
      body: { file: args.path },
    });
    const body = await readJson(res, "Centaur API /api/slack/files/upload");
    const file = fileInfo(body.file ?? {});
    return { file_ids: [typeof body.file_id === "string" ? body.file_id : file.id], files: [file] };
  }
  const ticket = await slackPost(ctx, "files.getUploadURLExternal", { filename, length: stat.bytes });
  const uploadUrl = typeof ticket.upload_url === "string" ? ticket.upload_url : "";
  const fileId = typeof ticket.file_id === "string" ? ticket.file_id : "";
  if (!uploadUrl || !fileId) throw new Error("Slack files.getUploadURLExternal returned no upload_url");
  if (!uploadUrl.startsWith(`https://${FILES_HOST}/`)) {
    throw new Error(`Slack gave an upload URL at ${uploadUrl}, not https://${FILES_HOST}; refusing to upload there`);
  }
  // The upload URL is presigned: no bearer token goes to the file host.
  const upload = await ctx.fetch(uploadUrl, {
    method: "POST",
    headers: { "content-type": content_type },
    body: { file: args.path },
  });
  if (!upload.ok) {
    throw new Error(`uploading ${filename} returned HTTP ${upload.status}: ${(await upload.text()).slice(0, 500)}`);
  }
  const completed = await slackPost(ctx, "files.completeUploadExternal", {
    files: JSON.stringify([{ id: fileId, title }]),
    channel_id: channel, thread_ts: args.thread_ts, initial_comment: args.initial_comment,
  });
  const uploaded = ((completed.files ?? []) as Body[]).map(fileInfo);
  const ids = uploaded.map((file) => file.id).filter((id) => id);
  return { file_ids: ids.length > 0 ? ids : [fileId], files: uploaded };
}
