import {
  apiSessionUrl,
  ensureSessionResponseOk,
  type RequestRetryEvent,
  requestWithRetries,
  SessionApiError,
  streamSessionEvents,
} from './session-transport.js';
import type { Logger } from 'chat';
import type {
  AppendMessagesRequest,
  CreateSessionRequest,
  ExecuteSessionRequest,
  ExecuteSessionResponse,
  FetchFn,
  GoogleChatApiAttachment,
  GoogleChatApiMessage,
  JsonObject,
  JsonValue,
  SessionMessage,
  SessionStreamEvent,
} from './types.js';

export { SessionApiError } from './session-transport.js';

const MAX_CODEX_INPUT_LINE_CHARS = 900 * 1024;
const STAGED_ATTACHMENT_CHUNK_CHARS = 700 * 1024;

export type CentaurSessionClientOptions = {
  apiKey?: string;
  apiUrl: string;
  defaultHarnessType?: string;
  fetch?: FetchFn;
  idleTimeoutMs?: number;
  logger?: Logger;
  maxDurationMs?: number;
  requestMaxRetries?: number;
  requestRetryDelayMs?: number;
};

export class CentaurSessionClient {
  constructor(private readonly options: CentaurSessionClientOptions) {}

  async createSession(threadId: string, message?: GoogleChatApiMessage): Promise<void> {
    const harnessType = this.options.defaultHarnessType ?? 'codex';
    const response = await this.postCreateSession(threadId, harnessType, message);
    if (response.ok) {
      return;
    }
    const body = await safeResponseText(response);
    const existingHarness = response.status === 409 ? existingHarnessFromConflict(body) : undefined;
    if (existingHarness && existingHarness !== harnessType) {
      const retry = await this.postCreateSession(threadId, existingHarness, message);
      await ensureSessionResponseOk(retry, 'create session');
      return;
    }
    throw new SessionApiError({
      action: 'create session',
      body,
      retryable: isRetryableSessionStatus(response.status),
      status: response.status,
      statusText: response.statusText,
    });
  }

  private async postCreateSession(
    threadId: string,
    harnessType: string,
    message?: GoogleChatApiMessage,
  ): Promise<Response> {
    const body: CreateSessionRequest = {
      harness_type: harnessType,
      metadata: {
        source: 'googlechatbot',
        platform: 'googlechat',
        thread_id: threadId,
        ...requesterMetadata(message),
      },
    };
    return requestWithRetries({
      action: 'create session',
      maxRetries: this.options.requestMaxRetries,
      onRetry: this.logRetry,
      retryDelayMs: this.options.requestRetryDelayMs,
      operation: async () => {
        const response = await this.fetch(apiSessionUrl(this.options.apiUrl, threadId), {
          method: 'POST',
          headers: this.headers(),
          body: JSON.stringify(body),
        });
        if (!response.ok && response.status !== 409 && isRetryableSessionStatus(response.status)) {
          throw new SessionApiError({
            action: 'create session',
            body: await safeResponseText(response),
            retryable: true,
            status: response.status,
            statusText: response.statusText,
          });
        }
        return response;
      },
    });
  }

  async appendMessages(threadId: string, messages: GoogleChatApiMessage[]): Promise<void> {
    const body: AppendMessagesRequest = {
      messages: messages.map(toSessionMessage),
    };
    await requestWithRetries({
      action: 'append session messages',
      maxRetries: this.options.requestMaxRetries,
      onRetry: this.logRetry,
      retryDelayMs: this.options.requestRetryDelayMs,
      operation: async () => {
        const response = await this.fetch(apiSessionUrl(this.options.apiUrl, threadId, 'messages'), {
          method: 'POST',
          headers: this.headers(),
          body: JSON.stringify(body),
        });
        await ensureSessionResponseOk(response, 'append session messages');
      },
    });
  }

  async executeSession(
    threadId: string,
    message: GoogleChatApiMessage,
    options: { signal?: AbortSignal } = {},
  ): Promise<ExecuteSessionResponse> {
    const body: ExecuteSessionRequest = {
      idempotency_key: message.id,
      metadata: sessionMetadata(message, { action: 'execute' }),
      input_lines: toCodexInputLines(message, threadId),
      ...(this.options.idleTimeoutMs === undefined ? {} : { idle_timeout_ms: this.options.idleTimeoutMs }),
      ...(this.options.maxDurationMs === undefined ? {} : { max_duration_ms: this.options.maxDurationMs }),
    };
    return requestWithRetries({
      action: 'execute session',
      maxRetries: this.options.requestMaxRetries,
      onRetry: this.logRetry,
      retryDelayMs: this.options.requestRetryDelayMs,
      operation: async () => {
        const response = await this.fetch(apiSessionUrl(this.options.apiUrl, threadId, 'execute'), {
          method: 'POST',
          headers: this.headers(),
          signal: options.signal,
          body: JSON.stringify(body),
        });
        await ensureSessionResponseOk(response, 'execute session');
        return (await response.json()) as ExecuteSessionResponse;
      },
    });
  }

