import { afterEach, describe, expect, it } from "bun:test";
import type { TelegramInboxRecord, TransitionPatch } from "../src/inbox";
import type {
  TelegramDispatcher,
  TelegramInboxStore,
  TelegramOwnership,
} from "../src/index";
import {
  createActiveExecution,
  createTelegramDispatcher,
} from "../src/index";
import { createRateLimitedTelegramApi } from "../src/rate-limit";
import type { TelegramApi } from "../src/telegram-api";
import { TelegramApiError } from "../src/telegram-api";
import type {
  OwnershipLease,
  TelegramInboxStatus,
  TelegramMessage,
  TelegramUpdate,
  TelegrambotOptions,
} from "../src/types";
import { TERMINAL_INBOX_STATUSES } from "../src/types";
import { noopLogger } from "../src/utils";

/**
 * Orchestration unit tests: the dispatcher's stage machine driven end to end
 * with in-memory fakes — an in-memory inbox store (the TelegramInboxStore
 * seam), a recording fake Telegram Bot API, and a fake api-rs fetch — no
 * Postgres and no network. The Postgres-backed inbox behavior itself is
 * covered by inbox.test.ts / telegram-emulate.test.ts.
 */

const BOT_USER_ID = "999001";
const BOT_USERNAME = "fake_bot";

const NONTERMINAL: readonly TelegramInboxStatus[] = [
  "received",
  "message_appended",
  "steering_pending",
  "execution_accepted",
  "render_obligation_persisted",
];

const lease: OwnershipLease = {
  botUserId: BOT_USER_ID,
  holderId: "holder-1",
  generation: 1,
};
const ownership = (): TelegramOwnership => ({ botUserId: BOT_USER_ID, lease });

// ---------------------------------------------------------------------------
// In-memory inbox store
// ---------------------------------------------------------------------------

type FakeStore = TelegramInboxStore & {
  row(updateId: number): TelegramInboxRecord;
  rows: Map<number, TelegramInboxRecord>;
  seed(update: TelegramUpdate): void;
};

function createFakeStore(): FakeStore {
  const rows = new Map<number, TelegramInboxRecord>();
  const terminal = new Set<TelegramInboxStatus>(TERMINAL_INBOX_STATUSES);
  const nonterminal = (record: TelegramInboxRecord): boolean =>
    !terminal.has(record.status);

  const doTransition = (
    updateId: number,
    fromStatuses: readonly TelegramInboxStatus[],
    toStatus: TelegramInboxStatus,
    patch: TransitionPatch = {},
  ): boolean => {
    const row = rows.get(updateId);
    if (!row || !fromStatuses.includes(row.status)) return false;
    row.status = toStatus;
    if (patch.executionId !== undefined) row.executionId = patch.executionId;
    if (patch.renderObligation !== undefined) {
      row.renderObligation = patch.renderObligation;
    }
    if (patch.statusReason !== undefined) row.statusReason = patch.statusReason;
    row.updatedAt = new Date();
    return true;
  };

  return {
    rows,
    seed(update) {
      rows.set(update.update_id, {
        botUserId: BOT_USER_ID,
        updateId: update.update_id,
        payload: update,
        status: "received",
        threadKey: null,
        clientMessageId: null,
        executionId: null,
        statusReason: null,
        renderObligation: null,
        receivedAt: new Date(),
        updatedAt: new Date(),
      });
    },
    row(updateId) {
      const row = rows.get(updateId);
      if (!row) throw new Error(`no inbox row for update ${updateId}`);
      return row;
    },
    async acceptForProcessing(_lease, updateId, threadKey, clientMessageId) {
      const row = rows.get(updateId);
      if (!row || row.status !== "received") return false;
      row.threadKey = threadKey;
      row.clientMessageId = clientMessageId;
      return true;
    },
    async claimNextPerThread(_botUserId, excludeThreadKeys, limit) {
      const excluded = new Set(excludeThreadKeys);
      const oldestPerThread = new Map<string | null, TelegramInboxRecord>();
      const ordered = [...rows.values()].sort(
        (a, b) => a.updateId - b.updateId,
      );
      for (const row of ordered) {
        if (!nonterminal(row)) continue;
        if (row.threadKey !== null && excluded.has(row.threadKey)) continue;
        if (!oldestPerThread.has(row.threadKey)) {
          oldestPerThread.set(row.threadKey, row);
        }
      }
      return [...oldestPerThread.values()]
        .sort((a, b) => a.updateId - b.updateId)
        .slice(0, limit)
        .map((row) => ({ ...row }));
    },
    async claimThreadBacklog(_botUserId, threadKey, excludeUpdateIds, limit) {
      const excluded = new Set(excludeUpdateIds);
      return [...rows.values()]
        .filter(
          (row) =>
            nonterminal(row) &&
            row.threadKey === threadKey &&
            !excluded.has(row.updateId),
        )
        .sort((a, b) => a.updateId - b.updateId)
        .slice(0, limit)
        .map((row) => ({ ...row }));
    },
    async markIgnored(_lease, updateId, reason) {
      return doTransition(updateId, NONTERMINAL, "ignored", {
        statusReason: reason,
      });
    },
    async markRejected(_lease, updateId, reason) {
      return doTransition(updateId, NONTERMINAL, "rejected", {
        statusReason: reason,
      });
    },
    async pruneTerminal() {
      return 0;
    },
    async transition(_lease, updateId, fromStatuses, toStatus, patch) {
      return doTransition(updateId, fromStatuses, toStatus, patch);
    },
  };
}

