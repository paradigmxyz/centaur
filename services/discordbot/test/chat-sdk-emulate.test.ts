// Discord port of slackbotv2/test/chat-sdk-emulate.test.ts. The `emulate`
// package has no Discord service, so the Slack emulator is replaced by a fake
// Discord REST server (node:http) that the REAL patched @chat-adapter/discord
// adapter talks to; ingress drives the adapter's forwarded-Gateway-event
// webhook path (`x-discord-gateway-token`), so the full chat SDK pipeline
// (dedupe, locks, handler routing) runs exactly as in production. The fake
// api-rs session API is ported from upstream nearly verbatim.
//
// Deliberate Discord deltas this harness encodes (NOT bugs):
// - No assistant status/title: a 👀 reaction on the triggering message via raw
//   REST (PUT .../reactions/...) settles to ✅ / ❌ instead.
// - Reasoning blurbs post as separate `-# ` subtext messages (append-only).
// - The final answer streams into separate lazily-created message(s), split
//   across multiple messages at ≤1900 chars.
// - concurrency: "drop"; no webhook ingress route on the Hono app.
// - Render-obligation state lives under `discordbot:render:*` keys.
// - Threads are renamed only when the bot itself created them.
import {
  createServer,
  type IncomingMessage,
  type Server as HttpServer,
  type ServerResponse,
} from "node:http";
import { connect } from "node:net";
import {
  afterAll,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "bun:test";
import type { ServerNotification } from "@centaur/harness-events";
import { createMemoryState } from "@chat-adapter/state-memory";
import {
  createDiscordbot,
  type Discordbot,
  type DiscordbotApiMessage,
  type DiscordbotAppendMessagesRequest,
  type DiscordbotCreateSessionRequest,
  type DiscordbotExecuteSessionRequest,
  type DiscordbotOptions,
  type DiscordbotSessionMessage,
} from "../src/index";

const BOT_TOKEN = "discordbot-emulate-token";
const APP_ID = "900000000000000001";
const USER_ID = "100000000000000001";
const TRIGGER_BOT_ID = "400000000000000001";
const GUILD_ID = "200000000000000001";
const CHANNEL_ID = "300000000000000001";
const PUBLIC_KEY = "a".repeat(64);

let discordApi: FakeDiscordApi;
let codexApi: MockSessionApi;
let bot: Discordbot;

beforeAll(async () => {
  discordApi = await startFakeDiscordApi();
  codexApi = await startMockCodexApi();
});

beforeEach(() => {
  discordApi.reset();
  codexApi.reset();
  bot = createTestBot();
});

afterAll(async () => {
  await codexApi?.close();
  await discordApi?.close();
});

describe("discordbot", () => {
  it("syncs thread context, forwards subscribed messages, and renders execute streams append-only", async () => {
    const state = createMemoryState();
    await state.connect();
    bot = createTestBot({ state });

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const parentId = discordApi.seedUserMessage(
      threadId,
      "The deploy context is above.",
    );
    const key = threadKey(threadId);
    const fileUrl = `${discordApi.url}/cdn/captured.png`;

    const firstMentionId = await dispatchMessage({
      attachments: [
        {
          content_type: "image/png",
          filename: "captured.png",
          height: 600,
          size: 16,
          url: fileUrl,
          width: 800,
        },
      ],
      channelId: threadId,
      content: `<@${APP_ID}> run with this screenshot`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitForSettle(threadId, firstMentionId);

    const followUpId = await dispatchMessage({
      channelId: threadId,
      content: "Additional detail for the subscribed thread.",
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.appends.length === 2);

    const secondMentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> now execute with the latest`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitForSettle(threadId, secondMentionId);

    expect(codexApi.creates.map((create) => create.threadKey)).toEqual([
      key,
      key,
      key,
    ]);
    expect(codexApi.appends).toHaveLength(3);
    expect(codexApi.executes).toHaveLength(2);

    const firstAppend = codexApi.appends[0]!;
    expect(firstAppend.threadKey).toBe(key);
    expect(
      firstAppend.body.messages.map((message) => message.client_message_id),
    ).toEqual([parentId, firstMentionId]);
    expect(sessionMessageTexts(firstAppend.body.messages)).toContain(
      "The deploy context is above.",
    );
    expect(
      sessionMessageTexts(firstAppend.body.messages).some((text) =>
        text.includes("run with this screenshot"),
      ),
    ).toBe(true);
    const firstAttachment = firstAppend.body.messages
      .flatMap((message) => message.parts)
      .find((part) => isRecord(part) && part.type === "attachment");
    expect(firstAttachment).toEqual(
      expect.objectContaining({
        attachment_type: "image",
        mimeType: "image/png",
        name: "captured.png",
        type: "attachment",
        url: fileUrl,
      }),
    );
    // Discord delta: attachments forward by URL, never inlined as base64.
    expect((firstAttachment as Record<string, unknown>).dataBase64).toBe(
      undefined,
    );

    const firstExecute = codexApi.executes[0]!;
    expect(firstExecute.threadKey).toBe(key);
    expect(firstExecute.body.idempotency_key).toBe(firstMentionId);
    const firstInputLine = JSON.parse(
      firstExecute.body.input_lines[0]!,
    ) as Record<string, unknown>;
    expect(firstInputLine).toEqual(
      expect.objectContaining({ type: "user", thread_key: key }),
    );
    expect(JSON.stringify(firstInputLine)).toContain(fileUrl);

    const followUpAppend = codexApi.appends[1]!;
    expect(followUpAppend.body.messages[0]?.client_message_id).toBe(followUpId);
    expect(sessionMessageTexts(followUpAppend.body.messages)).toEqual([
      "Additional detail for the subscribed thread.",
    ]);

    const secondExecute = codexApi.executes[1]!;
    expect(secondExecute.body.idempotency_key).toBe(secondMentionId);
    expect(
      JSON.stringify(JSON.parse(secondExecute.body.input_lines[0]!)),
    ).toContain("now execute with the latest");

    // Append-only narration: reasoning blurbs as `-# ` subtext messages.
    // Thoughts that complete close together may merge into one blurb, so
    // assert the contents and the per-line subtext prefix, not boundaries.
    const blurbText = blurbPostsIn(threadId).join("\n");
    expect(blurbText).toContain("Checking the command output");
    expect(blurbText).toContain("Inspecting the event stream");
    for (const blurb of blurbPostsIn(threadId)) {
      for (const line of blurb.split("\n")) {
        if (line.trim()) expect(line.startsWith("-# ")).toBe(true);
      }
    }

    // The final answers land as their own lazily-created messages.
    const answers = answerPostsIn(threadId);
    expect(
      answers.filter((text) => text.includes("Executed request 1.")),
    ).toHaveLength(1);
    expect(
      answers.filter((text) => text.includes("Executed request 2.")),
    ).toHaveLength(1);
    for (const content of botPostsIn(threadId)) {
      expect(content.trim()).not.toBe("");
      expect(content).not.toContain("tests passed");
    }

    // No Slack-style status/title; the reaction lifecycle is the indicator.
    expect(reactionsOn(threadId, firstMentionId)).toEqual([
      { emoji: "👀", method: "PUT" },
      { emoji: "✅", method: "PUT" },
      { emoji: "👀", method: "DELETE" },
    ]);
    expect(reactionsOn(threadId, secondMentionId)).toEqual([
      { emoji: "👀", method: "PUT" },
      { emoji: "✅", method: "PUT" },
      { emoji: "👀", method: "DELETE" },
    ]);

    // Pre-existing thread: never renamed.
    expect(discordApi.renames).toHaveLength(0);

    const threadState = await state.get<Record<string, unknown>>(
      `thread-state:${key}`,
    );
    expect(threadState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        renderObligation: null,
      }),
    );
  });

  it("creates, names, and answers in a bot-created thread for a channel mention", async () => {
    const mentionId = await dispatchMessage({
      channelId: CHANNEL_ID,
      content: `<@${APP_ID}> rename this thread please`,
      mention: true,
    });
    const createdThreadId = mentionId;
    const key = threadKey(createdThreadId);
    // Thread-starter messages live in the parent channel, so the reaction
    // lifecycle happens there.
    await waitForSettle(CHANNEL_ID, mentionId);

    expect(
      discordApi.calls.some(
        (call) =>
          call.method === "POST" &&
          call.path === `/channels/${CHANNEL_ID}/messages/${mentionId}/threads`,
      ),
    ).toBe(true);
    expect(discordApi.renames).toEqual([
      { channelId: createdThreadId, name: "rename this thread please" },
    ]);

    // The starter message (fetched from the parent channel) is the context.
    expect(codexApi.appends).toHaveLength(1);
    expect(
      codexApi.appends[0]!.body.messages.map(
        (message) => message.client_message_id,
      ),
    ).toEqual([mentionId]);
    expect(codexApi.executes).toHaveLength(1);
    expect(codexApi.executes[0]!.threadKey).toBe(key);

    expect(answerPostsIn(createdThreadId).join("\n")).toContain(
      "Executed request 1.",
    );
  });

  it("forwards subscribed messages to /messages without executing during a stream", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    discordApi.seedUserMessage(threadId, "Context before the long run.");
    const key = threadKey(threadId);

    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> start a long run`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    await dispatchMessage({
      channelId: threadId,
      content: "Actually queue this extra constraint.",
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.appends.length === 2);
    expect(codexApi.executes).toHaveLength(1);
    expect(sessionMessageTexts(codexApi.appends[1]!.body.messages)).toEqual([
      "Actually queue this extra constraint.",
    ]);

    codexApi.emitOutputLines(key, sampleCodexOutputLines("Long run done."));
    await waitForSettle(threadId, mentionId);
  });

  it("does not execute a second mention while a stream is already active", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    discordApi.seedUserMessage(
      threadId,
      "Context before the long mention run.",
    );
    const key = threadKey(threadId);

    const firstMentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> start a long run`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    const secondMentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> add this while still running`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.appends.length === 2);
    expect(codexApi.executes).toHaveLength(1);
    expect(codexApi.streamCount).toBe(1);
    expect(
      sessionMessageTexts(codexApi.appends[1]!.body.messages)[0],
    ).toContain("add this while still running");
    // The demoted mention gets no reaction churn; it is appended silently.
    expect(reactionsOn(threadId, secondMentionId)).toEqual([]);

    codexApi.emitOutputLines(key, sampleCodexOutputLines("First run done."));
    await waitForSettle(threadId, firstMentionId);
  });

  // Regression (a) — upstream 4c7ee514.
  it("ignores non-JSON sandbox bootstrap output lines instead of ending the stream", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> answer after bootstrap noise`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    codexApi.emitOutputLine(
      key,
      "installed 62 Centaur tool CLI shims into /home/agent/.local/bin",
    );
    codexApi.emitOutputLines(
      key,
      sampleCodexOutputLines("Answer despite bootstrap noise."),
    );

    await waitForSettle(threadId, mentionId);
    const posts = botPostsIn(threadId).join("\n");
    expect(posts).toContain("Answer despite bootstrap noise.");
    expect(posts).not.toContain(
      "Execution completed, but no final text was captured.",
    );
    expect(hasReaction(threadId, mentionId, "PUT", "❌")).toBe(false);
  });

  it("renders raw turn.failed session output as visible final text and settles ❌", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> run a failing turn`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "item.started",
        item: {
          id: "cmd-1",
          type: "commandExecution",
          command: "gh auth status",
          status: "inProgress",
        },
      }),
    );
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "turn.failed",
        error: {
          message: "Reconnecting... 2/5",
          additionalDetails: "unexpected status 502 Bad Gateway",
        },
      }),
    );

    await waitForSettle(threadId, mentionId, "❌");
    expect(answerPostsIn(threadId).join("\n")).toContain(
      "Execution failed: Reconnecting... 2/5: unexpected status 502 Bad Gateway",
    );
  });

  it("renders successful completions with no final answer as visible text", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> complete with no final text`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "item.completed",
        item: {
          id: "cmd-1",
          type: "commandExecution",
          command: "true",
          status: "completed",
          aggregatedOutput: "",
        },
      }),
    );
    codexApi.emitSessionEvent(key, "session.execution_completed", {
      execution_id: "exe-empty",
      status: "completed",
    });

    await waitForSettle(threadId, mentionId);
    expect(answerPostsIn(threadId).join("\n")).toContain(
      "Execution completed, but no final text was captured.",
    );
  });

  it("renders api-rs completion result text when no final answer delta streamed", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> complete from terminal payload`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    codexApi.emitSessionEvent(key, "session.execution_completed", {
      execution_id: "exe-terminal-result",
      status: "completed",
      result_text: "TERMINAL_RESULT_VISIBLE",
    });

    await waitForSettle(threadId, mentionId);
    const posts = answerPostsIn(threadId).join("\n");
    expect(posts).toContain("TERMINAL_RESULT_VISIBLE");
    expect(posts).not.toContain(
      "Execution completed, but no final text was captured.",
    );
  });

  it("does not duplicate final text when execution completion follows final answer deltas", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> guard against duplicate final text`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "item.started",
        item: {
          id: "answer-1",
          type: "agentMessage",
          text: "",
          phase: "final_answer",
        },
      }),
    );
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "item.agentMessage.delta",
        itemId: "answer-1",
        delta: "DUPLICATE_DELIVERY_GUARD_OK",
      }),
    );
    codexApi.emitSessionEvent(key, "session.execution_completed", {
      execution_id: "exe-duplicate-guard",
      status: "completed",
    });

    await waitForSettle(threadId, mentionId);
    const occurrences = botPostsIn(threadId)
      .join("\n")
      .split("DUPLICATE_DELIVERY_GUARD_OK").length;
    expect(occurrences - 1).toBe(1);
  });

  it("honors plain-text-only requests with a single message and no blurbs", async () => {
    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    discordApi.seedUserMessage(
      threadId,
      "Context before a plain text request.",
    );

    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> Answer from context only. Plain text only, no interactive blocks or dashboards.`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitForSettle(threadId, mentionId);

    expect(blurbPostsIn(threadId)).toEqual([]);
    expect(botPostsIn(threadId)).toHaveLength(1);
    expect(botPostsIn(threadId)[0]).toContain("Executed request 1.");
    expect(botPostsIn(threadId)[0]).not.toContain("Implementation plan");
  });

  it("puts the 👀 working reaction up before a slow execute completes", async () => {
    const releaseExecute = codexApi.holdNextExecute();

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> start visibly`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => hasReaction(threadId, mentionId, "PUT", "👀"));
    // The event stream must not open while execute is still in flight.
    expect(codexApi.eventRequests).toHaveLength(0);
    expect(hasReaction(threadId, mentionId, "PUT", "✅")).toBe(false);

    releaseExecute();
    await waitForSettle(threadId, mentionId);
    expect(answerPostsIn(threadId).join("\n")).toContain("Executed request 1.");
  });

  // Regression (f) — multi-message answer splitting, no silent truncation.
  it("splits answers longer than 1900 chars across messages with code fences reopened", async () => {
    codexApi.autoRespond = false;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> write a long fenced answer`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    const fenceLines = Array.from(
      { length: 120 },
      (_, index) => `fence-line-${index} ${"x".repeat(40)}`,
    );
    const answer = `SPLIT_ANSWER_START\n\`\`\`ts\n${fenceLines.join("\n")}\n\`\`\`\nSPLIT_ANSWER_END`;
    codexApi.emitOutputLines(key, sampleCodexOutputLines(answer));

    await waitForSettle(threadId, mentionId);
    const answers = answerPostsIn(threadId);
    expect(answers.length).toBeGreaterThan(1);
    for (const content of answers) {
      expect(content.length).toBeLessThanOrEqual(1900);
      // Code fences are closed and reopened around every split.
      const fenceMarkers = content
        .split("\n")
        .filter((line) => line.trimStart().startsWith("```"));
      expect(fenceMarkers.length % 2).toBe(0);
    }
    const combined = answers.join("\n");
    expect(combined).toContain("SPLIT_ANSWER_START");
    expect(combined).toContain("SPLIT_ANSWER_END");
    expect(combined).toContain("fence-line-0");
    expect(combined).toContain("fence-line-119");
    expect(combined).not.toContain("[truncated");
  });

  // Regression (f) — a failing final edit must not fail the run.
  it("keeps a successful run ✅ when the final answer edit fails", async () => {
    codexApi.autoRespond = false;
    discordApi.setFailMessageEdits(true);

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> answer with a tail`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "item.started",
        item: {
          id: "answer-1",
          type: "agentMessage",
          text: "",
          phase: "final_answer",
        },
      }),
    );
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "item.agentMessage.delta",
        itemId: "answer-1",
        delta: "partial answer ",
      }),
    );
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "item.agentMessage.delta",
        itemId: "answer-1",
        delta: "with a tail",
      }),
    );
    codexApi.emitOutputLine(
      key,
      JSON.stringify({
        type: "turn.completed",
        turn: { id: "turn-1", items: [] },
      }),
    );

    await waitForSettle(threadId, mentionId);
    const posts = botPostsIn(threadId);
    expect(posts.some((content) => content.includes("partial answer"))).toBe(
      true,
    );
    expect(
      posts.some((content) =>
        content.includes("The end of this answer failed to post"),
      ),
    ).toBe(true);
    expect(hasReaction(threadId, mentionId, "PUT", "❌")).toBe(false);
  });

  // Regression (g) — transient create/append failure is retried in place.
  it("retries a transient createSession failure and succeeds without user-visible error", async () => {
    codexApi.failNextCreate = true;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> survive a create blip`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitForSettle(threadId, mentionId);
    expect(codexApi.creates).toHaveLength(2);
    expect(codexApi.appends).toHaveLength(1);
    expect(codexApi.executes).toHaveLength(1);
    const posts = botPostsIn(threadId).join("\n");
    expect(posts).toContain("Executed request 1.");
    expect(posts).not.toContain("Execution failed");
  });

  it("retries a retryable execute failure inside the render stream", async () => {
    codexApi.failNextExecute = true;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> survive an execute blip`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitForSettle(threadId, mentionId);
    expect(codexApi.executes).toHaveLength(2);
    expect(
      codexApi.executes.map((execute) => execute.body.idempotency_key),
    ).toEqual([mentionId, mentionId]);
    expect(codexApi.appends).toHaveLength(1);
    const answers = answerPostsIn(threadId);
    expect(
      answers.filter((content) => content.includes("Executed request 1.")),
    ).toHaveLength(1);
  });

  it("reuses an accepted execution when the execute response is lost", async () => {
    codexApi.failNextExecuteAfterAccept = true;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> first try accepted`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitForSettle(threadId, mentionId);
    expect(codexApi.executes).toHaveLength(2);
    expect(
      codexApi.executes.map((execute) => execute.body.idempotency_key),
    ).toEqual([mentionId, mentionId]);
    const posts = botPostsIn(threadId).join("\n");
    expect(posts).toContain("Executed request 1.");
    expect(posts).not.toContain("Executed request 2.");
  });

  it("retries retryable event stream open failures after execute", async () => {
    codexApi.failNextEvents = true;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> recover after stream open failure`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitForSettle(threadId, mentionId);
    expect(codexApi.eventRequests).toHaveLength(2);
    expect(codexApi.eventRequests[0]).toEqual({
      afterEventId: 0,
      executionId: "exe-1",
      threadKey: key,
    });
    expect(codexApi.eventRequests[1]).toEqual({
      afterEventId: 0,
      executionId: "exe-1",
      threadKey: key,
    });
    expect(
      answerPostsIn(threadId).filter((content) =>
        content.includes("Executed request 1."),
      ),
    ).toHaveLength(1);
  });

  // Regression (e) — the render retry loop is bounded and settles ❌.
  it("stops the render retry loop after max attempts and settles ❌ without duplicate posts", async () => {
    const state = createMemoryState();
    await state.connect();
    bot = createTestBot({ state });
    codexApi.failAllEvents = true;

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> exhaust the retries`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitFor(() => hasReaction(threadId, mentionId, "PUT", "❌"), 40_000);
    await waitFor(() => hasReaction(threadId, mentionId, "DELETE", "👀"));

    // Initial attempt + RENDER_RETRY_MAX_ATTEMPTS retries, all idempotent.
    expect(codexApi.executes).toHaveLength(11);
    expect(
      new Set(codexApi.executes.map((execute) => execute.body.idempotency_key))
        .size,
    ).toBe(1);

    // No duplicate posting across the retries; one final failure message.
    const posts = botPostsIn(threadId);
    expect(posts).toHaveLength(1);
    expect(posts[0]).toContain("Streaming retries exhausted");

    const threadState = await state.get<Record<string, unknown>>(
      `thread-state:${key}`,
    );
    expect(threadState).toEqual(
      expect.objectContaining({ activeExecution: false }),
    );
    // The obligation is kept so a restart can still retry the render.
    expect(threadState?.renderObligation).not.toBe(null);
  }, 45_000);

  // Regression (d) — stale activeExecution flags must not wedge the thread.
  it("does not let a stale activeExecution flag block a new execution", async () => {
    const state = createMemoryState();
    await state.connect();
    bot = createTestBot({ state });

    const staleThreadId = discordApi.nextId();
    const flagOnlyThreadId = discordApi.nextId();
    const liveThreadId = discordApi.nextId();
    for (const threadId of [staleThreadId, flagOnlyThreadId, liveThreadId]) {
      discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    }
    await state.set(`thread-state:${threadKey(staleThreadId)}`, {
      activeExecution: true,
      activeExecutionStartedAt: Date.now() - 31 * 60 * 1000,
    });
    await state.set(`thread-state:${threadKey(flagOnlyThreadId)}`, {
      activeExecution: true,
    });
    await state.set(`thread-state:${threadKey(liveThreadId)}`, {
      activeExecution: true,
      activeExecutionStartedAt: Date.now(),
    });

    const staleMentionId = await dispatchMessage({
      channelId: staleThreadId,
      content: `<@${APP_ID}> run despite the stale flag`,
      mention: true,
      thread: { id: staleThreadId, parentId: CHANNEL_ID },
    });
    await waitForSettle(staleThreadId, staleMentionId);

    const flagOnlyMentionId = await dispatchMessage({
      channelId: flagOnlyThreadId,
      content: `<@${APP_ID}> run despite the timestampless flag`,
      mention: true,
      thread: { id: flagOnlyThreadId, parentId: CHANNEL_ID },
    });
    await waitForSettle(flagOnlyThreadId, flagOnlyMentionId);

    await dispatchMessage({
      channelId: liveThreadId,
      content: `<@${APP_ID}> blocked by a live flag`,
      mention: true,
      thread: { id: liveThreadId, parentId: CHANNEL_ID },
    });
    await waitFor(() =>
      codexApi.appends.some(
        (append) => append.threadKey === threadKey(liveThreadId),
      ),
    );

    expect(codexApi.executes.map((execute) => execute.threadKey)).toEqual([
      threadKey(staleThreadId),
      threadKey(flagOnlyThreadId),
    ]);
  });

  // Regression (h) — contentless execute-mode messages skip execution.
  it("skips execution for a contentless mention and reacts ❓", async () => {
    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: "",
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitFor(() => hasReaction(threadId, mentionId, "PUT", "❓"));
    expect(codexApi.creates).toHaveLength(0);
    expect(codexApi.appends).toHaveLength(0);
    expect(codexApi.executes).toHaveLength(0);
    expect(hasReaction(threadId, mentionId, "PUT", "👀")).toBe(false);
  });

  // Regression (i) — per-guild concurrency cap with slot release on settle.
  it("caps concurrent executions per guild with 🚦 and releases the slot after settle", async () => {
    bot = createTestBot({ maxConcurrentExecutionsPerGuild: 1 });
    codexApi.autoRespond = false;

    const threadA = discordApi.nextId();
    const threadB = discordApi.nextId();
    const threadC = discordApi.nextId();
    for (const threadId of [threadA, threadB, threadC]) {
      discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    }

    const mentionA = await dispatchMessage({
      channelId: threadA,
      content: `<@${APP_ID}> hold the only slot`,
      mention: true,
      thread: { id: threadA, parentId: CHANNEL_ID },
    });
    await waitFor(() => codexApi.executes.length === 1);
    await waitFor(() => codexApi.streamCount === 1);

    const mentionB = await dispatchMessage({
      channelId: threadB,
      content: `<@${APP_ID}> over the cap`,
      mention: true,
      thread: { id: threadB, parentId: CHANNEL_ID },
    });
    await waitFor(() => hasReaction(threadB, mentionB, "PUT", "🚦"));
    // Demoted to append-only context; never executed.
    await waitFor(() =>
      codexApi.appends.some(
        (append) => append.threadKey === threadKey(threadB),
      ),
    );
    expect(codexApi.executes).toHaveLength(1);

    codexApi.emitOutputLines(
      threadKey(threadA),
      sampleCodexOutputLines("First guarded answer."),
    );
    await waitForSettle(threadA, mentionA);
    await sleep(100);

    codexApi.autoRespond = true;
    const mentionC = await dispatchMessage({
      channelId: threadC,
      content: `<@${APP_ID}> take the freed slot`,
      mention: true,
      thread: { id: threadC, parentId: CHANNEL_ID },
    });
    await waitForSettle(threadC, mentionC);
    expect(codexApi.executes).toHaveLength(2);
    expect(codexApi.executes[1]!.threadKey).toBe(threadKey(threadC));
  });

  // Regression (j) — permission errors during context collection are surfaced.
  it("posts the missing-permissions message and settles ❌ on a history 403", async () => {
    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    discordApi.failChannelHistory(
      threadId,
      403,
      '{"message": "Missing Access", "code": 50001}',
    );

    const mentionId = await dispatchMessage({
      channelId: threadId,
      content: `<@${APP_ID}> read the history`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });

    await waitFor(() => hasReaction(threadId, mentionId, "PUT", "❌"));
    expect(botPostsIn(threadId).join("\n")).toContain(
      "missing permissions (Read Message History)",
    );
    expect(codexApi.creates).toHaveLength(0);
    expect(codexApi.executes).toHaveLength(0);
  });

  it("recovers unfinished render obligations from Chat SDK state on startup", async () => {
    const sharedState = createMemoryState();
    await sharedState.connect();

    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const key = threadKey(threadId);
    const mentionId = discordApi.seedUserMessage(
      threadId,
      "recover a completed run",
    );
    const message = recoveryApiMessage(
      key,
      mentionId,
      "recover a completed run",
    );
    await sharedState.set(`thread-state:${key}`, {
      activeExecution: true,
      executedMessageIds: [mentionId],
      forwardedMessageIds: [mentionId],
      historyForwarded: true,
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: "exe-recovery",
        message,
      },
    });
    await sharedState.appendToList("discordbot:render:index", key);
    codexApi.emitOutputLines(key, sampleCodexOutputLines("Recovered request."));

    bot = createTestBot({
      recoverRenderObligationsOnStart: true,
      state: sharedState,
    });

    await waitFor(() => codexApi.eventRequests.length === 1, 3000);
    await waitForSettle(threadId, mentionId);

    expect(codexApi.creates).toHaveLength(0);
    expect(codexApi.appends).toHaveLength(0);
    expect(codexApi.executes).toHaveLength(0);
    expect(codexApi.eventRequests).toEqual([
      { afterEventId: 0, executionId: "exe-recovery", threadKey: key },
    ]);
    expect(answerPostsIn(threadId).join("\n")).toContain("Recovered request.");

    const recoveredThreadState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${key}`,
    );
    expect(recoveredThreadState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        lastEventId: expect.any(Number),
        renderObligation: null,
      }),
    );
    expect(Number(recoveredThreadState?.lastEventId)).toBeGreaterThan(0);
  });

  // Regression (c) — upstream d6953481: per-thread isolation in recovery.
  it("recovers healthy render obligations even when another one is poisoned", async () => {
    const sharedState = createMemoryState();
    await sharedState.connect();

    const poisonedKey = `discord:${GUILD_ID}:${CHANNEL_ID}:${discordApi.nextId()}`;
    // A corrupt obligation (no message) makes recovery of this thread throw.
    await sharedState.set(`thread-state:${poisonedKey}`, {
      renderObligation: { afterEventId: 0, executionId: "exe-poisoned" },
    });

    const healthyThreadId = discordApi.nextId();
    discordApi.seedThreadChannel(healthyThreadId, CHANNEL_ID);
    const healthyKey = threadKey(healthyThreadId);
    const healthyMentionId = discordApi.seedUserMessage(
      healthyThreadId,
      "recover the healthy run",
    );
    await sharedState.set(`thread-state:${healthyKey}`, {
      activeExecution: true,
      historyForwarded: true,
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: "exe-healthy",
        message: recoveryApiMessage(
          healthyKey,
          healthyMentionId,
          "recover the healthy run",
        ),
      },
    });
    // The poisoned thread comes FIRST so an aborting scan would skip the rest.
    await sharedState.appendToList("discordbot:render:index", poisonedKey);
    await sharedState.appendToList("discordbot:render:index", healthyKey);
    codexApi.emitOutputLines(
      healthyKey,
      sampleCodexOutputLines("Recovered around the poison."),
    );

    bot = createTestBot({
      recoverRenderObligationsOnStart: true,
      state: sharedState,
    });

    await waitForSettle(healthyThreadId, healthyMentionId);
    expect(answerPostsIn(healthyThreadId).join("\n")).toContain(
      "Recovered around the poison.",
    );
    expect(
      codexApi.eventRequests.filter(
        (request) => request.threadKey === healthyKey,
      ),
    ).toHaveLength(1);
    const healthyState = await sharedState.get<Record<string, unknown>>(
      `thread-state:${healthyKey}`,
    );
    expect(healthyState).toEqual(
      expect.objectContaining({
        activeExecution: false,
        renderObligation: null,
      }),
    );

    // Stop the background recovery loop from re-scanning the poisoned thread.
    await sharedState.set(`thread-state:${poisonedKey}`, {
      renderObligation: null,
    });
  });

  // Regression (b) — upstream d6953481: oversized attachments degrade.
  it("degrades oversized attachments via fetchError without buffering a download", async () => {
    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const hugeUrl = `${discordApi.url}/cdn/huge.zip`;

    const mentionId = await dispatchMessage({
      attachments: [
        {
          content_type: "application/zip",
          filename: "huge.zip",
          size: 200 * 1024 * 1024,
          url: hugeUrl,
        },
      ],
      channelId: threadId,
      content: `<@${APP_ID}> summarize this archive`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await waitForSettle(threadId, mentionId);

    const attachmentPart = codexApi.appends
      .flatMap((append) => append.body.messages)
      .flatMap((message) => message.parts)
      .find((part) => isRecord(part) && part.type === "attachment") as
      | Record<string, unknown>
      | undefined;
    expect(attachmentPart).toBeDefined();
    expect(String(attachmentPart?.fetchError)).toContain(
      "attachment too large to inline",
    );
    expect(attachmentPart?.dataBase64).toBe(undefined);

    expect(codexApi.executes).toHaveLength(1);
    expect(codexApi.executes[0]!.body.input_lines[0]).toContain(
      "fetch_error=attachment too large to inline",
    );

    // The oversized file is never downloaded.
    expect(
      discordApi.calls.some((call) => call.path.startsWith("/cdn/huge.zip")),
    ).toBe(false);
  });

  it("keeps fail-closed guild and trigger-bot allowlist behavior", async () => {
    // A mention from a non-allowlisted guild is dropped before any mutation.
    await dispatchMessage({
      channelId: CHANNEL_ID,
      content: `<@${APP_ID}> from an external guild`,
      guildId: "999999999999999999",
      mention: true,
    });
    await sleep(50);
    expect(
      discordApi.calls.some((call) => call.path.endsWith("/threads")),
    ).toBe(false);
    expect(codexApi.creates).toHaveLength(0);
    expect(codexApi.executes).toHaveLength(0);

    // A bot-authored mention is ignored unless the bot is allowlisted.
    const threadId = discordApi.nextId();
    discordApi.seedThreadChannel(threadId, CHANNEL_ID);
    const deniedBotMentionId = await dispatchMessage({
      authorBot: true,
      authorId: TRIGGER_BOT_ID,
      channelId: threadId,
      content: `<@${APP_ID}> from another bot`,
      mention: true,
      thread: { id: threadId, parentId: CHANNEL_ID },
    });
    await sleep(50);
    expect(codexApi.executes).toHaveLength(0);
    expect(reactionsOn(threadId, deniedBotMentionId)).toEqual([]);

    bot = createTestBot({ triggerBotAllowlist: [TRIGGER_BOT_ID] });
    const allowedThreadId = discordApi.nextId();
    discordApi.seedThreadChannel(allowedThreadId, CHANNEL_ID);
    const allowedBotMentionId = await dispatchMessage({
      authorBot: true,
      authorId: TRIGGER_BOT_ID,
      channelId: allowedThreadId,
      content: `<@${APP_ID}> from an allowed bot`,
      mention: true,
      thread: { id: allowedThreadId, parentId: CHANNEL_ID },
    });
    await waitForSettle(allowedThreadId, allowedBotMentionId);
    expect(codexApi.executes).toHaveLength(1);
  });
});

function createTestBot(overrides: Partial<DiscordbotOptions> = {}): Discordbot {
  return createDiscordbot({
    apiKey: "discordbot-test-key",
    apiUrl: codexApi.url,
    applicationId: APP_ID,
    botToken: BOT_TOKEN,
    discordApiUrl: discordApi.url,
    guildAllowlist: [GUILD_ID],
    publicKey: PUBLIC_KEY,
    recoverRenderObligationsOnStart: false,
    state: createMemoryState(),
    ...overrides,
  });
}

function threadKey(threadId: string): string {
  return `discord:${GUILD_ID}:${CHANNEL_ID}:${threadId}`;
}

/**
 * Seeds the raw message into the fake Discord store and dispatches it to the
 * bot through the REAL adapter's forwarded-Gateway-event webhook path, the
 * production ingress shape (`startGatewayListener` direct mode constructs the
 * identical payloads).
 */
async function dispatchMessage(input: {
  attachments?: Record<string, unknown>[];
  authorBot?: boolean;
  authorId?: string;
  channelId: string;
  content: string;
  guildId?: string;
  mention?: boolean;
  thread?: { id: string; parentId: string };
}): Promise<string> {
  const raw = discordApi.seedRawMessage(input.channelId, {
    attachments: input.attachments ?? [],
    author: {
      bot: input.authorBot === true,
      global_name: "Test User",
      id: input.authorId ?? USER_ID,
      username: "tester",
    },
    content: input.content,
  });
  const data: Record<string, unknown> = {
    ...raw,
    guild_id: input.guildId ?? GUILD_ID,
    mention_roles: [],
    mentions: input.mention ? [{ id: APP_ID }] : [],
    ...(input.thread
      ? { thread: { id: input.thread.id, parent_id: input.thread.parentId } }
      : {}),
  };
  const response = await bot.chat.webhooks.discord!(
    new Request("http://discordbot.test/gateway", {
      body: JSON.stringify({
        type: "GATEWAY_MESSAGE_CREATE",
        timestamp: new Date().toISOString(),
        data,
      }),
      headers: {
        "content-type": "application/json",
        "x-discord-gateway-token": BOT_TOKEN,
      },
      method: "POST",
    }),
    { waitUntil: () => undefined },
  );
  expect(response.status).toBe(200);
  return String(raw.id);
}

function recoveryApiMessage(
  key: string,
  messageId: string,
  text: string,
): DiscordbotApiMessage {
  return {
    attachments: [],
    author: {
      fullName: "Test User",
      isBot: false,
      isMe: false,
      userId: USER_ID,
      userName: "tester",
    },
    id: messageId,
    isMention: true,
    raw: {},
    text,
    threadId: key,
    timestamp: new Date().toISOString(),
  };
}

function botPostsIn(channelId: string): string[] {
  return discordApi
    .messagesIn(channelId)
    .filter((message) => message.author.id === APP_ID)
    .map((message) => message.content);
}

function blurbPostsIn(channelId: string): string[] {
  return botPostsIn(channelId).filter((content) => content.startsWith("-# "));
}

function answerPostsIn(channelId: string): string[] {
  return botPostsIn(channelId).filter((content) => !content.startsWith("-# "));
}

function reactionsOn(
  channelId: string,
  messageId: string,
): { emoji: string; method: string }[] {
  return discordApi.reactionCalls
    .filter(
      (call) => call.channelId === channelId && call.messageId === messageId,
    )
    .map((call) => ({ emoji: call.emoji, method: call.method }));
}

function hasReaction(
  channelId: string,
  messageId: string,
  method: "PUT" | "DELETE",
  emoji: string,
): boolean {
  return discordApi.reactionCalls.some(
    (call) =>
      call.channelId === channelId &&
      call.messageId === messageId &&
      call.method === method &&
      call.emoji === emoji,
  );
}

async function waitForSettle(
  channelId: string,
  messageId: string,
  emoji: "✅" | "❌" = "✅",
  timeoutMs = 5000,
): Promise<void> {
  await waitFor(
    () => hasReaction(channelId, messageId, "PUT", emoji),
    timeoutMs,
  );
  await waitFor(
    () => hasReaction(channelId, messageId, "DELETE", "👀"),
    timeoutMs,
  );
}

function sessionMessageTexts(messages: DiscordbotSessionMessage[]): string[] {
  return messages.flatMap((message) =>
    message.parts.flatMap((part) => {
      if (
        isRecord(part) &&
        part.type === "text" &&
        typeof part.text === "string"
      ) {
        return [part.text];
      }
      return [];
    }),
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function sampleCodexNotifications(answer: string): ServerNotification[] {
  return [
    {
      method: "turn/started",
      params: {
        threadId: "thread-1",
        turn: {
          id: "turn-1",
          items: [],
          itemsView: "full",
          status: "inProgress",
          error: null,
          startedAt: 1,
          completedAt: null,
          durationMs: null,
        },
      },
    },
    {
      method: "item/started",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        startedAtMs: 2,
        item: {
          type: "agentMessage",
          id: "commentary-1",
          text: "",
          phase: "commentary",
          memoryCitation: null,
        },
      },
    },
    {
      method: "item/started",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        startedAtMs: 4,
        item: {
          type: "agentMessage",
          id: "answer-1",
          text: "",
          phase: "final_answer",
          memoryCitation: null,
        },
      },
    },
    {
      method: "item/agentMessage/delta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "commentary-1",
        delta: "Checking the command output",
      },
    },
    {
      method: "item/reasoning/summaryTextDelta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "reasoning-1",
        summaryIndex: 0,
        delta: "Inspecting the event stream",
      },
    },
    {
      method: "item/completed",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        completedAtMs: 2,
        item: {
          type: "agentMessage",
          id: "commentary-1",
          text: "Checking the command output",
          phase: "commentary",
          memoryCitation: null,
        },
      },
    },
    {
      method: "turn/plan/updated",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        explanation: "Implementation plan",
        plan: [
          { step: "Inspect App Server events", status: "completed" },
          { step: "Stream Chat SDK chunks", status: "inProgress" },
        ],
      },
    },
    {
      method: "item/started",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        startedAtMs: 2,
        item: {
          type: "commandExecution",
          id: "cmd-1",
          command: "pnpm test",
          cwd: "/repo",
          processId: "proc-1",
          source: "agent",
          status: "inProgress",
          commandActions: [],
          aggregatedOutput: null,
          exitCode: null,
          durationMs: null,
        },
      },
    },
    {
      method: "item/commandExecution/outputDelta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "cmd-1",
        delta: "tests passed\n",
      },
    },
    {
      method: "item/completed",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        completedAtMs: 3,
        item: {
          type: "commandExecution",
          id: "cmd-1",
          command: "pnpm test",
          cwd: "/repo",
          processId: "proc-1",
          source: "agent",
          status: "completed",
          commandActions: [],
          aggregatedOutput: "tests passed\n",
          exitCode: 0,
          durationMs: 50,
        },
      },
    },
    {
      method: "item/agentMessage/delta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "answer-1",
        delta: answer,
      },
    },
  ] as unknown as ServerNotification[];
}

function sampleCodexOutputLines(answer: string): string[] {
  return [
    ...sampleCodexNotifications(answer).map((notification) =>
      JSON.stringify(notification),
    ),
    JSON.stringify({
      type: "turn.completed",
      turn: { id: "turn-1", items: [] },
    }),
  ];
}

type RawDiscordAuthor = {
  bot?: boolean;
  global_name?: string;
  id: string;
  username: string;
};

type RawDiscordMessage = {
  attachments: Record<string, unknown>[];
  author: RawDiscordAuthor;
  channel_id: string;
  content: string;
  edited_timestamp: null;
  id: string;
  timestamp: string;
  type: number;
};

type DiscordRestCall = {
  body?: Record<string, unknown>;
  method: string;
  path: string;
};

type DiscordReactionCall = {
  channelId: string;
  emoji: string;
  messageId: string;
  method: "PUT" | "DELETE";
};

type FakeDiscordApi = {
  calls: DiscordRestCall[];
  close(): Promise<void>;
  failChannelHistory(channelId: string, status: number, body: string): void;
  messagesIn(channelId: string): RawDiscordMessage[];
  nextId(): string;
  reactionCalls: DiscordReactionCall[];
  renames: { channelId: string; name: string }[];
  reset(): void;
  seedRawMessage(
    channelId: string,
    input: {
      attachments?: Record<string, unknown>[];
      author: RawDiscordAuthor;
      content: string;
    },
  ): RawDiscordMessage;
  seedThreadChannel(threadId: string, parentId: string): void;
  seedUserMessage(channelId: string, content: string): string;
  setFailMessageEdits(value: boolean): void;
  url: string;
};

async function startFakeDiscordApi(): Promise<FakeDiscordApi> {
  const calls: DiscordRestCall[] = [];
  const reactionCalls: DiscordReactionCall[] = [];
  const renames: { channelId: string; name: string }[] = [];
  const channels = new Map<string, Record<string, unknown>>();
  const messagesByChannel = new Map<string, RawDiscordMessage[]>();
  const historyFailures = new Map<string, { body: string; status: number }>();
  let failMessageEdits = false;
  let idCounter = 0;
  const port = await availablePort(4143);

  const nextId = (): string =>
    `50000000000000${String(++idCounter).padStart(4, "0")}`;

  const channelMessages = (channelId: string): RawDiscordMessage[] => {
    let list = messagesByChannel.get(channelId);
    if (!list) {
      list = [];
      messagesByChannel.set(channelId, list);
    }
    return list;
  };

  const seedRawMessage: FakeDiscordApi["seedRawMessage"] = (
    channelId,
    input,
  ) => {
    const message: RawDiscordMessage = {
      attachments: input.attachments ?? [],
      author: input.author,
      channel_id: channelId,
      content: input.content,
      edited_timestamp: null,
      id: nextId(),
      timestamp: new Date().toISOString(),
      type: 0,
    };
    channelMessages(channelId).push(message);
    return message;
  };

  const server = createServer((req, res) => {
    void handleFakeDiscordRequest(req, res, {
      calls,
      channelMessages,
      channels,
      get failMessageEdits() {
        return failMessageEdits;
      },
      historyFailures,
      nextId,
      port,
      reactionCalls,
      renames,
    }).catch((error) => {
      res.writeHead(500, { "content-type": "application/json" });
      res.end(JSON.stringify({ message: String(error), code: 0 }));
    });
  });
  await listen(server, port);

  return {
    calls,
    close: () => closeServer(server),
    failChannelHistory(channelId, status, body) {
      historyFailures.set(channelId, { body, status });
    },
    messagesIn: (channelId) => channelMessages(channelId),
    nextId,
    reactionCalls,
    renames,
    reset() {
      calls.length = 0;
      reactionCalls.length = 0;
      renames.length = 0;
      channels.clear();
      messagesByChannel.clear();
      historyFailures.clear();
      failMessageEdits = false;
    },
    seedRawMessage,
    seedThreadChannel(threadId, parentId) {
      channels.set(threadId, {
        id: threadId,
        name: `thread-${threadId}`,
        parent_id: parentId,
        type: 11,
      });
      channelMessages(threadId);
    },
    seedUserMessage(channelId, content) {
      return seedRawMessage(channelId, {
        author: {
          bot: false,
          global_name: "Test User",
          id: USER_ID,
          username: "tester",
        },
        content,
      }).id;
    },
    setFailMessageEdits(value) {
      failMessageEdits = value;
    },
    url: `http://127.0.0.1:${port}`,
  };
}

