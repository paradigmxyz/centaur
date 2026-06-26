import { createHmac, timingSafeEqual } from "node:crypto";
import { createGitHubAdapter, type GitHubAdapter } from "@chat-adapter/github";
import { createPostgresState } from "@chat-adapter/state-pg";
import {
  Chat,
  type Adapter,
  type Logger,
  type Message as ChatMessage,
  type StateAdapter,
  type Thread,
} from "chat";
import { Hono, type Context } from "hono";
import pg from "pg";
import {
  authorAssociationFromRaw,
  isCommentAuthorAllowed,
} from "./authorization";
import { handleBodyMention } from "./body-mention";
import { backgroundWaitUntil, requestContext, waitUntil } from "./context";
import {
  handleIssueEvent,
  isIssueAssignedToBot,
  issueWorkThreadKey,
} from "./issue-manager";
import { extractMessageOverrides } from "./overrides";
import {
  handleCiEvent,
  handlePullRequestEvent,
  handleReviewEvent,
  isPrOwned,
  managementThreadKey,
  type PrManagerContext,
} from "./pr-manager";
import { handleReviewRequest } from "./review";
import {
  forwardToSessionApi,
  isRetryableSessionApiError,
  serializeMessage,
} from "./session-api";
import {
  githubContextPreamble,
  parseGithubThreadKey,
  reactSafe,
  reviewCommentContextFromRaw,
  runSessionTurn,
} from "./turn";
import type {
  Githubbot,
  GithubbotApiMessage,
  GithubbotOptions,
  GithubbotThreadState,
  GithubbotTrace,
} from "./types";
import { errorMessage, noopLogger, nowMs, traceLog } from "./utils";

export type {
  Githubbot,
  GithubbotApiMessage,
  GithubbotOptions,
  GithubbotThreadState,
} from "./types";

const POSTGRES_CONNECT_INITIAL_DELAY_MS = 250;
const POSTGRES_CONNECT_MAX_DELAY_MS = 10_000;
const DEDUP_WINDOW = 200;

export function createGithubbot(options: GithubbotOptions): Githubbot {
  const userName = options.userName ?? "github-bot";
  const logger = options.logger ?? noopLogger;
  const github = createGitHubAdapter({
    token: options.token,
    webhookSecret: options.webhookSecret,
    userName,
    ...(options.botUserId ? { botUserId: Number(options.botUserId) } : {}),
    ...(options.githubApiUrl ? { apiUrl: options.githubApiUrl } : {}),
    logger,
  });
  const state = options.state ?? createDefaultState(options, logger);
  const chat = new Chat<{ github: typeof github }, GithubbotThreadState>({
    userName,
    adapters: { github },
    state,
    // Serialize handling per thread so a redelivered or near-simultaneous comment
    // can't run two handlers at once. The conversational dedup below claims a
    // message id via a read-modify-write on thread state, so the deprecated
    // onLockConflict: "force" would let two concurrent deliveries both pass the
    // claim and double-execute/double-reply. "drop" keeps the lock (one runs, the
    // duplicate is dropped) — matching discordbot, which migrated off "force" for
    // exactly this reason.
    concurrency: "drop",
    // The GitHub adapter buffers a turn and posts one comment (it rate-limits
    // edits), so the SDK's post+edit streaming placeholder doesn't apply — the
    // final answer posts once when the run settles.
    fallbackStreamingPlaceholderText: null,
    logger,
  });

  // A comment that @-mentions the bot opens (or continues) the PR/issue thread's
  // session and is answered in-thread. Subscribe so later non-mention replies in
  // the same thread flow in as context.
  chat.onNewMention(async (thread, message) => {
    await handleMessage(thread, message, {
      adapter: github,
      mode: "execute",
      options,
      prManagerCtx,
      subscribe: true,
    });
  });

  // Follow-ups in a subscribed thread: a further @-mention runs another turn; a
  // plain comment is ingested as context for the next turn (mirrors slackbotv2).
  chat.onSubscribedMessage(async (thread, message) => {
    await handleMessage(thread, message, {
      adapter: github,
      mode: message.isMention === true ? "execute" : "append",
      options,
      prManagerCtx,
    });
  });

  let chatInitialized = false;
  const ensureChatInitialized = async (): Promise<void> => {
    if (chatInitialized) return;
    try {
      await chat.initialize();
      chatInitialized = true;
    } catch (error) {
      logger.warn("githubbot_chat_initialize_failed", {
        error: errorMessage(error),
      });
    }
  };

  const prManagerCtx: PrManagerContext = {
    octokit: github.octokit,
    options,
    state,
    userName,
  };

  const app = new Hono();
  app.get("/health", (c) => c.json({ ok: true, service: "githubbot" }));

  const handleGithubWebhook = async (c: Context) => {
    const eventType = c.req.header("x-github-event") ?? "";
    const deliveryId = c.req.header("x-github-delivery") ?? "";
    await ensureChatInitialized();
    const context = {
      retryableErrors: [],
      waitUntil: (p: Promise<unknown>) => waitUntil(c, p),
    };

    // Comment threads (mentions, follow-ups) are the adapter's domain: it
    // verifies the signature and maps the payload to a thread/message that
    // drives onNewMention/onSubscribedMessage.
    if (
      eventType === "issue_comment" ||
      eventType === "pull_request_review_comment"
    ) {
      return requestContext.run(context, () =>
        chat.webhooks.github(c.req.raw, {
          waitUntil: (p) => waitUntil(c, p),
        }),
      );
    }

    // Every other event is a lifecycle event the adapter ignores (review
    // requests in v1; PR/review/CI events in v2). The adapter never sees these
    // bodies, so verify the signature ourselves before acting.
    if (!LIFECYCLE_EVENTS.has(eventType)) {
      return new globalThis.Response("ok", { status: 200 });
    }
    const rawBody = await c.req.raw.clone().text();
    if (
      !verifyGithubSignature(
        rawBody,
        c.req.header("x-hub-signature-256"),
        options.webhookSecret,
      )
    ) {
      return new globalThis.Response("invalid signature", { status: 401 });
    }
    const handled = requestContext.run(context, () =>
      Promise.all([
        routeLifecycleEvent(eventType, rawBody, {
          botUserName: userName,
          deliveryId,
          options,
          prManagerCtx,
          state,
        }) ?? undefined,
        // Orthogonal to the lifecycle routes: an @-mention in a freshly-opened
        // issue/PR body runs a conversational turn (the adapter only sees
        // comments, so this is the only place a body mention is caught).
        handleBodyMention(prManagerCtx, eventType, rawBody) ?? undefined,
      ]),
    );
    waitUntil(c, handled);
    return new globalThis.Response("ok", { status: 200 });
  };
  app.post("/api/webhooks/github", handleGithubWebhook);

  if (options.connectStateOnStart !== false) {
    void ensureStateConnected(state, options).then(ensureChatInitialized);
  }

  return { app, chat };
}

