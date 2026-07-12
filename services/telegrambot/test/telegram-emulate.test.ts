import { afterAll, afterEach, describe, expect, it } from "bun:test";
import pg from "pg";
import type { Pool } from "pg";
import { createTelegrambot } from "../src/index";
import { parsedTextLength } from "../src/telegram-render";
import type {
  JsonObject,
  TelegramInboxStatus,
  TelegramMessage,
  TelegramUpdate,
  Telegrambot,
  TelegrambotOptions,
} from "../src/types";
import { noopLogger } from "../src/utils";
import { startFakeTelegramServer } from "./support/fake-telegram-server";
import type {
  FakeTelegramServer,
  RecordedTelegramCall,
} from "./support/fake-telegram-server";

/**
 * End-to-end emulation (pattern: discordbot/test/chat-sdk-emulate.test.ts):
 * real Postgres + the fake Telegram Bot API HTTP server + a fake api-rs
 * session server with a scripted SSE event stream. Skipped cleanly when
 * TELEGRAMBOT_TEST_DATABASE_URL is unset, matching poller.test.ts.
 */

const databaseUrl = process.env.TELEGRAMBOT_TEST_DATABASE_URL;

const TELEGRAM_MUTATION_METHODS = [
  "sendMessage",
  "editMessageText",
  "setMessageReaction",
  "sendChatAction",
];

// ---------------------------------------------------------------------------
// Fake api-rs session server (Bun.serve + SSE ReadableStream)
// ---------------------------------------------------------------------------

type SessionRequest = { body: JsonObject; threadKey: string };

type SessionEvent = {
  data: string;
  event: string;
  executionId: string | undefined;
  id: number;
  threadKey: string;
};

type StreamEntry = {
  afterEventId: number;
  controller: ReadableStreamDefaultController<Uint8Array>;
  executionId: string | undefined;
  threadKey: string;
};

type FakeSessionApi = {
  appends: SessionRequest[];
  creates: SessionRequest[];
  emit(
    threadKey: string,
    event: string,
    data: unknown,
    executionId?: string,
  ): void;
  emitAnswerLines(threadKey: string, lines: string[]): void;
  executes: SessionRequest[];
  stop(): void;
  streamCount(): number;
  url: string;
};

function startFakeSessionApi(): FakeSessionApi {
  const appends: SessionRequest[] = [];
  const creates: SessionRequest[] = [];
  const executes: SessionRequest[] = [];
  const events: SessionEvent[] = [];
  const streams = new Set<StreamEntry>();
  const idempotent = new Map<string, string>();
  const encoder = new TextEncoder();
  let eventId = 0;

  const matches = (entry: StreamEntry, event: SessionEvent): boolean =>
    event.threadKey === entry.threadKey &&
    event.id > entry.afterEventId &&
    (!entry.executionId ||
      !event.executionId ||
      event.executionId === entry.executionId);

  const write = (entry: StreamEntry, event: SessionEvent): void => {
    try {
      entry.controller.enqueue(
        encoder.encode(
          `id: ${event.id}\nevent: ${event.event}\ndata: ${event.data}\n\n`,
        ),
      );
    } catch {
      streams.delete(entry);
    }
  };

  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    // SSE streams stay open for the whole scripted execution; Bun's default
    // 10s idle timeout would sever them mid-test.
    idleTimeout: 255,
    async fetch(req) {
      const url = new URL(req.url);
      const match =
        /^\/api\/session\/([^/]+)(?:\/(messages|execute|events))?$/.exec(
          url.pathname,
        );
      if (!match) return new Response("not found", { status: 404 });
      const threadKey = decodeURIComponent(match[1] ?? "");
      const endpoint = match[2];

      if (endpoint === "events") {
        const afterEventId =
          Number.parseInt(url.searchParams.get("after_event_id") ?? "0", 10) ||
          0;
        const executionId = url.searchParams.get("execution_id") ?? undefined;
        let entry: StreamEntry | null = null;
        const stream = new ReadableStream<Uint8Array>({
          start(controller) {
            entry = { afterEventId, controller, executionId, threadKey };
            streams.add(entry);
            for (const event of events) {
              if (matches(entry, event)) write(entry, event);
            }
          },
          cancel() {
            if (entry) streams.delete(entry);
          },
        });
        return new Response(stream, {
          headers: {
            "cache-control": "no-cache",
            "content-type": "text/event-stream",
          },
        });
      }

      const body = (await req.json()) as JsonObject;
      if (endpoint === "messages") {
        appends.push({ body, threadKey });
        const messages = Array.isArray(body.messages) ? body.messages : [];
        return Response.json({
          ok: true,
          message_ids: messages.map(
            (_, index) => `msg-${appends.length}-${index}`,
          ),
        });
      }
      if (endpoint === "execute") {
        executes.push({ body, threadKey });
        const key = `${threadKey}:${String(body.idempotency_key ?? "")}`;
        const existing = idempotent.get(key);
        const executionId = existing ?? `exe-${idempotent.size + 1}`;
        if (!existing) idempotent.set(key, executionId);
        return Response.json({
          ok: true,
          execution_id: executionId,
          status: "accepted",
          thread_key: threadKey,
        });
      }
      creates.push({ body, threadKey });
      return Response.json({
        harness_type: body.harness_type,
        status: "active",
        thread_key: threadKey,
      });
    },
  });

  return {
    appends,
    creates,
    executes,
    url: `http://127.0.0.1:${server.port}`,
    streamCount: () => streams.size,
    emit(threadKey, event, data, executionId) {
      const entry: SessionEvent = {
        data: typeof data === "string" ? data : JSON.stringify(data),
        event,
        executionId,
        id: ++eventId,
        threadKey,
      };
      events.push(entry);
      for (const stream of [...streams]) {
        if (matches(stream, entry)) write(stream, entry);
      }
    },
    emitAnswerLines(threadKey, lines) {
      for (const line of lines) {
        this.emit(threadKey, "session.output.line", line);
      }
    },
    stop() {
      for (const stream of [...streams]) {
        try {
          stream.controller.close();
        } catch {
          // already closed
        }
      }
      server.stop(true);
    },
  };
}