// ---------------------------------------------------------------------------
// Recording fake Telegram Bot API (plain surface, wrapped by the real
// rate-limit scheduler with zero pacing so tests stay fast)
// ---------------------------------------------------------------------------

type ApiCall = { method: string; params: Record<string, unknown> };

type FakeTelegramApi = TelegramApi & {
  calls: ApiCall[];
  callsFor(method: string): ApiCall[];
  failNextSendMessage(error: Error): void;
};

function createFakeTelegramApi(): FakeTelegramApi {
  const calls: ApiCall[] = [];
  let nextMessageId = 5_000;
  let sendFailure: Error | null = null;
  const record = (method: string, params: unknown): void => {
    calls.push({ method, params: params as Record<string, unknown> });
  };
  return {
    calls,
    callsFor: (method) => calls.filter((call) => call.method === method),
    failNextSendMessage(error) {
      sendFailure = error;
    },
    async getMe() {
      return {
        id: Number(BOT_USER_ID),
        is_bot: true,
        first_name: "Fake",
        username: BOT_USERNAME,
      };
    },
    async getUpdates() {
      return [];
    },
    async deleteWebhook() {},
    async sendMessage(params) {
      record("sendMessage", params);
      if (sendFailure) {
        const failure = sendFailure;
        sendFailure = null;
        throw failure;
      }
      return {
        message_id: nextMessageId++,
        date: Math.floor(Date.now() / 1000),
        chat: { id: Number(params.chat_id), type: "private" as const },
        text: params.text,
      };
    },
    async editMessageText(params) {
      record("editMessageText", params);
    },
    async setMessageReaction(params) {
      record("setMessageReaction", params);
    },
    async sendChatAction(params) {
      record("sendChatAction", params);
    },
    async getFile() {
      throw new Error("getFile not stubbed");
    },
    async downloadFile() {
      throw new Error("downloadFile not stubbed");
    },
  };
}

// ---------------------------------------------------------------------------
// Fake api-rs fetch
// ---------------------------------------------------------------------------

type FakeSessionApi = {
  appends: number;
  creates: number;
  executes: number;
  failEvents: boolean;
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
};

