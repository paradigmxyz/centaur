import type { Pool, PoolClient } from "pg";
import { withTransaction } from "./db";
import { OwnershipLostError, fenceSql } from "./ownership";
import type {
  Logger,
  OwnershipLease,
  TelegramInboxRow,
  TelegramInboxStatus,
  TelegramRenderObligation,
  TelegramUpdate,
} from "./types";
import { TERMINAL_INBOX_STATUSES } from "./types";
import { noopLogger } from "./utils";

/**
 * Durable Telegram inbox (spec §2 / §2a). Receipt is separate from
 * processing: ingestBatch persists a whole getUpdates batch and advances
 * receive_offset in one explicit transaction, and only that committed cursor
 * feeds the next poll. Processing stages then move idempotently through
 * fenced compare-and-set transitions; a redelivered update never regresses
 * its stage and a fenced-out old owner can neither move the cursor nor touch
 * a row.
 */

/**
 * Telegram delta: types.ts's TelegramInboxRow omits render_obligation (the
 * shared row shape predates the renderer wiring), so the inbox exposes rows
 * with the parsed obligation attached.
 */
export type TelegramInboxRecord = TelegramInboxRow & {
  renderObligation: TelegramRenderObligation | null;
};

export type TransitionPatch = {
  executionId?: string;
  renderObligation?: TelegramRenderObligation;
  statusReason?: string;
};

/** Fault-injection seam for tests proving receipt atomicity (crash between
 * inbox insert and cursor advance must roll back both). Never set in
 * production paths. */
export type IngestHooks = {
  beforeCursorAdvance?: (client: PoolClient) => void | Promise<void>;
};

const TERMINAL_STATUS_ARRAY = [...TERMINAL_INBOX_STATUSES];

/** Terminal statuses that must carry a durable reason (spec: ignored /
 * rejected / failed are terminal only after a reason is recorded). */
const REASON_REQUIRED_STATUSES: readonly TelegramInboxStatus[] = [
  "ignored",
  "rejected",
  "failed",
];

/**
 * Upsert the ENTIRE returned getUpdates batch and advance receive_offset to
 * max(update_id) + 1 in ONE transaction. ON CONFLICT DO NOTHING keeps the
 * existing stage of redelivered updates; the cursor advance is fenced and
 * uses GREATEST so a redelivered older batch can never regress the committed
 * cursor. Any failure (including ownership loss) rolls back both the inserts
 * and the cursor. Update ids are never assumed contiguous.
 *
 * Returns the committed cursor: max(update_id)+1 for a non-empty batch, the
 * current committed cursor (possibly null on first start) for an empty one.
 */
export async function ingestBatch(
  pool: Pool,
  lease: OwnershipLease,
  updates: readonly TelegramUpdate[],
  logger?: Logger,
  hooks?: IngestHooks,
): Promise<number | null> {
  const log = logger ?? noopLogger;

  if (updates.length === 0) {
    // Nothing to persist, but an empty poll must still prove the lease: a
    // fenced-out owner has no business learning "its" cursor and polling on.
    const result = await pool.query<{ receive_offset: string | null }>(
      `
      SELECT receive_offset FROM telegram_poll_state
      WHERE bot_user_id = $1 AND holder_id = $2 AND generation = $3
        AND lease_expires_at > now()
      `,
      [lease.botUserId, lease.holderId, lease.generation],
    );
    const row = result.rows[0];
    if (!row) throw new OwnershipLostError(lease, "ingest_batch");
    return row.receive_offset === null ? null : Number(row.receive_offset);
  }

  const maxUpdateId = Math.max(...updates.map((update) => update.update_id));
  const newOffset = maxUpdateId + 1;

  const committedOffset = await withTransaction(pool, async (client) => {
    await client.query(
      `
      INSERT INTO telegram_update_inbox (bot_user_id, update_id, payload)
      SELECT $1, (elem->>'update_id')::bigint, elem
      FROM jsonb_array_elements($2::jsonb) AS elem
      ON CONFLICT (bot_user_id, update_id) DO NOTHING
      `,
      [lease.botUserId, JSON.stringify(updates)],
    );

    await hooks?.beforeCursorAdvance?.(client);

    const cursor = await client.query<{ receive_offset: string }>(
      `
      UPDATE telegram_poll_state
      SET receive_offset = GREATEST(coalesce(receive_offset, $4::bigint), $4::bigint)
      WHERE bot_user_id = $1 AND holder_id = $2 AND generation = $3
        AND lease_expires_at > now()
      RETURNING receive_offset
      `,
      [lease.botUserId, lease.holderId, lease.generation, newOffset],
    );
    const row = cursor.rows[0];
    if (!row) throw new OwnershipLostError(lease, "ingest_batch");
    return Number(row.receive_offset);
  });

  log.info("telegrambot_inbox_ingested", {
    bot_user_id: lease.botUserId,
    count: updates.length,
    new_offset: committedOffset,
  });
  return committedOffset;
}