function answerStartLine(): string {
  return JSON.stringify({
    method: "item/started",
    params: {
      threadId: "thread-1",
      turnId: "turn-1",
      startedAtMs: 1,
      item: {
        type: "agentMessage",
        id: "answer-1",
        text: "",
        phase: "final_answer",
        memoryCitation: null,
      },
    },
  });
}

function answerDeltaLine(delta: string): string {
  return JSON.stringify({
    method: "item/agentMessage/delta",
    params: {
      threadId: "thread-1",
      turnId: "turn-1",
      itemId: "answer-1",
      delta,
    },
  });
}

function turnCompletedLine(): string {
  return JSON.stringify({
    type: "turn.completed",
    turn: { id: "turn-1", items: [] },
  });
}

function answerLines(answer: string): string[] {
  return [answerStartLine(), answerDeltaLine(answer), turnCompletedLine()];
}

// ---------------------------------------------------------------------------
// Fixtures / helpers
// ---------------------------------------------------------------------------

let nextBotId = 800_000_000 + Math.floor(Math.random() * 100_000_000);
let nextUpdateId = 1;

function dmUpdate(userId: number, text: string): TelegramUpdate {
  const updateId = nextUpdateId++;
  const message: TelegramMessage = {
    message_id: updateId,
    from: { id: userId, is_bot: false, first_name: "Alice" },
    chat: { id: userId, type: "private", first_name: "Alice" },
    date: Math.floor(Date.now() / 1000),
    text,
  };
  return { update_id: updateId, message };
}

function groupCommandUpdate(chatId: number, prompt: string): TelegramUpdate {
  const updateId = nextUpdateId++;
  const command = "/ask@fake_bot";
  const message: TelegramMessage = {
    message_id: updateId,
    from: { id: 42, is_bot: false, first_name: "Alice" },
    chat: { id: chatId, type: "supergroup", title: "ops" },
    date: Math.floor(Date.now() / 1000),
    text: `${command} ${prompt}`,
    entities: [{ type: "bot_command", offset: 0, length: command.length }],
  };
  return { update_id: updateId, message };
}

async function waitFor(
  condition: () => boolean | Promise<boolean>,
  label: string,
  timeoutMs = 10_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await condition()) return;
    await Bun.sleep(25);
  }
  throw new Error(`timed out waiting for ${label}`);
}

function chatCalls(
  server: FakeTelegramServer,
  method: string,
  chatId: number,
): RecordedTelegramCall[] {
  return server
    .callsFor(method)
    .filter((call) => Number(call.params["chat_id"]) === chatId);
}

function answerSends(
  server: FakeTelegramServer,
  chatId: number,
): RecordedTelegramCall[] {
  // Narrator blurbs are italic (<i>…), answer sends are not; the scripted
  // streams below never emit reasoning, so every send in the chat is answer.
  return chatCalls(server, "sendMessage", chatId);
}

