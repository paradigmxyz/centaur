import type { Ctx, Response } from "../centaur";
import { query, readJson, encode, type Params } from "../_lib/http";

// Results are Slack's own objects, passed through. The interfaces list the
// fields worth knowing; Slack sends more.

const SLACK = "https://slack.com/api";
const DEFAULT_CENTAUR_API = "http://api:8000";

/**
 * How Slack is reached, chosen from the calling context. `direct`: slack.com with the linked
 * Slack user token, when the caller has a live `slack` OAuth credential. `centaur`: Centaur's
 * API, scoped to the caller's channel grants. Every function picks the path automatically.
 */
export type Access = "direct" | "centaur";

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

async function centaur(ctx: Ctx, path: string, params: Params = {}, method = "GET"): Promise<Body> {
  const base = (ctx.env.CENTAUR_API_URL || DEFAULT_CENTAUR_API).replace(/\/+$/, "");
  const headers: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the API credential is injected for the API host.
  if (ctx.env.CENTAUR_API_BEARER_TOKEN) headers.authorization = `Bearer ${ctx.env.CENTAUR_API_BEARER_TOKEN}`;
  const res = await ctx.fetch(`${base}${path}${query(params)}`, { method, headers });
  return readJson(res, `Centaur API ${path}`);
}

const seg = (value: string): string => encodeURIComponent(value.trim());
const nextCursor = (body: Body): string | null => body.response_metadata?.next_cursor || null;

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
export async function get_access(args: {}, ctx: Ctx): Promise<{ access: Access; thread_key: string | null }> {
  return { access: access(ctx), thread_key: ctx.context?.thread_key ?? null };
}

/**
 * Slack's native search across the workspace. `query` takes Slack search syntax (`in:#channel`,
 * `from:@user`, `before:`, `after:`, `has:link`). `sort` is score or timestamp; `count` at most
 * 100. Needs direct access (a linked Slack user token); fails through the Centaur API.
 */
export async function search_messages(
  args: { query: string; sort?: "score" | "timestamp"; sort_dir?: "asc" | "desc"; count?: number; page?: number },
  ctx: Ctx,
): Promise<{ total: number; matches: SearchMatch[]; paging?: { count: number; total: number; page: number; pages: number } }> {
  if (access(ctx) !== "direct") {
    throw new Error("search_messages needs a linked Slack user token (direct access); this caller reaches Slack through the Centaur API");
  }
  const { messages } = await slackGet(ctx, "search.messages", args);
  return { total: messages?.total ?? 0, matches: messages?.matches ?? [], paging: messages?.paging };
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
