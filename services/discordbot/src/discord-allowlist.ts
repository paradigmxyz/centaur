import type { Logger, Message } from "chat";
import type { DiscordbotOptions } from "./types";

/**
 * Decode a Discord thread key `discord:{guildId}:{channelId}[:{threadId}]` into parts.
 * Returns an empty object if the id is not a Discord thread key.
 */
export function parseDiscordThreadKey(threadKey: string): {
  guildId?: string;
  channelId?: string;
  threadId?: string;
} {
  const parts = threadKey.split(":");
  if (parts[0] !== "discord") return {};
  return { guildId: parts[1], channelId: parts[2], threadId: parts[3] };
}

/**
 * Authorization gate for inbound Discord messages.
 *
 * Unlike the Slack allowlist (which is fail-open), this is intentionally **fail-closed**:
 * the api-rs control plane has no ingress auth, so this guard is the primary authorization
 * boundary. Direct messages are denied outright, and an empty/unset guild allowlist means the
 * bot is inert until configured.
 */
export function isAllowedDiscordMessage(
  message: Message,
  options: DiscordbotOptions,
  logger: Logger,
): boolean {
  if (message.author.isMe === true) {
    return false;
  }
  // Discord delta (mirrors slackbotv2's trigger-bot allowlist semantics):
  // bot-authored messages are rejected unless the bot is explicitly
  // allowlisted. The gateway only forwards bot messages that pass the
  // adapter's `shouldForwardBotMessage` hook (wired at the adapter
  // construction site); this gate re-checks with the full payload, where
  // application_id/webhook_id matching is possible.
  if (message.author.isBot === true) {
    if (
      !isAllowedTriggerBotMessage(message, resolveTriggerBotAllowlist(options))
    ) {
      logger.warn("discordbot_message_ignored_bot_not_allowlisted", {
        message_id: message.id,
        thread_id: message.threadId,
        user_id: message.author.userId,
      });
      return false;
    }
  }

  const { guildId } = parseDiscordThreadKey(message.threadId);
  if (!guildId || guildId === "@me") {
    logger.warn("discordbot_message_ignored_dm", {
      message_id: message.id,
      thread_id: message.threadId,
    });
    return false;
  }

  const allowlist =
    options.guildAllowlist ??
    splitEnvList(process.env.DISCORDBOT_GUILD_ALLOWLIST);
  if (allowlist.length === 0) {
    logger.warn("discordbot_message_ignored_allowlist_empty", {
      message_id: message.id,
      guild_id: guildId,
    });
    return false;
  }
  if (!new Set(allowlist).has(guildId)) {
    logger.warn("discordbot_message_ignored_guild_not_allowlisted", {
      message_id: message.id,
      guild_id: guildId,
    });
    return false;
  }

  return true;
}

/**
 * Discord delta (mirrors slackbotv2's `isAllowedTriggerBotMessage`): whether a
 * bot-authored message may trigger the agent. The allowlist carries bot user
 * ids; the message's author id plus the raw payload's `application_id` and
 * `webhook_id` are all accepted as matches (webhook-style integrations post
 * under those identities).
 */
export function isAllowedTriggerBotMessage(
  message: Pick<Message, "author" | "raw">,
  allowlist: readonly string[] | undefined,
): boolean {
  if (!allowlist?.length) return false;
  const raw =
    message.raw && typeof message.raw === "object"
      ? (message.raw as { application_id?: unknown; webhook_id?: unknown })
      : {};
  const identifiers = new Set(
    [
      message.author.userId,
      typeof raw.application_id === "string" ? raw.application_id : undefined,
      typeof raw.webhook_id === "string" ? raw.webhook_id : undefined,
    ]
      .map((value) => value?.trim())
      .filter((value): value is string => Boolean(value)),
  );
  return allowlist.some((entry) => identifiers.has(entry.trim()));
}

/**
 * Guild-level slice of the allowlist check, usable from adapter hooks that run
 * before a full `Message` exists (e.g. `shouldHandleMention`, which gates
 * thread creation). Fail-closed like `isAllowedDiscordMessage`: DMs
 * (`guildId` unset or `@me`) and an empty allowlist are denied.
 */
export function isAllowedDiscordGuild(
  guildId: string | undefined,
  options: DiscordbotOptions,
): boolean {
  if (!guildId || guildId === "@me") return false;
  const allowlist =
    options.guildAllowlist ??
    splitEnvList(process.env.DISCORDBOT_GUILD_ALLOWLIST);
  return allowlist.length > 0 && new Set(allowlist).has(guildId);
}

/** Resolved trigger-bot allowlist (options first, env fallback). */
export function resolveTriggerBotAllowlist(
  options: DiscordbotOptions,
): string[] {
  return [
    ...(options.triggerBotAllowlist ??
      splitEnvList(process.env.DISCORDBOT_TRIGGER_BOT_ALLOWLIST)),
  ];
}

/** True when the bot has no guild allowlist configured and will ignore every message. */
export function isGuildAllowlistEmpty(options: DiscordbotOptions): boolean {
  const allowlist =
    options.guildAllowlist ??
    splitEnvList(process.env.DISCORDBOT_GUILD_ALLOWLIST);
  return allowlist.length === 0;
}

function splitEnvList(value: string | undefined): string[] {
  return (value ?? "")
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean);
}