function reactionEmojis(
  server: FakeTelegramServer,
  chatId: number,
): string[] {
  return chatCalls(server, "setMessageReaction", chatId).map((call) => {
    const reaction = call.params["reaction"] as Array<{ emoji: string }>;
    return reaction[0]?.emoji ?? "";
  });
}

describe.skipIf(!databaseUrl)("telegram end-to-end emulation", () => {
  const pool: Pool = new pg.Pool({ connectionString: databaseUrl });
  const servers: FakeTelegramServer[] = [];
  const sessionApis: FakeSessionApi[] = [];
  const bots: Telegrambot[] = [];

  afterEach(async () => {
    for (const bot of bots.splice(0)) {
      await bot.shutdown().catch(() => undefined);
    }
    for (const session of sessionApis.splice(0)) session.stop();
    for (const server of servers.splice(0)) server.stop();
  });

  afterAll(async () => {
    await pool.end();
  });

  function fakeTelegram(): FakeTelegramServer {
    const server = startFakeTelegramServer({
      botUser: { id: nextBotId++, username: "fake_bot" },
    });
    servers.push(server);
    return server;
  }

  function fakeSession(): FakeSessionApi {
    const session = startFakeSessionApi();
    sessionApis.push(session);
    return session;
  }

  function startBot(
    server: FakeTelegramServer,
    session: FakeSessionApi,
    overrides: Partial<TelegrambotOptions> = {},
  ): Telegrambot {
    const bot = createTelegrambot({
      answerEditIntervalMs: 50,
      apiUrl: session.url,
      botToken: server.token,
      chatAllowlist: [],
      logger: noopLogger,
      pollTimeoutSeconds: 1,
      postgresUrl: databaseUrl ?? "",
      telegramApiUrl: server.baseUrl,
      userAllowlist: [],
      ...overrides,
    });
    bots.push(bot);
    void bot.start();
    return bot;
  }

  async function inboxRow(
    botUserId: number,
    updateId: number,
  ): Promise<{
    render_obligation: { postedMessageIds: number[] } | null;
    status: TelegramInboxStatus;
    status_reason: string | null;
  } | null> {
    const result = await pool.query(
      `SELECT status, status_reason, render_obligation
       FROM telegram_update_inbox WHERE bot_user_id = $1 AND update_id = $2`,
      [String(botUserId), updateId],
    );
    return result.rows[0] ?? null;
  }

  async function rowStatus(
    botUserId: number,
    updateId: number,
  ): Promise<TelegramInboxStatus | null> {
    return (await inboxRow(botUserId, updateId))?.status ?? null;
  }

  it(
    "streams an allowlisted DM end to end: thread key, metadata, reactions, HTML reply",
    async () => {
      const server = fakeTelegram();
      const session = fakeSession();
      const userId = 4242;
      startBot(server, session, { userAllowlist: [String(userId)] });

      const update = dmUpdate(userId, "hello centaur");
      server.queueUpdates(update);

      await waitFor(() => session.executes.length === 1, "execute accepted");
      const threadKey = `telegram:private:${userId}`;
      expect(session.executes[0]?.threadKey).toBe(threadKey);
      expect(session.creates[0]?.threadKey).toBe(threadKey);
      const metadata = session.creates[0]?.body.metadata as JsonObject;
      expect(metadata.source).toBe("telegrambot");
      expect(metadata.platform).toBe("telegram");
      expect(metadata.thread_id).toBe(threadKey);
      expect(metadata.telegram_chat_type).toBe("private");
      expect(metadata.user_id).toBe(String(userId));
      expect(metadata.telegram_conversation_name).toBe("Alice");

      session.emitAnswerLines(threadKey, answerLines("**hi** _there_"));

      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, update.update_id)) ===
          "completed",
        "inbox row completed",
      );

      const sends = answerSends(server, userId);
      expect(sends.length).toBe(1);
      expect(sends[0]?.params["text"]).toBe("<b>hi</b> <i>there</i>");
      expect(sends[0]?.params["parse_mode"]).toBe("HTML");
      expect(sends[0]?.params["reply_parameters"]).toEqual({
        message_id: update.message?.message_id,
      });
      // 👀 while working, ✅ on settle (setMessageReaction replaces the set).
      expect(reactionEmojis(server, userId)).toEqual(["👀", "✅"]);
    },
    20_000,
  );

  it(
    "routes a group /ask@bot command from an allowlisted chat to a telegram:chat key",
    async () => {
      const server = fakeTelegram();
      const session = fakeSession();
      const chatId = -100_123;
      startBot(server, session, { chatAllowlist: [String(chatId)] });

      const update = groupCommandUpdate(chatId, "deploy the thing");
      server.queueUpdates(update);

      await waitFor(() => session.executes.length === 1, "execute accepted");
      const threadKey = `telegram:chat:${chatId}`;
      expect(session.executes[0]?.threadKey).toBe(threadKey);
      // The /ask@bot prefix is stripped from the session input.
      const inputLines = JSON.stringify(session.executes[0]?.body.input_lines);
      expect(inputLines).toContain("deploy the thing");
      expect(inputLines).not.toContain("/ask");

      session.emitAnswerLines(threadKey, answerLines("done"));
      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, update.update_id)) ===
          "completed",
        "inbox row completed",
      );
      expect(answerSends(server, chatId).length).toBe(1);
    },
    20_000,
  );

  it(
    "appends a same-chat follow-up to the same session",
    async () => {
      const server = fakeTelegram();
      const session = fakeSession();
      const userId = 5151;
      startBot(server, session, { userAllowlist: [String(userId)] });
      const threadKey = `telegram:private:${userId}`;

      const first = dmUpdate(userId, "first question");
      server.queueUpdates(first);
      await waitFor(() => session.executes.length === 1, "first execute");
      session.emitAnswerLines(threadKey, answerLines("first answer"));
      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, first.update_id)) === "completed",
        "first row completed",
      );

      const second = dmUpdate(userId, "follow-up question");
      server.queueUpdates(second);
      await waitFor(() => session.executes.length === 2, "second execute");
      session.emitAnswerLines(threadKey, answerLines("second answer"));
      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, second.update_id)) ===
          "completed",
        "second row completed",
      );

      // One durable session: every create/append/execute targets the same
      // typed thread key (create metadata is re-upserted per message by
      // design, so the session identity — not the call count — is the check).
      expect(new Set(session.creates.map((c) => c.threadKey)).size).toBe(1);
      expect(session.creates[0]?.threadKey).toBe(threadKey);
      expect(session.appends.length).toBe(2);
      expect(
        session.executes.map((call) => call.body.idempotency_key),
      ).toEqual([
        `telegram:${userId}:${first.message?.message_id}`,
        `telegram:${userId}:${second.message?.message_id}`,
      ]);
    },
    25_000,
  );

  it(
    "rejects a non-allowlisted chat with zero Telegram mutations and zero api-rs calls",
    async () => {
      const server = fakeTelegram();
      const session = fakeSession();
      startBot(server, session, { chatAllowlist: ["-999"] });

      const update = groupCommandUpdate(-100_777, "let me in");
      server.queueUpdates(update);

      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, update.update_id)) === "rejected",
        "row rejected",
      );
      const row = await inboxRow(server.botUser.id, update.update_id);
      expect(row?.status_reason).toBe("not_allowlisted");
      expect(
        session.creates.length + session.appends.length + session.executes.length,
      ).toBe(0);
      for (const method of TELEGRAM_MUTATION_METHODS) {
        expect(server.callsFor(method).length).toBe(0);
      }
    },
    20_000,
  );

  it(
    "recovers a crash after execution_accepted and delivers without a second execute",
    async () => {
      const server = fakeTelegram();
      const session = fakeSession();
      const userId = 6161;
      const threadKey = `telegram:private:${userId}`;
      const botA = startBot(server, session, {
        userAllowlist: [String(userId)],
      });

      const update = dmUpdate(userId, "crash mid-flight");
      server.queueUpdates(update);
      await waitFor(() => session.executes.length === 1, "execute accepted");
      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, update.update_id)) ===
          "render_obligation_persisted",
        "obligation persisted",
      );
      // Nothing was sent yet — the scripted stream is silent.
      expect(answerSends(server, userId).length).toBe(0);

      // "Crash": stop the first instance entirely (releases the lease), then
      // bring up a fresh one against the same durable state.
      await botA.shutdown();
      startBot(server, session, { userAllowlist: [String(userId)] });

      session.emitAnswerLines(threadKey, answerLines("recovered answer"));
      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, update.update_id)) ===
          "completed",
        "row completed after recovery",
        20_000,
      );

      // Recovery resumed the persisted obligation — it never re-executed.
      expect(session.executes.length).toBe(1);
      const sends = answerSends(server, userId);
      expect(sends.length).toBe(1);
      expect(sends[0]?.params["text"]).toBe("recovered answer");
    },
    40_000,
  );

  it(
    "retries delivery after a dropped send without re-executing, and resumes with an edit once a message_id is persisted",
    async () => {
      const server = fakeTelegram();
      const session = fakeSession();
      const userId = 7171;
      const threadKey = `telegram:private:${userId}`;
      const botA = startBot(server, session, {
        userAllowlist: [String(userId)],
      });

      // Part 1: the first answer send fails (response lost); the persisted
      // obligation retries terminal delivery — duplicates allowed, but no
      // re-execution.
      server.failNext("sendMessage", { status: 500, description: "dropped" });
      const update = dmUpdate(userId, "fault injection");
      server.queueUpdates(update);
      await waitFor(() => session.executes.length === 1, "execute accepted");
      session.emitAnswerLines(threadKey, [
        answerStartLine(),
        answerDeltaLine("part one"),
      ]);
      // First attempt hits the scripted 500; the retry sweep re-renders and
      // the second send succeeds.
      await waitFor(
        async () => {
          const row = await inboxRow(server.botUser.id, update.update_id);
          return (row?.render_obligation?.postedMessageIds.length ?? 0) > 0;
        },
        "message_id persisted after retry",
        20_000,
      );
      expect(session.executes.length).toBe(1);
      // Two send attempts (the dropped one + the successful retry) at most;
      // exactly one message_id was recorded.
      expect(answerSends(server, userId).length).toBeGreaterThanOrEqual(2);

      // Part 2: crash after the message_id is durably recorded; the new
      // instance must resume with an edit / next chunk, never repost chunk 1.
      const sendsBeforeRestart = answerSends(server, userId).length;
      await botA.shutdown();
      startBot(server, session, { userAllowlist: [String(userId)] });

      session.emitAnswerLines(threadKey, [
        answerDeltaLine(" and part two"),
        turnCompletedLine(),
      ]);
      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, update.update_id)) ===
          "completed",
        "row completed after restart",
        20_000,
      );

      expect(session.executes.length).toBe(1);
      // No repost: the recorded chunk is grown in place via edits.
      expect(answerSends(server, userId).length).toBe(sendsBeforeRestart);
      const edits = chatCalls(server, "editMessageText", userId);
      expect(edits.length).toBeGreaterThanOrEqual(1);
      const lastEdit = edits[edits.length - 1];
      expect(String(lastEdit?.params["text"])).toContain("part two");
      const row = await inboxRow(server.botUser.id, update.update_id);
      expect(row?.render_obligation?.postedMessageIds.length).toBe(1);
    },
    40_000,
  );

  it(
    "splits a long answer with a code fence across the 4096 parsed-char boundary into balanced chunks",
    async () => {
      const server = fakeTelegram();
      const session = fakeSession();
      const userId = 8181;
      const threadKey = `telegram:private:${userId}`;
      startBot(server, session, { userAllowlist: [String(userId)] });

      const codeLines = Array.from(
        { length: 260 },
        (_, index) => `print("row ${index} of the generated report")`,
      ).join("\n");
      const answer = `Intro paragraph.\n\n\`\`\`python\n${codeLines}\n\`\`\`\n\nClosing remarks.`;

      const update = dmUpdate(userId, "long answer please");
      server.queueUpdates(update);
      await waitFor(() => session.executes.length === 1, "execute accepted");
      session.emitAnswerLines(threadKey, answerLines(answer));
      await waitFor(
        async () =>
          (await rowStatus(server.botUser.id, update.update_id)) ===
          "completed",
        "row completed",
        20_000,
      );

      const sends = answerSends(server, userId);
      expect(sends.length).toBeGreaterThanOrEqual(2);
      for (const send of sends) {
        const text = String(send.params["text"]);
        // Telegram's limit applies to the PARSED text; tags ride free.
        expect(parsedTextLength(text)).toBeLessThanOrEqual(4096);
        // Balanced entities in every independently valid chunk.
        expect(count(text, "<pre>")).toBe(count(text, "</pre>"));
        expect(count(text, "<code")).toBe(count(text, "</code>"));
      }
      // The fence crosses the boundary: a later chunk re-opens the
      // language-classed code block.
      const reopened = sends
        .slice(1)
        .some((send) =>
          String(send.params["text"]).startsWith(
            '<pre><code class="language-python">',
          ),
        );
      expect(reopened).toBe(true);
    },
    30_000,
  );
});

function count(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}
