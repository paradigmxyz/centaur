import { describe, expect, it } from "bun:test";
import { TelegramNarrator, setTelegramReaction } from "../src/telegram-narrator";
import type { TelegramNarratorClock } from "../src/telegram-narrator";
import { TelegramApiError } from "../src/telegram-api";
import type { TelegramApi } from "../src/telegram-api";
import type { Logger, TelegramMessage } from "../src/types";

const EYES = "👀";
const CHECK = "👍";
const CROSS = "👎";

const CHAT_ID = -1001234;
const TRIGGER_MESSAGE_ID = 41;

const tick = (): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, 0));

/**
 * Virtual clock (same seam as the rate-limit suite): sleep() parks until
 * advance() moves time past the wake point, then flushes continuations so the
 * woken typing loop can send and sleep again within the same advance window.
 */
class FakeClock implements TelegramNarratorClock {
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

type RecordedCall = { method: string; params: Record<string, unknown> };

type Hooks = {
  onSendMessage?: () => void;
  onSetMessageReaction?: () => void;
  onSendChatAction?: () => void;
};

type Harness = {
  api: TelegramApi;
  calls: RecordedCall[];
  events: LoggedEvent[];
  logger: Logger;
  posts: () => RecordedCall[];
  reactions: () => RecordedCall[];
  typing: () => RecordedCall[];
};

function harness(hooks: Hooks = {}): Harness {
  const calls: RecordedCall[] = [];
  const record = (method: string, params: unknown): void => {
    calls.push({ method, params: params as Record<string, unknown> });
  };
  const message: TelegramMessage = {
    message_id: 900,
    chat: { id: CHAT_ID, type: "supergroup" },
    date: 0,
  };
  const api: TelegramApi = {
    getMe: async () => ({ id: 42, is_bot: true, first_name: "bot" }),
    getUpdates: async () => [],
    deleteWebhook: async () => undefined,
    sendMessage: async (params) => {
      record("sendMessage", params);
      hooks.onSendMessage?.();
      return message;
    },
    editMessageText: async (params) => {
      record("editMessageText", params);
    },
    setMessageReaction: async (params) => {
      record("setMessageReaction", params);
      hooks.onSetMessageReaction?.();
    },
    sendChatAction: async (params) => {
      record("sendChatAction", params);
      hooks.onSendChatAction?.();
    },
    getFile: async (fileId) => ({ file_id: fileId, file_unique_id: "u" }),
    downloadFile: async () => new Uint8Array(),
  };
  const { logger, events } = captureLogger();
  return {
    api,
    calls,
    events,
    logger,
    posts: () => calls.filter((call) => call.method === "sendMessage"),
    reactions: () =>
      calls.filter((call) => call.method === "setMessageReaction"),
    typing: () => calls.filter((call) => call.method === "sendChatAction"),
  };
}

function startNarrator(
  h: Harness,
  options?: {
    clock?: TelegramNarratorClock;
    maxPosts?: number;
    messageThreadId?: number;
    minPostGapMs?: number;
    typingIntervalMs?: number;
  },
): TelegramNarrator {
  return TelegramNarrator.start(
    h.api,
    {
      chatId: CHAT_ID,
      messageThreadId: options?.messageThreadId,
      triggerMessageId: TRIGGER_MESSAGE_ID,
    },
    {
      logger: h.logger,
      clock: options?.clock,
      maxPosts: options?.maxPosts,
      minPostGapMs: options?.minPostGapMs ?? 1,
      typingIntervalMs: options?.typingIntervalMs,
    },
  );
}

function task(input: {
  id: string;
  title: string;
  status?: "pending" | "in_progress" | "complete" | "error";
  details?: string;
}): {
  type: "task_update";
  id: string;
  title: string;
  status: "pending" | "in_progress" | "complete" | "error";
  details?: string;
} {
  return {
    type: "task_update",
    id: input.id,
    title: input.title,
    status: input.status ?? "in_progress",
    ...(input.details ? { details: input.details } : {}),
  };
}

function emojiOf(call: RecordedCall): string {
  const reaction = call.params.reaction as Array<{ emoji: string }>;
  return reaction[0]?.emoji ?? "";
}

function postText(call: RecordedCall): string {
  return String(call.params.text ?? "");
}

describe("TelegramNarrator reactions", () => {
  it("sets 👀 on the triggering message at start", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("done");

    expect(h.reactions()[0]?.params).toEqual({
      chat_id: CHAT_ID,
      message_id: TRIGGER_MESSAGE_ID,
      reaction: [{ type: "emoji", emoji: EYES }],
    });
  });

  it("settles done with a single replacing 👍 call (no delete step)", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("done");

    expect(h.reactions().map(emojiOf)).toEqual([EYES, CHECK]);
    // Every call carries exactly one emoji: replacement, never accumulation.
    for (const call of h.reactions()) {
      expect(call.params.reaction).toHaveLength(1);
    }
  });