/**
 * Committed cursor for the next poll. Null on first start, in which case
 * getUpdates is called WITHOUT an offset so the earliest pending updates are
 * ingested (never jump to the queue head). Read-only, so unfenced — the fence
 * protects the writes; a stale read only produces a poll whose ingest will be
 * fenced out.
 */
export async function readReceiveOffset(
  pool: Pool,
  botUserId: string,
): Promise<number | null> {
  const result = await pool.query<{ receive_offset: string | null }>(
    "SELECT receive_offset FROM telegram_poll_state WHERE bot_user_id = $1",
    [botUserId],
  );
  const offset = result.rows[0]?.receive_offset;
  return offset === null || offset === undefined ? null : Number(offset);
}

export async function markIgnored(
  pool: Pool,
  lease: OwnershipLease,
  updateId: number,
  reason: string,
  logger?: Logger,
): Promise<boolean> {
  return transition(pool, lease, updateId, NONTERMINAL_STATUSES, "ignored", {
    statusReason: reason,
  }, logger);
}

export async function markRejected(
  pool: Pool,
  lease: OwnershipLease,
  updateId: number,
  reason: string,
  logger?: Logger,
): Promise<boolean> {
  return transition(pool, lease, updateId, NONTERMINAL_STATUSES, "rejected", {
    statusReason: reason,
  }, logger);
}

const NONTERMINAL_STATUSES: readonly TelegramInboxStatus[] = [
  "received",
  "message_appended",
  "steering_pending",
  "execution_accepted",
  "render_obligation_persisted",
];

/**
 * Stamp acceptance metadata (typed thread key + stable client_message_id) on
 * a received row. Status deliberately stays `received`: acceptance is
 * metadata for FIFO grouping and idempotent append keys, not a processing
 * stage — the append transition is what moves the row forward.
 */
export async function acceptForProcessing(
  pool: Pool,
  lease: OwnershipLease,
  updateId: number,
  threadKey: string,
  clientMessageId: string,
): Promise<boolean> {
  return withTransaction(pool, async (client) => {
    const result = await client.query(
      `
      UPDATE telegram_update_inbox
      SET thread_key = $5, client_message_id = $6, updated_at = now()
      WHERE bot_user_id = $1 AND update_id = $4 AND status = 'received'
        AND ${fenceSql(1, 2, 3)}
      `,
      [
        lease.botUserId,
        lease.holderId,
        lease.generation,
        updateId,
        threadKey,
        clientMessageId,
      ],
    );
    return (result.rowCount ?? 0) === 1;
  });
}

/**
 * Fenced compare-and-set stage transition. Returns false when the row is not
 * in one of `fromStatuses` (a concurrent/older transition already happened —
 * idempotent recovery treats that as "someone got there first") or when the
 * fence no longer holds; either way the caller must not assume the write
 * landed. Patch fields coalesce onto existing values so a retried transition
 * never erases earlier durable metadata.
 */
