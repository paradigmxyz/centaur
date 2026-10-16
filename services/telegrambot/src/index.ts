import { randomUUID } from "node:crypto";
import {
  harnessToChatSdkStream,
  type CodexAppServerToChatStreamOptions,
  type RendererEvent,
} from "@centaur/rendering";
import {
  createTelegramAdapter,
  type TelegramAdapter,
} from "@chat-adapter/telegram";
import { createPostgresState } from "@chat-adapter/state-pg";
import {
  Chat,
  type Adapter,
  type Logger,
  type Message as ChatMessage,
  type SlashCommandEvent,
  type StateAdapter,
  type Thread,
  markdownToPlainText,
  ThreadHistoryCache,
} from "chat";
import { Hono } from "hono";
import pg from "pg";
import {
  executeSessionTurn,
  forwardToSessionApi,
  isContentlessApiMessage,
  isRetryableSessionApiError,
  openSessionEventStream,
  serializeMessage,
  sessionStreamError,
  startingStreamNotification,
} from "./session-api";
import {
  REACTION_CONTENTLESS,
  REACTION_NOTED,
  reactToTelegramMessage,
  TelegramRunIndicator,
} from "./telegram-narrator";
import {
  conversationName,
  isAllowedTelegramMessage,
  isAllowlistEmpty,
  isStorableTelegramMessage,
  messageTrigger,
  routeCommand,
  type TelegramPolicyMessage,
  withReplyQuote,
} from "./telegram-policy";
import type {
  ForwardSessionInput,
  TelegramTrigger,
  Telegrambot,
  TelegrambotApiMessage,
  TelegrambotExecuteSessionResponse,
  TelegrambotOptions,
  TelegrambotRenderObligation,
  TelegrambotRendererSource,
  TelegrambotThreadState,
  TelegrambotTrace,
} from "./types";
import {
  AsyncTextQueue,
  elapsedMs,
  errorMessage,
  noopLogger,
  nowMs,
  sleep,
  takeMessageChunk,
  traceLog,
  truncateWithNotice,
} from "./utils";

export type {
  Telegrambot,
  TelegrambotApiAttachment,
  TelegrambotApiAuthor,
  TelegrambotApiMessage,
  TelegrambotAppendMessagesRequest,
  TelegrambotCreateSessionRequest,
  TelegrambotExecuteSessionRequest,
  TelegrambotExecuteSessionResponse,
  TelegrambotFetch,
  TelegrambotMode,
  TelegrambotOptions,
  TelegrambotSessionMessage,
  TelegrambotSessionMessageRole,
  TelegrambotThreadState,
} from "./types";

/** Hono route Telegram posts updates to in webhook mode. */
export const TELEGRAM_WEBHOOK_PATH = "/api/webhooks/telegram";

// Telegram's chat action ("typing…") expires after about five seconds.
const TYPING_KEEPALIVE_MS = 4_000;
const RENDER_OBLIGATION_INDEX_KEY = "telegrambot:render:index";
const RENDER_OBLIGATION_INDEX_MAX_LENGTH = 2000;
const RENDER_INDEX_TTL_MS = 30 * 24 * 60 * 60 * 1000;
const RENDER_RECOVERY_LEASE_TTL_MS = 2 * 60 * 1000;
const RENDER_LEASE_REFRESH_INTERVAL_MS = 60 * 1000;
const RENDER_RETRY_INITIAL_DELAY_MS = 250;
const RENDER_RETRY_MAX_DELAY_MS = 5_000;
// Ported from discordbot: an unbounded render retry loop replays the stream
// from the original afterEventId forever whenever the error keeps classifying
// retryable; the persisted obligation still lets the next restart retry.
const RENDER_RETRY_MAX_ATTEMPTS = 10;
const POSTGRES_CONNECT_INITIAL_DELAY_MS = 250;
const POSTGRES_CONNECT_MAX_DELAY_MS = 10_000;
// A crash between marking `activeExecution` and the render clearing it would
// otherwise block every later turn in the chat.
const ACTIVE_EXECUTION_TTL_MS = 30 * 60 * 1000;
// Bounded in-process retry for the create/append handoff. In polling mode a
// failure that survives it is re-thrown so the adapter's durable polling
// checkpoint redelivers the update with backoff (the Slack-503 analog).
const FORWARD_RETRY_DELAYS_MS = [1_000, 3_000];
// Matches Chat SDK's per-thread lock TTL for the slash-command path, which
// the SDK does not lock on its own.
const COMMAND_LOCK_TTL_MS = 30_000;

// observeGroups: per-chat history kept for group context, and how much of it
// rides along with a trigger.
const GROUP_HISTORY_MAX_MESSAGES = 200;
const GROUP_HISTORY_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const GROUP_CONTEXT_MAX_MESSAGES = 50;
const GROUP_CONTEXT_MESSAGE_MAX_CHARS = 2_000;
// Telegram rejects text over 4096 characters after entity parsing; MarkdownV2
// escaping can grow the rendered text, so source chunks leave headroom.
const ANSWER_MESSAGE_MAX_CHARS = 3_500;
const ANSWER_MAX_FULL_MESSAGES = 8;
// Bot API flood limits: ~1 message/second per private chat and ~20 a minute
// per group, edits included. These are floors for answer edits.
const ANSWER_EDIT_INTERVAL_PRIVATE_MS = 1_500;
const ANSWER_EDIT_INTERVAL_GROUP_MS = 3_100;
const ANSWER_EDIT_INTERVAL_MIN_MS = 1_000;

const USAGE_TEXT = [
  "Ask me something:",
  "• in a private chat, just send a message;",
  "• in a group, reply to one of my messages or send `/ask@<bot> <question>`.",
].join("\n");

