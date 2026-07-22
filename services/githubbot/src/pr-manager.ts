import type { GitHubAdapter } from "@chat-adapter/github";
import type { StateAdapter } from "chat";
import { backgroundWaitUntil } from "./context";
import { runTurnStream } from "./turn";
import type {
  ForwardSessionInput,
  GithubbotApiMessage,
  GithubbotOptions,
  GithubbotTrace,
} from "./types";
import { errorMessage, noopLogger, nowMs, stringValue, traceLog } from "./utils";

/**
 * v2: PR self-management for PRs the bot owns (it authored them, or they carry
 * the managed label). Reacts to PR/review/CI lifecycle webhooks to drive an
 * owned PR toward merge:
 *  - Fix CI    — once *all* checks for a head SHA are settled and red, run a
 *                bounded fix turn (the agent diagnoses + pushes via gh).
 *  - Address review — one holistic turn per submitted review.
 *  - Merge     — deterministic: when GitHub reports the PR mergeable (clean),
 *                the bot merges it directly (no agent — branch protection is the
 *                source of truth). dirty -> conflict turn; behind -> update.
 * Escalation tags a human and stops; the bot backs off human-authored commits.
 */
/** The Octokit instance the GitHub adapter exposes (its `.octokit` getter). */
type Octokit = GitHubAdapter["octokit"];

export type PrManagerContext = {
  octokit: Octokit;
  options: GithubbotOptions;
  state: StateAdapter;
  userName: string;
};

const STATE_TTL_MS = 90 * 24 * 60 * 60 * 1000;
const CLAIM_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const DEFAULT_CI_FIX_MAX_ATTEMPTS = 3;

// CI conclusions that count as a hard, fixable failure (neutral/skipped/success
// and the in-progress states are not failures).
const FAILED_CONCLUSIONS = new Set([
  "action_required",
  "cancelled",
  "failure",
  "stale",
  "timed_out",
]);

// ---------------------------------------------------------------------------
// Pure decision helpers (unit-tested without GitHub).
// ---------------------------------------------------------------------------

export type CiCheck = { status: string; conclusion: string | null; name: string };
export type CiStatus = { state: string; context: string };

export type CiEvaluation = {
  settled: boolean;
  failed: boolean;
  failingNames: string[];
};

/**
 * Decide whether all CI for a SHA is finished, and whether it's red. "Settled"
 * means no check run is still queued/in-progress and no legacy commit status is
 * pending — the point the user wants us to wait for before acting.
 */
export function evaluateCi(
  checks: CiCheck[],
  statuses: CiStatus[],
): CiEvaluation {
  const anyCheckPending = checks.some((c) => c.status !== "completed");
  const anyStatusPending = statuses.some((s) => s.state === "pending");
  const failingChecks = checks.filter(
    (c) =>
      c.status === "completed" &&
      c.conclusion !== null &&
      FAILED_CONCLUSIONS.has(c.conclusion),
  );
  const failingStatuses = statuses.filter(
    (s) => s.state === "failure" || s.state === "error",
  );
  const failingNames = [
    ...failingChecks.map((c) => c.name),
    ...failingStatuses.map((s) => s.context),
  ];
  return {
    settled: !anyCheckPending && !anyStatusPending,
    failed: failingNames.length > 0,
    failingNames,
  };
}

/**
 * A PR is bot-owned when the bot is one of its assignees. Ownership is purely an
 * assignment mechanism: assign the PR to the bot to have it manage the PR toward
 * merge (and unassign to hand it back).
 */
export function isOwnedPr(input: {
  assignees: string[];
  userName: string;
}): boolean {
  const target = input.userName.toLowerCase();
  return input.assignees.some((login) => login.toLowerCase() === target);
}

export type MergeDecision =
  | "merge"
  | "resolve_conflict"
  | "update_branch"
  | "wait"
  | "skip_disabled"
  | "skip_hold"
  | "skip_draft"
  | "skip_closed";

/**
 * Whether (and how) to act on merge-readiness. Branch protection is the source
 * of truth, surfaced as mergeable_state: only "clean" merges; "dirty" needs a
 * conflict turn; "behind" needs a branch update; everything else waits.
 */
