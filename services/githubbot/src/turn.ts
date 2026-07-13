import {
  codexAppServerToChatSdkStream,
  type CodexAppServerToChatStreamOptions,
  type RendererEvent,
} from "@centaur/rendering";
import type { GitHubAdapter } from "@chat-adapter/github";
import type { Thread } from "chat";
import { buildCommentReplyBody, CommentReplyCollector } from "./comment-bot";
import { runExclusive } from "./context";
import {
  executeSessionTurn,
  forwardToSessionApi,
  isRetryableSessionApiError,
  openSessionEventStream,
  sessionStreamError,
  startingStreamNotification,
} from "./session-api";
import type {
  ForwardSessionInput,
  GithubbotApiMessage,
  GithubbotExecuteSessionResponse,
  GithubbotOptions,
  GithubbotRendererSource,
  GithubbotThreadState,
  GithubbotTrace,
} from "./types";
import { errorMessage, noopLogger, traceLog } from "./utils";

const THREAD_TURN_MAX_RETRIES = 3;
const RENDER_RETRY_INITIAL_DELAY_MS = 250;
const RENDER_RETRY_MAX_DELAY_MS = 5_000;
const REVIEW_HUNK_MAX_CHARS = 4_000;

/** Decoded GitHub thread key (mirrors the adapter's encodeThreadId formats). */
export type GithubThreadRef = {
  owner: string;
  repo: string;
  number: number;
  type: "pr" | "issue";
  reviewCommentId?: number;
};

/** The file/line/hunk a review-comment thread is anchored to. */
export type ReviewCommentContext = {
  path?: string;
  line?: number;
  diffHunk?: string;
};

/** Accumulated result of one streamed agent turn. */
export type TurnResult = {
  answer: string;
  cotLines: string[];
  errorText: string;
  failed: boolean;
  fallbackText: string;
};

const THREAD_KEY_PATTERN =
  /^github:([^/:]+)\/([^:]+):(?:issue:(\d+)|(\d+)(?::rc:(\d+))?)$/;

/**
 * Parse a `github:{owner}/{repo}:{number}` style thread key back into its parts.
 * Returns null for keys that don't match (e.g. the synthetic `github-review:…`
 * key, or a non-GitHub key).
 */
export function parseGithubThreadKey(threadKey: string): GithubThreadRef | null {
  const match = THREAD_KEY_PATTERN.exec(threadKey);
  if (!match) return null;
  const issueNumber = match[3];
  const number = Number(issueNumber ?? match[4]);
  if (!Number.isFinite(number)) return null;
  return {
    owner: match[1]!,
    repo: match[2]!,
    number,
    type: issueNumber ? "issue" : "pr",
    ...(match[5] ? { reviewCommentId: Number(match[5]) } : {}),
  };
}

/**
 * Pull the file/line/diff-hunk a review-comment message is anchored to out of the
 * adapter's raw message (`{type: "review_comment", comment}`). Returns undefined
 * for PR-conversation or issue messages.
 */
export function reviewCommentContextFromRaw(
  raw: unknown,
): ReviewCommentContext | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const record = raw as { type?: unknown; comment?: unknown };
  if (record.type !== "review_comment") return undefined;
  if (!record.comment || typeof record.comment !== "object") return undefined;
  const comment = record.comment as {
    path?: unknown;
    line?: unknown;
    diff_hunk?: unknown;
  };
  return {
    path: typeof comment.path === "string" ? comment.path : undefined,
    line: typeof comment.line === "number" ? comment.line : undefined,
    diffHunk:
      typeof comment.diff_hunk === "string" ? comment.diff_hunk : undefined,
  };
}

/**
 * Per-turn context header naming the PR/issue the thread maps to and how to
 * reply. The agent runs in a sandbox with `gh`/git, so it fetches the details
 * itself — this anchors it to the right subject. For a review-comment thread it
 * also carries the file/line and diff hunk the thread is pinned to, since that
 * location is the whole point of the thread.
 */
