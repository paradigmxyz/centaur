import { randomUUID } from "node:crypto";
import {
  codexAppServerToChatSdkStream,
  type ChatSDKStreamChunk,
  type CodexAppServerToChatStreamOptions,
  type RendererEvent,
} from "@centaur/rendering";
import { createDiscordAdapter } from "@chat-adapter/discord";
import { createPostgresState } from "@chat-adapter/state-pg";
import {
  Chat,
  type Adapter,
  type Logger,
  type Message as ChatMessage,
  type StateAdapter,
  type Thread,
} from "chat";
import { Hono } from "hono";
import pg from "pg";
import {
  isAllowedDiscordGuild,
  isAllowedDiscordMessage,
  isGuildAllowlistEmpty,
  parseDiscordThreadKey,
  resolveTriggerBotAllowlist,
} from "./discord-allowlist";
import { DiscordNarrator, reactToDiscordMessage } from "./discord-narrator";
import { fetchThreadStarterMessage } from "./discord-starter";
import {
  deriveThreadName,
  isThreadCreatedForMessage,
  renameThreadFromMessage,
} from "./discord-threading";
import { setGatewayConnected } from "./gateway";
import {
  collectInitialContext,
  executeSessionTurn,
  forwardToSessionApi,
  isContentlessApiMessage,
  isDiscordPermissionError,
  isRetryableSessionApiError,
  openSessionEventStream,
  serializeMessage,
  sessionStreamError,
  startingStreamNotification,
} from "./session-api";
import type {
  Discordbot,
  DiscordbotApiMessage,
  DiscordbotExecuteSessionResponse,
  DiscordbotMessageMode,
  DiscordbotOptions,
  DiscordbotRenderObligation,
  DiscordbotRendererSource,
  DiscordbotThreadState,
  DiscordbotTrace,
  ForwardSessionInput,
  TypingCapableAdapter,
} from "./types";
import {
  AsyncTextQueue,
  elapsedMs,
  errorMessage,
  GuildExecutionLimiter,
  noopLogger,
  nowMs,
  sliceSurrogateSafe,
  takeDiscordMessageChunk,
  traceLog,
} from "./utils";

export type {
  Discordbot,
  DiscordbotApiAttachment,
  DiscordbotApiAuthor,
  DiscordbotApiMessage,
  DiscordbotAppendMessagesRequest,
  DiscordbotCreateSessionRequest,
  DiscordbotExecuteSessionRequest,
  DiscordbotExecuteSessionResponse,
  DiscordbotFetch,
  DiscordbotOptions,
  DiscordbotSessionMessage,
  DiscordbotSessionMessageRole,
} from "./types";

const TYPING_KEEPALIVE_MS = 8000;
const RENDER_OBLIGATION_INDEX_KEY = "discordbot:render:index";
const RENDER_OBLIGATION_INDEX_MAX_LENGTH = 2000;
const RENDER_INDEX_TTL_MS = 30 * 24 * 60 * 60 * 1000;
const RENDER_RECOVERY_LEASE_TTL_MS = 2 * 60 * 1000;
const RENDER_RETRY_INITIAL_DELAY_MS = 250;
const RENDER_RETRY_MAX_DELAY_MS = 5_000;
// Discord caps message content at 2000 chars; leave headroom so the honest
// "[truncated ...]" suffix lands instead of the adapter's silent "..." cut.
const DISCORD_FALLBACK_TEXT_MAX_CHARS = 1_900;
const POSTGRES_CONNECT_INITIAL_DELAY_MS = 250;
const POSTGRES_CONNECT_MAX_DELAY_MS = 10_000;
// Discord delta (no slackbotv2 analog): `activeExecution` persisted before the
// execution commit is only cleared by the render finally; a crash/SIGTERM in
// between would wedge the thread forever (Gateway ingress has no redelivery),
// so the flag is ignored once older than this TTL.
const ACTIVE_EXECUTION_TTL_MS = 30 * 60 * 1000;
// Discord delta (no slackbotv2 analog): cap on the render retry loop. Upstream
// retries forever — tolerable on Slack, where webhook redelivery bounds a
// stuck turn — but here an unbounded loop replays the render from the original
// afterEventId forever (duplicating posts and never settling 👀) whenever the
// error keeps classifying retryable, including a TypeError thrown by a
// programming bug. TypeError stays retryable for parity with upstream (fetch
// network failures surface as TypeErrors); the cap is what kills the loop.
const RENDER_RETRY_MAX_ATTEMPTS = 10;
// Discord delta (no slackbotv2 analog): bounded retry for the synchronous
// create/append phase — Slack relies on webhook redelivery (503) for transient
// failures there; the Gateway has no redelivery and the message is already
// dedupe-marked, so giving up immediately loses the turn permanently.
const FORWARD_RETRY_DELAYS_MS = [1_000, 3_000];
// Discord delta (no slackbotv2 analog): per-guild in-flight execution cap.
const DEFAULT_MAX_CONCURRENT_EXECUTIONS_PER_GUILD = 3;
// Discord delta: answer streaming across multiple messages (see
// streamAnswerToThread). Edits to the in-progress message are throttled to
// this cadence; everything past the full-message cap lands in one final
// honestly-truncated message.
const ANSWER_EDIT_INTERVAL_MS = 1_500;
const ANSWER_MESSAGE_MAX_CHARS = DISCORD_FALLBACK_TEXT_MAX_CHARS;
const ANSWER_MAX_FULL_MESSAGES = 10;