export function decideMerge(input: {
  autoMerge: boolean;
  draft: boolean;
  holdLabel: string;
  labels: string[];
  merged: boolean;
  mergeableState: string;
  state: string;
}): MergeDecision {
  if (!input.autoMerge) return "skip_disabled";
  if (input.merged || input.state !== "open") return "skip_closed";
  if (input.draft) return "skip_draft";
  if (input.labels.map((l) => l.toLowerCase()).includes(input.holdLabel.toLowerCase())) {
    return "skip_hold";
  }
  if (input.mergeableState === "dirty") return "resolve_conflict";
  if (input.mergeableState === "behind") return "update_branch";
  if (input.mergeableState === "clean") return "merge";
  // blocked / unstable / unknown / has_hooks -> not cleanly mergeable yet.
  return "wait";
}

// ---------------------------------------------------------------------------
// Per-PR state (stored as a JSON blob in the shared KV).
// ---------------------------------------------------------------------------

type PrState = {
  consecutiveCiFixes?: number;
};

function prKey(ctx: PrManagerContext, owner: string, repo: string, n: number): string {
  return `${ctx.options.stateKeyPrefix ?? "centaur-githubbot"}:pr:${owner}/${repo}#${n}`;
}

export function managementThreadKey(
  owner: string,
  repo: string,
  n: number,
): string {
  return `github-manage:${owner}/${repo}:${n}`;
}

async function loadState(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  n: number,
): Promise<PrState> {
  try {
    return (await ctx.state.get<PrState>(prKey(ctx, owner, repo, n))) ?? {};
  } catch {
    return {};
  }
}

async function saveState(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  n: number,
  value: PrState,
): Promise<void> {
  try {
    await ctx.state.set(prKey(ctx, owner, repo, n), value, STATE_TTL_MS);
  } catch (error) {
    logger(ctx).debug("githubbot_pr_state_save_failed", {
      error: errorMessage(error),
    });
  }
}

async function claim(ctx: PrManagerContext, key: string): Promise<boolean> {
  try {
    return await ctx.state.setIfNotExists(key, "1", CLAIM_TTL_MS);
  } catch {
    // If the claim store is unavailable, proceed (better to act than to silently
    // drop work); the in-turn idempotency keys still guard double execution.
    return true;
  }
}

/**
 * Release a claim so the action it guarded can be retried on a later event.
 * Used when an irreversible side effect (the merge) fails after the claim is
 * taken — otherwise the stale claim would suppress every future attempt.
 */
async function release(ctx: PrManagerContext, key: string): Promise<void> {
  try {
    await ctx.state.delete(key);
  } catch {
    // best-effort; the claim's TTL eventually expires if delete fails.
  }
}

function logger(ctx: PrManagerContext) {
  return ctx.options.logger ?? noopLogger;
}

// ---------------------------------------------------------------------------
// Webhook handlers.
// ---------------------------------------------------------------------------

type PullRequestSummary = {
  assignees: string[];
  draft: boolean;
  headRef: string;
  headRepoFullName: string | null;
  headSha: string;
  labels: string[];
  mergeableState: string;
  merged: boolean;
  number: number;
  state: string;
  title: string;
};

function assigneeLogins(
  value: ({ login?: string } | null)[] | null | undefined,
): string[] {
  if (!value) return [];
  return value.map((a) => a?.login ?? "").filter(Boolean);
}

function summarizePr(pr: {
  draft?: boolean | null;
  head: { ref: string; repo?: { full_name?: string | null } | null; sha: string };
  labels: { name?: string }[];
  mergeable_state?: string;
  merged?: boolean;
  number: number;
  state: string;
  title: string;
  assignees?: ({ login?: string } | null)[] | null;
}): PullRequestSummary {
  return {
    assignees: assigneeLogins(pr.assignees),
    draft: pr.draft === true,
    headRef: pr.head.ref,
    headRepoFullName: pr.head.repo?.full_name ?? null,
    headSha: pr.head.sha,
    labels: pr.labels.map((l) => l.name ?? "").filter(Boolean),
    mergeableState: pr.mergeable_state ?? "unknown",
    merged: pr.merged === true,
    number: pr.number,
    state: pr.state,
    title: pr.title,
  };
}