export function githubContextPreamble(
  threadKey: string,
  reviewComment?: ReviewCommentContext,
): string | undefined {
  const ref = parseGithubThreadKey(threadKey);
  if (!ref) return undefined;
  const subject = `${ref.owner}/${ref.repo}#${ref.number}`;

  if (ref.type === "issue") {
    return (
      `You are responding in the comment thread of GitHub issue ${subject}. ` +
      `Fetch the issue's details with the gh CLI in your sandbox (e.g. ` +
      `\`gh issue view ${ref.number}\`) for any context the comment doesn't ` +
      `give you. Your turn's final message is posted back as your reply here.`
    );
  }

  if (ref.reviewCommentId !== undefined) {
    const location = reviewComment?.path
      ? `\`${reviewComment.path}\`${reviewComment.line ? ` line ${reviewComment.line}` : ""}`
      : "a specific line";
    const hunk = reviewComment?.diffHunk
      ? `\n\nThe diff hunk this thread is anchored to:\n\n\`\`\`diff\n${truncate(reviewComment.diffHunk, REVIEW_HUNK_MAX_CHARS)}\n\`\`\``
      : "";
    return (
      `You are responding in a pull-request review-comment thread on ${subject}, ` +
      `pinned to ${location}. This is an independent thread scoped to that code ` +
      `location — keep your reply focused on it. Use the gh CLI and git in your ` +
      `sandbox to read the surrounding code and the full diff as needed. Your ` +
      `turn's final message is posted back as your reply in this thread.${hunk}`
    );
  }

  return (
    `You are responding in the main conversation thread of GitHub pull request ` +
    `${subject}. The comment alone may not be enough context, so fetch the PR ` +
    `before replying — use the gh CLI in your sandbox (e.g. \`gh pr view ` +
    `${ref.number}\`, \`gh pr diff ${ref.number}\`). Your turn's final message ` +
    `is posted back as your reply in this thread.`
  );
}

export async function reactSafe(
  adapter: GitHubAdapter,
  threadKey: string,
  messageId: string,
  emoji: string,
  logger: GithubbotOptions["logger"],
): Promise<void> {
  try {
    await adapter.addReaction(threadKey, messageId, emoji);
  } catch (error) {
    (logger ?? noopLogger).debug("githubbot_reaction_failed", {
      error: errorMessage(error),
    });
  }
}

/**
 * Run the create/append + execute + stream + collect core for one turn, with a
 * bounded retry on transient (cold-start) failures. Thread-agnostic: it operates
 * on the session API alone, so both the conversational path (which has a Chat
 * thread to post into) and the review path (which posts via the agent's own gh
 * calls) share it.
 */
export function runTurnStream(
  options: GithubbotOptions,
  forwardInput: ForwardSessionInput,
): Promise<TurnResult> {
  // Serialize turns targeting the same session so a conversational mention and a
  // lifecycle-driven management turn (both keyed to `github-manage:…`) can't run
  // concurrently in one sandbox and interleave git/push operations. Different
  // session keys still run in parallel.
  return runExclusive(forwardInput.threadId, () =>
    runTurnStreamInner(options, forwardInput),
  );
}

async function runTurnStreamInner(
  options: GithubbotOptions,
  forwardInput: ForwardSessionInput,
): Promise<TurnResult> {
  const logger = options.logger ?? noopLogger;
  for (let attempt = 0; attempt <= THREAD_TURN_MAX_RETRIES; attempt++) {
    try {
      // create + append (idempotent), then execute + stream.
      await forwardToSessionApi(
        options,
        { ...forwardInput, executeMessage: undefined, openStream: false },
        {},
      );
      const collector = new CommentReplyCollector();
      const fallback = new GithubRenderFallback();
      for await (const chunk of codexAppServerToChatSdkStream(
        fallback.collectSource(streamSessionAfterHandoff(options, forwardInput)),
        rendererOptions(options),
      )) {
        collector.update(chunk);
      }
      return {
        answer: collector.answer,
        cotLines: collector.cotLines,
        errorText: collector.errorText,
        failed: collector.failed,
        fallbackText: fallback.text(),
      };
    } catch (error) {
      if (
        isRetryableSessionApiError(error) &&
        attempt < THREAD_TURN_MAX_RETRIES
      ) {
        traceLog(options, "githubbot_turn_stream_retry", forwardInput.trace, {
          retry_attempt: attempt + 1,
        });
        await sleep(renderRetryDelayMs(attempt));
        continue;
      }
      logger.warn("githubbot_turn_stream_failed", {
        error: errorMessage(error),
      });
      return {
        answer: "",
        cotLines: [],
        errorText: errorMessage(error),
        failed: true,
        fallbackText: "",
      };
    }
  }
  return {
    answer: "",
    cotLines: [],
    errorText: "exhausted retries",
    failed: true,
    fallbackText: "",
  };
}