export function createDiscordbot(options: DiscordbotOptions): Discordbot {
  const userName = options.userName ?? "centaur";
  const logger = options.logger ?? noopLogger;

  if (isGuildAllowlistEmpty(options)) {
    logger.warn("discordbot_guild_allowlist_empty_inert", {
      hint: "Set DISCORDBOT_GUILD_ALLOWLIST; the bot ignores all messages until configured.",
    });
  }

  const discord = createDiscordAdapter({
    apiUrl: options.discordApiUrl,
    applicationId: options.applicationId,
    botToken: options.botToken,
    publicKey: options.publicKey,
    mentionRoleIds: options.mentionRoleIds,
    userName,
    logger,
    // Discord delta (patched adapter): gate mentions BEFORE the adapter
    // creates a public thread — a mention rejected by the fail-closed guild
    // allowlist must not mutate the server (no orphan threads in
    // non-allowlisted guilds). The full message gate (isAllowedDiscordMessage)
    // still runs in the handlers below.
    shouldHandleMention: ({ guildId }) =>
      isAllowedDiscordGuild(guildId, options),
    // Discord delta (patched adapter): the gateway drops bot-authored
    // messages by default; forward only allowlisted trigger bots in
    // allowlisted guilds. The allowlist entry must be the id the message is
    // authored as (bot user id, or the webhook id for webhook integrations);
    // isAllowedDiscordMessage applies the broader application_id/webhook_id
    // matching once the full payload is available.
    shouldForwardBotMessage: ({ authorId, guildId }) =>
      isAllowedDiscordGuild(guildId, options) &&
      resolveTriggerBotAllowlist(options).includes(authorId),
    // Discord delta (patched adapter): the Gateway never redelivers, so a
    // message dropped on a thread-lock conflict is otherwise lost with zero
    // signal — surface it with a 🔁 reaction so the user knows to resend.
    onMessageDropped: (info) => {
      logger.warn("discordbot_message_dropped_lock_conflict", {
        channel_id: info.channelId,
        guild_id: info.guildId,
        message_id: info.messageId,
        thread_id: info.threadId,
      });
      void reactToDiscordMessage(
        options,
        {
          emoji: "🔁",
          messageId: info.messageId,
          threadKey: `discord:${info.guildId}:${info.channelId}${
            info.threadId ? `:${info.threadId}` : ""
          }`,
        },
        logger,
      );
    },
    // Discord delta (patched adapter): track gateway liveness so /health goes
    // 503 once the connection has been down for >60s (see gateway.ts).
    onGatewayStatusChange: (connected) => setGatewayConnected(connected),
  });
  const state = options.state ?? createDefaultState(options, logger);
  const chat = new Chat<{ discord: typeof discord }, DiscordbotThreadState>({
    userName,
    adapters: { discord },
    state,
    // No SDK-level streaming placeholder: instant feedback is the 👀 reaction
    // the narrator puts on the triggering message, and the final answer must
    // land as a NEW message at the bottom of the timeline — with null, the
    // SDK's post+edit fallback only creates the answer message once the first
    // visible answer text arrives.
    fallbackStreamingPlaceholderText: null,
    // Serialize handlers per thread via the SDK's per-thread lock. The deprecated
    // `onLockConflict: 'force'` force-released the lock so two handlers ran concurrently on one
    // thread — two near-simultaneous mentions could both pass the `activeExecution` check and
    // double-execute. `'drop'` keeps the lock: a second message that lands while a handler holds the
    // thread lock is dropped rather than run in parallel. Same code path as before for the
    // no-contention case, so single-message streaming is unchanged.
    concurrency: "drop",
    logger,
  });

  // Discord delta (no slackbotv2 analog): per-guild in-flight execution cap;
  // the pod is a singleton by design, so an in-memory counter is authoritative.
  const executionLimiter = new GuildExecutionLimiter(
    options.maxConcurrentExecutionsPerGuild ??
      DEFAULT_MAX_CONCURRENT_EXECUTIONS_PER_GUILD,
  );

  chat.onNewMention(async (thread, message) => {
    if (!isAllowedDiscordMessage(message, options, logger)) return;
    await thread.subscribe();
    await syncThreadMessageToSession(thread, message, {
      executionLimiter,
      mode: "execute",
      options,
      state,
    });
  });

  chat.onSubscribedMessage(async (thread, message) => {
    if (!isAllowedDiscordMessage(message, options, logger)) return;
    await syncThreadMessageToSession(thread, message, {
      executionLimiter,
      mode: message.isMention === true ? "execute" : "append",
      options,
      state,
    });
  });

  const app = new Hono();
  app.get("/health", (c) => {
    const gatewayActive = options.isGatewayActive
      ? options.isGatewayActive()
      : true;
    return c.json(
      { ok: gatewayActive, service: "discordbot", gateway: gatewayActive },
      gatewayActive ? 200 : 503,
    );
  });

  if (options.recoverRenderObligationsOnStart !== false) {
    scheduleRenderObligationRecovery(chat, state, options);
  }

  return { app, chat, adapter: discord };
}

function createDefaultState(
  options: DiscordbotOptions,
  logger: Logger,
): StateAdapter {
  const stateLogger = logger.child("postgres-state");
  // Own the pool so we can attach an error handler. pg.Pool emits 'error' for
  // idle clients whose connection drops (Postgres restart, or a transient blip
  // while the pod's network is still being programmed at startup). With no
  // listener, node-postgres rethrows it as an uncaught exception and the process
  // crashes/spews. Logging and swallowing lets the pool reconnect on the next query.
  const pool = new pg.Pool({ connectionString: options.postgresUrl });
  pool.on("error", (error) => {
    stateLogger.warn("postgres pool error", { error: errorMessage(error) });
  });
  return createPostgresState({
    client: pool,
    keyPrefix: options.stateKeyPrefix ?? "centaur-discordbot",
    logger: stateLogger,
  });
}

/**
 * Blocks until the state backend accepts a connection, retrying with exponential
 * backoff. The first DB connection fires within milliseconds of process start and
 * can lose a race with the pod's network programming (a one-off ECONNREFUSED).
 * Retrying instead of throwing absorbs that race; the first successful connect
 * also flips the adapter's `connected` flag, so the message path comes alive too.
 */
async function ensureStateConnected(
  state: StateAdapter,
  options: DiscordbotOptions,
): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      await state.connect();
      if (attempt > 0) {
        traceLog(options, "discordbot_postgres_connected", undefined, {
          attempts: attempt + 1,
        });
      }
      return;
    } catch (error) {
      const delayMs = Math.min(
        POSTGRES_CONNECT_INITIAL_DELAY_MS * 2 ** attempt,
        POSTGRES_CONNECT_MAX_DELAY_MS,
      );
      traceLog(options, "discordbot_postgres_connect_retry", undefined, {
        attempt: attempt + 1,
        delay_ms: delayMs,
        error: errorMessage(error),
      });
      await sleep(delayMs);
    }
  }
}

