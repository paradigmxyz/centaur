import {
  codexAppServerToChatSdkStream,
  type CodexAppServerToChatStreamOptions,
  type RendererEvent,
} from "@centaur/rendering";
import { Hono } from "hono";
import type { Pool } from "pg";
import { createPool } from "./db";
import type { TelegramInboxRecord, TransitionPatch } from "./inbox";
import {
  acceptForProcessing,
  claimNextPerThread,
  claimThreadBacklog,
  markIgnored,
  markRejected,
  pruneTerminal,
  transition,
} from "./inbox";
import type { IngestedUpdates, PollerController } from "./poller";
import { createPollerController } from "./poller";
import type { RateLimitedTelegramApi } from "./rate-limit";
import { createRateLimitedTelegramApi } from "./rate-limit";
import type { TelegramApiMessage } from "./session-api";
import {
  SessionApiError,
  executeSessionTurn,
  forwardToSessionApi,
  isContentlessApiMessage,
  isRetryableSessionApiError,
  isSteeringDeliveredEvent,
  isSteeringFailedEvent,
  openSessionEventStream,
  serializeTelegramAttachments,
  serializeTelegramMessage,
  startingStreamNotification,
  steeringDeliveredMessageIds,
  telegramAttachmentCandidates,
  type TelegramSessionCreateContext,
} from "./session-api";
import {
  extractCommandText,
  isAllowedTelegramMessage,
  isChatAllowlistEmpty,
  isTriggerMessage,
  isUserAllowlistEmpty,
} from "./telegram-allowlist";
import {
  TelegramApiError,
  createTelegramApi,
  isTelegramParseError,
} from "./telegram-api";
import { TelegramNarrator } from "./telegram-narrator";
import {
  TELEGRAM_MAX_MESSAGE_CHARS,
  chunkTelegramHtml,
  escapeTelegramHtml,
  parsedTextLength,
  renderMarkdownToTelegramHtml,
} from "./telegram-render";
import {
  deriveClientMessageId,
  deriveConversationName,
  deriveThreadKey,
  parseTelegramThreadKey,
} from "./telegram-threading";
import type {
  Logger,
  OwnershipLease,
  TelegramInboxStatus,
  TelegramMessage,
  TelegramNarratorOutcome,
  TelegramRenderObligation,
  TelegramUpdate,
  Telegrambot,
  TelegrambotOptions,
  TelegrambotRendererSource,
} from "./types";
import { errorMessage, noopLogger, nowMs } from "./utils";

export type { Telegrambot, TelegrambotOptions } from "./types";

// Composition root. Telegram delta vs discordbot/index.ts: there is no Chat
// SDK adapter for Telegram, so instead of Chat handlers + thread state this
// file owns a durable-inbox dispatcher — per-thread FIFO workers over the
// fenced Postgres ledger (src/inbox.ts), with rendering resumed from persisted
// TelegramRenderObligation rows rather than Chat SDK state keys.

const DEFAULT_MAX_CONCURRENT_THREADS = 4;
const DEFAULT_ANSWER_EDIT_INTERVAL_MS = 1_500;
// Spec §6: collapse honestly past a max-messages cap — a runaway answer stops
// opening new messages and says so instead of flooding the chat.
const DEFAULT_ANSWER_MAX_MESSAGES = 8;
const ANSWER_TRUNCATION_NOTICE =
  "<i>… answer truncated: it exceeded the Telegram message cap.</i>";
const DEFAULT_RETENTION_HOURS = 72;
// Periodic recovery sweep: restarts resume mid-flight work, missed nudges and
// steering fallbacks self-heal. Failed renders additionally schedule their own
// sooner retry sweep (see renderRetryDelayMs).
const SWEEP_INTERVAL_MS = 15_000;
const PRUNE_INTERVAL_MS = 15 * 60 * 1000;
// A steering_pending row with no in-process active execution (recovery, or an
// execute-conflict against an execution another/previous process started)
// re-tries the idempotent execute on this cadence until api-rs accepts it.
const STEERING_RETRY_BASE_DELAY_MS = 1_000;
const STEERING_RETRY_MAX_DELAY_MS = 30_000;
const RENDER_RETRY_BASE_DELAY_MS = 1_000;
const RENDER_RETRY_MAX_DELAY_MS = 30_000;
// Consecutive no-progress re-claims of the same oldest row before the thread
// worker yields to the sweep (hot-loop guard that never reorders the thread).
const MAX_BLOCKED_ROW_RECLAIMS = 3;
// Typing keepalive starts only when execution is noticeably slow: no answer
// text within this window after the render stream opens.
const SLOW_TYPING_AFTER_MS = 5_000;
// Sweep passes per nudge: each pass stamps at most one unrouted (NULL
// thread_key) row, so draining a backlog of them needs a bounded loop.
const MAX_SWEEP_PASSES = 50;

const NONTERMINAL_STATUSES: readonly TelegramInboxStatus[] = [
  "received",
  "message_appended",
  "steering_pending",
  "execution_accepted",
  "render_obligation_persisted",
];

// ---------------------------------------------------------------------------
// Inbox store seam
// ---------------------------------------------------------------------------

/**
 * The dispatcher's view of the durable inbox. A thin interface over
 * src/inbox.ts so orchestration unit tests can drive the full stage machine
 * against an in-memory fake without Postgres; every mutation is fenced with
 * the lease the caller passes (the poller's CURRENT lease, never a cached
 * one).
 */
export type TelegramInboxStore = {
  acceptForProcessing(
    lease: OwnershipLease,
    updateId: number,
    threadKey: string,
    clientMessageId: string,
  ): Promise<boolean>;
  claimNextPerThread(
    botUserId: string,
    excludeThreadKeys: readonly string[],
    limit: number,
  ): Promise<TelegramInboxRecord[]>;
  claimThreadBacklog(
    botUserId: string,
    threadKey: string,
    excludeUpdateIds: readonly number[],
    limit: number,
  ): Promise<TelegramInboxRecord[]>;
  markIgnored(
    lease: OwnershipLease,
    updateId: number,
    reason: string,
  ): Promise<boolean>;
  markRejected(
    lease: OwnershipLease,
    updateId: number,
    reason: string,
  ): Promise<boolean>;
  pruneTerminal(lease: OwnershipLease, retentionHours: number): Promise<number>;
  transition(
    lease: OwnershipLease,
    updateId: number,
    fromStatuses: readonly TelegramInboxStatus[],
    toStatus: TelegramInboxStatus,
    patch?: TransitionPatch,
  ): Promise<boolean>;
};

