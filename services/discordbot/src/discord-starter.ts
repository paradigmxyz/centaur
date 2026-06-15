import type { Attachment, Logger } from "chat";
import { parseDiscordThreadKey } from "./discord-allowlist";
import { DEFAULT_DISCORD_API_URL } from "./discord-threading";
import type {
  DiscordbotApiAttachment,
  DiscordbotApiMessage,
  DiscordbotOptions,
  JsonObject,
} from "./types";
import { isJsonObject } from "./utils";

/**
 * Discord delta with no slackbotv2 analog: a thread created from a message keeps
 * that starter message in the **parent channel** (the thread shares its ID), so it
 * never appears in the thread's own history — Slack's conversations.replies
 * returns the parent as the first reply, but Discord requires this extra fetch
 * (mirrors discord.js `ThreadChannel#fetchStarterMessage`).
 *
 * Returns null when the key has no thread segment, the thread was not created
 * from a message (404), or on any failure — context enrichment must never block
 * execution.
 */
export async function fetchThreadStarterMessage(
  options: DiscordbotOptions,
  threadKey: string,
  logger: Logger,
): Promise<DiscordbotApiMessage | null> {
  const { channelId, threadId } = parseDiscordThreadKey(threadKey);
  if (!channelId || !threadId) return null;

  const fetchFn = options.fetch ?? fetch;
  const apiBase = (options.discordApiUrl ?? DEFAULT_DISCORD_API_URL).replace(
    /\/$/,
    "",
  );
  try {
    const response = await fetchFn(
      `${apiBase}/channels/${channelId}/messages/${threadId}`,
      { headers: { authorization: `Bot ${options.botToken}` } },
    );
    if (!response.ok) {
      // 404 = the thread was created standalone ("+ New Thread") or the
      // starter message was deleted; both are normal, not errors.
      if (response.status !== 404) {
        logger.warn("discordbot_thread_starter_fetch_failed", {
          status: response.status,
          thread_id: threadId,
        });
      }
      return null;
    }
    return rawMessageToApiMessage(
      await response.json(),
      threadKey,
      options.applicationId,
    );
  } catch (error) {
    logger.warn("discordbot_thread_starter_fetch_error", {
      error: error instanceof Error ? error.message : String(error),
      thread_id: threadId,
    });
    return null;
  }
}

/**
 * Append flattened embed content to a message's text. Webhook-style messages
 * (Sentry alerts, GitHub notifications) carry their payload entirely in
 * `embeds` with empty `content`; the chat adapter only surfaces `content`, so
 * without this the agent sees an empty message.
 */
export function withDiscordEmbedText(text: string, raw: unknown): string {
  if (!isJsonObject(raw)) return text;
  const embeds = Array.isArray(raw.embeds) ? raw.embeds : [];
  const embedText = embeds
    .map((embed) => embedToText(embed))
    .filter(Boolean)
    .join("\n\n");
  if (!embedText) return text;
  return text.trim() ? `${text}\n\n${embedText}` : embedText;
}

function embedToText(embed: unknown): string {
  if (!isJsonObject(embed)) return "";
  const lines: string[] = [];
  const author = isJsonObject(embed.author)
    ? nonEmptyString(embed.author.name)
    : undefined;
  if (author) lines.push(author);
  const title = nonEmptyString(embed.title);
  const url = nonEmptyString(embed.url);
  if (title) {
    lines.push(url ? `${title} (${url})` : title);
  } else if (url) {
    lines.push(url);
  }
  const description = nonEmptyString(embed.description);
  if (description) lines.push(description);
  const fields = Array.isArray(embed.fields) ? embed.fields : [];
  for (const field of fields) {
    if (!isJsonObject(field)) continue;
    const name = nonEmptyString(field.name);
    const value = nonEmptyString(field.value);
    if (name && value) lines.push(`${name}: ${value}`);
    else if (value) lines.push(value);
  }
  const footer = isJsonObject(embed.footer)
    ? nonEmptyString(embed.footer.text)
    : undefined;
  if (footer) lines.push(footer);
  if (lines.length === 0) return "";
  return `[embed] ${lines.join("\n")}`;
}

function rawMessageToApiMessage(
  raw: unknown,
  threadKey: string,
  botUserId: string,
): DiscordbotApiMessage | null {
  if (!isJsonObject(raw) || typeof raw.id !== "string") return null;
  const author = isJsonObject(raw.author) ? raw.author : {};
  const userId = nonEmptyString(author.id) ?? "unknown";
  const userName = nonEmptyString(author.username) ?? "unknown";
  return {
    attachments: rawAttachments(raw),
    author: {
      fullName: nonEmptyString(author.global_name) ?? userName,
      isBot: author.bot === true,
      isMe: userId === botUserId,
      userId,
      userName,
    },
    id: raw.id,
    isMention: false,
    raw,
    text: withDiscordEmbedText(nonEmptyString(raw.content) ?? "", raw),
    threadId: threadKey,
    timestamp: nonEmptyString(raw.timestamp) ?? "",
  };
}

function rawAttachments(raw: JsonObject): DiscordbotApiAttachment[] {
  const attachments = Array.isArray(raw.attachments) ? raw.attachments : [];
  return attachments.filter(isJsonObject).map((attachment) => ({
    height: numberValue(attachment.height),
    mimeType: nonEmptyString(attachment.content_type),
    name: nonEmptyString(attachment.filename),
    size: numberValue(attachment.size),
    type: attachmentType(nonEmptyString(attachment.content_type)),
    url: nonEmptyString(attachment.url),
    width: numberValue(attachment.width),
  }));
}

// Mirrors the chat adapter's getAttachmentType MIME mapping.
function attachmentType(mimeType: string | undefined): Attachment["type"] {
  if (mimeType?.startsWith("image/")) return "image";
  if (mimeType?.startsWith("video/")) return "video";
  if (mimeType?.startsWith("audio/")) return "audio";
  return "file";
}

function nonEmptyString(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" ? value : undefined;
}