  async streamEvents(input: {
    afterEventId: number;
    executionId?: string;
    onEventId(eventId: number): void;
    signal?: AbortSignal;
    threadId: string;
  }): Promise<AsyncIterable<SessionStreamEvent>> {
    return streamSessionEvents({
      afterEventId: input.afterEventId,
      apiUrl: this.options.apiUrl,
      executionId: input.executionId,
      fetch: this.fetch.bind(this),
      headers: this.headers(false),
      maxRetries: this.options.requestMaxRetries,
      onRetry: this.logRetry,
      onEventId: input.onEventId,
      retryDelayMs: this.options.requestRetryDelayMs,
      signal: input.signal,
      threadId: input.threadId,
    });
  }

  private fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    return (this.options.fetch ?? fetch)(input, init);
  }

  private headers(jsonBody = true): HeadersInit {
    return {
      ...(jsonBody ? { 'content-type': 'application/json' } : {}),
      ...(this.options.apiKey ? { authorization: `Bearer ${this.options.apiKey}` } : {}),
    };
  }

  private logRetry = (event: RequestRetryEvent): void => {
    this.options.logger?.warn('centaur_session_request_retrying', event);
  };
}

export function toSessionMessage(message: GoogleChatApiMessage): SessionMessage {
  return {
    client_message_id: message.id,
    role: message.author.isBot ? 'assistant' : 'user',
    parts: sessionMessageParts(message),
    metadata: sessionMetadata(message),
  };
}

export function sessionMessageParts(message: GoogleChatApiMessage): JsonValue[] {
  const parts: JsonValue[] = [];
  if (message.text.trim()) {
    parts.push({ type: 'text', text: message.text });
  }
  for (const attachment of message.attachments) {
    parts.push(sessionAttachmentPart(attachment));
  }
  return parts.length > 0 ? parts : [{ type: 'text', text: '' }];
}

function sessionAttachmentPart(attachment: GoogleChatApiAttachment): JsonObject {
  const part: JsonObject = {
    attachment_type: 'googlechat',
    contentType: attachment.contentType,
    mimeType: attachment.contentType,
    name: attachment.name,
    type: 'attachment',
  };
  if (attachment.dataBase64) {
    part.dataBase64 =
      attachment.dataBase64.length > MAX_CODEX_INPUT_LINE_CHARS
        ? undefined
        : attachment.dataBase64;
    if (!part.dataBase64) {
      part.dataBase64Omitted = `${attachment.dataBase64.length} base64 chars omitted from stored session message`;
    }
  }
  if (attachment.fetchError) {
    part.fetchError = attachment.fetchError;
  }
  return part;
}

export function sessionMetadata(message: GoogleChatApiMessage, extra: JsonObject = {}): JsonObject {
  return {
    source: 'googlechatbot',
    platform: 'googlechat',
    message_id: message.id,
    thread_id: message.threadId,
    is_mention: message.isMention,
    timestamp: message.timestamp,
    space_id: message.spaceName,
    space_type: message.spaceType,
    gchat_thread_name: message.threadName,
    user_id: message.author.userId,
    user_name: message.author.userName,
    user_email: message.author.email,
    gchat_conversation_name: message.spaceDisplayName
      ?? (message.author.fullName || message.author.userName || undefined),
    ...extra,
  };
}

export function toCodexInputLines(message: GoogleChatApiMessage, threadId: string): string[] {
  const staged = new Map<GoogleChatApiAttachment, string>();
  const lines: string[] = [];
  for (const attachment of message.attachments) {
    if (!attachment.dataBase64) {
      continue;
    }
    const inlineLine = toCodexInputLineWithStaged(message, threadId, staged);
    if (inlineLine.length <= MAX_CODEX_INPUT_LINE_CHARS && attachment.dataBase64.length <= MAX_CODEX_INPUT_LINE_CHARS) {
      continue;
    }
    const stagedAttachmentId = `att-${message.id}-${staged.size + 1}`;
    staged.set(attachment, stagedAttachmentId);
    lines.push(...stagedAttachmentInputLines(attachment, stagedAttachmentId));
  }
  lines.push(toCodexInputLineWithStaged(message, threadId, staged));
  return lines;
}

