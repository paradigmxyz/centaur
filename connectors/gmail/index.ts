import type { Ctx } from "../centaur";
import { query, readJson, type Params } from "../_lib/http";

// Results are Gmail's own resources, passed through: messages, payload/parts, labels. Envelopes
// (message lists, label lists) are unwrapped; bodies are not reformatted. Gmail's reference is
// https://developers.google.com/gmail/api/reference/rest. Authentication is iron-proxy's
// oauth_token transform, which mints an access token from GOOGLE_TOKEN_JSON and injects the
// Authorization header on the wire; the connector itself sends no credentials unless
// GOOGLE_ACCESS_TOKEN is set (local testing only).

const GMAIL = "https://gmail.googleapis.com/gmail/v1/users/me";

/** Headers fetched for the metadata-format messages `search_messages` returns. */
const DEFAULT_METADATA_HEADERS = ["Subject", "From", "To", "Cc", "Date", "Message-ID"];

/** A Gmail message header, as Gmail sends it. */
export interface Header {
  name: string;
  value: string;
  [field: string]: unknown;
}

/** A message part body: inline `data` (base64url) or an `attachmentId` to fetch separately. */
export interface MessagePartBody {
  attachmentId?: string;
  size?: number;
  data?: string;
  [field: string]: unknown;
}

/** One node of a message's MIME tree. */
export interface MessagePart {
  partId?: string;
  mimeType?: string;
  filename?: string;
  headers?: Header[];
  body?: MessagePartBody;
  parts?: MessagePart[];
  [field: string]: unknown;
}

/** A Gmail message. `payload` is the parsed MIME tree; `raw` is present only for `format: "raw"`. */
export interface GmailMessage {
  id: string;
  threadId?: string;
  labelIds?: string[];
  snippet?: string;
  historyId?: string;
  internalDate?: string;
  sizeEstimate?: number;
  payload?: MessagePart;
  raw?: string;
  [field: string]: unknown;
}

/** A Gmail label. `type` is `system` or `user`. */
export interface Label {
  id: string;
  name: string;
  type?: string;
  messageListVisibility?: string;
  labelListVisibility?: string;
  color?: { backgroundColor?: string; textColor?: string; [field: string]: unknown };
  [field: string]: unknown;
}

/** A file attached to an outgoing message, as base64. */
export interface AttachmentInput {
  /** File name shown to the recipient, for example `report.pdf`. */
  filename: string;
  /** File bytes, base64 in either alphabet, with or without padding. */
  content_base64: string;
  /** Defaults to `application/octet-stream`. */
  mime_type?: string;
}

/** An attachment referenced by a received message; fetch it with `get_attachment`. */
export interface AttachmentRef {
  partId: string;
  filename: string;
  mimeType: string;
  size: number;
  attachmentId: string;
}

function headers(ctx: Ctx): Record<string, string> {
  const out: Record<string, string> = { accept: "application/json" };
  // Behind iron-proxy the access token is injected on the wire; locally it may come from the env.
  if (ctx.env.GOOGLE_ACCESS_TOKEN) out.authorization = `Bearer ${ctx.env.GOOGLE_ACCESS_TOKEN}`;
  return out;
}

/** A Gmail REST call against `/users/me`; returns the JSON body, `{}` for an empty 204. */
async function gmail(
  ctx: Ctx,
  path: string,
  options: { method?: string; params?: Params; body?: unknown } = {},
): Promise<any> {
  const requestHeaders = headers(ctx);
  if (options.body !== undefined) requestHeaders["content-type"] = "application/json";
  const res = await ctx.fetch(`${GMAIL}${path}${query(options.params ?? {})}`, {
    method: options.method ?? (options.body === undefined ? "GET" : "POST"),
    headers: requestHeaders,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });
  if (res.status === 204) return {};
  return readJson(res, "Gmail");
}

const seg = (value: string): string => encodeURIComponent(value.trim());

