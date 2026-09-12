export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export type JsonObject = { [key: string]: JsonValue | undefined };
export type FetchFn = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

/**
 * The subset of a Google Chat event we read. Both delivery paths are covered:
 * the Workspace Add-on webhook shape (`chat.messagePayload`) and the Workspace
 * Events notification delivered through Pub/Sub (`message` + `space`), which is
 * what the adapter hands us as `Message.raw`.
 */
export type GoogleChatEventEnvelope = {
  chat?: {
    eventTime?: string;
    messagePayload?: {
      message?: GoogleChatRawMessage;
      space?: GoogleChatRawSpace;
    };
    user?: GoogleChatRawUser;
  };
  eventTime?: string;
  eventType?: string;
  message?: GoogleChatRawMessage;
  space?: GoogleChatRawSpace;
} & Record<string, unknown>;

export type GoogleChatRawMessage = {
  annotations?: Array<{
    length?: number;
    startIndex?: number;
    type?: string;
    userMention?: {
      type?: string;
      user?: { displayName?: string; name?: string; type?: string };
    };
  }>;
  argumentText?: string;
  attachment?: Array<{
    attachmentDataRef?: { resourceName?: string | null } | null;
    contentName?: string;
    contentType?: string;
    downloadUri?: string;
    name?: string;
  }>;
  createTime?: string;
  formattedText?: string;
  name?: string;
  sender?: GoogleChatRawUser;
  space?: GoogleChatRawSpace;
  text?: string;
  thread?: { name?: string };
} & Record<string, unknown>;

export type GoogleChatRawSpace = {
  displayName?: string;
  name?: string;
  singleUserBotDm?: boolean;
  spaceType?: string;
  type?: string;
} & Record<string, unknown>;

export type GoogleChatRawUser = {
  displayName?: string;
  domainId?: string;
  email?: string;
  name?: string;
  type?: string;
};

export type GoogleChatApiAuthor = {
  email?: string;
  fullName?: string;
  isBot: boolean;
  userId: string;
  userName?: string;
};

export type GoogleChatApiAttachment = {
  contentType: string;
  dataBase64?: string;
  downloadUri?: string;
  fetchError?: string;
  name?: string;
  resourceName?: string;
};

export type GoogleChatApiMessage = {
  attachments: GoogleChatApiAttachment[];
  author: GoogleChatApiAuthor;
  id: string;
  isMention: boolean;
  raw: JsonValue;
  spaceDisplayName?: string;
  spaceName: string;
  spaceType?: string;
  text: string;
  threadId: string;
  threadName?: string;
  timestamp: string;
};

export type SessionMessageRole = 'user' | 'assistant' | 'system' | 'tool';

export type SessionMessage = {
  client_message_id?: string;
  metadata: JsonObject;
  parts: JsonValue[];
  role: SessionMessageRole;
};

export type CreateSessionRequest = {
  harness_type: string;
  metadata: JsonObject;
};

export type AppendMessagesRequest = {
  messages: SessionMessage[];
};

export type ExecuteSessionRequest = {
  idempotency_key?: string;
  idle_timeout_ms?: number;
  input_lines: string[];
  max_duration_ms?: number;
  metadata: JsonObject;
};

export type ExecuteSessionResponse = {
  execution_id: string;
  ok: boolean;
  status: string;
  thread_key: string;
};

export type SessionStreamEvent = {
  data: unknown;
  event: string;
  eventId?: number;
  eventKind: string;
};

export type GoogleChatThreadState = {
  active: boolean;
  activeExecution?: boolean;
  /**
   * Epoch ms when `activeExecution` was last set. The flag is ignored once it
   * is older than the configured TTL so a crashed render does not wedge a
   * Google Chat thread forever.
   */
  activeExecutionStartedAt?: number | null;
  appendBarrier?: boolean;
  appendInFlight?: number;
  executedMessageIds?: string[];
  forwardedMessageIds?: string[];
  lastEventId?: number;
  renderObligation?: {
    afterEventId: number;
    executionId?: string;
    message: GoogleChatApiMessage;
    progressMessageId?: string;
  } | null;
};

export interface GoogleChatThreadStateStore {
  get(threadKey: string): Promise<GoogleChatThreadState | undefined>;
  list(): Promise<Array<{ state: GoogleChatThreadState; threadKey: string }>>;
  set(threadKey: string, state: GoogleChatThreadState): Promise<void>;
}

export interface GoogleChatRenderRecoveryStateStore extends GoogleChatThreadStateStore {
  acquireInboundMessageLease(threadKey: string, messageId: string, ttlMs: number): Promise<(() => Promise<void>) | null>;
  acquireLiveRenderLease(threadKey: string, ttlMs: number): Promise<() => Promise<void>>;
  acquireRenderRecoveryLease(threadKey: string, ttlMs: number): Promise<(() => Promise<void>) | null>;
  acquireThreadTurnLease(threadKey: string, ttlMs: number): Promise<(() => Promise<void>) | null>;
  indexRenderObligation(threadKey: string, options: { maxLength: number; ttlMs: number }): Promise<void>;
  listRenderObligationThreadKeys(): Promise<string[]>;
}

/**
 * Everything needed to post into a Google Chat thread after a restart, when no
 * live `Thread` handle exists. Google Chat is addressed by resource names, so
 * the space (and thread, when the message was threaded) is the whole reference.
 */
export type StoredSpaceReference = {
  isDm?: boolean;
  spaceDisplayName?: string;
  spaceName: string;
  spaceType?: string;
  threadName?: string;
};

export interface SpaceReferenceStore {
  getReference(threadKey: string): Promise<StoredSpaceReference | undefined>;
  setReference(threadKey: string, reference: StoredSpaceReference): Promise<void>;
}
