import type { TelegramMessage, TelegramUser } from "@chat-adapter/telegram";
import type { Logger } from "chat";
import type { TelegramTrigger, TelegrambotOptions } from "./types";

/** The only command that starts a turn. */
export const ASK_COMMAND = "/ask";

/** Bot API fields the adapter's TelegramMessage type does not declare. */
export type TelegramPolicyMessage = TelegramMessage & {
  is_automatic_forward?: boolean;
  quote?: { text?: string };
  via_bot?: TelegramUser;
};

export type TelegramPolicyOptions = Pick<
  TelegrambotOptions,
  "chatAllowlist" | "userAllowlist"
>;

/**
 * Decode a Chat SDK Telegram thread key `telegram:{chatId}[:{topicId}]`.
 * Returns an empty object for anything else (including business keys, which
 * this service never enables).
 */
export function parseTelegramThreadKey(threadKey: string): {
  chatId?: string;
  topicId?: string;
} {
  const parts = threadKey.split(":");
  if (parts[0] !== "telegram" || parts.length < 2 || parts.length > 3) {
    return {};
  }
  const chatId = parts[1];
  if (!chatId || !/^-?\d+$/.test(chatId)) return {};
  const topicId = parts[2];
  if (topicId !== undefined && !/^\d+$/.test(topicId)) return {};
  return { chatId, topicId };
}

/**
 * Authorization gate for inbound Telegram messages.
 *
 * Fail-closed like discordbot's guild gate: api-rs trusts the ingress, so this
 * is the primary authorization boundary. A Telegram bot is reachable by anyone
 * who knows its username and there is no workspace or guild boundary, so:
 *
 * - private chats are allowed only for allowlisted user ids;
 * - groups and supergroups are allowed only for allowlisted chat ids, and
 *   only from senders who are also on the user allowlist;
 * - channels, edits, bot authors, inline-bot relays (`via_bot`), and
 *   messages without a human `from` are denied;
 * - `sender_chat` identities (anonymous group admins, posting "as" a channel)
 *   and linked-channel auto-forwards are denied, because allowlisting a
 *   discussion group does not vouch for the channel identity behind them.
 *
 * Privacy mode only minimizes what Telegram delivers; it is not authorization,
 * so every check runs regardless of how the bot is configured in BotFather.
 */
export function isAllowedTelegramMessage(
  raw: TelegramPolicyMessage,
  options: TelegramPolicyOptions,
  botUserId: string | undefined,
  logger: Logger,
): boolean {
  const chatId = String(raw.chat.id);
  const fields = { chat_id: chatId, message_id: raw.message_id };
  const from = raw.from;

  if (from && botUserId && String(from.id) === botUserId) {
    // Self-echo; routine, so no warn.
    return false;
  }
  if (raw.edit_date !== undefined) {
    logger.info("telegrambot_message_ignored_edit", fields);
    return false;
  }
  if (raw.sender_chat) {
    logger.warn("telegrambot_message_ignored_sender_chat", {
      ...fields,
      sender_chat_id: String(raw.sender_chat.id),
    });
    return false;
  }
  if (raw.is_automatic_forward) {
    logger.warn("telegrambot_message_ignored_automatic_forward", fields);
    return false;
  }
  if (!from) {
    logger.warn("telegrambot_message_ignored_missing_sender", fields);
    return false;
  }
  if (from.is_bot) {
    logger.warn("telegrambot_message_ignored_bot_author", {
      ...fields,
      user_id: String(from.id),
    });
    return false;
  }
  if (raw.via_bot) {
    logger.warn("telegrambot_message_ignored_via_bot", {
      ...fields,
      user_id: String(from.id),
    });
    return false;
  }

  const chatType = raw.chat.type;
  if (chatType === "private") {
    const allowlist = options.userAllowlist ?? [];
    if (allowlist.length === 0) {
      logger.warn("telegrambot_message_ignored_allowlist_empty", {
        ...fields,
        chat_type: chatType,
      });
      return false;
    }
    if (!allowlist.includes(String(from.id))) {
      logger.warn("telegrambot_message_ignored_user_not_allowlisted", {
        ...fields,
        user_id: String(from.id),
      });
      return false;
    }
    return true;
  }

  if (chatType === "group" || chatType === "supergroup") {
    const allowlist = options.chatAllowlist ?? [];
    if (allowlist.length === 0) {
      logger.warn("telegrambot_message_ignored_allowlist_empty", {
        ...fields,
        chat_type: chatType,
      });
      return false;
    }
    if (!allowlist.includes(chatId)) {
      logger.warn("telegrambot_message_ignored_chat_not_allowlisted", fields);
      return false;
    }
    // Group membership spreads through invite links, so an allowlisted chat
    // does not vouch for its members; the sender must be allowlisted too.
    if (!(options.userAllowlist ?? []).includes(String(from.id))) {
      logger.warn("telegrambot_message_ignored_user_not_allowlisted", {
        ...fields,
        user_id: String(from.id),
      });
      return false;
    }
    return true;
  }

  logger.warn("telegrambot_message_ignored_unsupported_chat_type", {
    ...fields,
    chat_type: chatType,
  });
  return false;
}