export function createTelegrambot(options: TelegrambotOptions): Telegrambot {
  const logger = options.logger ?? noopLogger;
  const mode = options.mode ?? "polling";

  if (isAllowlistEmpty(options)) {
    logger.warn("telegrambot_allowlist_empty_inert", {
      hint: "Set TELEGRAMBOT_CHAT_ALLOWLIST and/or TELEGRAMBOT_USER_ALLOWLIST; the bot ignores all messages until configured.",
    });
  }

  const telegram = createTelegramAdapter({
    apiUrl: options.telegramApiUrl,
    botToken: options.botToken,
    mode,
    secretToken: options.webhookSecretToken,
    // Explicit values so ambient TELEGRAM_* variables can never loosen them:
    // webhooks always require the secret token, and authorization is this
    // service's allowlist gate, not the adapter's user filter (which would
    // also drop allowlisted-group members).
    allowUnverifiedWebhooks: false,
    allowedUserIds: [],
    // A reply to one of the bot's messages reaches onNewMention; plain
    // @mentions also do, and are rejected by messageTrigger.
    mentionOnReply: true,
    userName: options.userName,
    logger: logger.child("telegram"),
    longPolling: {
      // Only new messages; edits, channel posts, callbacks, and reactions are
      // never fetched. Telegram remembers this list for the bot.
      allowedUpdates: ["message"],
      ...(options.pollTimeoutSeconds
        ? { timeout: options.pollTimeoutSeconds }
        : {}),
    },
  });
  // The SDK would otherwise store every incoming message (Telegram has no
  // history API) before any handler runs, including chats this service never
  // admits. Store only after the allowlist check, through the same SDK cache.
  (telegram as { persistThreadHistory: boolean }).persistThreadHistory = false;
  const state = options.state ?? createDefaultState(options, logger);
  const history = options.observeGroups
    ? new ThreadHistoryCache(state, {
        maxMessages: GROUP_HISTORY_MAX_MESSAGES,
        ttlMs: GROUP_HISTORY_TTL_MS,
      })
    : undefined;
  const chat = new Chat<{ telegram: typeof telegram }, TelegrambotThreadState>({
    userName: options.userName ?? "centaur",
    adapters: { telegram },
    state,
    // The answer is posted by streamAnswerToThread; no SDK placeholder.
    fallbackStreamingPlaceholderText: null,
    // Per-chat lock: a second message while a handler holds the lock throws
    // a LockError. In polling mode the adapter's checkpoint retries that
    // update; handlers only hold the lock for the create/append handoff.
    concurrency: "drop",
    logger,
  });

  const ingress: Ingress = { adapter: telegram, history, options, state };

  chat.onNewMention(async (thread, message) => {
    await handleTelegramMessage(thread, message, ingress);
  });

  if (history) {
    // With privacy mode off every group message arrives; ones that address
    // nobody are only kept as context for the next trigger.
    chat.onNewMessage(/(?:)/, async (thread, message) => {
      await rememberTelegramMessage(thread.id, message, ingress);
    });
  }

  chat.onSlashCommand(async (event) => {
    await handleTelegramCommand(chat, event, ingress);
  });

  const app = new Hono();
  app.get("/health", (c) => {
    const transport = mode === "polling" ? telegram.isPolling : true;
    return c.json(
      { ok: transport, service: "telegrambot", mode, transport },
      transport ? 200 : 503,
    );
  });
  if (mode === "webhook") {
    // Secret-token verification happens inside the adapter before the body
    // is parsed (401 on mismatch); update_id claims dedupe redeliveries.
    app.post(TELEGRAM_WEBHOOK_PATH, (c) =>
      telegram.handleWebhook(c.req.raw, { waitUntil: backgroundWaitUntil }),
    );
  }

  if (options.recoverRenderObligationsOnStart !== false) {
    scheduleRenderObligationRecovery(chat, state, options);
  }

  return { adapter: telegram, app, chat };
}

type Ingress = {
  adapter: TelegramAdapter;
  /** Present only with observeGroups. */
  history?: ThreadHistoryCache;
  options: TelegrambotOptions;
  state: StateAdapter;
};

/** Keep an allowlisted chat's message as group context (observeGroups). */
async function rememberTelegramMessage(
  threadId: string,
  message: ChatMessage,
  ingress: Ingress,
): Promise<void> {
  const { adapter, history, options } = ingress;
  if (!history) return;
  const raw = message.raw as TelegramPolicyMessage;
  if (!isStorableTelegramMessage(raw, options, adapter.botUserId)) return;
  try {
    await history.append(threadId, message);
  } catch (error) {
    // Context is best effort; never fail the update over it.
    (options.logger ?? noopLogger).warn("telegrambot_history_append_failed", {
      error: errorMessage(error),
      thread_id: threadId,
    });
  }
}

async function handleTelegramMessage(
  thread: Thread<TelegrambotThreadState>,
  message: ChatMessage,
  ingress: Ingress,
): Promise<void> {
  const { adapter, options } = ingress;
  const logger = options.logger ?? noopLogger;
  const raw = message.raw as TelegramPolicyMessage;
  const botUserId = adapter.botUserId;
  await rememberTelegramMessage(thread.id, message, ingress);
  if (!isAllowedTelegramMessage(raw, options, botUserId, logger)) return;
  const trigger = messageTrigger(
    raw,
    botUserId,
    options.observeGroups ? { botUserName: adapter.userName } : undefined,
  );
  if (!trigger) {
    logger.info("telegrambot_message_ignored_not_triggered", {
      chat_id: String(raw.chat.id),
      message_id: raw.message_id,
    });
    return;
  }
  await syncThreadMessageToSession(thread, message, {
    ...ingress,
    conversationName: conversationName(raw),
    text: withReplyQuote(message.text, raw, botUserId),
    trigger,
  });
}