async function fetchPr(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  n: number,
): Promise<PullRequestSummary | null> {
  try {
    const { data } = await ctx.octokit.rest.pulls.get({
      owner,
      repo,
      pull_number: n,
    });
    return summarizePr(data as Parameters<typeof summarizePr>[0]);
  } catch (error) {
    logger(ctx).warn("githubbot_pr_fetch_failed", {
      error: errorMessage(error),
      pr: `${owner}/${repo}#${n}`,
    });
    return null;
  }
}

const OWNED_CACHE_TTL_MS = 10 * 60 * 1000;

/**
 * Whether a PR is bot-owned, cached briefly so the conversational path doesn't
 * hit the API on every comment. Ownership rarely changes, and a stale "owned"
 * only affects which session a reply shares context with — low stakes.
 */
export async function isPrOwned(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  number: number,
): Promise<boolean> {
  const cacheKey = `${ctx.options.stateKeyPrefix ?? "centaur-githubbot"}:owned-cache:${owner}/${repo}#${number}`;
  try {
    const cached = await ctx.state.get<string>(cacheKey);
    if (cached === "1") return true;
    if (cached === "0") return false;
  } catch {
    // fall through to a live lookup
  }
  const pr = await fetchPr(ctx, owner, repo, number);
  const owned = pr ? owns(ctx, pr) : false;
  try {
    await ctx.state.set(cacheKey, owned ? "1" : "0", OWNED_CACHE_TTL_MS);
  } catch {
    // best-effort cache
  }
  return owned;
}

function owns(ctx: PrManagerContext, pr: PullRequestSummary): boolean {
  return isOwnedPr({ assignees: pr.assignees, userName: ctx.userName });
}

/** `pull_request` lifecycle (non-review_requested actions). */
export async function handlePullRequestEvent(
  ctx: PrManagerContext,
  rawBody: string,
): Promise<void> {
  const payload = parseJson(rawBody);
  if (!payload) return;
  const action = stringValue(payload.action);
  if (!action || action === "review_requested") return; // review_requested is v1's.
  const repo = repoFromPayload(payload);
  const prNode = payload.pull_request;
  if (!repo || !isRecord(prNode)) return;
  const number = numberValue(prNode.number);
  if (number === undefined) return;
  if (action === "closed") return; // nothing to drive once closed/merged.

  const pr = await fetchPr(ctx, repo.owner, repo.repo, number);
  if (!pr || !owns(ctx, pr)) return;
  // Being assigned the PR is the explicit signal to take it over: evaluate CI now
  // (forcing past the human-commit back-off — the assignment is a human handing
  // it to us) so an already-red or already-green PR is acted on immediately,
  // rather than only on the next lifecycle event. processCi fixes red CI or merges
  // when green.
  if (action === "assigned") {
    await processCi(ctx, repo.owner, repo.repo, number, pr.headSha, true);
    return;
  }
  // Any other state change that could flip mergeability re-evaluates the merge
  // gate; it's deterministic and idempotent, so over-calling is harmless.
  await tryMerge(ctx, repo.owner, repo.repo, number);
}

/** `pull_request_review` submitted -> address review, or merge on approval. */
export async function handleReviewEvent(
  ctx: PrManagerContext,
  rawBody: string,
): Promise<void> {
  const payload = parseJson(rawBody);
  if (!payload) return;
  if (stringValue(payload.action) !== "submitted") return;
  const repo = repoFromPayload(payload);
  const prNode = payload.pull_request;
  const reviewNode = payload.review;
  if (!repo || !isRecord(prNode) || !isRecord(reviewNode)) return;
  const number = numberValue(prNode.number);
  const reviewId = numberValue(reviewNode.id);
  if (number === undefined || reviewId === undefined) return;
  const reviewer = stringValue(isRecord(reviewNode.user) ? reviewNode.user.login : undefined);
  const reviewState = stringValue(reviewNode.state)?.toLowerCase();

  const pr = await fetchPr(ctx, repo.owner, repo.repo, number);
  if (!pr || !owns(ctx, pr)) return;
  // Never act on the bot's own review (it shouldn't review its own PRs anyway).
  if (reviewer && reviewer.toLowerCase() === ctx.userName.toLowerCase()) return;

  if (
    !(await claim(
      ctx,
      `${ctx.options.stateKeyPrefix ?? "centaur-githubbot"}:review-handled:${repo.owner}/${repo.repo}#${number}:${reviewId}`,
    ))
  ) {
    return;
  }

  if (reviewState === "approved") {
    await tryMerge(ctx, repo.owner, repo.repo, number);
    return;
  }
  if (reviewState === "changes_requested" || reviewState === "commented") {
    fireAddressReviewTurn(ctx, repo.owner, repo.repo, pr, reviewer ?? "the reviewer", reviewId);
  }
}