async function handleFakeDiscordRequest(
  req: IncomingMessage,
  res: ServerResponse,
  input: {
    calls: DiscordRestCall[];
    channelMessages(channelId: string): RawDiscordMessage[];
    channels: Map<string, Record<string, unknown>>;
    failMessageEdits: boolean;
    historyFailures: Map<string, { body: string; status: number }>;
    nextId(): string;
    port: number;
    reactionCalls: DiscordReactionCall[];
    renames: { channelId: string; name: string }[];
  },
): Promise<void> {
  const url = new URL(req.url ?? "/", `http://127.0.0.1:${input.port}`);
  const request = await nodeRequestToWebRequest(req, url);
  const method = req.method ?? "GET";
  const path = decodeURIComponent(url.pathname);
  const body = await requestJsonBody(request);
  input.calls.push({ body, method, path });

  const reactionMatch =
    /^\/channels\/([^/]+)\/messages\/([^/]+)\/reactions\/(.+)\/@me$/.exec(path);
  if (reactionMatch && (method === "PUT" || method === "DELETE")) {
    input.reactionCalls.push({
      channelId: reactionMatch[1]!,
      emoji: reactionMatch[3]!,
      messageId: reactionMatch[2]!,
      method,
    });
    await sendWebResponse(res, new Response(null, { status: 204 }));
    return;
  }

  const threadsMatch = /^\/channels\/([^/]+)\/messages\/([^/]+)\/threads$/.exec(
    path,
  );
  if (threadsMatch && method === "POST") {
    const channelId = threadsMatch[1]!;
    const messageId = threadsMatch[2]!;
    const name = String(body?.name ?? `Thread ${messageId}`);
    input.channels.set(messageId, {
      id: messageId,
      name,
      parent_id: channelId,
      type: 11,
    });
    input.channelMessages(messageId);
    await sendWebResponse(
      res,
      Response.json({ id: messageId, name, parent_id: channelId, type: 11 }),
    );
    return;
  }

  const singleMessageMatch = /^\/channels\/([^/]+)\/messages\/([^/]+)$/.exec(
    path,
  );
  if (singleMessageMatch && method === "GET") {
    const message = input
      .channelMessages(singleMessageMatch[1]!)
      .find((item) => item.id === singleMessageMatch[2]!);
    if (!message) {
      await sendWebResponse(
        res,
        Response.json(
          { message: "Unknown Message", code: 10008 },
          { status: 404 },
        ),
      );
      return;
    }
    await sendWebResponse(res, Response.json(message));
    return;
  }
  if (singleMessageMatch && method === "PATCH") {
    if (input.failMessageEdits) {
      await sendWebResponse(
        res,
        Response.json(
          { message: "Internal Server Error", code: 0 },
          { status: 500 },
        ),
      );
      return;
    }
    const message = input
      .channelMessages(singleMessageMatch[1]!)
      .find((item) => item.id === singleMessageMatch[2]!);
    if (!message) {
      await sendWebResponse(
        res,
        Response.json(
          { message: "Unknown Message", code: 10008 },
          { status: 404 },
        ),
      );
      return;
    }
    message.content = String(body?.content ?? message.content);
    await sendWebResponse(res, Response.json(message));
    return;
  }

  const messagesMatch = /^\/channels\/([^/]+)\/messages$/.exec(path);
  if (messagesMatch && method === "GET") {
    const channelId = messagesMatch[1]!;
    const failure = input.historyFailures.get(channelId);
    if (failure) {
      await sendWebResponse(
        res,
        new Response(failure.body, {
          headers: { "content-type": "application/json" },
          status: failure.status,
        }),
      );
      return;
    }
    const limit =
      Number.parseInt(url.searchParams.get("limit") ?? "50", 10) || 50;
    // Discord returns newest-first; the adapter re-sorts oldest-first.
    const newestFirst = [...input.channelMessages(channelId)]
      .reverse()
      .slice(0, limit);
    await sendWebResponse(res, Response.json(newestFirst));
    return;
  }
  if (messagesMatch && method === "POST") {
    const channelId = messagesMatch[1]!;
    const message: RawDiscordMessage = {
      attachments: [],
      author: {
        bot: true,
        global_name: "Centaur",
        id: APP_ID,
        username: "centaur",
      },
      channel_id: channelId,
      content: String(body?.content ?? ""),
      edited_timestamp: null,
      id: input.nextId(),
      timestamp: new Date().toISOString(),
      type: 0,
    };
    input.channelMessages(channelId).push(message);
    await sendWebResponse(res, Response.json(message));
    return;
  }

  if (/^\/channels\/[^/]+\/typing$/.test(path) && method === "POST") {
    await sendWebResponse(res, new Response(null, { status: 204 }));
    return;
  }

  const channelMatch = /^\/channels\/([^/]+)$/.exec(path);
  if (channelMatch && method === "GET") {
    const channelId = channelMatch[1]!;
    const channel = input.channels.get(channelId) ?? {
      id: channelId,
      name: `channel-${channelId}`,
      type: 0,
    };
    await sendWebResponse(res, Response.json(channel));
    return;
  }
  if (channelMatch && method === "PATCH") {
    const channelId = channelMatch[1]!;
    const name = String(body?.name ?? "");
    const channel = input.channels.get(channelId) ?? {
      id: channelId,
      type: 11,
    };
    channel.name = name;
    input.channels.set(channelId, channel);
    input.renames.push({ channelId, name });
    await sendWebResponse(res, Response.json(channel));
    return;
  }

  const userMatch = /^\/users\/([^/]+)$/.exec(path);
  if (userMatch && method === "GET") {
    await sendWebResponse(
      res,
      Response.json({
        id: userMatch[1]!,
        username: "tester",
        global_name: "Test User",
      }),
    );
    return;
  }

  if (path.startsWith("/cdn/")) {
    await sendWebResponse(
      res,
      new Response("fake-binary", {
        headers: { "content-type": "application/octet-stream" },
      }),
    );
    return;
  }

  await sendWebResponse(
    res,
    Response.json({ message: "Unknown Route", code: 0 }, { status: 404 }),
  );
}

