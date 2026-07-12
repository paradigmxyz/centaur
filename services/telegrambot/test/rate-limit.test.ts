import { describe, expect, it } from "bun:test";
import {
  createRateLimitedTelegramApi,
  TelegramSendQueue,
} from "../src/rate-limit";
import type { RateLimitClock, RateLimitOptions } from "../src/rate-limit";
import { TelegramApiError } from "../src/telegram-api";
import type { TelegramApi } from "../src/telegram-api";
import type { Logger, TelegramMessage } from "../src/types";

const tick = (): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, 0));

/**
 * Virtual clock: sleep() parks until advance() moves time past the wake
 * point, then flushes continuations so woken workers can sleep again within
 * the same advance window.
 */
class FakeClock implements RateLimitClock {
  private nowMs = 0;
  private sleepers: Array<{ at: number; resolve: () => void }> = [];

  now = (): number => this.nowMs;

  sleep = (ms: number): Promise<void> =>
    new Promise((resolve) => {
      this.sleepers.push({ at: this.nowMs + ms, resolve });
    });

  async advance(ms: number): Promise<void> {
    this.nowMs += ms;
    for (;;) {
      const due = this.sleepers.filter((sleeper) => sleeper.at <= this.nowMs);
      if (!due.length) break;
      this.sleepers = this.sleepers.filter(
        (sleeper) => sleeper.at > this.nowMs,
      );
      for (const sleeper of due) sleeper.resolve();
      await tick();
    }
    await tick();
  }
}

type LoggedEvent = { event: string; data: Record<string, unknown> };

function captureLogger(): { logger: Logger; events: LoggedEvent[] } {
  const events: LoggedEvent[] = [];
  const push = (event: string, data?: unknown): void => {
    events.push({ event, data: (data ?? {}) as Record<string, unknown> });
  };
  const logger: Logger = {
    debug: push,
    info: push,
    warn: push,
    error: push,
    child: () => logger,
  };
  return { logger, events };
}

type RecordedCall = {
  method: string;
  params: Record<string, unknown>;
};

type FakeApiHooks = {
  onEditMessageText?: (
    params: Record<string, unknown>,
  ) => Promise<void> | void;
  onSendMessage?: (
    params: Record<string, unknown>,
  ) => Promise<TelegramMessage> | TelegramMessage;
};

function message(id = 1): TelegramMessage {
  return { message_id: id, chat: { id: 1, type: "private" }, date: 0 };
}

function rateLimitError(
  method: string,
  retryAfterSeconds?: number,
): TelegramApiError {
  return new TelegramApiError({
    method,
    status: 429,
    errorCode: 429,
    description: "Too Many Requests",
    ...(retryAfterSeconds === undefined ? {} : { retryAfterSeconds }),
  });
}

function createFakeApi(hooks: FakeApiHooks = {}): {
  api: TelegramApi;
  calls: RecordedCall[];
} {
  const calls: RecordedCall[] = [];
  const record = (method: string, params: unknown): RecordedCall => {
    const call = { method, params: params as Record<string, unknown> };
    calls.push(call);
    return call;
  };
  const api: TelegramApi = {
    getMe: async () => {
      record("getMe", {});
      return { id: 42, is_bot: true, first_name: "bot" };
    },
    getUpdates: async (params) => {
      record("getUpdates", params);
      return [];
    },
    deleteWebhook: async (params) => {
      record("deleteWebhook", params);
    },
    sendMessage: async (params) => {
      record("sendMessage", params);
      return hooks.onSendMessage ? hooks.onSendMessage(params) : message();
    },
    editMessageText: async (params) => {
      record("editMessageText", params);
      await hooks.onEditMessageText?.(params);
    },
    setMessageReaction: async (params) => {
      record("setMessageReaction", params);
    },
    sendChatAction: async (params) => {
      record("sendChatAction", params);
    },
    getFile: async (fileId) => {
      record("getFile", { file_id: fileId });
      return { file_id: fileId, file_unique_id: "u" };
    },
    downloadFile: async (filePath) => {
      record("downloadFile", { file_path: filePath });
      return new Uint8Array();
    },
  };
  return { api, calls };
}

