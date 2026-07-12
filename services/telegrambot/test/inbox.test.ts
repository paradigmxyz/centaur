import { afterAll, beforeAll, describe, expect, it } from "bun:test";
import pg from "pg";
import type { Pool } from "pg";
import {
  acceptForProcessing,
  claimNextPerThread,
  ingestBatch,
  listRecoverableObligations,
  markIgnored,
  markRejected,
  pruneTerminal,
  readReceiveOffset,
  transition,
} from "../src/inbox";
import { runMigrations } from "../src/migrations";
import { OwnershipLostError, acquireOwnership } from "../src/ownership";
import type {
  OwnershipLease,
  TelegramInboxStatus,
  TelegramRenderObligation,
  TelegramUpdate,
} from "../src/types";

/**
 * Durable inbox tests (spec §2 receipt contract + §2a atomicity) against real
 * Postgres. Skipped cleanly when TELEGRAMBOT_TEST_DATABASE_URL is unset.
 * Every test uses its own bot_user_id, so state is hermetic per test and the
 * shared tables are cleaned up in afterAll (same pattern as poller.test.ts).
 */

const databaseUrl = process.env.TELEGRAMBOT_TEST_DATABASE_URL;

let nextBotId = 830_000_000 + Math.floor(Math.random() * 100_000_000);

const NONTERMINAL_STATUSES: readonly TelegramInboxStatus[] = [
  "received",
  "message_appended",
  "steering_pending",
  "execution_accepted",
  "render_obligation_persisted",
];

function messageUpdate(updateId: number, text?: string): TelegramUpdate {
  return {
    update_id: updateId,
    message: {
      message_id: updateId,
      from: { id: 42, is_bot: false, first_name: "Alice" },
      chat: { id: -1001, type: "supergroup" as const, title: "ops" },
      date: Math.floor(Date.now() / 1000),
      text: text ?? `hello ${updateId}`,
    },
  };
}