type MockSessionRequest<T> = {
  body: T;
  threadKey: string;
};

type MockSessionEventRequest = {
  afterEventId: number;
  executionId?: string;
  threadKey: string;
};

type MockSessionEvent = {
  data: string;
  event: string;
  executionId?: string;
  id: number;
  threadKey: string;
};

type MockSessionApi = {
  appends: MockSessionRequest<DiscordbotAppendMessagesRequest>[];
  autoRespond: boolean;
  close(): Promise<void>;
  closeStreams(): void;
  creates: MockSessionRequest<DiscordbotCreateSessionRequest>[];
  emitOutputLine(threadKey: string, line: string, executionId?: string): void;
  emitOutputLines(
    threadKey: string,
    lines: string[],
    executionId?: string,
  ): void;
  emitSessionEvent(
    threadKey: string,
    event: string,
    data: unknown,
    executionId?: string,
  ): void;
  eventRequests: MockSessionEventRequest[];
  executes: MockSessionRequest<DiscordbotExecuteSessionRequest>[];
  failAllEvents: boolean;
  failNextCreate: boolean;
  failNextEvents: boolean;
  failNextExecute: boolean;
  failNextExecuteAfterAccept: boolean;
  holdNextExecute(): () => void;
  reset(): void;
  streamCount: number;
  url: string;
};