/** check_run / check_suite / workflow_run completed -> CI-settled gate. */
export async function handleCiEvent(
  ctx: PrManagerContext,
  eventType: string,
  rawBody: string,
): Promise<void> {
  const payload = parseJson(rawBody);
  if (!payload) return;
  const repo = repoFromPayload(payload);
  if (!repo) return;
  const target = ciTarget(eventType, payload);
  if (!target) return;
  const prNumbers =
    target.prNumbers.length > 0
      ? target.prNumbers
      : await fetchPrNumbersForCommit(ctx, repo.owner, repo.repo, target.headSha);
  await Promise.all(
    prNumbers.map((number) => processCi(ctx, repo.owner, repo.repo, number, target.headSha)),
  );
}

async function processCi(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  number: number,
  headSha: string,
  force = false,
): Promise<void> {
  const pr = await fetchPr(ctx, owner, repo, number);
  if (!pr || !owns(ctx, pr)) return;
  // Ignore CI for a SHA that's already been superseded by a newer push.
  if (pr.headSha !== headSha) return;

  const evaluation = await fetchCiEvaluation(ctx, owner, repo, headSha);
  if (!evaluation.settled) return; // wait until *all* checks are done.
  // Act once per fully-settled SHA (the last-arriving check event wins).
  if (
    !(await claim(
      ctx,
      `${ctx.options.stateKeyPrefix ?? "centaur-githubbot"}:ci-settled:${owner}/${repo}#${number}:${headSha}`,
    ))
  ) {
    return;
  }

  const trace = makeTrace(managementThreadKey(owner, repo, number), `ci-${headSha}`);
  if (!evaluation.failed) {
    // Green: reset the fix counter and consider merging.
    const state = await loadState(ctx, owner, repo, number);
    if (state.consecutiveCiFixes) {
      await saveState(ctx, owner, repo, number, { ...state, consecutiveCiFixes: 0 });
    }
    traceLog(ctx.options, "githubbot_ci_green", trace, { pr: `${owner}/${repo}#${number}` });
    await tryMerge(ctx, owner, repo, number);
    return;
  }

  // Red: back off if a human pushed the failing commit (don't step on them) —
  // unless this is a forced takeover (the PR was just assigned to us, so the
  // human has explicitly handed it over and we fix it regardless of who pushed).
  if (!force) {
    const headAuthor = await commitAuthor(ctx, owner, repo, headSha);
    if (headAuthor && headAuthor.toLowerCase() !== ctx.userName.toLowerCase()) {
      traceLog(ctx.options, "githubbot_ci_human_commit_skipped", trace, {
        author: headAuthor,
      });
      return;
    }
  }

  const maxAttempts = ctx.options.ciFixMaxAttempts ?? DEFAULT_CI_FIX_MAX_ATTEMPTS;
  const state = await loadState(ctx, owner, repo, number);
  const attempts = state.consecutiveCiFixes ?? 0;
  if (attempts >= maxAttempts) {
    await escalate(ctx, owner, repo, number, evaluation.failingNames, maxAttempts);
    return;
  }
  await saveState(ctx, owner, repo, number, {
    ...state,
    consecutiveCiFixes: attempts + 1,
  });
  fireCiFixTurn(ctx, owner, repo, pr, evaluation.failingNames, attempts + 1, maxAttempts);
}

