import type { Ctx } from "../centaur";

const SLACK = "https://slack.com/api";
const DEFAULT_CENTAUR_API = "http://api:8000";

/**
 * How Slack is reached, chosen from the calling context. `direct`: slack.com with the linked
 * Slack user token, when the caller has a live `slack` OAuth credential. `centaur`: Centaur's
 * API, scoped to the caller's channel grants. Every function picks the path automatically.
 */
export type Access = "direct" | "centaur";

export interface Message {
  ts: string;
  thread_ts: string | null;
  user: string | null;
  bot_id: string | null;
  subtype: string | null;
  text: string;
  reply_count: number | null;
  reactions: { name: string; count: number }[];
  files: { id: string; name: string | null; mimetype: string | null; size: number | null }[];
}

export interface Channel {
  id: string;
  name: string | null;
  is_private: boolean | null;
  is_im: boolean | null;
  is_mpim: boolean | null;
  is_member: boolean | null;
  num_members: number | null;
  topic: string | null;
  purpose: string | null;
  /** Only through the Centaur API: the caller's grants on this channel. */
  can_read_history: boolean | null;
  can_upload: boolean | null;
  can_download: boolean | null;
}

export interface User {
  id: string;
  name: string;
  real_name: string | null;
  display_name: string | null;
  email: string | null;
  title: string | null;
  tz: string | null;
  is_bot: boolean;
  deleted: boolean;
}

export interface SearchMatch {
  channel_id: string | null;
  channel_name: string | null;
  ts: string;
  user: string | null;
  username: string | null;
  text: string;
  permalink: string | null;
}

type Raw = Record<string, any>;
type Params = Record<string, string | number | boolean | undefined | null>;

// Routing only: iron-proxy injects the user token and the API checks grants either way.
function access(ctx: Ctx): Access {
  const credentials = ctx.context?.sandbox?.oauth_credentials ?? [];
  return credentials.some((c) => c.provider === "slack" && c.status === "live") ? "direct" : "centaur";
}

