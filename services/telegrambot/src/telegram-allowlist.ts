import type { Logger, TelegramMessage, TelegrambotOptions } from "./types";

/** The only addressed command the bot answers in v1. */
const TRIGGER_COMMAND = "/ask";

export type TelegramTrigger = "dm" | "reply" | "command";

/**
 * Allowlist inputs plus the bot's own identity (from getMe), needed to reject
 * self-echoes before any allowlist logic runs.
 */
export type TelegramAllowlistOptions = Pick<
  TelegrambotOptions,
  "chatAllowlist" | "userAllowlist"
> & {
  botUserId: string;
};

/**
 * Authorization gate for inbound Telegram messages.
 *
 * Intentionally **fail-closed**, mirroring discord-allowlist: the api-rs
 * control plane has no ingress auth, so this guard is the primary
 * authorization boundary and empty allowlists render the bot inert.
 *
 * Telegram delta: a bot is world-reachable with no workspace/guild boundary,
 * so DMs are not open by default — a private chat is allowed only when the
 * sender's user id is explicitly allowlisted, and groups/supergroups only when
 * the chat id is. Channel posts are out of scope for v1 and always denied.
 * Bot-authored and bot-relayed (`via_bot`) messages are rejected even though
 * Telegram normally withholds other bots' messages — privacy mode is delivery
 * minimization, not authorization.
 */
export function isAllowedTelegramMessage(
  message: TelegramMessage,
  options: TelegramAllowlistOptions,
  logger: Logger,
): boolean {
  const { chat, from } = message;

  if (!from) {
    logger.warn("telegrambot_message_ignored_missing_sender", {
      chat_id: String(chat.id),
      message_id: message.message_id,
    });
    return false;
  }
  if (String(from.id) === String(options.botUserId)) {
    // Self-echo; routine, so no warn (matches discordbot's silent isMe gate).
    return false;
  }
  if (from.is_bot) {
    logger.warn("telegrambot_message_ignored_bot_author", {
      chat_id: String(chat.id),
      message_id: message.message_id,
      user_id: String(from.id),
    });
    return false;
  }
  if (message.via_bot) {
    logger.warn("telegrambot_message_ignored_via_bot", {
      chat_id: String(chat.id),
      message_id: message.message_id,
      user_id: String(from.id),
      via_bot_id: String(message.via_bot.id),
    });
    return false;
  }

  if (chat.type === "private") {
    const allowlist = resolveUserAllowlist(options);
    if (allowlist.length === 0) {
      logger.warn("telegrambot_message_ignored_allowlist_empty", {
        chat_type: chat.type,
        message_id: message.message_id,
        user_id: String(from.id),
      });
      return false;
    }
    if (!new Set(allowlist).has(String(from.id))) {
      logger.warn("telegrambot_message_ignored_user_not_allowlisted", {
        message_id: message.message_id,
        user_id: String(from.id),
      });
      return false;
    }
    return true;
  }

  if (chat.type === "group" || chat.type === "supergroup") {
    // Telegram delta: allowlisting a discussion group does not vouch for its
    // linked channel. Channel posts surface in the group as auto-forwards
    // (is_automatic_forward) and anonymous-admin/channel-identity messages
    // carry sender_chat — both bypass per-user identity, so a reply chain off
    // them could otherwise reach execution without any allowlisted human
    // sender. Reject both shapes fail-closed.
    if (message.sender_chat) {
      logger.warn("telegrambot_message_ignored_sender_chat", {
        chat_id: String(chat.id),
        message_id: message.message_id,
        sender_chat_id: String(message.sender_chat.id),
      });
      return false;
    }
    if (message.is_automatic_forward) {
      logger.warn("telegrambot_message_ignored_automatic_forward", {
        chat_id: String(chat.id),
        message_id: message.message_id,
      });
      return false;
    }
    const allowlist = resolveChatAllowlist(options);
    if (allowlist.length === 0) {
      logger.warn("telegrambot_message_ignored_allowlist_empty", {
        chat_id: String(chat.id),
        chat_type: chat.type,
        message_id: message.message_id,
      });
      return false;
    }
    if (!new Set(allowlist).has(String(chat.id))) {
      logger.warn("telegrambot_message_ignored_chat_not_allowlisted", {
        chat_id: String(chat.id),
        message_id: message.message_id,
      });
      return false;
    }
    return true;
  }

  logger.warn("telegrambot_message_ignored_unsupported_chat_type", {
    chat_id: String(chat.id),
    chat_type: chat.type,
    message_id: message.message_id,
  });
  return false;
}

