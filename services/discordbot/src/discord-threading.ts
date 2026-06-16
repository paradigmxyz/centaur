import type { Logger } from "chat";
import { parseDiscordThreadKey } from "./discord-allowlist";
import type { DiscordbotOptions } from "./types";
import { sliceSurrogateSafe } from "./utils";

const DISCORD_THREAD_NAME_LIMIT = 100;
export const DEFAULT_DISCORD_API_URL = "https://discord.com/api/v10";

/**
 * Derive a Discord thread name from the triggering message text. The `@chat-adapter/discord`
 * adapter auto-creates a thread on a channel mention but names it generically
 * (`Thread <timestamp>`); this reproduces the Slack "assistant title" feel by naming the thread
 * after what the user actually asked.
 */
export function deriveThreadName(text: string, userName = "centaur"): string {
  const mentionless = text
    .replace(/<@!?\d+>/g, "") // user mentions <@123> / <@!123>
    .replace(/<@&\d+>/g, "") // role mentions <@&123>
    .replace(
      new RegExp(`^\\s*@?${escapeRegExp(userName)}\\b[:,]?\\s*`, "i"),
      "",
    )
    .trim();
  return clipOneLine(mentionless || "Centaur task", DISCORD_THREAD_NAME_LIMIT);
}

/**
 * Whether the thread was just auto-created for this triggering message, versus the message having
 * arrived inside a pre-existing thread. Discord gives a thread spawned from a message the same id as
 * that message (the adapter's own "thread already exists" fallback relies on this), so a key whose
 * thread segment equals the message id is one we just created and should name. A message that landed
 * in an existing thread carries that thread's own id instead, and renaming it would clobber a title
 * the user (or an earlier run) already set.
 */
export function isThreadCreatedForMessage(
  threadKey: string,
  messageId: string,
): boolean {
  const { threadId } = parseDiscordThreadKey(threadKey);
  return threadId !== undefined && threadId === messageId;
}

/**
 * Best-effort rename of the thread the session lives in. No-ops when the key carries no thread
 * segment (i.e. the message was not threaded). Failures are swallowed — naming is cosmetic and
 * must never block streaming.
 */
export async function renameThreadFromMessage(
  options: DiscordbotOptions,
  threadKey: string,
  name: string,
  logger: Logger,
): Promise<void> {
  const { threadId } = parseDiscordThreadKey(threadKey);
  if (!threadId) return;

  const fetchFn = options.fetch ?? fetch;
  const apiBase = (options.discordApiUrl ?? DEFAULT_DISCORD_API_URL).replace(
    /\/$/,
    "",
  );
  try {
    const response = await fetchFn(`${apiBase}/channels/${threadId}`, {
      method: "PATCH",
      headers: {
        authorization: `Bot ${options.botToken}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ name }),
    });
    if (!response.ok) {
      logger.warn("discordbot_thread_rename_failed", {
        status: response.status,
        thread_id: threadId,
      });
    }
  } catch (error) {
    logger.warn("discordbot_thread_rename_error", {
      error: error instanceof Error ? error.message : String(error),
      thread_id: threadId,
    });
  }
}

/**
 * Best-effort fetch of a Discord channel's name (`GET /channels/{id}`), used to
 * name the session principal in iron-control. Returns undefined on any failure
 * — the name is cosmetic, so a lookup failure just falls back to the synthetic
 * id-based principal name in api-rs.
 */
export async function fetchDiscordChannelName(
  options: DiscordbotOptions,
  channelId: string,
  logger: Logger,
): Promise<string | undefined> {
  const fetchFn = options.fetch ?? fetch;
  const apiBase = (options.discordApiUrl ?? DEFAULT_DISCORD_API_URL).replace(
    /\/$/,
    "",
  );
  try {
    const response = await fetchFn(`${apiBase}/channels/${channelId}`, {
      method: "GET",
      headers: { authorization: `Bot ${options.botToken}` },
    });
    if (!response.ok) {
      logger.warn("discordbot_channel_name_failed", {
        status: response.status,
        channel_id: channelId,
      });
      return undefined;
    }
    const channel = (await response.json()) as { name?: unknown };
    return typeof channel.name === "string" && channel.name.length > 0
      ? channel.name
      : undefined;
  } catch (error) {
    logger.warn("discordbot_channel_name_error", {
      error: error instanceof Error ? error.message : String(error),
      channel_id: channelId,
    });
    return undefined;
  }
}

function clipOneLine(value: string, max: number): string {
  const oneLine = value.replace(/\s+/g, " ").trim();
  if (oneLine.length <= max) return oneLine;
  // Discord delta: surrogate-safe cut — halving an emoji's surrogate pair
  // makes Discord reject the rename with a 400.
  return `${sliceSurrogateSafe(oneLine, Math.max(0, max - 1)).trimEnd()}…`;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
