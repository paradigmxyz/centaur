import { afterAll, afterEach, describe, expect, it } from "bun:test";
import pg from "pg";
import type { Pool } from "pg";
import { createPollerController } from "../src/poller";
import type {
  IngestedUpdates,
  PollerController,
  PollerControllerDeps,
} from "../src/poller";
import { acquireOwnership, releaseOwnership } from "../src/ownership";
import { runMigrations } from "../src/migrations";
import {
  createTelegramApi,
  isTelegramRateLimitError,
  TelegramApiError,
} from "../src/telegram-api";
import type { TelegramUpdate } from "../src/types";
import { startFakeTelegramServer } from "./support/fake-telegram-server";
import type { FakeTelegramServer } from "./support/fake-telegram-server";

/**
 * Poller controller tests: fake Bot API HTTP server + real Postgres.
 * DB-dependent cases are skipped cleanly when TELEGRAMBOT_TEST_DATABASE_URL
 * is unset; the fake-server semantics themselves are covered unconditionally.
 */

const databaseUrl = process.env.TELEGRAMBOT_TEST_DATABASE_URL;

const servers: FakeTelegramServer[] = [];
const controllers: PollerController[] = [];

function fakeServer(botId: number): FakeTelegramServer {
  const server = startFakeTelegramServer({ botUser: { id: botId } });
  servers.push(server);
  return server;
}

afterEach(async () => {
  for (const controller of controllers.splice(0)) {
    await controller.shutdown();
  }
  for (const server of servers.splice(0)) {
    server.stop();
  }
});

async function waitFor(
  condition: () => boolean | Promise<boolean>,
  label: string,
  timeoutMs = 8_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await condition()) return;
    await Bun.sleep(20);
  }
  throw new Error(`timed out waiting for ${label}`);
}

let nextBotId = 700_000_000 + Math.floor(Math.random() * 100_000_000);

function messageUpdate(
  updateId: number,
  overrides: Partial<TelegramUpdate> = {},
): TelegramUpdate {
  return {
    update_id: updateId,
    message: {
      message_id: updateId,
      from: { id: 42, is_bot: false, first_name: "Alice" },
      chat: { id: -1001, type: "supergroup" as const, title: "ops" },
      date: Math.floor(Date.now() / 1000),
      text: `hello ${updateId}`,
    },
    ...overrides,
  };
}

describe("fake telegram server", () => {
  it("honors getUpdates offset semantics: confirms below, redelivers at/above", async () => {
    const server = fakeServer(nextBotId++);
    const api = createTelegramApi({
      botToken: server.token,
      telegramApiUrl: server.baseUrl,
      pollTimeoutSeconds: 0,
    });
    server.queueUpdates(messageUpdate(1), messageUpdate(2), messageUpdate(3));

    const first = await api.getUpdates({ timeout: 0 });
    expect(first.map((update) => update.update_id)).toEqual([1, 2, 3]);

    // Same updates again without an offset: nothing confirmed yet.
    const again = await api.getUpdates({ timeout: 0 });
    expect(again.map((update) => update.update_id)).toEqual([1, 2, 3]);

    const confirmed = await api.getUpdates({ offset: 3, timeout: 0 });
    expect(confirmed.map((update) => update.update_id)).toEqual([3]);
    expect(server.confirmedUpdateIds).toEqual([1, 2]);
    expect(server.pendingUpdateIds()).toEqual([3]);
  });

  it("injects scripted failures with Telegram error shape", async () => {
    const server = fakeServer(nextBotId++);
    const api = createTelegramApi({
      botToken: server.token,
      telegramApiUrl: server.baseUrl,
      pollTimeoutSeconds: 0,
    });
    server.failNext("getUpdates", {
      status: 429,
      description: "Too Many Requests",
      retryAfterSeconds: 7,
    });

    expect.assertions(4);
    try {
      await api.getUpdates({ timeout: 0 });
    } catch (error) {
      expect(error).toBeInstanceOf(TelegramApiError);
      expect(isTelegramRateLimitError(error)).toBe(true);
      expect((error as TelegramApiError).retryAfterSeconds).toBe(7);
    }
    // Script consumed: the next call succeeds.
    expect(await api.getUpdates({ timeout: 0 })).toEqual([]);
  });
});