  it("settles as 👎 when an error task was seen", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({ id: "err-1", title: "Execution failed", status: "error" }),
    );
    await narrator.finish("done");

    expect(h.reactions().map(emojiOf)).toEqual([EYES, CROSS]);
  });

  it("settles as 👎 for a failed outcome", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("failed");

    expect(h.reactions().map(emojiOf)).toEqual([EYES, CROSS]);
  });

  it("leaves 👀 in place for a retrying outcome", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("retrying");

    expect(h.reactions().map(emojiOf)).toEqual([EYES]);
  });

  it("classifies reaction-unavailable 400s and suppresses them", async () => {
    const h = harness({
      onSetMessageReaction: () => {
        throw new TelegramApiError({
          method: "setMessageReaction",
          status: 400,
          errorCode: 400,
          description: "Bad Request: message can't be reacted",
        });
      },
    });
    const narrator = startNarrator(h);
    await expect(narrator.finish("done")).resolves.toBeUndefined();

    const classified = h.events.filter(
      (event) => event.event === "telegrambot_narrator_reaction_unavailable",
    );
    expect(classified.length).toBeGreaterThan(0);
    expect(
      h.events.some(
        (event) => event.event === "telegrambot_narrator_reaction_failed",
      ),
    ).toBe(false);
  });

  it("warns on unexpected reaction failures without throwing", async () => {
    const h = harness({
      onSetMessageReaction: () => {
        throw new Error("network down");
      },
    });
    const narrator = startNarrator(h);
    await expect(narrator.finish("done")).resolves.toBeUndefined();

    expect(
      h.events.some(
        (event) => event.event === "telegrambot_narrator_reaction_failed",
      ),
    ).toBe(true);
  });

  it("finishes settle exactly once across repeated finish calls", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await Promise.all([narrator.finish("done"), narrator.finish("failed")]);
    await narrator.finish("failed");

    expect(h.reactions().map(emojiOf)).toEqual([EYES, CHECK]);
  });
});

describe("setTelegramReaction", () => {
  it("never throws on failure", async () => {
    const h = harness({
      onSetMessageReaction: () => {
        throw new Error("boom");
      },
    });
    await expect(
      setTelegramReaction(
        h.api,
        { chatId: CHAT_ID, emoji: EYES, messageId: 7 },
        h.logger,
      ),
    ).resolves.toBeUndefined();
  });
});