export function createPgInboxStore(
  pool: Pool,
  logger?: Logger,
): TelegramInboxStore {
  return {
    acceptForProcessing: (lease, updateId, threadKey, clientMessageId) =>
      acceptForProcessing(pool, lease, updateId, threadKey, clientMessageId),
    claimNextPerThread: (botUserId, excludeThreadKeys, limit) =>
      claimNextPerThread(pool, botUserId, excludeThreadKeys, limit),
    claimThreadBacklog: (botUserId, threadKey, excludeUpdateIds, limit) =>
      claimThreadBacklog(pool, botUserId, threadKey, excludeUpdateIds, limit),
    markIgnored: (lease, updateId, reason) =>
      markIgnored(pool, lease, updateId, reason, logger),
    markRejected: (lease, updateId, reason) =>
      markRejected(pool, lease, updateId, reason, logger),
    pruneTerminal: (lease, retentionHours) =>
      pruneTerminal(pool, lease, retentionHours, logger),
    transition: (lease, updateId, fromStatuses, toStatus, patch) =>
      transition(pool, lease, updateId, fromStatuses, toStatus, patch, logger),
  };
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

export type TelegramOwnership = {
  botUserId: string;
  lease: OwnershipLease;
};

export type TelegramDispatcherDeps = {
  api: RateLimitedTelegramApi;
  /** Bot @username (from getMe) for trigger matching / command stripping. */
  botUsername(): Promise<string>;
  logger?: Logger;
  options: TelegrambotOptions;
  /** The poller's CURRENT fenced identity; null whenever it cannot be proven. */
  ownership(): TelegramOwnership | null;
  store: TelegramInboxStore;
};

export type TelegramDispatcher = {
  /** Schedule a sweep pass; the returned promise settles when it has run. */
  nudge(): Promise<void>;
  /** Starts the periodic recovery sweep and retention pruning. */
  start(): void;
  shutdown(): Promise<void>;
};

type SteeringOutcome = "delivered" | "failed" | "terminal";

type SteeringWaiter = {
  resolve(outcome: SteeringOutcome): void;
  serverMessageIds: readonly string[];
  updateId: number;
};

/**
 * In-process record of a live execution for one thread: follow-up rows go
 * steering_pending against it and wait here for `session.steering_delivered`
 * (matched by the SERVER-assigned message ids the append response returned —
 * NOT client_message_ids), `session.steering_failed`, or execution end.
 */
type ActiveExecution = {
  deliver(serverMessageIds: readonly string[]): void;
  executionId: string;
  fail(): void;
  settle(): void;
  wait(updateId: number, serverMessageIds: readonly string[]): Promise<SteeringOutcome>;
};

/** Exported for the settled-wait regression test only. */
export function createActiveExecution(executionId: string): ActiveExecution {
  const waiters = new Set<SteeringWaiter>();
  // Once settled, late waiters resolve immediately as execution-terminal: a
  // follow-up worker can capture this instance, await a fenced transition,
  // and only then register its wait — racing the render's finally(), which
  // settles existing waiters and removes the instance from activeExecutions.
  // Without this flag that late wait() would never resolve, permanently
  // wedging the follow-up row (its update id stays owned) and hanging
  // shutdown on the tracked promise.
  let settled = false;
  const resolve = (waiter: SteeringWaiter, outcome: SteeringOutcome) => {
    waiters.delete(waiter);
    waiter.resolve(outcome);
  };
  return {
    executionId,
    wait(updateId, serverMessageIds) {
      if (settled) return Promise.resolve("terminal");
      return new Promise<SteeringOutcome>((res) => {
        waiters.add({ resolve: res, serverMessageIds, updateId });
      });
    },
    deliver(serverMessageIds) {
      const delivered = new Set(serverMessageIds);
      for (const waiter of [...waiters]) {
        if (waiter.serverMessageIds.some((id) => delivered.has(id))) {
          resolve(waiter, "delivered");
        }
      }
    },
    // steering_failed carries no message ids, so every pending waiter is
    // bounced back to message_appended; the idempotent execute either lands
    // (prior execution gone) or conflicts back into steering_pending. Never a
    // dropped message, never a competing execution.
    fail() {
      for (const waiter of [...waiters]) resolve(waiter, "failed");
    },
    settle() {
      settled = true;
      for (const waiter of [...waiters]) resolve(waiter, "terminal");
    },
  };
}

type GateResult =
  | { kind: "accepted"; clientMessageId: string; threadKey: string }
  | { kind: "ignored"; reason: string }
  | { kind: "rejected"; reason: string };

export function createTelegramDispatcher(
  deps: TelegramDispatcherDeps,
): TelegramDispatcher {
  const { api, options, store } = deps;
  const logger = deps.logger ?? options.logger ?? noopLogger;
  const maxConcurrentThreads =
    options.maxConcurrentThreads ?? DEFAULT_MAX_CONCURRENT_THREADS;
  const answerEditIntervalMs =
    options.answerEditIntervalMs ?? DEFAULT_ANSWER_EDIT_INTERVAL_MS;
  const answerMaxMessages =
    options.answerMaxMessages ?? DEFAULT_ANSWER_MAX_MESSAGES;
  const retentionHours = options.retentionHours ?? DEFAULT_RETENTION_HOURS;

  let stopped = false;
  const stopController = new AbortController();
  // Session API calls ride a stop-abortable fetch so shutdown can sever an
  // open SSE render stream instead of waiting out the execution.
  const baseFetch = options.fetch ?? fetch;
  const sessionOptions: TelegrambotOptions = {
    ...options,
    logger,
    fetch: (input, init) =>
      baseFetch(input, { ...init, signal: stopController.signal }),
  };

  // Update ids owned by detached in-process work (a background render or a
  // steering wait): the claim scans must not re-serve them while they are
  // live, and shutdown abandons them (rows stay durable for the next owner).
  const ownedUpdateIds = new Set<number>();
  // Server-assigned `msg_…` ids returned by the append response, keyed by
  // update id — in-memory only; after a restart steering correlation degrades
  // to execution-terminal detection by design.
  const appendedServerIds = new Map<number, string[]>();
  const activeExecutions = new Map<string, ActiveExecution>();
  const workerThreads = new Map<string, Promise<void>>();
  const pendingThreads = new Set<string>();
  const detached = new Set<Promise<unknown>>();
  const renderAttempts = new Map<number, number>();
  const steeringAttempts = new Map<number, number>();
  let runningWorkers = 0;
  let sweepTimer: ReturnType<typeof setInterval> | undefined;
  let pruneTimer: ReturnType<typeof setInterval> | undefined;
  const retryTimers = new Set<ReturnType<typeof setTimeout>>();

  let sweepChain: Promise<void> = Promise.resolve();
  let sweepQueued = false;

  function track<T>(promise: Promise<T>): Promise<T> {
    const wrapped = promise.finally(() => detached.delete(wrapped));
    detached.add(wrapped);
    return wrapped;
  }

  /** Per-row in-memory bookkeeping is dropped on every terminal disposition —
   * failed/ignored/rejected rows would otherwise leak their entries for the
   * process's lifetime. */
  function dropRowState(updateId: number): void {
    appendedServerIds.delete(updateId);
    renderAttempts.delete(updateId);
    steeringAttempts.delete(updateId);
  }

  // -------------------------------------------------------------------------
  // Gate (stage `received`, pre-append)
  // -------------------------------------------------------------------------

  async function evaluateGate(message: TelegramMessage): Promise<GateResult> {
    const own = deps.ownership();
    if (!own) return { kind: "ignored", reason: "ownership_lost" };
    const derived = deriveThreadKey(message, own.botUserId);
    if ("rejected" in derived) {
      return { kind: "rejected", reason: derived.rejected };
    }
    const allowed = isAllowedTelegramMessage(
      message,
      {
        botUserId: own.botUserId,
        chatAllowlist: options.chatAllowlist,
        userAllowlist: options.userAllowlist,
      },
      logger,
    );
    if (!allowed) return { kind: "rejected", reason: "not_allowlisted" };
    const trigger = isTriggerMessage(
      message,
      own.botUserId,
      await deps.botUsername(),
    );
    if (!trigger) return { kind: "ignored", reason: "not_a_trigger" };
    return {
      kind: "accepted",
      clientMessageId: deriveClientMessageId(message),
      threadKey: derived.threadKey,
    };
  }

  /**
   * Gate + stamp an unrouted `received` row. Runs in the sweep (not a thread
   * worker) because the row's thread is unknown until acceptance; the claim
   * scan hands out at most one such row per pass, oldest first, which is what
   * preserves same-thread FIFO for rows that have not been stamped yet.
   * Returns the stamped thread key, or null when the row went terminal.
   */
  async function gateAndStamp(
    record: TelegramInboxRecord,
    own: TelegramOwnership,
  ): Promise<string | null> {
    const message = record.payload.message;
    if (!message) {
      // Poller-side markIgnored can fail and leave a non-message row in
      // `received`; the claim scan tolerates it and stamps the disposition
      // durably here instead of letting it stall the thread scan.
      await store.markIgnored(
        own.lease,
        record.updateId,
        `unsupported_update_type:${updateKind(record.payload)}`,
      );
      return null;
    }
    const gate = await evaluateGate(message);
    if (gate.kind === "rejected") {
      await store.markRejected(own.lease, record.updateId, gate.reason);
      return null;
    }
    if (gate.kind === "ignored") {
      if (gate.reason === "ownership_lost") return null;
      await store.markIgnored(own.lease, record.updateId, gate.reason);
      return null;
    }
    const stamped = await store.acceptForProcessing(
      own.lease,
      record.updateId,
      gate.threadKey,
      gate.clientMessageId,
    );
    return stamped ? gate.threadKey : null;
  }

  // -------------------------------------------------------------------------
  // Session helpers
  // -------------------------------------------------------------------------

  async function buildApiMessage(
    message: TelegramMessage,
    threadKey: string,
  ): Promise<TelegramApiMessage> {
    const attachments =
      telegramAttachmentCandidates(message).length > 0
        ? await serializeTelegramAttachments(api, message, logger)
        : [];
    const apiMessage = serializeTelegramMessage(message, threadKey, {
      attachments,
    });
    // Strip the /ask[@bot] prefix from the session input; caption-only media
    // messages have no `text`, so their caption must not be clobbered by the
    // empty command extraction.
    if (message.text !== undefined) {
      apiMessage.text = extractCommandText(message, await deps.botUsername());
    }
    return apiMessage;
  }

  function createContext(
    message: TelegramMessage,
    threadKey: string,
  ): TelegramSessionCreateContext {
    const parsed = parseTelegramThreadKey(threadKey);
    return {
      chatType: message.chat.type,
      conversationName: deriveConversationName(message.chat),
      userId: message.from ? String(message.from.id) : "unknown",
      ...(parsed.messageThreadId === undefined
        ? {}
        : { messageThreadId: parsed.messageThreadId }),
    };
  }

  function buildObligation(
    message: TelegramMessage,
    threadKey: string,
    executionId: string,
  ): TelegramRenderObligation {
    return {
      afterEventId: 0,
      chatId: String(message.chat.id),
      deliveredText: "",
      executionId,
      messageThreadId: parseTelegramThreadKey(threadKey).messageThreadId ?? null,
      postedMessageIds: [],
      threadKey,
      triggerMessageId: message.message_id,
    };
  }

  /** api-rs refuses to start a competing execution with a 409 conflict; the
   * message then waits as steering_pending instead of executing twice. */
  function isExecuteConflict(error: unknown): boolean {
    return error instanceof SessionApiError && error.status === 409;
  }

  // -------------------------------------------------------------------------
  // Thread workers (per-thread FIFO, bounded cross-thread concurrency)
  // -------------------------------------------------------------------------

  function ensureThreadWorker(threadKey: string): void {
    if (stopped || workerThreads.has(threadKey)) return;
    if (runningWorkers >= maxConcurrentThreads) {
      pendingThreads.add(threadKey);
      return;
    }
    pendingThreads.delete(threadKey);
    runningWorkers += 1;
    const promise = runThreadWorker(threadKey)
      .catch((error) => {
        logger.error("telegrambot_thread_worker_failed", {
          error: errorMessage(error),
          thread_key: threadKey,
        });
      })
      .finally(() => {
        workerThreads.delete(threadKey);
        runningWorkers -= 1;
        if (stopped) return;
        const next = pendingThreads.values().next();
        if (!next.done) {
          pendingThreads.delete(next.value);
          ensureThreadWorker(next.value);
        }
      });
    workerThreads.set(threadKey, track(promise));
  }

  async function runThreadWorker(threadKey: string): Promise<void> {
    // FIFO invariant: a blocked/deferred OLDER row must end this worker run —
    // never let a newer sibling in the same thread append or execute first
    // (the sweep retries the whole thread from its oldest row later). Only
    // update ids owned by detached work (post-append renders/steering waits)
    // are excluded from the claim: bypassing those is safe because their turn
    // already reached the durable session in order. A bounded number of
    // no-progress re-claims of the same oldest row guards against a CAS-race
    // hot loop without ever reordering.
    let blockedUpdateId: number | null = null;
    let blockedReclaims = 0;
    while (!stopped) {
      const own = deps.ownership();
      if (!own) return;
      const rows = await store.claimThreadBacklog(
        own.botUserId,
        threadKey,
        [...ownedUpdateIds],
        1,
      );
      const row = rows[0];
      if (!row) return;
      const outcome = await processRow(row, threadKey);
      if (outcome === "defer_thread") return;
      if (outcome === "advanced") {
        blockedUpdateId = null;
        blockedReclaims = 0;
        continue;
      }
      blockedReclaims = row.updateId === blockedUpdateId ? blockedReclaims + 1 : 1;
      blockedUpdateId = row.updateId;
      if (blockedReclaims >= MAX_BLOCKED_ROW_RECLAIMS) {
        scheduleRetrySweep(RENDER_RETRY_BASE_DELAY_MS);
        return;
      }
    }
  }

  /**
   * Advance one row through the stage machine until it is terminal, detached
   * (background render / steering wait), or blocked. Every transition is
   * fenced with the poller's CURRENT lease, re-read at each step.
   *
   * Outcomes: "advanced" — the row moved (or this process cannot act on it);
   * "blocked" — a fenced CAS lost a race and the same row should be re-read;
   * "defer_thread" — a transient failure or backoff means the WHOLE thread
   * must wait for the retry sweep (processing a newer sibling first would
   * break same-thread FIFO).
   */
  async function processRow(
    record: TelegramInboxRecord,
    threadKey: string,
  ): Promise<"advanced" | "blocked" | "defer_thread"> {
    let status = record.status;
    let executionId = record.executionId;
    let obligation = record.renderObligation;
    const message = record.payload.message;
    let apiMessage: TelegramApiMessage | null = null;
    const getApiMessage = async (): Promise<TelegramApiMessage> => {
      if (!message) throw new Error("inbox row has no message payload");
      apiMessage ??= await buildApiMessage(message, threadKey);
      return apiMessage;
    };

    if (status === "received") {
      const own = deps.ownership();
      if (!own) return "advanced";
      if (!message) {
        return (await store.markIgnored(
          own.lease,
          record.updateId,
          `unsupported_update_type:${updateKind(record.payload)}`,
        ))
          ? "advanced"
          : "blocked";
      }
      // Re-run the gate even though acceptance stamped the row: allowlists
      // may have changed across a restart, and the append inputs (trigger
      // classification, command stripping) are derived from it.
      const gate = await evaluateGate(message);
      if (gate.kind === "rejected") {
        return (await store.markRejected(own.lease, record.updateId, gate.reason))
          ? "advanced"
          : "blocked";
      }
      if (gate.kind === "ignored") {
        if (gate.reason === "ownership_lost") return "advanced";
        return (await store.markIgnored(own.lease, record.updateId, gate.reason))
          ? "advanced"
          : "blocked";
      }
      const built = await getApiMessage();
      if (isContentlessApiMessage(built)) {
        // Sticker/location/poll-style messages serialize to nothing;
        // executing them would fabricate a synthetic "continue" turn.
        return (await store.markIgnored(
          own.lease,
          record.updateId,
          "contentless_message",
        ))
          ? "advanced"
          : "blocked";
      }
      // Create-if-missing (metadata re-upserted on every create by design)
      // plus the idempotent append keyed by the stable client_message_id. The
      // returned SERVER-assigned message ids are what steering_delivered
      // events reference, so they are kept for live correlation.
      let serverIds: string[] = [];
      try {
        await forwardToSessionApi(
          sessionOptions,
          {
            afterEventId: 0,
            create: createContext(message, threadKey),
            messages: [built],
            onEventId: () => undefined,
            openStream: false,
            threadKey,
          },
          {
            onMessagesAppended: async (ids) => {
              serverIds = ids;
            },
          },
        );
      } catch (error) {
        return failOrDefer(record.updateId, "append", error);
      }
      appendedServerIds.set(record.updateId, serverIds);
      const current = deps.ownership();
      if (!current) return "advanced";
      if (
        !(await store.transition(
          current.lease,
          record.updateId,
          ["received"],
          "message_appended",
        ))
      ) {
        return "blocked";
      }
      status = "message_appended";
    }

    while (status === "message_appended" || status === "steering_pending") {
      if (stopped) return "advanced";
      const own = deps.ownership();
      if (!own) return "advanced";

      if (status === "message_appended") {
        const active = activeExecutions.get(threadKey);
        if (active) {
          if (
            !(await store.transition(
              own.lease,
              record.updateId,
              ["message_appended"],
              "steering_pending",
            ))
          ) {
            return "blocked";
          }
          // The render's finally() may have settled `active` and removed it
          // during the fenced transition above. The settled flag makes a late
          // wait() resolve as execution-terminal, but when a FRESH instance
          // already replaced it the wait belongs on that one instead.
          const freshest = activeExecutions.get(threadKey) ?? active;
          detachSteeringWait(record, threadKey, freshest);
          return "advanced";
        }
        try {
          const execution = await executeSessionTurn(sessionOptions, {
            afterEventId: 0,
            create: createContext(assertMessage(message), threadKey),
            executeMessage: await getApiMessage(),
            messages: [],
            onEventId: () => undefined,
            openStream: false,
            threadKey,
          });
          if (!execution) return "advanced";
          executionId = execution.execution_id;
        } catch (error) {
          if (isExecuteConflict(error)) {
            // An execution this process does not know about (recovery, or a
            // race) is running: keep the row nonterminal as steering_pending
            // and let the steering branch below decide between a live wait
            // and a backed-off execute retry. Never a second execution. The
            // attempt bump makes the branch defer rather than immediately
            // re-issuing the execute that just conflicted.
            steeringAttempts.set(
              record.updateId,
              (steeringAttempts.get(record.updateId) ?? 0) + 1,
            );
            if (
              !(await store.transition(
                own.lease,
                record.updateId,
                ["message_appended"],
                "steering_pending",
              ))
            ) {
              return "blocked";
            }
            status = "steering_pending";
            continue;
          }
          return failOrDefer(record.updateId, "execute", error);
        }
        steeringAttempts.delete(record.updateId);
        if (
          !(await store.transition(
            own.lease,
            record.updateId,
            ["message_appended"],
            "execution_accepted",
            { executionId },
          ))
        ) {
          return "blocked";
        }
        status = "execution_accepted";
        break;
      }

      // steering_pending
      const active = activeExecutions.get(threadKey);
      if (active) {
        detachSteeringWait(record, threadKey, active);
        return "advanced";
      }
      // No live execution in this process: after a restart the appended
      // message ids are gone, so steering resolution falls back to
      // execution-terminal detection — the idempotent execute either lands
      // (prior execution terminal) or conflicts again. Retries back off
      // exponentially and yield this worker's slot to the retry sweep instead
      // of spinning fenced writes + execute conflicts inside it for the life
      // of a foreign execution.
      const attempt = steeringAttempts.get(record.updateId) ?? 0;
      if (
        !(await store.transition(
          own.lease,
          record.updateId,
          ["steering_pending"],
          "message_appended",
        ))
      ) {
        return "blocked";
      }
      if (attempt === 0) {
        // Restart recovery: no conflict has been observed this process-life,
        // so the prior execution is most likely gone — execute immediately.
        status = "message_appended";
        continue;
      }
      scheduleRetrySweep(steeringRetryDelayMs(attempt));
      return "defer_thread";
    }

    if (status === "execution_accepted") {
      const own = deps.ownership();
      if (!own) return "advanced";
      obligation ??= buildObligation(
        assertMessage(message),
        threadKey,
        assertExecutionId(executionId),
      );
      if (
        !(await store.transition(
          own.lease,
          record.updateId,
          ["execution_accepted"],
          "render_obligation_persisted",
          { renderObligation: obligation },
        ))
      ) {
        return "blocked";
      }
      status = "render_obligation_persisted";
    }

    if (status === "render_obligation_persisted") {
      detachRender(record, obligation ?? record.renderObligation, threadKey);
      return "advanced";
    }

    return "advanced";
  }

  /** Retryable session API failures defer the WHOLE thread to the retry
   * sweep (skipping to a newer sibling would break same-thread FIFO);
   * permanent validation failures record a durable terminal reason. */
  async function failOrDefer(
    updateId: number,
    action: string,
    error: unknown,
  ): Promise<"advanced" | "defer_thread"> {
    if (isRetryableSessionApiError(error)) {
      logger.warn("telegrambot_dispatch_deferred", {
        action,
        error: errorMessage(error),
        update_id: updateId,
      });
      scheduleRetrySweep(RENDER_RETRY_BASE_DELAY_MS);
      return "defer_thread";
    }
    logger.warn("telegrambot_dispatch_failed", {
      action,
      error: errorMessage(error),
      update_id: updateId,
    });
    const own = deps.ownership();
    if (!own) return "advanced";
    await store.transition(
      own.lease,
      updateId,
      NONTERMINAL_STATUSES,
      "failed",
      { statusReason: `${action}: ${errorMessage(error)}` },
    );
    dropRowState(updateId);
    return "advanced";
  }

  // -------------------------------------------------------------------------
  // Steering waits (detached)
  // -------------------------------------------------------------------------

  function detachSteeringWait(
    record: TelegramInboxRecord,
    threadKey: string,
    active: ActiveExecution,
  ): void {
    ownedUpdateIds.add(record.updateId);
    const serverIds = appendedServerIds.get(record.updateId) ?? [];
    const promise = active
      .wait(record.updateId, serverIds)
      .then(async (outcome) => {
        if (stopped) return;
        const own = deps.ownership();
        if (!own) return;
        if (outcome === "delivered") {
          await store.transition(
            own.lease,
            record.updateId,
            ["steering_pending"],
            "steered",
          );
          dropRowState(record.updateId);
          return;
        }
        // steering_failed or execution terminal without delivery: back to
        // message_appended for the idempotent execute. The row is never
        // dropped and no competing execution starts (conflicts loop back).
        await store.transition(
          own.lease,
          record.updateId,
          ["steering_pending"],
          "message_appended",
        );
      })
      .catch((error) => {
        logger.warn("telegrambot_steering_resolution_failed", {
          error: errorMessage(error),
          update_id: record.updateId,
        });
      })
      .finally(() => {
        ownedUpdateIds.delete(record.updateId);
        if (!stopped) ensureThreadWorker(threadKey);
      });
    track(promise);
  }

  // -------------------------------------------------------------------------
  // Rendering (detached, at-least-once)
  // -------------------------------------------------------------------------

  function detachRender(
    record: TelegramInboxRecord,
    obligation: TelegramRenderObligation | null,
    threadKey: string,
  ): void {
    if (!obligation) {
      logger.error("telegrambot_render_obligation_missing", {
        update_id: record.updateId,
      });
      return;
    }
    ownedUpdateIds.add(record.updateId);
    const active = createActiveExecution(obligation.executionId);
    activeExecutions.set(threadKey, active);
    // Releasing ownership immediately after a FAILED attempt would let the
    // thread worker reclaim the row in a zero-delay hot loop; the row stays
    // owned until the backoff timer fires and hands it back to the sweep.
    const release = (): void => {
      ownedUpdateIds.delete(record.updateId);
      if (!stopped) {
        ensureThreadWorker(threadKey);
        void nudge();
      }
    };
    const releaseAfterBackoff = (): void => {
      const attempt = renderAttempts.get(record.updateId) ?? 0;
      renderAttempts.set(record.updateId, attempt + 1);
      if (stopped) return;
      const timer = setTimeout(() => {
        retryTimers.delete(timer);
        release();
      }, renderRetryDelayMs(attempt));
      timer.unref?.();
      retryTimers.add(timer);
    };
    const promise = runRender(record, obligation, active)
      .then((result) => {
        if (result === "completed") {
          dropRowState(record.updateId);
          release();
          return;
        }
        releaseAfterBackoff();
      })
      .catch((error) => {
        logger.error("telegrambot_render_crashed", {
          error: errorMessage(error),
          update_id: record.updateId,
        });
        releaseAfterBackoff();
      })
      .finally(() => {
        if (activeExecutions.get(threadKey) === active) {
          activeExecutions.delete(threadKey);
        }
        // Waiters resolve as execution-terminal either way: if the execution
        // is actually still live (render failure, not terminal), their
        // idempotent execute conflicts straight back into steering_pending.
        active.settle();
      });
    track(promise);
  }

  async function runRender(
    record: TelegramInboxRecord,
    obligation: TelegramRenderObligation,
    active: ActiveExecution,
  ): Promise<"completed" | "retry"> {
    // Narrator posts ride the plain (normal-priority) TelegramApi surface —
    // narration must never delay terminal answer delivery, which uses the
    // urgent path inside AnswerDelivery.
    const narrator = TelegramNarrator.start(
      api,
      {
        chatId: obligation.chatId,
        messageThreadId: obligation.messageThreadId,
        triggerMessageId: obligation.triggerMessageId,
      },
      { logger },
    );
    const slowTimer = setTimeout(() => {
      narrator.startTypingKeepalive();
    }, SLOW_TYPING_AFTER_MS);
    slowTimer.unref?.();
    let outcome: TelegramNarratorOutcome = "retrying";
    let result: "completed" | "retry" = "retry";
    try {
      let lastEventId = obligation.afterEventId;
      // Ref object rather than a plain `let`: the value is assigned from the
      // observeSource generator, and TS control-flow narrowing on a captured
      // let would pin it to the initializer's null.
      const terminal: { kind: "done" | "failed" | null } = { kind: null };
      let sawAnswerText = false;
      const delivery = new AnswerDelivery({
        answerEditIntervalMs,
        answerMaxMessages,
        api,
        logger,
        obligation,
        ownership: deps.ownership,
        store,
        updateId: record.updateId,
      });
      const stream = await openSessionEventStream(sessionOptions, {
        afterEventId: obligation.afterEventId,
        executionId: obligation.executionId,
        onEventId: (eventId) => {
          lastEventId = Math.max(lastEventId, eventId);
        },
        threadKey: obligation.threadKey,
      });
      const source = observeSource(stream, obligation.threadKey, active, (t) => {
        terminal.kind = t;
      });
      for await (const chunk of codexAppServerToChatSdkStream(
        source,
        rendererOptions(narrator),
      )) {
        if (stopped) throw new Error("dispatcher stopped mid-render");
        if (chunk.type === "markdown_text") {
          if (!sawAnswerText) {
            sawAnswerText = true;
            clearTimeout(slowTimer);
            narrator.stopTypingKeepalive();
          }
          delivery.append(chunk.text, lastEventId);
          await delivery.flush(false);
          continue;
        }
        if (chunk.type === "task_update") narrator.update(chunk);
      }
      const terminalKindSeen = terminal.kind;
      if (terminalKindSeen === null) {
        // Connection drop, not a terminal event: the execution may still be
        // running. Leave the row for the sweep; never fake a completion.
        throw new Error("session event stream ended without a terminal event");
      }
      await delivery.flush(true);
      const own = deps.ownership();
      if (
        own &&
        (await store.transition(
          own.lease,
          record.updateId,
          ["render_obligation_persisted"],
          "completed",
        ))
      ) {
        outcome = terminalKindSeen;
        result = "completed";
      }
      return result;
    } catch (error) {
      logger.warn("telegrambot_render_attempt_failed", {
        error: errorMessage(error),
        execution_id: obligation.executionId,
        thread_key: obligation.threadKey,
        update_id: record.updateId,
      });
      return "retry";
    } finally {
      clearTimeout(slowTimer);
      // Exactly one finish per render attempt: done/failed settle the
      // reaction; "retrying" leaves 👀 in place for the sweep's next attempt.
      await narrator.finish(outcome);
    }
  }

  /**
   * Prime the mapper (immediate answer streaming, no pre-stream grace), keep
   * steering events out of the renderer (they resolve steering_pending rows
   * instead), and record the terminal disposition.
   */
  async function* observeSource(
    stream: AsyncIterable<TelegrambotRendererSource>,
    threadKey: string,
    active: ActiveExecution,
    onTerminal: (kind: "done" | "failed") => void,
  ): AsyncIterable<TelegrambotRendererSource> {
    yield startingStreamNotification(threadKey);
    for await (const event of stream) {
      if (isSteeringDeliveredEvent(event)) {
        active.deliver(steeringDeliveredMessageIds(event));
        continue;
      }
      if (isSteeringFailedEvent(event)) {
        active.fail();
        continue;
      }
      const terminal = terminalKind(event);
      if (terminal) onTerminal(terminal);
      yield event;
    }
  }

  function rendererOptions(
    narrator: TelegramNarrator,
  ): CodexAppServerToChatStreamOptions {
    const mapper = options.mapper;
    return {
      ...mapper,
      // Answer text streams into its own Telegram messages, so there is no
      // card to wait for (same reasoning as discordbot).
      preStreamGraceMs: 0,
      async onRendererEvent(event: RendererEvent) {
        await mapper?.onRendererEvent?.(event);
        if (event.type === "renderer.status") narrator.status(event.status);
      },
    };
  }

  // -------------------------------------------------------------------------
  // Sweep
  // -------------------------------------------------------------------------

  function nudge(): Promise<void> {
    if (stopped) return Promise.resolve();
    if (!sweepQueued) {
      sweepQueued = true;
      sweepChain = sweepChain
        .then(() => {
          sweepQueued = false;
          return runSweep();
        })
        .catch((error) => {
          logger.warn("telegrambot_sweep_failed", {
            error: errorMessage(error),
          });
        });
    }
    return sweepChain;
  }

  function scheduleRetrySweep(delayMs: number): void {
    if (stopped) return;
    const timer = setTimeout(() => {
      retryTimers.delete(timer);
      void nudge();
    }, delayMs);
    timer.unref?.();
    retryTimers.add(timer);
  }

  /**
   * Dispatch + recovery share one path: claim the oldest nonterminal row per
   * thread (skipping threads that already have an in-process worker) and hand
   * each stamped thread to its FIFO worker. Unrouted rows (NULL thread_key,
   * including payloads without .message) are gated and stamped here, one per
   * pass, oldest first. Running everything through the durable claim scan is
   * what makes restarts resume mid-flight work before any new dispatch.
   */
  async function runSweep(): Promise<void> {
    for (let pass = 0; pass < MAX_SWEEP_PASSES; pass += 1) {
      if (stopped) return;
      const own = deps.ownership();
      if (!own) return;
      const exclude = [...workerThreads.keys(), ...pendingThreads];
      const records = await store.claimNextPerThread(
        own.botUserId,
        exclude,
        Math.max(maxConcurrentThreads * 2, 8),
      );
      let stampedUnrouted = false;
      for (const record of records) {
        if (stopped) return;
        if (ownedUpdateIds.has(record.updateId)) {
          // Oldest row is a live detached render/steering wait; its thread
          // worker (if any newer rows exist) drains the rest of the backlog.
          if (record.threadKey) ensureThreadWorker(record.threadKey);
          continue;
        }
        if (record.threadKey) {
          ensureThreadWorker(record.threadKey);
          continue;
        }
        stampedUnrouted = true;
        const threadKey = await gateAndStamp(record, own);
        if (threadKey) ensureThreadWorker(threadKey);
      }
      // Another pass only while unrouted rows are being drained (the scan
      // exposes one per pass); otherwise the workers own the rest.
      if (!stampedUnrouted) return;
    }
  }

  async function prune(): Promise<void> {
    const own = deps.ownership();
    if (!own) return;
    try {
      await store.pruneTerminal(own.lease, retentionHours);
    } catch (error) {
      logger.warn("telegrambot_prune_failed", { error: errorMessage(error) });
    }
  }

  return {
    nudge,

    start(): void {
      sweepTimer = setInterval(() => void nudge(), SWEEP_INTERVAL_MS);
      sweepTimer.unref?.();
      pruneTimer = setInterval(() => void prune(), PRUNE_INTERVAL_MS);
      pruneTimer.unref?.();
      void nudge();
      void track(prune());
    },

    async shutdown(): Promise<void> {
      stopped = true;
      if (sweepTimer !== undefined) clearInterval(sweepTimer);
      if (pruneTimer !== undefined) clearInterval(pruneTimer);
      for (const timer of retryTimers) clearTimeout(timer);
      retryTimers.clear();
      // Sever open SSE streams / in-flight session calls; the durable rows
      // stay at their stage for the next owner's recovery.
      stopController.abort();
      for (const active of activeExecutions.values()) active.settle();
      await Promise.allSettled([...detached, sweepChain]);
    },
  };
}

// ---------------------------------------------------------------------------
// Answer delivery (streamed, durable, at-least-once)
// ---------------------------------------------------------------------------

type AnswerDeliveryDeps = {
  answerEditIntervalMs: number;
  answerMaxMessages: number;
  api: RateLimitedTelegramApi;
  logger: Logger;
  obligation: TelegramRenderObligation;
  ownership(): TelegramOwnership | null;
  store: TelegramInboxStore;
  updateId: number;
};

/**
 * Streams the accumulated answer markdown into Telegram messages: the first
 * chunk is an urgent send replying to the trigger message, in-place growth is
 * a throttled urgent edit, and overflow past the 4096 parsed-char boundary
 * becomes further urgent sends. Every returned message_id and the delivered
 * markdown snapshot are persisted into the render obligation (fenced patch)
 * BEFORE any subsequent edit/chunk, so recovery resumes with an edit or the
 * next chunk and never reposts a recorded one. A send whose response was
 * never recorded is ambiguous and may be duplicated on recovery — that is the
 * documented at-least-once contract; execution is never repeated to redeliver.
 */
class AnswerDelivery {
  private readonly deps: AnswerDeliveryDeps;
  private markdown: string;
  private eventIdForAccumulated: number;
  private readonly posted: number[];
  private plainMode = false;
  private truncated = false;
  private currentContent: string | null = null;
  private lastEditAtMs = 0;

  constructor(deps: AnswerDeliveryDeps) {
    this.deps = deps;
    this.markdown = deps.obligation.deliveredText;
    this.eventIdForAccumulated = deps.obligation.afterEventId;
    this.posted = [...deps.obligation.postedMessageIds];
  }

  append(text: string, eventId: number): void {
    this.markdown += text;
    this.eventIdForAccumulated = eventId;
  }

  /** Rendered chunks of the full answer so far, each ≤4096 parsed chars with
   * balanced tags (fences close at a boundary and re-open in the next chunk).
   * Past the max-messages cap the chunk list is collapsed honestly: the final
   * message ends with an explicit truncation notice instead of the overflow
   * silently continuing into further sends (spec §6). Chunk boundaries are
   * prefix-stable as the markdown grows, so capped earlier messages never
   * churn. */
  private renderChunks(): string[] {
    // Plain mode: Telegram rejected the rendered HTML with a parse/entity
    // error, so the fallback is the escaped tag-free markdown — the same body
    // renderPlainTextFallback produces — still sent with parse_mode HTML.
    const html = this.plainMode
      ? escapeTelegramHtml(this.markdown)
      : renderMarkdownToTelegramHtml(this.markdown);
    const chunks = chunkTelegramHtml(html);
    const max = Math.max(1, this.deps.answerMaxMessages);
    if (chunks.length <= max) return chunks;
    if (!this.truncated) {
      this.truncated = true;
      this.deps.logger.warn("telegrambot_answer_truncated", {
        chat_id: this.deps.obligation.chatId,
        chunks: chunks.length,
        max_messages: max,
      });
    }
    // Re-chunk the capped tail to leave parsed-length budget for the notice;
    // the sub-chunker closes any open tags, so appending the notice is valid.
    const kept = chunks.slice(0, max);
    const tail = kept[max - 1] ?? "";
    const budget =
      TELEGRAM_MAX_MESSAGE_CHARS - parsedTextLength(ANSWER_TRUNCATION_NOTICE) - 1;
    const trimmedTail = chunkTelegramHtml(tail, budget)[0] ?? "";
    kept[max - 1] = `${trimmedTail}\n${ANSWER_TRUNCATION_NOTICE}`;
    return kept;
  }

  async flush(final: boolean): Promise<void> {
    let chunks = this.renderChunks();
    if (chunks.length === 0) return;

    while (this.posted.length < chunks.length) {
      const index = this.posted.length;
      if (index > 0) {
        // Freeze the previous message at its final chunk before overflowing
        // into a new one.
        chunks = await this.editChunk(index - 1, chunks);
        if (this.posted.length >= chunks.length) break;
      }
      chunks = await this.sendChunk(this.posted.length, chunks);
    }

    const tailIndex = Math.min(this.posted.length, chunks.length) - 1;
    const tailContent = tailIndex >= 0 ? chunks[tailIndex] : undefined;
    if (
      tailContent !== undefined &&
      tailContent !== this.currentContent &&
      (final || nowMs() - this.lastEditAtMs >= this.deps.answerEditIntervalMs)
    ) {
      this.lastEditAtMs = nowMs();
      await this.editChunk(tailIndex, chunks);
    }
  }

  private async sendChunk(index: number, chunks: string[]): Promise<string[]> {
    const content = chunks[index];
    if (content === undefined) return chunks;
    const { api, obligation } = this.deps;
    try {
      const message = await api.sendMessageUrgent({
        chat_id: obligation.chatId,
        parse_mode: "HTML",
        text: content,
        ...(obligation.messageThreadId === null
          ? {}
          : { message_thread_id: obligation.messageThreadId }),
        // Only the first answer message replies to the trigger; overflow
        // chunks follow it in the timeline. allow_sending_without_reply:
        // Telegram otherwise rejects the send outright when the trigger
        // message was deleted, which would wedge the render obligation in an
        // infinite retry loop — a reply-less answer beats no answer.
        ...(this.posted.length === 0
          ? {
              reply_parameters: {
                allow_sending_without_reply: true,
                message_id: obligation.triggerMessageId,
              },
            }
          : {}),
      });
      this.posted.push(message.message_id);
      this.currentContent = content;
      await this.persist();
      return chunks;
    } catch (error) {
      const fallback = this.enterPlainMode(error);
      if (!fallback) throw error;
      return index < fallback.length
        ? await this.sendChunk(index, fallback)
        : fallback;
    }
  }

  private async editChunk(index: number, chunks: string[]): Promise<string[]> {
    const content = chunks[index];
    const messageId = this.posted[index];
    if (content === undefined || messageId === undefined) return chunks;
    if (index === this.posted.length - 1 && content === this.currentContent) {
      return chunks;
    }
    const { api, obligation } = this.deps;
    try {
      await api.editMessageTextUrgent({
        chat_id: obligation.chatId,
        message_id: messageId,
        parse_mode: "HTML",
        text: content,
      });
    } catch (error) {
      if (isMessageNotModifiedError(error)) {
        // Recovery re-edit with identical content; the message already shows
        // exactly this chunk.
      } else {
        const fallback = this.enterPlainMode(error);
        if (!fallback) throw error;
        return index < fallback.length
          ? await this.editChunk(index, fallback)
          : fallback;
      }
    }
    if (index === this.posted.length - 1) {
      this.currentContent = content;
      await this.persist();
    }
    return chunks;
  }

  /** One-way switch to the escaped plain-text fallback on a Telegram parse
   * rejection; a second parse error in plain mode is a real fault. */
  private enterPlainMode(error: unknown): string[] | null {
    if (!isTelegramParseError(error) || this.plainMode) return null;
    this.deps.logger.warn("telegrambot_answer_parse_fallback", {
      chat_id: this.deps.obligation.chatId,
      error: errorMessage(error),
    });
    this.plainMode = true;
    this.currentContent = null;
    return this.renderChunks();
  }

  /**
   * Fenced obligation patch. afterEventId only ever advances together with
   * the deliveredText snapshot that contains those events' answer deltas —
   * advancing it past undelivered text would lose that text on SSE replay.
   */
  private async persist(): Promise<void> {
    const own = this.deps.ownership();
    if (!own) throw new Error("ownership lost during answer delivery");
    const moved = await this.deps.store.transition(
      own.lease,
      this.deps.updateId,
      ["render_obligation_persisted"],
      "render_obligation_persisted",
      {
        renderObligation: {
          ...this.deps.obligation,
          afterEventId: this.eventIdForAccumulated,
          deliveredText: this.markdown,
          postedMessageIds: [...this.posted],
        },
      },
    );
    if (!moved) throw new Error("render obligation persist was fenced out");
  }
}

// ---------------------------------------------------------------------------
// createTelegrambot
// ---------------------------------------------------------------------------

export function createTelegrambot(options: TelegrambotOptions): Telegrambot {
  const logger = options.logger ?? noopLogger;
  if (!options.pool && !options.postgresUrl) {
    throw new Error("telegrambot requires postgresUrl or an injected pool");
  }
  const createdPool = !options.pool;
  const pool = options.pool ?? createPool(options.postgresUrl ?? "", logger);

  // Fail-closed allowlists: warn loudly at boot when a surface is inert.
  if (isChatAllowlistEmpty(options)) {
    logger.warn("telegrambot_chat_allowlist_empty_inert", {
      hint: "Set TELEGRAMBOT_CHAT_ALLOWLIST; all group messages are ignored until configured.",
    });
  }
  if (isUserAllowlistEmpty(options)) {
    logger.warn("telegrambot_user_allowlist_empty_inert", {
      hint: "Set TELEGRAMBOT_USER_ALLOWLIST; all DMs are ignored until configured.",
    });
  }

  const rawApi = createTelegramApi(options);
  const api = createRateLimitedTelegramApi(rawApi, logger);

  // The trigger gate needs the bot @username, known only after getMe; cached
  // per process, re-fetched on failure.
  let botUsernamePromise: Promise<string> | null = null;
  const botUsername = (): Promise<string> => {
    botUsernamePromise ??= rawApi
      .getMe()
      .then((me) => me.username ?? options.userName ?? "centaur")
      .catch((error: unknown) => {
        botUsernamePromise = null;
        throw error;
      });
    return botUsernamePromise;
  };

  const store = createPgInboxStore(pool, logger);
  let poller: PollerController | null = null;
  const dispatcher = createTelegramDispatcher({
    api,
    botUsername,
    logger,
    options,
    ownership: () => poller?.ownership() ?? null,
    store,
  });
  poller = createPollerController({
    api,
    pool,
    logger,
    ...(options.leaseTtlMs === undefined
      ? {}
      : { leaseTtlMs: options.leaseTtlMs }),
    ...(options.pollTimeoutSeconds === undefined
      ? {}
      : { pollTimeoutSeconds: options.pollTimeoutSeconds }),
    // Ingested batches are already durable `received` rows; the dispatcher's
    // claim scan is the single dispatch path, so the callback is just a nudge.
    onUpdatesIngested: (_batch: IngestedUpdates) => void dispatcher.nudge(),
  });
  const controller = poller;

  const app = new Hono();
  app.get("/live", (c) => {
    const status = controller.status();
    return c.json(
      { ok: status.live, service: "telegrambot" },
      status.live ? 200 : 503,
    );
  });
  app.get("/ready", (c) => {
    const status = controller.status();
    return c.json(
      { ok: status.ready, reasons: status.reasons, service: "telegrambot" },
      status.ready ? 200 : 503,
    );
  });

  return {
    app,

    // The poller's background run loop gates every dispatch on fenced
    // ownership, and the dispatcher's first sweeps resume nonterminal rows
    // (oldest-first per thread) before any newly polled update is dispatched
    // — recovery and new work share the same ordered claim scan.
    async start(): Promise<void> {
      controller.start();
      if (options.recoverRenderObligationsOnStart !== false) {
        dispatcher.start();
      }
    },

    async shutdown(): Promise<void> {
      await dispatcher.shutdown();
      await controller.shutdown();
      if (createdPool) await pool.end();
    },
  };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function updateKind(update: TelegramUpdate): string {
  for (const key of Object.keys(update)) {
    if (key !== "update_id") return key;
  }
  return "unknown";
}

function assertMessage(message: TelegramMessage | undefined): TelegramMessage {
  if (!message) throw new Error("inbox row has no message payload");
  return message;
}

function assertExecutionId(executionId: string | null): string {
  if (!executionId) throw new Error("inbox row has no execution id");
  return executionId;
}

function renderRetryDelayMs(attempt: number): number {
  return Math.min(
    RENDER_RETRY_MAX_DELAY_MS,
    RENDER_RETRY_BASE_DELAY_MS * 2 ** Math.min(attempt, 10),
  );
}

function steeringRetryDelayMs(attempt: number): number {
  return Math.min(
    STEERING_RETRY_MAX_DELAY_MS,
    STEERING_RETRY_BASE_DELAY_MS * 2 ** Math.min(attempt, 10),
  );
}

/** Telegram answers an edit whose body already matches with a 400; recovery
 * legitimately re-edits the tail with identical content, so this is success. */
function isMessageNotModifiedError(error: unknown): boolean {
  return (
    error instanceof TelegramApiError &&
    error.status === 400 &&
    /not modified/i.test(error.description ?? "")
  );
}

/**
 * Terminal disposition of a session stream event. session-api ends the
 * iterable after these, but the iterable ALSO ends on a plain connection
 * drop — only an observed terminal event may complete the render.
 */
function terminalKind(
  source: TelegrambotRendererSource,
): "done" | "failed" | null {
  if (!source || typeof source !== "object") return null;
  const record = source as { data?: unknown; event?: unknown; eventKind?: unknown };
  const kind =
    typeof record.eventKind === "string"
      ? record.eventKind
      : typeof record.event === "string"
        ? record.event
        : "";
  if (kind === "session.execution_completed") return "done";
  if (
    kind === "session.execution_failed" ||
    kind === "session.stream_error" ||
    kind === "session.execution_cancelled"
  ) {
    return "failed";
  }
  if (kind !== "session.output.line" || typeof record.data !== "string") {
    return null;
  }
  let payload: unknown;
  try {
    payload = JSON.parse(record.data);
  } catch {
    return null;
  }
  if (!payload || typeof payload !== "object") return null;
  const line = payload as { method?: unknown; type?: unknown };
  if (line.type === "turn.failed" || line.method === "error") return "failed";
  if (
    line.type === "turn.completed" ||
    line.type === "turn.done" ||
    line.method === "turn/completed"
  ) {
    return "done";
  }
  return null;
}
