import { beforeEach, describe, expect, it } from "bun:test";
import type { Logger, Thread } from "chat";
import {
  clearConversationNameCacheForTests,
  hasLiveActiveExecution,
  resolveDiscordConversationName,
  streamAnswerToThread,
} from "../src/index";
import type { DiscordbotFetch, DiscordbotOptions } from "../src/types";
import { AsyncTextQueue } from "../src/utils";

const TTL_MS = 30 * 60 * 1000;

const noopLogger = {
  debug() {},
  info() {},
  warn() {},
  error() {},
  child() {
    return noopLogger;
  },
} as unknown as Logger;

describe("hasLiveActiveExecution", () => {
  it("is false when no execution is marked", () => {
    expect(hasLiveActiveExecution({}, TTL_MS)).toBe(false);
    expect(hasLiveActiveExecution({ activeExecution: false }, TTL_MS)).toBe(
      false,
    );
  });

  it("is true for a fresh execution within the TTL", () => {
    const now = 1_000_000_000;
    expect(
      hasLiveActiveExecution(
        { activeExecution: true, activeExecutionStartedAt: now - 1_000 },
        TTL_MS,
        now,
      ),
    ).toBe(true);
    expect(
      hasLiveActiveExecution(
        { activeExecution: true, activeExecutionStartedAt: now - TTL_MS },
        TTL_MS,
        now,
      ),
    ).toBe(true);
  });

  it("treats the flag as stale past the TTL (wedged-thread escape hatch)", () => {
    const now = 1_000_000_000;
    expect(
      hasLiveActiveExecution(
        { activeExecution: true, activeExecutionStartedAt: now - TTL_MS - 1 },
        TTL_MS,
        now,
      ),
    ).toBe(false);
  });

  it("treats a flag without a timestamp (written by older code) as stale", () => {
    expect(hasLiveActiveExecution({ activeExecution: true }, TTL_MS)).toBe(
      false,
    );
    expect(
      hasLiveActiveExecution(
        { activeExecution: true, activeExecutionStartedAt: null },
        TTL_MS,
      ),
    ).toBe(false);
  });
});

type FakeMessage = { id: string; content: string };

function fakeThread(input?: { failPostAfter?: number; failEdits?: boolean }): {
  thread: Thread;
  messages: FakeMessage[];
  postCalls: () => number;
} {
  const messages: FakeMessage[] = [];
  let posts = 0;
  const adapter = {
    async postMessage(_threadId: string, message: { raw?: string }) {
      posts += 1;
      if (input?.failPostAfter !== undefined && posts > input.failPostAfter) {
        throw new Error("post failed");
      }
      const id = `msg-${posts}`;
      messages.push({ id, content: message.raw ?? "" });
      return { id, threadId: "discord:G1:C1:T1" };
    },
    async editMessage(
      _threadId: string,
      messageId: string,
      message: { raw?: string },
    ) {
      if (input?.failEdits) throw new Error("edit failed");
      const target = messages.find((item) => item.id === messageId);
      if (!target) throw new Error("unknown message");
      target.content = message.raw ?? "";
      return { id: messageId, threadId: "discord:G1:C1:T1" };
    },
  };
  const thread = {
    id: "discord:G1:C1:T1",
    adapter,
  } as unknown as Thread;
  return { thread, messages, postCalls: () => posts };
}

function botOptions(): DiscordbotOptions {
  return {
    apiUrl: "http://localhost",
    applicationId: "app",
    botToken: "token",
    publicKey: "key",
  };
}

async function runStreamer(thread: Thread, pieces: string[]): Promise<void> {
  const queue = new AsyncTextQueue();
  const done = streamAnswerToThread(thread, queue, botOptions());
  for (const piece of pieces) queue.push(piece);
  queue.end();
  await done;
}

