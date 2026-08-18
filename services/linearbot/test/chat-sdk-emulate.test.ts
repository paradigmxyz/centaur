// Linear port of slackbotv2/discordbot's chat-sdk emulate harness. The
// `emulate` package has no Linear service, so this spins up a fake Linear
// GraphQL API (Bun.serve) that the REAL patched @chat-adapter/linear adapter
// talks to, plus a mock api-rs session API; ingress drives signed
// AgentSessionEvent webhooks through the bot's Hono route, so the full chat
// SDK pipeline (signature verification, dedupe, locks, handler routing) runs
// exactly as in production.
//
// Deliberate Linear deltas this harness encodes (NOT bugs):
// - The ack and reasoning surface is Linear agent activities (ephemeral
//   thought ack, persistent thought/action activities), not reactions.
// - The final answer posts exactly once: a `response` activity on success,
//   an `error` activity on failure. Nothing is ever edited.
// - One agent session = ONE thread key (linear:{issue}:s:{session}) across
//   created + prompted events — that is the adapter patch under test.
// - The initial context prepends the synthetic issue-context message built
//   from the webhook's promptContext.
import { createHmac } from "node:crypto";
import {
  afterAll,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "bun:test";
import { createMemoryState } from "@chat-adapter/state-memory";
import {
  createLinearbot,
  type Linearbot,
  type LinearbotAppendMessagesRequest,
  type LinearbotCreateSessionRequest,
  type LinearbotExecuteSessionRequest,
  type LinearbotOptions,
  type LinearbotSessionMessage,
} from "../src/index";
import { noopLogger } from "../src/utils";

const WEBHOOK_SECRET = "linearbot-emulate-secret";
const ORG_ID = "org-1";
const BOT_USER_ID = "bot-user-1";
const BOT_PROFILE_HANDLE = "centaur-bot";
const USER_ID = "user-1";
const ISSUE_ID = "issue-1";

let linearApi: FakeLinearApi;
let codexApi: MockSessionApi;
let bot: Linearbot;

beforeAll(async () => {
  linearApi = startFakeLinearApi();
  codexApi = startMockCodexApi();
});

beforeEach(() => {
  linearApi.reset();
  codexApi.reset();
  bot = createTestBot();
});

afterAll(() => {
  codexApi?.close();
  linearApi?.close();
});

function createTestBot(overrides: Partial<LinearbotOptions> = {}): Linearbot {
  const state = overrides.state ?? createMemoryState();
  return createLinearbot({
    apiUrl: codexApi.url,
    connectStateOnStart: false,
    linearAccessToken: "linear-emulate-token",
    linearApiUrl: linearApi.url,
    linearWebhookSecret: WEBHOOK_SECRET,
    logger: noopLogger,
    state,
    userName: "centaur",
    ...overrides,
  });
}

describe("linearbot comment-thread pipeline", () => {
  // Assertions filter by this thread's own key / parent comment id: a prior
  // test's detached run can post into the shared mock servers after reset.
  it("answers a comment @-mention with one comment (answer + collapsed CoT) on the comment-thread sandbox", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-q`;
    const res = await postWebhook(
      commentCreatedPayload({
        id: "comment-q",
        body: "@centaur how long will this take?",
      }),
    );
    expect(res.status).toBe(200);

    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    // The issue context rides inline in the first turn's execute prompt (so the
    // agent always knows what the task is), including the issue description.
    const execTexts = executeInputTexts(threadKey);
    expect(execTexts.some((t) => t.includes("[Linear issue context]"))).toBe(
      true,
    );
    expect(execTexts.some((t) => t.includes("Something broke"))).toBe(true);
    expect(execTexts.some((t) => t.includes("The deploy fails on boot."))).toBe(
      true,
    );
    // Not delegated to the bot → no ownership contract injected.
    expect(execTexts.some((t) => t.includes("You own this Linear issue"))).toBe(
      false,
    );

    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("About a day."));

    // The live comment is posted with the first thought, then finalized in
    // place — wait for the settled answer body, not just the comment.
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-q" && c.body.includes("About a day."),
      ),
    );
    const reply = linearApi.botComments.find(
      (c) => c.parentId === "comment-q",
    )!;
    expect(reply.issueId).toBe(ISSUE_ID);
    expect(reply.body).toContain("About a day.");
    expect(reply.body).toContain(">>> Chain of thought");
    // The command renders as an inline code span, not a fenced block that
    // would swallow the rest of the chain-of-thought list.
    expect(reply.body).toContain("Command execution: `pnpm test`");
    expect(reply.body).not.toContain("```");
    // Edited in place, not re-posted: exactly one comment in this thread.
    expect(
      linearApi.botComments.filter((c) => c.parentId === "comment-q"),
    ).toHaveLength(1);
    // The vestigial session is never the surface: no session-keyed execution.
    expect(codexApi.executes.some((e) => e.threadKey.includes(":s:"))).toBe(
      false,
    );
  });

  it("seeds full context on the first turn, then only the compact header on later turns", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-twoturn`;
    await postWebhook(
      commentCreatedPayload({ id: "comment-twoturn", body: "@centaur first" }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("One."));
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-twoturn" && c.body.includes("One."),
      ),
    );
    // First turn carries the full context, description included.
    expect(
      executeInputTexts(threadKey).some((t) =>
        t.includes("The deploy fails on boot."),
      ),
    ).toBe(true);
    const execsAfterFirst = codexApi.executes.filter(
      (e) => e.threadKey === threadKey,
    ).length;

    // A second mention in the same thread (reply under the root comment).
    await postWebhook(
      commentCreatedPayload({
        id: "comment-twoturn-2",
        parentId: "comment-twoturn",
        body: "@centaur second",
      }),
    );
    await waitFor(
      () =>
        codexApi.executes.filter((e) => e.threadKey === threadKey).length >
        execsAfterFirst,
    );
    // The second turn carries the compact header (task id + title) but not the
    // full description again.
    const secondExec = codexApi.executes
      .filter((e) => e.threadKey === threadKey)
      .at(-1)!;
    const secondTexts = secondExec.body.input_lines.flatMap(inputLineTexts);
    expect(secondTexts.some((t) => t.includes("[Linear issue context]"))).toBe(
      true,
    );
    expect(secondTexts.some((t) => t.includes("Something broke"))).toBe(true);
    expect(
      secondTexts.some((t) => t.includes("The deploy fails on boot.")),
    ).toBe(false);
  });

  it("posts a live 'Thinking…' comment on the first thought, then swaps it to the answer in place", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-live`;
    await postWebhook(
      commentCreatedPayload({ id: "comment-live", body: "@centaur go" }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );

    // Stream the thinking phase only — enough to settle one chain-of-thought
    // line, with no answer or terminal event yet.
    codexApi.emitOutputLines(threadKey, thinkingOnlyOutputLines());
    await waitFor(() =>
      linearApi.botComments.some((c) => c.parentId === "comment-live"),
    );
    const live = linearApi.botComments.find(
      (c) => c.parentId === "comment-live",
    )!;
    expect(live.body).toContain(">>> Thinking…");
    expect(live.body).toContain("pnpm test");
    expect(live.body).not.toContain(">>> Chain of thought");

    // Stream the answer + terminal — the SAME comment switches to its final
    // form (answer above a "Chain of thought" section).
    codexApi.emitOutputLines(threadKey, answerOutputLines("All set."));
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-live" && c.body.includes("All set."),
      ),
    );
    const finalReply = linearApi.botComments.find(
      (c) => c.parentId === "comment-live",
    )!;
    expect(finalReply.id).toBe(live.id);
    expect(finalReply.body).toContain("All set.");
    expect(finalReply.body).toContain(">>> Chain of thought");
    expect(finalReply.body).not.toContain(">>> Thinking…");
    // Edited in place, never re-posted.
    expect(
      linearApi.botComments.filter((c) => c.parentId === "comment-live"),
    ).toHaveLength(1);
  });

  it("detects a mention rendered as Linear's profile URL (not @name text)", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-url`;
    await postWebhook(
      commentCreatedPayload({
        id: "comment-url",
        body: `https://linear.app/acme/profiles/${BOT_PROFILE_HANDLE} how long will this take?`,
      }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("A day."));
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-url" && c.body.includes("A day."),
      ),
    );
    expect(
      linearApi.botComments.find((c) => c.parentId === "comment-url")!.body,
    ).toContain("A day.");
  });

  it("reacts 👀 on notice and swaps to ✅ when finished", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-r`;
    await postWebhook(
      commentCreatedPayload({ id: "comment-r", body: "@centaur status?" }),
    );
    // 👀 lands as soon as the run starts, before the answer.
    await waitFor(() =>
      linearApi.reactions.some(
        (r) => r.commentId === "comment-r" && r.emoji === "👀",
      ),
    );
    const working = linearApi.reactions.find(
      (r) => r.commentId === "comment-r" && r.emoji === "👀",
    )!;

    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("Green."));

    // On settle: ✅ added and 👀 removed; no ❌.
    await waitFor(() =>
      linearApi.reactions.some(
        (r) => r.commentId === "comment-r" && r.emoji === "✅",
      ),
    );
    await waitFor(() => linearApi.removedReactionIds.includes(working.id));
    expect(
      linearApi.reactions.some(
        (r) => r.commentId === "comment-r" && r.emoji === "❌",
      ),
    ).toBe(false);
  });

  it("dedupes a redelivered mention comment", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-d`;
    await postWebhook(
      commentCreatedPayload({ id: "comment-d", body: "@centaur ping" }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("Pong."));
    await waitFor(() =>
      linearApi.botComments.some((c) => c.parentId === "comment-d"),
    );

    await Bun.sleep(50);
    await postWebhook(
      commentCreatedPayload({ id: "comment-d", body: "@centaur ping" }),
    );
    await Bun.sleep(100);
    expect(
      linearApi.botComments.filter((c) => c.parentId === "comment-d"),
    ).toHaveLength(1);
  });

  it("ignores a comment that does not mention the bot", async () => {
    await postWebhook(
      commentCreatedPayload({ id: "comment-plain", body: "just a team note" }),
    );
    await Bun.sleep(100);
    expect(
      codexApi.executes.some(
        (e) => e.threadKey === `linear:${ISSUE_ID}:c:comment-plain`,
      ),
    ).toBe(false);
    expect(
      linearApi.botComments.some((c) => c.parentId === "comment-plain"),
    ).toBe(false);
  });

  it("ignores bot-authored comments (loop guard)", async () => {
    await postWebhook(
      commentCreatedPayload({
        id: "comment-self",
        body: "@centaur loop",
        user: null,
      }),
    );
    await Bun.sleep(100);
    expect(
      codexApi.executes.some(
        (e) => e.threadKey === `linear:${ISSUE_ID}:c:comment-self`,
      ),
    ).toBe(false);
  });

  it("appends a non-mention follow-up to an active thread's session as context (no run, no reply)", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-active`;
    // A mention runs a turn — the thread becomes active (historyForwarded).
    await postWebhook(
      commentCreatedPayload({ id: "comment-active", body: "@centaur hi" }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("Hello."));
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-active" && c.body.includes("Hello."),
      ),
    );
    const executesBefore = codexApi.executes.filter(
      (e) => e.threadKey === threadKey,
    ).length;
    const repliesBefore = linearApi.botComments.filter(
      (c) => c.parentId === "comment-active",
    ).length;

    // A reply in the thread that does NOT mention the bot.
    await postWebhook(
      commentCreatedPayload({
        id: "comment-follow",
        parentId: "comment-active",
        body: "actually, hold off on the deploy",
      }),
    );
    // It is appended to the session as context...
    await waitFor(() => appendCount(threadKey, "hold off on the deploy") === 1);
    await Bun.sleep(50);
    // ...without running a new turn or posting another reply.
    expect(
      codexApi.executes.filter((e) => e.threadKey === threadKey).length,
    ).toBe(executesBefore);
    expect(
      linearApi.botComments.filter((c) => c.parentId === "comment-active")
        .length,
    ).toBe(repliesBefore);
  });

  it("ingests a non-mention follow-up that arrives before the first turn finishes", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-midturn`;
    // The mention claims the thread and starts a turn; we hold off emitting any
    // output, so the turn is still streaming (historyForwarded not yet set).
    await postWebhook(
      commentCreatedPayload({ id: "comment-midturn", body: "@centaur begin" }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );

    // A non-mention reply lands mid-run — still ingested, because the thread is
    // active via the claimed mention (not yet via historyForwarded).
    await postWebhook(
      commentCreatedPayload({
        id: "comment-midturn-follow",
        parentId: "comment-midturn",
        body: "extra detail mid-run",
      }),
    );
    await waitFor(() => appendCount(threadKey, "extra detail mid-run") === 1);

    // Letting the turn finish still posts its answer as normal.
    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("Done."));
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-midturn" && c.body.includes("Done."),
      ),
    );
  });

  it("does not ingest a non-mention comment in a thread the bot is not active in", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-cold`;
    await postWebhook(
      commentCreatedPayload({
        id: "comment-cold-follow",
        parentId: "comment-cold",
        body: "just two humans chatting, no bot involved",
      }),
    );
    await Bun.sleep(100);
    expect(codexApi.appends.some((a) => a.threadKey === threadKey)).toBe(false);
    expect(codexApi.creates.some((c) => c.threadKey === threadKey)).toBe(false);
    expect(codexApi.executes.some((e) => e.threadKey === threadKey)).toBe(
      false,
    );
  });

  it("dedupes a redelivered non-mention follow-up (appends once)", async () => {
    const threadKey = `linear:${ISSUE_ID}:c:comment-dedup`;
    await postWebhook(
      commentCreatedPayload({ id: "comment-dedup", body: "@centaur start" }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("Started."));
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-dedup" && c.body.includes("Started."),
      ),
    );

    const followup = () =>
      commentCreatedPayload({
        id: "comment-redeliver",
        parentId: "comment-dedup",
        body: "one more note for context",
      });
    await postWebhook(followup());
    await waitFor(() => appendCount(threadKey, "one more note") === 1);
    await Bun.sleep(50);
    await postWebhook(followup());
    await Bun.sleep(100);
    expect(appendCount(threadKey, "one more note")).toBe(1);
  });

  it("runs an agent turn when the issue is assigned to the bot, posting a comment + applying status", async () => {
    const threadKey = `linear:${ISSUE_ID}`;
    const res = await postWebhook(
      issueAssignmentPayload({ updatedAt: "2026-06-16T00:00:00.000Z" }),
    );
    expect(res.status).toBe(200);

    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    // The synthetic "work this issue" instruction is the execute prompt.
    const exec = codexApi.executes.find((e) => e.threadKey === threadKey)!;
    const inputLine = JSON.parse(exec.body.input_lines[0]!) as {
      message: { content: Array<{ text?: string }> };
    };
    expect(
      inputLine.message.content.some((c) =>
        (c.text ?? "").includes("work the task"),
      ),
    ).toBe(true);

    codexApi.emitOutputLines(
      threadKey,
      sampleCodexOutputLines("Shipped.\n\nLinear-Status: done"),
    );
    await waitFor(() =>
      linearApi.botComments.some(
        (c) =>
          c.issueId === ISSUE_ID && !c.parentId && c.body.includes("Shipped."),
      ),
    );
    const reply = linearApi.botComments.find(
      (c) => c.issueId === ISSUE_ID && !c.parentId,
    )!;
    expect(reply.body).toContain("Shipped.");
    expect(reply.body).not.toContain("Linear-Status:");
    expect(reply.body).toContain(">>> Chain of thought");
    // Terminal marker moves the assigned issue to Done.
    await waitFor(() =>
      linearApi.issueStateUpdates.some((u) => u.stateId === "st-done"),
    );
    // Kickoff moved it to In Progress when work started (from Todo/unstarted).
    expect(
      linearApi.issueStateUpdates.some((u) => u.stateId === "st-progress"),
    ).toBe(true);
  });

  it("posts a 'starting work' comment up front on assignment, then swaps it to the answer", async () => {
    const threadKey = `linear:${ISSUE_ID}`;
    await postWebhook(
      issueAssignmentPayload({ updatedAt: "2026-06-16T04:00:00.000Z" }),
    );
    // The comment lands before any agent output is emitted.
    await waitFor(() =>
      linearApi.botComments.some((c) => c.issueId === ISSUE_ID && !c.parentId),
    );
    const start = linearApi.botComments.find(
      (c) => c.issueId === ISSUE_ID && !c.parentId,
    )!;
    expect(start.body).toContain("On it");
    expect(start.body).toContain(">>> Thinking…");

    codexApi.emitOutputLines(threadKey, sampleCodexOutputLines("All done."));
    await waitFor(() =>
      linearApi.botComments.some(
        (c) =>
          c.issueId === ISSUE_ID && !c.parentId && c.body.includes("All done."),
      ),
    );
    // Edited in place: still a single top-level comment.
    expect(
      linearApi.botComments.filter(
        (c) => c.issueId === ISSUE_ID && !c.parentId,
      ),
    ).toHaveLength(1);
  });

  it("does not run a turn on a non-assignee edit to an issue the bot owns", async () => {
    await postWebhook(
      issueAssignmentPayload({
        updatedAt: "2026-06-16T05:00:00.000Z",
        updatedFrom: { description: "old description" },
      }),
    );
    await Bun.sleep(100);
    expect(
      codexApi.executes.some((e) => e.threadKey === `linear:${ISSUE_ID}`),
    ).toBe(false);
    expect(
      linearApi.botComments.some((c) => c.issueId === ISSUE_ID && !c.parentId),
    ).toBe(false);
  });

  it("runs a turn when an issue is created already assigned to the bot", async () => {
    const threadKey = `linear:${ISSUE_ID}`;
    await postWebhook(
      issueAssignmentPayload({
        action: "create",
        updatedAt: "2026-06-16T06:00:00.000Z",
      }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    codexApi.emitOutputLines(
      threadKey,
      sampleCodexOutputLines("Created work."),
    );
    await waitFor(() =>
      linearApi.botComments.some(
        (c) =>
          c.issueId === ISSUE_ID &&
          !c.parentId &&
          c.body.includes("Created work."),
      ),
    );
  });

  it("treats a comment on a delegated issue as owned work (ownership context, not status)", async () => {
    linearApi.setIssueDelegate(BOT_USER_ID);
    const threadKey = `linear:${ISSUE_ID}:c:comment-deleg`;
    await postWebhook(
      commentCreatedPayload({
        id: "comment-deleg",
        body: "@centaur what's next",
      }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    // The agent is told it owns the delegated issue and should carry the work.
    expect(
      executeInputTexts(threadKey).some((t) =>
        t.includes("You own this Linear issue"),
      ),
    ).toBe(true);

    const statusUpdatesBefore = linearApi.issueStateUpdates.length;
    codexApi.emitOutputLines(
      threadKey,
      sampleCodexOutputLines("Handled.\n\nLinear-Status: done"),
    );
    await waitFor(() =>
      linearApi.botComments.some(
        (c) => c.parentId === "comment-deleg" && c.body.includes("Handled."),
      ),
    );
    // A comment turn answers and carries the ownership contract, but never
    // writes issue status: kickoff and the terminal marker belong to the
    // assignment thread (linear:{issueId}), so a delegate-plus-mention can't
    // double-drive status and a commenter can't force a transition. The marker
    // is still stripped from the visible reply.
    const reply = linearApi.botComments.find(
      (c) => c.parentId === "comment-deleg",
    )!;
    expect(reply.body).not.toContain("Linear-Status:");
    await Bun.sleep(100);
    expect(linearApi.issueStateUpdates.length).toBe(statusUpdatesBefore);
  });

  it("dedupes a redelivered assignment webhook", async () => {
    const threadKey = `linear:${ISSUE_ID}`;
    await postWebhook(
      issueAssignmentPayload({ updatedAt: "2026-06-16T01:00:00.000Z" }),
    );
    await waitFor(() =>
      codexApi.executes.some((e) => e.threadKey === threadKey),
    );
    const before = codexApi.executes.filter(
      (e) => e.threadKey === threadKey,
    ).length;
    await Bun.sleep(50);
    await postWebhook(
      issueAssignmentPayload({ updatedAt: "2026-06-16T01:00:00.000Z" }),
    );
    await Bun.sleep(100);
    expect(
      codexApi.executes.filter((e) => e.threadKey === threadKey).length,
    ).toBe(before);
  });

  it("settles a vestigial agent session minimally (no widget render)", async () => {
    const sessionId = "sess-settle";
    linearApi.addAgentSession({ id: sessionId, rootCommentId: "comment-x" });
    const res = await postWebhook(
      agentSessionCreatedPayload({
        sessionId,
        commentId: "comment-x",
        commentBody: "@centaur hi",
        promptContext: "ENG-1: x",
      }),
    );
    expect(res.status).toBe(200);

    // A single terminal response activity settles the session — no run.
    await waitFor(() =>
      linearApi.activities.some((a) => a.content.type === "response"),
    );
    const responses = linearApi.activities.filter(
      (a) => a.content.type === "response",
    );
    expect(responses).toHaveLength(1);
    expect(
      responses[0]?.content.type === "response"
        ? responses[0].content.body
        : "",
    ).toContain("comment thread");
    expect(
      codexApi.executes.some(
        (e) => e.threadKey === `linear:${ISSUE_ID}:s:${sessionId}`,
      ),
    ).toBe(false);
  });

  it("rejects webhooks with an invalid signature", async () => {
    const payload = commentCreatedPayload({
      id: "comment-forged",
      body: "@centaur forged",
    });
    const body = JSON.stringify(payload);
    const response = await bot.app.request("/api/webhooks/linear", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "linear-signature": createHmac("sha256", "wrong-secret")
          .update(body)
          .digest("hex"),
      },
      body,
    });
    expect(response.status).toBeGreaterThanOrEqual(400);
    await Bun.sleep(50);
    expect(codexApi.executes).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// Webhook driving
// ---------------------------------------------------------------------------

function agentSessionCreatedPayload(input: {
  appUserId?: string;
  commentBody?: string;
  commentId?: string;
  /** Creator user id; null omits the creator (automation-created session). */
  creatorId?: string | null;
  promptContext: string;
  sessionId: string;
}) {
  return {
    action: "created",
    type: "AgentSessionEvent",
    createdAt: new Date().toISOString(),
    organizationId: ORG_ID,
    webhookTimestamp: Date.now(),
    webhookId: "wh-1",
    promptContext: input.promptContext,
    agentSession: {
      id: input.sessionId,
      appUserId: input.appUserId ?? BOT_USER_ID,
      issueId: ISSUE_ID,
      url: `https://linear.app/acme/agent-session/${input.sessionId}`,
      ...(input.commentId
        ? { comment: { id: input.commentId, body: input.commentBody ?? "" } }
        : {}),
      ...(input.creatorId === null
        ? {}
        : {
            creator: {
              id: input.creatorId ?? USER_ID,
              name: "Ada Lovelace",
              email: "ada@example.com",
              url: "https://linear.app/acme/profiles/ada",
              avatarUrl: null,
            },
          }),
    },
  };
}

