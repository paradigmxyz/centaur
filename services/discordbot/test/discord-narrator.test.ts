import { describe, expect, it } from "bun:test";
import type { Logger, Thread } from "chat";
import { DiscordNarrator } from "../src/discord-narrator";
import type {
  DiscordbotApiMessage,
  DiscordbotFetch,
  DiscordbotOptions,
} from "../src/types";

const silentLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => silentLogger,
};

const EYES = encodeURIComponent("👀");
const CHECK = encodeURIComponent("✅");
const CROSS = encodeURIComponent("❌");

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

function apiMessage(
  input?: Partial<DiscordbotApiMessage>,
): DiscordbotApiMessage {
  return {
    attachments: [],
    author: {
      fullName: "User",
      isBot: false,
      isMe: false,
      userId: "U1",
      userName: "user",
    },
    id: "M1",
    isMention: true,
    raw: {},
    text: "hello",
    threadId: "discord:G1:C1:T9",
    timestamp: "2026-06-07T00:00:00.000Z",
    ...input,
  };
}

type Harness = {
  thread: Thread;
  message: DiscordbotApiMessage;
  botOptions: DiscordbotOptions;
  posts: string[];
  reactions: Array<{ method: string; url: string }>;
};

function harness(input?: {
  threadKey?: string;
  messageId?: string;
  failPosts?: boolean;
  failReactions?: boolean;
}): Harness {
  const posts: string[] = [];
  const reactions: Array<{ method: string; url: string }> = [];
  const threadKey = input?.threadKey ?? "discord:G1:C1:T9";
  const adapter = {
    postMessage: async (_threadId: string, message: unknown) => {
      if (input?.failPosts) throw new Error("post failed");
      posts.push(
        typeof message === "string"
          ? message
          : String((message as { raw?: string }).raw ?? ""),
      );
      return { id: `m${posts.length}`, raw: {}, threadId: threadKey };
    },
  };
  const fetchFn = (async (url: RequestInfo | URL, init?: RequestInit) => {
    if (input?.failReactions) throw new Error("network down");
    reactions.push({ method: init?.method ?? "GET", url: String(url) });
    return new Response(null, { status: 204 });
  }) as DiscordbotFetch;
  return {
    thread: { id: threadKey, adapter } as unknown as Thread,
    message: apiMessage({ id: input?.messageId ?? "M1", threadId: threadKey }),
    botOptions: {
      apiUrl: "http://localhost",
      applicationId: "app",
      botToken: "bot-token",
      publicKey: "key",
      discordApiUrl: "https://discord.com/api/v10",
      fetch: fetchFn,
    },
    posts,
    reactions,
  };
}

function startNarrator(
  h: Harness,
  options?: { minPostGapMs?: number; maxPosts?: number },
): DiscordNarrator {
  return DiscordNarrator.start(h.thread, h.message, h.botOptions, {
    logger: silentLogger,
    minPostGapMs: options?.minPostGapMs ?? 1,
    maxPosts: options?.maxPosts,
  });
}

describe("DiscordNarrator reactions", () => {
  it("adds 👀 to a message inside the thread via the thread channel", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("done");

    expect(h.reactions[0]).toEqual({
      method: "PUT",
      url: `https://discord.com/api/v10/channels/T9/messages/M1/reactions/${EYES}/@me`,
    });
  });

  it("routes a thread-starter message's reaction to the parent channel", async () => {
    const h = harness({ messageId: "T9" });
    const narrator = startNarrator(h);
    await narrator.finish("done");

    expect(h.reactions[0]?.url).toBe(
      `https://discord.com/api/v10/channels/C1/messages/T9/reactions/${EYES}/@me`,
    );
  });

  it("settles done as ✅ added before 👀 is removed", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("done");

    expect(h.reactions.map((r) => `${r.method} ${reactionOf(r.url)}`)).toEqual([
      `PUT ${EYES}`,
      `PUT ${CHECK}`,
      `DELETE ${EYES}`,
    ]);
  });

  it("settles as ❌ when an error task was seen", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({ id: "err-1", title: "Execution failed", status: "error" }),
    );
    await narrator.finish("done");

    expect(h.reactions.map((r) => `${r.method} ${reactionOf(r.url)}`)).toEqual([
      `PUT ${EYES}`,
      `PUT ${CROSS}`,
      `DELETE ${EYES}`,
    ]);
  });

  it("leaves 👀 in place for a retrying outcome", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    await narrator.finish("retrying");

    expect(h.reactions.map((r) => `${r.method} ${reactionOf(r.url)}`)).toEqual([
      `PUT ${EYES}`,
    ]);
  });

  it("swallows reaction failures", async () => {
    const h = harness({ failReactions: true });
    const narrator = startNarrator(h);
    await expect(narrator.finish("done")).resolves.toBeUndefined();
  });
});

describe("DiscordNarrator blurbs", () => {
  it("coalesces reasoning deltas and posts one subtext blurb when the thought completes", async () => {
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

    expect(h.posts).toEqual([
      "-# Comparing the deploy manifests against the defaults",
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

    expect(h.posts).toEqual([
      "-# Need to check the recent deploy history first",
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

    expect(h.posts).toEqual([]);
  });

  it("prefixes each line of a multi-line blurb", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({
        id: "thinking-1",
        title: "Thinking",
        status: "complete",
        details: "First line of thought\n\nSecond line of thought",
      }),
    );
    await narrator.finish("done");

    expect(h.posts).toEqual([
      "-# First line of thought\n\n-# Second line of thought",
    ]);
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

    expect(h.posts).toEqual([]);
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

    expect(h.posts).toEqual([
      "-# First completed thought here\n\n-# Second completed thought here",
    ]);
  });

  it("flushes an oversized pending thought early and truncates it", async () => {
    const h = harness();
    const narrator = startNarrator(h);
    narrator.update(
      task({ id: "reasoning-1", title: "Thinking", details: "x".repeat(700) }),
    );
    await narrator.finish("done");

    expect(h.posts).toHaveLength(1);
    expect(h.posts[0]?.length).toBeLessThanOrEqual(610);
    expect(h.posts[0]).toStartWith("-# ");
    expect(h.posts[0]).toEndWith("…");
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

    expect(h.posts.length).toBeLessThanOrEqual(2);
  });

  it("posts the pending thought during finish, before settling reactions", async () => {
    const h = harness();
    const order: string[] = [];
    const originalPost = (h.thread.adapter as { postMessage: unknown })
      .postMessage as (t: string, m: unknown) => Promise<unknown>;
    (h.thread.adapter as { postMessage: unknown }).postMessage = async (
      t: string,
      m: unknown,
    ) => {
      order.push("post");
      return originalPost(t, m);
    };
    const narrator = startNarrator(h, { minPostGapMs: 10_000 });
    narrator.update(
      task({
        id: "reasoning-1",
        title: "Thinking",
        details: "A final trailing thought",
      }),
    );
    await narrator.finish("done");

    expect(h.posts).toEqual(["-# A final trailing thought"]);
    // ✅ lands after the trailing blurb (reactions chain behind posts).
    const checkIndex = h.reactions.findIndex((r) => r.url.includes(CHECK));
    expect(checkIndex).toBeGreaterThan(-1);
    expect(order).toEqual(["post"]);
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
    expect(h.posts).toEqual([]);
  });

  it("swallows blurb post failures", async () => {
    const h = harness({ failPosts: true });
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
  });
});

function reactionOf(url: string): string {
  const match = url.match(/reactions\/([^/]+)\/@me$/);
  return match?.[1] ?? "";
}