describe("TelegramNarrator blurbs", () => {
  it("coalesces reasoning deltas into one italic HTML message when the thought completes", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({ id: "reasoning-1", title: "Thinking", details: "Comparing the " }),
    );
    narrator.update(
      task({
        id: "reasoning-2",
        title: "Thinking",
        status: "complete",
        details: "deploy manifests against the defaults",
      }),
    );
    await narrator.finish("done");

    expect(h.posts().map(postText)).toEqual([
      "<i>Comparing the deploy manifests against the defaults</i>",
    ]);
    expect(h.posts()[0]?.params.parse_mode).toBe("HTML");
    expect(h.posts()[0]?.params.chat_id).toBe(CHAT_ID);
  });

  it("escapes HTML in blurb content", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({
        id: "reasoning-1",
        title: "Thinking",
        status: "complete",
        details: "Compare <a> & <b> tags before rendering",
      }),
    );
    await narrator.finish("done");

    expect(h.posts().map(postText)).toEqual([
      "<i>Compare &lt;a&gt; &amp; &lt;b&gt; tags before rendering</i>",
    ]);
  });

  it("flushes the pending thought when the model moves on to a command", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({
        id: "reasoning-1",
        title: "Thinking",
        details: "Need to check the recent deploy history first",
      }),
    );
    narrator.update(task({ id: "cmd-1", title: "Command execution (1)" }));
    await narrator.finish("done");

    expect(h.posts().map(postText)).toEqual([
      "<i>Need to check the recent deploy history first</i>",
    ]);
  });

  it("never renders commands, tools, or plan updates", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update({ type: "plan_update", title: "Investigate" });
    narrator.update(
      task({ id: "cmd-1", title: "Command execution (1)", details: "ls" }),
    );
    narrator.update(task({ id: "tool-1", title: "Web search" }));
    await narrator.finish("done");

    expect(h.posts()).toEqual([]);
  });

  it("skips fragments too short to be worth a message", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({
        id: "thinking-1",
        title: "Thinking",
        status: "complete",
        details: "Hmm.",
      }),
    );
    await narrator.finish("done");

    expect(h.posts()).toEqual([]);
  });

  it("merges thoughts that complete within the min post gap into one message", async () => {
    const h = harness();
    const narrator = startNarrator(h, { minPostGapMs: 50 });
    narrator.update(
      task({
        id: "thinking-1",
        title: "Thinking",
        status: "complete",
        details: "First completed thought here",
      }),
    );
    narrator.update(
      task({
        id: "thinking-2",
        title: "Thinking",
        status: "complete",
        details: "Second completed thought here",
      }),
    );
    await new Promise((resolve) => setTimeout(resolve, 80));
    await narrator.finish("done");

    expect(h.posts().map(postText)).toEqual([
      "<i>First completed thought here</i>\n\n<i>Second completed thought here</i>",
    ]);
  });

  it("flushes an oversized pending thought early and truncates the visible text", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({ id: "reasoning-1", title: "Thinking", details: "x".repeat(700) }),
    );
    await narrator.finish("done");

    expect(h.posts()).toHaveLength(1);
    const text = postText(h.posts()[0] ?? { method: "", params: {} });
    expect(text).toStartWith("<i>");
    expect(text).toEndWith("…</i>");
    const visible = text.replaceAll("<i>", "").replaceAll("</i>", "");
    expect(visible.length).toBeLessThanOrEqual(600);
  });

  it("stops posting past the max post cap", async () => {
    const h = harness();
    const narrator = startNarrator(h, { maxPosts: 2 });
    for (let index = 0; index < 5; index++) {
      narrator.update(
        task({
          id: `thinking-${index}`,
          title: "Thinking",
          status: "complete",
          details: `Completed thought number ${index}`,
        }),
      );
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    await narrator.finish("done");

    expect(h.posts().length).toBeLessThanOrEqual(2);
  });

  it("posts the pending thought during finish, before settling the reaction", async () => {
    const h = harness();
    const narrator = startNarrator(h, { minPostGapMs: 10_000 });
    narrator.update(
      task({
        id: "reasoning-1",
        title: "Thinking",
        details: "A final trailing thought",
      }),
    );
    await narrator.finish("done");

    expect(h.posts().map(postText)).toEqual([
      "<i>A final trailing thought</i>",
    ]);
    const postIndex = h.calls.findIndex(
      (call) => call.method === "sendMessage",
    );
    const checkIndex = h.calls.findIndex(
      (call) => call.method === "setMessageReaction" && emojiOf(call) === CHECK,
    );
    expect(postIndex).toBeGreaterThan(-1);
    expect(checkIndex).toBeGreaterThan(postIndex);
  });

  it("ignores updates after finish", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("done");
    narrator.update(
      task({
        id: "thinking-1",
        title: "Thinking",
        status: "complete",
        details: "Posthumous thought that should not post",
      }),
    );
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(h.posts()).toEqual([]);
  });

  it("swallows blurb post failures", async () => {
    const h = harness({
      onSendMessage: () => {
        throw new Error("post failed");
      },
    });
    const narrator = startNarrator(h);
    narrator.update(
      task({
        id: "thinking-1",
        title: "Thinking",
        status: "complete",
        details: "A thought that will fail to post",
      }),
    );
    await expect(narrator.finish("done")).resolves.toBeUndefined();
    expect(
      h.events.some(
        (event) => event.event === "telegrambot_narrator_post_failed",
      ),
    ).toBe(true);
  });

  it("keeps blurbs in the originating forum topic; reactions carry no topic id", async () => {
    const h = harness();
    const narrator = startNarrator(h, { messageThreadId: 7 });
    narrator.update(
      task({
        id: "thinking-1",
        title: "Thinking",
        status: "complete",
        details: "Topic-scoped completed thought",
      }),
    );
    await narrator.finish("done");

    expect(h.posts()[0]?.params.message_thread_id).toBe(7);
    for (const call of h.reactions()) {
      expect("message_thread_id" in call.params).toBe(false);
    }
  });

  it("omits message_thread_id for plain (non-topic) chats", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({
        id: "thinking-1",
        title: "Thinking",
        status: "complete",
        details: "Plain-chat completed thought",
      }),
    );
    await narrator.finish("done");

    const params = h.posts()[0]?.params ?? {};
    expect("message_thread_id" in params).toBe(false);
  });
});