function commentCreatedPayload(input: {
  body: string;
  id: string;
  parentId?: string;
  user?: null;
}) {
  return {
    action: "create",
    type: "Comment",
    createdAt: new Date().toISOString(),
    organizationId: ORG_ID,
    webhookTimestamp: Date.now(),
    webhookId: "wh-3",
    url: `https://linear.app/acme/comment/${input.id}`,
    data: {
      id: input.id,
      body: input.body,
      issueId: ISSUE_ID,
      parentId: input.parentId,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      ...(input.user === null
        ? {}
        : {
            user: {
              id: USER_ID,
              name: "Ada Lovelace",
              email: "ada@example.com",
              url: "https://linear.app/acme/profiles/ada",
              avatarUrl: null,
            },
          }),
    },
  };
}

function agentSessionPromptedPayload(input: {
  body: string;
  rootCommentId: string;
  sessionId: string;
  sourceCommentId: string;
}) {
  return {
    action: "prompted",
    type: "AgentSessionEvent",
    createdAt: new Date().toISOString(),
    organizationId: ORG_ID,
    webhookTimestamp: Date.now(),
    webhookId: "wh-2",
    promptContext: "fresh prompt context (already forwarded)",
    agentSession: {
      id: input.sessionId,
      appUserId: BOT_USER_ID,
      issueId: ISSUE_ID,
      url: `https://linear.app/acme/agent-session/${input.sessionId}`,
      comment: { id: input.rootCommentId, body: "root" },
    },
    agentActivity: {
      id: `aa-${input.sourceCommentId}`,
      sourceCommentId: input.sourceCommentId,
      createdAt: new Date().toISOString(),
      content: { type: "prompt", body: input.body },
      user: {
        id: USER_ID,
        name: "Ada Lovelace",
        email: "ada@example.com",
        url: "https://linear.app/acme/profiles/ada",
        avatarUrl: null,
      },
    },
  };
}

