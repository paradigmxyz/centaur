import type { Ctx } from "../centaur";

// Telegram Bot API objects are passed through as the API sends them; the
// interfaces list fields worth knowing. User-account (MTProto/Telethon)
// operations are not available here: only the Bot HTTP API is reachable.

const DEFAULT_BASE = "https://api.telegram.org";

/** A Telegram user (the bot itself or a message sender). */
export interface User {
  id: number;
  is_bot: boolean;
  first_name: string;
  last_name?: string;
  username?: string;
  [field: string]: unknown;
}

/** A Telegram chat, group or channel. */
export interface Chat {
  id: number;
  type: string;
  title?: string;
  username?: string;
  first_name?: string;
  last_name?: string;
  description?: string;
  invite_link?: string;
  [field: string]: unknown;
}

/** A Telegram message. */
export interface Message {
  message_id: number;
  date: number;
  chat: Chat;
  from?: User;
  text?: string;
  caption?: string;
  reply_to_message?: Message;
  forward_origin?: unknown;
  [field: string]: unknown;
}

/** One update from `getUpdates`; usually `message`, `edited_message`, `channel_post` or `callback_query`. */
export interface Update {
  update_id: number;
  message?: Message;
  edited_message?: Message;
  channel_post?: Message;
  edited_channel_post?: Message;
  [field: string]: unknown;
}

/** Webhook status from `getWebhookInfo`. */
export interface WebhookInfo {
  url: string;
  has_custom_certificate: boolean;
  pending_update_count: number;
  ip_address?: string;
  last_error_date?: number;
  last_error_message?: string;
  max_connections?: number;
  allowed_updates?: string[];
  [field: string]: unknown;
}

function endpoint(ctx: Ctx, method: string): string {
  const token = ctx.env.TELEGRAM_BOT_TOKEN;
  if (!token) throw new Error("TELEGRAM_BOT_TOKEN is not set");
  const base = (ctx.env.TELEGRAM_API_URL || DEFAULT_BASE).replace(/\/+$/, "");
  // iron-proxy swaps the placeholder token in the path on the wire.
  return `${base}/bot${token}/${method}`;
}

async function call(ctx: Ctx, method: string, params: Record<string, unknown> = {}): Promise<any> {
  const res = await ctx.fetch(endpoint(ctx, method), {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/json" },
    body: JSON.stringify(params),
  });
  const text = await res.text();
  let body: any = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (body && body.ok === false) {
    const code = body.error_code ?? res.status;
    throw new Error(`Telegram ${method} failed (${code}): ${body.description ?? text}`);
  }
  if (!res.ok) throw new Error(`Telegram ${method} returned HTTP ${res.status}: ${text.slice(0, 1000)}`);
  if (body === null) throw new Error(`Telegram ${method} returned non-JSON: ${text.slice(0, 1000)}`);
  return body.result;
}

/** The bot's own account: id, username, first_name, can_join_groups and can_read_all_group_messages. */
export async function get_me(args: {}, ctx: Ctx): Promise<{ bot: User }> {
  return { bot: (await call(ctx, "getMe")) as User };
}

/**
 * Send a text message. `chat_id` is a numeric ID (or `@channelusername`); `text` is 1-4096
 * characters. `parse_mode` is `MarkdownV2`, `Markdown` (legacy) or `HTML`; `reply_to_message_id`
 * replies in the same chat. Returns Telegram's Message object.
 */
export async function send_message(
  args: {
    chat_id: number | string;
    text: string;
    parse_mode?: "Markdown" | "MarkdownV2" | "HTML";
    reply_to_message_id?: number;
  },
  ctx: Ctx,
): Promise<{ message: Message }> {
  const message = await call(ctx, "sendMessage", {
    chat_id: args.chat_id,
    text: args.text,
    parse_mode: args.parse_mode,
    reply_to_message_id: args.reply_to_message_id,
  });
  return { message: message as Message };
}

/**
 * Updates (messages sent to the bot) via long polling. `offset` acknowledges every update with a
 * lower id and starts after it; `limit` is 1-100 (default 100); `timeout` is seconds the server may
 * hold the request open (0 returns immediately; keep it under ~50 to stay inside the 60 s call
 * limit). Returns raw Update objects; empty while a webhook is set.
 */
export async function get_updates(
  args: { offset?: number; limit?: number; timeout?: number },
  ctx: Ctx,
): Promise<{ updates: Update[] }> {
  const updates = await call(ctx, "getUpdates", {
    offset: args.offset,
    limit: Math.max(1, Math.min(args.limit ?? 100, 100)),
    timeout: args.timeout ?? 0,
  });
  return { updates: updates as Update[] };
}

/**
 * Chat, group or channel info by numeric ID or `@channelusername`: id, type, title, username,
 * names, description, invite_link, pinned_message and more.
 */
export async function get_chat(
  args: { chat_id: number | string },
  ctx: Ctx,
): Promise<{ chat: Chat }> {
  return { chat: (await call(ctx, "getChat", { chat_id: args.chat_id })) as Chat };
}

/** Number of members in a group or channel. */
export async function get_chat_member_count(
  args: { chat_id: number | string },
  ctx: Ctx,
): Promise<{ member_count: number }> {
  return {
    member_count: (await call(ctx, "getChatMemberCount", { chat_id: args.chat_id })) as number,
  };
}

/**
 * Forward a message unchanged from `from_chat_id` to `chat_id`; returns the new Message. Bots can
 * forward from chats they can read.
 */
export async function forward_message(
  args: { chat_id: number | string; from_chat_id: number | string; message_id: number },
  ctx: Ctx,
): Promise<{ message: Message }> {
  const message = await call(ctx, "forwardMessage", {
    chat_id: args.chat_id,
    from_chat_id: args.from_chat_id,
    message_id: args.message_id,
  });
  return { message: message as Message };
}

/**
 * Delete a message. Bots can delete their own messages, and, with admin rights, other messages in
 * the chat.
 */
export async function delete_message(
  args: { chat_id: number | string; message_id: number },
  ctx: Ctx,
): Promise<{ deleted: boolean }> {
  const deleted = await call(ctx, "deleteMessage", {
    chat_id: args.chat_id,
    message_id: args.message_id,
  });
  return { deleted: deleted === true };
}

/** Register an HTTPS webhook URL for updates (this stops long polling). */
export async function set_webhook(args: { url: string }, ctx: Ctx): Promise<{ set: boolean }> {
  return { set: (await call(ctx, "setWebhook", { url: args.url })) === true };
}

/** Remove the webhook and return to long polling. */
export async function delete_webhook(args: {}, ctx: Ctx): Promise<{ deleted: boolean }> {
  return { deleted: (await call(ctx, "deleteWebhook")) === true };
}

/**
 * Webhook status: `url` (empty when polling), `has_custom_certificate`, `pending_update_count`,
 * `last_error_date`/`last_error_message` and `allowed_updates`.
 */
export async function get_webhook_info(args: {}, ctx: Ctx): Promise<{ webhook: WebhookInfo }> {
  return { webhook: (await call(ctx, "getWebhookInfo")) as WebhookInfo };
}
