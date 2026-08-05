import type { GitHubAdapter } from "@chat-adapter/github";
import { emitWorkflowEvent } from "./session-api";
import type { GithubbotOptions } from "./types";
import { errorMessage, noopLogger, stringValue } from "./utils";

/**
 * Converts GitHub lifecycle webhooks into api-rs workflow events. Events are
 * immutable per (event_type, correlation_id), so CI emits only after every
 * check settles and each correlation includes every waiter discriminator.
 *
 *   commit-scoped: <owner>/<repo>:<head_sha>
 *   PR-scoped:     <owner>/<repo>:pr-<n>:<head_sha>:<actor>
 *
 * Correlations are lowercased. GitHub App actors retain the `[bot]` suffix,
 * so waiter configs must use the exact login.
 */

type Octokit = GitHubAdapter["octokit"];

export type WorkflowEventProducerContext = {
  octokit: Octokit;
  options: GithubbotOptions;
};

type JsonRecord = Record<string, unknown>;

export const WORKFLOW_EVENT_CI_COMPLETED = "ci-completed";
export const WORKFLOW_EVENT_REVIEW_SUBMITTED = "review-submitted";

function ciCorrelationId(owner: string, repo: string, headSha: string): string {
  return `${owner}/${repo}:${headSha}`.toLowerCase();
}

function reviewCorrelationId(
  owner: string,
  repo: string,
  number: number,
  headSha: string,
  reviewer: string,
): string {
  return `${owner}/${repo}:pr-${number}:${headSha}:${reviewer}`.toLowerCase();
}

const FAILED_CONCLUSIONS = new Set([
  "action_required",
  "cancelled",
  "failure",
  "stale",
  "startup_failure",
  "timed_out",
]);

const SETTLED_CHECK_STATES = new Set([
  "ACTION_REQUIRED",
  "CANCELLED",
  "COMPLETED",
  "FAILURE",
  "NEUTRAL",
  "SKIPPED",
  "STALE",
  "STARTUP_FAILURE",
  "SUCCESS",
  "TIMED_OUT",
]);

const SETTLED_STATUS_STATES = new Set(["ERROR", "FAILURE", "SUCCESS"]);

export type CiCheck = { status: string; conclusion: string | null; name: string };
export type CiStatus = { state: string; context: string };

export type CiEvaluation = {
  settled: boolean;
  failed: boolean;
  failingNames: string[];
};