/**
 * Persists a Discord thread update into the session API. In execute mode the create/append/execute
 * handoff completes before the handler returns; SSE rendering continues in background.
 */
async function syncThreadMessageToSession(
  thread: Thread<DiscordbotThreadState>,
  message: ChatMessage,
  input: {
    executionLimiter: GuildExecutionLimiter;
    mode: DiscordbotMessageMode;
    options: DiscordbotOptions;
    state: StateAdapter;
  },
): Promise<void> {
  const traceStartedAtMs = nowMs();
  const logger = input.options.logger ?? noopLogger;
  const state = (await thread.state) ?? {};
  const messageIds = new Set(state.forwardedMessageIds ?? []);
  const executedMessageIds = new Set(state.executedMessageIds ?? []);
  // Discord delta: `state.activeExecution !== true` upstream — a stale flag
  // (crash before the render finally cleared it) must not wedge the thread.
  let shouldStartExecution =
    input.mode === "execute" &&
    !hasLiveActiveExecution(
      state,
      input.options.activeExecutionTtlMs ?? ACTIVE_EXECUTION_TTL_MS,
    ) &&
    !executedMessageIds.has(message.id);
  // Discord delta (no slackbotv2 analog): per-guild in-flight execution cap.
  // On exceed the message is demoted to append-only context and gets a 🚦.
  let releaseExecutionSlot: (() => void) | null = null;
  const guildId = parseDiscordThreadKey(thread.id).guildId;
  if (shouldStartExecution && guildId) {
    releaseExecutionSlot = input.executionLimiter.acquire(guildId);
    if (!releaseExecutionSlot) {
      shouldStartExecution = false;
      traceLog(input.options, "discordbot_forward_guild_execution_capped", {
        includeContext: false,
        messageId: message.id,
        mode: input.mode,
        openStream: false,
        startedAtMs: traceStartedAtMs,
        threadId: thread.id,
      });
      await reactToDiscordMessage(
        input.options,
        { emoji: "🚦", messageId: message.id, threadKey: thread.id },
        logger,
      );
    }
  }
  const shouldIncludeContext =
    shouldStartExecution && state.historyForwarded !== true;
  const isDuplicateIncrementalMessage =
    messageIds.has(message.id) &&
    !shouldStartExecution &&
    !shouldIncludeContext;
  const trace: DiscordbotTrace = {
    includeContext: shouldIncludeContext,
    messageId: message.id,
    mode: input.mode,
    openStream: shouldStartExecution,
    startedAtMs: traceStartedAtMs,
    threadId: thread.id,
  };
  if (isDuplicateIncrementalMessage) {
    traceLog(input.options, "discordbot_forward_duplicate_skipped", trace);
    return;
  }
  traceLog(input.options, "discordbot_forward_started", trace, {
    active_execution: state.activeExecution === true,
    history_forwarded: state.historyForwarded === true,
  });

  const serializeStartedAtMs = nowMs();
  const serializedMessage = await serializeMessage(message);
  traceLog(input.options, "discordbot_forward_message_serialized", trace, {
    attachment_count: serializedMessage.attachments.length,
    phase_ms: elapsedMs(serializeStartedAtMs),
  });

  // Discord delta (no slackbotv2 analog): a sticker-only/forwarded/poll/system
  // mention serializes to empty text with no attachments; executing it would
  // fabricate a synthetic "continue" turn. React ❓ and skip instead.
  if (shouldStartExecution && isContentlessApiMessage(serializedMessage)) {
    releaseExecutionSlot?.();
    traceLog(input.options, "discordbot_forward_contentless_skipped", trace);
    await reactToDiscordMessage(
      input.options,
      { emoji: "❓", messageId: message.id, threadKey: thread.id },
      logger,
    );
    return;
  }

  let context: DiscordbotApiMessage[] | undefined;

  if (shouldIncludeContext && !state.historyForwarded) {
    const contextStartedAtMs = nowMs();
    try {
      context = await collectInitialContext(thread, message);
    } catch (error) {
      if (!isDiscordPermissionError(error)) throw error;
      // Discord delta (no slackbotv2 analog): a 403 here (missing Read Message
      // History / Missing Access) previously propagated with total user
      // silence. Best-effort tell the user, settle ❌, and stop cleanly.
      releaseExecutionSlot?.();
      traceLog(
        input.options,
        "discordbot_forward_context_permission_denied",
        trace,
        { error: errorMessage(error) },
      );
      await thread
        .post(
          "I can't read this channel's history — I'm missing permissions (Read Message History).",
        )
        .catch(() => undefined);
      await reactToDiscordMessage(
        input.options,
        { emoji: "❌", messageId: message.id, threadKey: thread.id },
        logger,
      );
      return;
    }
    // Discord delta: a thread created from a message keeps that starter message
    // in the parent channel, so thread history alone misses it (Slack's
    // conversations.replies includes the parent). Prefer the fetched starter
    // over any thread-starter stub already in the history.
    const starter = await fetchThreadStarterMessage(
      input.options,
      thread.id,
      input.options.logger ?? noopLogger,
    );
    if (starter) {
      context = [starter, ...context.filter((item) => item.id !== starter.id)];
    }
    traceLog(input.options, "discordbot_forward_context_collected", trace, {
      message_count: context.length,
      phase_ms: elapsedMs(contextStartedAtMs),
      starter_included: starter !== null,
    });
  } else {
    traceLog(input.options, "discordbot_forward_context_skipped", trace, {
      message_count: 1,
    });
  }

  let lastEventId = state.lastEventId ?? 0;
  const candidateMessages = context ?? [serializedMessage];
  const messagesToAppend = candidateMessages.filter(
    (item) => !messageIds.has(item.id),
  );

  const forwardInput: ForwardSessionInput = {
    afterEventId: lastEventId,
    executeMessage: shouldStartExecution ? serializedMessage : undefined,
    messages: messagesToAppend,
    onEventId: (eventId) => {
      lastEventId = Math.max(lastEventId, eventId);
    },
    openStream: false,
    threadId: thread.id,
    trace,
  };

  const commitMessagesAppended = async (): Promise<void> => {
    const latest = (await thread.state) ?? {};
    const latestMessageIds = new Set(latest.forwardedMessageIds ?? []);
    for (const item of messagesToAppend) latestMessageIds.add(item.id);
    // Discord delta: write ONLY the fields this commit owns — setState merges
    // via get-then-set, so echoing fields read earlier (activeExecution,
    // renderObligation) here can resurrect values the background render's
    // finally just cleared. lastEventId takes the max against the freshly-read
    // value so a concurrent stream's higher watermark is never regressed.
    await thread.setState({
      forwardedMessageIds: Array.from(latestMessageIds).slice(-1000),
      historyForwarded: latest.historyForwarded || shouldIncludeContext,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
    });
    traceLog(input.options, "discordbot_forward_messages_committed", trace, {
      appended_message_count: messagesToAppend.length,
      forwarded_message_count: Math.min(latestMessageIds.size, 1000),
    });
  };

  const commitExecutionStarted = async (
    execution: DiscordbotExecuteSessionResponse,
  ): Promise<void> => {
    const latest = (await thread.state) ?? {};
    const latestExecutedMessageIds = new Set(latest.executedMessageIds ?? []);
    latestExecutedMessageIds.add(serializedMessage.id);
    await thread.setState({
      activeExecution: true,
      // Discord delta: refresh the staleness timestamp where the flag is
      // legitimately re-confirmed (see ACTIVE_EXECUTION_TTL_MS).
      activeExecutionStartedAt: Date.now(),
      executedMessageIds: Array.from(latestExecutedMessageIds).slice(-1000),
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
      renderObligation: {
        afterEventId: lastEventId,
        executionId: execution.execution_id,
        message: serializedMessage,
      },
    });
    await indexRenderObligation(input.state, {
      options: input.options,
      threadId: thread.id,
      trace,
    });
    traceLog(input.options, "discordbot_forward_execution_committed", trace, {
      execution_id: execution.execution_id,
      executed_message_count: Math.min(latestExecutedMessageIds.size, 1000),
    });
  };

  if (!shouldStartExecution) {
    if (messagesToAppend.length > 0) {
      await forwardToSessionApi(input.options, forwardInput, {
        onMessagesAppended: commitMessagesAppended,
      });
    }
    traceLog(input.options, "discordbot_forward_complete", trace);
    return;
  }

  try {
    await thread.setState({
      activeExecution: true,
      // Discord delta: staleness timestamp, cleared together with the flag.
      activeExecutionStartedAt: Date.now(),
    });
    traceLog(
      input.options,
      "discordbot_forward_active_execution_marked",
      trace,
    );
    // Create + append the session message only (fast). The execute call blocks
    // ~9s on cold sandbox spin-up (incl. the tool-server sidecar), so it's run
    // inside the render stream below — after the 👀 working reaction lands —
    // instead of before it. executeSession is idempotent
    // (idempotency_key = message id), so a render retry won't re-spawn.
    // Discord delta (no slackbotv2 analog): bounded retry — Slack answers 503
    // so Slack redelivers the webhook on a transient failure here; the Gateway
    // has no redelivery and the message is already dedupe-marked, so without a
    // retry a blip permanently drops the turn.
    await withTransientSessionApiRetry(
      () =>
        forwardToSessionApi(
          input.options,
          { ...forwardInput, executeMessage: undefined, openStream: false },
          { onMessagesAppended: commitMessagesAppended },
        ),
      input.options,
      trace,
    );
    scheduleExecutionRender(
      thread,
      serializedMessage,
      input.options,
      forwardInput,
      () => lastEventId,
      shouldIncludeContext,
      trace,
      commitExecutionStarted,
      releaseExecutionSlot ?? undefined,
    );
    traceLog(input.options, "discordbot_forward_complete", trace, {
      last_event_id: lastEventId,
    });
  } catch (error) {
    // Discord delta: release the per-guild slot — the render that would have
    // released it was never scheduled.
    releaseExecutionSlot?.();
    const latest = (await thread.state) ?? {};
    await thread.setState({
      activeExecution: false,
      activeExecutionStartedAt: null,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
    });
    // Discord ingress arrives via the Gateway, so there is no webhook retry to request
    // (slackbotv2 answers 503 to make Slack re-deliver). Surface the failure in-thread instead.
    await renderExecutionStream(
      thread,
      streamError(error),
      serializedMessage,
      input.options,
      false,
      trace,
    );
    traceLog(input.options, "discordbot_forward_complete", trace, {
      latest_active_execution: latest.activeExecution === true,
      last_event_id: lastEventId,
    });
  }
}

