import type {
  GoogleChatApiAttachment,
  GoogleChatApiMessage,
  GoogleChatEventEnvelope,
  GoogleChatRawMessage,
  GoogleChatRawSpace,
  JsonValue,
} from './types.js';

/** Decoded `gchat:<space>[:<base64url thread>][:dm]` thread key. */
export type GoogleChatThreadRef = {
  isDm: boolean;
  spaceName: string;
  threadName?: string;
};

/**
 * Parse the Chat SDK Google Chat thread key. The adapter encodes
 * `gchat:<spaceName>[:<base64url threadName>][:dm]`, where `spaceName` is the
 * `spaces/AAAA` resource name (it contains `/`, never `:`), so the segments
 * stay unambiguous.
 */
export function decodeGoogleChatThreadKey(threadKey: string): GoogleChatThreadRef | undefined {
  const isDm = threadKey.endsWith(':dm');
  const withoutDm = isDm ? threadKey.slice(0, -':dm'.length) : threadKey;
  const rest = withoutDm.startsWith('gchat:') ? withoutDm.slice('gchat:'.length) : undefined;
  if (rest === undefined) {
    return undefined;
  }
  const [spaceName, encodedThreadName, ...extra] = rest.split(':');
  if (!spaceName || extra.length > 0) {
    return undefined;
  }
  const threadName = encodedThreadName ? decodeBase64Url(encodedThreadName) : undefined;
  return { isDm, spaceName, ...(threadName ? { threadName } : {}) };
}

/** The `gchat:` thread key for a space (and thread, when the message is threaded). */
export function encodeGoogleChatThreadKey(ref: GoogleChatThreadRef): string {
  const threadPart = ref.threadName ? `:${encodeBase64Url(ref.threadName)}` : '';
  const dmPart = ref.isDm ? ':dm' : '';
  return `gchat:${ref.spaceName}${threadPart}${dmPart}`;
}

/**
 * Normalize either delivery shape into one message record. Direct webhooks
 * arrive as `{ chat: { messagePayload: { message, space } } }`; Workspace
 * Events pushed through Pub/Sub arrive as `{ message, space }`.
 */
export function serializeGoogleChatMessage(
  envelope: GoogleChatEventEnvelope,
  threadKey: string,
  text: string,
): GoogleChatApiMessage {
  const message = googleChatMessagePayload(envelope);
  const space = googleChatSpacePayload(envelope);
  const spaceName = space?.name ?? message?.space?.name ?? decodeGoogleChatThreadKey(threadKey)?.spaceName ?? 'unknown-space';
  const sender = message?.sender;
  return {
    attachments: serializeAttachments(message?.attachment ?? []),
    author: {
      email: sender?.email,
      fullName: sender?.displayName,
      isBot: sender?.type === 'BOT',
      userId: sender?.name ?? 'unknown-user',
      userName: sender?.displayName,
    },
    id: message?.name ?? `gchat-${Date.now()}`,
    isMention: false,
    raw: toJsonValue(redactAttachmentUris(envelope)),
    spaceDisplayName: space?.displayName ?? message?.space?.displayName,
    spaceName,
    spaceType: space?.spaceType ?? space?.type ?? message?.space?.spaceType,
    text,
    threadId: threadKey,
    threadName: message?.thread?.name,
    timestamp: message?.createTime
      ? new Date(message.createTime).toISOString()
      : new Date().toISOString(),
  };
}

export function googleChatMessagePayload(envelope: GoogleChatEventEnvelope): GoogleChatRawMessage | undefined {
  return envelope.chat?.messagePayload?.message ?? envelope.message;
}

export function googleChatSpacePayload(envelope: GoogleChatEventEnvelope): GoogleChatRawSpace | undefined {
  return envelope.chat?.messagePayload?.space
    ?? envelope.space
    ?? envelope.chat?.messagePayload?.message?.space
    ?? envelope.message?.space;
}