async function startMockCodexApi(): Promise<MockSessionApi> {
  const appends: MockSessionRequest<DiscordbotAppendMessagesRequest>[] = [];
  const creates: MockSessionRequest<DiscordbotCreateSessionRequest>[] = [];
  const eventRequests: MockSessionEventRequest[] = [];
  const events: MockSessionEvent[] = [];
  const executes: MockSessionRequest<DiscordbotExecuteSessionRequest>[] = [];
  const idempotentExecutions = new Map<string, string>();
  const streams = new Set<ServerResponse>();
  let autoRespond = true;
  let executeHold: Promise<void> | null = null;
  let executeHoldRelease: (() => void) | null = null;
  let eventId = 0;
  let failAllEvents = false;
  let failNextCreate = false;
  let failNextEvents = false;
  let failNextExecute = false;
  let failNextExecuteAfterAccept = false;
  const port = await availablePort(4163);
  const closeStreams = () => {
    for (const stream of streams) stream.end();
    streams.clear();
  };
  const server = createServer((req, res) => {
    void handleMockCodexRequest(req, res, {
      appends,
      creates,
      events,
      eventRequests,
      executes,
      get autoRespond() {
        return autoRespond;
      },
      get executeHold() {
        return executeHold;
      },
      get failAllEvents() {
        return failAllEvents;
      },
      get failNextCreate() {
        return failNextCreate;
      },
      get failNextEvents() {
        return failNextEvents;
      },
      get failNextExecute() {
        return failNextExecute;
      },
      get failNextExecuteAfterAccept() {
        return failNextExecuteAfterAccept;
      },
      idempotentExecutions,
      nextEventId() {
        eventId += 1;
        return eventId;
      },
      port,
      setFailNextCreate(value) {
        failNextCreate = value;
      },
      setFailNextEvents(value) {
        failNextEvents = value;
      },
      setFailNextExecute(value) {
        failNextExecute = value;
      },
      setFailNextExecuteAfterAccept(value) {
        failNextExecuteAfterAccept = value;
      },
      streams,
    }).catch((error) => {
      res.writeHead(500, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: String(error) }));
    });
  });
  await listen(server, port);

  const api: MockSessionApi = {
    appends,
    creates,
    eventRequests,
    executes,
    reset() {
      appends.length = 0;
      creates.length = 0;
      eventRequests.length = 0;
      events.length = 0;
      executes.length = 0;
      idempotentExecutions.clear();
      executeHoldRelease?.();
      executeHold = null;
      executeHoldRelease = null;
      closeStreams();
      autoRespond = true;
      eventId = 0;
      failAllEvents = false;
      failNextCreate = false;
      failNextEvents = false;
      failNextExecute = false;
      failNextExecuteAfterAccept = false;
    },
    url: `http://127.0.0.1:${port}`,
    closeStreams,
    get autoRespond() {
      return autoRespond;
    },
    set autoRespond(value: boolean) {
      autoRespond = value;
    },
    get failAllEvents() {
      return failAllEvents;
    },
    set failAllEvents(value: boolean) {
      failAllEvents = value;
    },
    get failNextCreate() {
      return failNextCreate;
    },
    set failNextCreate(value: boolean) {
      failNextCreate = value;
    },
    get failNextEvents() {
      return failNextEvents;
    },
    set failNextEvents(value: boolean) {
      failNextEvents = value;
    },
    get failNextExecute() {
      return failNextExecute;
    },
    set failNextExecute(value: boolean) {
      failNextExecute = value;
    },
    get failNextExecuteAfterAccept() {
      return failNextExecuteAfterAccept;
    },
    set failNextExecuteAfterAccept(value: boolean) {
      failNextExecuteAfterAccept = value;
    },
    holdNextExecute() {
      if (executeHoldRelease) throw new Error("execute is already held");
      executeHold = new Promise((resolve) => {
        executeHoldRelease = resolve;
      });
      return () => {
        const release = executeHoldRelease;
        executeHoldRelease = null;
        executeHold = null;
        release?.();
      };
    },
    get streamCount() {
      return streams.size;
    },
    emitOutputLine(threadKey: string, line: string, executionId?: string) {
      emitMockSessionEvent({
        data: line,
        event: "session.output.line",
        executionId,
        events,
        id: ++eventId,
        streams,
        threadKey,
      });
    },
    emitOutputLines(threadKey: string, lines: string[], executionId?: string) {
      for (const line of lines)
        api.emitOutputLine(threadKey, line, executionId);
    },
    emitSessionEvent(
      threadKey: string,
      event: string,
      data: unknown,
      executionId?: string,
    ) {
      emitMockSessionEvent({
        data: typeof data === "string" ? data : JSON.stringify(data),
        event,
        executionId,
        events,
        id: ++eventId,
        streams,
        threadKey,
      });
    },
    async close() {
      closeStreams();
      await closeServer(server);
    },
  };
  return api;
}

