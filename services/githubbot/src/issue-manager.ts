import { backgroundWaitUntil } from "./context";
import { DEFAULT_ISSUE_PROMPT } from "./issue-prompt";
import type { PrManagerContext } from "./pr-manager";
import { reactWorkingOnSubject, settleSubjectReaction } from "./reactions";
import { runTurnStream } from "./turn";
import type {
  ForwardSessionInput,
  GithubbotApiMessage,
  GithubbotTrace,
} from "./types";
import { errorMessage, noopLogger, nowMs, stringValue, traceLog } from "./utils";

/**
 * Issues, like PRs, are worked on assignment: assigning an issue to the bot is
 * the signal to pick it up. On the `issues` `assigned` webhook (when the bot is
 * among the assignees), the bot runs an autonomous work turn — read the issue,
 * implement a fix, and open a PR (self-assigning that PR so it then drives it to
 * merge via the PR-management flow). The methodology is the bundled
 * DEFAULT_ISSUE_PROMPT unless the deployment fully replaces it via
 * options.issuePrompt.
 *
 * The work runs on its own isolated session thread (`github-issue:{owner}/{repo}:
 * {n}`), kept separate from the issue's conversation thread so a work run never
 * shares a sandbox with chit-chat — but persistent per issue, so a re-assignment
 * builds on the prior attempt. The agent does all GitHub I/O via `gh`, so the bot
 * does not post through the adapter.
 */

// Reuse the PR manager's context shape (octokit + options + state + userName);
// the two managers share the same GitHub credentials and KV store.
type IssueManagerContext = PrManagerContext;

// Assignment webhooks are de-duplicated by delivery id for a week — long enough
// to cover GitHub's redelivery window without growing state unboundedly.
const ISSUE_WORK_DEDUP_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const ASSIGNED_CACHE_TTL_MS = 10 * 60 * 1000;

export function issueWorkThreadKey(
  owner: string,
  repo: string,
  n: number,
): string {
  return `github-issue:${owner}/${repo}:${n}`;
}

/** `issues` lifecycle: on `assigned` to the bot, run an autonomous work turn. */
export function handleIssueEvent(
  ctx: IssueManagerContext,
  rawBody: string,
  deliveryId: string,
): Promise<void> | null {
  const payload = parseJson(rawBody);
  if (!payload) return null;
  if (stringValue(payload.action) !== "assigned") return null;
  const issue = isRecord(payload.issue) ? payload.issue : null;
  const repo = repoFromPayload(payload);
  if (!issue || !repo) return null;
  const number = numberValue(issue.number);
  if (number === undefined) return null;
  if (stringValue(issue.state) !== "open") return null;
  if (!isAssignedToBot(assigneeLogins(issue.assignees), ctx.userName)) {
    // A different assignee — not ours to act on.
    return null;
  }

  const { options, state } = ctx;
  const title = stringValue(issue.title) ?? `#${number}`;
  const url =
    stringValue(issue.html_url) ??
    `https://github.com/${repo.owner}/${repo.repo}/issues/${number}`;
  const assigner =
    stringValue(isRecord(payload.sender) ? payload.sender.login : undefined) ??
    "a teammate";
  const threadKey = issueWorkThreadKey(repo.owner, repo.repo, number);
  const trace: GithubbotTrace = {
    includeContext: false,
    messageId: `issue-${threadKey}-${deliveryId}`,
    mode: "execute",
    openStream: true,
    startedAtMs: nowMs(),
    threadId: threadKey,
  };

  return (async () => {
    const logger = options.logger ?? noopLogger;
    // Claim the delivery before the background run so a redelivery never
    // double-works. State-keyed (not Chat-thread-keyed) because the work thread
    // is synthetic and never touches the adapter.
    const dedupKey = `${options.stateKeyPrefix ?? "centaur-githubbot"}:issue-delivery:${threadKey}:${deliveryId}`;
    let claimed = true;
    try {
      claimed = await state.setIfNotExists(
        dedupKey,
        "1",
        ISSUE_WORK_DEDUP_TTL_MS,
      );
    } catch (error) {
      logger.debug("githubbot_issue_dedup_failed", {
        error: errorMessage(error),
      });
    }
    if (!claimed) {
      traceLog(options, "githubbot_issue_duplicate_skipped", trace, {
        delivery_id: deliveryId,
      });
      return;
    }
    traceLog(options, "githubbot_issue_assigned", trace, {
      assigner,
      issue: `${repo.owner}/${repo.repo}#${number}`,
    });
    // No triggering comment on an assignment, so ack on the issue itself —
    // instant 👀, settled to 🚀/😕 when the work turn finishes.
    await reactWorkingOnSubject(ctx.octokit, repo.owner, repo.repo, number, logger);

    let lastEventId = 0;
    const forwardInput: ForwardSessionInput = {
      afterEventId: 0,
      // The full issue-work methodology rides as the context preamble; a
      // deployment can fully replace it via options.issuePrompt.
      contextPreamble: options.issuePrompt ?? DEFAULT_ISSUE_PROMPT,
      conversationName: `${repo.owner}/${repo.repo}#${number}: ${title}`,
      executeMessage: issueTriggerMessage({
        assigner,
        deliveryId,
        number,
        owner: repo.owner,
        repo: repo.repo,
        threadKey,
        title,
        url,
      }),
      messages: [],
      model: undefined,
      onEventId: (eventId) => {
        lastEventId = Math.max(lastEventId, eventId);
        forwardInput.afterEventId = lastEventId;
      },
      openStream: false,
      threadId: threadKey,
      trace,
    };

    backgroundWaitUntil(
      runTurnStream(options, forwardInput)
        .then(async (result) => {
          traceLog(options, "githubbot_issue_turn_complete", trace, {
            failed: result.failed,
          });
          await settleSubjectReaction(
            ctx.octokit,
            repo.owner,
            repo.repo,
            number,
            result.failed,
            logger,
          );
        })
        .catch(async (error) => {
          logger.warn("githubbot_issue_turn_failed", {
            error: errorMessage(error),
          });
          await settleSubjectReaction(
            ctx.octokit,
            repo.owner,
            repo.repo,
            number,
            true,
            logger,
          );
        }),
    );
  })();
}