/**
 * Whether an *already allowed* message triggers the agent, and how.
 *
 * DMs: every allowed message triggers (`"command"` when it is an /ask form so
 * the caller strips the prefix, `"dm"` otherwise). Groups: only (a) replies
 * to one of the bot's own messages and (b) the addressed command
 * `/ask@{botUsername}` carried as a `bot_command` entity at offset 0 — a bare
 * `/ask` in a group is not addressed to us and may target another bot.
 *
 * Telegram delta: plain textual `@botname` mentions deliberately do NOT
 * trigger. Under privacy mode Telegram does not reliably deliver arbitrary
 * mention messages to an ordinary non-admin bot, and with privacy off (or an
 * admin bot) unrelated group traffic IS delivered and must still fail this
 * gate — so the v1 contract is replies + addressed commands + DMs until the
 * live privacy-mode acceptance test proves mention delivery (spec §3).
 * Non-triggering group messages are ignored locally, never executed.
 */
export function isTriggerMessage(
  message: TelegramMessage,
  botUserId: string,
  botUsername: string,
): TelegramTrigger | null {
  const chatType = message.chat.type;

  if (chatType === "private") {
    return matchesTriggerCommand(message, botUsername, { allowBare: true })
      ? "command"
      : "dm";
  }
  if (chatType !== "group" && chatType !== "supergroup") return null;

  const repliedTo = message.reply_to_message?.from;
  if (repliedTo && String(repliedTo.id) === String(botUserId)) return "reply";
  if (matchesTriggerCommand(message, botUsername, { allowBare: false })) {
    return "command";
  }
  return null;
}

/**
 * Message text with the leading `/ask[@botUsername]` stripped, ready for use
 * as the session input. Text without a recognized trigger command passes
 * through trimmed (DM free-text).
 */
export function extractCommandText(
  message: TelegramMessage,
  botUsername: string,
): string {
  const text = message.text ?? "";
  const command = commandEntityText(message);
  if (command === undefined) return text.trim();
  const normalized = command.toLowerCase();
  const addressed = `${TRIGGER_COMMAND}@${normalizeBotUsername(botUsername)}`;
  if (normalized !== TRIGGER_COMMAND && normalized !== addressed) {
    return text.trim();
  }
  return text.slice(command.length).trim();
}

/** Resolved group/supergroup chat allowlist (options first, env fallback). */
export function resolveChatAllowlist(
  options: Pick<TelegrambotOptions, "chatAllowlist">,
): string[] {
  return [
    ...(options.chatAllowlist ??
      splitEnvList(process.env.TELEGRAMBOT_CHAT_ALLOWLIST)),
  ];
}

/** Resolved DM user allowlist (options first, env fallback). */
export function resolveUserAllowlist(
  options: Pick<TelegrambotOptions, "userAllowlist">,
): string[] {
  return [
    ...(options.userAllowlist ??
      splitEnvList(process.env.TELEGRAMBOT_USER_ALLOWLIST)),
  ];
}

/** True when no group chats are allowlisted and every group message is ignored. */
export function isChatAllowlistEmpty(
  options: Pick<TelegrambotOptions, "chatAllowlist">,
): boolean {
  return resolveChatAllowlist(options).length === 0;
}

/** True when no DM users are allowlisted and every private message is ignored. */
export function isUserAllowlistEmpty(
  options: Pick<TelegrambotOptions, "userAllowlist">,
): boolean {
  return resolveUserAllowlist(options).length === 0;
}

/**
 * The command token at the start of the message, taken from an explicit
 * `bot_command` entity at offset 0. Requiring the entity (rather than
 * regexing the text) means Telegram itself vouched the token is a command;
 * entity offsets/lengths are UTF-16 code units, matching String#slice.
 */
function commandEntityText(message: TelegramMessage): string | undefined {
  const text = message.text;
  if (!text) return undefined;
  const entity = (message.entities ?? []).find(
    (candidate) => candidate.type === "bot_command" && candidate.offset === 0,
  );
  if (!entity) return undefined;
  return text.slice(0, entity.length);
}

function matchesTriggerCommand(
  message: TelegramMessage,
  botUsername: string,
  { allowBare }: { allowBare: boolean },
): boolean {
  const command = commandEntityText(message);
  if (command === undefined) return false;
  const normalized = command.toLowerCase();
  if (normalized === `${TRIGGER_COMMAND}@${normalizeBotUsername(botUsername)}`) {
    return true;
  }
  return allowBare && normalized === TRIGGER_COMMAND;
}

/** Bot usernames compare case-insensitively; tolerate a configured leading @. */
function normalizeBotUsername(botUsername: string): string {
  return botUsername.replace(/^@/, "").toLowerCase();
}

function splitEnvList(value: string | undefined): string[] {
  return (value ?? "")
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean);
}
