import type { Ctx, Response } from "../centaur";
import { query } from "../_lib/http";

// Discord's own REST objects are passed through; the interfaces list the
// fields worth knowing. Guild and channel names are resolved to IDs client-side
// because agents usually know names, not snowflakes.

const DEFAULT_BASE = "https://discord.com/api/v10";

// A descriptive bot User-Agent keeps guild/channel reads working under a bot
// token; browser UAs trip Discord's edge anti-automation checks.
const USER_AGENT = "DiscordBot (https://github.com/paradigmxyz/centaur, 0.1.0)";

// Message flag that suppresses link/embed unfurling (1 << 2).
const SUPPRESS_EMBEDS = 1 << 2;

const PUBLIC_THREAD_TYPE = 11;
const PRIVATE_THREAD_TYPE = 12;

// Discord attachment URLs only ever point at these hosts.
const CDN_HOSTS = ["cdn.discordapp.com", "media.discordapp.net"];

/** A Discord user (the token's user or a message author). */
export interface User {
  id: string;
  username: string;
  discriminator?: string;
  global_name?: string | null;
  avatar?: string | null;
  bot?: boolean;
  [field: string]: unknown;
}

/** A partial guild object from `GET /users/@me/guilds`. */
export interface Guild {
  id: string;
  name: string;
  icon?: string | null;
  owner?: boolean;
  permissions?: string;
  features?: string[];
  approximate_member_count?: number;
  approximate_presence_count?: number;
  [field: string]: unknown;
}

/** A Discord channel object; threads are channels with `type` 10-12 too. */
export interface Channel {
  id: string;
  type: number;
  name?: string | null;
  guild_id?: string;
  parent_id?: string | null;
  position?: number;
  topic?: string | null;
  nsfw?: boolean;
  thread_metadata?: {
    archived?: boolean;
    auto_archive_duration?: number;
    archive_timestamp?: string;
    locked?: boolean;
    [field: string]: unknown;
  };
  [field: string]: unknown;
}

/** A file attached to a message. */
export interface Attachment {
  id: string;
  filename: string;
  url: string;
  proxy_url?: string;
  size: number;
  content_type?: string;
  height?: number | null;
  width?: number | null;
  [field: string]: unknown;
}

/** A Discord message object. */
export interface Message {
  id: string;
  channel_id: string;
  author: User;
  content: string;
  timestamp: string;
  edited_timestamp?: string | null;
  message_reference?: { message_id?: string; channel_id?: string; guild_id?: string; [field: string]: unknown };
  attachments: Attachment[];
  embeds?: unknown[];
  [field: string]: unknown;
}

/** The invite object returned by joining a server. */
export interface Invite {
  code: string;
  guild?: Guild;
  channel?: Channel;
  [field: string]: unknown;
}

/** A downloaded attachment: metadata plus base64 bytes. */
export interface DownloadedFile {
  url: string;
  filename: string;
  content_type: string | null;
  size: number;
  content_base64: string;
}

