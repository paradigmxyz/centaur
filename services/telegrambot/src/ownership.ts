import type { Pool } from "pg";
import type { Logger, OwnershipLease } from "./types";
import { errorMessage, noopLogger } from "./utils";

/**
 * Fenced ownership per spec §2 option (b): a lease row in telegram_poll_state
 * keyed by bot_user_id with holder_id, a monotonically increasing generation,
 * and a database-time expiry. Every cursor update, inbox claim, and stage
 * transition must match (holder_id, generation, lease_expires_at > now()) in
 * the same statement (see fenceSql), so a paused/partitioned old owner cannot
 * mutate durable state after a takeover — `replicas: 1` is rollout hygiene,
 * not the correctness mechanism.
 *
 * Telegram delta: discordbot leans on the Discord Gateway's own
 * one-session-per-token semantics; getUpdates has no such server-side session,
 * so the fence lives entirely in Postgres. All expiry math uses database
 * now() exclusively — client clocks never participate, so clock skew between
 * pods cannot open a split-brain window.
 */

/** Thrown when a fenced statement proves the lease is no longer ours. */
export class OwnershipLostError extends Error {
  readonly botUserId: string;
  readonly holderId: string;
  readonly generation: number;

  constructor(lease: OwnershipLease, operation: string) {
    super(
      `ownership lost during ${operation} (holder ${lease.holderId}, generation ${lease.generation})`,
    );
    this.name = "OwnershipLostError";
    this.botUserId = lease.botUserId;
    this.holderId = lease.holderId;
    this.generation = lease.generation;
  }
}

/**
 * Fence fragment for statements on OTHER tables (inbox transitions/claims):
 * an EXISTS guard against the lease row using the caller's placeholder
 * positions for (bot_user_id, holder_id, generation). Statements on
 * telegram_poll_state itself put the same predicates directly in their WHERE
 * clause instead.
 */
export function fenceSql(
  botUserIdParam: number,
  holderIdParam: number,
  generationParam: number,
): string {
  return `EXISTS (
    SELECT 1 FROM telegram_poll_state AS fence
    WHERE fence.bot_user_id = $${botUserIdParam}
      AND fence.holder_id = $${holderIdParam}
      AND fence.generation = $${generationParam}
      AND fence.lease_expires_at > now()
  )`;
}

/**
 * Take the lease iff it is expired or already ours, in one statement so two
 * candidates cannot both win. Generation increments ONLY on takeover (holder
 * change); a same-holder re-acquire extends the lease without bumping, because
 * holder ids are unique per process instance and the process's own in-flight
 * fenced work stays valid. Returns null when someone else holds an unexpired
 * lease.
 */
export async function acquireOwnership(
  pool: Pool,
  botUserId: string,
  holderId: string,
  ttlMs: number,
  logger?: Logger,
): Promise<OwnershipLease | null> {
  const log = logger ?? noopLogger;
  const result = await pool.query<{ generation: string }>(
    `
    INSERT INTO telegram_poll_state (bot_user_id, holder_id, generation, lease_expires_at)
    VALUES ($1, $2, 1, now() + ($3::bigint * interval '1 millisecond'))
    ON CONFLICT (bot_user_id) DO UPDATE SET
      holder_id = EXCLUDED.holder_id,
      generation = CASE
        WHEN telegram_poll_state.holder_id IS NOT DISTINCT FROM EXCLUDED.holder_id
          THEN telegram_poll_state.generation
        ELSE telegram_poll_state.generation + 1
      END,
      lease_expires_at = now() + ($3::bigint * interval '1 millisecond')
    WHERE telegram_poll_state.holder_id IS NOT DISTINCT FROM EXCLUDED.holder_id
       OR telegram_poll_state.lease_expires_at IS NULL
       OR telegram_poll_state.lease_expires_at <= now()
    RETURNING generation
    `,
    [botUserId, holderId, ttlMs],
  );

  const row = result.rows[0];
  if (!row) return null;

  const lease: OwnershipLease = {
    botUserId,
    holderId,
    generation: Number(row.generation),
  };
  log.info("telegrambot_ownership_acquired", {
    bot_user_id: botUserId,
    holder_id: holderId,
    generation: lease.generation,
  });
  return lease;
}

/**
 * Extends the lease only when it is provably still ours AND unexpired.
 * Requiring unexpired is deliberately stricter than holder+generation alone:
 * once the lease has lapsed, fenced statements are already failing, so
 * resurrecting the same generation would make ownership loss non-monotonic.
 * Errors are folded into `false` — an uncertain renewal is a lost renewal;
 * the caller must stop initiating work.
 */
export async function renewLease(
  pool: Pool,
  lease: OwnershipLease,
  ttlMs: number,
  logger?: Logger,
): Promise<boolean> {
  const log = logger ?? noopLogger;
  try {
    const result = await pool.query(
      `
      UPDATE telegram_poll_state
      SET lease_expires_at = now() + ($4::bigint * interval '1 millisecond')
      WHERE bot_user_id = $1
        AND holder_id = $2
        AND generation = $3
        AND lease_expires_at > now()
      `,
      [lease.botUserId, lease.holderId, lease.generation, ttlMs],
    );
    const renewed = (result.rowCount ?? 0) === 1;
    if (!renewed) {
      log.warn("telegrambot_ownership_lost", {
        bot_user_id: lease.botUserId,
        holder_id: lease.holderId,
        generation: lease.generation,
        reason: "renewal_fenced_out",
      });
    }
    return renewed;
  } catch (error) {
    log.warn("telegrambot_ownership_renew_uncertain", {
      bot_user_id: lease.botUserId,
      holder_id: lease.holderId,
      generation: lease.generation,
      error: errorMessage(error),
    });
    return false;
  }
}

/**
 * Best-effort shutdown: expire our own lease immediately so a successor can
 * acquire without waiting out the TTL. Holder/generation stay in place —
 * generation must only ever move forward, and the successor's takeover is
 * what increments it. Failures are logged and swallowed; the TTL is the
 * backstop.
 */
export async function releaseOwnership(
  pool: Pool,
  lease: OwnershipLease,
  logger?: Logger,
): Promise<void> {
  const log = logger ?? noopLogger;
  try {
    const result = await pool.query(
      `
      UPDATE telegram_poll_state
      SET lease_expires_at = now()
      WHERE bot_user_id = $1 AND holder_id = $2 AND generation = $3
      `,
      [lease.botUserId, lease.holderId, lease.generation],
    );
    log.info("telegrambot_ownership_released", {
      bot_user_id: lease.botUserId,
      holder_id: lease.holderId,
      generation: lease.generation,
      released: (result.rowCount ?? 0) === 1,
    });
  } catch (error) {
    log.warn("telegrambot_ownership_release_failed", {
      bot_user_id: lease.botUserId,
      error: errorMessage(error),
    });
  }
}