/** Deterministic merge gate — no agent; GitHub's mergeable_state decides. */
async function tryMerge(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  number: number,
): Promise<void> {
  const pr = await fetchPr(ctx, owner, repo, number);
  if (!pr || !owns(ctx, pr)) return;
  const decision = decideMerge({
    autoMerge: ctx.options.autoMerge !== false,
    draft: pr.draft,
    holdLabel: ctx.options.holdLabel ?? "do-not-merge",
    labels: pr.labels,
    merged: pr.merged,
    mergeableState: pr.mergeableState,
    state: pr.state,
  });
  const trace = makeTrace(managementThreadKey(owner, repo, number), `merge-${pr.headSha}`);
  traceLog(ctx.options, "githubbot_merge_decision", trace, {
    decision,
    mergeable_state: pr.mergeableState,
    pr: `${owner}/${repo}#${number}`,
  });

  if (decision === "merge") {
    // The claim guards against two concurrent lifecycle events both calling
    // merge. It's released on failure (below) so a transient merge error — "Base
    // branch was modified", a secondary rate limit, a 5xx — is retried on the
    // next event instead of leaving a clean, approved PR permanently unmerged
    // behind a stale claim. On success the claim stays as the "merged" marker.
    const mergedClaimKey = `${ctx.options.stateKeyPrefix ?? "centaur-githubbot"}:merged:${owner}/${repo}#${number}:${pr.headSha}`;
    if (!(await claim(ctx, mergedClaimKey))) {
      return;
    }
    try {
      await ctx.octokit.rest.pulls.merge({
        owner,
        repo,
        pull_number: number,
        merge_method: ctx.options.mergeMethod ?? "squash",
      });
      traceLog(ctx.options, "githubbot_merged", trace, { pr: `${owner}/${repo}#${number}` });
      if (
        ctx.options.deleteBranchOnMerge !== false &&
        pr.headRepoFullName?.toLowerCase() === `${owner}/${repo}`.toLowerCase()
      ) {
        try {
          await ctx.octokit.rest.git.deleteRef({
            owner,
            repo,
            ref: `heads/${pr.headRef}`,
          });
        } catch (error) {
          logger(ctx).debug("githubbot_branch_delete_failed", {
            error: errorMessage(error),
          });
        }
      }
    } catch (error) {
      // Re-merging an already-merged PR is a no-op (decideMerge returns
      // skip_closed next time), so releasing on any failure is safe.
      await release(ctx, mergedClaimKey);
      logger(ctx).warn("githubbot_merge_failed", {
        error: errorMessage(error),
        pr: `${owner}/${repo}#${number}`,
      });
    }
    return;
  }
  if (decision === "update_branch") {
    try {
      await ctx.octokit.rest.pulls.updateBranch({ owner, repo, pull_number: number });
    } catch (error) {
      logger(ctx).debug("githubbot_update_branch_failed", {
        error: errorMessage(error),
      });
    }
    return;
  }
  if (decision === "resolve_conflict") {
    fireConflictTurn(ctx, owner, repo, pr);
  }
}

async function escalate(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  number: number,
  failingNames: string[],
  maxAttempts: number,
): Promise<void> {
  const handle = ctx.options.escalationHandle?.replace(/^@/, "");
  const mention = handle ? `@${handle} ` : "";
  const checks = failingNames.length ? failingNames.join(", ") : "the CI checks";
  const body =
    `${mention}I've tried to fix CI on this PR ${maxAttempts} times without ` +
    `success and am pausing automatic fixes. Still failing: ${checks}. ` +
    `Could a human take a look?`;
  try {
    await ctx.octokit.rest.issues.createComment({
      owner,
      repo,
      issue_number: number,
      body,
    });
    traceLog(
      ctx.options,
      "githubbot_ci_escalated",
      makeTrace(managementThreadKey(owner, repo, number), `escalate-${number}`),
      { pr: `${owner}/${repo}#${number}` },
    );
  } catch (error) {
    logger(ctx).warn("githubbot_escalation_failed", {
      error: errorMessage(error),
    });
  }
}

// ---------------------------------------------------------------------------
// Agentic turns (run on the management thread; the agent does GitHub I/O via gh).
// ---------------------------------------------------------------------------

