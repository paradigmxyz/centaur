import { afterAll, beforeAll, describe, expect, it } from "bun:test";
import pg from "pg";
import type { Pool } from "pg";
import { ingestBatch, readReceiveOffset, transition } from "../src/inbox";
import { runMigrations } from "../src/migrations";
import {
  OwnershipLostError,
  acquireOwnership,
  releaseOwnership,
  renewLease,
} from "../src/ownership";
import type { OwnershipLease, TelegramUpdate } from "../src/types";

/**
 * Fenced ownership lease tests (spec §2 option b) against real Postgres.
 * Skipped cleanly when TELEGRAMBOT_TEST_DATABASE_URL is unset. All expiry
 * timing is database now(); the short TTLs + generous sleeps below only need
 * "clearly before expiry" / "clearly after expiry", never exact clocks.
 */

const databaseUrl = process.env.TELEGRAMBOT_TEST_DATABASE_URL;

let nextBotId = 820_000_000 + Math.floor(Math.random() * 100_000_000);

function messageUpdate(updateId: number): TelegramUpdate {
  return {
    update_id: updateId,
    message: {
      message_id: updateId,
      from: { id: 42, is_bot: false, first_name: "Alice" },
      chat: { id: -1001, type: "supergroup" as const, title: "ops" },
      date: Math.floor(Date.now() / 1000),
      text: `hello ${updateId}`,
    },
  };
}