function scheduleExecutionRender(
  thread: Thread<DiscordbotThreadState>,
  message: DiscordbotApiMessage,
  options: DiscordbotOptions,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  isInitialExecution: boolean,
  trace?: DiscordbotTrace,
  onExecutionStarted?: (
    execution: DiscordbotExecuteSessionResponse,
  ) => Promise<void>,
  onSettled?: () => void,
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
          isInitialExecution,
          trace,
          onExecutionStarted,
        );
        if (result === "complete") return;
        // Discord delta (no slackbotv2 analog): cap the retry loop — see
        // RENDER_RETRY_MAX_ATTEMPTS. On exhaustion, clear the active flag and
        // surface a failure (❌) instead of duplicating posts forever; the
        // persisted render obligation still lets the next restart retry.
        if (attempt >= RENDER_RETRY_MAX_ATTEMPTS) {
          traceLog(options, "discordbot_render_retries_exhausted", trace, {
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
            false,
            trace,
          ).catch(() => undefined);
          return;
        }
        const delayMs = renderRetryDelayMs(attempt);
        attempt += 1;
        traceLog(options, "discordbot_render_retry_scheduled", trace, {
          retry_delay_ms: delayMs,
          retry_attempt: attempt,
        });
        await sleep(delayMs);
      }
    } finally {
      // Discord delta: reliably release the per-guild execution slot.
      onSettled?.();
    }
  })();
  backgroundWaitUntil(promise);
}

/**
 * Discord delta (no slackbotv2 analog): treat a persisted `activeExecution`
 * flag as live only while its staleness timestamp is within the TTL. A crash
 * between marking the flag and the render finally clearing it would otherwise
 * block `shouldStartExecution` forever (Gateway ingress has no redelivery).
 * Flags without a timestamp (written by older code, so necessarily from
 * before the last restart) count as stale.
 */