async function handleTelegramCommand(
  chat: Chat<Record<string, Adapter>, TelegrambotThreadState>,
  event: SlashCommandEvent<TelegrambotThreadState>,
  ingress: Ingress,
): Promise<void> {
  const { adapter, options, state } = ingress;
  const logger = options.logger ?? noopLogger;
  const raw = event.raw as TelegramPolicyMessage;
  const botUserId = adapter.botUserId;
  const threadId = event.channel.id;
  const message = adapter.parseMessage(raw);
  await rememberTelegramMessage(threadId, message, ingress);
  if (!isAllowedTelegramMessage(raw, options, botUserId, logger)) return;

  const route = routeCommand(raw, event.command);
  if (route === "ignore") {
    logger.info("telegrambot_command_ignored_unaddressed", {
      chat_id: String(raw.chat.id),
      command: event.command,
      message_id: raw.message_id,
    });
    return;
  }

  const text = withReplyQuote(event.text, raw, botUserId);
  if (route === "help" || !text.trim()) {
    await adapter
      .postMessage(threadId, { markdown: USAGE_TEXT }, message.id)
      .catch((error) => {
        logger.warn("telegrambot_usage_post_failed", {
          error: errorMessage(error),
          thread_id: threadId,
        });
      });
    return;
  }

  // Chat SDK takes its per-chat lock for messages but not for slash commands;
  // take the same lock so a command and a message cannot interleave on one
  // chat's thread state.
  const lockKey = adapter.channelIdFromThreadId(threadId);
  const lock = await state.acquireLock(lockKey, COMMAND_LOCK_TTL_MS);
  if (!lock) {
    logger.warn("telegrambot_command_lock_busy", {
      message_id: message.id,
      thread_id: threadId,
    });
    throw new Error(`telegrambot: chat ${lockKey} is busy; retry the command`);
  }
  try {
    await syncThreadMessageToSession(chat.thread(threadId), message, {
      ...ingress,
      conversationName: conversationName(raw),
      text,
      trigger: "command",
    });
  } finally {
    await state.releaseLock(lock);
  }
}

function createDefaultState(
  options: TelegrambotOptions,
  logger: Logger,
): StateAdapter {
  const stateLogger = logger.child("postgres-state");
  // Own the pool so idle-client errors (Postgres restart, network blips at
  // startup) are logged instead of crashing the process.
  const pool = new pg.Pool({ connectionString: options.postgresUrl });
  pool.on("error", (error) => {
    stateLogger.warn("postgres pool error", { error: errorMessage(error) });
  });
  return createPostgresState({
    client: pool,
    keyPrefix: options.stateKeyPrefix ?? "centaur-telegrambot",
    logger: stateLogger,
  });
}

/**
 * Blocks until the state backend accepts a connection, retrying with
 * exponential backoff, so a startup race with the pod network does not wedge
 * render recovery.
 */
async function ensureStateConnected(
  state: StateAdapter,
  options: TelegrambotOptions,
): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      await state.connect();
      if (attempt > 0) {
        traceLog(options, "telegrambot_postgres_connected", undefined, {
          attempts: attempt + 1,
        });
      }
      return;
    } catch (error) {
      const delayMs = Math.min(
        POSTGRES_CONNECT_INITIAL_DELAY_MS * 2 ** attempt,
        POSTGRES_CONNECT_MAX_DELAY_MS,
      );
      traceLog(options, "telegrambot_postgres_connect_retry", undefined, {
        attempt: attempt + 1,
        delay_ms: delayMs,
        error: errorMessage(error),
      });
      await sleep(delayMs);
    }
  }
}

/**
 * Persists a triggering Telegram message into the session API and, unless a
 * run is already active in the chat, starts an execution. The create/append
 * handoff completes before the handler returns; execution and SSE rendering
 * continue in the background.
 */