/**
 * Runs one conversational agent turn on a thread's sandbox and posts the result
 * as a single comment. A 👀 reaction acks the triggering comment while the bot
 * works, then settles to 🚀 (done) or 😕 (failed). The answer streams into a
 * collector and posts once at the end — GitHub rate-limits comment edits, so v1
 * buffers rather than live-editing.
 */
export async function runSessionTurn(input: {
  adapter: GitHubAdapter;
  contextPreamble?: string;
  conversationName?: string;
  executeMessage: GithubbotApiMessage;
  options: GithubbotOptions;
  overrides: { harnessType?: string; model?: string };
  /** Comment to react to (👀 → 🚀/😕); the triggering comment, if any. */
  reactMessageId?: string;
  /**
   * Session/sandbox key, when it differs from the posting thread — e.g. an
   * owned PR's conversation mention runs in the PR's management session
   * (`github-manage:…`) for shared context but still posts to the
   * conversation thread. Defaults to `threadKey`.
   */
  sessionThreadKey?: string;
  thread: Thread<GithubbotThreadState>;
  threadKey: string;
  trace: GithubbotTrace;
}): Promise<void> {
  const {
    adapter,
    conversationName,
    executeMessage,
    options,
    overrides,
    reactMessageId,
    thread,
    threadKey,
    trace,
  } = input;
  const logger = options.logger ?? noopLogger;
  // The 👀 working ack is fired by the caller (handleMessage) before this turn's
  // setup so it lands instantly; here we only settle it to 🚀/😕 at the end.
  const threadState = (await thread.state) ?? {};
  let lastEventId = threadState.lastEventId ?? 0;
  const forwardInput: ForwardSessionInput = {
    afterEventId: lastEventId,
    contextPreamble: input.contextPreamble ?? githubContextPreamble(threadKey),
    conversationName,
    executeMessage,
    harnessType: overrides.harnessType,
    messages: [],
    model: overrides.model,
    onEventId: (eventId) => {
      lastEventId = Math.max(lastEventId, eventId);
      forwardInput.afterEventId = lastEventId;
    },
    openStream: false,
    threadId: input.sessionThreadKey ?? threadKey,
    trace,
  };

  const result = await runTurnStream(options, forwardInput);
  await thread.setState({
    contextSeeded: true,
    historyForwarded: true,
    lastEventId,
  });
  const body = result.failed
    ? buildCommentReplyBody({
        answer: `⚠️ I ran into an error before finishing:\n\n${result.errorText || "unknown error"}`,
        cotLines: result.cotLines,
      })
    : buildCommentReplyBody({
        answer: result.answer,
        cotLines: result.cotLines,
        fallback: result.fallbackText,
      });
  try {
    await thread.post(body);
  } catch (error) {
    logger.warn("githubbot_thread_reply_failed", {
      error: errorMessage(error),
    });
  }
  if (reactMessageId) {
    await reactSafe(
      adapter,
      threadKey,
      reactMessageId,
      result.failed ? "confused" : "rocket",
      logger,
    );
  }
  traceLog(options, "githubbot_thread_turn_complete", trace, {
    chars: body.length,
    failed: result.failed,
  });
}

/**
 * Captures the terminal result text from the raw session stream so the final
 * answer has a fallback when the chat-SDK mapper emits no markdown.
 */
export class GithubRenderFallback {
  private terminalText = "";