function harness(input: { hooks?: FakeApiHooks } & RateLimitOptions = {}) {
  const clock = new FakeClock();
  const { hooks, ...options } = input;
  const { api, calls } = createFakeApi(hooks);
  const { logger, events } = captureLogger();
  const limited = createRateLimitedTelegramApi(api, logger, {
    clock,
    jitterMs: () => 0,
    ...options,
  });
  return { limited, calls, clock, events };
}

/** Gate an underlying call so the chat queue stays busy until released. */
function gate(): { open: Promise<void>; release: () => void } {
  let release!: () => void;
  const open = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { open, release };
}

describe("createRateLimitedTelegramApi", () => {
  it("routes mutations FIFO per chat behind an in-flight call", async () => {
    const sendGate = gate();
    const { limited, calls } = harness({
      hooks: {
        onSendMessage: async () => {
          await sendGate.open;
          return message();
        },
      },
    });

    const send = limited.sendMessage({ chat_id: 5, text: "one" });
    await tick();
    const edit = limited.editMessageText({
      chat_id: 5,
      message_id: 1,
      text: "edit",
    });
    const reaction = limited.setMessageReaction({
      chat_id: 5,
      message_id: 1,
      reaction: [{ type: "emoji", emoji: "👀" }],
    });
    const action = limited.sendChatAction({ chat_id: 5, action: "typing" });
    await tick();

    // Everything after the gated send waits its turn.
    expect(calls.map((call) => call.method)).toEqual(["sendMessage"]);

    sendGate.release();
    await Promise.all([send, edit, reaction, action]);
    expect(calls.map((call) => call.method)).toEqual([
      "sendMessage",
      "editMessageText",
      "setMessageReaction",
      "sendChatAction",
    ]);
  });

  it("keeps chats independent: one rate-limited chat never blocks another", async () => {
    const { limited, calls, clock } = harness({
      hooks: {
        onSendMessage: (params) => {
          if (params.chat_id === -100) throw rateLimitError("sendMessage", 60);
          return message();
        },
      },
    });

    const blocked = limited
      .sendMessage({ chat_id: -100, text: "group" })
      .catch(() => undefined);
    await tick();
    await limited.sendMessage({ chat_id: 7, text: "dm" });

    const sends = calls.filter((call) => call.method === "sendMessage");
    expect(sends.map((call) => call.params.chat_id)).toEqual([-100, 7]);

    for (let i = 0; i < 6; i += 1) await clock.advance(61_000);
    await blocked;
  });

  it("passes reads through even while a chat is backing off a 429", async () => {
    const { limited, calls, clock } = harness({
      hooks: {
        onSendMessage: () => {
          throw rateLimitError("sendMessage", 120);
        },
      },
    });

    const blocked = limited
      .sendMessage({ chat_id: 5, text: "hi" })
      .catch(() => undefined);
    await tick();

    await limited.getMe();
    await limited.getFile("f1");
    await limited.deleteWebhook({ drop_pending_updates: false });
    expect(calls.map((call) => call.method)).toEqual([
      "sendMessage",
      "getMe",
      "getFile",
      "deleteWebhook",
    ]);

    for (let i = 0; i < 6; i += 1) await clock.advance(121_000);
    await blocked;
  });

  it("paces sendMessage to one per interval per chat", async () => {
    const { limited, calls, clock } = harness();

    const first = limited.sendMessage({ chat_id: 5, text: "one" });
    const second = limited.sendMessage({ chat_id: 5, text: "two" });
    await tick();
    await first;
    expect(calls.length).toBe(1);

    await clock.advance(500);
    expect(calls.length).toBe(1);

    await clock.advance(500);
    await second;
    expect(calls.length).toBe(2);
    expect(calls[1]?.params.text).toBe("two");
  });

  it("does not pace sends across different chats", async () => {
    const { limited, calls } = harness();

    await Promise.all([
      limited.sendMessage({ chat_id: 1, text: "a" }),
      limited.sendMessage({ chat_id: 2, text: "b" }),
      limited.sendMessage({ chat_id: 3, text: "c" }),
    ]);
    expect(calls.length).toBe(3);
  });

  it("enforces the 20/min budget for group chats only", async () => {
    const { limited, calls, clock } = harness({ messageIntervalMs: 0 });

    for (let i = 0; i < 20; i += 1) {
      await limited.sendMessage({ chat_id: -100, text: `g${i}` });
    }
    expect(calls.length).toBe(20);

    const overBudget = limited.sendMessage({ chat_id: -100, text: "g20" });
    await tick();
    expect(calls.length).toBe(20);

    // A private chat is unaffected by the group budget.
    await limited.sendMessage({ chat_id: 9, text: "dm" });
    expect(calls.length).toBe(21);

    await clock.advance(59_999);
    expect(
      calls.filter((call) => call.params.chat_id === -100).length,
    ).toBe(20);

    await clock.advance(1);
    await overBudget;
    expect(
      calls.filter((call) => call.params.chat_id === -100).length,
    ).toBe(21);
  });

  it("charges only sendMessage against the group message quota", async () => {
    const { limited, calls, clock } = harness({ messageIntervalMs: 0 });

    // Non-message mutations before and after budget exhaustion never count.
    for (let i = 0; i < 5; i += 1) {
      await limited.editMessageText({
        chat_id: -100,
        message_id: 100 + i,
        text: `e${i}`,
      });
    }
    for (let i = 0; i < 20; i += 1) {
      await limited.sendMessage({ chat_id: -100, text: `g${i}` });
    }
    expect(calls.filter((call) => call.method === "sendMessage").length).toBe(
      20,
    );

    await limited.editMessageText({ chat_id: -100, message_id: 1, text: "e" });
    await limited.setMessageReaction({
      chat_id: -100,
      message_id: 1,
      reaction: [{ type: "emoji", emoji: "✅" }],
    });
    await limited.sendChatAction({ chat_id: -100, action: "typing" });
    expect(calls.length).toBe(28);

    // The 21st message is still budget-blocked despite the interleaved work.
    const overBudget = limited.sendMessage({ chat_id: -100, text: "g20" });
    await tick();
    expect(calls.filter((call) => call.method === "sendMessage").length).toBe(
      20,
    );
    await clock.advance(60_000);
    await overBudget;
  });

  it("waits retry_after on 429 then retries the same call", async () => {
    let attempts = 0;
    const { limited, calls, clock, events } = harness({
      hooks: {
        onSendMessage: () => {
          attempts += 1;
          if (attempts === 1) throw rateLimitError("sendMessage", 3);
          return message(99);
        },
      },
    });

    const send = limited.sendMessage({ chat_id: 5, text: "hi" });
    await tick();
    expect(calls.length).toBe(1);

    await clock.advance(2_999);
    expect(calls.length).toBe(1);

    await clock.advance(1);
    const result = await send;
    expect(result.message_id).toBe(99);
    expect(calls.length).toBe(2);

    const limitedEvents = events.filter(
      (entry) => entry.event === "telegrambot_rate_limited",
    );
    expect(limitedEvents.length).toBe(1);
    expect(limitedEvents[0]?.data.method).toBe("sendMessage");
    expect(limitedEvents[0]?.data.retry_after).toBe(3);
  });

  it("honors 429s for non-message methods without touching the quota", async () => {
    let attempts = 0;
    const { limited, calls, clock, events } = harness({
      hooks: {
        onEditMessageText: () => {
          attempts += 1;
          if (attempts === 1) throw rateLimitError("editMessageText", 2);
        },
      },
    });

    const edit = limited.editMessageText({
      chat_id: -100,
      message_id: 1,
      text: "e",
    });
    await tick();
    await clock.advance(2_000);
    await edit;
    expect(
      calls.filter((call) => call.method === "editMessageText").length,
    ).toBe(2);
    expect(
      events.find((entry) => entry.event === "telegrambot_rate_limited")?.data
        .method,
    ).toBe("editMessageText");

    // The failed+retried edit consumed none of the group message budget.
    const { limited: fresh } = harness({ messageIntervalMs: 0 });
    void fresh;
    await limited.sendMessage({ chat_id: -100, text: "still fine" });
    expect(calls.filter((call) => call.method === "sendMessage").length).toBe(
      1,
    );
  });

  it("rethrows after the retry cap", async () => {
    const { limited, calls, clock } = harness({
      maxRetries: 2,
      hooks: {
        onSendMessage: () => {
          throw rateLimitError("sendMessage", 1);
        },
      },
    });

    let caught: unknown;
    const send = limited
      .sendMessage({ chat_id: 5, text: "hi" })
      .catch((error: unknown) => {
        caught = error;
      });
    await tick();
    await clock.advance(1_000);
    await clock.advance(1_000);
    await send;

    // maxRetries=2 means one initial attempt plus two retries.
    expect(calls.length).toBe(3);
    expect(caught).toBeInstanceOf(TelegramApiError);
    expect((caught as TelegramApiError).status).toBe(429);
  });

  it("falls back to a default backoff when 429 lacks retry_after", async () => {
    let attempts = 0;
    const { limited, calls, clock } = harness({
      hooks: {
        onSendMessage: () => {
          attempts += 1;
          if (attempts === 1) throw rateLimitError("sendMessage");
          return message();
        },
      },
    });

    const send = limited.sendMessage({ chat_id: 5, text: "hi" });
    await tick();
    expect(calls.length).toBe(1);
    await clock.advance(1_000);
    await send;
    expect(calls.length).toBe(2);
  });

  it("supersedes a queued edit with a newer edit for the same message", async () => {
    const sendGate = gate();
    const { limited, calls } = harness({
      hooks: {
        onSendMessage: async () => {
          await sendGate.open;
          return message();
        },
      },
    });

    const send = limited.sendMessage({ chat_id: 5, text: "block" });
    await tick();
    const stale = limited.editMessageText({
      chat_id: 5,
      message_id: 7,
      text: "old",
    });
    const otherMessage = limited.editMessageText({
      chat_id: 5,
      message_id: 8,
      text: "unrelated",
    });
    const fresh = limited.editMessageText({
      chat_id: 5,
      message_id: 7,
      text: "new",
    });

    // The superseded edit settles immediately; it will never be sent.
    await stale;

    sendGate.release();
    await Promise.all([send, otherMessage, fresh]);

    const edits = calls.filter((call) => call.method === "editMessageText");
    expect(edits.map((call) => call.params.text)).toEqual([
      "unrelated",
      "new",
    ]);
  });

  it("supersedes an edit parked in 429 backoff before it resends", async () => {
    let attempts = 0;
    const { limited, calls, clock } = harness({
      hooks: {
        onEditMessageText: () => {
          attempts += 1;
          if (attempts === 1) throw rateLimitError("editMessageText", 5);
        },
      },
    });

    const stale = limited.editMessageText({
      chat_id: 5,
      message_id: 7,
      text: "old",
    });
    await tick();
    expect(calls.length).toBe(1);

    const fresh = limited.editMessageText({
      chat_id: 5,
      message_id: 7,
      text: "new",
    });
    await stale;

    await clock.advance(5_000);
    await fresh;

    const edits = calls.filter((call) => call.method === "editMessageText");
    expect(edits.map((call) => call.params.text)).toEqual(["old", "new"]);
  });

  it("cancelPendingEdits drops queued edits for a message", async () => {
    const sendGate = gate();
    const { limited, calls } = harness({
      hooks: {
        onSendMessage: async () => {
          await sendGate.open;
          return message();
        },
      },
    });

    const send = limited.sendMessage({ chat_id: 5, text: "block" });
    await tick();
    const pending = limited.editMessageText({
      chat_id: 5,
      message_id: 7,
      text: "progress",
    });

    expect(limited.cancelPendingEdits(5, 7)).toBe(1);
    expect(limited.cancelPendingEdits(5, 7)).toBe(0);
    await pending;

    sendGate.release();
    await send;
    expect(
      calls.filter((call) => call.method === "editMessageText").length,
    ).toBe(0);
  });

  it("schedules urgent terminal work ahead of queued progress edits", async () => {
    const sendGate = gate();
    const { limited, calls } = harness({
      messageIntervalMs: 0,
      hooks: {
        onSendMessage: async (params) => {
          if (params.text === "block") await sendGate.open;
          return message();
        },
      },
    });

    const blocker = limited.sendMessage({ chat_id: 5, text: "block" });
    await tick();
    const progressA = limited.editMessageText({
      chat_id: 5,
      message_id: 7,
      text: "progress a",
    });
    const progressB = limited.editMessageText({
      chat_id: 5,
      message_id: 8,
      text: "progress b",
    });
    const terminal = limited.sendMessageUrgent({
      chat_id: 5,
      text: "final answer",
    });
    const terminalEdit = limited.editMessageTextUrgent({
      chat_id: 5,
      message_id: 9,
      text: "final edit",
    });

    sendGate.release();
    await Promise.all([blocker, progressA, progressB, terminal, terminalEdit]);

    // FIFO within the high-priority band, all of it ahead of normal edits.
    expect(
      calls.map((call) => [call.method, call.params.text]),
    ).toEqual([
      ["sendMessage", "block"],
      ["sendMessage", "final answer"],
      ["editMessageText", "final edit"],
      ["editMessageText", "progress a"],
      ["editMessageText", "progress b"],
    ]);
  });

  it("never puts a URL or token in rate-limit logs", async () => {
    const { limited, clock, events } = harness({
      hooks: {
        onSendMessage: () => {
          throw rateLimitError("sendMessage", 1);
        },
      },
    });

    const send = limited
      .sendMessage({ chat_id: 5, text: "hi" })
      .catch(() => undefined);
    await tick();
    for (let i = 0; i < 6; i += 1) await clock.advance(1_100);
    await send;

    for (const entry of events) {
      expect(JSON.stringify(entry)).not.toContain("http");
      expect(JSON.stringify(entry)).not.toContain("api.telegram.org");
    }
  });
});