describe.skipIf(!databaseUrl)("durable inbox (requires Postgres)", () => {
  const pool: Pool = new pg.Pool({
    connectionString: databaseUrl,
    max: 10,
    connectionTimeoutMillis: 10_000,
  });
  const usedBotIds: string[] = [];

  async function setup(): Promise<{
    botUserId: string;
    lease: OwnershipLease;
  }> {
    const botUserId = String(nextBotId++);
    usedBotIds.push(botUserId);
    const lease = await acquireOwnership(pool, botUserId, "worker-1", 60_000);
    if (!lease) throw new Error("failed to acquire test lease");
    return { botUserId, lease };
  }

  async function inboxRow(
    botUserId: string,
    updateId: number,
  ): Promise<{
    status: string;
    executionId: string | null;
    statusReason: string | null;
    text: string | undefined;
  } | null> {
    const result = await pool.query<{
      status: string;
      execution_id: string | null;
      status_reason: string | null;
      payload: TelegramUpdate;
    }>(
      `SELECT status, execution_id, status_reason, payload
       FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = $2`,
      [botUserId, updateId],
    );
    const row = result.rows[0];
    if (!row) return null;
    return {
      status: row.status,
      executionId: row.execution_id,
      statusReason: row.status_reason,
      text: row.payload.message?.text,
    };
  }

  async function inboxUpdateIds(botUserId: string): Promise<number[]> {
    const result = await pool.query<{ update_id: string }>(
      "SELECT update_id FROM telegram_update_inbox WHERE bot_user_id = $1 ORDER BY update_id",
      [botUserId],
    );
    return result.rows.map((row) => Number(row.update_id));
  }

  beforeAll(async () => {
    await runMigrations(pool);
  });

  afterAll(async () => {
    if (usedBotIds.length > 0) {
      await pool.query(
        "DELETE FROM telegram_update_inbox WHERE bot_user_id = ANY($1::text[])",
        [usedBotIds],
      );
      await pool.query(
        "DELETE FROM telegram_poll_state WHERE bot_user_id = ANY($1::text[])",
        [usedBotIds],
      );
    }
    await pool.end();
  });

  it("readReceiveOffset is null on first start, before and after the lease row exists", async () => {
    const botUserId = String(nextBotId++);
    usedBotIds.push(botUserId);

    // No poll_state row at all.
    expect(await readReceiveOffset(pool, botUserId)).toBe(null);

    // Lease row exists but no batch was ever committed: still null, so the
    // first getUpdates is issued WITHOUT an offset.
    const lease = await acquireOwnership(pool, botUserId, "worker-1", 60_000);
    expect(lease).not.toBeNull();
    expect(await readReceiveOffset(pool, botUserId)).toBe(null);
  });

  it("ingestBatch persists the whole batch and advances the cursor to max(update_id)+1 in one commit", async () => {
    const { botUserId, lease } = await setup();

    const committed = await ingestBatch(pool, lease, [
      messageUpdate(10),
      messageUpdate(12),
    ]);
    expect(committed).toBe(13);
    expect(await readReceiveOffset(pool, botUserId)).toBe(13);
    expect(await inboxUpdateIds(botUserId)).toEqual([10, 12]);

    const row = await inboxRow(botUserId, 12);
    expect(row?.status).toBe("received");
    expect(row?.text).toBe("hello 12");
  });

  it("an empty batch returns the committed cursor and still proves the lease", async () => {
    const { botUserId, lease } = await setup();

    // Empty poll before any batch: cursor is still null.
    expect(await ingestBatch(pool, lease, [])).toBe(null);
    await ingestBatch(pool, lease, [messageUpdate(7)]);
    expect(await ingestBatch(pool, lease, [])).toBe(8);

    // A fenced-out identity may not even read "its" cursor via an empty poll.
    const ghost: OwnershipLease = {
      botUserId,
      holderId: "ghost",
      generation: 99,
    };
    await expect(ingestBatch(pool, ghost, [])).rejects.toThrow(
      OwnershipLostError,
    );
  });

  it("a failure between batch upsert and cursor advance rolls back BOTH", async () => {
    const { botUserId, lease } = await setup();
    await ingestBatch(pool, lease, [messageUpdate(20)]);

    await expect(
      ingestBatch(pool, lease, [messageUpdate(21), messageUpdate(22)], undefined, {
        beforeCursorAdvance: () => {
          throw new Error("injected crash before cursor advance");
        },
      }),
    ).rejects.toThrow("injected crash before cursor advance");

    // Neither the rows nor the cursor moved: the next poll re-fetches the
    // same batch instead of silently losing it.
    expect(await inboxUpdateIds(botUserId)).toEqual([20]);
    expect(await readReceiveOffset(pool, botUserId)).toBe(21);

    // The same batch then ingests cleanly (redelivery path).
    expect(await ingestBatch(pool, lease, [messageUpdate(21), messageUpdate(22)])).toBe(
      23,
    );
    expect(await inboxUpdateIds(botUserId)).toEqual([20, 21, 22]);
  });

  it("re-ingesting a duplicate update keeps the existing row's stage and payload while still advancing the cursor", async () => {
    const { botUserId, lease } = await setup();
    await ingestBatch(pool, lease, [messageUpdate(30, "original text")]);
    expect(
      await transition(pool, lease, 30, ["received"], "message_appended"),
    ).toBe(true);

    // Telegram redelivers 30 (with a mutated payload, to prove DO NOTHING)
    // alongside new update 31.
    const committed = await ingestBatch(pool, lease, [
      messageUpdate(30, "redelivered text"),
      messageUpdate(31),
    ]);
    expect(committed).toBe(32);
    expect(await readReceiveOffset(pool, botUserId)).toBe(32);

    const row = await inboxRow(botUserId, 30);
    expect(row?.status).toBe("message_appended");
    expect(row?.text).toBe("original text");
    expect((await inboxRow(botUserId, 31))?.status).toBe("received");
  });

  it("non-sequential update ids set the offset to max+1 without assuming contiguity", async () => {
    const { botUserId, lease } = await setup();
    const committed = await ingestBatch(pool, lease, [
      messageUpdate(100),
      messageUpdate(150),
      messageUpdate(120),
    ]);
    expect(committed).toBe(151);
    expect(await readReceiveOffset(pool, botUserId)).toBe(151);
    expect(await inboxUpdateIds(botUserId)).toEqual([100, 120, 150]);
  });

  it("the cursor never regresses when an older batch is replayed", async () => {
    const { botUserId, lease } = await setup();
    await ingestBatch(pool, lease, [messageUpdate(40), messageUpdate(41)]);
    expect(await readReceiveOffset(pool, botUserId)).toBe(42);

    // Replay of an older, already-confirmed batch: rows dedupe, cursor holds.
    const committed = await ingestBatch(pool, lease, [messageUpdate(40)]);
    expect(committed).toBe(42);
    expect(await readReceiveOffset(pool, botUserId)).toBe(42);
  });

  it("receipt is independent of processing: 100 updates with the first held nonterminal still let update 101 ingest and advance", async () => {
    const { botUserId, lease } = await setup();
    const batch = Array.from({ length: 100 }, (_, index) =>
      messageUpdate(index + 1),
    );
    expect(await ingestBatch(pool, lease, batch)).toBe(101);

    // First update starts processing and stays at a nonterminal stage.
    expect(
      await acceptForProcessing(
        pool,
        lease,
        1,
        "telegram:chat:-1001",
        "telegram:-1001:1",
      ),
    ).toBe(true);
    expect(
      await transition(pool, lease, 1, ["received"], "message_appended"),
    ).toBe(true);

    // Telegram would only return update 101 to a poll at offset 101; that
    // batch must ingest and advance regardless of update 1's stage.
    expect(await ingestBatch(pool, lease, [messageUpdate(101)])).toBe(102);
    expect(await readReceiveOffset(pool, botUserId)).toBe(102);
    expect((await inboxRow(botUserId, 1))?.status).toBe("message_appended");
    expect((await inboxRow(botUserId, 101))?.status).toBe("received");
  });

  it("markIgnored and markRejected record durable reasons and are terminal", async () => {
    const { botUserId, lease } = await setup();
    await ingestBatch(pool, lease, [messageUpdate(50), messageUpdate(51)]);

    expect(
      await markIgnored(pool, lease, 50, "unsupported_update_type:channel_post"),
    ).toBe(true);
    expect(await markRejected(pool, lease, 51, "not_allowlisted")).toBe(true);

    const ignored = await inboxRow(botUserId, 50);
    expect(ignored?.status).toBe("ignored");
    expect(ignored?.statusReason).toBe("unsupported_update_type:channel_post");
    const rejected = await inboxRow(botUserId, 51);
    expect(rejected?.status).toBe("rejected");
    expect(rejected?.statusReason).toBe("not_allowlisted");

    // Terminal: neither a repeat marking nor a normal stage transition moves
    // the row again, and the original reason is preserved.
    expect(await markIgnored(pool, lease, 50, "second reason")).toBe(false);
    expect(
      await transition(pool, lease, 50, NONTERMINAL_STATUSES, "message_appended"),
    ).toBe(false);
    expect((await inboxRow(botUserId, 50))?.statusReason).toBe(
      "unsupported_update_type:channel_post",
    );

    // A terminal reason is mandatory, checked before any write.
    await expect(markIgnored(pool, lease, 50, "   ")).rejects.toThrow(
      /durable reason/,
    );
    await expect(
      transition(pool, lease, 51, ["received"], "rejected"),
    ).rejects.toThrow(/durable reason/);
  });

  it("transition is a fenced compare-and-set: wrong fromStatus changes nothing, the right one persists patch fields with JSON round-trip", async () => {
    const { botUserId, lease } = await setup();
    await ingestBatch(pool, lease, [messageUpdate(60)]);

    // Wrong fromStatus: no write at all.
    expect(
      await transition(pool, lease, 60, ["message_appended"], "execution_accepted", {
        executionId: "exec-should-not-land",
      }),
    ).toBe(false);
    const untouched = await inboxRow(botUserId, 60);
    expect(untouched?.status).toBe("received");
    expect(untouched?.executionId).toBe(null);

    // Correct CAS chain persists each patch.
    expect(
      await transition(pool, lease, 60, ["received"], "message_appended"),
    ).toBe(true);
    expect(
      await transition(pool, lease, 60, ["message_appended"], "execution_accepted", {
        executionId: "exec-1",
      }),
    ).toBe(true);

    const obligation: TelegramRenderObligation = {
      threadKey: "telegram:chat:-1001:17",
      executionId: "exec-1",
      chatId: "-1001",
      messageThreadId: 17,
      triggerMessageId: 60,
      afterEventId: 41,
      postedMessageIds: [900, 901],
      deliveredText: "partial answer\nwith unicode \u{1F680}",
    };
    expect(
      await transition(
        pool,
        lease,
        60,
        ["execution_accepted"],
        "render_obligation_persisted",
        { renderObligation: obligation },
      ),
    ).toBe(true);

    // The obligation round-trips through jsonb byte-for-value identical, and
    // earlier patch fields (execution id) were not erased.
    const recoverable = await listRecoverableObligations(pool, botUserId);
    expect(recoverable).toHaveLength(1);
    expect(recoverable[0]?.updateId).toBe(60);
    expect(recoverable[0]?.executionId).toBe("exec-1");
    expect(recoverable[0]?.renderObligation).toEqual(obligation);
    expect(recoverable[0]?.status).toBe("render_obligation_persisted");
  });

  it("claimNextPerThread returns the oldest nonterminal row per thread, at most one per thread, honoring exclusions and unstamped rows", async () => {
    const { botUserId, lease } = await setup();
    await ingestBatch(pool, lease, [
      messageUpdate(70),
      messageUpdate(71),
      messageUpdate(72),
      messageUpdate(73),
      messageUpdate(74),
    ]);

    // Thread A holds 70 (in-flight) and 72; thread B holds 71 (nonterminal)
    // and 73 (terminal); 74 stays unstamped `received`.
    await acceptForProcessing(pool, lease, 70, "thread-a", "telegram:-1001:70");
    await acceptForProcessing(pool, lease, 72, "thread-a", "telegram:-1001:72");
    await acceptForProcessing(pool, lease, 71, "thread-b", "telegram:-1001:71");
    await acceptForProcessing(pool, lease, 73, "thread-b", "telegram:-1001:73");
    await transition(pool, lease, 70, ["received"], "message_appended");
    await transition(pool, lease, 73, ["received"], "completed");

    const claimed = await claimNextPerThread(pool, botUserId, [], 10);
    expect(claimed.map((row) => row.updateId)).toEqual([70, 71, 74]);
    expect(claimed.map((row) => row.threadKey)).toEqual([
      "thread-a",
      "thread-b",
      null,
    ]);

    // Exclusion list skips a claimed thread but never hides unstamped rows.
    const excluded = await claimNextPerThread(pool, botUserId, ["thread-a"], 10);
    expect(excluded.map((row) => row.updateId)).toEqual([71, 74]);

    // Limit takes the oldest threads first.
    const limited = await claimNextPerThread(pool, botUserId, [], 2);
    expect(limited.map((row) => row.updateId)).toEqual([70, 71]);

    expect(await claimNextPerThread(pool, botUserId, [], 0)).toEqual([]);
  });

  it("pruneTerminal deletes only old terminal rows below the committed cursor and never touches poll_state", async () => {
    const { botUserId, lease } = await setup();
    await ingestBatch(pool, lease, [
      messageUpdate(80),
      messageUpdate(81),
      messageUpdate(82),
    ]);
    // Cursor is 83.
    await markIgnored(pool, lease, 80, "old terminal below cursor");
    await markIgnored(pool, lease, 81, "fresh terminal below cursor");

    // Synthetic terminal row ABOVE the committed cursor (Telegram has not
    // confirmed it; it could still be redelivered) — must survive pruning.
    await pool.query(
      `INSERT INTO telegram_update_inbox (bot_user_id, update_id, payload, status, status_reason)
       VALUES ($1, 90, '{"update_id": 90}'::jsonb, 'completed', null)`,
      [botUserId],
    );

    // Age rows 80 (terminal), 82 (nonterminal), and 90 (above cursor) past
    // the retention window; 81 stays fresh.
    await pool.query(
      `UPDATE telegram_update_inbox SET updated_at = now() - interval '48 hours'
       WHERE bot_user_id = $1 AND update_id = ANY($2::bigint[])`,
      [botUserId, [80, 82, 90]],
    );

    const pollStateBefore = await pool.query(
      "SELECT receive_offset, holder_id, generation FROM telegram_poll_state WHERE bot_user_id = $1",
      [botUserId],
    );

    const deleted = await pruneTerminal(pool, lease, 24);
    expect(deleted).toBe(1);

    // Only 80 (terminal AND old AND below the cursor) is gone. The fresh
    // terminal row, the old nonterminal row, and the terminal row above the
    // cursor all survive.
    expect(await inboxUpdateIds(botUserId)).toEqual([81, 82, 90]);

    // The receipt cursor and lease are untouched.
    const pollStateAfter = await pool.query(
      "SELECT receive_offset, holder_id, generation FROM telegram_poll_state WHERE bot_user_id = $1",
      [botUserId],
    );
    expect(pollStateAfter.rows).toEqual(pollStateBefore.rows);
    expect(await readReceiveOffset(pool, botUserId)).toBe(83);

    // A fenced-out identity prunes nothing.
    const ghost: OwnershipLease = {
      botUserId,
      holderId: "ghost",
      generation: 99,
    };
    await pool.query(
      `UPDATE telegram_update_inbox SET updated_at = now() - interval '48 hours'
       WHERE bot_user_id = $1 AND update_id = 81`,
      [botUserId],
    );
    expect(await pruneTerminal(pool, ghost, 24)).toBe(0);
    expect(await inboxUpdateIds(botUserId)).toEqual([81, 82, 90]);
  });
});
