import type { Pool } from "pg";
import type { IngestHooks } from "./inbox";
import { ingestBatch, markIgnored, readReceiveOffset } from "./inbox";
import type { SchemaVersionState } from "./migrations";
import { checkSchemaVersion, runMigrations } from "./migrations";
import {
  OwnershipLostError,
  acquireOwnership,
  releaseOwnership,
  renewLease,
} from "./ownership";
import type { TelegramApi } from "./telegram-api";
import {
  isFatalTelegramAuthError,
  isTelegramRateLimitError,
} from "./telegram-api";
import type { Logger, OwnershipLease, TelegramUpdate } from "./types";
import { errorMessage, noopLogger, nowMs } from "./utils";

/**
 * Long-poll ingress controller, modeled on discordbot's gateway controller:
 * transport only, crash-to-restart on configuration faults, injectable
 * `onFatalEnd` for tests. Telegram delta: discord.js owns reconnection and the
 * Gateway enforces one session per token server-side, so gateway.ts only
 * monitors a connection; `getUpdates` has neither, so this controller also
 * owns the startup sequence (migrations -> schema check -> getMe -> fenced
 * ownership -> deleteWebhook) and the fenced receipt loop. Message processing
 * lives elsewhere — ingested batches are handed to `onUpdatesIngested` and
 * must never gate the next poll.
 */

const DEFAULT_LEASE_TTL_MS = 30_000;
const DEFAULT_POLL_TIMEOUT_SECONDS = 50;
const DEFAULT_BACKOFF_BASE_MS = 500;
const DEFAULT_BACKOFF_MAX_MS = 30_000;
const DEFAULT_HEARTBEAT_INTERVAL_MS = 1_000;
const DEFAULT_HEARTBEAT_STALE_MS = 10_000;

/** Only `message` is requested; everything else is filtered server-side. Any
 * other update kind Telegram still queues is durably marked ignored during
 * ingest (see settleIngested). */
const ALLOWED_UPDATES = ["message"];

export type PollerStatus = {
  /** Event-loop heartbeat via a monotonic timer — a wedged loop goes false. */
  live: boolean;
  /** Migrations + schema + identity + ownership + webhook + poll freshness. */
  ready: boolean;
  reasons: string[];
  lastSuccessfulPollAtMs: number | null;
};

export type IngestedUpdates = {
  botUserId: string;
  lease: OwnershipLease;
  /** Message-bearing updates only; non-message updates in the batch were
   * already durably marked ignored before this callback fires. */
  updates: TelegramUpdate[];
};

export type PollerControllerDeps = {
  api: TelegramApi;
  pool: Pool;
  logger?: Logger;
  /** Stable per-process holder id for the fenced lease (default random UUID —
   * a restart is deliberately a new holder so takeover bumps the generation). */
  holderId?: string;
  leaseTtlMs?: number;
  pollTimeoutSeconds?: number;
  /** Readiness freshness window; must exceed the long-poll timeout so one
   * quiet-but-healthy poll cannot read as staleness (default max(90s, poll
   * timeout + 30s)). */
  freshnessWindowMs?: number;
  heartbeatIntervalMs?: number;
  heartbeatStaleMs?: number;
  backoff?: { baseMs?: number; maxMs?: number };
  /** Dispatch nudge for the worker layer. Fire-and-forget: rendering and
   * processing must never gate the next poll, and a failed callback is safe
   * because the rows are already durable and recovery scans re-serve them. */
  onUpdatesIngested?: (batch: IngestedUpdates) => void | Promise<void>;
  /** Test override, mirrors discordbot gateway.ts. Defaults to process.exit(1)
   * so k8s restarts and surfaces the configuration fault. */
  onFatalEnd?: () => void;
  /** Fault-injection seam for receipt-atomicity tests. Never set in production. */
  ingestHooks?: IngestHooks;
};

export type PollerController = {
  /** Launches the startup/poll loop in the background and returns; readiness
   * (with reasons) is observable via status() while startup retries. */
  start(): void;
  status(): PollerStatus;
  /** Current fenced identity while ownership is held, for worker claims and
   * render recovery; null whenever the lease cannot be proven. */
  ownership(): { botUserId: string; lease: OwnershipLease } | null;
  /** Aborts an in-flight getUpdates, stops timers, releases the lease
   * best-effort. */
  shutdown(): Promise<void>;
};