async function handleMockCodexRequest(
  req: IncomingMessage,
  res: ServerResponse,
  input: {
    appends: MockSessionRequest<DiscordbotAppendMessagesRequest>[];
    autoRespond: boolean;
    creates: MockSessionRequest<DiscordbotCreateSessionRequest>[];
    events: MockSessionEvent[];
    eventRequests: MockSessionEventRequest[];
    executeHold: Promise<void> | null;
    executes: MockSessionRequest<DiscordbotExecuteSessionRequest>[];
    failAllEvents: boolean;
    failNextCreate: boolean;
    failNextEvents: boolean;
    failNextExecute: boolean;
    failNextExecuteAfterAccept: boolean;
    idempotentExecutions: Map<string, string>;
    nextEventId(): number;
    port: number;
    setFailNextCreate(value: boolean): void;
    setFailNextEvents(value: boolean): void;
    setFailNextExecute(value: boolean): void;
    setFailNextExecuteAfterAccept(value: boolean): void;
    streams: Set<ServerResponse>;
  },
): Promise<void> {
  const url = new URL(req.url ?? "/", `http://127.0.0.1:${input.port}`);
  const match =
    /^\/api\/session\/([^/]+)(?:\/(messages|execute|events))?$/.exec(
      url.pathname,
    );
  if (!match?.[1]) {
    await sendWebResponse(res, new Response("not found", { status: 404 }));
    return;
  }
  const threadKey = decodeURIComponent(match[1]);
  const endpoint = match[2] ?? "session";

  if (endpoint === "session") {
    const request = await nodeRequestToWebRequest(req, url);
    const body = (await request.json()) as DiscordbotCreateSessionRequest;
    input.creates.push({ threadKey, body });
    if (input.failNextCreate) {
      input.setFailNextCreate(false);
      await sendWebResponse(
        res,
        new Response("unavailable", {
          status: 503,
          statusText: "Service Unavailable",
        }),
      );
      return;
    }
    await sendWebResponse(
      res,
      Response.json({
        thread_key: threadKey,
        sandbox_id: null,
        harness_type: body.harness_type,
        harness_thread_id: null,
        status: "active",
      }),
    );
    return;
  }

  if (endpoint === "events") {
    const afterEventId =
      Number.parseInt(url.searchParams.get("after_event_id") ?? "0", 10) || 0;
    const executionId = url.searchParams.get("execution_id") || undefined;
    input.eventRequests.push({ threadKey, afterEventId, executionId });
    if (input.failAllEvents || input.failNextEvents) {
      input.setFailNextEvents(false);
      await sendWebResponse(
        res,
        new Response("unavailable", {
          status: 503,
          statusText: "Service Unavailable",
        }),
      );
      return;
    }
    res.writeHead(200, {
      "cache-control": "no-cache",
      connection: "keep-alive",
      "content-type": "text/event-stream",
    });
    input.streams.add(res);
    for (const event of input.events) {
      if (
        event.threadKey === threadKey &&
        event.id > afterEventId &&
        (!executionId ||
          !event.executionId ||
          event.executionId === executionId)
      ) {
        writeMockSseEvent(res, event);
      }
    }
    req.once("close", () => {
      input.streams.delete(res);
    });
    return;
  }

  const request = await nodeRequestToWebRequest(req, url);
  if (endpoint === "messages") {
    const body = (await request.json()) as DiscordbotAppendMessagesRequest;
    input.appends.push({ threadKey, body });
    await sendWebResponse(
      res,
      Response.json({
        ok: true,
        message_ids: body.messages.map((_, index) => `msg-${index + 1}`),
      }),
    );
    return;
  }

  const body = (await request.json()) as DiscordbotExecuteSessionRequest;
  input.executes.push({ threadKey, body });
  if (input.failNextExecute) {
    input.setFailNextExecute(false);
    await sendWebResponse(
      res,
      new Response("unavailable", {
        status: 503,
        statusText: "Service Unavailable",
      }),
    );
    return;
  }
  if (input.executeHold) await input.executeHold;
  const idempotencyMapKey = body.idempotency_key
    ? `${threadKey}:${body.idempotency_key}`
    : undefined;
  const existingExecutionId = idempotencyMapKey
    ? input.idempotentExecutions.get(idempotencyMapKey)
    : undefined;
  const executionId =
    existingExecutionId ??
    `exe-${input.idempotentExecutions.size + input.executes.length}`;
  if (idempotencyMapKey && !existingExecutionId) {
    input.idempotentExecutions.set(idempotencyMapKey, executionId);
  }
  if (!existingExecutionId && input.autoRespond) {
    for (const line of sampleCodexOutputLines(
      `Executed request ${input.idempotentExecutions.size}.`,
    )) {
      emitMockSessionEvent({
        data: line,
        event: "session.output.line",
        executionId,
        events: input.events,
        id: input.nextEventId(),
        streams: input.streams,
        threadKey,
      });
    }
  }
  if (input.failNextExecuteAfterAccept) {
    input.setFailNextExecuteAfterAccept(false);
    await sendWebResponse(
      res,
      new Response("response lost after accept", {
        status: 503,
        statusText: "Service Unavailable",
      }),
    );
    return;
  }
  await sendWebResponse(
    res,
    Response.json({
      ok: true,
      execution_id: executionId,
      thread_key: threadKey,
      status: "completed",
    }),
  );
}