function fireCiFixTurn(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  pr: PullRequestSummary,
  failingNames: string[],
  attempt: number,
  maxAttempts: number,
): void {
  const handle = ctx.options.escalationHandle?.replace(/^@/, "");
  const fallback = handle
    ? `if you can't tell, @-mention @${handle}`
    : "if you can't tell, @-mention a maintainer";
  const preamble =
    `CI failed on pull request ${owner}/${repo}#${pr.number} at commit ` +
    `${pr.headSha}. Failing checks: ${failingNames.join(", ") || "unknown"}.\n\n` +
    `Fix it in your sandbox:\n` +
    `- Pull the failing logs (e.g. \`gh pr checks ${pr.number}\`, ` +
    `\`gh run view <run-id> --log-failed\`), understand the failure, fix it, and ` +
    `push to the PR's head branch (${pr.headRef}).\n` +
    `- If a check is flaky (infra/timeout, not your code), you may re-run it once ` +
    `instead of changing code.\n` +
    `- If you cannot confidently fix it, do NOT push a guess. Post a comment on ` +
    `the PR summarizing what's failing and what you tried, and @-mention the right ` +
    `human — find them via \`git blame\` on the affected files, recent authors ` +
    `(\`git log\`), or GitHub's suggested reviewers; ${fallback}.\n\n` +
    `This is fix attempt ${attempt} of ${maxAttempts}.`;
  fireManagementTurn(ctx, owner, repo, pr, preamble, {
    id: `fix-${owner}/${repo}#${pr.number}-${pr.headSha}-${attempt}`,
    label: "ci-fix",
    text: `Fix the failing CI on ${owner}/${repo}#${pr.number}.`,
  });
}

function fireAddressReviewTurn(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  pr: PullRequestSummary,
  reviewer: string,
  reviewId: number,
): void {
  const preamble =
    `A review was submitted on pull request ${owner}/${repo}#${pr.number} ` +
    `(head ${pr.headSha}). Address it as the PR author, working in your sandbox:\n` +
    `- Read all of the feedback: \`gh pr view ${pr.number} --comments\` and the ` +
    `pull-request review-comments API.\n` +
    `- Make the changes you agree with in a single coherent commit and push to ` +
    `${pr.headRef}.\n` +
    `- Reply to each review thread saying what you changed; where you disagree, ` +
    `explain why, briefly and respectfully. Resolve the threads you've addressed.\n` +
    `- Re-request review from @${reviewer} once you've pushed.\n` +
    `- If a request is unclear or you can't address it, say so in the thread and ask.`;
  fireManagementTurn(ctx, owner, repo, pr, preamble, {
    id: `review-resp-${owner}/${repo}#${pr.number}-${reviewId}`,
    label: "address-review",
    text: `Address the review on ${owner}/${repo}#${pr.number} from @${reviewer}.`,
  });
}

function fireConflictTurn(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  pr: PullRequestSummary,
): void {
  const preamble =
    `Pull request ${owner}/${repo}#${pr.number} has merge conflicts with its ` +
    `base branch. In your sandbox, update ${pr.headRef} against the base (rebase ` +
    `or merge), resolve the conflicts correctly, and push. If the conflicts are ` +
    `non-trivial or you're unsure of the right resolution, stop and @-mention a ` +
    `human instead of force-pushing a guess.`;
  fireManagementTurn(ctx, owner, repo, pr, preamble, {
    id: `conflict-${owner}/${repo}#${pr.number}-${pr.headSha}`,
    label: "resolve-conflict",
    text: `Resolve the merge conflicts on ${owner}/${repo}#${pr.number}.`,
  });
}