async function syncThreadMessageToSession(
  thread: Thread<TelegrambotThreadState>,
  message: ChatMessage,
  input: Ingress & {
    conversationName?: string;
    text: string;
    trigger: TelegramTrigger;
  },
): Promise<void> {
  const { options } = input;
  const logger = options.logger ?? noopLogger;
  const threadState = (await thread.state) ?? {};
  const forwardedMessageIds = new Set(threadState.forwardedMessageIds ?? []);
  const executedMessageIds = new Set(threadState.executedMessageIds ?? []);
  const activeRun = hasLiveActiveExecution(
    threadState,
    options.activeExecutionTtlMs ?? ACTIVE_EXECUTION_TTL_MS,
  );
  const shouldStartExecution =
    !activeRun && !executedMessageIds.has(message.id);
  const trace: TelegrambotTrace = {
    messageId: message.id,
    mode: shouldStartExecution ? "execute" : "append",
    startedAtMs: nowMs(),
    threadId: thread.id,
  };
  // Duplicate delivery (webhook redelivery, polling replay after a crash):
  // already appended and nothing left to start.
  if (forwardedMessageIds.has(message.id) && !shouldStartExecution) {
    traceLog(options, "telegrambot_forward_duplicate_skipped", trace);
    return;
  }
  traceLog(options, "telegrambot_forward_started", trace, {
    active_execution: activeRun,
    trigger: input.trigger,
  });

  const serializeStartedAtMs = nowMs();
  const serializedMessage = await serializeMessage(message, {
    text: input.text,
    trigger: input.trigger,
  });
  traceLog(options, "telegrambot_forward_message_serialized", trace, {
    attachment_count: serializedMessage.attachments.length,
    phase_ms: elapsedMs(serializeStartedAtMs),
  });

  if (isContentlessApiMessage(serializedMessage)) {
    traceLog(options, "telegrambot_forward_contentless_skipped", trace);
    await reactToTelegramMessage(
      thread,
      message.id,
      REACTION_CONTENTLESS,
      logger,
    );
    return;
  }

  // Like slackbotv2's thread context: a turn started in a group carries what
  // the group said since the bot was last addressed.
  const contextMessages =
    shouldStartExecution && input.history && !thread.isDM
      ? await collectGroupContext(input.history, thread.id, message.id, {
          forwardedMessageIds,
          logger,
          maxMessages:
            options.groupContextMaxMessages ?? GROUP_CONTEXT_MAX_MESSAGES,
        })
      : [];
  if (contextMessages.length > 0) {
    traceLog(options, "telegrambot_forward_context_collected", trace, {
      message_count: contextMessages.length,
    });
  }

  let lastEventId = threadState.lastEventId ?? 0;
  const renderLease: { release: (() => Promise<void>) | null } = {
    release: null,
  };
  const messagesToAppend = [
    ...contextMessages,
    ...(forwardedMessageIds.has(message.id) ? [] : [serializedMessage]),
  ];
  const forwardInput: ForwardSessionInput = {
    afterEventId: lastEventId,
    conversationName: input.conversationName,
    executeMessage: shouldStartExecution ? serializedMessage : undefined,
    messages: messagesToAppend,
    onEventId: (eventId) => {
      lastEventId = Math.max(lastEventId, eventId);
    },
    threadId: thread.id,
    trace,
  };

  const commitMessagesAppended = async (): Promise<void> => {
    const latest = (await thread.state) ?? {};
    const latestMessageIds = new Set(latest.forwardedMessageIds ?? []);
    for (const item of messagesToAppend) latestMessageIds.add(item.id);
    // Write only the fields this commit owns: setState merges via
    // get-then-set, so echoing fields read earlier could resurrect values the
    // background render just cleared.
    await thread.setState({
      forwardedMessageIds: Array.from(latestMessageIds).slice(-1000),
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
    });
    traceLog(options, "telegrambot_forward_messages_committed", trace, {
      appended_message_count: messagesToAppend.length,
    });
  };

  const commitExecutionStarted = async (
    execution: TelegrambotExecuteSessionResponse,
  ): Promise<void> => {
    const latest = (await thread.state) ?? {};
    const latestExecutedMessageIds = new Set(latest.executedMessageIds ?? []);
    latestExecutedMessageIds.add(serializedMessage.id);
    // Take the render lease before the obligation becomes visible so a
    // concurrent recovery sweep never claims it mid-render.
    try {
      renderLease.release = await acquireRenderLease(input.state, thread.id);
    } catch (error) {
      traceLog(options, "telegrambot_render_lease_acquire_failed", trace, {
        error: errorMessage(error),
      });
    }
    await thread.setState({
      activeExecution: true,
      activeExecutionStartedAt: Date.now(),
      executedMessageIds: Array.from(latestExecutedMessageIds).slice(-1000),
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
      renderObligation: {
        afterEventId: lastEventId,
        executionId: execution.execution_id,
        message: serializedMessage,
      },
    });
    await indexRenderObligation(input.state, thread.id, options, trace);
    traceLog(options, "telegrambot_forward_execution_committed", trace, {
      execution_id: execution.execution_id,
    });
  };

  if (!shouldStartExecution) {
    // A run is already active in this chat: the message joins the session as
    // context for the next turn and is marked as noted.
    await withTransientSessionApiRetry(
      () =>
        forwardToSessionApi(options, forwardInput, {
          onMessagesAppended: commitMessagesAppended,
        }),
      options,
      trace,
    );
    if (activeRun) {
      await reactToTelegramMessage(thread, message.id, REACTION_NOTED, logger);
    }
    traceLog(options, "telegrambot_forward_complete", trace);
    return;
  }

  try {
    await thread.setState({
      activeExecution: true,
      activeExecutionStartedAt: Date.now(),
    });
    // Create + append only (fast). The execute call blocks on cold sandbox
    // spin-up, so it runs inside the render stream after the 👀 reaction.
    // executeSession is idempotent (idempotency_key = message id).
    await withTransientSessionApiRetry(
      () =>
        forwardToSessionApi(options, forwardInput, {
          onMessagesAppended: commitMessagesAppended,
        }),
      options,
      trace,
    );
  } catch (error) {
    const latest = (await thread.state) ?? {};
    await thread.setState({
      activeExecution: false,
      activeExecutionStartedAt: null,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
    });
    if (isRetryableSessionApiError(error)) {
      // Nothing was executed and the message is not marked executed: let the
      // transport redeliver (polling checkpoint retry) instead of answering.
      traceLog(options, "telegrambot_forward_deferred", trace, {
        error: errorMessage(error),
      });
      throw error;
    }
    traceLog(options, "telegrambot_forward_failed", trace, {
      error: errorMessage(error),
    });
    await renderExecutionStream(
      thread,
      streamError(error),
      serializedMessage,
      options,
      trace,
    );
    return;
  }

  scheduleExecutionRender(
    thread,
    serializedMessage,
    options,
    forwardInput,
    () => lastEventId,
    renderLease,
    trace,
    commitExecutionStarted,
  );
  traceLog(options, "telegrambot_forward_complete", trace, {
    last_event_id: lastEventId,
  });
}

function scheduleExecutionRender(
  thread: Thread<TelegrambotThreadState>,
  message: TelegrambotApiMessage,
  options: TelegrambotOptions,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  renderLease: { release: (() => Promise<void>) | null },
  trace?: TelegrambotTrace,
  onExecutionStarted?: (
    execution: TelegrambotExecuteSessionResponse,
  ) => Promise<void>,
): void {
  const promise = (async () => {
    try {
      let attempt = 0;
      while (true) {
        const result = await renderExecutionAttempt(
          thread,
          message,
          options,
          input,
          getLastEventId,
          trace,
          onExecutionStarted,
        );
        if (result === "complete") return;
        if (attempt >= RENDER_RETRY_MAX_ATTEMPTS) {
          traceLog(options, "telegrambot_render_retries_exhausted", trace, {
            retry_attempts: attempt,
          });
          const latest = (await thread.state) ?? {};
          await thread.setState({
            activeExecution: false,
            activeExecutionStartedAt: null,
            lastEventId: Math.max(latest.lastEventId ?? 0, getLastEventId()),
          });
          await renderExecutionStream(
            thread,
            streamError(
              new Error(
                "Streaming retries exhausted; giving up on rendering this run.",
              ),
            ),
            message,
            options,
            trace,
          ).catch(() => undefined);
          return;
        }
        const delayMs = renderRetryDelayMs(attempt);
        attempt += 1;
        traceLog(options, "telegrambot_render_retry_scheduled", trace, {
          retry_delay_ms: delayMs,
          retry_attempt: attempt,
        });
        await sleep(delayMs);
      }
    } finally {
      // Hand the obligation back to the recovery sweep's jurisdiction.
      await renderLease.release?.();
    }
  })();
  backgroundWaitUntil(promise);
}

