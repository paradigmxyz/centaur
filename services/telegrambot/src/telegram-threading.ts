import type { TelegramChat, TelegramMessage } from "./types";

export type TelegramThreadKind = "private" | "chat";

export type ParsedTelegramThreadKey = {
  kind?: TelegramThreadKind;
  chatId?: string;
  messageThreadId?: number;
};

/**
 * Derive the durable typed thread key for an inbound message, or a rejection
 * reason recorded as the update's durable disposition.
 *
 * Telegram delta (vs discord-threading, where the Chat SDK adapter hands us a
 * ready-made key): the key encodes the chat kind so api-rs can derive a user
 * principal for DMs and a chat principal for groups without trusting mutable
 * metadata — `telegram:private:{chatId}[:{messageThreadId}]` and
 * `telegram:chat:{chatId}[:{messageThreadId}]`. Private `chat.id` is the
 * canonical user identity, so a private message is accepted only when the
 * sender provably *is* that chat: `from` is a real user, `sender_chat` is
 * absent, and `String(from.id) === String(chat.id)`. Telegram Business and
 * channel direct-message-topic conversations break that equality (the peer's
 * messages arrive with a `from` that differs from the owning chat), so the
 * identity check rejects them without needing their extra fields; channel
 * chats are rejected outright (channel posts are out of scope for v1).
 */
export function deriveThreadKey(
  message: TelegramMessage,
  botUserId: string,
): { threadKey: string } | { rejected: string } {
  const { chat, from } = message;

  if (from && String(from.id) === String(botUserId)) {
    return { rejected: "self_message" };
  }
  if (chat.type === "channel") {
    return { rejected: "channel_chat" };
  }

  if (chat.type === "private") {
    if (!from) return { rejected: "private_missing_from" };
    if (from.is_bot) return { rejected: "private_bot_sender" };
    if (message.sender_chat) return { rejected: "private_sender_chat_present" };
    if (String(from.id) !== String(chat.id)) {
      return { rejected: "private_identity_mismatch" };
    }
    // Private forum-mode threads carry message_thread_id without
    // is_topic_message; preserve it whenever present.
    return {
      threadKey: buildThreadKey("private", chat.id, message.message_thread_id),
    };
  }

  if (chat.type === "group" || chat.type === "supergroup") {
    return {
      threadKey: buildThreadKey("chat", chat.id, groupTopicId(message)),
    };
  }

  return { rejected: "unsupported_chat_type" };
}

/**
 * Topic segment for group/supergroup keys. Non-forum supergroup replies also
 * carry `message_thread_id` (the reply-chain root), and keying on it would
 * split one chat's session per reply chain — reply-to-bot follow-ups must land
 * in the same session — so only genuine forum topics extend the key.
 */
function groupTopicId(message: TelegramMessage): number | undefined {
  return message.is_topic_message === true
    ? message.message_thread_id
    : undefined;
}

function buildThreadKey(
  kind: TelegramThreadKind,
  chatId: number,
  messageThreadId: number | undefined,
): string {
  return messageThreadId === undefined
    ? `telegram:${kind}:${chatId}`
    : `telegram:${kind}:${chatId}:${messageThreadId}`;
}

/**
 * Decode `telegram:{kind}:{chatId}[:{messageThreadId}]` into parts. Returns an
 * empty object for anything that is not a well-formed Telegram thread key
 * (mirrors parseDiscordThreadKey, but fail-closed on the typed discriminator
 * and a malformed topic segment since principals hang off these parts).
 */
export function parseTelegramThreadKey(
  threadKey: string,
): ParsedTelegramThreadKey {
  const parts = threadKey.split(":");
  if (parts[0] !== "telegram") return {};
  const kind = parts[1];
  if (kind !== "private" && kind !== "chat") return {};
  const chatId = parts[2];
  if (!chatId || parts.length > 4) return {};
  const rawThreadId = parts[3];
  if (rawThreadId === undefined) return { kind, chatId };
  const messageThreadId = Number(rawThreadId);
  if (!/^\d+$/.test(rawThreadId) || !Number.isSafeInteger(messageThreadId)) {
    return {};
  }
  return { kind, chatId, messageThreadId };
}

/**
 * Human-readable conversation name for the session principal: `chat.title`
 * for groups, the person's name/username for DMs, falling back to the id so
 * the principal is never nameless.
 */
export function deriveConversationName(chat: TelegramChat): string {
  if (chat.type === "private") {
    const fullName = [chat.first_name, chat.last_name]
      .map((part) => part?.trim())
      .filter(Boolean)
      .join(" ");
    if (fullName && chat.username) return `${fullName} (@${chat.username})`;
    if (fullName) return fullName;
    if (chat.username) return `@${chat.username}`;
    return String(chat.id);
  }
  return chat.title?.trim() || String(chat.id);
}

/**
 * Stable append/execute idempotency key. `chat.id` is globally unique and
 * `message_id` is unique within a chat, so re-delivered updates (crash between
 * receipt and append, duplicate getUpdates batches) dedupe to one session
 * message and one execution.
 */
export function deriveClientMessageId(message: TelegramMessage): string {
  return `telegram:${message.chat.id}:${message.message_id}`;
}