/** `metadataHeaders` is a repeated query parameter, which `query` cannot express. */
function metadataQuery(values: string[]): string {
  return values.map((name) => `metadataHeaders=${encodeURIComponent(name)}`).join("&");
}

function randomId(): string {
  let out = "";
  while (out.length < 32) out += Math.floor(Math.random() * 0x100000000).toString(16).padStart(8, "0");
  return out.slice(0, 32);
}

/** Base64url with padding, as Gmail's `raw` field and the Python tool use. */
function b64url(ctx: Ctx, data: string): string {
  return ctx.base64.encode(data).replace(/\+/g, "-").replace(/\//g, "_");
}

function wrap76(value: string): string {
  const lines: string[] = [];
  for (let i = 0; i < value.length; i += 76) lines.push(value.slice(i, i + 76));
  return lines.join("\r\n");
}

function utf8Length(ch: string): number {
  const code = ch.codePointAt(0) ?? 0;
  if (code < 0x80) return 1;
  if (code < 0x800) return 2;
  if (code < 0x10000) return 3;
  return 4;
}

/** A header value, CR/LF-stripped, RFC 2047 base64 encoded-word folded when it is not ASCII. */
function headerValue(ctx: Ctx, value: string): string {
  const clean = value.replace(/[\r\n]+/g, " ").trim();
  if (!/[^\x20-\x7e]/.test(clean)) return clean;
  const words: string[] = [];
  let chunk = "";
  let size = 0;
  for (const ch of clean) {
    const width = utf8Length(ch);
    if (chunk && size + width > 45) {
      words.push(chunk);
      chunk = "";
      size = 0;
    }
    chunk += ch;
    size += width;
  }
  if (chunk) words.push(chunk);
  return words.map((word) => `=?UTF-8?B?${ctx.base64.encode(word)}?=`).join("\r\n ");
}

const boundary = (): string => `----=_centaur_${randomId()}`;

/** Split an address list on commas outside quotes and angle brackets. */
function splitAddresses(value: string): string[] {
  const parts: string[] = [];
  let current = "";
  let quoted = false;
  let depth = 0;
  for (const ch of value) {
    if (ch === '"') quoted = !quoted;
    else if (!quoted && ch === "<") depth += 1;
    else if (!quoted && ch === ">") depth = Math.max(depth - 1, 0);
    if (ch === "," && !quoted && depth === 0) {
      parts.push(current);
      current = "";
      continue;
    }
    current += ch;
  }
  parts.push(current);
  return parts.map((part) => part.trim()).filter(Boolean);
}

/** One address: only the display name is encoded, so `Ana <a@b.com>` keeps an ASCII addr-spec. */
function addressValue(ctx: Ctx, value: string): string {
  const open = value.lastIndexOf("<");
  const close = value.lastIndexOf(">");
  if (open === -1 || close <= open) return headerValue(ctx, value);
  const display = value.slice(0, open).trim().replace(/^"(.*)"$/, "$1");
  const address = value.slice(open + 1, close).trim();
  return display ? `${headerValue(ctx, display)} <${address}>` : `<${address}>`;
}

/** An RFC 5322 address list, re-joined with `, `. */
function addressList(ctx: Ctx, value: string): string {
  return splitAddresses(value).map((part) => addressValue(ctx, part)).join(", ");
}

/** A leaf MIME part: its Content-* headers and a base64 body. */
function contentPart(ctx: Ctx, contentType: string, body: string): string {
  return [
    `Content-Type: ${contentType}`,
    "Content-Transfer-Encoding: base64",
    "",
    wrap76(ctx.base64.encode(body)),
  ].join("\r\n");
}

function attachmentPart(ctx: Ctx, file: AttachmentInput): string {
  const name = file.filename.replace(/[\r\n]+/g, " ").trim();
  const ascii = name.replace(/[^\x20-\x7e]|["\\]/g, "_");
  const disposition = /[^\x20-\x7e]/.test(name)
    ? `attachment; filename="${ascii}"; filename*=UTF-8''${encodeURIComponent(name)}`
    : `attachment; filename="${ascii}"`;
  const data = wrap76(ctx.base64.encode(ctx.base64.decode(file.content_base64)));
  return [
    `Content-Type: ${file.mime_type ?? "application/octet-stream"}; name="${ascii}"`,
    "Content-Transfer-Encoding: base64",
    `Content-Disposition: ${disposition}`,
    "",
    data,
  ].join("\r\n");
}

interface OutgoingMessage {
  to: string;
  subject: string;
  body: string;
  cc?: string;
  bcc?: string;
  html?: string;
  attachments?: AttachmentInput[];
  /** Original `Message-ID`, threaded into `In-Reply-To` and `References`. */
  inReplyTo?: string;
}

/** RFC 2822 (MIME) text for a message; the caller base64urls it. */
function buildMessage(ctx: Ctx, input: OutgoingMessage): string {
  const head: string[] = [`To: ${addressList(ctx, input.to)}`];
  if (input.cc) head.push(`Cc: ${addressList(ctx, input.cc)}`);
  if (input.bcc) head.push(`Bcc: ${addressList(ctx, input.bcc)}`);
  head.push(`Subject: ${headerValue(ctx, input.subject)}`);
  if (input.inReplyTo) {
    head.push(`In-Reply-To: ${input.inReplyTo}`);
    head.push(`References: ${input.inReplyTo}`);
  }
  head.push("MIME-Version: 1.0");

  let content: string;
  if (input.html !== undefined) {
    const alt = boundary();
    content = [
      `Content-Type: multipart/alternative; boundary="${alt}"`,
      "",
      `--${alt}`,
      contentPart(ctx, 'text/plain; charset="UTF-8"', input.body),
      `--${alt}`,
      contentPart(ctx, 'text/html; charset="UTF-8"', input.html),
      `--${alt}--`,
    ].join("\r\n");
  } else {
    content = contentPart(ctx, 'text/plain; charset="UTF-8"', input.body);
  }

  if (input.attachments?.length) {
    const mixed = boundary();
    const chunks = [`--${mixed}`, content];
    for (const file of input.attachments) chunks.push(`--${mixed}`, attachmentPart(ctx, file));
    chunks.push(`--${mixed}--`);
    content = [
      `Content-Type: multipart/mixed; boundary="${mixed}"`,
      "",
      chunks.join("\r\n"),
    ].join("\r\n");
  }

  return `${[...head, content].join("\r\n")}\r\n`;
}

/** Address list without display names, for replies: `Ana <a@b.com>` becomes `a@b.com`. */
function addressOf(value: string): string {
  const open = value.lastIndexOf("<");
  const close = value.lastIndexOf(">");
  if (open !== -1 && close > open) return value.slice(open + 1, close).trim();
  return value.trim();
}

function decodeText(ctx: Ctx, data: string): string {
  try {
    return ctx.base64.decodeText(data);
  } catch {
    return "";
  }
}

/**
 * Search messages with Gmail's own query syntax (`from:`, `subject:`, `is:unread`, `has:attachment`,
 * `after:2024-01-01`, ...), newest first. Each result is a metadata-format Gmail message (id,
 * threadId, snippet, labelIds) whose `payload.headers` carry the requested headers, by default
 * Subject, From, To, Cc, Date and Message-ID. Costs one request per message, so `max_results`
 * defaults to 20 and is capped at 100. More results: pass `next_page_token` as `page_token`.
 * Ordering and thread collapsing are Gmail's own.
 */
export async function search_messages(
  args: {
    query: string;
    max_results?: number;
    page_token?: string;
    include_spam_trash?: boolean;
    metadata_headers?: string[];
  },
  ctx: Ctx,
): Promise<{ messages: GmailMessage[]; next_page_token: string | null }> {
  const maxResults = Math.min(Math.max(args.max_results ?? 20, 1), 100);
  const list = await gmail(ctx, "/messages", {
    params: {
      q: args.query,
      maxResults,
      pageToken: args.page_token,
      includeSpamTrash: args.include_spam_trash,
    },
  });
  const wanted = args.metadata_headers?.length ? args.metadata_headers : DEFAULT_METADATA_HEADERS;
  const ids: { id: string }[] = list.messages ?? [];
  const messages = await Promise.all(
    ids.map((message) =>
      gmail(ctx, `/messages/${seg(message.id)}?${["format=metadata", metadataQuery(wanted)].join("&")}`),
    ),
  );
  return { messages, next_page_token: list.nextPageToken ?? null };
}

/**
 * One message by ID. `format` is Gmail's: `full` (default) parses the MIME tree into `payload`,
 * `metadata` returns headers only (pass `metadata_headers`), `raw` returns the RFC 822 message as
 * base64url in `raw`, `minimal` returns ids and labels. For decoded text and HTML use
 * `get_message_body`.
 */
export async function get_message(
  args: {
    message_id: string;
    format?: "full" | "metadata" | "raw" | "minimal";
    metadata_headers?: string[];
  },
  ctx: Ctx,
): Promise<{ message: GmailMessage }> {
  const format = args.format ?? "full";
  const parts = [`format=${format}`];
  if (format === "metadata") {
    parts.push(metadataQuery(args.metadata_headers?.length ? args.metadata_headers : DEFAULT_METADATA_HEADERS));
  }
  const message = await gmail(ctx, `/messages/${seg(args.message_id)}?${parts.join("&")}`);
  return { message };
}

/**
 * One message with its bodies decoded: the full Gmail `message` plus `text` and `html` (UTF-8 of
 * the first part of each type; "" when the message has none, or when a part is not valid UTF-8)
 * and `attachments`, the attachment parts to fetch with `get_attachment`. Attachments are not
 * downloaded here.
 */
export async function get_message_body(
  args: { message_id: string },
  ctx: Ctx,
): Promise<{ message: GmailMessage; text: string; html: string; attachments: AttachmentRef[] }> {
  const message: GmailMessage = await gmail(ctx, `/messages/${seg(args.message_id)}?format=full`);
  const found = { text: "", html: "", attachments: [] as AttachmentRef[] };
  const walk = (part: MessagePart | undefined, path: string): void => {
    if (!part) return;
    const mimeType = part.mimeType ?? "";
    const data = part.body?.data;
    if (mimeType === "text/plain" && !found.text && data) found.text = decodeText(ctx, data);
    if (mimeType === "text/html" && !found.html && data) found.html = decodeText(ctx, data);
    if (part.body?.attachmentId) {
      found.attachments.push({
        partId: part.partId ?? path,
        filename: part.filename ?? "",
        mimeType,
        size: part.body.size ?? 0,
        attachmentId: part.body.attachmentId,
      });
    }
    (part.parts ?? []).forEach((child, index) => walk(child, `${path}.${index}`));
  };
  walk(message.payload, "0");
  return { message, text: found.text, html: found.html, attachments: found.attachments };
}

/**
 * Download one attachment of a received message by its `attachmentId` from `get_message_body`.
 * Returns the bytes as `data_base64` (standard alphabet) and `size`; a response is capped at
 * 10 MiB, so very large attachments fail instead of truncating.
 */
export async function get_attachment(
  args: { message_id: string; attachment_id: string },
  ctx: Ctx,
): Promise<{ attachment_id: string; size: number; data_base64: string }> {
  const body = await gmail(
    ctx,
    `/messages/${seg(args.message_id)}/attachments/${seg(args.attachment_id)}`,
  );
  const bytes = ctx.base64.decode(body.data ?? "");
  return { attachment_id: args.attachment_id, size: bytes.length, data_base64: ctx.base64.encode(bytes) };
}

/**
 * All labels in the mailbox: Gmail label objects with id, name, type (`system` or `user`), the
 * list/visibility settings and colours. Use the ids with `archive_messages` (via `removeLabelIds`)
 * or Gmail queries (`label:`).
 */
export async function list_labels(args: {}, ctx: Ctx): Promise<{ labels: Label[] }> {
  const body = await gmail(ctx, "/labels");
  return { labels: body.labels ?? [] };
}

/**
 * Send an email: `body` is the plain-text body, and `html` (optional) makes the message
 * multipart/alternative with `body` as its text fallback. `to`/`cc`/`bcc` are RFC 5322 address
 * lists (`Ana <a@b.com>, b@c.com`); subject and display names are RFC 2047 encoded when not
 * ASCII. Attachments are supplied inline as base64. Returns the sent message (id, threadId,
 * labelIds).
 */
export async function send_message(
  args: {
    to: string;
    subject: string;
    body: string;
    cc?: string;
    bcc?: string;
    html?: string;
    attachments?: AttachmentInput[];
  },
  ctx: Ctx,
): Promise<{ message: GmailMessage }> {
  const raw = b64url(ctx, buildMessage(ctx, args));
  return { message: await gmail(ctx, "/messages", { body: { raw } }) };
}

/**
 * Reply in the original message's thread: recipient is the original `From`, the subject gets a
 * `Re: ` prefix unless it has one, and `In-Reply-To`/`References` carry the original `Message-ID`.
 * Same `html`/`attachments` support as `send_message`.
 */
export async function reply(
  args: {
    message_id: string;
    body: string;
    html?: string;
    attachments?: AttachmentInput[];
  },
  ctx: Ctx,
): Promise<{ message: GmailMessage }> {
  const original = await gmail(
    ctx,
    `/messages/${seg(args.message_id)}?${["format=metadata", metadataQuery(["From", "Subject", "Message-ID"])].join("&")}`,
  );
  const fields: Record<string, string> = {};
  for (const header of (original.payload?.headers ?? []) as Header[]) fields[header.name] = header.value;
  const subject = fields.Subject ?? "(no subject)";
  const messageId = (fields["Message-ID"] || args.message_id).replace(/[\r\n]+/g, " ").trim();
  const to = addressOf(fields.From ?? "");
  if (!to) throw new Error(`message ${args.message_id} has no From header to reply to`);
  const raw = b64url(
    ctx,
    buildMessage(ctx, {
      to,
      subject: subject.toLowerCase().startsWith("re:") ? subject : `Re: ${subject}`,
      body: args.body,
      html: args.html,
      attachments: args.attachments,
      inReplyTo: messageId,
    }),
  );
  return {
    message: await gmail(ctx, "/messages", {
      body: { raw, threadId: original.threadId },
    }),
  };
}

/** Archive messages by removing the INBOX label; they stay in All Mail. One request per ID. */
export async function archive_messages(
  args: { message_ids: string[] },
  ctx: Ctx,
): Promise<{ archived: number }> {
  await Promise.all(
    args.message_ids.map((id) =>
      gmail(ctx, `/messages/${seg(id)}/modify`, { body: { removeLabelIds: ["INBOX"] } }),
    ),
  );
  return { archived: args.message_ids.length };
}

/**
 * Move messages to the trash (recoverable for 30 days); this does not permanently delete them.
 * One request per ID.
 */
export async function delete_messages(
  args: { message_ids: string[] },
  ctx: Ctx,
): Promise<{ deleted: number }> {
  await Promise.all(
    // A bodyless POST has no Content-Length, which Gmail answers with 411.
    args.message_ids.map((id) => gmail(ctx, `/messages/${seg(id)}/trash`, { body: {} })),
  );
  return { deleted: args.message_ids.length };
}