function fireManagementTurn(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  pr: PullRequestSummary,
  preamble: string,
  message: { id: string; label: string; text: string },
): void {
  const threadKey = managementThreadKey(owner, repo, pr.number);
  const trace = makeTrace(threadKey, message.id);
  // A deployment can prepend its own constraints to the management methodology
  // (the per-action preamble still rides underneath).
  const guidance = ctx.options.managementPrompt;
  const contextPreamble = guidance ? `${guidance}\n\n${preamble}` : preamble;
  let lastEventId = 0;
  const forwardInput: ForwardSessionInput = {
    afterEventId: 0,
    contextPreamble,
    conversationName: `${owner}/${repo}#${pr.number}: ${pr.title}`,
    executeMessage: managementMessage(message.id, threadKey, message.text),
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
  traceLog(ctx.options, "githubbot_management_turn_started", trace, {
    pr: `${owner}/${repo}#${pr.number}`,
    work: message.label,
  });
  backgroundWaitUntil(
    runTurnStream(ctx.options, forwardInput)
      .then((result) => {
        traceLog(ctx.options, "githubbot_management_turn_complete", trace, {
          failed: result.failed,
          work: message.label,
        });
      })
      .catch((error) => {
        logger(ctx).warn("githubbot_management_turn_failed", {
          error: errorMessage(error),
          work: message.label,
        });
      }),
  );
}

function managementMessage(
  id: string,
  threadKey: string,
  text: string,
): GithubbotApiMessage {
  return {
    attachments: [],
    author: {
      fullName: "GitHub",
      isBot: false,
      isMe: false,
      userId: "github-pr-manager",
      userName: "github-pr-manager",
    },
    id,
    isMention: true,
    raw: { githubbotManagement: true },
    text,
    threadId: threadKey,
    timestamp: new Date().toISOString(),
  };
}

// ---------------------------------------------------------------------------
// GitHub API reads.
// ---------------------------------------------------------------------------

async function fetchCiEvaluation(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  sha: string,
): Promise<CiEvaluation> {
  const checks: CiCheck[] = [];
  const statuses: CiStatus[] = [];
  try {
    const { data } = await ctx.octokit.rest.checks.listForRef({
      owner,
      repo,
      ref: sha,
      per_page: 100,
    });
    for (const run of data.check_runs) {
      checks.push({ status: run.status, conclusion: run.conclusion, name: run.name });
    }
  } catch (error) {
    logger(ctx).debug("githubbot_checks_list_failed", { error: errorMessage(error) });
  }
  try {
    const { data } = await ctx.octokit.rest.repos.getCombinedStatusForRef({
      owner,
      repo,
      ref: sha,
    });
    for (const s of data.statuses) {
      statuses.push({ state: s.state, context: s.context });
    }
  } catch (error) {
    logger(ctx).debug("githubbot_status_fetch_failed", { error: errorMessage(error) });
  }
  return evaluateCi(checks, statuses);
}

async function commitAuthor(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  sha: string,
): Promise<string | undefined> {
  try {
    const { data } = await ctx.octokit.rest.repos.getCommit({ owner, repo, ref: sha });
    return data.author?.login ?? undefined;
  } catch {
    return undefined;
  }
}

async function fetchPrNumbersForCommit(
  ctx: PrManagerContext,
  owner: string,
  repo: string,
  sha: string,
): Promise<number[]> {
  try {
    const { data } =
      await ctx.octokit.rest.repos.listPullRequestsAssociatedWithCommit({
        owner,
        repo,
        commit_sha: sha,
      });
    return data.map((pr) => pr.number).filter((n) => typeof n === "number");
  } catch (error) {
    logger(ctx).debug("githubbot_commit_prs_fetch_failed", {
      error: errorMessage(error),
      ref: `${owner}/${repo}@${sha}`,
    });
    return [];
  }
}

// ---------------------------------------------------------------------------
// Payload parsing helpers.
// ---------------------------------------------------------------------------

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

function ciTarget(
  eventType: string,
  payload: JsonRecord,
): { headSha: string; prNumbers: number[] } | null {
  if (eventType === "status") {
    const headSha = stringValue(payload.sha);
    return headSha ? { headSha, prNumbers: [] } : null;
  }
  const node =
    eventType === "check_run"
      ? payload.check_run
      : eventType === "check_suite"
        ? payload.check_suite
        : eventType === "workflow_run"
          ? payload.workflow_run
          : undefined;
  if (!isRecord(node)) return null;
  const headSha = stringValue(node.head_sha);
  if (!headSha) return null;
  const prs = node.pull_requests;
  const prNumbers: number[] = [];
  if (Array.isArray(prs)) {
    for (const pr of prs) {
      const n = isRecord(pr) ? numberValue(pr.number) : undefined;
      if (n !== undefined) prNumbers.push(n);
    }
  }
  return { headSha, prNumbers };
}

function makeTrace(threadKey: string, messageId: string): GithubbotTrace {
  return {
    includeContext: false,
    messageId,
    mode: "execute",
    openStream: true,
    startedAtMs: nowMs(),
    threadId: threadKey,
  };
}
