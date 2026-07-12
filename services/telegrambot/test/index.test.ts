import { afterEach, describe, expect, it } from "bun:test";
import type { TelegramInboxRecord, TransitionPatch } from "../src/inbox";
import type {
  TelegramDispatcher,
  TelegramInboxStore,
  TelegramOwnership,
} from "../src/index";
import { createTelegramDispatcher } from "../src/index";
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
  chatAllowlist?: string[];
  userAllowlist?: string[];
}): Harness {
  const store = createFakeStore();
  const api = createFakeTelegramApi();
  const session = createFakeSessionApi(input.answer ?? "answer text");
  const options: TelegrambotOptions = {
    answerEditIntervalMs: 10,
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
    expect(sends[0]?.params.reply_parameters).toEqual({ message_id: 1 });
    // 👀 while working, replaced by ✅ on settle.
    expect(reactionEmojis(api)).toEqual(["👀", "✅"]);
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
    // "retrying" leaves 👀 in place: no ✅/❌ may be posted, and no answer.
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
    expect(reactionEmojis(api)).toEqual(["👀", "✅"]);
  });
});