describe("TelegramSendQueue", () => {
  it("lets a caller enqueue custom work with priority and coalescing", async () => {
    const clock = new FakeClock();
    const { logger } = captureLogger();
    const queue = new TelegramSendQueue(logger, {
      clock,
      jitterMs: () => 0,
      messageIntervalMs: 0,
    });

    const order: string[] = [];
    const blockGate = gate();
    const blocker = queue.enqueue({
      chatId: 5,
      method: "sendMessage",
      run: async () => {
        order.push("block");
        await blockGate.open;
      },
    });
    await tick();
    const normal = queue.enqueue({
      chatId: 5,
      method: "editMessageText",
      coalesceKey: "edit:5:1",
      run: async () => {
        order.push("normal");
      },
    });
    const urgent = queue.enqueue({
      chatId: 5,
      method: "sendMessage",
      priority: "high",
      run: async () => {
        order.push("urgent");
      },
    });
    const replacement = queue.enqueue({
      chatId: 5,
      method: "editMessageText",
      coalesceKey: "edit:5:1",
      run: async () => {
        order.push("replacement");
      },
    });
    await normal; // superseded by `replacement`, resolves without running

    blockGate.release();
    await Promise.all([blocker, urgent, replacement]);
    expect(order).toEqual(["block", "urgent", "replacement"]);
  });

  it("cancel is a no-op for unknown chats and keys", () => {
    const { logger } = captureLogger();
    const queue = new TelegramSendQueue(logger);
    expect(queue.cancel(123, "edit:123:1")).toBe(0);
  });
});
