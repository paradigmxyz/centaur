import pg from "pg";
import type { TelegramPolicyMessage } from "./telegram-policy";

/**
 * A group message as company context stores it. Telegram has no history API
 * and allows one update consumer per bot token, so this service hands every
 * message from a connected group to company context as it arrives.
 */
export type TelegramContextMessage = {
  chatId: string;
  chatTitle: string;
  media: TelegramContextMedia[];
  messageId: number;
  replyToMessageId: number | null;
  senderId: string | null;
  senderName: string;
  senderUsername: string;
  /** Unix seconds, as Telegram reports `date`. */
  sentAt: number;
  text: string;
  topicId: number | null;
};

/** An attached medium: what it is, never its contents. */
export type TelegramContextMedia = {
  kind: string;
  mime_type?: string;
  name?: string;
};

export type TelegramContextSink = {
  record(message: TelegramContextMessage): Promise<void>;
  close?(): Promise<void>;
};

/**
 * The Chat SDK message parsed from `raw`. For an album (`media_group_id`) the
 * adapter combines every item into one message: `raw` is only the last item,
 * while `text` carries the caption and `attachments` every item.
 */
export type ParsedTelegramMessage = {
  attachments?: readonly { mimeType?: string; name?: string; type: string }[];
  text?: string;
};

/** Chat SDK attachment types named as the Bot API names them. */
const ATTACHMENT_KINDS: Record<string, string> = { image: "photo" };

const CONTEXT_WRITE_TIMEOUT_MS = 5_000;

type MediaFile ={ file_name?: string; mime_type?: string; title?: string };

const MEDIA_KINDS = [
  "photo",
  "video",
  "animation",
  "audio",
  "voice",
  "video_note",
  "document",
  "sticker",
] as const;

/** Normalizes a Bot API message for company context. */
export function toContextMessage(
  raw: TelegramPolicyMessage,
  parsed?: ParsedTelegramMessage,
): TelegramContextMessage {
  const isTopicMessage =
    (raw as { is_topic_message?: boolean }).is_topic_message === true;
  const replied = raw.reply_to_message;
  // In forum topics every message implicitly replies to the topic's first
  // message (whose id equals message_thread_id); that is not a reply. Outside
  // topics message_thread_id names a reply chain, so every reply counts.
  const replyToMessageId =
    replied && !(isTopicMessage && replied.message_id === raw.message_thread_id)
      ? replied.message_id
      : null;
  const album = raw.media_group_id !== undefined && parsed !== undefined;
  // A message sent as a chat (anonymous admin, linked channel) carries a
  // stand-in `from`; the chat is the real sender.
  const senderChat = raw.sender_chat;
  const from = raw.from;
  return {
    chatId: String(raw.chat.id),
    chatTitle: (raw.chat.title ?? "").trim(),
    media: album ? albumMedia(parsed) : contextMedia(raw),
    messageId: raw.message_id,
    replyToMessageId,
    senderId: senderChat
      ? String(senderChat.id)
      : from
        ? String(from.id)
        : null,
    senderName: senderChat
      ? (senderChat.title ?? "").trim()
      : [from?.first_name, from?.last_name].filter(Boolean).join(" ").trim(),
    senderUsername: (senderChat ? senderChat.username : from?.username) ?? "",
    sentAt: raw.date,
    text: raw.text ?? raw.caption ?? (album ? (parsed.text ?? "") : ""),
    topicId:
      isTopicMessage && raw.message_thread_id !== undefined
        ? raw.message_thread_id
        : null,
  };
}

/** Whether a normalized message carries anything worth indexing. */
export function hasContextContent(message: TelegramContextMessage): boolean {
  return message.text.trim().length > 0 || message.media.length > 0;
}

function albumMedia(parsed: ParsedTelegramMessage): TelegramContextMedia[] {
  return (parsed.attachments ?? []).map((attachment) => ({
    kind: ATTACHMENT_KINDS[attachment.type] ?? attachment.type,
    ...(attachment.name ? { name: attachment.name } : {}),
    ...(attachment.mimeType ? { mime_type: attachment.mimeType } : {}),
  }));
}

function contextMedia(raw: TelegramPolicyMessage): TelegramContextMedia[] {
  const media: TelegramContextMedia[] = [];
  const fields = raw as unknown as Record<string, unknown>;
  for (const kind of MEDIA_KINDS) {
    const value = fields[kind];
    if (!value) continue;
    // A photo is a list of sizes of one image.
    const file = (Array.isArray(value) ? {} : value) as MediaFile;
    const name =
      kind === "sticker"
        ? (value as { emoji?: string }).emoji
        : (file.file_name ?? file.title);
    media.push({
      kind,
      ...(name ? { name } : {}),
      ...(file.mime_type ? { mime_type: file.mime_type } : {}),
    });
  }
  return media;
}

/**
 * Writes messages to company context's staging table. The company-context
 * service owns the table (its migrations create it) and projects, publishes,
 * and expires what lands there. A redelivered message is stored once. Tight
 * timeouts keep an unreachable database from piling up pending writes.
 */
export function createPostgresContextSink(
  connectionString: string,
  onPoolError: (error: unknown) => void,
): TelegramContextSink {
  const pool = new pg.Pool({
    connectionString,
    connectionTimeoutMillis: CONTEXT_WRITE_TIMEOUT_MS,
    max: 2,
    query_timeout: CONTEXT_WRITE_TIMEOUT_MS,
    statement_timeout: CONTEXT_WRITE_TIMEOUT_MS,
  });
  pool.on("error", onPoolError);
  return {
    async record(message) {
      await pool.query(
        `INSERT INTO company_context_system.telegram_messages (
           chat_id, message_id, topic_id, chat_title, sender_id, sender_name,
           sender_username, reply_to_message_id, text, media, sent_at
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, to_timestamp($11))
         ON CONFLICT (chat_id, message_id) DO NOTHING`,
        [
          message.chatId,
          message.messageId,
          message.topicId,
          message.chatTitle,
          message.senderId,
          message.senderName,
          message.senderUsername,
          message.replyToMessageId,
          message.text,
          JSON.stringify(message.media),
          message.sentAt,
        ],
      );
    },
    async close() {
      await pool.end();
    },
  };
}
