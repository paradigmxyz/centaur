import type { RustSessionStreamEvent } from '@centaur/harness-events'
import type { CodexAppServerToChatStreamOptions } from '@centaur/rendering'
import type { Attachment, Chat, Logger, StateAdapter } from 'chat'
import type { Hono } from 'hono'
import type { ChannelDefaults } from './channel-defaults'
import type { SlackDisplayTextSource } from './slack-display-text'

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
  dataBase64Omitted?: string
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

export type SlackbotV2ApiMessageLink = {
  description?: string
  imageUrl?: string
  isSlackMessage?: boolean
  siteName?: string
  title?: string
  url: string
}

export type SlackbotV2ApiMessage = {
  attachments: SlackbotV2ApiAttachment[]
  author: SlackbotV2ApiAuthor
  displayText?: string
  displayTextSource?: SlackDisplayTextSource
  id: string
  isMention: boolean
  links?: SlackbotV2ApiMessageLink[]
  raw: unknown
  rawSlackAttachmentCount?: number
  rawSlackBlockCount?: number
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
  /** 'restart': switch the thread to harness_type if it's pinned to another harness. */
  on_harness_conflict?: 'reject' | 'restart'
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

export type SlackbotV2InterruptSessionResponse = {
  execution_id?: string
  interrupted: boolean
  ok: boolean
  thread_key: string
}

export type SlackbotV2Fetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>

export type SlackbotV2Options = {
  allowedExternalTeamIds?: readonly string[]
  apiKey?: string
  apiUrl: string
  assistantStatus?: string
  /**
   * When enabled, session.activity_summary events update Slack's assistant
   * status and structured task output is hidden from the Slack stream.
   */
  activitySummaryStatusEnabled?: boolean
  botToken: string
  botUserId?: string
  /**
   * Public origin of the Console UI (same value the Console itself uses,
   * `CENTAUR_CONSOLE_PUBLIC_URL`). When set, the first assistant message in a
   * Slack thread gets an "Open chat in Console" context link. Unset skips
   * the block entirely.
   */
  consolePublicUrl?: string
  /**
   * Per-channel default harness/model/provider/reasoning, keyed by Slack
   * conversation id (SLACKBOTV2_CHANNEL_DEFAULTS). See channel-defaults.ts.
   */
  channelDefaults?: ChannelDefaults
  /**
   * Harness for new threads when no --claude/--amp/--codex flag is given
   * (HarnessType wire value: codex | amp | claudecode). Defaults to codex.
   */
  defaultHarnessType?: string
  fetch?: SlackbotV2Fetch
  /**
   * Deployment-configured default model per harness wire value (claudecode |
   * codex), from the CLAUDE_MODEL / CODEX_MODEL env vars the chart mirrors
   * out of sandbox.extraEnv. Display/metadata only — never forwarded to the
   * harness. Unset harnesses fall back to the models pinned in this repo's
   * harness config files (see console-session-link.ts).
   */
  harnessDefaultModels?: Record<string, string>
  /**
   * Backoff delays between in-process retries of a Slack handoff after a
   * retryable session API failure. Slack's own webhook redelivery cannot
   * drive these retries: Slack times deliveries out after ~3s, so its
   * redelivery races the still-running original attempt, is deduped, and is
   * acknowledged before the original attempt fails. The bot retries locally
   * instead and posts a visible error once the delays are exhausted.
   */
  handoffRetryDelaysMs?: readonly number[]
  /** Milliseconds before an idle execution pauses its sandbox. Defaults to up to 3h. */
  idleTimeoutMs?: number
  logger?: Logger
  maxDurationMs?: number
  postgresUrl?: string
  recoverRenderObligationsOnStart?: boolean
  /** Maximum Slack message age eligible for startup render recovery. */
  renderRecoveryMaxObligationAgeMs?: number
  /** Per-thread deadline for one recovery attempt during the startup scan. */
  renderRecoveryThreadTimeoutMs?: number
  /** Deadline for Centaur session API HTTP calls made during Slack handoff. */
  sessionApiTimeoutMs?: number
  signingSecret: string
  slackApiUrl?: string
  /** Deadline for optional Slack Web API metadata lookups. */
  slackApiTimeoutMs?: number
  state?: StateAdapter
  stateKeyPrefix?: string
  streamTaskDisplayMode?: 'none' | 'plan' | 'timeline'
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
  /** Last thread-level harness selected by Slack flags. Null clears persisted state. */
  harnessType?: string | null
  historyForwarded?: boolean
  lastEventId?: number
  /** Last thread-level model selected by Slack flags. Null clears persisted state. */
  model?: string | null
  /** Last thread-level model provider selected by Slack flags. Null clears persisted state. */
  provider?: string | null
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
  slackUserId?: string
  startedAtMs: number
  threadId: string
}

export type ForwardSessionInput = {
  afterEventId: number
  executeContextMessages?: SlackbotV2ApiMessage[]
  /**
   * Prepended to the execute message content as a text part. Set when a
   * harness restart discards the previous harness's conversation state so the
   * new harness still sees the thread history.
   */
  contextPreamble?: string
  executionId?: string
  executeMessage?: SlackbotV2ApiMessage
  /** Effective harness selected by sticky thread flags (--claude/--amp/--codex). */
  harnessType?: string
  messages: SlackbotV2ApiMessage[]
  /** Effective model selected by sticky thread flags (--model/--opus/...). */
  model?: string
  /**
   * Model recorded in execute metadata for readers like the Console: the
   * explicit override when one is set, else the configured/baked harness
   * default. Metadata only — never forwarded to the harness (that is `model`).
   */
  metadataModel?: string
  /** Effective model provider selected by sticky thread flags (--bedrock); codex only. */
  provider?: string
  /** Per-turn reasoning effort parsed from the `-rsn` flag (codex only). */
  reasoning?: string
  onEventId(eventId: number): void
  openStream: boolean
  threadId: string
  trace?: SlackbotV2Trace
}