export function hasLiveActiveExecution(
  state: Pick<
    DiscordbotThreadState,
    "activeExecution" | "activeExecutionStartedAt"
  >,
  ttlMs: number,
  nowEpochMs = Date.now(),
): boolean {
  if (state.activeExecution !== true) return false;
  if (typeof state.activeExecutionStartedAt !== "number") return false;
  return nowEpochMs - state.activeExecutionStartedAt <= ttlMs;
}

/**
 * Discord delta (no slackbotv2 analog): bounded retry for the synchronous
 * create/append phase. Only retryable-classified failures are retried, with
 * the FORWARD_RETRY_DELAYS_MS backoff (3 attempts total).
 */
async function withTransientSessionApiRetry<T>(
  operation: () => Promise<T>,
  options: DiscordbotOptions,
  trace?: DiscordbotTrace,
): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await operation();
    } catch (error) {
      const delayMs = FORWARD_RETRY_DELAYS_MS[attempt];
      if (delayMs === undefined || !isRetryableSessionApiError(error)) {
        throw error;
      }
      traceLog(options, "discordbot_forward_transient_retry", trace, {
        attempt: attempt + 1,
        delay_ms: delayMs,
        error: errorMessage(error),
      });
      await sleep(delayMs);
    }
  }
}

async function renderExecutionAttempt(
  thread: Thread<DiscordbotThreadState>,
  message: DiscordbotApiMessage,
  options: DiscordbotOptions,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  isInitialExecution: boolean,
  trace?: DiscordbotTrace,
  onExecutionStarted?: (
    execution: DiscordbotExecuteSessionResponse,
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
      isInitialExecution,
      trace,
    );
    rendered = true;
    traceLog(options, "discordbot_render_complete", trace);
    return "complete";
  } catch (error) {
    if (isRetryableSessionApiError(error)) {
      retry = true;
      traceLog(options, "discordbot_render_deferred", trace, {
        error: errorMessage(error),
        last_event_id: getLastEventId(),
      });
      return "retry";
    }
    traceLog(options, "discordbot_render_failed", trace, {
      error: errorMessage(error),
    });
    throw error;
  } finally {
    const latest = (await thread.state) ?? {};
    await thread.setState({
      activeExecution: retry,
      // Discord delta: refresh the staleness timestamp while a retry keeps
      // the flag alive; clear both together otherwise.
      activeExecutionStartedAt: retry ? Date.now() : null,
      lastEventId: Math.max(latest.lastEventId ?? 0, getLastEventId()),
      ...(rendered ? { renderObligation: null } : {}),
    });
    traceLog(options, "discordbot_render_finalized", trace, {
      obligation_cleared: rendered,
      retry_scheduled: retry,
      last_event_id: getLastEventId(),
    });
  }
}

function scheduleRenderObligationRecovery(
  chat: Chat<Record<string, Adapter>, DiscordbotThreadState>,
  state: StateAdapter,
  options: DiscordbotOptions,
): void {
  backgroundWaitUntil(recoverRenderObligationsWithRetry(chat, state, options));
}

async function recoverRenderObligationsWithRetry(
  chat: Chat<Record<string, Adapter>, DiscordbotThreadState>,
  state: StateAdapter,
  options: DiscordbotOptions,
): Promise<void> {
  // Wait for Postgres before scanning for obligations. This is also what warms the
  // shared pool at startup, so transient connect failures don't wedge the bot.
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
        "discordbot_render_recovery_retry_scheduled",
        undefined,
        {
          deferred_count: deferredCount,
          retry_delay_ms: delayMs,
          retry_attempt: attempt,
        },
      );
      await sleep(delayMs);
    } catch (error) {
      traceLog(options, "discordbot_render_recovery_failed", undefined, {
        error: errorMessage(error),
      });
      return;
    }
  }
}