/**
 * Whether an issue is assigned to the bot, cached briefly so the conversational
 * path doesn't hit the API on every comment. Mirrors the PR manager's isPrOwned;
 * a stale result only affects which session a reply shares context with.
 */
export async function isIssueAssignedToBot(
  ctx: IssueManagerContext,
  owner: string,
  repo: string,
  number: number,
): Promise<boolean> {
  const cacheKey = `${ctx.options.stateKeyPrefix ?? "centaur-githubbot"}:issue-assigned-cache:${owner}/${repo}#${number}`;
  try {
    const cached = await ctx.state.get<string>(cacheKey);
    if (cached === "1") return true;
    if (cached === "0") return false;
  } catch {
    // fall through to a live lookup
  }
  let assigned = false;
  try {
    const { data } = await ctx.octokit.rest.issues.get({
      owner,
      repo,
      issue_number: number,
    });
    assigned = isAssignedToBot(
      assigneeLogins(data.assignees),
      ctx.userName,
    );
  } catch (error) {
    (ctx.options.logger ?? noopLogger).debug(
      "githubbot_issue_assignment_lookup_failed",
      { error: errorMessage(error) },
    );
    return false;
  }
  try {
    await ctx.state.set(cacheKey, assigned ? "1" : "0", ASSIGNED_CACHE_TTL_MS);
  } catch {
    // best-effort cache
  }
  return assigned;
}

function issueTriggerMessage(input: {
  assigner: string;
  deliveryId: string;
  number: number;
  owner: string;
  repo: string;
  threadKey: string;
  title: string;
  url: string;
}): GithubbotApiMessage {
  const text =
    `You have been assigned GitHub issue ${input.owner}/${input.repo}#${input.number} — ` +
    `"${input.title}" (${input.url}) by @${input.assigner}. Work it now, following ` +
    `your guidance above, using the gh CLI and git in your sandbox.`;
  return {
    attachments: [],
    author: {
      fullName: "GitHub",
      isBot: false,
      isMe: false,
      userId: "github-issue",
      userName: "github-issue",
    },
    // Keyed by delivery id so a fresh assignment re-executes (the state claim
    // dedupes a redelivery of the same assignment).
    id: `issue-${input.threadKey}-${input.deliveryId}`,
    isMention: true,
    raw: { githubbotIssueWork: true, url: input.url },
    text,
    threadId: input.threadKey,
    timestamp: new Date().toISOString(),
  };
}

// ---------------------------------------------------------------------------
// Payload parsing helpers (kept local so the issue manager is self-contained).
// ---------------------------------------------------------------------------

type JsonRecord = Record<string, unknown>;

export function isAssignedToBot(assignees: string[], userName: string): boolean {
  const target = userName.toLowerCase();
  return assignees.some((login) => login.toLowerCase() === target);
}

export function assigneeLogins(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const logins: string[] = [];
  for (const entry of value) {
    const login = isRecord(entry) ? stringValue(entry.login) : undefined;
    if (login) logins.push(login);
  }
  return logins;
}

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