type MessageHandlerInput = {
  adapter: GitHubAdapter;
  mode: "execute" | "append";
  options: GithubbotOptions;
  prManagerCtx: PrManagerContext;
  subscribe?: boolean;
};

/**
 * Routes one inbound comment: an @-mention runs an agent turn and replies; a
 * plain follow-up in an active thread is appended as context for the next turn.
 * Dedups against thread state so a webhook redelivery never double-acts, and
 * never acts on the bot's own comments.
 */
async function handleMessage(
  thread: Thread<GithubbotThreadState>,
  message: ChatMessage,
  input: MessageHandlerInput,
): Promise<void> {
  const { adapter, mode, options } = input;
  const logger = options.logger ?? noopLogger;
  if (message.author.isMe) {
    traceLog(options, "githubbot_self_message_skipped", undefined, {
      message_id: message.id,
      thread_id: thread.id,
    });
    return;
  }
  // Only sufficiently-trusted authors may drive a turn: the agent runs in a
  // write-capable sandbox and posts its transcript back, so an untrusted
  // commenter must not be able to steer it (or read its tool output). Gates both
  // mentions (execute) and follow-up context (append). Fails closed.
  if (!isCommentAuthorAllowed(message.raw, options)) {
    traceLog(options, "githubbot_unauthorized_author_skipped", undefined, {
      association: authorAssociationFromRaw(message.raw) ?? "unknown",
      message_id: message.id,
      thread_id: thread.id,
    });
    return;
  }
  const threadKey = thread.id;
  const threadState = (await thread.state) ?? {};

  if (mode === "execute") {
    if ((threadState.repliedMessageIds ?? []).includes(message.id)) {
      traceLog(options, "githubbot_comment_duplicate_skipped", undefined, {
        message_id: message.id,
        thread_id: threadKey,
      });
      return;
    }
    // Claim before the background run so a redelivery never double-replies.
    await thread.setState({
      repliedMessageIds: [
        ...(threadState.repliedMessageIds ?? []),
        message.id,
      ].slice(-DEDUP_WINDOW),
    });
    // Fire the 👀 working ack now — before subscribe, the ownership lookup, and
    // message serialization — so it lands instantly instead of waiting on the
    // turn's setup round-trips. Best-effort; the turn settles it to 🚀/😕.
    void reactSafe(adapter, threadKey, message.id, "eyes", logger);
    if (input.subscribe) {
      try {
        await thread.subscribe();
      } catch (error) {
        logger.debug("githubbot_subscribe_failed", {
          error: errorMessage(error),
        });
      }
    }
    const sessionThreadKey = await resolveManagementSession(
      thread,
      threadKey,
      input,
    );
    const serialized = await serializeMessage(message);
    const overrides = extractMessageOverrides(serialized.text);
    serialized.text = overrides.cleanedText;
    const trace: GithubbotTrace = {
      includeContext: false,
      messageId: message.id,
      mode: "execute",
      openStream: true,
      startedAtMs: nowMs(),
      threadId: threadKey,
    };
    const reviewComment = reviewCommentContextFromRaw(message.raw);
    backgroundWaitUntil(
      runSessionTurn({
        adapter,
        contextPreamble: githubContextPreamble(threadKey, reviewComment),
        executeMessage: serialized,
        options,
        overrides: { harnessType: overrides.harnessType, model: overrides.model },
        reactMessageId: message.id,
        sessionThreadKey,
        thread,
        threadKey,
        trace,
      }).catch((error) => {
        logger.warn("githubbot_turn_failed", { error: errorMessage(error) });
      }),
    );
    return;
  }

  // append mode: only ingest into threads the bot is already active in.
  const isActiveThread =
    threadState.historyForwarded === true ||
    (threadState.repliedMessageIds ?? []).length > 0;
  if (!isActiveThread) {
    traceLog(options, "githubbot_followup_inactive_thread_skipped", undefined, {
      message_id: message.id,
      thread_id: threadKey,
    });
    return;
  }
  if ((threadState.ingestedMessageIds ?? []).includes(message.id)) {
    traceLog(options, "githubbot_followup_duplicate_skipped", undefined, {
      message_id: message.id,
      thread_id: threadKey,
    });
    return;
  }
  await thread.setState({
    ingestedMessageIds: [
      ...(threadState.ingestedMessageIds ?? []),
      message.id,
    ].slice(-DEDUP_WINDOW),
  });
  const sessionKey = threadState.managementSessionKey ?? threadKey;
  const serialized = await serializeMessage(message);
  const trace: GithubbotTrace = {
    includeContext: false,
    messageId: message.id,
    mode: "execute",
    openStream: false,
    startedAtMs: nowMs(),
    threadId: sessionKey,
  };
  backgroundWaitUntil(appendFollowup(options, serialized, sessionKey, trace));
}