function emitMockSessionEvent(input: {
  data: string;
  event: string;
  executionId?: string;
  events: MockSessionEvent[];
  id: number;
  streams: Set<ServerResponse>;
  threadKey: string;
}): void {
  const event: MockSessionEvent = {
    data: input.data,
    event: input.event,
    executionId: input.executionId,
    id: input.id,
    threadKey: input.threadKey,
  };
  input.events.push(event);
  for (const stream of input.streams) writeMockSseEvent(stream, event);
}

function writeMockSseEvent(
  stream: ServerResponse,
  event: MockSessionEvent,
): void {
  stream.write(`id: ${event.id}\n`);
  stream.write(`event: ${event.event}\n`);
  for (const line of event.data.split("\n")) {
    stream.write(`data: ${line}\n`);
  }
  stream.write("\n");
}

async function nodeRequestToWebRequest(
  req: IncomingMessage,
  url: URL,
): Promise<Request> {
  const headers = new Headers();
  for (const [key, value] of Object.entries(req.headers)) {
    if (Array.isArray(value)) {
      for (const item of value) headers.append(key, item);
    } else if (typeof value === "string") {
      headers.set(key, value);
    }
  }

  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  const body = Buffer.concat(chunks);
  return new Request(url, {
    body:
      body.length > 0 && req.method !== "GET" && req.method !== "HEAD"
        ? body
        : undefined,
    headers,
    method: req.method,
  });
}

async function requestJsonBody(
  request: Request,
): Promise<Record<string, unknown> | undefined> {
  const raw = await request.text();
  if (!raw) return undefined;
  try {
    const parsed: unknown = JSON.parse(raw);
    return isRecord(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

async function sendWebResponse(
  res: ServerResponse,
  response: Response,
): Promise<void> {
  res.statusCode = response.status;
  res.statusMessage = response.statusText;
  response.headers.forEach((value, key) => {
    res.setHeader(key, value);
  });
  if (response.body === null || response.status === 204) {
    res.end();
    return;
  }
  res.end(Buffer.from(await response.arrayBuffer()));
}

function listen(server: HttpServer, port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });
}

function closeServer(server: HttpServer): Promise<void> {
  return new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

async function waitFor(
  predicate: () => boolean,
  timeoutMs = 5000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error("Timed out waiting for condition");
}

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function availablePort(preferred: number): Promise<number> {
  for (let port = preferred; port < preferred + 100; port++) {
    if (!(await isPortOpen(port))) return port;
  }
  throw new Error(`No available port near ${preferred}`);
}

async function isPortOpen(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = connect(port, "127.0.0.1");
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
    socket.setTimeout(250, () => {
      socket.destroy();
      resolve(false);
    });
  });
}