/**
 * A persisted `activeExecution` flag counts only while its timestamp is within
 * the TTL; flags without a timestamp are stale by definition.
 */
export function hasLiveActiveExecution(
  state: Pick<
    TelegrambotThreadState,
    "activeExecution" | "activeExecutionStartedAt"
  >,
  ttlMs: number,
  nowEpochMs = Date.now(),
): boolean {
  if (state.activeExecution !== true) return false;
  if (typeof state.activeExecutionStartedAt !== "number") return false;
  return nowEpochMs - state.activeExecutionStartedAt <= ttlMs;
}

async function withTransientSessionApiRetry<T>(
  operation: () => Promise<T>,
  options: TelegrambotOptions,
  trace?: TelegrambotTrace,
): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await operation();
    } catch (error) {
      const delayMs = FORWARD_RETRY_DELAYS_MS[attempt];
      if (delayMs === undefined || !isRetryableSessionApiError(error)) {
        throw error;
      }
      traceLog(options, "telegrambot_forward_transient_retry", trace, {
        attempt: attempt + 1,
        delay_ms: delayMs,
        error: errorMessage(error),
      });
      await sleep(delayMs);
    }
  }
}

async function renderExecutionAttempt(
  thread: Thread<TelegrambotThreadState>,
  message: TelegrambotApiMessage,
  options: TelegrambotOptions,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  trace?: TelegrambotTrace,
  onExecutionStarted?: (
    execution: TelegrambotExecuteSessionResponse,
  ) => Promise<void>,
): Promise<"complete" | "retry"> {
  let rendered = false;
  let retry = false;
  try {
    await renderExecutionStream(
      thread,
      streamSessionAfterHandoff(options, input, onExecutionStarted),
      message,
      options,
      trace,
    );
    rendered = true;
    traceLog(options, "telegrambot_render_complete", trace);
    return "complete";
  } catch (error) {
    if (isRetryableSessionApiError(error)) {
      retry = true;
      traceLog(options, "telegrambot_render_deferred", trace, {
        error: errorMessage(error),
        last_event_id: getLastEventId(),
      });
      return "retry";
    }
    traceLog(options, "telegrambot_render_failed", trace, {
      error: errorMessage(error),
    });
    throw error;
  } finally {
    const latest = (await thread.state) ?? {};
    await thread.setState({
      activeExecution: retry,
      activeExecutionStartedAt: retry ? Date.now() : null,
      lastEventId: Math.max(latest.lastEventId ?? 0, getLastEventId()),
      ...(rendered ? { renderObligation: null } : {}),
    });
    traceLog(options, "telegrambot_render_finalized", trace, {
      obligation_cleared: rendered,
      retry_scheduled: retry,
      last_event_id: getLastEventId(),
    });
  }
}

function scheduleRenderObligationRecovery(
  chat: Chat<Record<string, Adapter>, TelegrambotThreadState>,
  state: StateAdapter,
  options: TelegrambotOptions,
): void {
  backgroundWaitUntil(recoverRenderObligationsWithRetry(chat, state, options));
}

async function recoverRenderObligationsWithRetry(
  chat: Chat<Record<string, Adapter>, TelegrambotThreadState>,
  state: StateAdapter,
  options: TelegrambotOptions,
): Promise<void> {
  await ensureStateConnected(state, options);
  let attempt = 0;
  while (true) {
    try {
      const deferredCount = await recoverRenderObligations(
        chat,
        state,
        options,
      );
      if (deferredCount === 0) return;
      const delayMs = renderRetryDelayMs(attempt);
      attempt += 1;
      traceLog(
        options,
        "telegrambot_render_recovery_retry_scheduled",
        undefined,
        {
          deferred_count: deferredCount,
          retry_delay_ms: delayMs,
          retry_attempt: attempt,
        },
      );
      await sleep(delayMs);
    } catch (error) {
      traceLog(options, "telegrambot_render_recovery_failed", undefined, {
        error: errorMessage(error),
      });
      return;
    }
  }
}