/**
 * Whether an already-allowed, non-command message starts a turn.
 *
 * Every DM message does. In a group only a reply to one of the bot's own
 * messages does; a plain textual `@botname` mention deliberately does not.
 * With privacy mode on (BotFather's default) Telegram does not reliably
 * deliver mention-only messages to a bot, so treating them as a trigger
 * would be best-effort; with privacy mode off it would turn ordinary group
 * chatter into a trigger surface. Groups use replies and `/ask@botname`.
 */
export function messageTrigger(
  raw: TelegramPolicyMessage,
  botUserId: string | undefined,
): TelegramTrigger | null {
  if (raw.chat.type === "private") return "dm";
  if (raw.chat.type !== "group" && raw.chat.type !== "supergroup") return null;
  return isReplyToBot(raw, botUserId) ? "reply" : null;
}

/**
 * True for an explicit reply to one of the bot's messages. In forum topics
 * every message carries `reply_to_message` pointing at the topic-creation
 * service message (whose id equals `message_thread_id`); that implicit reply
 * does not count.
 */
export function isReplyToBot(
  raw: TelegramPolicyMessage,
  botUserId: string | undefined,
): boolean {
  const replied = raw.reply_to_message;
  if (!botUserId || !replied?.from) return false;
  if (String(replied.from.id) !== botUserId) return false;
  return replied.message_id !== raw.message_thread_id;
}

export type TelegramCommandRoute = "ask" | "help" | "ignore";

/**
 * Route a slash command the adapter already matched to this bot (it drops
 * `/cmd@otherbot`). In a group a command must be addressed (`/ask@botname`):
 * a bare `/ask` may be meant for another bot in the same chat. In a DM the
 * bare form is fine. Commands other than `/ask` get the usage text.
 */
export function routeCommand(
  raw: TelegramPolicyMessage,
  command: string,
): TelegramCommandRoute {
  const isPrivate = raw.chat.type === "private";
  if (!isPrivate && !isAddressedCommand(raw)) return "ignore";
  return command.toLowerCase() === ASK_COMMAND ? "ask" : "help";
}

/** Whether the leading `bot_command` entity carries an `@botname` suffix. */
export function isAddressedCommand(raw: TelegramPolicyMessage): boolean {
  const hasText = raw.text !== undefined;
  const text = hasText ? raw.text : raw.caption;
  const entities = hasText ? raw.entities : raw.caption_entities;
  const entity = entities?.find(
    (candidate) => candidate.type === "bot_command" && candidate.offset === 0,
  );
  if (!text || !entity) return false;
  return text.slice(0, entity.length).includes("@");
}

const QUOTE_MAX_CHARS = 2000;

/**
 * Prefix the replied-to message as a quote when a user asks about someone
 * else's message (`/ask@botname` sent as a reply). Replies to the bot itself
 * are skipped: the session already holds the bot's answers. Only the text the
 * asking user explicitly pointed at is included, never its attachments.
 */
export function withReplyQuote(
  text: string,
  raw: TelegramPolicyMessage,
  botUserId: string | undefined,
): string {
  const replied = raw.reply_to_message;
  if (!replied || replied.message_id === raw.message_thread_id) return text;
  if (botUserId && String(replied.from?.id) === botUserId) return text;
  const quoted = (raw.quote?.text ?? replied.text ?? replied.caption ?? "")
    .trim()
    .slice(0, QUOTE_MAX_CHARS);
  if (!quoted) return text;
  const author =
    replied.from?.username ??
    replied.from?.first_name ??
    replied.sender_chat?.title ??
    "unknown";
  const quoteBlock = quoted
    .split("\n")
    .map((line) => `> ${line}`)
    .join("\n");
  return `In reply to ${author}:\n${quoteBlock}\n\n${text}`.trim();
}

/** Display name for the session principal: group title or DM user name. */
export function conversationName(
  raw: TelegramPolicyMessage,
): string | undefined {
  const chat = raw.chat;
  if (chat.title?.trim()) return chat.title.trim();
  const name = [chat.first_name, chat.last_name]
    .filter(Boolean)
    .join(" ")
    .trim();
  return name || chat.username || undefined;
}

export function isAllowlistEmpty(options: TelegramPolicyOptions): boolean {
  return (
    (options.chatAllowlist ?? []).length === 0 &&
    (options.userAllowlist ?? []).length === 0
  );
}