function issueAssignmentPayload(input: {
  updatedAt: string;
  action?: "create" | "update";
  assigneeId?: string | null;
  delegateId?: string | null;
  updatedFrom?: Record<string, unknown>;
}) {
  return {
    action: input.action ?? "update",
    type: "Issue",
    createdAt: new Date().toISOString(),
    organizationId: ORG_ID,
    webhookTimestamp: Date.now(),
    webhookId: "wh-issue",
    ...(input.updatedFrom ? { updatedFrom: input.updatedFrom } : {}),
    data: {
      id: ISSUE_ID,
      assigneeId:
        input.assigneeId === undefined ? BOT_USER_ID : input.assigneeId,
      ...(input.delegateId !== undefined
        ? { delegateId: input.delegateId }
        : {}),
      updatedAt: input.updatedAt,
    },
  };
}

async function postWebhook(payload: unknown): Promise<Response> {
  const body = JSON.stringify(payload);
  return bot.app.request("/api/webhooks/linear", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "linear-signature": createHmac("sha256", WEBHOOK_SECRET)
        .update(body)
        .digest("hex"),
    },
    body,
  });
}

function appendedTexts(): string[] {
  return codexApi.appends.flatMap((append) =>
    sessionMessageTexts(append.body.messages),
  );
}

// Text parts of every execute prompt on a thread — the issue context rides here
// (as a contextPreamble) ahead of the user's message.
function executeInputTexts(threadKey: string): string[] {
  return codexApi.executes
    .filter((e) => e.threadKey === threadKey)
    .flatMap((e) => e.body.input_lines)
    .flatMap(inputLineTexts);
}