/** Exported for tests: one recovery sweep; returns the deferred count. */
export async function recoverRenderObligations(
  chat: Chat<Record<string, Adapter>, TelegrambotThreadState>,
  state: StateAdapter,
  options: TelegrambotOptions,
): Promise<number> {
  const startedAtMs = nowMs();
  await chat.initialize();
  const indexedThreadIds = await state.getList<string>(
    RENDER_OBLIGATION_INDEX_KEY,
  );
  const threadIds = Array.from(new Set(indexedThreadIds));
  let deferredCount = 0;
  traceLog(options, "telegrambot_render_recovery_scan", undefined, {
    obligation_count: threadIds.length,
    phase_ms: elapsedMs(startedAtMs),
  });

  for (const threadId of threadIds) {
    try {
      const thread = chat.thread(threadId);
      const obligation = (await thread.state)?.renderObligation;
      if (!obligation) continue;

      const leaseToken = randomUUID();
      const leaseAcquired = await state.setIfNotExists(
        renderRecoveryLeaseKey(threadId),
        leaseToken,
        RENDER_RECOVERY_LEASE_TTL_MS,
      );
      if (!leaseAcquired) {
        traceLog(
          options,
          "telegrambot_render_recovery_lease_skipped",
          undefined,
          { thread_id: threadId },
        );
        continue;
      }

      try {
        // Re-read under the lease: another worker may have completed or
        // replaced the obligation since the unleased read above.
        const leasedObligation = (await thread.state)?.renderObligation;
        if (!leasedObligation) {
          traceLog(
            options,
            "telegrambot_render_recovery_obligation_gone",
            undefined,
            { thread_id: threadId },
          );
          continue;
        }
        if (
          await recoverRenderObligation(
            chat,
            options,
            threadId,
            leasedObligation,
          )
        ) {
          deferredCount += 1;
        }
      } finally {
        const activeLeaseToken = await state.get<string>(
          renderRecoveryLeaseKey(threadId),
        );
        if (activeLeaseToken === leaseToken) {
          await state.delete(renderRecoveryLeaseKey(threadId));
        }
      }
    } catch (error) {
      // One thread's corrupt state or failed render must not abort the scan.
      deferredCount += 1;
      traceLog(
        options,
        "telegrambot_render_recovery_thread_failed",
        undefined,
        {
          error: errorMessage(error),
          thread_id: threadId,
        },
      );
    }
  }
  return deferredCount;
}

async function recoverRenderObligation(
  chat: Chat<Record<string, Adapter>, TelegrambotThreadState>,
  options: TelegrambotOptions,
  threadId: string,
  obligation: TelegrambotRenderObligation,
): Promise<boolean> {
  const trace: TelegrambotTrace = {
    messageId: obligation.message.id,
    mode: "execute",
    startedAtMs: nowMs(),
    threadId,
  };
  const thread = chat.thread(threadId);
  const threadState = (await thread.state) ?? {};
  let lastEventId = Math.max(
    threadState.lastEventId ?? 0,
    obligation.afterEventId,
  );
  const input: ForwardSessionInput = {
    afterEventId: lastEventId,
    executionId: obligation.executionId,
    messages: [],
    onEventId: (eventId) => {
      lastEventId = Math.max(lastEventId, eventId);
    },
    threadId,
    trace,
  };

  let openedStream: AsyncIterable<TelegrambotRendererSource>;
  try {
    openedStream = await openSessionEventStream(options, input);
  } catch (error) {
    const retryable = isRetryableSessionApiError(error);
    traceLog(options, "telegrambot_render_recovery_deferred", trace, {
      error: errorMessage(error),
      last_event_id: lastEventId,
      retryable,
    });
    if (retryable) return true;
    await renderExecutionStream(
      thread,
      streamError(error),
      obligation.message,
      options,
      trace,
    );
    await thread.setState({
      activeExecution: false,
      activeExecutionStartedAt: null,
      lastEventId,
      renderObligation: null,
    });
    return false;
  }

  let rendered = false;
  try {
    await thread.setState({
      activeExecution: true,
      activeExecutionStartedAt: Date.now(),
      lastEventId,
    });
    await renderExecutionStream(
      thread,
      streamOpenedSession(input, openedStream),
      obligation.message,
      options,
      trace,
    );
    rendered = true;
    traceLog(options, "telegrambot_render_recovery_complete", trace);
  } catch (error) {
    traceLog(options, "telegrambot_render_recovery_render_failed", trace, {
      error: errorMessage(error),
    });
    throw error;
  } finally {
    const latest = (await thread.state) ?? {};
    await thread.setState({
      activeExecution: false,
      activeExecutionStartedAt: null,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
      ...(rendered ? { renderObligation: null } : {}),
    });
    traceLog(options, "telegrambot_render_recovery_finalized", trace, {
      obligation_cleared: rendered,
      last_event_id: lastEventId,
    });
  }
  return false;
}

async function indexRenderObligation(
  state: StateAdapter,
  threadId: string,
  options: TelegrambotOptions,
  trace?: TelegrambotTrace,
): Promise<void> {
  await state.appendToList(RENDER_OBLIGATION_INDEX_KEY, threadId, {
    maxLength: RENDER_OBLIGATION_INDEX_MAX_LENGTH,
    ttlMs: RENDER_INDEX_TTL_MS,
  });
  traceLog(options, "telegrambot_render_obligation_indexed", trace);
}

async function* streamOpenedSession(
  input: Pick<ForwardSessionInput, "threadId">,
  stream: AsyncIterable<TelegrambotRendererSource>,
): AsyncIterable<TelegrambotRendererSource> {
  // The synthetic starting item primes the mapper's task state so answer
  // deltas stream immediately.
  yield startingStreamNotification(input.threadId);
  for await (const event of stream) yield event;
}

function renderRecoveryLeaseKey(threadId: string): string {
  return `telegrambot:render:lease:${threadId}`;
}

/**
 * Holds the per-thread render lease during a live render so the recovery
 * sweep cannot claim the just-indexed obligation and post a duplicate answer.
 * The TTL keeps this crash-safe; the lease is refreshed while the render runs.
 */
async function acquireRenderLease(
  state: StateAdapter,
  threadId: string,
): Promise<() => Promise<void>> {
  const key = renderRecoveryLeaseKey(threadId);
  const token = randomUUID();
  await state.set(key, token, RENDER_RECOVERY_LEASE_TTL_MS);
  const refresh = setInterval(() => {
    void state
      .get<string>(key)
      .then((current) =>
        current === token
          ? state.set(key, token, RENDER_RECOVERY_LEASE_TTL_MS)
          : undefined,
      )
      .catch(() => undefined);
  }, RENDER_LEASE_REFRESH_INTERVAL_MS);
  return async () => {
    clearInterval(refresh);
    try {
      const current = await state.get<string>(key);
      if (current === token) await state.delete(key);
    } catch {
      // Best effort: TTL expiry is the backstop.
    }
  };
}