function toCodexInputLineWithStaged(
  message: GoogleChatApiMessage,
  threadId: string,
  staged: Map<GoogleChatApiAttachment, string>,
): string {
  return JSON.stringify({
    type: 'user',
    thread_key: threadId,
    trace_metadata: sessionMetadata(message, { action: 'execute' }),
    message: {
      role: 'user',
      content: codexInputContent(message, staged),
    },
  });
}

function codexInputContent(
  message: GoogleChatApiMessage,
  staged: Map<GoogleChatApiAttachment, string>,
): JsonValue[] {
  const content: JsonValue[] = [];
  if (message.text.trim()) {
    content.push({ type: 'text', text: message.text });
  }
  for (const attachment of message.attachments) {
    content.push(codexAttachmentInput(attachment, staged.get(attachment)));
  }
  return content.length > 0 ? content : [{ type: 'text', text: 'continue' }];
}

function codexAttachmentInput(attachment: GoogleChatApiAttachment, stagedAttachmentId?: string): JsonValue {
  if (stagedAttachmentId) {
    return {
      type: 'attachment',
      attachment_type: 'googlechat',
      stagedAttachmentId,
      name: attachment.name,
      contentType: attachment.contentType,
      mimeType: attachment.contentType,
    };
  }
  if (!attachment.dataBase64) {
    return {
      type: 'text',
      text: attachmentUnavailableDescription(attachment),
    };
  }
  return sessionAttachmentPart(attachment);
}

function attachmentUnavailableDescription(attachment: GoogleChatApiAttachment): string {
  return [
    `Google Chat attachment was not downloaded: ${attachment.name ?? '(unnamed)'}`,
    `Content-Type: ${attachment.contentType}`,
    attachment.fetchError ? `Download error: ${attachment.fetchError}` : '',
  ].filter(Boolean).join('\n');
}

function stagedAttachmentInputLines(
  attachment: GoogleChatApiAttachment,
  stagedAttachmentId: string,
): string[] {
  const dataBase64 = attachment.dataBase64;
  if (!dataBase64) {
    return [];
  }
  const lines: string[] = [];
  const chunkSize = STAGED_ATTACHMENT_CHUNK_CHARS - (STAGED_ATTACHMENT_CHUNK_CHARS % 4);
  for (let offset = 0, index = 0; offset < dataBase64.length; offset += chunkSize, index += 1) {
    const chunk = dataBase64.slice(offset, offset + chunkSize);
    lines.push(JSON.stringify({
      type: 'attachment.chunk',
      attachmentId: stagedAttachmentId,
      name: attachment.name,
      mimeType: attachment.contentType,
      attachmentType: 'googlechat',
      chunkIndex: index,
      final: offset + chunkSize >= dataBase64.length,
      dataBase64: chunk,
    }));
  }
  return lines;
}

function requesterMetadata(message?: GoogleChatApiMessage): JsonObject {
  return {
    ...(message?.spaceName ? { space_id: message.spaceName } : {}),
    ...(message?.spaceType ? { space_type: message.spaceType } : {}),
    ...(message?.author.userId ? { user_id: message.author.userId } : {}),
    ...(message?.author.userId ? { gchat_user_id: message.author.userId } : {}),
    ...(message?.author.userName ? { gchat_user_name: message.author.userName } : {}),
    ...(message?.author.email ? { user_email: message.author.email } : {}),
    ...(conversationName(message) ? { gchat_conversation_name: conversationName(message) } : {}),
  };
}

function conversationName(message?: GoogleChatApiMessage): string | undefined {
  return message?.spaceDisplayName
    || message?.author.fullName
    || message?.author.userName
    || undefined;
}

async function safeResponseText(response: Response): Promise<string> {
  try {
    return await response.text();
  } catch {
    return '';
  }
}

function isRetryableSessionStatus(status: number): boolean {
  return status === 408 || status === 429 || status >= 500;
}

function existingHarnessFromConflict(body: string): string | undefined {
  try {
    const parsed = JSON.parse(body) as unknown;
    if (isJsonObject(parsed)) {
      const existing = parsed.existing_harness;
      if (typeof existing === 'string' && existing.trim()) {
        return existing;
      }
    }
  } catch {
    // Fall through to the plain-text error parser.
  }
  const match = body.match(/already exists with harness_type\s+([A-Za-z0-9_-]+)/);
  return match?.[1];
}

function isJsonObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