function inputLineTexts(line: string): string[] {
  try {
    const parsed = JSON.parse(line) as {
      message?: { content?: Array<{ text?: unknown }> };
    };
    return (parsed.message?.content ?? []).flatMap((part) =>
      typeof part.text === "string" ? [part.text] : [],
    );
  } catch {
    return [];
  }
}

// Number of append requests on a thread whose messages contain `text` — used to
// assert a non-mention follow-up is appended (and appended exactly once).
function appendCount(threadKey: string, text: string): number {
  return codexApi.appends.filter(
    (a) =>
      a.threadKey === threadKey &&
      sessionMessageTexts(a.body.messages).some((t) => t.includes(text)),
  ).length;
}

function sessionMessageTexts(messages: LinearbotSessionMessage[]): string[] {
  return messages.flatMap((message) =>
    message.parts.flatMap((part) => {
      if (
        part &&
        typeof part === "object" &&
        !Array.isArray(part) &&
        part.type === "text" &&
        typeof part.text === "string"
      ) {
        return [part.text];
      }
      return [];
    }),
  );
}

async function waitFor(
  condition: () => boolean,
  timeoutMs = 5_000,
): Promise<void> {
  const startedAt = Date.now();
  while (!condition()) {
    if (Date.now() - startedAt > timeoutMs) {
      throw new Error("waitFor timed out");
    }
    await Bun.sleep(10);
  }
}