describe("streamAnswerToThread", () => {
  it("posts a short answer as a single message", async () => {
    const { thread, messages } = fakeThread();
    await runStreamer(thread, ["Hello ", "world"]);
    expect(messages.length).toBe(1);
    expect(messages[0]?.content).toBe("Hello world");
  });

  it("splits a long answer across multiple messages, each within the cap", async () => {
    const paragraphs = Array.from(
      { length: 12 },
      (_, i) => `paragraph ${i}: ${"lorem ipsum ".repeat(40)}`,
    );
    const { thread, messages } = fakeThread();
    await runStreamer(
      thread,
      paragraphs.map((p) => `${p}\n\n`),
    );
    expect(messages.length).toBeGreaterThan(1);
    for (const message of messages) {
      expect(message.content.length).toBeLessThanOrEqual(1_900);
    }
    const combined = messages.map((m) => m.content).join("\n");
    for (let i = 0; i < paragraphs.length; i++) {
      expect(combined).toContain(`paragraph ${i}:`);
    }
    // Never the adapter's silent "..." truncation.
    expect(combined).not.toContain("...");
  });

  it("propagates a mid-stream post failure (deleted thread fails the run)", async () => {
    const { thread } = fakeThread({ failPostAfter: 0 });
    await expect(runStreamer(thread, ["hello"])).rejects.toThrow("post failed");
  });

  it("does not reject when the final edit fails; partial content stands and a note is appended", async () => {
    const { thread, messages } = fakeThread({ failEdits: true });
    // The second piece lands within the edit cadence, so the in-progress
    // message only gets its final content via the flush edit — which fails.
    await runStreamer(thread, ["partial answer ", "with a tail"]);
    expect(messages.length).toBe(2);
    expect(messages[0]?.content).toBe("partial answer ");
    expect(messages[1]?.content).toContain("⚠️");
  });
});

describe("resolveDiscordConversationName", () => {
  beforeEach(() => clearConversationNameCacheForTests());

  function options(fetchFn: DiscordbotFetch): DiscordbotOptions {
    return {
      apiUrl: "http://localhost",
      applicationId: "app",
      botToken: "token",
      discordApiUrl: "https://discord.test/api",
      fetch: fetchFn,
      publicKey: "key",
    };
  }

  it("resolves the channel name from GET /channels/{channelId}", async () => {
    const fetchFn: DiscordbotFetch = async (url) => {
      expect(String(url)).toBe("https://discord.test/api/channels/C1");
      return Response.json({ name: "general" });
    };
    expect(
      await resolveDiscordConversationName(
        options(fetchFn),
        "discord:G1:C1:T1",
        noopLogger,
      ),
    ).toBe("general");
  });

  it("caches by channel so a second thread does not re-fetch", async () => {
    let fetches = 0;
    const fetchFn: DiscordbotFetch = async () => {
      fetches += 1;
      return Response.json({ name: "general" });
    };
    const opts = options(fetchFn);
    expect(
      await resolveDiscordConversationName(opts, "discord:G1:C1:T1", noopLogger),
    ).toBe("general");
    expect(
      await resolveDiscordConversationName(opts, "discord:G1:C1:T2", noopLogger),
    ).toBe("general");
    expect(fetches).toBe(1);
  });

  it("returns undefined when the thread key has no channel segment", async () => {
    let fetched = false;
    const fetchFn: DiscordbotFetch = async () => {
      fetched = true;
      return Response.json({ name: "general" });
    };
    expect(
      await resolveDiscordConversationName(options(fetchFn), "api", noopLogger),
    ).toBeUndefined();
    expect(fetched).toBe(false);
  });

  it("returns undefined (never throws) when the channel fetch fails", async () => {
    const fetchFn: DiscordbotFetch = async () =>
      Response.json({ message: "Missing Access" }, { status: 403 });
    expect(
      await resolveDiscordConversationName(
        options(fetchFn),
        "discord:G1:C1:T1",
        noopLogger,
      ),
    ).toBeUndefined();
  });
});