describe("TelegramNarrator status", () => {
  it("posts a status once and drops repeats, empties, and short fragments", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.status("Reviewing deployment manifests");
    narrator.status("Reviewing deployment manifests");
    narrator.status("   ");
    narrator.status("Short");
    await new Promise((resolve) => setTimeout(resolve, 10));
    await narrator.finish("done");

    expect(h.posts().map(postText)).toEqual([
      "<i>Reviewing deployment manifests</i>",
    ]);
  });

  it("posts distinct consecutive statuses", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.status("Reviewing deployment manifests");
    narrator.status("Cross-checking helm values");
    await new Promise((resolve) => setTimeout(resolve, 10));
    await narrator.finish("done");

    expect(h.posts().map(postText)).toEqual([
      "<i>Reviewing deployment manifests</i>\n\n<i>Cross-checking helm values</i>",
    ]);
  });
});

describe("TelegramNarrator typing keepalive", () => {
  it("sends typing immediately and then on a sub-5s interval so the indicator never lapses", async () => {
    const clock = new FakeClock();
    const h = harness();
    const narrator = startNarrator(h, { clock });
    narrator.startTypingKeepalive();
    await tick();
    expect(h.typing()).toHaveLength(1);
    expect(h.typing()[0]?.params).toEqual({ chat_id: CHAT_ID, action: "typing" });

    // The indicator lives ~5s; the keepalive must refresh strictly before
    // that, measured from the start of the previous send.
    await clock.advance(3_999);
    expect(h.typing()).toHaveLength(1);
    await clock.advance(1);
    expect(h.typing()).toHaveLength(2);
    await clock.advance(4_000);
    expect(h.typing()).toHaveLength(3);

    narrator.stopTypingKeepalive();
    await narrator.finish("done");
  });

  it("propagates the forum topic id on every chat action", async () => {
    const clock = new FakeClock();
    const h = harness();
    const narrator = startNarrator(h, { clock, messageThreadId: 7 });
    narrator.startTypingKeepalive();
    await tick();
    await clock.advance(5_000);

    expect(h.typing().length).toBeGreaterThanOrEqual(2);
    for (const call of h.typing()) {
      expect(call.params.action).toBe("typing");
      expect(call.params.message_thread_id).toBe(7);
    }
    await narrator.finish("done");
  });

  it("stop halts the cadence", async () => {
    const clock = new FakeClock();
    const h = harness();
    const narrator = startNarrator(h, { clock });
    narrator.startTypingKeepalive();
    await tick();
    await clock.advance(5_000);
    const before = h.typing().length;

    narrator.stopTypingKeepalive();
    await clock.advance(20_000);
    expect(h.typing()).toHaveLength(before);
    await narrator.finish("done");
  });

  it("finish stops the keepalive and does not hang on a parked sleep", async () => {
    const clock = new FakeClock();
    const h = harness();
    const narrator = startNarrator(h, { clock });
    narrator.startTypingKeepalive();
    await tick();

    await narrator.finish("done");
    const before = h.typing().length;
    await clock.advance(20_000);
    expect(h.typing()).toHaveLength(before);
  });

  it("survives sendChatAction failures and keeps the cadence", async () => {
    const clock = new FakeClock();
    let attempts = 0;
    const h = harness({
      onSendChatAction: () => {
        attempts += 1;
        if (attempts === 1) throw new Error("typing failed");
      },
    });
    const narrator = startNarrator(h, { clock });
    narrator.startTypingKeepalive();
    await tick();
    await clock.advance(5_000);

    expect(h.typing()).toHaveLength(2);
    expect(
      h.events.some(
        (event) => event.event === "telegrambot_narrator_typing_failed",
      ),
    ).toBe(true);
    await narrator.finish("done");
  });

  it("double start does not double the cadence", async () => {
    const clock = new FakeClock();
    const h = harness();
    const narrator = startNarrator(h, { clock });
    narrator.startTypingKeepalive();
    narrator.startTypingKeepalive();
    await tick();
    expect(h.typing()).toHaveLength(1);
    await clock.advance(5_000);
    expect(h.typing()).toHaveLength(2);
    await narrator.finish("done");
  });

  it("does not start after finish", async () => {
    const clock = new FakeClock();
    const h = harness();
    const narrator = startNarrator(h, { clock });
    await narrator.finish("done");
    narrator.startTypingKeepalive();
    await tick();
    await clock.advance(20_000);
    expect(h.typing()).toEqual([]);
  });
});