// ---------------------------------------------------------------------------
// Codex App Server sample stream (mirrors the discordbot harness)
// ---------------------------------------------------------------------------

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

// The thinking phase only: turn start, the answer item, a reasoning delta, and a
// completed command — enough to settle the first chain-of-thought line, with no
// answer text or terminal event. Pair with answerOutputLines to finish the run.
function thinkingOnlyOutputLines(): string[] {
  return sampleCodexNotifications("")
    .slice(0, 5)
    .map((notification) => JSON.stringify(notification));
}

// The answer delta + terminal event that finalize a run begun with
// thinkingOnlyOutputLines.
function answerOutputLines(answer: string): string[] {
  return [
    JSON.stringify({
      method: "item/agentMessage/delta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "answer-1",
        delta: answer,
      },
    }),
    JSON.stringify({
      type: "turn.completed",
      turn: { id: "turn-1", items: [] },
    }),
  ];
}

function sampleCodexNotifications(
  answer: string,
): Array<Record<string, unknown>> {
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
          id: "answer-1",
          text: "",
          phase: "final_answer",
          memoryCitation: null,
        },
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
  ];
}

// ---------------------------------------------------------------------------
// Fake Linear GraphQL API
// ---------------------------------------------------------------------------

type RecordedActivity = {
  agentSessionId: string;
  content:
    | { type: "thought"; body: string }
    | { type: "action"; action: string; parameter: string; result?: string }
    | { type: "response"; body: string }
    | { type: "error"; body: string };
  ephemeral?: boolean;
};