describe.skipIf(!databaseUrl)("poller controller (requires Postgres)", () => {
  const pool: Pool = new pg.Pool({
    connectionString: databaseUrl,
    max: 10,
    connectionTimeoutMillis: 10_000,
  });
  const usedBotIds: string[] = [];
  let migrated = false;

  afterAll(async () => {
    if (usedBotIds.length > 0 && migrated) {
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

  async function setup(
    overrides: Partial<PollerControllerDeps> = {},
  ): Promise<{
    server: FakeTelegramServer;
    controller: PollerController;
    botUserId: string;
    batches: IngestedUpdates[];
    fatal: { called: boolean };
  }> {
    if (!migrated) {
      await runMigrations(pool);
      migrated = true;
    }
    const botId = nextBotId++;
    const botUserId = String(botId);
    usedBotIds.push(botUserId);
    const server = fakeServer(botId);
    const api = createTelegramApi({
      botToken: server.token,
      telegramApiUrl: server.baseUrl,
    });
    const batches: IngestedUpdates[] = [];
    const fatal = { called: false };
    const controller = createPollerController({
      api,
      pool,
      leaseTtlMs: 1_500,
      pollTimeoutSeconds: 1,
      freshnessWindowMs: 5_000,
      backoff: { baseMs: 25, maxMs: 100 },
      onUpdatesIngested: (batch) => {
        batches.push(batch);
      },
      onFatalEnd: () => {
        fatal.called = true;
      },
      ...overrides,
    });
    controllers.push(controller);
    return { server, controller, botUserId, batches, fatal };
  }

  async function receiveOffset(botUserId: string): Promise<number | null> {
    const result = await pool.query<{ receive_offset: string | null }>(
      "SELECT receive_offset FROM telegram_poll_state WHERE bot_user_id = $1",
      [botUserId],
    );
    const raw = result.rows[0]?.receive_offset;
    return raw === null || raw === undefined ? null : Number(raw);
  }

  it("startup order: webhook deleted only after ownership, before polling", async () => {
    const { server, controller, botUserId } = await setup();
    // A rival holds an unexpired lease: the controller must resolve its
    // identity but go no further than the ownership gate.
    const rival = await acquireOwnership(pool, botUserId, "rival", 60_000);
    expect(rival).not.toBeNull();
    if (!rival) throw new Error("rival lease not acquired");

    controller.start();
    await waitFor(
      () => server.callsFor("getMe").length > 0,
      "getMe while ownership blocked",
    );
    await Bun.sleep(300);
    expect(server.callsFor("deleteWebhook")).toHaveLength(0);
    expect(server.callsFor("getUpdates")).toHaveLength(0);
    const blocked = controller.status();
    expect(blocked.ready).toBe(false);
    expect(blocked.reasons).toContain("ownership_not_held");

    // Rival releases: startup proceeds ownership -> deleteWebhook -> poll.
    await releaseOwnership(pool, rival);
    await waitFor(
      () => server.callsFor("getUpdates").length > 0,
      "polling after ownership acquired",
    );
    const webhookIndex = server.calls.findIndex(
      (call) => call.method === "deleteWebhook",
    );
    const pollIndex = server.calls.findIndex(
      (call) => call.method === "getUpdates",
    );
    expect(webhookIndex).toBeGreaterThan(-1);
    expect(webhookIndex).toBeLessThan(pollIndex);
    await waitFor(() => controller.status().ready, "ready after startup");
    expect(controller.ownership()?.botUserId).toBe(botUserId);
  });

  it("first start polls without an offset, then advances to max(update_id)+1", async () => {
    const { server, controller, botUserId, batches } = await setup();
    // Non-contiguous ids: the cursor is max+1, never a contiguity assumption.
    server.queueUpdates(messageUpdate(10), messageUpdate(12));

    controller.start();
    await waitFor(
      () => server.callsFor("getUpdates").length > 0,
      "first poll",
    );
    const firstPoll = server.callsFor("getUpdates")[0];
    expect(firstPoll?.params["offset"]).toBeUndefined();
    expect(firstPoll?.params["allowed_updates"]).toEqual(["message"]);

    await waitFor(
      () =>
        server
          .callsFor("getUpdates")
          .some((call) => call.params["offset"] === 13),
      "next poll uses committed offset 13",
    );
    expect(await receiveOffset(botUserId)).toBe(13);
    await waitFor(() => batches.length > 0, "dispatch callback");
    expect(
      batches[0]?.updates.map((update) => update.update_id),
    ).toEqual([10, 12]);
    expect(batches[0]?.botUserId).toBe(botUserId);
  });

  it("polls only from the committed cursor across restarts", async () => {
    const { server, controller, botUserId } = await setup();
    server.queueUpdates(messageUpdate(5), messageUpdate(7));
    controller.start();
    await waitFor(
      async () => (await receiveOffset(botUserId)) === 8,
      "cursor committed at 8",
    );
    await controller.shutdown();

    const callsBeforeRestart = server.callsFor("getUpdates").length;
    const restarted = createPollerController({
      api: createTelegramApi({
        botToken: server.token,
        telegramApiUrl: server.baseUrl,
      }),
      pool,
      leaseTtlMs: 1_500,
      pollTimeoutSeconds: 1,
      freshnessWindowMs: 5_000,
      backoff: { baseMs: 25, maxMs: 100 },
      onFatalEnd: () => undefined,
    });
    controllers.push(restarted);
    restarted.start();
    await waitFor(
      () => server.callsFor("getUpdates").length > callsBeforeRestart,
      "restarted controller polls",
    );
    const firstAfterRestart = server.callsFor("getUpdates")[callsBeforeRestart];
    expect(firstAfterRestart?.params["offset"]).toBe(8);
  });

  it("re-polls the same offset when ingest fails before the cursor commits", async () => {
    let failures = 1;
    const { server, controller, botUserId, batches } = await setup({
      ingestHooks: {
        beforeCursorAdvance: () => {
          if (failures > 0) {
            failures -= 1;
            throw new Error("injected crash before cursor advance");
          }
        },
      },
    });
    server.queueUpdates(messageUpdate(21));

    controller.start();
    await waitFor(
      async () => (await receiveOffset(botUserId)) === 22,
      "batch eventually confirmed",
    );
    // The failed ingest never confirmed the batch: the next poll carried the
    // same (absent) offset and Telegram redelivered update 21.
    const polls = server.callsFor("getUpdates");
    expect(polls.length).toBeGreaterThanOrEqual(2);
    expect(polls[0]?.params["offset"]).toBeUndefined();
    expect(polls[1]?.params["offset"]).toBeUndefined();
    const rows = await pool.query(
      "SELECT update_id FROM telegram_update_inbox WHERE bot_user_id = $1",
      [botUserId],
    );
    expect(rows.rowCount).toBe(1);
    await waitFor(() => batches.length > 0, "dispatch after successful ingest");
    expect(batches).toHaveLength(1);
  });

  it("fatal 401 on getMe fires onFatalEnd and never polls", async () => {
    const { server, controller, fatal } = await setup();
    server.failNext("getMe", { status: 401, description: "Unauthorized" });

    controller.start();
    await waitFor(() => fatal.called, "onFatalEnd");
    expect(server.callsFor("getUpdates")).toHaveLength(0);
    expect(server.callsFor("deleteWebhook")).toHaveLength(0);
    const status = controller.status();
    expect(status.ready).toBe(false);
    expect(status.reasons).toContain("fatal_configuration_error");
    expect(status.live).toBe(true);
  });

  it("fatal 409 on getUpdates fires onFatalEnd and stops polling", async () => {
    const { server, controller, fatal } = await setup();
    server.failNext("getUpdates", {
      status: 409,
      description: "Conflict: terminated by other getUpdates request",
    });

    controller.start();
    await waitFor(() => fatal.called, "onFatalEnd");
    const pollsAtFatal = server.callsFor("getUpdates").length;
    await Bun.sleep(300);
    expect(server.callsFor("getUpdates")).toHaveLength(pollsAtFatal);
  });

  it("transient 5xx: backoff keeps retrying, ready fails past the freshness window, live stays true", async () => {
    const { server, controller } = await setup({ freshnessWindowMs: 300 });
    server.failNext(
      "getUpdates",
      { status: 500, description: "Internal Server Error" },
      Number.POSITIVE_INFINITY,
    );

    controller.start();
    await waitFor(() => {
      const status = controller.status();
      return !status.ready && status.reasons.includes("poll_stale");
    }, "readiness degrades to poll_stale");
    expect(controller.status().live).toBe(true);
    const failedPolls = server.callsFor("getUpdates").length;
    expect(failedPolls).toBeGreaterThanOrEqual(2);

    // Outage clears: polling resumes on the same loop and readiness recovers.
    server.clearScripts("getUpdates");
    await waitFor(() => controller.status().ready, "ready after recovery");
    expect(controller.status().lastSuccessfulPollAtMs).not.toBeNull();
  });

  it("429 delays the next poll by at least retry_after", async () => {
    const { server, controller } = await setup();
    server.failNext("getUpdates", {
      status: 429,
      description: "Too Many Requests",
      retryAfterSeconds: 1,
    });

    controller.start();
    await waitFor(
      () => server.callsFor("getUpdates").length >= 2,
      "poll after 429",
    );
    const polls = server.callsFor("getUpdates");
    const first = polls[0];
    const second = polls[1];
    if (!first || !second) throw new Error("expected two polls");
    expect(second.atMs - first.atMs).toBeGreaterThanOrEqual(700);
  });

  it("ownership loss stops polling immediately and blocks reacquisition", async () => {
    const { server, controller, botUserId } = await setup();
    controller.start();
    await waitFor(() => controller.status().ready, "ready");

    // Usurp the lease the way a successor takeover would: new holder, bumped
    // generation, unexpired term. The next renewal cannot prove the lease.
    await pool.query(
      `UPDATE telegram_poll_state
       SET holder_id = 'usurper', generation = generation + 1,
           lease_expires_at = now() + interval '60 seconds'
       WHERE bot_user_id = $1`,
      [botUserId],
    );

    await waitFor(() => {
      const status = controller.status();
      return !status.ready && status.reasons.includes("ownership_not_held");
    }, "readiness fails on ownership loss");
    expect(controller.ownership()).toBeNull();

    const pollsAtLoss = server.callsFor("getUpdates").length;
    // Usurper's lease is 60s: reacquisition must fail, so no new polls even
    // after the old lease term lapses and startup retries begin.
    await Bun.sleep(1_500);
    expect(server.callsFor("getUpdates")).toHaveLength(pollsAtLoss);
    expect(controller.status().reasons).toContain("ownership_not_held");
  });

  it("empty batches still refresh poll freshness", async () => {
    const { server, controller, botUserId } = await setup({
      pollTimeoutSeconds: 0,
    });
    controller.start();
    await waitFor(() => controller.status().ready, "ready");
    await waitFor(
      () => controller.status().lastSuccessfulPollAtMs !== null,
      "first successful poll",
    );
    const first = controller.status().lastSuccessfulPollAtMs;
    await Bun.sleep(250);
    const second = controller.status().lastSuccessfulPollAtMs;
    if (first === null || second === null) throw new Error("expected polls");
    expect(second).toBeGreaterThan(first);
    expect(controller.status().ready).toBe(true);
    // No updates were ever queued, so nothing was written to the inbox.
    const rows = await pool.query(
      "SELECT count(*)::int AS count FROM telegram_update_inbox WHERE bot_user_id = $1",
      [botUserId],
    );
    expect(rows.rows[0]?.count).toBe(0);
    expect(server.callsFor("getUpdates").length).toBeGreaterThan(1);
  });

  it("non-message updates get a durable ignored disposition and never stall the cursor", async () => {
    const { server, controller, botUserId, batches } = await setup();
    server.queueUpdates(
      {
        update_id: 30,
        channel_post: { message_id: 1, chat: { id: 5, type: "channel" } },
      },
      messageUpdate(31),
    );

    controller.start();
    await waitFor(
      async () => (await receiveOffset(botUserId)) === 32,
      "cursor advanced past both updates",
    );
    await waitFor(async () => {
      const result = await pool.query<{ status: string }>(
        "SELECT status FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = 30",
        [botUserId],
      );
      return result.rows[0]?.status === "ignored";
    }, "channel_post durably ignored");
    const ignored = await pool.query<{ status_reason: string | null }>(
      "SELECT status_reason FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = 30",
      [botUserId],
    );
    expect(ignored.rows[0]?.status_reason).toBe(
      "unsupported_update_type:channel_post",
    );

    await waitFor(() => batches.length > 0, "dispatch");
    // Only the message-bearing update reaches dispatch.
    expect(
      batches.flatMap((batch) =>
        batch.updates.map((update) => update.update_id),
      ),
    ).toEqual([31]);
    const messageRow = await pool.query<{ status: string }>(
      "SELECT status FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = 31",
      [botUserId],
    );
    expect(messageRow.rows[0]?.status).toBe("received");
  });

  it("shutdown aborts a hanging long poll promptly and releases the lease", async () => {
    const { server, controller, botUserId } = await setup({
      pollTimeoutSeconds: 40,
      leaseTtlMs: 5_000,
    });
    controller.start();
    await waitFor(
      () => server.callsFor("getUpdates").length > 0,
      "long poll in flight",
    );

    const startedAt = Date.now();
    await controller.shutdown();
    expect(Date.now() - startedAt).toBeLessThan(1_500);

    const released = await pool.query<{ released: boolean }>(
      "SELECT lease_expires_at <= now() AS released FROM telegram_poll_state WHERE bot_user_id = $1",
      [botUserId],
    );
    expect(released.rows[0]?.released).toBe(true);
  });
});
