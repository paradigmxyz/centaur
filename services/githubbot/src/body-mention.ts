import { resolveAllowedAuthorAssociations } from "./authorization";
import { backgroundWaitUntil } from "./context";
import { buildCommentReplyBody } from "./comment-bot";
import type { PrManagerContext } from "./pr-manager";
import { reactWorkingOnSubject, settleSubjectReaction } from "./reactions";
import { githubContextPreamble, runTurnStream } from "./turn";
import type {
  ForwardSessionInput,
  GithubbotApiMessage,
  GithubbotTrace,
} from "./types";
import { errorMessage, noopLogger, nowMs, stringValue, traceLog } from "./utils";

/**
 * The adapter only surfaces issue/PR *comments*, so an @-mention written into the
 * body of a freshly-opened issue or pull request never reaches the conversational
 * path. This handler closes that parity gap: on `issues`/`pull_request` `opened`,
 * if the body mentions the bot (and the author is allowed), it runs a
 * conversational turn keyed to the issue/PR thread and posts the reply as a
 * comment — the same outcome as if the mention had arrived as a comment.
 *
 * Only `opened` is handled (not `edited`): re-triggering on every later edit would
 * be noisy and hard to dedup, and a mention can always be (re-)issued as a comment.
 */
export function handleBodyMention(
  ctx: PrManagerContext,
  eventType: string,
  rawBody: string,
): Promise<void> | null {
  if (eventType !== "issues" && eventType !== "pull_request") return null;
  const payload = parseJson(rawBody);
  if (!payload || stringValue(payload.action) !== "opened") return null;

  const isPr = eventType === "pull_request";
  const node = isPr
    ? isRecord(payload.pull_request)
      ? payload.pull_request
      : null
    : isRecord(payload.issue)
      ? payload.issue
      : null;
  const repo = repoFromPayload(payload);
  if (!node || !repo) return null;
  const number = numberValue(node.number);
  if (number === undefined) return null;

  const body = stringValue(node.body);
  if (!body || !mentionsBot(body, ctx.userName)) return null;

  // Never act on the bot's own issue/PR (it opens PRs during issue work).
  const author = stringValue(isRecord(node.user) ? node.user.login : undefined);
  if (author && author.toLowerCase() === ctx.userName.toLowerCase()) return null;

  // Same trust gate as the comment path, read from the issue/PR author.
  const allowed = resolveAllowedAuthorAssociations(
    ctx.options.allowedAuthorAssociations,
  );
  const association = stringValue(node.author_association)?.toUpperCase();
  const authorized =
    allowed.includes("*") || (!!association && allowed.includes(association));
  const { options, state } = ctx;
  const threadKey = isPr
    ? `github:${repo.owner}/${repo.repo}:${number}`
    : `github:${repo.owner}/${repo.repo}:issue:${number}`;
  const trace: GithubbotTrace = {
    includeContext: false,
    messageId: `body-${threadKey}`,
    mode: "execute",
    openStream: true,
    startedAtMs: nowMs(),
    threadId: threadKey,
  };
  if (!authorized) {
    traceLog(options, "githubbot_body_mention_unauthorized_skipped", trace, {
      association: association ?? "unknown",
    });
    return null;
  }

  return (async () => {
    const logger = options.logger ?? noopLogger;
    // A body mention fires at most once per subject; claim before the run so a
    // redelivery of the `opened` webhook never double-replies.
    const dedupKey = `${options.stateKeyPrefix ?? "centaur-githubbot"}:body-mention:${threadKey}`;
    let claimed = true;
    try {
      claimed = await state.setIfNotExists(dedupKey, "1", BODY_MENTION_TTL_MS);
    } catch (error) {
      logger.debug("githubbot_body_mention_dedup_failed", {
        error: errorMessage(error),
      });
    }
    if (!claimed) {
      traceLog(options, "githubbot_body_mention_duplicate_skipped", trace);
      return;
    }
    traceLog(options, "githubbot_body_mention", trace, {
      subject: `${repo.owner}/${repo.repo}#${number}`,
    });
    await reactWorkingOnSubject(ctx.octokit, repo.owner, repo.repo, number, logger);

    const forwardInput: ForwardSessionInput = {
      afterEventId: 0,
      contextPreamble: githubContextPreamble(threadKey),
      conversationName: `${repo.owner}/${repo.repo}#${number}`,
      executeMessage: bodyMentionMessage(threadKey, number, body),
      messages: [],
      model: undefined,
      onEventId: () => undefined,
      openStream: false,
      threadId: threadKey,
      trace,
    };

    const result = await runTurnStream(options, forwardInput);
    const reply = result.failed
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
      await ctx.octokit.rest.issues.createComment({
        owner: repo.owner,
        repo: repo.repo,
        issue_number: number,
        body: reply,
      });
    } catch (error) {
      logger.warn("githubbot_body_mention_reply_failed", {
        error: errorMessage(error),
      });
    }
    await settleSubjectReaction(
      ctx.octokit,
      repo.owner,
      repo.repo,
      number,
      result.failed,
      logger,
    );
  })();
}

const BODY_MENTION_TTL_MS = 7 * 24 * 60 * 60 * 1000;

/** Whether `body` contains a standalone @-mention of `userName` (case-insensitive). */
export function mentionsBot(body: string, userName: string): boolean {
  const escaped = userName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|[^a-zA-Z0-9_/-])@${escaped}(?![a-zA-Z0-9_-])`, "i").test(
    body,
  );
}

function bodyMentionMessage(
  threadKey: string,
  number: number,
  body: string,
): GithubbotApiMessage {
  return {
    attachments: [],
    author: {
      fullName: "GitHub",
      isBot: false,
      isMe: false,
      userId: "github-body-mention",
      userName: "github-body-mention",
    },
    id: `body-${threadKey}`,
    isMention: true,
    raw: { githubbotBodyMention: true },
    text: body,
    threadId: threadKey,
    timestamp: new Date().toISOString(),
  };
}

type JsonRecord = Record<string, unknown>;

function isRecord(value: unknown): value is JsonRecord {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function parseJson(rawBody: string): JsonRecord | null {
  try {
    const value = JSON.parse(rawBody);
    return isRecord(value) ? value : null;
  } catch {
    return null;
  }
}

function numberValue(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function repoFromPayload(
  payload: JsonRecord,
): { owner: string; repo: string } | null {
  const repository = payload.repository;
  if (!isRecord(repository)) return null;
  const fullName = stringValue(repository.full_name);
  if (!fullName) return null;
  const [owner, repo] = fullName.split("/", 2);
  if (!owner || !repo) return null;
  return { owner, repo };
}