function apiBase(ctx: Ctx): string {
  return (ctx.env.DISCORD_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
}

function authHeaders(ctx: Ctx): Record<string, string> {
  const headers: Record<string, string> = { "user-agent": USER_AGENT };
  // Behind iron-proxy the token is injected on the wire; locally it comes from the env.
  if (ctx.env.DISCORD_BOT_TOKEN) headers.authorization = ctx.env.DISCORD_BOT_TOKEN;
  return headers;
}

function errorMessage(status: number, text: string): string {
  let message = text;
  try {
    const body = JSON.parse(text);
    if (body && typeof body.message === "string") message = body.message;
  } catch {
    // Keep the raw body.
  }
  return `Discord API error (${status}): ${message.slice(0, 1000)}`;
}

async function responseBody(res: Response): Promise<any> {
  if (res.status === 204) return {};
  const text = await res.text();
  if (!res.ok) throw new Error(errorMessage(res.status, text));
  return text ? JSON.parse(text) : {};
}

async function request(ctx: Ctx, method: string, endpoint: string, body?: unknown): Promise<any> {
  const headers = authHeaders(ctx);
  if (body !== undefined) headers["content-type"] = "application/json";
  for (let attempt = 0; ; attempt++) {
    const res = await ctx.fetch(`${apiBase(ctx)}${endpoint}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (res.status === 429 && attempt === 0) {
      const text = await res.text();
      let retryAfter = 0;
      try {
        retryAfter = Number(JSON.parse(text).retry_after ?? 0);
      } catch {
        retryAfter = 0;
      }
      if (retryAfter > 0 && retryAfter <= 30) {
        await ctx.sleep(retryAfter * 1000);
        continue;
      }
      throw new Error(errorMessage(429, text));
    }
    return responseBody(res);
  }
}

async function guilds(ctx: Ctx): Promise<Guild[]> {
  return (await request(ctx, "GET", "/users/@me/guilds")) as Guild[];
}

async function findGuild(ctx: Ctx, guild: string): Promise<Guild> {
  const needle = guild.trim();
  const lower = needle.toLowerCase();
  const all = await guilds(ctx);
  const byId = all.find((candidate) => String(candidate.id) === needle);
  if (byId) return byId;
  const exact = all.find((candidate) => (candidate.name || "").toLowerCase() === lower);
  if (exact) return exact;
  const partial = all.find((candidate) => (candidate.name || "").toLowerCase().includes(lower));
  if (partial) return partial;
  throw new Error(`Guild not found: ${guild}`);
}

async function guildChannels(ctx: Ctx, guildId: string): Promise<Channel[]> {
  return (await request(ctx, "GET", `/guilds/${encodeURIComponent(guildId)}/channels`)) as Channel[];
}

async function findChannel(ctx: Ctx, channel: string): Promise<Channel> {
  const trimmed = channel.trim();
  if (/^\d+$/.test(trimmed)) {
    return (await request(ctx, "GET", `/channels/${trimmed}`)) as Channel;
  }
  const needle = trimmed.replace(/^#+/, "").toLowerCase();
  let partial: Channel | undefined;
  for (const guild of await guilds(ctx)) {
    for (const candidate of await guildChannels(ctx, guild.id)) {
      if (candidate.type !== 0) continue;
      const name = (candidate.name || "").toLowerCase();
      if (name === needle) return candidate;
      if (!partial && name.includes(needle)) partial = candidate;
    }
  }
  if (partial) return partial;
  throw new Error(`Channel not found: ${channel}`);
}

function utf8(text: string): Uint8Array {
  const bytes: number[] = [];
  for (let index = 0; index < text.length; index++) {
    let code = text.charCodeAt(index);
    if (code < 0x80) {
      bytes.push(code);
    } else if (code < 0x800) {
      bytes.push(0xc0 | (code >> 6), 0x80 | (code & 0x3f));
    } else if (code >= 0xd800 && code <= 0xdbff && index + 1 < text.length) {
      const next = text.charCodeAt(++index);
      code = 0x10000 + ((code - 0xd800) << 10) + (next - 0xdc00);
      bytes.push(0xf0 | (code >> 18), 0x80 | ((code >> 12) & 0x3f), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
    } else {
      bytes.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
    }
  }
  return new Uint8Array(bytes);
}

function quoted(value: string): string {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/[\r\n]/g, " ");
}

function multipart(boundary: string, payloadJson: string, filename: string, contentType: string, file: Uint8Array): Uint8Array {
  const head = utf8(
    `--${boundary}\r\n` +
      `Content-Disposition: form-data; name="payload_json"\r\n` +
      `Content-Type: application/json\r\n\r\n` +
      `${payloadJson}\r\n` +
      `--${boundary}\r\n` +
      `Content-Disposition: form-data; name="files[0]"; filename="${quoted(filename)}"\r\n` +
      `Content-Type: ${contentType}\r\n\r\n`,
  );
  const tail = utf8(`\r\n--${boundary}--\r\n`);
  const body = new Uint8Array(head.length + file.length + tail.length);
  body.set(head, 0);
  body.set(file, head.length);
  body.set(tail, head.length + file.length);
  return body;
}

interface ParsedUrl {
  host: string;
  path: string;
}

function parseHttpsUrl(value: string): ParsedUrl | null {
  const match = /^https:\/\/([^/?#]+)(\/[^?#]*)?/i.exec(value.trim());
  if (!match) return null;
  return { host: match[1].toLowerCase().replace(/:\d+$/, ""), path: match[2] || "/" };
}

/**
 * The token's own user: id, username, discriminator, global_name and avatar. Read-only
 * connectivity/auth check.
 */
export async function get_me(args: {}, ctx: Ctx): Promise<{ user: User }> {
  return { user: (await request(ctx, "GET", "/users/@me")) as User };
}

/**
 * Join a server with an invite code or URL (`https://discord.gg/<code>`, `discord.com/invite/<code>`
 * or the bare code). Returns Discord's invite object, including `guild` for a server invite.
 */
export async function join_server(args: { invite: string }, ctx: Ctx): Promise<{ invite: Invite }> {
  const match = /^(?:https?:\/\/)?(?:discord(?:\.gg|\.com\/invite)\/)?([A-Za-z0-9-]+)$/.exec(
    args.invite.trim(),
  );
  if (!match) throw new Error("Provide a Discord invite code or URL.");
  return { invite: (await request(ctx, "POST", `/invites/${match[1]}`, {})) as Invite };
}

/**
 * Servers/guilds the token is a member of, with approximate member counts when Discord sends
 * them. `query` filters case-insensitively on name; `limit` caps results (default 100).
 */
export async function list_servers(
  args: { query?: string; limit?: number },
  ctx: Ctx,
): Promise<{ guilds: Guild[] }> {
  const needle = (args.query || "").trim().toLowerCase();
  const all = await guilds(ctx);
  const selected = needle
    ? all.filter((guild) => (guild.name || "").toLowerCase().includes(needle))
    : all;
  return { guilds: selected.slice(0, args.limit ?? 100) };
}

/**
 * Text channels (type 0) in a server, by guild ID or name (exact, then partial match). `query`
 * filters channel names case-insensitively, with or without a leading `#`.
 */
export async function list_channels(
  args: { guild: string; query?: string },
  ctx: Ctx,
): Promise<{ channels: Channel[] }> {
  const guild = await findGuild(ctx, args.guild);
  const needle = (args.query || "").trim().replace(/^#+/, "").toLowerCase();
  const channels = await guildChannels(ctx, guild.id);
  return {
    channels: channels.filter(
      (channel) => channel.type === 0 && (!needle || (channel.name || "").toLowerCase().includes(needle)),
    ),
  };
}

/**
 * Recent messages in a channel (name, `#name` or ID), newest first, as Discord's message
 * objects. `limit` is 1-100 (default 50). Attachments are in each message's `attachments`.
 */
export async function get_messages(
  args: { channel: string; limit?: number },
  ctx: Ctx,
): Promise<{ messages: Message[] }> {
  const channel = await findChannel(ctx, args.channel);
  const limit = Math.max(1, Math.min(args.limit ?? 50, 100));
  const messages = (await request(
    ctx,
    "GET",
    `/channels/${channel.id}/messages${query({ limit })}`,
  )) as Message[];
  return { messages };
}

/**
 * Case-insensitive substring search over a channel's recent messages, newest first. Pages
 * backwards 100 messages per request until `limit` matches (default 50) or the channel starts;
 * a rare term can therefore cost many requests.
 */
export async function search_messages(
  args: { query: string; channel: string; limit?: number },
  ctx: Ctx,
): Promise<{ messages: Message[] }> {
  const channel = await findChannel(ctx, args.channel);
  const needle = args.query.toLowerCase();
  const limit = args.limit ?? 50;
  const found: Message[] = [];
  let before: string | undefined;
  while (found.length < limit) {
    const page = (await request(
      ctx,
      "GET",
      `/channels/${channel.id}/messages${query({ limit: 100, before })}`,
    )) as Message[];
    if (page.length === 0) break;
    for (const message of page) {
      if ((message.content || "").toLowerCase().includes(needle)) {
        found.push(message);
        if (found.length >= limit) break;
      }
    }
    before = page[page.length - 1].id;
  }
  return { messages: found };
}

/**
 * Search every text channel in a server (guild name or ID) for a substring, newest first, up to
 * `limit` matches (default 50). Channels that deny history are skipped. Costs one request per
 * channel scanned.
 */
export async function search_all(
  args: { guild: string; query: string; limit?: number },
  ctx: Ctx,
): Promise<{ messages: Message[] }> {
  const guild = await findGuild(ctx, args.guild);
  const limit = args.limit ?? 50;
  const found: Message[] = [];
  for (const channel of (await guildChannels(ctx, guild.id)).filter((candidate) => candidate.type === 0)) {
    if (found.length >= limit) break;
    try {
      const result = await search_messages(
        { query: args.query, channel: channel.id, limit: limit - found.length },
        ctx,
      );
      found.push(...result.messages);
    } catch {
      // No history access in this channel; keep scanning.
    }
  }
  return { messages: found };
}

/**
 * Messages around one message, oldest first: the `before` earlier messages, the target, then the
 * `after` later messages. Both counts default to 10 and are capped at 100.
 */
export async function get_context(
  args: { channel: string; message_id: string; before?: number; after?: number },
  ctx: Ctx,
): Promise<{ messages: Message[] }> {
  const channel = await findChannel(ctx, args.channel);
  const id = args.message_id;
  const before = Math.max(0, Math.min(args.before ?? 10, 100));
  const after = Math.max(0, Math.min(args.after ?? 10, 100));
  const target = (await request(
    ctx,
    "GET",
    `/channels/${channel.id}/messages/${encodeURIComponent(id)}`,
  )) as Message;
  const earlier = before
    ? ((await request(
        ctx,
        "GET",
        `/channels/${channel.id}/messages${query({ limit: before, before: id })}`,
      )) as Message[])
    : [];
  const later = after
    ? ((await request(
        ctx,
        "GET",
        `/channels/${channel.id}/messages${query({ limit: after, after: id })}`,
      )) as Message[])
    : [];
  return { messages: [...earlier.reverse(), target, ...later.reverse()] };
}

/**
 * Post a message. `channel` is a name, `#name` or ID. `reply_to_message_id` makes it a reply;
 * `suppress_embeds` stops Discord unfurling link previews. Returns the created message.
 */
export async function post_message(
  args: { channel: string; content: string; reply_to_message_id?: string; suppress_embeds?: boolean },
  ctx: Ctx,
): Promise<{ message: Message }> {
  const channel = await findChannel(ctx, args.channel);
  const payload: Record<string, unknown> = { content: args.content };
  if (args.suppress_embeds) payload.flags = SUPPRESS_EMBEDS;
  if (args.reply_to_message_id) payload.message_reference = { message_id: args.reply_to_message_id };
  return {
    message: (await request(ctx, "POST", `/channels/${channel.id}/messages`, payload)) as Message,
  };
}

/**
 * Upload one file to a channel. `content_base64` holds the file bytes (standard base64; use
 * `ctx.base64` or a shell to encode them), `filename` is the name Discord shows, `content_type`
 * defaults to `application/octet-stream`, `content` is optional message text, and
 * `reply_to_message_id` replies to a message. Returns the created message.
 */
export async function upload_file(
  args: {
    channel: string;
    filename: string;
    content_base64: string;
    content?: string;
    content_type?: string;
    reply_to_message_id?: string;
  },
  ctx: Ctx,
): Promise<{ message: Message }> {
  const channel = await findChannel(ctx, args.channel);
  const payload: Record<string, unknown> = {};
  if (args.content) payload.content = args.content;
  if (args.reply_to_message_id) payload.message_reference = { message_id: args.reply_to_message_id };
  const boundary = `----centaur${Date.now().toString(16)}`;
  const body = multipart(
    boundary,
    JSON.stringify(payload),
    args.filename,
    args.content_type || "application/octet-stream",
    ctx.base64.decode(args.content_base64),
  );
  const headers = authHeaders(ctx);
  headers["content-type"] = `multipart/form-data; boundary=${boundary}`;
  const res = await ctx.fetch(`${apiBase(ctx)}/channels/${channel.id}/messages`, {
    method: "POST",
    headers,
    body,
  });
  return { message: (await responseBody(res)) as Message };
}

/**
 * Download a Discord CDN attachment URL and return its bytes base64-encoded. Only
 * `https://cdn.discordapp.com/...` and `https://media.discordapp.net/...` are accepted (the
 * message `attachments[].url` values); Discord CDN links are pre-signed, so no auth is sent.
 * Responses are capped at the runtime's 10 MiB body limit.
 */
export async function download_url(args: { url: string }, ctx: Ctx): Promise<{ file: DownloadedFile }> {
  const parsed = parseHttpsUrl(args.url);
  if (!parsed || !CDN_HOSTS.includes(parsed.host)) {
    throw new Error(
      "Discord downloads only accept https Discord CDN URLs " +
        `(cdn.discordapp.com / media.discordapp.net); refusing ${JSON.stringify(args.url)}`,
    );
  }
  const res = await ctx.fetch(args.url, { headers: { "user-agent": USER_AGENT } });
  if (!res.ok) throw new Error(`Discord download failed (${res.status}) for ${args.url}`);
  const bytes = await res.bytes();
  return {
    file: {
      url: args.url,
      filename: parsed.path.split("/").pop() || "download",
      content_type: res.headers["content-type"] || null,
      size: bytes.length,
      content_base64: ctx.base64.encode(bytes),
    },
  };
}

/**
 * Download every attachment on one message (channel name, `#name` or ID plus `message_id`) and
 * return each file's metadata with base64 bytes. Attachments without a URL are skipped; each file
 * is subject to the same CDN host allowlist and 10 MiB body limit as `download_url`.
 */
export async function download_message_attachments(
  args: { channel: string; message_id: string },
  ctx: Ctx,
): Promise<{ files: DownloadedFile[] }> {
  const channel = await findChannel(ctx, args.channel);
  const message = (await request(
    ctx,
    "GET",
    `/channels/${channel.id}/messages/${encodeURIComponent(args.message_id)}`,
  )) as Message;
  const files: DownloadedFile[] = [];
  for (const attachment of message.attachments || []) {
    if (!attachment.url) continue;
    const result = await download_url({ url: attachment.url }, ctx);
    files.push({ ...result.file, filename: attachment.filename || result.file.filename });
  }
  return { files };
}

/**
 * Create a thread in a channel (name, `#name` or ID). With `from_message_id` it branches a public
 * thread off that message; otherwise it creates a standalone thread (public by default, private
 * when `private` is true). `content`, when given for a standalone thread, is posted as the first
 * message. Returns Discord's thread channel object and that first message if posted.
 */
export async function create_thread(
  args: { channel: string; name: string; from_message_id?: string; content?: string; private?: boolean },
  ctx: Ctx,
): Promise<{ thread: Channel; message?: Message }> {
  const channel = await findChannel(ctx, args.channel);
  let thread: Channel;
  if (args.from_message_id) {
    thread = (await request(
      ctx,
      "POST",
      `/channels/${channel.id}/messages/${encodeURIComponent(args.from_message_id)}/threads`,
      { name: args.name },
    )) as Channel;
  } else {
    thread = (await request(ctx, "POST", `/channels/${channel.id}/threads`, {
      name: args.name,
      type: args.private ? PRIVATE_THREAD_TYPE : PUBLIC_THREAD_TYPE,
    })) as Channel;
  }
  if (args.content && !args.from_message_id) {
    const message = (await request(ctx, "POST", `/channels/${thread.id}/messages`, {
      content: args.content,
    })) as Message;
    return { thread, message };
  }
  return { thread };
}
