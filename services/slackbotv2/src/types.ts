import type { RustSessionStreamEvent } from '@centaur/harness-events'
import type { CodexAppServerToChatStreamOptions } from '@centaur/rendering'
import type { Attachment, Chat, Logger, StateAdapter } from 'chat'
import type { Hono } from 'hono'

export type JsonPrimitive = string | number | boolean | null
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[]
export type JsonObject = { [key: string]: JsonValue | undefined }

export type SlackbotV2ApiAuthor = {
  fullName: string
  isBot: boolean | 'unknown'
  isMe: boolean
  userId: string
  userName: string
}

export type SlackbotV2ApiAttachment = {
  dataBase64?: string
  fetchError?: string
  fetchMetadata?: Record<string, string>
  height?: number
  mimeType?: string
  name?: string
  size?: number
  type: Attachment['type']
  url?: string
  width?: number
}

export type SlackbotV2ApiMessage = {
  attachments: SlackbotV2ApiAttachment[]
  author: SlackbotV2ApiAuthor
  id: string
  isMention: boolean
  raw: unknown
  teamId: string
  text: string
  threadId: string
  timestamp: string
}

export type SlackbotV2SessionMessageRole = 'user' | 'assistant' | 'system' | 'tool'

export type SlackbotV2SessionMessage = {
  client_message_id?: string
  metadata: JsonObject
  parts: JsonValue[]
  role: SlackbotV2SessionMessageRole
}

export type SlackbotV2AppendMessagesRequest = {
  messages: SlackbotV2SessionMessage[]
}

export type SlackbotV2CreateSessionRequest = {
  harness_type: string
  metadata: JsonObject
}

export type SlackbotV2ExecuteSessionRequest = {
  idempotency_key?: string
  idle_timeout_ms?: number
  input_lines: string[]
  max_duration_ms?: number
  metadata: JsonObject
}

export type SlackbotV2ExecuteSessionResponse = {
  execution_id: string
  ok: boolean
  status: string
  thread_key: string
}

export type SlackbotV2Fetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>

export type SlackbotV2Options = {
  allowedExternalTeamIds?: readonly string[]
  apiKey?: string
  apiUrl: string
  assistantStatus?: string
  botToken: string
  botUserId?: string
  fetch?: SlackbotV2Fetch
  idleTimeoutMs?: number
  logger?: Logger
  maxDurationMs?: number
  postgresUrl?: string
  recoverRenderObligationsOnStart?: boolean
  /** Per-thread deadline for one recovery attempt during the startup scan. */
  renderRecoveryThreadTimeoutMs?: number
  signingSecret: string
  slackApiUrl?: string
  state?: StateAdapter
  stateKeyPrefix?: string
  streamTaskDisplayMode?: 'plan' | 'timeline'
  triggerBotAllowlist?: readonly string[]
  userName?: string
  mapper?: CodexAppServerToChatStreamOptions
}

export type SlackbotV2 = {
  app: Hono
  chat: Chat
}

export type SlackbotV2ThreadState = {
  activeExecution?: boolean
  executedMessageIds?: string[]
  forwardedMessageIds?: string[]
  historyForwarded?: boolean
  lastEventId?: number
  renderObligation?: SlackbotV2RenderObligation | null
}

export type SlackbotV2RenderObligation = {
  afterEventId: number
  executionId: string
  message: SlackbotV2ApiMessage
}

export type SlackbotV2MessageMode = 'append' | 'execute'

export type SlackbotV2RendererSource = RustSessionStreamEvent | JsonObject

export type SlackbotV2Trace = {
  includeContext: boolean
  messageId: string
  mode: SlackbotV2MessageMode
  openStream: boolean
  startedAtMs: number
  threadId: string
}

export type ForwardSessionInput = {
  afterEventId: number
  executionId?: string
  executeMessage?: SlackbotV2ApiMessage
  messages: SlackbotV2ApiMessage[]
  onEventId(eventId: number): void
  openStream: boolean
  threadId: string
  trace?: SlackbotV2Trace
}