  async *collectSource(
    stream: AsyncIterable<GithubbotRendererSource>,
  ): AsyncIterable<GithubbotRendererSource> {
    for await (const event of stream) {
      this.captureTerminalText(event);
      yield event;
    }
  }

  text(): string {
    return this.terminalText.trim();
  }

  private captureTerminalText(event: GithubbotRendererSource): void {
    if (!event || typeof event !== "object") return;
    const eventKind = String(
      "eventKind" in event
        ? event.eventKind
        : "event" in event
          ? event.event
          : "",
    );
    if (
      eventKind !== "session.execution_completed" &&
      eventKind !== "session.execution_cancelled" &&
      !isTerminalCodexAppServerEvent(event)
    ) {
      return;
    }
    const data =
      "data" in event && event.data && typeof event.data === "object"
        ? event.data
        : event;
    const text = terminalResultText(data);
    if (text) this.terminalText = text;
  }
}

function isTerminalCodexAppServerEvent(event: unknown): boolean {
  if (!event || typeof event !== "object") return false;
  const type = (event as { type?: unknown }).type;
  return type === "result" || type === "turn.done" || type === "turn.completed";
}

function terminalResultText(event: unknown): string {
  if (!event || typeof event !== "object") return "";
  for (const key of ["result", "result_text", "text", "final_text"]) {
    const value = (event as Record<string, unknown>)[key];
    if (typeof value !== "string") continue;
    const resultText = value.trim();
    if (resultText) return resultText;
  }
  return "";
}

async function* streamSessionAfterHandoff(
  options: GithubbotOptions,
  input: ForwardSessionInput,
  onExecutionStarted?: (
    execution: GithubbotExecuteSessionResponse,
  ) => Promise<void>,
): AsyncIterable<GithubbotRendererSource> {
  // The synthetic starting item primes the mapper's task state so answer deltas
  // stream without the pre-stream grace delay. Execute runs here, inside the
  // render stream, so a sandbox-spawn failure surfaces in the same render
  // rather than leaving the run looking alive forever (api-rs writes no event
  // if the spawn itself fails).
  yield startingStreamNotification(input.threadId);
  traceLog(options, "githubbot_stream_heartbeat_emitted", input.trace);

  if (input.executeMessage) {
    try {
      const execution = await executeSessionTurn(options, input);
      if (execution) {
        // Scope the event stream we open below to this execution.
        input.executionId = execution.execution_id;
        await onExecutionStarted?.(execution);
      }
    } catch (error) {
      traceLog(options, "githubbot_forward_failed", input.trace, {
        error: errorMessage(error),
      });
      if (isRetryableSessionApiError(error)) throw error;
      yield sessionStreamError(error);
      return;
    }
  }

  let stream: AsyncIterable<GithubbotRendererSource>;
  try {
    stream = await openSessionEventStream(options, input);
  } catch (error) {
    traceLog(options, "githubbot_forward_failed", input.trace, {
      error: errorMessage(error),
    });
    if (isRetryableSessionApiError(error)) throw error;
    yield sessionStreamError(error);
    return;
  }

  for await (const event of stream) yield event;
}

// Vestigial wrapper kept so call sites diff cleanly against slackbotv2, whose
// rendererOptions hooks onRendererEvent to update the Slack assistant title (no
// GitHub analog). Today it only forwards the configured mapper.
function rendererOptions(
  options: GithubbotOptions,
): CodexAppServerToChatStreamOptions {
  const mapper = options.mapper;
  return {
    ...mapper,
    async onRendererEvent(event: RendererEvent) {
      await mapper?.onRendererEvent?.(event);
    },
  };
}

function renderRetryDelayMs(attempt: number): number {
  return Math.min(
    RENDER_RETRY_INITIAL_DELAY_MS * 2 ** attempt,
    RENDER_RETRY_MAX_DELAY_MS,
  );
}

function truncate(text: string, maxChars: number): string {
  if (text.length <= maxChars) return text;
  return `${text.slice(0, maxChars)}\n…(truncated)`;
}

async function sleep(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