export async function transition(
  pool: Pool,
  lease: OwnershipLease,
  updateId: number,
  fromStatuses: readonly TelegramInboxStatus[],
  toStatus: TelegramInboxStatus,
  patch: TransitionPatch = {},
  logger?: Logger,
): Promise<boolean> {
  const log = logger ?? noopLogger;
  if (
    REASON_REQUIRED_STATUSES.includes(toStatus) &&
    !patch.statusReason?.trim()
  ) {
    throw new Error(
      `inbox status '${toStatus}' is terminal and requires a durable reason`,
    );
  }
  if (fromStatuses.length === 0) return false;

  const moved = await withTransaction(pool, async (client) => {
    const result = await client.query(
      `
      UPDATE telegram_update_inbox
      SET status = $5,
          execution_id = coalesce($6, execution_id),
          render_obligation = coalesce($7::jsonb, render_obligation),
          status_reason = coalesce($8, status_reason),
          updated_at = now()
      WHERE bot_user_id = $1 AND update_id = $4 AND status = ANY($9::text[])
        AND ${fenceSql(1, 2, 3)}
      `,
      [
        lease.botUserId,
        lease.holderId,
        lease.generation,
        updateId,
        toStatus,
        patch.executionId ?? null,
        patch.renderObligation ? JSON.stringify(patch.renderObligation) : null,
        patch.statusReason ?? null,
        [...fromStatuses],
      ],
    );
    return (result.rowCount ?? 0) === 1;
  });

  if (moved) {
    log.info("telegrambot_inbox_transition", {
      bot_user_id: lease.botUserId,
      update_id: updateId,
      to_status: toStatus,
      ...(patch.statusReason ? { status_reason: patch.statusReason } : {}),
    });
  }
  return moved;
}

/**
 * Recovery/dispatch scan: the OLDEST nonterminal row per thread_key (FIFO
 * within a thread), at most one per thread, up to `limit` threads, skipping
 * threads already claimed in-process. Rows not yet stamped with a thread_key
 * (status `received`, pre-acceptance) form a single NULL group so at most one
 * unstamped row — the oldest — is handed out per scan; its true thread is
 * unknown until acceptance, so dispatching several concurrently could break
 * same-thread FIFO.
 *
 * Plain SELECT by design: workers are in-process and the fence protects the
 * transitions, not the read — a stale claim simply loses its CAS.
 */
export async function claimNextPerThread(
  pool: Pool,
  botUserId: string,
  excludeThreadKeys: readonly string[],
  limit: number,
): Promise<TelegramInboxRecord[]> {
  if (limit <= 0) return [];
  const result = await pool.query<InboxDbRow>(
    `
    SELECT * FROM (
      SELECT DISTINCT ON (thread_key) *
      FROM telegram_update_inbox
      WHERE bot_user_id = $1
        AND NOT (status = ANY($2::text[]))
        AND (thread_key IS NULL OR NOT (thread_key = ANY($3::text[])))
      ORDER BY thread_key, update_id ASC
    ) AS oldest_per_thread
    ORDER BY update_id ASC
    LIMIT $4
    `,
    [botUserId, TERMINAL_STATUS_ARRAY, [...excludeThreadKeys], limit],
  );
  return result.rows.map(mapRow);
}

/**
 * Retention: delete terminal rows that are BOTH below the committed
 * receive_offset (Telegram has confirmed them; they can never be redelivered
 * into a fresh stage) and older than the retention window. The join against
 * the lease row is the fence — a fenced-out owner deletes nothing. Never
 * touches telegram_poll_state, so the receipt cursor survives pruning.
 */