function answerOutputLines(answer: string): string[] {
  return [
    JSON.stringify({
      method: "item/started",
      params: {
        threadId: "t",
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
    }),
    JSON.stringify({
      method: "item/agentMessage/delta",
      params: {
        threadId: "t",
        turnId: "turn-1",
        itemId: "answer-1",
        delta: answer,
      },
    }),
    JSON.stringify({ type: "turn.completed", turn: { id: "turn-1", items: [] } }),
  ];
}

function sseBody(lines: string[]): string {
  return lines
    .map(
      (line, index) =>
        `id: ${index + 1}\nevent: session.output.line\ndata: ${line}\n\n`,
    )
    .join("");
}

function createFakeSessionApi(answer: string): FakeSessionApi {
  const api: FakeSessionApi = {
    appends: 0,
    creates: 0,
    executes: 0,
    failEvents: false,
    async fetch(input) {
      const url = new URL(
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.toString()
            : input.url,
      );
      const match =
        /^\/api\/session\/([^/]+)(?:\/(messages|execute|events))?$/.exec(
          url.pathname,
        );
      if (!match) return new Response("not found", { status: 404 });
      const endpoint = match[2];
      if (endpoint === "events") {
        if (api.failEvents) {
          return new Response("unavailable", {
            status: 503,
            statusText: "Service Unavailable",
          });
        }
        return new Response(sseBody(answerOutputLines(answer)), {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        });
      }
      if (endpoint === "messages") {
        api.appends += 1;
        return Response.json({ ok: true, message_ids: [`msg-${api.appends}`] });
      }
      if (endpoint === "execute") {
        api.executes += 1;
        return Response.json({
          ok: true,
          execution_id: "exe-1",
          thread_key: decodeURIComponent(match[1] ?? ""),
          status: "accepted",
        });
      }
      api.creates += 1;
      return Response.json({ ok: true });
    },
  };
  return api;
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

function dmUpdate(updateId: number, userId: number, text: string): TelegramUpdate {
  const message: TelegramMessage = {
    message_id: updateId,
    from: { id: userId, is_bot: false, first_name: "Alice" },
    chat: { id: userId, type: "private", first_name: "Alice" },
    date: Math.floor(Date.now() / 1000),
    text,
  };
  return { update_id: updateId, message };
}

function groupUpdate(
  updateId: number,
  chatId: number,
  text: string,
): TelegramUpdate {
  const message: TelegramMessage = {
    message_id: updateId,
    from: { id: 42, is_bot: false, first_name: "Alice" },
    chat: { id: chatId, type: "supergroup", title: "ops" },
    date: Math.floor(Date.now() / 1000),
    text,
  };
  return { update_id: updateId, message };
}

type Harness = {
  api: FakeTelegramApi;
  dispatcher: TelegramDispatcher;
  session: FakeSessionApi;
  store: FakeStore;
};

const dispatchers: TelegramDispatcher[] = [];

function createHarness(input: {
  answer?: string;
  answerMaxMessages?: number;
  chatAllowlist?: string[];
  userAllowlist?: string[];
}): Harness {
  const store = createFakeStore();
  const api = createFakeTelegramApi();
  const session = createFakeSessionApi(input.answer ?? "answer text");
  const options: TelegrambotOptions = {
    answerEditIntervalMs: 10,
    ...(input.answerMaxMessages === undefined
      ? {}
      : { answerMaxMessages: input.answerMaxMessages }),
    apiUrl: "http://fake-api-rs.invalid",
    botToken: "fake-token",
    chatAllowlist: input.chatAllowlist ?? [],
    fetch: (request, init) => session.fetch(request, init),
    userAllowlist: input.userAllowlist ?? [],
  };
  const dispatcher = createTelegramDispatcher({
    api: createRateLimitedTelegramApi(api, noopLogger, {
      jitterMs: () => 0,
      messageIntervalMs: 0,
    }),
    botUsername: async () => BOT_USERNAME,
    logger: noopLogger,
    options,
    ownership,
    store,
  });
  dispatchers.push(dispatcher);
  return { api, dispatcher, session, store };
}

afterEach(async () => {
  for (const dispatcher of dispatchers.splice(0)) {
    await dispatcher.shutdown();
  }
});

async function waitFor(
  condition: () => boolean,
  label: string,
  timeoutMs = 5_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (condition()) return;
    await Bun.sleep(10);
  }
  throw new Error(`timed out waiting for ${label}`);
}

function reactionEmojis(api: FakeTelegramApi): string[] {
  return api.callsFor("setMessageReaction").map((call) => {
    const reaction = call.params.reaction as Array<{ emoji: string }>;
    return reaction[0]?.emoji ?? "";
  });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("telegram dispatcher stage machine", () => {
  it("walks an allowed DM from received to completed", async () => {
    const { api, dispatcher, session, store } = createHarness({
      answer: "**hi** there",
      userAllowlist: ["42"],
    });
    store.seed(dmUpdate(1, 42, "hello centaur"));
    await dispatcher.nudge();
    await waitFor(() => store.row(1).status === "completed", "row completed");

    expect(session.creates).toBe(1);
    expect(session.appends).toBe(1);
    expect(session.executes).toBe(1);
    expect(store.row(1).threadKey).toBe("telegram:private:42");
    expect(store.row(1).clientMessageId).toBe("telegram:42:1");
    expect(store.row(1).renderObligation?.postedMessageIds.length).toBe(1);

    const sends = api.callsFor("sendMessage");
    expect(sends.length).toBe(1);
    expect(sends[0]?.params.text).toBe("<b>hi</b> there");
    expect(sends[0]?.params.parse_mode).toBe("HTML");
    // allow_sending_without_reply: a deleted trigger message must degrade to
    // a reply-less answer, never a wedged infinite render retry.
    expect(sends[0]?.params.reply_parameters).toEqual({
      allow_sending_without_reply: true,
      message_id: 1,
    });
    // 👀 while working, replaced by 👍 on settle.
    expect(reactionEmojis(api)).toEqual(["👀", "👍"]);
  });

  it("rejects an unauthorized DM with a durable reason", async () => {
    const { api, dispatcher, session, store } = createHarness({
      userAllowlist: ["7"],
    });
    store.seed(dmUpdate(2, 42, "let me in"));
    await dispatcher.nudge();
    await waitFor(() => store.row(2).status === "rejected", "row rejected");

    expect(store.row(2).statusReason).toBe("not_allowlisted");
    expect(session.creates + session.appends + session.executes).toBe(0);
    expect(api.calls.length).toBe(0);
  });

  it("ignores a non-triggering group message", async () => {
    const { api, dispatcher, session, store } = createHarness({
      chatAllowlist: ["-1001"],
    });
    store.seed(groupUpdate(3, -1001, "just chatting"));
    await dispatcher.nudge();
    await waitFor(() => store.row(3).status === "ignored", "row ignored");

    expect(store.row(3).statusReason).toBe("not_a_trigger");
    expect(session.creates + session.appends + session.executes).toBe(0);
    expect(api.calls.length).toBe(0);
  });

  it("durably ignores a payload without .message", async () => {
    const { dispatcher, session, store } = createHarness({});
    store.seed({ update_id: 4, channel_post: { message_id: 9 } });
    await dispatcher.nudge();
    await waitFor(() => store.row(4).status === "ignored", "row ignored");

    expect(store.row(4).statusReason).toBe(
      "unsupported_update_type:channel_post",
    );
    expect(session.creates + session.appends + session.executes).toBe(0);
  });

  it("executes exactly once on double dispatch", async () => {
    const { dispatcher, session, store } = createHarness({
      userAllowlist: ["42"],
    });
    store.seed(dmUpdate(5, 42, "run it"));
    const first = dispatcher.nudge();
    const second = dispatcher.nudge();
    await Promise.all([first, second]);
    await dispatcher.nudge();
    await waitFor(() => store.row(5).status === "completed", "row completed");

    expect(session.executes).toBe(1);
    expect(session.appends).toBe(1);
  });

  it("leaves the obligation in place and settles the narrator as retrying on render failure", async () => {
    const { api, dispatcher, session, store } = createHarness({
      userAllowlist: ["42"],
    });
    session.failEvents = true;
    store.seed(dmUpdate(6, 42, "will fail to render"));
    await dispatcher.nudge();
    await waitFor(
      () =>
        store.row(6).status === "render_obligation_persisted" &&
        store.row(6).renderObligation !== null &&
        api.callsFor("setMessageReaction").length > 0,
      "render attempted",
    );
    // Let the failed attempt settle its narrator before asserting.
    await Bun.sleep(50);

    expect(session.executes).toBe(1);
    expect(store.row(6).status).toBe("render_obligation_persisted");
    expect(store.row(6).renderObligation?.executionId).toBe("exe-1");
    // "retrying" leaves 👀 in place: no 👍/👎 may be posted, and no answer.
    const emojis = reactionEmojis(api);
    expect(emojis.length).toBeGreaterThan(0);
    expect(emojis.every((emoji) => emoji === "👀")).toBe(true);
    expect(api.callsFor("sendMessage").length).toBe(0);
  });

  it("falls back to escaped plain text exactly once on a Telegram parse error", async () => {
    const { api, dispatcher, store } = createHarness({
      answer: "**bold** answer",
      userAllowlist: ["42"],
    });
    api.failNextSendMessage(
      new TelegramApiError({
        method: "sendMessage",
        status: 400,
        description: "Bad Request: can't parse entities",
      }),
    );
    store.seed(dmUpdate(7, 42, "render me"));
    await dispatcher.nudge();
    await waitFor(() => store.row(7).status === "completed", "row completed");

    const sends = api.callsFor("sendMessage");
    expect(sends.length).toBe(2);
    // First attempt was the rendered HTML Telegram rejected...
    expect(sends[0]?.params.text).toBe("<b>bold</b> answer");
    // ...the fallback is the escaped tag-free markdown, still parse_mode HTML.
    expect(sends[1]?.params.text).toBe("**bold** answer");
    expect(sends[1]?.params.parse_mode).toBe("HTML");
    expect(reactionEmojis(api)).toEqual(["👀", "👍"]);
  });
});

// ---------------------------------------------------------------------------
// Steering follow-ups (spec §2 recovery cases) and FIFO regressions. These use
// a push-style fake api-rs whose SSE stream stays open until the test emits
// events, so a follow-up can arrive while a render is genuinely live.
// ---------------------------------------------------------------------------

type LiveSessionApi = {
  appendedClientIds: string[];
  appends: number;
  conflictsRemaining: number;
  creates: number;
  emit(event: string, data: unknown): void;
  emitAnswer(answer: string): void;
  executes: number;
  failNextAppend: boolean;
  fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
  streams: number;
};

function createLiveSessionApi(): LiveSessionApi {
  const encoder = new TextEncoder();
  let controller: ReadableStreamDefaultController<Uint8Array> | null = null;
  let eventId = 0;
  const frame = (event: string, data: string): Uint8Array =>
    encoder.encode(`id: ${++eventId}\nevent: ${event}\ndata: ${data}\n\n`);
  const api: LiveSessionApi = {
    appendedClientIds: [],
    appends: 0,
    conflictsRemaining: 0,
    creates: 0,
    executes: 0,
    failNextAppend: false,
    streams: 0,
    emit(event, data) {
      controller?.enqueue(frame(event, JSON.stringify(data)));
    },
    emitAnswer(answer) {
      for (const line of answerOutputLines(answer)) {
        controller?.enqueue(frame("session.output.line", line));
      }
    },
    async fetch(input, init) {
      const url = new URL(
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.toString()
            : input.url,
      );
      const match =
        /^\/api\/session\/([^/]+)(?:\/(messages|execute|events))?$/.exec(
          url.pathname,
        );
      if (!match) return new Response("not found", { status: 404 });
      const endpoint = match[2];
      if (endpoint === "events") {
        api.streams += 1;
        const body = new ReadableStream<Uint8Array>({
          start(streamController) {
            controller = streamController;
          },
        });
        return new Response(body, {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        });
      }
      if (endpoint === "messages") {
        if (api.failNextAppend) {
          api.failNextAppend = false;
          return new Response("unavailable", {
            status: 503,
            statusText: "Service Unavailable",
          });
        }
        api.appends += 1;
        const parsed = JSON.parse(String(init?.body ?? "{}")) as {
          messages?: Array<{ client_message_id?: string }>;
        };
        api.appendedClientIds.push(
          parsed.messages?.[0]?.client_message_id ?? "",
        );
        return Response.json({ ok: true, message_ids: [`msg-${api.appends}`] });
      }
      if (endpoint === "execute") {
        if (api.conflictsRemaining > 0) {
          api.conflictsRemaining -= 1;
          return new Response("execution already active", {
            status: 409,
            statusText: "Conflict",
          });
        }
        api.executes += 1;
        return Response.json({
          ok: true,
          execution_id: `exe-${api.executes}`,
          thread_key: decodeURIComponent(match[1] ?? ""),
          status: "accepted",
        });
      }
      api.creates += 1;
      return Response.json({ ok: true });
    },
  };
  return api;
}

type LiveHarness = {
  api: FakeTelegramApi;
  dispatcher: TelegramDispatcher;
  session: LiveSessionApi;
  store: FakeStore;
};

function createLiveHarness(): LiveHarness {
  const store = createFakeStore();
  const api = createFakeTelegramApi();
  const session = createLiveSessionApi();
  const options: TelegrambotOptions = {
    answerEditIntervalMs: 10,
    apiUrl: "http://fake-api-rs.invalid",
    botToken: "fake-token",
    chatAllowlist: [],
    fetch: (request, init) => session.fetch(request, init),
    userAllowlist: ["42"],
  };
  const dispatcher = createTelegramDispatcher({
    api: createRateLimitedTelegramApi(api, noopLogger, {
      jitterMs: () => 0,
      messageIntervalMs: 0,
    }),
    botUsername: async () => BOT_USERNAME,
    logger: noopLogger,
    options,
    ownership,
    store,
  });
  dispatchers.push(dispatcher);
  return { api, dispatcher, session, store };
}

describe("steering follow-ups", () => {
  it("steers a follow-up into the live execution on steering_delivered", async () => {
    const { dispatcher, session, store } = createLiveHarness();
    store.seed(dmUpdate(1, 42, "first question"));
    await dispatcher.nudge();
    await waitFor(() => session.streams === 1, "render stream open");

    store.seed(dmUpdate(2, 42, "follow-up"));
    await dispatcher.nudge();
    await waitFor(
      () => store.row(2).status === "steering_pending",
      "follow-up steering_pending",
    );
    // The follow-up's turn already reached the durable session, in order.
    expect(session.appendedClientIds).toEqual([
      "telegram:42:1",
      "telegram:42:2",
    ]);

    // api-rs confirms delivery by the SERVER-assigned append id.
    session.emit("session.steering_delivered", {
      execution_id: "exe-1",
      message_ids: ["msg-2"],
      thread_key: "telegram:private:42",
    });
    await waitFor(() => store.row(2).status === "steered", "row steered");

    session.emitAnswer("done");
    await waitFor(() => store.row(1).status === "completed", "row completed");
    expect(session.executes).toBe(1);
  });

  it("re-executes idempotently when steering fails", async () => {
    const { dispatcher, session, store } = createLiveHarness();
    store.seed(dmUpdate(1, 42, "first question"));
    await dispatcher.nudge();
    await waitFor(() => session.streams === 1, "render stream open");

    store.seed(dmUpdate(2, 42, "follow-up"));
    await dispatcher.nudge();
    await waitFor(
      () => store.row(2).status === "steering_pending",
      "follow-up steering_pending",
    );

    session.emit("session.steering_failed", {
      execution_id: "exe-1",
      error: "execution not accepting input",
      thread_key: "telegram:private:42",
    });
    // The failed steer bounces back and waits on the still-live execution;
    // its terminal settle releases the idempotent execute, whose own render
    // opens a second stream this test must feed.
    session.emitAnswer("first answer");
    await waitFor(() => store.row(1).status === "completed", "row 1 completed");
    await waitFor(() => session.streams === 2, "second render stream open");
    session.emitAnswer("second answer");
    await waitFor(() => store.row(2).status === "completed", "row 2 completed");
    // The message was never dropped and never ran concurrently.
    expect(session.executes).toBe(2);
    expect(session.appends).toBe(2);
  });

  it("falls back to exactly one idempotent execute when the execution ends without delivering", async () => {
    const { dispatcher, session, store } = createLiveHarness();
    store.seed(dmUpdate(1, 42, "first question"));
    await dispatcher.nudge();
    await waitFor(() => session.streams === 1, "render stream open");

    store.seed(dmUpdate(2, 42, "follow-up"));
    await dispatcher.nudge();
    await waitFor(
      () => store.row(2).status === "steering_pending",
      "follow-up steering_pending",
    );

    // Execution terminates without ever acknowledging the steer; the
    // fallback execute runs its own render on a second stream.
    session.emitAnswer("first answer");
    await waitFor(() => store.row(1).status === "completed", "row 1 completed");
    await waitFor(() => session.streams === 2, "second render stream open");
    session.emitAnswer("second answer");
    await waitFor(() => store.row(2).status === "completed", "row 2 completed");
    expect(session.executes).toBe(2);
  });

  it("recovers a crash after append: resumes at message_appended without re-appending", async () => {
    const { dispatcher, session, store } = createLiveHarness();
    // Durable state left by a process that crashed after the append landed
    // but before any execute/steering disposition.
    store.seed(dmUpdate(1, 42, "recovered question"));
    const row = store.row(1);
    row.status = "message_appended";
    row.threadKey = "telegram:private:42";
    row.clientMessageId = "telegram:42:1";

    await dispatcher.nudge();
    await waitFor(() => session.streams === 1, "render stream open");
    session.emitAnswer("recovered answer");
    await waitFor(() => store.row(1).status === "completed", "row completed");
    expect(session.appends).toBe(0);
    expect(session.executes).toBe(1);
  });

  it("retries a foreign-execution conflict with backoff and executes exactly once after it ends", async () => {
    const { dispatcher, session, store } = createLiveHarness();
    // api-rs knows an execution this process does not (e.g. pre-crash);
    // the first execute conflicts, the retry after backoff is accepted.
    session.conflictsRemaining = 1;
    store.seed(dmUpdate(1, 42, "conflicted question"));
    await dispatcher.nudge();
    // The conflicted row parks as steering_pending, reverts, and retries the
    // execute after backoff — observable only by the eventual stream open
    // (the intermediate states last microseconds in-process).
    await waitFor(() => session.streams === 1, "render stream open", 10_000);
    session.emitAnswer("late answer");
    await waitFor(() => store.row(1).status === "completed", "row completed");
    expect(session.executes).toBe(1);
    expect(session.appends).toBe(1);
  });
});

describe("same-thread FIFO under transient failures", () => {
  it("defers the whole thread when the oldest row's append fails, preserving order", async () => {
    const { dispatcher, session, store } = createLiveHarness();
    session.failNextAppend = true;
    store.seed(dmUpdate(1, 42, "first"));
    store.seed(dmUpdate(2, 42, "second"));
    await dispatcher.nudge();

    // Regression: the old skip-set would append "second" past the transiently
    // failed "first", inverting the session transcript order.
    await waitFor(() => session.appends >= 1, "first append retried", 10_000);
    expect(session.appendedClientIds[0]).toBe("telegram:42:1");

    await waitFor(() => session.streams === 1, "render stream open");
    await waitFor(
      () => store.row(2).status === "steering_pending",
      "second row steered into live execution",
    );
    expect(session.appendedClientIds).toEqual([
      "telegram:42:1",
      "telegram:42:2",
    ]);

    session.emit("session.steering_delivered", {
      execution_id: "exe-1",
      message_ids: ["msg-2"],
      thread_key: "telegram:private:42",
    });
    await waitFor(() => store.row(2).status === "steered", "row 2 steered");
    session.emitAnswer("answer");
    await waitFor(() => store.row(1).status === "completed", "row 1 completed");
  });
});

describe("active execution settling", () => {
  it("resolves a wait registered after settle as terminal instead of wedging", async () => {
    // Regression: a follow-up worker can capture the ActiveExecution, await a
    // fenced transition, and register its wait after the render's finally()
    // settled existing waiters — that wait must resolve, not hang forever.
    const active = createActiveExecution("exe-1");
    active.settle();
    expect(await active.wait(7, ["msg-9"])).toBe("terminal");
  });
});

describe("answer max-messages cap", () => {
  it("collapses honestly past the cap with a truncation notice", async () => {
    const paragraph = "lorem ipsum dolor sit amet ".repeat(40).trim();
    const longAnswer = Array.from({ length: 12 }, () => paragraph).join("\n\n");
    const { api, dispatcher, store } = createHarness({
      answer: longAnswer,
      answerMaxMessages: 2,
      userAllowlist: ["42"],
    });
    store.seed(dmUpdate(1, 42, "long question"));
    await dispatcher.nudge();
    await waitFor(() => store.row(1).status === "completed", "row completed");

    const texts = [
      ...api.callsFor("sendMessage").map((call) => String(call.params.text)),
    ];
    const edits = api.callsFor("editMessageText");
    const finalTail = edits.length
      ? String(edits[edits.length - 1]?.params.text)
      : texts[texts.length - 1];
    // Never more messages than the cap, and the final one says so honestly.
    expect(texts.length).toBe(2);
    expect(finalTail).toContain("answer truncated");
    expect(store.row(1).renderObligation?.postedMessageIds.length).toBe(2);
  });
});