/**
 * When a conversation thread maps to work the bot owns, return that work's
 * session key so the turn shares the sandbox/context the bot uses for it — while
 * replies still post to this thread. For an owned PR that's the management
 * session (`github-manage:…`, where it fixes CI and addresses reviews); for an
 * issue assigned to the bot it's the issue-work session (`github-issue:…`).
 * Returns undefined when the thread maps to neither (the turn then runs on its
 * own conversation session) or on lookup failure. The resolved key is cached on
 * the thread so follow-ups skip the lookup.
 */
async function resolveManagementSession(
  thread: Thread<GithubbotThreadState>,
  threadKey: string,
  input: MessageHandlerInput,
): Promise<string | undefined> {
  const ref = parseGithubThreadKey(threadKey);
  if (!ref) return undefined;
  let sessionKey: string | undefined;
  try {
    if (ref.type === "pr") {
      if (
        await isPrOwned(input.prManagerCtx, ref.owner, ref.repo, ref.number)
      ) {
        sessionKey = managementThreadKey(ref.owner, ref.repo, ref.number);
      }
    } else if (
      await isIssueAssignedToBot(
        input.prManagerCtx,
        ref.owner,
        ref.repo,
        ref.number,
      )
    ) {
      sessionKey = issueWorkThreadKey(ref.owner, ref.repo, ref.number);
    }
  } catch (error) {
    (input.options.logger ?? noopLogger).debug(
      "githubbot_ownership_lookup_failed",
      { error: errorMessage(error) },
    );
    return undefined;
  }
  if (!sessionKey) return undefined;
  try {
    await thread.setState({ managementSessionKey: sessionKey });
  } catch {
    // best-effort; follow-ups will just re-resolve
  }
  return sessionKey;
}

/**
 * Appends a non-mention follow-up to its session as context (create is
 * idempotent; no execute, no reply). Best-effort — context enrichment must not
 * surface errors to the thread.
 */