export async function pruneTerminal(
  pool: Pool,
  lease: OwnershipLease,
  retentionHours: number,
  logger?: Logger,
): Promise<number> {
  const log = logger ?? noopLogger;
  const result = await pool.query(
    `
    DELETE FROM telegram_update_inbox AS inbox
    USING telegram_poll_state AS fence
    WHERE inbox.bot_user_id = $1
      AND fence.bot_user_id = $1
      AND fence.holder_id = $2
      AND fence.generation = $3
      AND fence.lease_expires_at > now()
      AND inbox.status = ANY($4::text[])
      AND inbox.updated_at < now() - ($5::double precision * interval '1 hour')
      AND fence.receive_offset IS NOT NULL
      AND inbox.update_id < fence.receive_offset
    `,
    [
      lease.botUserId,
      lease.holderId,
      lease.generation,
      TERMINAL_STATUS_ARRAY,
      retentionHours,
    ],
  );
  const deleted = result.rowCount ?? 0;
  if (deleted > 0) {
    log.info("telegrambot_inbox_pruned", {
      bot_user_id: lease.botUserId,
      deleted,
      retention_hours: retentionHours,
    });
  }
  return deleted;
}

/**
 * Per-thread worker feed: the oldest nonterminal rows for ONE stamped
 * thread_key, oldest first, skipping update ids already owned by in-process
 * work (a detached render or steering wait). claimNextPerThread only exposes
 * one row per thread, which is right for cross-thread scheduling but starves
 * live steering — a follow-up must be appendable while the thread's oldest row
 * is still rendering in the background — so the thread worker drains its own
 * backlog through this query instead.
 *
 * Plain SELECT for the same reason as claimNextPerThread: the fence protects
 * transitions, not reads.
 */
export async function claimThreadBacklog(
  pool: Pool,
  botUserId: string,
  threadKey: string,
  excludeUpdateIds: readonly number[],
  limit: number,
): Promise<TelegramInboxRecord[]> {
  if (limit <= 0) return [];
  const result = await pool.query<InboxDbRow>(
    `
    SELECT * FROM telegram_update_inbox
    WHERE bot_user_id = $1
      AND thread_key = $2
      AND NOT (status = ANY($3::text[]))
      AND NOT (update_id = ANY($4::bigint[]))
    ORDER BY update_id ASC
    LIMIT $5
    `,
    [botUserId, threadKey, TERMINAL_STATUS_ARRAY, [...excludeUpdateIds], limit],
  );
  return result.rows.map(mapRow);
}

/**
 * Startup render recovery: rows whose execution was accepted and whose render
 * obligation is durable but whose terminal delivery was never confirmed.
 * Long-polling has no platform redelivery, so these are the only record that
 * an answer still owes the chat a message.
 */
export async function listRecoverableObligations(
  pool: Pool,
  botUserId: string,
): Promise<TelegramInboxRecord[]> {
  const result = await pool.query<InboxDbRow>(
    `
    SELECT * FROM telegram_update_inbox
    WHERE bot_user_id = $1 AND status = 'render_obligation_persisted'
    ORDER BY update_id ASC
    `,
    [botUserId],
  );
  return result.rows.map(mapRow);
}

type InboxDbRow = {
  bot_user_id: string;
  update_id: string;
  payload: TelegramUpdate;
  status: TelegramInboxStatus;
  thread_key: string | null;
  client_message_id: string | null;
  execution_id: string | null;
  status_reason: string | null;
  render_obligation: TelegramRenderObligation | null;
  received_at: Date;
  updated_at: Date;
};

function mapRow(row: InboxDbRow): TelegramInboxRecord {
  return {
    botUserId: row.bot_user_id,
    updateId: Number(row.update_id),
    payload: row.payload,
    status: row.status,
    threadKey: row.thread_key,
    clientMessageId: row.client_message_id,
    executionId: row.execution_id,
    statusReason: row.status_reason,
    renderObligation: row.render_obligation,
    receivedAt: row.received_at,
    updatedAt: row.updated_at,
  };
}
