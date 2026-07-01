export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export type JsonObject = { [key: string]: JsonValue | undefined };
export type FetchFn = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export type TeamsActivity = {
  attachments?: Array<{
    content?: unknown;
    contentType?: string;
    contentUrl?: string;
    name?: string;
  }>;
  channelData?: {
    channel?: { id?: string };
    team?: { id?: string };
    teamsChannelId?: string;
    teamsTeamId?: string;
    tenant?: { id?: string };
  } & Record<string, unknown>;
  channelId?: string;
  conversation?: { id?: string; conversationType?: string; tenantId?: string };
  entities?: Array<{
    mentioned?: { id?: string; name?: string };
    quotedReply?: {
      isReplyDeleted?: boolean;
      messageId?: string;
      preview?: string | null;
      senderId?: string | null;
      senderName?: string | null;
      time?: string | null;
    };
    text?: string;
    type?: string;
  }>;
  from?: { aadObjectId?: string; id?: string; name?: string };
  id?: string;
  localTimestamp?: string;
  localTimezone?: string;
  recipient?: { id?: string; name?: string };
  replyToId?: string;
  serviceUrl?: string;
  text?: string;
  textFormat?: string;
  timestamp?: string | Date;
  type?: string;
} & Record<string, unknown>;

export type TeamsApiAuthor = {
  aadObjectId?: string;
  fullName?: string;
  isBot: boolean;
  userId: string;
  userName?: string;
};

export type TeamsApiAttachment = {
  content?: JsonValue;
  contentType: string;
  contentUrl?: string;
  dataBase64?: string;
  fetchError?: string;
  name?: string;
};

export type TeamsApiMessage = {
  attachments: TeamsApiAttachment[];
  author: TeamsApiAuthor;
  channelId?: string;
  conversationId: string;
  conversationType?: string;
  id: string;
  isMention: boolean;
  raw: JsonValue;
  teamId?: string;
  tenantId?: string;
  text: string;
  threadId: string;
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

export type TeamsThreadState = {
  active: boolean;
  activeExecution?: boolean;
  /**
   * Epoch ms when `activeExecution` was last set. The flag is ignored once it
   * is older than the configured TTL so a crashed render does not wedge a Teams
   * thread forever.
   */
  activeExecutionStartedAt?: number | null;
  appendBarrier?: boolean;
  appendInFlight?: number;
  executedMessageIds?: string[];
  forwardedMessageIds?: string[];
  historyForwarded?: boolean;
  lastEventId?: number;
  renderObligation?: {
    afterEventId: number;
    contextMessages?: TeamsApiMessage[];
    executionId?: string;
    message: TeamsApiMessage;
    progressActivityId?: string;
  } | null;
};

export interface TeamsThreadStateStore {
  get(threadKey: string): Promise<TeamsThreadState | undefined>;
  list(): Promise<Array<{ state: TeamsThreadState; threadKey: string }>>;
  set(threadKey: string, state: TeamsThreadState): Promise<void>;
}

export interface TeamsRenderRecoveryStateStore extends TeamsThreadStateStore {
  acquireInboundMessageLease(threadKey: string, messageId: string, ttlMs: number): Promise<(() => Promise<void>) | null>;
  acquireLiveRenderLease(threadKey: string, ttlMs: number): Promise<() => Promise<void>>;
  acquireRenderRecoveryLease(threadKey: string, ttlMs: number): Promise<(() => Promise<void>) | null>;
  acquireThreadTurnLease(threadKey: string, ttlMs: number): Promise<(() => Promise<void>) | null>;
  indexRenderObligation(threadKey: string, options: { maxLength: number; ttlMs: number }): Promise<void>;
  listRenderObligationThreadKeys(): Promise<string[]>;
}

export type StoredConversationReference = {
  activityId?: string;
  bot?: JsonObject;
  channelId?: string;
  conversation?: JsonObject;
  conversationId: string;
  conversationType?: string;
  serviceUrl?: string;
  teamId?: string;
  tenantId?: string;
  user?: JsonObject;
};

export interface ConversationReferenceStore {
  getReference(threadKey: string): Promise<StoredConversationReference | undefined>;
  setReference(threadKey: string, reference: StoredConversationReference): Promise<void>;
}