async function recoverRenderObligations(
  chat: Chat<Record<string, Adapter>, DiscordbotThreadState>,
  state: StateAdapter,
  options: DiscordbotOptions,
): Promise<number> {
  const startedAtMs = nowMs();
  await chat.initialize();
  const indexedThreadIds = await state.getList<string>(
    RENDER_OBLIGATION_INDEX_KEY,
  );
  const threadIds = Array.from(new Set(indexedThreadIds));
  let deferredCount = 0;
  traceLog(options, "discordbot_render_recovery_scan", undefined, {
    obligation_count: threadIds.length,
    phase_ms: elapsedMs(startedAtMs),
  });

  for (const threadId of threadIds) {
    try {
      const thread = chat.thread(threadId);
      const threadState = await thread.state;
      const obligation = threadState?.renderObligation;
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
          "discordbot_render_recovery_lease_skipped",
          undefined,
          {
            thread_id: threadId,
          },
        );
        continue;
      }

      try {
        // Discord delta (no slackbotv2 analog): the obligation above was read
        // BEFORE the lease; another worker may have completed or replaced it
        // in between. Re-read under the lease and recover the current value.
        const leasedObligation = (await thread.state)?.renderObligation;
        if (!leasedObligation) {
          traceLog(
            options,
            "discordbot_render_recovery_obligation_gone",
            undefined,
            { thread_id: threadId },
          );
          continue;
        }
        if (
          await recoverRenderObligation(
            chat,
            state,
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
      // One thread's corrupt state or failed render must not abort the scan:
      // log it, count it as deferred so a later pass retries it, and keep
      // recovering the remaining threads.
      deferredCount += 1;
      traceLog(options, "discordbot_render_recovery_thread_failed", undefined, {
        error: errorMessage(error),
        thread_id: threadId,
      });
    }
  }
  return deferredCount;
}

async function recoverRenderObligation(
  chat: Chat<Record<string, Adapter>, DiscordbotThreadState>,
  state: StateAdapter,
  options: DiscordbotOptions,
  threadId: string,
  obligation: DiscordbotRenderObligation,
): Promise<boolean> {
  const trace: DiscordbotTrace = {
    includeContext: false,
    messageId: obligation.message.id,
    mode: "execute",
    openStream: true,
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
    openStream: false,
    threadId,
    trace,
  };

  let openedStream: AsyncIterable<DiscordbotRendererSource>;
  try {
    openedStream = await openSessionEventStream(options, input);
  } catch (error) {
    const retryable = isRetryableSessionApiError(error);
    traceLog(options, "discordbot_render_recovery_deferred", trace, {
      error: errorMessage(error),
      last_event_id: lastEventId,
      retryable,
    });
    if (retryable) return true;
    await renderRecoveredExecutionStream(
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
      // Discord delta: staleness timestamp, cleared together with the flag.
      activeExecutionStartedAt: Date.now(),
      lastEventId,
    });
    await renderRecoveredExecutionStream(
      thread,
      streamOpenedSession(input, openedStream),
      obligation.message,
      options,
      trace,
    );
    rendered = true;
    traceLog(options, "discordbot_render_recovery_complete", trace);
  } catch (error) {
    traceLog(options, "discordbot_render_recovery_render_failed", trace, {
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
    traceLog(options, "discordbot_render_recovery_finalized", trace, {
      obligation_cleared: rendered,
      last_event_id: lastEventId,
    });
  }
  return false;
}

async function indexRenderObligation(
  state: StateAdapter,
  input: {
    options: DiscordbotOptions;
    threadId: string;
    trace?: DiscordbotTrace;
  },
): Promise<void> {
  await state.appendToList(RENDER_OBLIGATION_INDEX_KEY, input.threadId, {
    maxLength: RENDER_OBLIGATION_INDEX_MAX_LENGTH,
    ttlMs: RENDER_INDEX_TTL_MS,
  });
  traceLog(input.options, "discordbot_render_obligation_indexed", input.trace);
}

async function* streamOpenedSession(
  input: Pick<ForwardSessionInput, "threadId" | "trace">,
  stream: AsyncIterable<DiscordbotRendererSource>,
): AsyncIterable<DiscordbotRendererSource> {
  // Deliberate delta from slackbotv2 (which removed its synthetic starting
  // task): the synthetic item primes the mapper's task state so answer deltas
  // stream immediately instead of waiting out the pre-stream grace period.
  yield startingStreamNotification(input.threadId);
  for await (const event of stream) yield event;
}

function renderRecoveryLeaseKey(threadId: string): string {
  return `discordbot:render:lease:${threadId}`;
}

async function renderExecutionStream(
  thread: Thread,
  stream: AsyncIterable<DiscordbotRendererSource>,
  message: DiscordbotApiMessage,
  options: DiscordbotOptions,
  isInitialExecution: boolean,
  trace?: DiscordbotTrace,
): Promise<void> {
  const logger = options.logger ?? noopLogger;
  if (
    isInitialExecution &&
    options.nameThreads !== false &&
    isThreadCreatedForMessage(thread.id, message.id)
  ) {
    await renameThreadFromMessage(
      options,
      thread.id,
      deriveThreadName(message.text, options.userName),
      logger,
    );
    traceLog(options, "discordbot_thread_named", trace);
  }

  if (isPlainTextOnlyRequest(message.text)) {
    // Discord delta (no slackbotv2 analog): plain-text-only runs previously
    // bypassed the narrator entirely — no 👀, no ✅/❌. Reuse the narrator for
    // the reaction lifecycle only (update() is never called, so no reasoning
    // blurbs are posted; the run still produces a single plain-text message).
    const narrator = DiscordNarrator.start(thread, message, options, {
      logger,
    });
    try {
      const sawError = await renderPlainTextExecutionStream(
        thread,
        stream,
        options,
        trace,
      );
      await narrator.finish(sawError ? "failed" : "done");
    } catch (error) {
      await narrator.finish(
        isRetryableSessionApiError(error) ? "retrying" : "failed",
      );
      throw error;
    }
    return;
  }

  // Append-only narration: an instant 👀 reaction on the triggering message,
  // then the agent's reasoning blurbs posted as their own subtext (-#)
  // messages as each thought completes, then the answer streamed into a separate message
  // created on first answer text. On settle the 👀 flips to ✅/❌. No bot
  // message is ever edited or deleted, so messages keep their place in the
  // timeline even when users chime in mid-run.
  const narrator = DiscordNarrator.start(thread, message, options, { logger });
  const stopTyping = startTypingKeepalive(thread, logger);
  try {
    await renderSplitExecutionStreams(thread, stream, options, narrator);
    await narrator.finish("done");
  } catch (error) {
    await narrator.finish(
      isRetryableSessionApiError(error) ? "retrying" : "failed",
    );
    throw error;
  } finally {
    stopTyping();
  }
}

/**
 * Consumes the renderer's chunk stream, routing task updates to the narrator
 * (reasoning blurbs) and answer text to separately streamed messages. The
 * answer post is created lazily on the first visible answer chunk (which is
 * also what keeps the startingStreamNotification priming working: the
 * synthetic item only unblocks deltas, it never creates an empty message).
 */
async function renderSplitExecutionStreams(
  thread: Thread,
  stream: AsyncIterable<DiscordbotRendererSource>,
  options: DiscordbotOptions,
  narrator: DiscordNarrator,
): Promise<void> {
  const answerText = new AsyncTextQueue();
  let answerPost: Promise<unknown> | null = null;
  let sourceFailed = false;
  try {
    for await (const chunk of codexAppServerToChatSdkStream(
      stream,
      rendererOptions(options),
    )) {
      if (chunk.type === "markdown_text") {
        if (!answerPost) {
          // Discord delta: stream the answer ourselves across multiple
          // ≤1900-char messages instead of the chat SDK's single-message
          // post+edit fallback, which silently truncates at Discord's
          // 2000-char cap with "...".
          answerPost = streamAnswerToThread(thread, answerText, options);
          // Swallow rejections until the finally below awaits the original
          // promise; otherwise an early failure (deleted thread/403) becomes
          // an unhandled rejection while this loop keeps consuming.
          answerPost.catch(() => undefined);
        }
        answerText.push(chunk.text);
        continue;
      }
      narrator.update(chunk);
    }
  } catch (error) {
    sourceFailed = true;
    throw error;
  } finally {
    answerText.end();
    if (answerPost) {
      // Settle the answer post either way, but when the source stream failed
      // that error is the one worth propagating.
      if (sourceFailed) await answerPost.catch(() => undefined);
      else await answerPost;
    }
  }
}

/**
 * Discord delta (replaces the chat SDK's post+edit fallbackStream for the
 * answer): streams answer text into Discord across MULTIPLE messages, each
 * ≤ ANSWER_MESSAGE_MAX_CHARS, splitting at newline/whitespace boundaries
 * (never through a surrogate pair; code fences are closed and re-opened when
 * a split inside one is unavoidable). The in-progress message is created on
 * the first visible text and edited at most once per ANSWER_EDIT_INTERVAL_MS.
 * Past ANSWER_MAX_FULL_MESSAGES the remainder collapses into one final
 * honestly-truncated message ("[truncated N chars ...]", never a silent
 * "..."). A failure of the final edit/post does not fail the run: it is
 * logged, the already-posted content stands, and a short note is appended
 * best-effort. Mid-stream post failures still propagate (the thread is gone).
 * Exported for tests only.
 */
export async function streamAnswerToThread(
  thread: Thread,
  source: AsyncIterable<string>,
  options: DiscordbotOptions,
): Promise<void> {
  const logger = options.logger ?? noopLogger;
  const editIntervalMs = Math.max(
    options.answerEditIntervalMs ?? ANSWER_EDIT_INTERVAL_MS,
    ANSWER_EDIT_INTERVAL_MS,
  );
  let pending = "";
  let current: { id: string; threadId: string } | null = null;
  let lastEditedContent = "";
  let lastEditAtMs = 0;
  let postedCount = 0;

  const postNew = async (content: string): Promise<void> => {
    // `raw` skips the SDK's markdown round-trip: re-stringifying could change
    // the text length past Discord's 2000-char cap (re-triggering the
    // adapter's silent truncation) and Discord renders markdown natively.
    const raw = await thread.adapter.postMessage(thread.id, { raw: content });
    current = { id: raw.id, threadId: raw.threadId || thread.id };
    lastEditedContent = content;
    lastEditAtMs = nowMs();
    postedCount += 1;
  };

  const editCurrent = async (content: string): Promise<void> => {
    if (!current || content === lastEditedContent) return;
    await thread.adapter.editMessage(current.threadId, current.id, {
      raw: content,
    });
    lastEditedContent = content;
  };

  /** Freeze the in-progress message at `content` (or post it whole). */
  const finalizeMessage = async (content: string): Promise<void> => {
    if (current) {
      await editCurrent(content);
      current = null;
      lastEditedContent = "";
    } else if (content.trim()) {
      await postNew(content);
      current = null;
      lastEditedContent = "";
    }
  };

  const overflowed = (): boolean => postedCount >= ANSWER_MAX_FULL_MESSAGES;
  const pendingView = (): string =>
    overflowed()
      ? truncateDiscordText(
          pending,
          ANSWER_MESSAGE_MAX_CHARS,
          "Discord final answer",
        )
      : pending;

  for await (const piece of source) {
    pending += piece;
    while (!overflowed()) {
      const split = takeDiscordMessageChunk(pending, ANSWER_MESSAGE_MAX_CHARS);
      if (!split) break;
      await finalizeMessage(split.chunk);
      pending = split.rest;
    }
    const view = pendingView();
    if (!view.trim()) continue;
    if (!current) {
      await postNew(view);
    } else if (nowMs() - lastEditAtMs >= editIntervalMs) {
      lastEditAtMs = nowMs();
      try {
        await editCurrent(view);
      } catch (error) {
        // In-progress edits are cosmetic; the final flush retries the content.
        logger.warn("discordbot_answer_edit_failed", {
          error: errorMessage(error),
        });
      }
    }
  }

  // Final flush. A failure here must not fail an otherwise-successful run.
  try {
    while (!overflowed()) {
      const split = takeDiscordMessageChunk(pending, ANSWER_MESSAGE_MAX_CHARS);
      if (!split) break;
      await finalizeMessage(split.chunk);
      pending = split.rest;
    }
    const view = pendingView();
    if (view.trim()) await finalizeMessage(view);
  } catch (error) {
    logger.warn("discordbot_answer_finalize_failed", {
      error: errorMessage(error),
      pending_chars: pending.length,
    });
    try {
      await thread.adapter.postMessage(thread.id, {
        raw: "-# ⚠️ The end of this answer failed to post; the output above may be incomplete.",
      });
    } catch {
      // Best-effort only — the run itself succeeded.
    }
  }
}

async function renderRecoveredExecutionStream(
  thread: Thread,
  stream: AsyncIterable<DiscordbotRendererSource>,
  message: DiscordbotApiMessage,
  options: DiscordbotOptions,
  trace?: DiscordbotTrace,
): Promise<void> {
  // Recovered renders never rename the thread; naming happens on the initial execution.
  // The narration/answer message split (and the plain-text-only branch) comes
  // via renderExecutionStream.
  await renderExecutionStream(thread, stream, message, options, false, trace);
}

async function renderPlainTextExecutionStream(
  thread: Thread,
  stream: AsyncIterable<DiscordbotRendererSource>,
  options: DiscordbotOptions,
  trace?: DiscordbotTrace,
): Promise<boolean> {
  const logger = options.logger ?? noopLogger;
  const fallback = new DiscordRenderFallback();
  const stopTyping = startTypingKeepalive(thread, logger);
  // Discord delta: reported back so the caller can settle the reaction
  // lifecycle as ❌ when the renderer surfaced an in-stream failure.
  let sawError = false;
  try {
    const chatStream = fallback.collectChatSdk(
      codexAppServerToChatSdkStream(
        fallback.collectSource(stream),
        rendererOptions(options),
      ),
    );
    for await (const chunk of chatStream) {
      if (chunk.type === "task_update" && chunk.status === "error") {
        sawError = true;
      }
    }
    const text = truncateDiscordText(
      fallback.text() || "Execution completed, but no final text was captured.",
      DISCORD_FALLBACK_TEXT_MAX_CHARS,
      "Discord final answer",
    );
    traceLog(options, "discordbot_render_plain_text_final", trace, {
      chars: text.length,
    });
    await thread.post(text);
  } finally {
    stopTyping();
  }
  return sawError;
}

class DiscordRenderFallback {
  private markdownText = "";
  private terminalText = "";

  async *collectSource(
    stream: AsyncIterable<DiscordbotRendererSource>,
  ): AsyncIterable<DiscordbotRendererSource> {
    for await (const event of stream) {
      this.captureTerminalText(event);
      yield event;
    }
  }

  async *collectChatSdk(
    stream: AsyncIterable<ChatSDKStreamChunk>,
  ): AsyncIterable<ChatSDKStreamChunk> {
    for await (const chunk of stream) {
      if (chunk.type === "markdown_text") this.markdownText += chunk.text;
      yield chunk;
    }
  }

  text(): string {
    return (this.terminalText || this.markdownText).trim();
  }

  private captureTerminalText(event: DiscordbotRendererSource): void {
    if (!event || typeof event !== "object") return;
    const eventKind = String(
      "eventKind" in event
        ? event.eventKind
        : "event" in event
          ? event.event
          : "",
    );
    if (
      eventKind !== "session.execution_completed" &&
      eventKind !== "session.execution_cancelled" &&
      !isTerminalCodexAppServerEvent(event)
    ) {
      return;
    }
    const data =
      "data" in event && event.data && typeof event.data === "object"
        ? event.data
        : event;
    const text = terminalResultText(data);
    if (text) this.terminalText = text;
  }
}

function isTerminalCodexAppServerEvent(event: unknown): boolean {
  if (!event || typeof event !== "object") return false;
  const type = (event as { type?: unknown }).type;
  return type === "result" || type === "turn.done" || type === "turn.completed";
}

function terminalResultText(event: unknown): string {
  if (!event || typeof event !== "object") return "";
  for (const key of ["result", "result_text", "text", "final_text"]) {
    const value = (event as Record<string, unknown>)[key];
    if (typeof value !== "string") continue;
    const resultText = value.trim();
    if (resultText) return resultText;
  }
  return "";
}

function isPlainTextOnlyRequest(text: string): boolean {
  const normalized = text.toLowerCase();
  return (
    /\bplain\s+text\s+only\b/.test(normalized) ||
    /\bno\s+interactive\s+blocks?\b/.test(normalized) ||
    /\bno\s+dashboards?\b/.test(normalized)
  );
}

function truncateDiscordText(
  value: string,
  maxChars: number,
  label: string,
): string {
  if (value.length <= maxChars) return value;
  let omitted = value.length - maxChars;
  while (true) {
    const suffix = `\n[truncated ${omitted} chars from ${label}]`;
    const keep = Math.max(0, maxChars - suffix.length);
    const actualOmitted = value.length - keep;
    if (actualOmitted === omitted)
      // Discord delta: surrogate-safe cut — halving an emoji's surrogate pair
      // makes Discord reject the whole payload with a 400.
      return `${sliceSurrogateSafe(value, keep).trimEnd()}${suffix}`;
    omitted = actualOmitted;
  }
}

async function* streamSessionAfterHandoff(
  options: DiscordbotOptions,
  input: ForwardSessionInput,
  onExecutionStarted?: (
    execution: DiscordbotExecuteSessionResponse,
  ) => Promise<void>,
): AsyncIterable<DiscordbotRendererSource> {
  // The 👀 working reaction is already queued before this generator is
  // consumed, so the user has instant feedback while the cold sandbox (incl.
  // tool-server sidecar) spends ~9s spinning up. Execute runs here, inside the
  // render stream, so a sandbox-spawn failure surfaces in the same render
  // rather than leaving the run looking alive forever (api-rs writes no event
  // if the spawn itself fails). The synthetic starting item primes the
  // mapper's task state so answer deltas stream without the pre-stream grace
  // delay.
  yield startingStreamNotification(input.threadId);
  traceLog(options, "discordbot_stream_heartbeat_emitted", input.trace);

  if (input.executeMessage) {
    try {
      const execution = await executeSessionTurn(options, input);
      if (execution) {
        // Scope the event stream we open below to this execution (upstream
        // #422 sets this where execute returns; for us that's in-stream).
        input.executionId = execution.execution_id;
        await onExecutionStarted?.(execution);
      }
    } catch (error) {
      traceLog(options, "discordbot_forward_failed", input.trace, {
        error: errorMessage(error),
      });
      if (isRetryableSessionApiError(error)) throw error;
      yield sessionStreamError(error);
      return;
    }
  }

  let stream: AsyncIterable<DiscordbotRendererSource>;
  try {
    stream = await openSessionEventStream(options, input);
  } catch (error) {
    traceLog(options, "discordbot_forward_failed", input.trace, {
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
): AsyncIterable<DiscordbotRendererSource> {
  yield sessionStreamError(error);
}

function backgroundWaitUntil(promise: Promise<unknown>): void {
  // Discord ingress runs in a long-lived Gateway process (no per-request waitUntil);
  // background work just needs its rejections swallowed after they are traced.
  void promise.catch(() => undefined);
}

// Vestigial wrapper kept so call sites diff cleanly against slackbotv2, whose
// rendererOptions hooks onRendererEvent to update the Slack assistant title
// (no Discord analog). Today it only forwards the configured mapper.
function rendererOptions(
  options: DiscordbotOptions,
): CodexAppServerToChatStreamOptions {
  const mapper = options.mapper;
  return {
    ...mapper,
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

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Discord's typing indicator expires after ~10s, so a single call blinks off mid-run. Re-fire on
 * an interval while the stream is open; errors are swallowed (typing is cosmetic) and the interval
 * is always cleared by the returned stop function.
 */
function startTypingKeepalive(thread: Thread, logger: Logger): () => void {
  const adapter = thread.adapter as TypingCapableAdapter;
  if (!adapter.startTyping) return () => undefined;

  const fire = (): void => {
    void adapter.startTyping?.(thread.id).catch((error) => {
      logger.debug("discordbot_typing_error", {
        error: error instanceof Error ? error.message : String(error),
      });
    });
  };
  fire();
  const interval = globalThis.setInterval(fire, TYPING_KEEPALIVE_MS);
  return () => globalThis.clearInterval(interval);
}