/** A DM space, from either the event payload or the thread key's `:dm` marker. */
export function isDirectMessageSpace(message: GoogleChatApiMessage, threadKey: string): boolean {
  const spaceType = message.spaceType?.toUpperCase();
  if (spaceType === 'DIRECT_MESSAGE' || spaceType === 'DM') {
    return true;
  }
  return decodeGoogleChatThreadKey(threadKey)?.isDm === true;
}

/**
 * Space/sender policy, fail-closed like the other Centaur ingresses: an empty
 * allowlist means "ignore everything", never "allow everything".
 *
 * Every configured gate must pass:
 * - space: a non-DM message must be in `allowedSpaceIds`, which must be set.
 * - DM: a DM space is ignored unless `allowDirectMessages` is on *and* a sender
 *   allowlist is configured — a DM has no shared space to scope it by.
 * - sender: when `allowedSenderEmails`/`allowedDomains` are set, the sender's
 *   email must match one of them.
 */
export function isAllowedGoogleChatMessage(input: {
  allowDirectMessages: boolean;
  allowedDomains: readonly string[];
  allowedSenderEmails: readonly string[];
  allowedSpaceIds: readonly string[];
  message: GoogleChatApiMessage;
  threadKey: string;
}): boolean {
  const hasSenderAllowlist = input.allowedSenderEmails.length > 0 || input.allowedDomains.length > 0;
  const isDm = isDirectMessageSpace(input.message, input.threadKey);

  if (isDm) {
    if (!input.allowDirectMessages || !hasSenderAllowlist) {
      return false;
    }
  } else if (input.allowedSpaceIds.length === 0 || !input.allowedSpaceIds.includes(input.message.spaceName)) {
    return false;
  }

  if (!hasSenderAllowlist) {
    return true;
  }
  const email = input.message.author.email?.trim().toLowerCase();
  if (!email) {
    return false;
  }
  if (input.allowedSenderEmails.includes(email)) {
    return true;
  }
  const domain = email.split('@').at(-1);
  return Boolean(domain && input.allowedDomains.includes(domain));
}

function serializeAttachments(
  attachments: NonNullable<GoogleChatRawMessage['attachment']>,
): GoogleChatApiAttachment[] {
  return attachments.map((attachment, index) => ({
    contentType: attachment.contentType ?? 'application/octet-stream',
    downloadUri: undefined,
    name: attachment.contentName || `attachment-${index + 1}`,
    resourceName: attachment.attachmentDataRef?.resourceName ?? undefined,
  }));
}

/**
 * Google Chat attachment `downloadUri` values are authenticated, time-limited
 * URLs. They are dropped from the payload we persist and forward, exactly like
 * the Teams ingress redacts Graph download URLs.
 */
function redactAttachmentUris<T>(value: T): T {
  if (Array.isArray(value)) {
    return value.map(redactAttachmentUris) as T;
  }
  if (!isRecord(value)) {
    return value;
  }
  const output: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (key === 'downloadUri' && typeof entry === 'string') {
      output.downloadUriRedacted = true;
      continue;
    }
    if (key === 'thumbnailUri' && typeof entry === 'string') {
      output.thumbnailUriRedacted = true;
      continue;
    }
    output[key] = redactAttachmentUris(entry);
  }
  return output as T;
}

function decodeBase64Url(value: string): string | undefined {
  try {
    const decoded = Buffer.from(value, 'base64url').toString('utf-8');
    return decoded || undefined;
  } catch {
    return undefined;
  }
}

function encodeBase64Url(value: string): string {
  return Buffer.from(value, 'utf-8').toString('base64url');
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function toJsonValue(value: unknown): JsonValue {
  if (value === null || typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map(toJsonValue);
  }
  if (typeof value === 'object' && value !== null) {
    const output: Record<string, JsonValue> = {};
    for (const [key, entry] of Object.entries(value)) {
      if (entry !== undefined && typeof entry !== 'function') {
        output[key] = toJsonValue(entry);
      }
    }
    return output;
  }
  return String(value);
}