async function renderExecutionStream(
  thread: Thread,
  stream: AsyncIterable<TelegrambotRendererSource>,
  message: TelegrambotApiMessage,
  options: TelegrambotOptions,
  trace?: TelegrambotTrace,
): Promise<void> {
  const logger = options.logger ?? noopLogger;
  const indicator = TelegramRunIndicator.start(thread, message.id, logger);
  const stopTyping = startTypingKeepalive(thread, logger);
  try {
    await renderAnswerStream(thread, stream, options, indicator, message.id);
    await indicator.finish("done");
    traceLog(options, "telegrambot_render_settled", trace);
  } catch (error) {
    await indicator.finish(
      isRetryableSessionApiError(error) ? "retrying" : "failed",
    );
    throw error;
  } finally {
    stopTyping();
  }
}

/**
 * Consumes the renderer's chunk stream: answer text streams into lazily
 * created message(s); everything else only feeds the run indicator.
 */
async function renderAnswerStream(
  thread: Thread,
  stream: AsyncIterable<TelegrambotRendererSource>,
  options: TelegrambotOptions,
  indicator: TelegramRunIndicator,
  replyToMessageId: string,
): Promise<void> {
  const answerText = new AsyncTextQueue();
  let answerPost: Promise<unknown> | null = null;
  let sourceFailed = false;
  try {
    for await (const chunk of harnessToChatSdkStream(
      stream,
      rendererOptions(options),
    )) {
      if (chunk.type === "markdown_text") {
        if (!answerPost) {
          answerPost = streamAnswerToThread(thread, answerText, options, {
            replyToMessageId,
          });
          // Swallow until the finally awaits it, so an early failure is not
          // an unhandled rejection while this loop keeps consuming.
          answerPost.catch(() => undefined);
        }
        answerText.push(chunk.text);
        continue;
      }
      indicator.update(chunk);
    }
  } catch (error) {
    sourceFailed = true;
    throw error;
  } finally {
    answerText.end();
    if (answerPost) {
      if (sourceFailed) await answerPost.catch(() => undefined);
      else await answerPost;
    }
  }
}

/**
 * Streams answer text across as many Telegram messages as needed, each at
 * most ANSWER_MESSAGE_MAX_CHARS of markdown (the adapter renders MarkdownV2
 * and falls back to plain text when Telegram rejects the entities). The
 * in-progress message is created on the first visible text and edited no more
 * often than the per-chat flood limit allows. In a group the first message
 * replies to the trigger, so users continue by replying to it. Past
 * ANSWER_MAX_FULL_MESSAGES the remainder collapses into one honestly truncated
 * message. A failed final flush does not fail the run. Exported for tests.
 */
export async function streamAnswerToThread(
  thread: Thread,
  source: AsyncIterable<string>,
  options: TelegrambotOptions,
  input: { replyToMessageId?: string } = {},
): Promise<void> {
  const logger = options.logger ?? noopLogger;
  const isDM = thread.adapter.isDM?.(thread.id) ?? false;
  const editIntervalMs = Math.max(
    options.answerEditIntervalMs ??
      (isDM ? ANSWER_EDIT_INTERVAL_PRIVATE_MS : ANSWER_EDIT_INTERVAL_GROUP_MS),
    ANSWER_EDIT_INTERVAL_MIN_MS,
  );
  const replyTo = isDM ? undefined : input.replyToMessageId;
  let pending = "";
  let current: { id: string; threadId: string } | null = null;
  let lastEditedContent = "";
  let lastEditAtMs = 0;
  let lastPostFailedAtMs: number | null = null;
  let postedCount = 0;

  const postNew = async (content: string): Promise<void> => {
    const target = postedCount === 0 ? replyTo : undefined;
    const raw =
      target && thread.adapter.reply
        ? await thread.adapter.reply(thread.id, target, { markdown: content })
        : await thread.adapter.postMessage(thread.id, { markdown: content });
    current = { id: raw.id, threadId: raw.threadId || thread.id };
    lastEditedContent = content;
    lastEditAtMs = nowMs();
    postedCount += 1;
  };

  const editCurrent = async (content: string): Promise<void> => {
    if (!current || content === lastEditedContent) return;
    await thread.adapter.editMessage(current.threadId, current.id, {
      markdown: content,
    });
    lastEditedContent = content;
  };

  const finalizeMessage = async (content: string): Promise<void> => {
    if (current) {
      await editCurrent(content);
    } else if (hasVisibleText(content)) {
      await postNew(content);
    }
    current = null;
    lastEditedContent = "";
  };

  const overflowed = (): boolean => postedCount >= ANSWER_MAX_FULL_MESSAGES;
  const pendingView = (): string =>
    overflowed()
      ? truncateWithNotice(pending, ANSWER_MESSAGE_MAX_CHARS, "final answer")
      : pending;

  for await (const piece of source) {
    pending += piece;
    while (!overflowed()) {
      const split = takeMessageChunk(pending, ANSWER_MESSAGE_MAX_CHARS);
      if (!split) break;
      await finalizeMessage(split.chunk);
      pending = split.rest;
    }
    const view = pendingView();
    // A streamed prefix such as `##` is non-blank markdown that renders to
    // no text, which Telegram rejects (RICH_MESSAGE_EMPTY); wait for more.
    if (!hasVisibleText(view)) continue;
    if (!current) {
      if (
        lastPostFailedAtMs !== null &&
        nowMs() - lastPostFailedAtMs < editIntervalMs
      ) {
        continue;
      }
      try {
        await postNew(view);
        lastPostFailedAtMs = null;
      } catch (error) {
        // Like in-progress edits, a failed early post is retried (at the edit
        // cadence, then by the final flush) instead of failing the run.
        lastPostFailedAtMs = nowMs();
        logger.warn("telegrambot_answer_post_failed", {
          error: errorMessage(error),
        });
      }
    } else if (nowMs() - lastEditAtMs >= editIntervalMs) {
      lastEditAtMs = nowMs();
      try {
        await editCurrent(view);
      } catch (error) {
        // In-progress edits are cosmetic; the final flush retries the content.
        logger.warn("telegrambot_answer_edit_failed", {
          error: errorMessage(error),
        });
      }
    }
  }

  try {
    while (!overflowed()) {
      const split = takeMessageChunk(pending, ANSWER_MESSAGE_MAX_CHARS);
      if (!split) break;
      await finalizeMessage(split.chunk);
      pending = split.rest;
    }
    const view = pendingView();
    if (hasVisibleText(view)) await finalizeMessage(view);
  } catch (error) {
    logger.warn("telegrambot_answer_finalize_failed", {
      error: errorMessage(error),
      pending_chars: pending.length,
    });
    try {
      await thread.adapter.postMessage(thread.id, {
        raw: "⚠️ The end of this answer failed to post; the output above may be incomplete.",
      });
    } catch {
      // Best effort only — the run itself succeeded.
    }
  }
}