async function appendFollowup(
  options: GithubbotOptions,
  serialized: GithubbotApiMessage,
  threadKey: string,
  trace: GithubbotTrace,
): Promise<void> {
  try {
    await forwardToSessionApi(
      options,
      {
        afterEventId: 0,
        executeMessage: undefined,
        messages: [serialized],
        onEventId: () => undefined,
        openStream: false,
        threadId: threadKey,
        trace,
      },
      {},
    );
    traceLog(options, "githubbot_followup_appended", trace, {
      message_id: serialized.id,
    });
  } catch (error) {
    (options.logger ?? noopLogger).warn("githubbot_followup_append_failed", {
      error: errorMessage(error),
    });
  }
}

const LIFECYCLE_EVENTS = new Set([
  "pull_request",
  "pull_request_review",
  "issues",
  "check_run",
  "check_suite",
  "status",
  "workflow_run",
]);

/**
 * Route a signature-verified lifecycle event: PR review requests go to v1's
 * review handler; everything else (PR/review/CI lifecycle) goes to the v2 PR
 * manager. Returns the work promise (awaited for the webhook's keep-alive) or
 * null when there's nothing to do.
 */
function routeLifecycleEvent(
  eventType: string,
  rawBody: string,
  input: {
    botUserName: string;
    deliveryId: string;
    options: GithubbotOptions;
    prManagerCtx: PrManagerContext;
    state: StateAdapter;
  },
): Promise<void> | null {
  if (eventType === "pull_request") {
    if (pullRequestAction(rawBody) === "review_requested") {
      return handleReviewRequest(rawBody, {
        botUserName: input.botUserName,
        deliveryId: input.deliveryId,
        octokit: input.prManagerCtx.octokit,
        options: input.options,
        state: input.state,
      });
    }
    return handlePullRequestEvent(input.prManagerCtx, rawBody);
  }
  if (eventType === "pull_request_review") {
    return handleReviewEvent(input.prManagerCtx, rawBody);
  }
  if (eventType === "issues") {
    return handleIssueEvent(input.prManagerCtx, rawBody, input.deliveryId);
  }
  return handleCiEvent(input.prManagerCtx, eventType, rawBody);
}

function pullRequestAction(rawBody: string): string | undefined {
  try {
    const payload = JSON.parse(rawBody) as { action?: unknown };
    return typeof payload.action === "string" ? payload.action : undefined;
  } catch {
    return undefined;
  }
}

/** Verify GitHub's `X-Hub-Signature-256` HMAC over the raw body. */
function verifyGithubSignature(
  body: string,
  signature: string | undefined,
  secret: string,
): boolean {
  if (!signature) return false;
  const expected = `sha256=${createHmac("sha256", secret).update(body, "utf8").digest("hex")}`;
  const provided = Buffer.from(signature);
  const computed = Buffer.from(expected);
  if (provided.length !== computed.length) return false;
  return timingSafeEqual(provided, computed);
}

function createDefaultState(
  options: GithubbotOptions,
  logger: Logger,
): StateAdapter {
  const stateLogger = logger.child("postgres-state");
  // Own the pool so we can attach an error handler. pg.Pool emits 'error' for
  // idle clients whose connection drops (Postgres restart, or a transient blip
  // while the pod's network is still being programmed at startup). With no
  // listener, node-postgres rethrows it as an uncaught exception and the process
  // crashes/spews. Logging and swallowing lets the pool reconnect on the next query.
  const pool = new pg.Pool({ connectionString: options.postgresUrl });
  pool.on("error", (error) => {
    stateLogger.warn("postgres pool error", { error: errorMessage(error) });
  });
  return createPostgresState({
    client: pool,
    keyPrefix: options.stateKeyPrefix ?? "centaur-githubbot",
    logger: stateLogger,
  });
}

/**
 * Blocks until the state backend accepts a connection, retrying with exponential
 * backoff. The first DB connection fires within milliseconds of process start
 * and can lose a race with the pod's network programming (a one-off
 * ECONNREFUSED). Retrying absorbs that race; the first successful connect also
 * flips the adapter's `connected` flag, so the message path comes alive too.
 */
async function ensureStateConnected(
  state: StateAdapter,
  options: GithubbotOptions,
): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      await state.connect();
      if (attempt > 0) {
        traceLog(options, "githubbot_postgres_connected", undefined, {
          attempts: attempt + 1,
        });
      }
      return;
    } catch (error) {
      const delayMs = Math.min(
        POSTGRES_CONNECT_INITIAL_DELAY_MS * 2 ** attempt,
        POSTGRES_CONNECT_MAX_DELAY_MS,
      );
      traceLog(options, "githubbot_postgres_connect_retry", undefined, {
        attempt: attempt + 1,
        delay_ms: delayMs,
        error: errorMessage(error),
      });
      await sleep(delayMs);
    }
  }
}

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