describe.skipIf(!databaseUrl)("ownership lease (requires Postgres)", () => {
  const pool: Pool = new pg.Pool({
    connectionString: databaseUrl,
    max: 10,
    connectionTimeoutMillis: 10_000,
  });
  const usedBotIds: string[] = [];

  function newBotUserId(): string {
    const botUserId = String(nextBotId++);
    usedBotIds.push(botUserId);
    return botUserId;
  }

  async function acquired(
    botUserId: string,
    holderId: string,
    ttlMs: number,
  ): Promise<OwnershipLease> {
    const lease = await acquireOwnership(pool, botUserId, holderId, ttlMs);
    if (!lease) throw new Error(`expected ${holderId} to acquire the lease`);
    return lease;
  }

  async function leaseRow(botUserId: string): Promise<{
    holderId: string | null;
    generation: number;
    expired: boolean;
    expiresAt: Date | null;
  }> {
    const result = await pool.query<{
      holder_id: string | null;
      generation: string;
      expired: boolean | null;
      lease_expires_at: Date | null;
    }>(
      `SELECT holder_id, generation, lease_expires_at,
              lease_expires_at <= now() AS expired
       FROM telegram_poll_state WHERE bot_user_id = $1`,
      [botUserId],
    );
    const row = result.rows[0];
    if (!row) throw new Error(`no poll_state row for ${botUserId}`);
    return {
      holderId: row.holder_id,
      generation: Number(row.generation),
      expired: row.expired ?? true,
      expiresAt: row.lease_expires_at,
    };
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

  it("acquires on empty state at generation 1 with an unexpired lease", async () => {
    const botUserId = newBotUserId();
    const lease = await acquireOwnership(pool, botUserId, "holder-a", 60_000);

    expect(lease).toEqual({ botUserId, holderId: "holder-a", generation: 1 });
    const row = await leaseRow(botUserId);
    expect(row.holderId).toBe("holder-a");
    expect(row.generation).toBe(1);
    expect(row.expired).toBe(false);
  });

  it("renewLease extends the lease for the current holder and generation", async () => {
    const botUserId = newBotUserId();
    const lease = await acquired(botUserId, "holder-a", 60_000);
    const before = (await leaseRow(botUserId)).expiresAt;
    if (!before) throw new Error("expected an expiry");

    // Renewing with a longer TTL must strictly extend database expiry.
    expect(await renewLease(pool, lease, 120_000)).toBe(true);
    const after = (await leaseRow(botUserId)).expiresAt;
    if (!after) throw new Error("expected an expiry");
    expect(after.getTime()).toBeGreaterThan(before.getTime());
    expect((await leaseRow(botUserId)).generation).toBe(1);
  });

  it("a rival cannot take an unexpired lease", async () => {
    const botUserId = newBotUserId();
    await acquired(botUserId, "holder-a", 60_000);

    expect(await acquireOwnership(pool, botUserId, "holder-b", 60_000)).toBe(
      null,
    );
    const row = await leaseRow(botUserId);
    expect(row.holderId).toBe("holder-a");
    expect(row.generation).toBe(1);
  });

  it("takeover succeeds only after expiry, increments the generation, and fences out the old holder's renew", async () => {
    const botUserId = newBotUserId();
    const oldLease = await acquired(botUserId, "holder-a", 250);

    // Before expiry: no takeover.
    expect(await acquireOwnership(pool, botUserId, "holder-b", 60_000)).toBe(
      null,
    );

    await Bun.sleep(600);
    const newLease = await acquired(botUserId, "holder-b", 60_000);
    expect(newLease.generation).toBe(2);

    // The old holder can no longer prove its lease.
    expect(await renewLease(pool, oldLease, 60_000)).toBe(false);
    const row = await leaseRow(botUserId);
    expect(row.holderId).toBe("holder-b");
    expect(row.generation).toBe(2);
  });

  it("release expires the lease immediately; reacquire keeps the generation for the same holder and bumps it for a successor", async () => {
    const botUserId = newBotUserId();
    const first = await acquired(botUserId, "holder-a", 60_000);
    expect(first.generation).toBe(1);

    await releaseOwnership(pool, first);
    expect((await leaseRow(botUserId)).expired).toBe(true);

    // Same holder reacquiring its own released lease: no generation bump —
    // its in-flight fenced work stays valid.
    const again = await acquired(botUserId, "holder-a", 60_000);
    expect(again.generation).toBe(1);
    expect((await leaseRow(botUserId)).expired).toBe(false);

    // A successor after release does not wait out the TTL and takes a new
    // generation.
    await releaseOwnership(pool, again);
    const successor = await acquired(botUserId, "holder-b", 60_000);
    expect(successor.generation).toBe(2);
  });

  it("split-brain: after takeover the expired holder's renew, cursor writes, and transitions all fail while the new holder's succeed", async () => {
    const botUserId = newBotUserId();
    const leaseA = await acquired(botUserId, "holder-a", 300);

    // A works normally while its lease is live.
    expect(await ingestBatch(pool, leaseA, [messageUpdate(1)])).toBe(2);
    expect(await readReceiveOffset(pool, botUserId)).toBe(2);

    // A pauses (GC/partition) past expiry; B takes over.
    await Bun.sleep(700);
    const leaseB = await acquired(botUserId, "holder-b", 60_000);
    expect(leaseB.generation).toBe(2);

    // A resumes: renewal fails...
    expect(await renewLease(pool, leaseA, 60_000)).toBe(false);

    // ...its fenced cursor write throws AND rolls back the batch it tried to
    // persist...
    await expect(
      ingestBatch(pool, leaseA, [messageUpdate(5)]),
    ).rejects.toThrow(OwnershipLostError);
    const ghostRow = await pool.query(
      "SELECT 1 FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = 5",
      [botUserId],
    );
    expect(ghostRow.rowCount).toBe(0);
    expect(await readReceiveOffset(pool, botUserId)).toBe(2);

    // ...even an empty poll cannot use A's identity to learn the cursor...
    await expect(ingestBatch(pool, leaseA, [])).rejects.toThrow(
      OwnershipLostError,
    );

    // ...and its fenced stage transition is a no-op.
    expect(
      await transition(pool, leaseA, 1, ["received"], "message_appended"),
    ).toBe(false);
    const untouched = await pool.query<{ status: string }>(
      "SELECT status FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = 1",
      [botUserId],
    );
    expect(untouched.rows[0]?.status).toBe("received");

    // B, holding the current generation, does all of the above successfully.
    expect(await ingestBatch(pool, leaseB, [messageUpdate(5)])).toBe(6);
    expect(await readReceiveOffset(pool, botUserId)).toBe(6);
    expect(
      await transition(pool, leaseB, 1, ["received"], "message_appended"),
    ).toBe(true);
    const moved = await pool.query<{ status: string }>(
      "SELECT status FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = 1",
      [botUserId],
    );
    expect(moved.rows[0]?.status).toBe("message_appended");
  });
});