function encode(params: Params): string {
  return Object.entries(params)
    .filter(([, value]) => value !== undefined && value !== null && value !== "")
    .map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`)
    .join("&");
}

function token(ctx: Ctx): string {
  const value = ctx.env.SLACK_BOT_TOKEN;
  if (!value) throw new Error("SLACK_BOT_TOKEN is not set for direct Slack access");
  return value;
}

async function slackResult(res: Awaited<ReturnType<Ctx["fetch"]>>, method: string): Promise<Raw> {
  const text = await res.text();
  if (!res.ok) {
    const retry = res.headers["retry-after"];
    throw new Error(`Slack ${method} returned HTTP ${res.status}${retry ? ` (Retry-After: ${retry}s)` : ""}: ${text.slice(0, 1000)}`);
  }
  const body = JSON.parse(text);
  if (!body.ok) throw new Error(`Slack ${method} failed: ${body.error ?? text.slice(0, 1000)}`);
  return body;
}

async function slackGet(ctx: Ctx, method: string, params: Params = {}): Promise<Raw> {
  const qs = encode(params);
  const res = await ctx.fetch(`${SLACK}/${method}${qs ? `?${qs}` : ""}`, {
    headers: { authorization: `Bearer ${token(ctx)}` },
  });
  return slackResult(res, method);
}

async function slackPost(ctx: Ctx, method: string, params: Params): Promise<Raw> {
  const res = await ctx.fetch(`${SLACK}/${method}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${token(ctx)}`,
      "content-type": "application/x-www-form-urlencoded",
    },
    body: encode(params),
  });
  return slackResult(res, method);
}

function centaurBase(ctx: Ctx): string {
  return (ctx.env.CENTAUR_API_URL || DEFAULT_CENTAUR_API).replace(/\/+$/, "");
}

function centaurHeaders(ctx: Ctx, extra: Record<string, string> = {}): Record<string, string> {
  const headers: Record<string, string> = { accept: "application/json", ...extra };
  // Behind iron-proxy the API credential is injected for the API host.
  const bearer = ctx.env.CENTAUR_API_BEARER_TOKEN;
  if (bearer) headers.authorization = `Bearer ${bearer}`;
  return headers;
}

async function centaur(
  ctx: Ctx,
  path: string,
  params: Params = {},
  init: { method?: string; body?: Uint8Array; contentType?: string } = {},
): Promise<Raw> {
  const qs = encode(params);
  const res = await ctx.fetch(`${centaurBase(ctx)}${path}${qs ? `?${qs}` : ""}`, {
    method: init.method ?? "GET",
    headers: centaurHeaders(ctx, init.contentType ? { "content-type": init.contentType } : {}),
    body: init.body,
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Centaur API ${path} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  return JSON.parse(text);
}

const seg = (value: string): string => encodeURIComponent(value.trim());
const str = (value: any): string | null => (typeof value === "string" && value !== "" ? value : null);
const num = (value: any): number | null => (typeof value === "number" ? value : null);
const bool = (value: any): boolean | null => (typeof value === "boolean" ? value : null);
const nextCursor = (raw: Raw): string | null => str(raw.response_metadata?.next_cursor);

/** Slack ts, YYYY-MM-DD (UTC midnight) or ISO time to a Slack ts. */
function slackTs(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const raw = value.trim();
  if (/^\d+(\.\d+)?$/.test(raw)) return raw;
  const millis = Date.parse(/^\d{4}-\d{2}-\d{2}$/.test(raw) ? `${raw}T00:00:00Z` : raw);
  if (Number.isNaN(millis)) throw new Error(`timestamp ${JSON.stringify(value)} is not a Slack ts, YYYY-MM-DD or ISO time`);
  return (millis / 1000).toFixed(6);
}

function message(raw: Raw): Message {
  return {
    ts: raw.ts,
    thread_ts: str(raw.thread_ts),
    user: str(raw.user),
    bot_id: str(raw.bot_id),
    subtype: str(raw.subtype),
    text: raw.text ?? "",
    reply_count: num(raw.reply_count),
    reactions: (raw.reactions ?? []).map((r: Raw) => ({ name: r.name, count: r.count ?? 0 })),
    files: (raw.files ?? []).map((f: Raw) => ({
      id: f.id, name: str(f.name), mimetype: str(f.mimetype), size: num(f.size),
    })),
  };
}

function channel(raw: Raw): Channel {
  const text = (value: any): string | null => str(typeof value === "string" ? value : value?.value);
  return {
    id: raw.id,
    name: str(raw.name),
    is_private: bool(raw.is_private),
    is_im: bool(raw.is_im),
    is_mpim: bool(raw.is_mpim),
    is_member: bool(raw.is_member),
    num_members: num(raw.num_members ?? raw.member_count),
    topic: text(raw.topic),
    purpose: text(raw.purpose),
    can_read_history: bool(raw.can_read_history),
    can_upload: bool(raw.can_upload),
    can_download: bool(raw.can_download),
  };
}

function user(raw: Raw): User {
  const profile = raw.profile ?? {};
  return {
    id: raw.id,
    name: raw.name ?? "",
    real_name: str(raw.real_name ?? profile.real_name),
    display_name: str(profile.display_name),
    email: str(profile.email),
    title: str(profile.title),
    tz: str(raw.tz),
    is_bot: Boolean(raw.is_bot),
    deleted: Boolean(raw.deleted),
  };
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
): Promise<{ total: number; matches: SearchMatch[]; page: number | null; pages: number | null }> {
  if (access(ctx) !== "direct") {
    throw new Error("search_messages needs a linked Slack user token (direct access); this caller reaches Slack through the Centaur API");
  }
  const raw = await slackGet(ctx, "search.messages", {
    query: args.query, sort: args.sort, sort_dir: args.sort_dir, count: args.count, page: args.page,
  });
  const messages = raw.messages ?? {};
  return {
    total: messages.total ?? 0,
    matches: (messages.matches ?? []).map((m: Raw) => ({
      channel_id: str(m.channel?.id),
      channel_name: str(m.channel?.name),
      ts: m.ts,
      user: str(m.user),
      username: str(m.username),
      text: m.text ?? "",
      permalink: str(m.permalink),
    })),
    page: num(messages.paging?.page),
    pages: num(messages.paging?.pages),
  };
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
    const raw = await centaur(ctx, "/api/slack/channels", {
      limit, cursor: args.cursor, query: args.query, history_only: args.readable_only ?? false,
    });
    return { channels: (raw.channels ?? []).map(channel), next_cursor: nextCursor(raw) };
  }
  const needle = args.query?.trim().toLowerCase();
  const method = args.readable_only ? "users.conversations" : "conversations.list";
  const found: Channel[] = [];
  let cursor = args.cursor;
  for (;;) {
    const raw = await slackGet(ctx, method, {
      limit, cursor, types: "public_channel,private_channel", exclude_archived: true,
    });
    for (const c of (raw.channels ?? []).map(channel)) {
      if (!needle || c.name?.toLowerCase().includes(needle)) found.push(c);
    }
    cursor = nextCursor(raw) ?? undefined;
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
  const raw = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/channels/${seg(args.channel)}/history`, params)
    : await slackGet(ctx, "conversations.history", { ...params, channel: args.channel });
  return { messages: (raw.messages ?? []).map(message), has_more: Boolean(raw.has_more), next_cursor: nextCursor(raw) };
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
  const raw = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/channels/${seg(args.channel)}/threads/${seg(args.ts)}/replies`, params)
    : await slackGet(ctx, "conversations.replies", { ...params, channel: args.channel, ts: args.ts });
  return { messages: (raw.messages ?? []).map(message), has_more: Boolean(raw.has_more), next_cursor: nextCursor(raw) };
}

/** A message's permalink by conversation ID and ts. */
export async function permalink(args: { channel: string; ts: string }, ctx: Ctx): Promise<{ permalink: string }> {
  const raw = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/channels/${seg(args.channel)}/messages/${seg(args.ts)}/permalink`)
    : await slackGet(ctx, "chat.getPermalink", { channel: args.channel, message_ts: args.ts });
  return { permalink: raw.permalink };
}