export function evaluateCi(
  checks: CiCheck[],
  statuses: CiStatus[],
): CiEvaluation {
  const anyCheckPending = checks.some((c) => c.status !== "completed");
  const anyStatusPending = statuses.some(
    (s) => s.state !== "success" && s.state !== "failure" && s.state !== "error",
  );
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

// The rollup includes EXPECTED required checks and remains readable when a
// fine-grained PAT cannot access the REST check-runs endpoint.
const CI_ROLLUP_QUERY = `query($owner: String!, $repo: String!, $sha: GitObjectID!, $after: String) {
  repository(owner: $owner, name: $repo) {
    object(oid: $sha) {
      ... on Commit {
        statusCheckRollup {
          state
          contexts(first: 100, after: $after) {
            nodes {
              __typename
              ... on CheckRun {
                name
                status
                conclusion
                startedAt
                checkSuite {
                  app { slug }
                  workflowRun {
                    event
                    workflow { name }
                  }
                }
              }
              ... on StatusContext { context state createdAt }
            }
            pageInfo { hasNextPage endCursor }
            checkRunCountsByState { state count }
            statusContextCountsByState { state count }
          }
        }
      }
    }
  }
}`;

type CiRollupContext = {
  __typename: string;
  checkSuite?: {
    app?: { slug?: string | null } | null;
    workflowRun?: {
      event?: string | null;
      workflow?: { name?: string | null } | null;
    } | null;
  } | null;
  createdAt?: string | null;
  name?: string;
  status?: string;
  conclusion?: string | null;
  context?: string;
  state?: string;
  startedAt?: string | null;
};

type CiRollupCheck = CiRollupContext & { name: string; status: string };
type CiRollupStatus = CiRollupContext & { context: string; state: string };

type CiStateCount = {
  count: number;
  state: string;
};

type CiPageInfo = {
  endCursor?: string | null;
  hasNextPage: boolean;
};

type CiRollup = {
  state: string;
  contexts?: {
    nodes?: (CiRollupContext | null)[] | null;
    pageInfo?: CiPageInfo | null;
    checkRunCountsByState?: CiStateCount[] | null;
    statusContextCountsByState?: CiStateCount[] | null;
  } | null;
};

type CiRollupResponse = {
  repository?: { object?: { statusCheckRollup?: CiRollup | null } | null } | null;
};

/** Read the complete CI rollup, returning null rather than assuming green. */
export async function fetchCiEvaluation(
  ctx: WorkflowEventProducerContext,
  owner: string,
  repo: string,
  sha: string,
): Promise<CiEvaluation | null> {
  const logger = ctx.options.logger ?? noopLogger;
  const nodes: CiRollupContext[] = [];
  let after: string | null = null;
  let rollupState: string | undefined;
  let checkRunCounts: CiStateCount[] | null | undefined;
  let statusContextCounts: CiStateCount[] | null | undefined;
  let detailReadable = true;

  for (;;) {
    let rollup: CiRollup | null | undefined;
    try {
      const result: CiRollupResponse = await ctx.octokit.graphql<CiRollupResponse>(
        CI_ROLLUP_QUERY,
        { after, owner, repo, sha },
      );
      rollup = result.repository?.object?.statusCheckRollup;
    } catch (error) {
      const partial = (error as { data?: CiRollupResponse }).data;
      rollup = partial?.repository?.object?.statusCheckRollup;
      if (!rollup) {
        logger.warn("githubbot_ci_rollup_failed", { error: errorMessage(error) });
        return null;
      }
    }
    if (!rollup) return null;

    rollupState = rollup.state;
    checkRunCounts ??= rollup.contexts?.checkRunCountsByState;
    statusContextCounts ??= rollup.contexts?.statusContextCountsByState;
    const pageNodes = rollup.contexts?.nodes;
    const readableNodes = pageNodes?.filter(
      (node): node is CiRollupContext => node !== null,
    );
    if (!pageNodes || readableNodes?.length !== pageNodes.length) {
      detailReadable = false;
    } else {
      nodes.push(...readableNodes);
    }

    const pageInfo: CiPageInfo | null | undefined = rollup.contexts?.pageInfo;
    if (!detailReadable || !pageInfo?.hasNextPage) break;
    if (!pageInfo.endCursor) {
      logger.warn("githubbot_ci_rollup_pagination_failed", { ref: `${owner}/${repo}@${sha}` });
      return null;
    }
    after = pageInfo.endCursor;
  }

  if (!rollupState) return null;
  const detail = detailReadable ? evaluateCiRollupContexts(nodes) : null;
  const aggregatePending = rollupState === "PENDING" || rollupState === "EXPECTED";
  const countsPending = stateCountsPending(checkRunCounts, statusContextCounts);
  let settled = rollupState === "SUCCESS";
  if (detail) settled = detail.settled && !aggregatePending;
  else if (countsPending !== undefined) {
    settled = !aggregatePending && !countsPending;
  }
  return {
    settled,
    failed:
      rollupState === "FAILURE" || rollupState === "ERROR" || detail?.failed === true,
    failingNames: detail?.failingNames ?? [],
  };
}

function evaluateCiRollupContexts(nodes: CiRollupContext[]): CiEvaluation {
  const checks = latestCiChecks(nodes).map((node) => ({
    status: node.status.toLowerCase(),
    conclusion: node.conclusion?.toLowerCase() ?? null,
    name: node.name,
  }));
  const statuses = latestCiStatuses(nodes).map((node) => ({
    state: node.state.toLowerCase(),
    context: node.context,
  }));
  return evaluateCi(checks, statuses);
}

function latestCiChecks(nodes: CiRollupContext[]): CiRollupCheck[] {
  const latest = new Map<string, CiRollupCheck>();
  for (const node of nodes) {
    if (!isCiRollupCheck(node)) continue;
    const workflowRun = node.checkSuite?.workflowRun;
    const key = [
      node.checkSuite?.app?.slug ?? "",
      node.name,
      workflowRun?.workflow?.name ?? "",
      workflowRun?.event ?? "",
    ].join("\0");
    const current = latest.get(key);
    if (!current || (node.startedAt ?? "") >= (current.startedAt ?? "")) {
      latest.set(key, node);
    }
  }
  return [...latest.values()];
}

function latestCiStatuses(nodes: CiRollupContext[]): CiRollupStatus[] {
  const latest = new Map<string, CiRollupStatus>();
  for (const node of nodes) {
    if (!isCiRollupStatus(node)) continue;
    const current = latest.get(node.context);
    if (!current || (node.createdAt ?? "") >= (current.createdAt ?? "")) {
      latest.set(node.context, node);
    }
  }
  return [...latest.values()];
}

function isCiRollupCheck(node: CiRollupContext): node is CiRollupCheck {
  return node.__typename === "CheckRun" && Boolean(node.name && node.status);
}

function isCiRollupStatus(node: CiRollupContext): node is CiRollupStatus {
  return node.__typename === "StatusContext" && Boolean(node.context && node.state);
}

function stateCountsPending(
  checkRunCounts: CiStateCount[] | null | undefined,
  statusContextCounts: CiStateCount[] | null | undefined,
): boolean | undefined {
  if (!checkRunCounts || !statusContextCounts) return undefined;
  return (
    checkRunCounts.some(({ count, state }) => count > 0 && !SETTLED_CHECK_STATES.has(state)) ||
    statusContextCounts.some(({ count, state }) => count > 0 && !SETTLED_STATUS_STATES.has(state))
  );
}

// A push can briefly read SUCCESS before the real suite registers. Confirming
// avoids locking the immutable event row with that false green.
const DEFAULT_CI_SETTLE_CONFIRM_MS = 15_000;

export async function prepareCiCompleted(
  ctx: WorkflowEventProducerContext,
  eventType: string,
  repo: { owner: string; repo: string },
  payload: JsonRecord,
  headSha: string,
): Promise<{
  emission: Promise<void> | null;
  evaluation: CiEvaluation | null;
}> {
  if (
    ctx.options.workflowEvents !== true ||
    !ciCompletionSignaled(eventType, payload)
  ) {
    return { emission: null, evaluation: null };
  }
  const evaluation = await fetchCiEvaluation(ctx, repo.owner, repo.repo, headSha);
  return {
    evaluation,
    emission: evaluation?.settled
      ? emitCiCompleted(ctx, repo, headSha, evaluation)
      : null,
  };
}

async function emitCiCompleted(
  ctx: WorkflowEventProducerContext,
  repo: { owner: string; repo: string },
  headSha: string,
  evaluation: CiEvaluation,
): Promise<void> {
  if (!evaluation.failed) {
    await sleep(ctx.options.ciSettleConfirmMs ?? DEFAULT_CI_SETTLE_CONFIRM_MS);
    const confirmed = await fetchCiEvaluation(ctx, repo.owner, repo.repo, headSha);
    if (!confirmed?.settled || confirmed.failed) return;
  }
  await emitWorkflowEvent(ctx.options, {
    eventType: WORKFLOW_EVENT_CI_COMPLETED,
    correlationId: ciCorrelationId(repo.owner, repo.repo, headSha),
    payload: { failed: evaluation.failed, failing: evaluation.failingNames },
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function ciCompletionSignaled(eventType: string, payload: JsonRecord): boolean {
  if (eventType === "status") {
    const state = stringValue(payload.state);
    return Boolean(state) && state !== "pending";
  }
  return stringValue(payload.action) === "completed";
}

export async function maybeEmitReviewSubmitted(
  ctx: WorkflowEventProducerContext,
  repo: { owner: string; repo: string },
  number: number,
  headSha: string,
  reviewer: string | undefined,
  reviewState: string | undefined,
  reviewId: number,
): Promise<void> {
  if (ctx.options.workflowEvents !== true || !reviewer) return;
  await emitWorkflowEvent(ctx.options, {
    eventType: WORKFLOW_EVENT_REVIEW_SUBMITTED,
    correlationId: reviewCorrelationId(repo.owner, repo.repo, number, headSha, reviewer),
    payload: { review_id: reviewId, state: reviewState ?? null },
  });
}