type PollExit = "stopped" | "fatal" | "ownership_lost" | "transient";

export function createPollerController(
  deps: PollerControllerDeps,
): PollerController {
  const { api, pool } = deps;
  const log = deps.logger ?? noopLogger;
  const holderId = deps.holderId ?? crypto.randomUUID();
  const leaseTtlMs = deps.leaseTtlMs ?? DEFAULT_LEASE_TTL_MS;
  const pollTimeoutSeconds =
    deps.pollTimeoutSeconds ?? DEFAULT_POLL_TIMEOUT_SECONDS;
  const freshnessWindowMs =
    deps.freshnessWindowMs ??
    Math.max(90_000, (pollTimeoutSeconds + 30) * 1000);
  const heartbeatIntervalMs =
    deps.heartbeatIntervalMs ?? DEFAULT_HEARTBEAT_INTERVAL_MS;
  const heartbeatStaleMs = deps.heartbeatStaleMs ?? DEFAULT_HEARTBEAT_STALE_MS;
  const backoffBaseMs = deps.backoff?.baseMs ?? DEFAULT_BACKOFF_BASE_MS;
  const backoffMaxMs = deps.backoff?.maxMs ?? DEFAULT_BACKOFF_MAX_MS;
  const onFatalEnd = deps.onFatalEnd ?? (() => process.exit(1));
  // Client-side watchdog for a poll whose response never arrives (network
  // black hole): telegram-api only applies its own timeout when no signal is
  // passed, and the controller always passes one for shutdown/loss aborts.
  const pollWatchdogMs = (pollTimeoutSeconds + 20) * 1000;

  const stop = new AbortController();
  let started = false;
  let fatal = false;
  let runPromise: Promise<void> | undefined;

  let heartbeatTimer: ReturnType<typeof setInterval> | undefined;
  let renewTimer: ReturnType<typeof setInterval> | undefined;
  let currentPollAbort: AbortController | null = null;

  let lastHeartbeatMono = nowMs();
  let lastPollOkMono: number | null = null;
  let lastSuccessfulPollAtMs: number | null = null;
  let pollLoopStartedMono: number | null = null;

  // Ownership terms: each pollLoop entry is a new term so an in-flight lease
  // renewal from a previous term (still awaiting Postgres when the loop moved
  // on) can never mark the CURRENT term lost or abort its poll.
  let term = 0;
  let ownershipLostThisTerm = false;

  const readiness = {
    database: false,
    schema: false,
    identity: false,
    ownership: false,
    webhook: false,
  };
  let schemaState: SchemaVersionState | null = null;
  let botUserId: string | null = null;
  let lease: OwnershipLease | null = null;

  function stopRenewTimer(): void {
    if (renewTimer !== undefined) {
      clearInterval(renewTimer);
      renewTimer = undefined;
    }
  }

  function fatalExit(stage: string, error: unknown): void {
    fatal = true;
    // TelegramApiError messages are rebuilt from method + description and can
    // never carry the token-bearing request URL (see telegram-api.ts).
    log.error("telegrambot_fatal_configuration_error", {
      stage,
      error: errorMessage(error),
    });
    stopRenewTimer();
    currentPollAbort?.abort();
    onFatalEnd();
  }

  function handleOwnershipLoss(myTerm: number, reason: string): void {
    if (myTerm !== term || ownershipLostThisTerm) return;
    ownershipLostThisTerm = true;
    readiness.ownership = false;
    // deleteWebhook is an owner-only action: the next owner (possibly this
    // process, next term) must re-reconcile before polling again.
    readiness.webhook = false;
    stopRenewTimer();
    currentPollAbort?.abort();
    log.warn("telegrambot_ownership_lost_polling_stopped", {
      reason,
      holder_id: holderId,
    });
  }

  function markPollSuccess(): void {
    lastPollOkMono = nowMs();
    lastSuccessfulPollAtMs = Date.now();
  }

  /**
   * Renewal cadence at ~TTL/3 on its own timer: renewal must not depend on
   * poll latency, since a healthy long poll legitimately blocks longer than
   * the lease TTL. renewLease folds errors into `false` — an uncertain
   * renewal is a lost renewal.
   */
  function startRenewTimer(activeLease: OwnershipLease, myTerm: number): void {
    stopRenewTimer();
    const cadenceMs = Math.max(50, Math.floor(leaseTtlMs / 3));
    let renewing = false;
    renewTimer = setInterval(() => {
      if (renewing || stop.signal.aborted || ownershipLostThisTerm) return;
      renewing = true;
      void renewLease(pool, activeLease, leaseTtlMs, log)
        .then((renewed) => {
          if (!renewed) handleOwnershipLoss(myTerm, "renewal_failed");
        })
        .finally(() => {
          renewing = false;
        });
    }, cadenceMs);
    renewTimer.unref?.();
  }

  /**
   * Spec-mandated startup order: database (migrations + schema gate) ->
   * getMe (the bot user id scopes all durable state; never the token) ->
   * fenced ownership -> deleteWebhook(drop_pending_updates: false) as an
   * idempotent owner-only action. Any failure leaves readiness flags exactly
   * where they failed (reasons surface via status()) and the run loop retries
   * with backoff — receive_offset is never touched here.
   */
  async function startupOnce(): Promise<{
    lease: OwnershipLease;
    botUserId: string;
  } | null> {
    try {
      await runMigrations(pool, log);
      readiness.database = true;
    } catch (error) {
      readiness.database = false;
      log.warn("telegrambot_database_init_failed", {
        error: errorMessage(error),
      });
      return null;
    }

    try {
      const check = await checkSchemaVersion(pool);
      schemaState = check.state;
      readiness.schema = check.ok;
      if (!check.ok) {
        log.warn("telegrambot_schema_version_incompatible", {
          state: check.state,
          db_version: check.dbVersion,
          supported_version: check.supportedVersion,
        });
        return null;
      }
    } catch (error) {
      readiness.schema = false;
      log.warn("telegrambot_schema_check_failed", {
        error: errorMessage(error),
      });
      return null;
    }

    try {
      const me = await api.getMe();
      botUserId = String(me.id);
      readiness.identity = true;
    } catch (error) {
      readiness.identity = false;
      if (isFatalTelegramAuthError(error)) {
        fatalExit("get_me", error);
        return null;
      }
      log.warn("telegrambot_get_me_failed", { error: errorMessage(error) });
      return null;
    }

    try {
      const acquired = await acquireOwnership(
        pool,
        botUserId,
        holderId,
        leaseTtlMs,
        log,
      );
      if (!acquired) {
        readiness.ownership = false;
        log.info("telegrambot_ownership_unavailable", {
          bot_user_id: botUserId,
          holder_id: holderId,
        });
        return null;
      }
      lease = acquired;
      readiness.ownership = true;
    } catch (error) {
      readiness.ownership = false;
      log.warn("telegrambot_ownership_acquire_failed", {
        error: errorMessage(error),
      });
      return null;
    }

    try {
      // Never drop pending updates: the durable inbox decides what to do with
      // the backlog, deployment transitions must not.
      await api.deleteWebhook({ drop_pending_updates: false });
      readiness.webhook = true;
      log.info("telegrambot_webhook_reconciled", { bot_user_id: botUserId });
    } catch (error) {
      readiness.webhook = false;
      if (isFatalTelegramAuthError(error)) {
        fatalExit("delete_webhook", error);
        return null;
      }
      log.warn("telegrambot_delete_webhook_failed", {
        error: errorMessage(error),
      });
      return null;
    }

    return { lease, botUserId };
  }

  /**
   * Durable dispositions for non-message updates happen inside the same
   * ingest flow so an unexpected update kind (channel_post etc.) can never
   * stall processing, then message-bearing updates are handed to the dispatch
   * callback fire-and-forget. A markIgnored failure after the batch committed
   * is recoverable: the row is already durable in `received` and the worker
   * claim scan re-serves it, so only ownership loss is escalated.
   */
  async function settleIngested(
    activeLease: OwnershipLease,
    activeBotUserId: string,
    updates: readonly TelegramUpdate[],
  ): Promise<void> {
    const dispatchable: TelegramUpdate[] = [];
    for (const update of updates) {
      if (update.message) {
        dispatchable.push(update);
        continue;
      }
      await markIgnored(
        pool,
        activeLease,
        update.update_id,
        `unsupported_update_type:${updateKind(update)}`,
        log,
      );
    }
    if (dispatchable.length === 0 || !deps.onUpdatesIngested) return;
    const callback = deps.onUpdatesIngested;
    void Promise.resolve()
      .then(() =>
        callback({
          botUserId: activeBotUserId,
          lease: activeLease,
          updates: dispatchable,
        }),
      )
      .catch((error) => {
        log.warn("telegrambot_dispatch_callback_failed", {
          error: errorMessage(error),
        });
      });
  }

  async function pollLoop(
    activeLease: OwnershipLease,
    activeBotUserId: string,
  ): Promise<PollExit> {
    term += 1;
    const myTerm = term;
    ownershipLostThisTerm = false;
    pollLoopStartedMono = nowMs();
    startRenewTimer(activeLease, myTerm);

    // Only the committed cursor may feed a poll. Null on first start: call
    // getUpdates WITHOUT an offset so the earliest pending updates are
    // ingested — never jump to the queue head, never drop the backlog.
    let committedOffset: number | null;
    try {
      committedOffset = await readReceiveOffset(pool, activeBotUserId);
    } catch (error) {
      log.warn("telegrambot_receive_offset_read_failed", {
        error: errorMessage(error),
      });
      return "transient";
    }

    const backoff = createBackoff(backoffBaseMs, backoffMaxMs);
    while (true) {
      if (stop.signal.aborted) return "stopped";
      if (ownershipLostThisTerm) return "ownership_lost";
      const pollAbort = new AbortController();
      currentPollAbort = pollAbort;
      try {
        const updates = await api.getUpdates(
          {
            ...(committedOffset !== null ? { offset: committedOffset } : {}),
            timeout: pollTimeoutSeconds,
            allowed_updates: [...ALLOWED_UPDATES],
          },
          AbortSignal.any([
            pollAbort.signal,
            AbortSignal.timeout(pollWatchdogMs),
          ]),
        );
        currentPollAbort = null;
        if (stop.signal.aborted) return "stopped";
        if (ownershipLostThisTerm) return "ownership_lost";
        // Atomic receipt: the whole batch and the cursor commit together, and
        // the empty-batch path still proves the lease + refreshes freshness.
        // A batch that fails to ingest durably is NOT confirmed — the catch
        // below leaves committedOffset unchanged and Telegram redelivers.
        committedOffset = await ingestBatch(
          pool,
          activeLease,
          updates,
          log,
          deps.ingestHooks,
        );
        markPollSuccess();
        backoff.reset();
        if (updates.length > 0) {
          await settleIngested(activeLease, activeBotUserId, updates);
        }
      } catch (error) {
        currentPollAbort = null;
        if (stop.signal.aborted) return "stopped";
        if (error instanceof OwnershipLostError) {
          handleOwnershipLoss(myTerm, "fenced_statement");
          return "ownership_lost";
        }
        if (ownershipLostThisTerm) return "ownership_lost";
        if (isFatalTelegramAuthError(error)) {
          fatalExit("get_updates", error);
          return "fatal";
        }
        const delayMs = retryDelay(error, backoff);
        log.warn("telegrambot_poll_retry", {
          error: errorMessage(error),
          retry_in_ms: delayMs,
        });
        await sleep(delayMs, stop.signal);
      }
    }
  }

  async function run(): Promise<void> {
    const startupBackoff = createBackoff(backoffBaseMs, backoffMaxMs);
    while (!stop.signal.aborted && !fatal) {
      const owned = await startupOnce();
      if (stop.signal.aborted || fatal) return;
      if (!owned) {
        await sleep(startupBackoff.next(), stop.signal);
        continue;
      }
      startupBackoff.reset();
      log.info("telegrambot_poller_started", {
        bot_user_id: owned.botUserId,
        holder_id: holderId,
      });
      const exit = await pollLoop(owned.lease, owned.botUserId);
      stopRenewTimer();
      if (exit === "stopped" || exit === "fatal") return;
      if (exit === "ownership_lost") {
        // Reacquire only after the old lease term has fully lapsed: loss was
        // detected at most one TTL before expiry, so a full-TTL wait
        // guarantees no fenced write from this holder's old term can race the
        // successor's takeover.
        await sleep(leaseTtlMs, stop.signal);
      } else {
        await sleep(startupBackoff.next(), stop.signal);
      }
    }
  }

  function status(): PollerStatus {
    const now = nowMs();
    const live = !started || now - lastHeartbeatMono <= heartbeatStaleMs;
    const reasons: string[] = [];
    if (!started) reasons.push("not_started");
    if (stop.signal.aborted) reasons.push("shutting_down");
    if (fatal) reasons.push("fatal_configuration_error");
    if (!readiness.database) reasons.push("database_not_ready");
    if (readiness.database && !readiness.schema) {
      reasons.push(
        schemaState === "unsupported_future"
          ? "schema_version_unsupported"
          : "schema_version_pending",
      );
    }
    if (!readiness.identity) reasons.push("bot_identity_unresolved");
    if (!readiness.ownership) reasons.push("ownership_not_held");
    if (!readiness.webhook) reasons.push("webhook_not_reconciled");
    // Freshness anchors on poll-loop start until the first success so a quiet
    // first long poll is not misread as staleness; the window exceeds the
    // long-poll timeout, so a healthy poll always lands inside it.
    const anchor = lastPollOkMono ?? pollLoopStartedMono;
    if (anchor === null) reasons.push("poll_never_started");
    else if (now - anchor > freshnessWindowMs) reasons.push("poll_stale");
    return {
      live,
      ready: reasons.length === 0,
      reasons,
      lastSuccessfulPollAtMs,
    };
  }

  return {
    start(): void {
      if (started) return;
      started = true;
      lastHeartbeatMono = nowMs();
      heartbeatTimer = setInterval(() => {
        lastHeartbeatMono = nowMs();
      }, heartbeatIntervalMs);
      heartbeatTimer.unref?.();
      runPromise = run().catch((error) => {
        // Mirrors gateway.ts: the loop ending on its own is always a bug or a
        // configuration fault — exit so k8s restarts with backoff.
        log.error("telegrambot_poller_ended_unexpectedly", {
          error: errorMessage(error),
        });
        fatal = true;
        onFatalEnd();
      });
    },

    status,

    ownership() {
      return lease && botUserId && readiness.ownership && !fatal
        ? { botUserId, lease }
        : null;
    },

    async shutdown(): Promise<void> {
      stop.abort();
      stopRenewTimer();
      if (heartbeatTimer !== undefined) clearInterval(heartbeatTimer);
      currentPollAbort?.abort();
      if (runPromise) await runPromise;
      const heldLease = lease;
      // Best-effort: expire our own lease so a successor starts immediately.
      // releaseOwnership is holder+generation guarded, so releasing a lease
      // we already lost is a harmless no-op.
      if (heldLease && readiness.ownership) {
        await releaseOwnership(pool, heldLease, log);
      }
    },
  };
}

function updateKind(update: TelegramUpdate): string {
  for (const key of Object.keys(update)) {
    if (key !== "update_id") return key;
  }
  return "unknown";
}

type Backoff = { next(): number; reset(): void };

/** Exponential backoff with jitter (half fixed, half random) so a fleet of
 * retries against a recovering dependency does not synchronize. */
function createBackoff(baseMs: number, maxMs: number): Backoff {
  let attempt = 0;
  return {
    next(): number {
      const capped = Math.min(maxMs, baseMs * 2 ** attempt);
      attempt = Math.min(attempt + 1, 20);
      return Math.round(capped / 2 + Math.random() * (capped / 2));
    },
    reset(): void {
      attempt = 0;
    },
  };
}

function retryDelay(error: unknown, backoff: Backoff): number {
  const backoffMs = backoff.next();
  // 429 retry_after is authoritative: waiting less just burns the budget.
  if (isTelegramRateLimitError(error) && error.retryAfterSeconds !== undefined) {
    return Math.max(error.retryAfterSeconds * 1000, backoffMs);
  }
  return backoffMs;
}

/** Abort-aware sleep so shutdown never waits out a backoff or lease term. */
function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted || ms <= 0) {
      resolve();
      return;
    }
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}