/**
 * Workspace users with handle, names, email, title and timezone. `query` filters on handle, real
 * name, display name, email or title (case-insensitive), paging until `limit` matches.
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
    const raw = access(ctx) === "centaur"
      ? await centaur(ctx, "/api/slack/users", params)
      : await slackGet(ctx, "users.list", params);
    for (const u of (raw.members ?? []).map(user)) {
      const hay = [u.name, u.real_name, u.display_name, u.email, u.title];
      if (!needle || hay.some((v) => v?.toLowerCase().includes(needle))) found.push(u);
    }
    cursor = nextCursor(raw) ?? undefined;
    if (!cursor || found.length >= limit) return { users: found.slice(0, limit), next_cursor: cursor ?? null };
  }
}

/** One user's profile by user ID (U… or W…). */
export async function user_details(args: { user: string }, ctx: Ctx): Promise<{ user: User }> {
  const raw = access(ctx) === "centaur"
    ? await centaur(ctx, `/api/slack/users/${seg(args.user)}`)
    : await slackGet(ctx, "users.info", { user: args.user.trim() });
  return { user: user(raw.user ?? {}) };
}

/**
 * Write: post a message. `channel` is a conversation ID or a user ID (U…) to DM them from the
 * bot; `text` is Slack mrkdwn; `thread_ts` replies in a thread. Direct access only.
 */
export async function send_message(
  args: { channel: string; text: string; thread_ts?: string; unfurl_links?: boolean },
  ctx: Ctx,
): Promise<{ channel: string; ts: string }> {
  if (access(ctx) !== "direct") {
    throw new Error("send_message needs direct Slack access; the Centaur API does not post messages");
  }
  const raw = await slackPost(ctx, "chat.postMessage", {
    channel: args.channel.trim(), text: args.text, thread_ts: args.thread_ts, unfurl_links: args.unfurl_links,
  });
  return { channel: raw.channel, ts: raw.ts };
}

/** Write: add an emoji reaction (with or without colons) to a message. */
export async function add_reaction(
  args: { channel: string; ts: string; emoji: string },
  ctx: Ctx,
): Promise<{ changed: boolean }> {
  const name = args.emoji.trim().replace(/^:|:$/g, "");
  if (access(ctx) === "centaur") {
    const raw = await centaur(
      ctx, `/api/slack/channels/${seg(args.channel)}/messages/${seg(args.ts)}/reactions/${seg(name)}`, {},
      { method: "POST" },
    );
    return { changed: raw.added !== false };
  }
  try {
    await slackPost(ctx, "reactions.add", { channel: args.channel, timestamp: args.ts, name });
    return { changed: true };
  } catch (error) {
    if (String((error as Error).message).includes("already_reacted")) return { changed: false };
    throw error;
  }
}