type FakeComment = {
  body: string;
  botActor?: { id: string; name: string };
  id: string;
  parentId?: string;
  userId?: string;
};

type RecordedBotComment = {
  id: string;
  issueId: string;
  body: string;
  parentId?: string;
};

type RecordedReaction = { id: string; commentId: string; emoji: string };

type FakeLinearApi = {
  activities: RecordedActivity[];
  addAgentSession(input: { id: string; rootCommentId?: string }): void;
  addUserComment(input: { body: string; parentId?: string }): string;
  botComments: RecordedBotComment[];
  close(): void;
  issueStateUpdates: Array<{ issueId: string; stateId: string }>;
  reactions: RecordedReaction[];
  removedReactionIds: string[];
  reset(): void;
  setIssueDelegate(userId: string | null): void;
  unhandledOperations: string[];
  url: string;
};

const WORKFLOW_STATES = [
  { id: "st-triage", name: "Triage", position: 0, type: "triage" },
  { id: "st-todo", name: "Todo", position: 1, type: "unstarted" },
  { id: "st-progress", name: "In Progress", position: 2, type: "started" },
  { id: "st-done", name: "Done", position: 3, type: "completed" },
];

function startFakeLinearApi(): FakeLinearApi {
  const activities: RecordedActivity[] = [];
  const botComments: RecordedBotComment[] = [];
  const reactions: RecordedReaction[] = [];
  const removedReactionIds: string[] = [];
  const comments = new Map<string, FakeComment>();
  const sessions = new Map<string, { id: string; rootCommentId?: string }>();
  const unhandledOperations: string[] = [];
  const issueStateUpdates: Array<{ issueId: string; stateId: string }> = [];
  let issueDelegateId: string | null = null;
  let issueStateId = "st-todo";
  let idCounter = 0;
  const nextId = (prefix: string) => `${prefix}-${++idCounter}`;

  const commentNode = (comment: FakeComment) => ({
    id: comment.id,
    body: comment.body,
    createdAt: "2026-06-10T00:00:00.000Z",
    updatedAt: "2026-06-10T00:00:00.000Z",
    issueId: ISSUE_ID,
    parentId: comment.parentId ?? null,
    url: `https://linear.app/acme/comment/${comment.id}`,
    reactionData: [],
    reactions: [],
    botActor: comment.botActor
      ? {
          id: comment.botActor.id,
          name: comment.botActor.name,
          userDisplayName: comment.botActor.name,
          avatarUrl: null,
        }
      : null,
    user: comment.userId ? { id: comment.userId } : null,
  });

  const userNode = (id: string) => ({
    id,
    name: "Ada Lovelace",
    displayName: "ada",
    email: "ada@example.com",
    avatarUrl: null,
    active: true,
    admin: false,
    app: false,
    createdAt: "2026-06-01T00:00:00.000Z",
    updatedAt: "2026-06-01T00:00:00.000Z",
  });

  const handle = (query: string, variables: Record<string, unknown>) => {
    if (query.includes("LinearbotIssueStatus")) {
      const currentState = WORKFLOW_STATES.find(
        (state) => state.id === issueStateId,
      );
      return {
        issue: {
          id: String(variables.issueId ?? ISSUE_ID),
          delegate: issueDelegateId ? { id: issueDelegateId } : null,
          state: currentState
            ? { id: currentState.id, type: currentState.type }
            : null,
          team: { states: { nodes: WORKFLOW_STATES } },
        },
      };
    }
    if (query.includes("LinearbotIssueContext")) {
      return {
        issue: {
          identifier: "ENG-1",
          title: "Something broke",
          description: "The deploy fails on boot.",
          url: "https://linear.app/acme/issue/ENG-1",
          state: { name: "Todo" },
          delegate: issueDelegateId ? { id: issueDelegateId } : null,
        },
      };
    }
    if (query.includes("LinearbotIssueStateUpdate")) {
      const issueId = String(variables.issueId ?? "");
      const stateId = String(variables.stateId ?? "");
      issueStateUpdates.push({ issueId, stateId });
      issueStateId = stateId;
      return { issueUpdate: { success: true } };
    }
    if (query.includes("LinearAdapterViewerOrganization")) {
      return {
        viewer: {
          id: BOT_USER_ID,
          displayName: "centaur",
          organization: { id: ORG_ID },
        },
      };
    }
    if (query.includes("LinearbotBotProfile")) {
      return {
        viewer: {
          id: BOT_USER_ID,
          url: `https://linear.app/acme/profiles/${BOT_PROFILE_HANDLE}`,
        },
      };
    }
    if (
      /mutation\s+(\w*[aA]gentActivityCreate|createAgentActivity)/.test(
        query,
      ) ||
      query.includes("agentActivityCreate(")
    ) {
      const input = variables.input as {
        agentSessionId: string;
        content: RecordedActivity["content"];
        ephemeral?: boolean;
      };
      activities.push({
        agentSessionId: input.agentSessionId,
        content: input.content,
        ...(input.ephemeral !== undefined
          ? { ephemeral: input.ephemeral }
          : {}),
      });
      const activityId = nextId("activity");
      const backingCommentId = nextId("bot-comment");
      comments.set(backingCommentId, {
        id: backingCommentId,
        body: "body" in input.content ? (input.content.body ?? "") : "",
        botActor: { id: BOT_USER_ID, name: "centaur" },
      });
      return {
        agentActivityCreate: {
          lastSyncId: idCounter,
          success: true,
          agentActivity: {
            id: activityId,
            createdAt: "2026-06-10T00:00:01.000Z",
            updatedAt: "2026-06-10T00:00:01.000Z",
            ephemeral: input.ephemeral ?? false,
            content: input.content,
            agentSession: { id: input.agentSessionId },
            sourceComment: { id: backingCommentId },
            user: { id: BOT_USER_ID },
          },
        },
      };
    }
    if (query.includes("agentSessionUpdate(")) {
      return { agentSessionUpdate: { success: true, lastSyncId: idCounter } };
    }
    if (query.includes("LinearbotCommentCreate")) {
      const id = nextId("bot-reply");
      const parentId = variables.parentId
        ? String(variables.parentId)
        : undefined;
      const body = String(variables.body ?? "");
      comments.set(id, {
        id,
        body,
        parentId,
        botActor: { id: BOT_USER_ID, name: "centaur" },
      });
      botComments.push({
        id,
        issueId: String(variables.issueId ?? ""),
        body,
        parentId,
      });
      return { commentCreate: { success: true, comment: { id } } };
    }
    if (query.includes("LinearbotCommentUpdate")) {
      const id = String(variables.id ?? "");
      const body = String(variables.body ?? "");
      const existing = comments.get(id);
      if (existing) existing.body = body;
      const record = botComments.find((comment) => comment.id === id);
      if (record) record.body = body;
      return { commentUpdate: { success: true } };
    }
    if (query.includes("LinearbotReactionCreate")) {
      const id = nextId("reaction");
      reactions.push({
        id,
        commentId: String(variables.commentId ?? ""),
        emoji: String(variables.emoji ?? ""),
      });
      return { reactionCreate: { reaction: { id } } };
    }
    if (query.includes("LinearbotReactionDelete")) {
      removedReactionIds.push(String(variables.id ?? ""));
      return { reactionDelete: { success: true } };
    }
    if (
      /query\s+agentActivity\b/i.test(query) ||
      query.includes("agentActivity(id:")
    ) {
      // The adapter re-fetches the created activity by id; the stored
      // backing comment id is the most recent one.
      const id = String(variables.id ?? "");
      const backingCommentId = Array.from(comments.keys())
        .filter((key) => key.startsWith("bot-comment"))
        .pop();
      return {
        agentActivity: {
          id,
          createdAt: "2026-06-10T00:00:01.000Z",
          updatedAt: "2026-06-10T00:00:01.000Z",
          ephemeral: false,
          content: { type: "response", body: "" },
          agentSession: {
            id: activities[activities.length - 1]?.agentSessionId ?? "",
          },
          sourceComment: backingCommentId ? { id: backingCommentId } : null,
          user: { id: BOT_USER_ID },
        },
      };
    }
    if (
      /query\s+agentSession\b/i.test(query) ||
      query.includes("agentSession(id:")
    ) {
      const id = String(variables.id ?? "");
      const session = sessions.get(id);
      if (!session) return { agentSession: null };
      return {
        agentSession: {
          id: session.id,
          issueId: ISSUE_ID,
          createdAt: "2026-06-10T00:00:00.000Z",
          updatedAt: "2026-06-10T00:00:00.000Z",
          externalLinks: [],
          externalUrls: null,
          context: null,
          status: "active",
          appUser: { id: BOT_USER_ID },
          comment: session.rootCommentId ? { id: session.rootCommentId } : null,
          creator: { id: USER_ID },
          issue: { id: ISSUE_ID },
        },
      };
    }
    if (/query\s+comments\b/i.test(query) || /\bcomments\(/.test(query)) {
      const filter = variables.filter as
        | { parent?: { id?: { eq?: string } } }
        | undefined;
      const parentId = filter?.parent?.id?.eq;
      const nodes = Array.from(comments.values())
        .filter((comment) => comment.parentId === parentId)
        .map(commentNode);
      return {
        comments: {
          nodes,
          pageInfo: { hasNextPage: false, endCursor: null },
        },
      };
    }
    if (/query\s+comment\b/i.test(query) || /\bcomment\(/.test(query)) {
      const id = String(
        variables.id ?? (variables as { commentId?: string }).commentId ?? "",
      );
      const comment = comments.get(id);
      return { comment: comment ? commentNode(comment) : null };
    }
    if (/query\s+user\b/i.test(query) || /\buser\(/.test(query)) {
      return { user: userNode(String(variables.id ?? USER_ID)) };
    }
    return undefined;
  };

  const server = Bun.serve({
    port: 0,
    fetch: async (request) => {
      const body = (await request.json()) as {
        query: string;
        variables?: Record<string, unknown>;
      };
      const data = handle(body.query, body.variables ?? {});
      if (data === undefined) {
        const operation =
          /(query|mutation)\s+(\w+)/.exec(body.query)?.[2] ??
          body.query.slice(0, 120);
        unhandledOperations.push(operation);
        return Response.json(
          { errors: [{ message: `unhandled operation: ${operation}` }] },
          { status: 400 },
        );
      }
      return Response.json({ data });
    },
  });

  return {
    activities,
    botComments,
    issueStateUpdates,
    reactions,
    removedReactionIds,
    unhandledOperations,
    url: `http://127.0.0.1:${server.port}/graphql`,
    setIssueDelegate(userId) {
      issueDelegateId = userId;
    },
    addUserComment(input) {
      const id = nextId("comment");
      comments.set(id, {
        id,
        body: input.body,
        parentId: input.parentId,
        userId: USER_ID,
      });
      return id;
    },
    addAgentSession(input) {
      sessions.set(input.id, input);
    },
    close() {
      server.stop(true);
    },
    reset() {
      activities.length = 0;
      botComments.length = 0;
      reactions.length = 0;
      removedReactionIds.length = 0;
      comments.clear();
      sessions.clear();
      unhandledOperations.length = 0;
      issueStateUpdates.length = 0;
      issueDelegateId = null;
      issueStateId = "st-todo";
      idCounter = 0;
    },
  };
}

// ---------------------------------------------------------------------------
// Mock api-rs session API
// ---------------------------------------------------------------------------

type MockSessionRequest<T> = { body: T; threadKey: string };

type MockSessionApi = {
  appends: MockSessionRequest<LinearbotAppendMessagesRequest>[];
  close(): void;
  creates: MockSessionRequest<LinearbotCreateSessionRequest>[];
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
  eventRequests: Array<{
    afterEventId: number;
    executionId?: string;
    threadKey: string;
  }>;
  executes: MockSessionRequest<LinearbotExecuteSessionRequest>[];
  reset(): void;
  url: string;
};

function startMockCodexApi(): MockSessionApi {
  const appends: MockSessionRequest<LinearbotAppendMessagesRequest>[] = [];
  const creates: MockSessionRequest<LinearbotCreateSessionRequest>[] = [];
  const executes: MockSessionRequest<LinearbotExecuteSessionRequest>[] = [];
  const eventRequests: Array<{
    afterEventId: number;
    executionId?: string;
    threadKey: string;
  }> = [];
  const idempotentExecutions = new Map<string, string>();
  type StreamHandle = {
    controller: ReadableStreamDefaultController<Uint8Array>;
    executionId?: string;
  };
  const streams = new Map<string, StreamHandle[]>();
  const pendingEvents = new Map<
    string,
    Array<{ data: string; event: string }>
  >();
  let eventId = 0;
  let executionCounter = 0;
  const encoder = new TextEncoder();

  const sseChunk = (event: string, data: string): Uint8Array => {
    eventId += 1;
    return encoder.encode(`id: ${eventId}\nevent: ${event}\ndata: ${data}\n\n`);
  };

  const emit = (
    threadKey: string,
    event: string,
    data: string,
    executionId?: string,
  ) => {
    const handles = (streams.get(threadKey) ?? []).filter(
      (handle) =>
        !executionId ||
        !handle.executionId ||
        handle.executionId === executionId,
    );
    if (handles.length === 0) {
      const queue = pendingEvents.get(threadKey) ?? [];
      queue.push({ data, event });
      pendingEvents.set(threadKey, queue);
      return;
    }
    for (const handle of handles) {
      try {
        handle.controller.enqueue(sseChunk(event, data));
      } catch {
        // Stream already closed.
      }
    }
  };

  const server = Bun.serve({
    port: 0,
    idleTimeout: 60,
    fetch: async (request) => {
      const url = new URL(request.url);
      const match = url.pathname.match(/^\/api\/session\/([^/]+)(?:\/(.+))?$/);
      if (!match) return Response.json({ error: "not found" }, { status: 404 });
      const threadKey = decodeURIComponent(match[1]!);
      const suffix = match[2];

      if (request.method === "POST" && !suffix) {
        const body = (await request.json()) as LinearbotCreateSessionRequest;
        creates.push({ body, threadKey });
        return Response.json({ ok: true });
      }
      if (request.method === "POST" && suffix === "messages") {
        const body = (await request.json()) as LinearbotAppendMessagesRequest;
        appends.push({ body, threadKey });
        return Response.json({ ok: true });
      }
      if (request.method === "POST" && suffix === "execute") {
        const body = (await request.json()) as LinearbotExecuteSessionRequest;
        executes.push({ body, threadKey });
        const idempotencyKey = `${threadKey}:${body.idempotency_key ?? ""}`;
        let executionId = idempotentExecutions.get(idempotencyKey);
        if (!executionId) {
          executionCounter += 1;
          executionId = `exec-${executionCounter}`;
          idempotentExecutions.set(idempotencyKey, executionId);
        }
        return Response.json({
          execution_id: executionId,
          ok: true,
          status: "running",
          thread_key: threadKey,
        });
      }
      if (request.method === "GET" && suffix === "events") {
        const executionId = url.searchParams.get("execution_id") ?? undefined;
        eventRequests.push({
          afterEventId: Number(url.searchParams.get("after_event_id") ?? 0),
          ...(executionId ? { executionId } : {}),
          threadKey,
        });
        const stream = new ReadableStream<Uint8Array>({
          start(controller) {
            const handle: StreamHandle = { controller, executionId };
            const handles = streams.get(threadKey) ?? [];
            handles.push(handle);
            streams.set(threadKey, handles);
            controller.enqueue(encoder.encode(": connected\n\n"));
            const queued = pendingEvents.get(threadKey) ?? [];
            pendingEvents.delete(threadKey);
            for (const item of queued) {
              controller.enqueue(sseChunk(item.event, item.data));
            }
          },
          cancel() {
            // Consumer cancelled; drop the handle on next emit.
          },
        });
        return new Response(stream, {
          headers: { "content-type": "text/event-stream" },
        });
      }
      return Response.json({ error: "unsupported" }, { status: 405 });
    },
  });

  return {
    appends,
    creates,
    eventRequests,
    executes,
    url: `http://127.0.0.1:${server.port}`,
    emitOutputLines(threadKey, lines, executionId) {
      for (const line of lines) {
        emit(threadKey, "session.output.line", line, executionId);
      }
    },
    emitSessionEvent(threadKey, event, data, executionId) {
      emit(threadKey, event, JSON.stringify(data), executionId);
    },
    close() {
      server.stop(true);
    },
    reset() {
      appends.length = 0;
      creates.length = 0;
      eventRequests.length = 0;
      executes.length = 0;
      idempotentExecutions.clear();
      pendingEvents.clear();
      for (const handles of streams.values()) {
        for (const handle of handles) {
          try {
            handle.controller.close();
          } catch {
            // already closed
          }
        }
      }
      streams.clear();
      eventId = 0;
      executionCounter = 0;
    },
  };
}