/**
 * The group's kept messages since the last one forwarded to the session,
 * oldest first, newest `maxMessages` only, text only. A failure degrades to
 * no context rather than failing the turn.
 */
async function collectGroupContext(
  history: ThreadHistoryCache,
  threadId: string,
  currentMessageId: string,
  input: {
    forwardedMessageIds: ReadonlySet<string>;
    logger: Logger;
    maxMessages: number;
  },
): Promise<TelegrambotApiMessage[]> {
  let kept: ChatMessage[];
  try {
    kept = await history.getMessages(threadId);
  } catch (error) {
    input.logger.warn("telegrambot_context_read_failed", {
      error: errorMessage(error),
      thread_id: threadId,
    });
    return [];
  }
  let start = 0;
  kept.forEach((item, index) => {
    if (input.forwardedMessageIds.has(item.id)) start = index + 1;
  });
  return kept
    .slice(start)
    .filter(
      (item) =>
        item.id !== currentMessageId &&
        !input.forwardedMessageIds.has(item.id) &&
        item.text.trim().length > 0,
    )
    .slice(-input.maxMessages)
    .map((item) => ({
      attachments: [],
      author: {
        fullName: item.author.fullName,
        isBot: item.author.isBot,
        isMe: item.author.isMe,
        userId: item.author.userId,
        userName: item.author.userName,
      },
      id: item.id,
      isMention: false,
      text: item.text.slice(0, GROUP_CONTEXT_MESSAGE_MAX_CHARS),
      threadId: item.threadId,
      timestamp: new Date(item.metadata.dateSent).toISOString(),
    }));
}

/** Whether markdown renders to any visible text once formatting is removed. */
function hasVisibleText(markdown: string): boolean {
  return markdownToPlainText(markdown).trim().length > 0;
}

async function* streamSessionAfterHandoff(
  options: TelegrambotOptions,
  input: ForwardSessionInput,
  onExecutionStarted?: (
    execution: TelegrambotExecuteSessionResponse,
  ) => Promise<void>,
): AsyncIterable<TelegrambotRendererSource> {
  // The 👀 reaction is already queued before this generator is consumed, so
  // the user has feedback while the cold sandbox spins up. Execute runs here
  // so a sandbox-spawn failure surfaces in the same render.
  yield startingStreamNotification(input.threadId);

  if (input.executeMessage) {
    try {
      const execution = await executeSessionTurn(options, input);
      if (execution) {
        input.executionId = execution.execution_id;
        await onExecutionStarted?.(execution);
      }
    } catch (error) {
      traceLog(options, "telegrambot_execute_failed", input.trace, {
        error: errorMessage(error),
      });
      if (isRetryableSessionApiError(error)) throw error;
      yield sessionStreamError(error);
      return;
    }
  }

  let stream: AsyncIterable<TelegrambotRendererSource>;
  try {
    stream = await openSessionEventStream(options, input);
  } catch (error) {
    traceLog(options, "telegrambot_events_open_failed", input.trace, {
      error: errorMessage(error),
    });
    if (isRetryableSessionApiError(error)) throw error;
    yield sessionStreamError(error);
    return;
  }

  for await (const event of stream) yield event;
}

async function* streamError(
  error: unknown,
): AsyncIterable<TelegrambotRendererSource> {
  yield sessionStreamError(error);
}

function backgroundWaitUntil(promise: Promise<unknown>): void {
  // Long-lived process: background work just needs its rejections swallowed
  // after they are traced.
  void promise.catch(() => undefined);
}

function rendererOptions(
  options: TelegrambotOptions,
): CodexAppServerToChatStreamOptions {
  const mapper = options.mapper;
  return {
    ...mapper,
    // Answer text streams into its own messages, so there is no card to wait
    // for: stream deltas immediately (see discordbot).
    preStreamGraceMs: 0,
    async onRendererEvent(event: RendererEvent) {
      await mapper?.onRendererEvent?.(event);
    },
  };
}

function renderRetryDelayMs(attempt: number): number {
  return Math.min(
    RENDER_RETRY_INITIAL_DELAY_MS * 2 ** attempt,
    RENDER_RETRY_MAX_DELAY_MS,
  );
}

/**
 * Telegram's typing action expires after ~5s; re-fire on an interval while
 * the run renders. Errors are swallowed (typing is cosmetic).
 */
function startTypingKeepalive(thread: Thread, logger: Logger): () => void {
  const fire = (): void => {
    void thread.adapter.startTyping(thread.id).catch((error: unknown) => {
      logger.debug("telegrambot_typing_error", { error: errorMessage(error) });
    });
  };
  fire();
  const interval = globalThis.setInterval(fire, TYPING_KEEPALIVE_MS);
  return () => globalThis.clearInterval(interval);
}
